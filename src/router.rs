use crate::{
    babel::{self, SeqNoRequest},
    crypto::{PacketBuffer, PublicKey, SecretKey, SharedSecret},
    filters::RouteUpdateFilter,
    ip_pubkey::IpPubkeyMap,
    metric::Metric,
    packet::{ControlPacket, DataPacket},
    peer::Peer,
    router_id::RouterId,
    routing_table::{RouteEntry, RouteExpirationType, RouteKey, RoutingTable},
    sequence_number::SeqNo,
    source_table::{FeasibilityDistance, SourceKey, SourceTable},
    subnet::Subnet,
};
use etherparse::{
    icmpv6::{DestUnreachableCode, TimeExceededCode},
    Icmpv6Type,
};
use left_right::{ReadHandle, WriteHandle};
use log::{debug, error, info, trace, warn};
use std::{
    error::Error,
    fmt::Debug,
    net::{IpAddr, Ipv6Addr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender};

const HELLO_INTERVAL: u16 = 4;
const IHU_INTERVAL: u16 = HELLO_INTERVAL * 3;
const UPDATE_INTERVAL: u16 = HELLO_INTERVAL * 4;
const ROUTE_PROPAGATION_INTERVAL: u64 = 3;
const DEAD_PEER_TRESHOLD: u64 = 8;
/// Amount of time to wait between consecutive seqno bumps of the local router seqno.
const SEQNO_BUMP_TIMEOUT: Duration = Duration::from_secs(4);

/// Metric change of more than 10 is considered a large change.
const BIG_METRIC_CHANGE_TRESHOLD: Metric = Metric::new(10);

/// The amount a metric of a route needs to improve before we will consider switching to it.
const SIGNIFICANT_METRIC_IMPROVEMENT: Metric = Metric::new(10);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StaticRoute {
    subnet: Subnet,
}

impl StaticRoute {
    pub fn new(subnet: Subnet) -> Self {
        Self { subnet }
    }
}

#[derive(Clone)]
pub struct Router {
    inner_w: Arc<Mutex<WriteHandle<RouterInner, RouterOpLogEntry>>>,
    inner_r: ReadHandle<RouterInner>,
    router_id: RouterId,
    node_keypair: (SecretKey, PublicKey),
    router_data_tx: Sender<DataPacket>,
    router_control_tx: UnboundedSender<(ControlPacket, Peer)>,
    node_tun: UnboundedSender<DataPacket>,
    node_tun_subnet: Subnet,
    update_filters: Arc<Vec<Box<dyn RouteUpdateFilter + Send + Sync>>>,
    /// Channel injected into peers, so they can notify the router if they exit.
    dead_peer_sink: mpsc::Sender<Peer>,
}

impl Router {
    pub fn new(
        node_tun: UnboundedSender<DataPacket>,
        node_tun_subnet: Subnet,
        static_routes: Vec<StaticRoute>,
        node_keypair: (SecretKey, PublicKey),
        update_filters: Vec<Box<dyn RouteUpdateFilter + Send + Sync>>,
    ) -> Result<Self, Box<dyn Error>> {
        // Tx is passed onto each new peer instance. This enables peers to send control packets to the router.
        let (router_control_tx, router_control_rx) = mpsc::unbounded_channel();
        // Tx is passed onto each new peer instance. This enables peers to send data packets to the router.
        let (router_data_tx, router_data_rx) = mpsc::channel::<DataPacket>(1000);
        let (expired_source_key_sink, expired_source_key_stream) = mpsc::channel(1);
        let (expired_route_entry_sink, expired_route_entry_stream) = mpsc::channel(1);
        let (dead_peer_sink, dead_peer_stream) = mpsc::channel(1);

        let router_inner = RouterInner::new(expired_source_key_sink, expired_route_entry_sink)?;
        let (mut inner_w, inner_r) = left_right::new_from_empty(router_inner);
        inner_w.append(RouterOpLogEntry::SetStaticRoutes(static_routes));
        inner_w.publish();

        let router_id = RouterId::new(node_keypair.1);

        let router = Router {
            inner_w: Arc::new(Mutex::new(inner_w)),
            inner_r,
            router_id,
            node_keypair,
            router_data_tx,
            router_control_tx,
            node_tun,
            node_tun_subnet,
            dead_peer_sink,
            update_filters: Arc::new(update_filters),
        };

        tokio::spawn(Router::start_periodic_hello_sender(router.clone()));
        tokio::spawn(Router::handle_incoming_control_packet(
            router.clone(),
            router_control_rx,
        ));
        tokio::spawn(Router::handle_incoming_data_packet(
            router.clone(),
            router_data_rx,
        ));
        tokio::spawn(Router::propagate_static_route(router.clone()));
        tokio::spawn(Router::propagate_selected_routes(router.clone()));

        tokio::spawn(Router::check_for_dead_peers(router.clone()));

        tokio::spawn(Router::process_expired_source_keys(
            router.clone(),
            expired_source_key_stream,
        ));

        tokio::spawn(Router::process_expired_route_keys(
            router.clone(),
            expired_route_entry_stream,
        ));

        tokio::spawn(Router::process_dead_peers(router.clone(), dead_peer_stream));

        Ok(router)
    }

    pub fn router_control_tx(&self) -> UnboundedSender<(ControlPacket, Peer)> {
        self.router_control_tx.clone()
    }

    pub fn router_data_tx(&self) -> Sender<DataPacket> {
        self.router_data_tx.clone()
    }

    pub fn node_tun_subnet(&self) -> Subnet {
        self.node_tun_subnet
    }

    pub fn node_tun(&self) -> UnboundedSender<DataPacket> {
        self.node_tun.clone()
    }

    pub fn peer_interfaces(&self) -> Vec<Peer> {
        self.inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles")
            .peer_interfaces
            .clone()
    }

    pub fn add_peer_interface(&self, peer: Peer) {
        debug!("Adding peer {} to router", peer.connection_identifier());
        self.inner_w
            .lock()
            .expect("Mutex isn't poinsoned")
            .append(RouterOpLogEntry::AddPeer(peer))
            .publish();
    }

    pub fn node_secret_key(&self) -> SecretKey {
        self.node_keypair.0.clone()
    }

    pub fn node_public_key(&self) -> PublicKey {
        self.node_keypair.1
    }

    /// Get the [`RouterId`] of the `Router`.
    pub fn router_id(&self) -> RouterId {
        self.router_id
    }

    /// Add a new destination [`PublicKey`] to the destination map. This will also compute and store
    /// the [`SharedSecret`] from the `Router`'s [`SecretKey`].
    pub fn add_dest_pubkey_map_entry(&self, dest: Subnet, pubkey: PublicKey) {
        let ss = self.node_keypair.0.shared_secret(&pubkey);

        self.inner_w
            .lock()
            .expect("Mutex isn't poisoned")
            .append(RouterOpLogEntry::AddDestPubkey(dest, pubkey, ss))
            .publish();
    }

    /// Get the [`PublicKey`] for an [`Ipv6Addr`] if a mapping exists.
    pub fn get_pubkey(&self, ip: IpAddr) -> Option<PublicKey> {
        if let IpAddr::V6(ip) = ip {
            self.inner_r
                .enter()
                .expect(
                    "Write handle is saved on router so it is not dropped before the read handles",
                )
                .dest_pubkey_map
                .lookup(ip)
                .map(|(pk, _)| pk)
                .copied()
        } else {
            None
        }
    }

    /// Gets the cached [`SharedSecret`] for the remote.
    pub fn get_shared_secret_from_dest(&self, dest: Ipv6Addr) -> Option<SharedSecret> {
        self.inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles")
            .dest_pubkey_map
            .lookup(dest)
            .map(|(_, ss)| ss.clone())
    }

    /// Gets the cached [`SharedSecret`] based on the associated [`PublicKey`] of the remote.
    pub fn get_shared_secret_by_pubkey(&self, dest: &PublicKey) -> Option<SharedSecret> {
        self.inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles")
            .dest_pubkey_map
            .lookup(dest.address())
            .map(|(_, ss)| ss.clone())
    }

    /// Get a reference to this `Router`s' dead peer sink.
    pub fn dead_peer_sink(&self) -> &mpsc::Sender<Peer> {
        &self.dead_peer_sink
    }

    pub fn print_selected_routes(&self) {
        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");

        for (route_key, route_entry) in inner.routing_table.iter().filter(|(_, re)| re.selected()) {
            println!(
                "{} next-hop {} metric: {} (seqno {})",
                route_key.subnet(),
                route_entry.neighbour().connection_identifier(),
                route_entry.metric(),
                route_entry.seqno(),
            );
        }
    }

    pub fn print_fallback_routes(&self) {
        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");

        for (route_key, route_entry) in inner.routing_table.iter().filter(|(_, re)| !re.selected())
        {
            println!(
                "{} next-hop {} metric: {} (seqno {})",
                route_key.subnet(),
                route_entry.neighbour().connection_identifier(),
                route_entry.metric(),
                route_entry.seqno(),
            );
        }
    }

    pub fn print_source_table(&self) {
        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");

        let source_table = &inner.source_table;
        for (sk, se) in source_table.iter() {
            println!("Source key: {}", sk);
            println!("Source entry: {:?}", se);
            println!("\n");
        }
    }

    /// Prints the origin of every known [`Subnet`], i.e. the fully derived address.
    pub fn print_subnet_origins(&self) {
        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");

        for (route_key, route_entry) in inner.routing_table.iter().filter(|(_, re)| re.selected()) {
            println!(
                "{} origin {}",
                route_key.subnet(),
                route_entry.source().router_id().to_pubkey().address(),
            );
        }
    }

    async fn check_for_dead_peers(self) {
        let ihu_threshold = tokio::time::Duration::from_secs(DEAD_PEER_TRESHOLD);

        loop {
            // check for dead peers every second
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            trace!("Checking for dead peers");

            let dead_peers = {
                // a peer is assumed dead when the peer's last sent ihu exceeds a threshold
                let mut dead_peers = Vec::new();
                for peer in self.inner_r.enter().expect("Write handle is saved on router so it can never go out of scope before read handle").peer_interfaces.iter() {
                    // check if the peer's last_received_ihu is greater than the threshold
                    if peer.time_last_received_ihu().elapsed() > ihu_threshold {
                        // peer is dead
                        info!("Peer {} is dead", peer.connection_identifier());
                        dead_peers.push(peer.clone());
                    }
                }
                dead_peers
            };

            for dead_peer in dead_peers {
                self.handle_dead_peer(dead_peer);
            }
        }
    }

    /// Remove a dead peer from the router.
    fn handle_dead_peer(&self, dead_peer: Peer) {
        debug!(
            "Cleaning up peer {} which is reportedly dead",
            dead_peer.connection_identifier()
        );

        // Scope for the mutex lock
        let subnets_to_select = {
            let mut inner_w = self.inner_w.lock().unwrap();

            let inner = self.inner_r.enter().expect(
                "Write handle is saved on the router so it is not dropped before the read handles",
            );

            let mut subnets_to_select = Vec::new();

            for (rk, re) in inner.routing_table.iter() {
                if rk.neighbour() == &dead_peer {
                    subnets_to_select.push(rk.subnet());
                    inner_w.append(RouterOpLogEntry::UpdateRouteEntry(
                        rk,
                        re.seqno(),
                        Metric::infinite(),
                        re.source().router_id(),
                    ));
                }
            }
            // Make sure we release the read handle, so a publish on the write handle eventually
            // succeeds.
            drop(inner);
            inner_w.append(RouterOpLogEntry::RemovePeer(dead_peer));
            inner_w.publish();

            subnets_to_select
        };

        // And run required route selection
        for subnet in subnets_to_select {
            self.route_selection(subnet);
        }
    }

    /// Run route selection for a given subnet
    fn route_selection(&self, subnet: Subnet) {
        debug!("Running route selection for {subnet}");
        let mut inner_w = self.inner_w.lock().unwrap();

        let routes = inner_w
            .enter()
            .expect("Deref through write handle so there always is a write handle in scope here")
            .routing_table
            .entries(subnet);

        // No routes for subnet, nothing to do here.
        if routes.is_empty() {
            return;
        }

        // If there is no selected route there is nothing to do here. We keep expired routes in the
        // table for a while so updates of those should already have propagated to peers.
        if let Some(new_selected) = self.find_best_route(
            &routes,
            if routes[0].selected() {
                Some(&routes[0])
            } else {
                None
            },
        ) {
            if new_selected.neighbour() == routes[0].neighbour() && routes[0].selected() {
                debug!(
                    "New selected route for {subnet} is the same as the route alreayd installed"
                );
                return;
            }

            if new_selected.metric().is_infinite()
                && routes[0].metric().is_infinite()
                && routes[0].selected()
            {
                debug!("New selected route for {subnet} is retracted, like the previously selected route");
                return;
            }

            inner_w.append(RouterOpLogEntry::SelectRoute(RouteKey::new(
                subnet,
                new_selected.neighbour().clone(),
            )));
            inner_w.publish();

            Router::trigger_update(&mut inner_w, subnet, self.router_id);
        }
    }

    /// Remove expired source keys from the router state.
    async fn process_expired_source_keys(
        self,
        mut expired_source_key_stream: mpsc::Receiver<SourceKey>,
    ) {
        while let Some(sk) = expired_source_key_stream.recv().await {
            let mut inner = self.inner_w.lock().unwrap();
            debug!("Removing expired source entry {sk}");
            inner.append(RouterOpLogEntry::RemoveSourceEntry(sk));
            inner.publish();
        }
        warn!("Expired source key processing halted");
    }

    /// Remove expired route keys from the router state.
    async fn process_expired_route_keys(
        self,
        mut expired_route_key_stream: mpsc::Receiver<(RouteKey, RouteExpirationType)>,
    ) {
        while let Some((rk, expiration_type)) = expired_route_key_stream.recv().await {
            tokio::time::sleep(Duration::from_secs(1)).await;
            debug!("Got expiration event for route {rk}");
            let subnet = rk.subnet();
            let mut inner = self.inner_w.lock().unwrap();
            // Load current key
            let entry = inner
                .enter()
                .expect("We enter through a write handle so this can never be None")
                .routing_table
                .get(&rk)
                .cloned();
            if entry.is_none() {
                continue;
            }
            let entry = entry.unwrap();
            if !entry.metric().is_infinite()
                && matches!(expiration_type, RouteExpirationType::Retract)
            {
                debug!("Route {rk} expired, increasing metric to infinity");
                inner.append(RouterOpLogEntry::UpdateRouteEntry(
                    rk,
                    entry.seqno(),
                    Metric::infinite(),
                    entry.source().router_id(),
                ));
            } else if entry.metric().is_infinite()
                && matches!(expiration_type, RouteExpirationType::Remove)
            {
                debug!("Route {rk} expired, removing retracted route");
                inner.append(RouterOpLogEntry::RemoveRoute(rk));
            } else {
                continue;
            }
            inner.publish();
            // Re run route selection if this was the selected route. We should do this before
            // publishing to potentially select a new route, however a time based expiraton of a
            // selected route generally means no other routes are viable anyway, so the short lived
            // black hole this could create is not really a concern.
            if entry.selected() {
                let routes = inner
                    .enter()
                    .expect("We enter through a write handle so this can never be None")
                    .routing_table
                    .entries(subnet);
                // Only inject selected route if we are simply retracting it, otherwise it is
                // actually already removed.
                if let Some(r) = self.find_best_route(
                    &routes,
                    if matches!(expiration_type, RouteExpirationType::Retract) {
                        Some(&entry)
                    } else {
                        None
                    },
                ) {
                    debug!("Rerun route selection after expiration event");
                    inner
                        .append(RouterOpLogEntry::SelectRoute(RouteKey::new(
                            subnet,
                            r.neighbour().clone(),
                        )))
                        .publish();
                    // If the entry wasn't retracted yet, notify our peers.
                    if !entry.metric().is_infinite() {
                        Router::trigger_update(&mut inner, subnet, self.router_id);
                    }
                }
            }
        }
        warn!("Expired route key processing halted");
    }

    /// Process notifications about peers who are dead. This allows peers who can self-diagnose
    /// connection states to notify us, and allow for more efficient cleanup.
    async fn process_dead_peers(self, mut dead_peer_stream: mpsc::Receiver<Peer>) {
        while let Some(dead_peer) = dead_peer_stream.recv().await {
            self.handle_dead_peer(dead_peer);
        }
        warn!("Processing of dead peers halted");
    }

    async fn handle_incoming_control_packet(
        self,
        mut router_control_rx: UnboundedReceiver<(ControlPacket, Peer)>,
    ) {
        while let Some((control_packet, source_peer)) = router_control_rx.recv().await {
            trace!(
                "Received control packet from {}",
                source_peer.connection_identifier()
            );
            match control_packet {
                babel::Tlv::Hello(hello) => self.handle_incoming_hello(hello, source_peer),
                babel::Tlv::Ihu(ihu) => self.handle_incoming_ihu(ihu, source_peer),
                babel::Tlv::Update(update) => self.handle_incoming_update(update, source_peer),
                babel::Tlv::RouteRequest(route_request) => {
                    self.handle_incoming_route_request(route_request, source_peer)
                }
                babel::Tlv::SeqNoRequest(seqno_request) => {
                    self.handle_incoming_seqno_request(seqno_request, source_peer)
                }
            }
        }
    }

    fn handle_incoming_hello(&self, _: babel::Hello, source_peer: Peer) {
        // Upon receiving and Hello message from a peer, this node has to send a IHU back
        let ihu = ControlPacket::new_ihu(IHU_INTERVAL, None);
        if let Err(e) = source_peer.send_control_packet(ihu) {
            error!("Error sending IHU to peer: {e}");
        }
    }

    fn handle_incoming_ihu(&self, _: babel::Ihu, source_peer: Peer) {
        // reset the IHU timer associated with the peer
        // measure time between Hello and and IHU and set the link cost
        let time_diff = tokio::time::Instant::now()
            .duration_since(source_peer.time_last_received_hello())
            .as_millis();

        source_peer.set_link_cost(time_diff as u16);

        // set the last_received_ihu for this peer
        source_peer.set_time_last_received_ihu(tokio::time::Instant::now());
    }

    /// Process a route request. We reply with an Update if we have a selected route for the
    /// requested subnet. If no subnet is requested (in other words the wildcard address), we dump
    /// the full routing table.
    fn handle_incoming_route_request(&self, route_request: babel::RouteRequest, source_peer: Peer) {
        let mut inner_w = self.inner_w.lock().unwrap();

        let ops = {
            let inner = inner_w
                .enter()
                .expect("We deref through a write handle so this is always Some; qed");

            let mut ops = vec![];
            // Handle the case of a single subnet.
            if let Some(subnet) = route_request.prefix() {
                if let Some(sre) = inner.routing_table.lookup_selected(subnet.address()) {
                    // Optimization: Don't send an update if the selected route next-hop is the peer
                    // who requested the route, as per the babel protocol the update will never be
                    // accepted.
                    if sre.neighbour() == &source_peer {
                        return;
                    }
                    let update = babel::Update::new(
                        UPDATE_INTERVAL,
                        sre.seqno(),
                        sre.metric() + Metric::from(sre.neighbour().link_cost()),
                        subnet,
                        sre.source().router_id(),
                    );
                    ops.extend(inner.send_update(&source_peer, update));
                }
            } else {
                // Requested a full route table dump
                ops.extend(inner.propagate_selected_routes_to_peer(&source_peer));
                ops.extend(inner.propagate_static_route_to_peer(&source_peer, self.router_id()));
            }
            ops
        };

        inner_w.extend(ops);
        inner_w.publish();
    }

    fn handle_incoming_seqno_request(&self, mut seqno_request: SeqNoRequest, source_peer: Peer) {
        // According to the babel rfc, we shoudl maintain a table of recent SeqNo requests and
        // periodically retry requests without reply. We will however not do this for now and rely
        // on the fact that we have stable links in general.

        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");

        // If we have a selected route for the prefix, and its router id is different from the
        // requested router id, or the router id is the same and the entries sequence number is
        // not smaller than the requested sequence number, send an update for the route
        // to the peer (triggered update).
        if let Some(route_entry) = inner
            .routing_table
            .lookup_selected(seqno_request.prefix().address())
        {
            if !route_entry.metric().is_infinite()
                && (seqno_request.router_id() != route_entry.source().router_id()
                    || !route_entry.seqno().lt(&seqno_request.seqno()))
            {
                // we have a more up to date route or a different route, send an update
                debug!(
                    "Replying to seqno request for seqno {} of {} with update packet",
                    seqno_request.seqno(),
                    seqno_request.prefix()
                );
                drop(inner);
                let update = babel::Update::new(
                    UPDATE_INTERVAL,
                    route_entry.seqno(), // updates receive the seqno of the router
                    route_entry.metric() + Metric::from(source_peer.link_cost()),
                    // the cost of the route is the cost of the route + the cost of the link to the peer
                    route_entry.source().subnet(),
                    // we looked for the router_id, which is a public key, in the dest_pubkey_map
                    // if the router_id is not in the map, then the route came from the node itself
                    route_entry.source().router_id(),
                );
                let mut inner_w = self.inner_w.lock().expect("Mutex isn't poisoned");

                let op = inner_w
                    .enter()
                    .expect("We enter through a write handle so this can never be None")
                    .send_update(&source_peer, update);

                if let Some(op) = op {
                    inner_w.append(op);
                    inner_w.publish();
                }

                return;
            }
        }

        // Otherwise, if the router id in the request matches the router id in our selected route
        // and the requested sequence number is larger than the one on our selected route, compare
        // the router id with our own router id. If it matches, bump our own sequence number by 1.
        // At this point, we also send an update for the route (triggered update), to distribute
        // the route.
        //
        // Note that we currently don't install local routes in the routing table and as such
        // can't check on this. Therefore this condition is reworked. We always advertise local
        // routes with the current router id and the current router seqno. So we check if the
        // prefix is part of our static routes, if the router id is our own, and if the
        // requested seqno is greater than our own.
        if seqno_request.router_id() == self.router_id
            && seqno_request.seqno().gt(&inner.router_seqno)
            && inner.static_routes.contains(&StaticRoute {
                subnet: seqno_request.prefix(),
            })
        {
            if inner.last_seqno_bump.elapsed() >= SEQNO_BUMP_TIMEOUT {
                trace!("Ignoring seqno bump request which happened too fast");
                return;
            }
            // Bump router seqno
            // TODO: should we only send an update to the peer who sent the seqno request
            // instad of updating all our peers?
            drop(inner);
            let mut inner_w = self.inner_w.lock().expect("Mutex isn't poisoned");
            debug!("Bumping local router sequence number");
            inner_w.append(RouterOpLogEntry::BumpSequenceNumber(Instant::now()));
            // We already need to publish here so the sequence number is set correctly when
            // calling the method to propagate the static routes.
            inner_w.publish();

            let ops = inner_w
                .enter()
                .expect("We enter through a write handle so this can never be None")
                .propagate_static_route(self.router_id);

            inner_w.extend(ops);
            inner_w.publish();
            return;
        }

        // Otherwise, if the router-id from the request is not our own, we check the hop count
        // field. If it is at least 2, we decrement it by 1, and forward the packet. To do so, we
        // try to find a route to the subnet. First we check for a feasible route and send the
        // packet there if the next hop is not the sender of this packet. Otherwise, we check for
        // any route which might potentially be unfeasible, which also did not originate the
        // packet.
        if seqno_request.router_id() != self.router_id && seqno_request.hop_count() > 1 {
            seqno_request.decrement_hop_count();

            let possible_routes = inner.routing_table.entries(seqno_request.prefix());

            // First only consider feasible routes.
            if let Some(re) = possible_routes.iter().find(|re| {
                inner.source_table.route_feasible(re)
                    && re.neighbour() != &source_peer
                    && !re.metric().is_infinite()
            }) {
                debug!(
                    "Forwarding seqno request {} for {} to {}",
                    seqno_request.seqno(),
                    seqno_request.prefix(),
                    re.neighbour().connection_identifier()
                );
                if let Err(e) = re.neighbour().send_control_packet(seqno_request.into()) {
                    error!("Failed to foward seqno request: {e}");
                }
                return;
            }

            // Finally consider infeasible routes as well.
            if let Some(re) = possible_routes
                .iter()
                .find(|re| re.neighbour() != &source_peer && !re.metric().is_infinite())
            {
                debug!(
                    "Forwarding seqno request {} for {} to {}",
                    seqno_request.seqno(),
                    seqno_request.prefix(),
                    re.neighbour().connection_identifier()
                );
                if let Err(e) = re.neighbour().send_control_packet(seqno_request.into()) {
                    error!("Failed to foward seqno request: {e}");
                }
            }
        }
    }

    /// Finds the best feasible route from a list of [`route entries`](RouteEntry). It is possible
    /// for this method to select a retracted route. In this case, retraction updates should be
    /// send out.
    ///
    /// This method only selects a different best route if it is significantly better compared to
    /// the current route.
    fn find_best_route<'a>(
        &self,
        routes: &'a [RouteEntry],
        current: Option<&'a RouteEntry>,
    ) -> Option<&'a RouteEntry> {
        let inner = self.inner_r.enter().expect("Write handle is saved on router so it can never go out of scope before this read handle; qed.");
        // Since retracted routes have the highest possible metrics, this will only select one if
        // no non-retracted routes are feasible.
        let best = routes
            .iter()
            .filter(|re| inner.source_table.route_feasible(re))
            .min_by_key(|re| re.metric() + Metric::from(re.neighbour().link_cost()));

        if let (Some(best), Some(current)) = (best, current) {
            // If we swap to an actually different route, only do so if the metric is
            // significantly better OR if it is directly connected (metric 0).
            if (best.source() != current.source() || best.neighbour() != current.neighbour())
                && !(best.metric() < current.metric() - SIGNIFICANT_METRIC_IMPROVEMENT
                    || best.metric().is_direct())
            {
                debug!("maintaining currently selected route since new route is not significantly better");
                return Some(current);
            }
        }

        best
    }

    fn handle_incoming_update(&self, update: babel::Update, source_peer: Peer) {
        // Check if we actually allow this update based on filters.
        for filter in &*self.update_filters {
            if !filter.allow(&update) {
                debug!("Update denied by filter");
                return;
            }
        }

        let metric = update.metric();
        let router_id = update.router_id();
        let seqno = update.seqno();
        let subnet = update.subnet();

        // Make sure the shared secret is known for a destination.
        self.add_dest_pubkey_map_entry(subnet, router_id.to_pubkey());

        // create route key from incoming update control struct
        let update_route_key = RouteKey::new(subnet, source_peer.clone());
        // used later to filter out static route
        if self.route_key_is_from_static_route(&update_route_key) {
            return;
        }

        let mut inner_w = self.inner_w.lock().expect("Mutex isn't poisoned");

        // We load all routes here from the routing table in memory. Because we hold the mutex for the
        // writer, this view is accurate and we can't diverge until the mutex is released. We will
        // then apply every action on the list of route entries both to the local list, and as a
        // RouterOpLogEntry. This is needed because publishing intermediate results might cause
        // readers to observe intermediate state, which could lead to problems.
        let (mut routing_table_entries, update_feasible) = {
            let inner = inner_w
                .enter()
                .expect("We deref through a write handle so this enter never fails");
            (
                inner.routing_table.entries(subnet),
                inner.source_table.is_update_feasible(&update),
            )
        };

        // Take a deep copy of the old selected route if there is one, deep copy since we will
        // potentially mutate it.
        let old_selected_route = routing_table_entries
            .iter()
            .find(|entry| entry.selected())
            .cloned();

        let maybe_existing_entry_idx = routing_table_entries
            .iter()
            .position(|entry| entry.neighbour() == &source_peer);

        debug!(
             "Got update packet from {} for subnet {subnet} with metric {metric} and seqno {seqno} and router-id {router_id}. Route entry exists: {} and update is feasible: {update_feasible}",
             source_peer.connection_identifier(),
             maybe_existing_entry_idx.is_some(),
         );

        if let Some(existing_entry_idx) = maybe_existing_entry_idx {
            let existing_entry = &mut routing_table_entries[existing_entry_idx];
            // Unfeasible updates to the selected route are not applied, but we do request a seqno
            // bump.
            if existing_entry.selected()
                && !update_feasible
                && existing_entry.source().router_id() == router_id
            {
                let fd = *inner_w
                    .enter()
                    .expect("We deref through a write handle so this enter never fails")
                    .source_table
                    .get(&existing_entry.source())
                    .unwrap_or_else(|| {
                        panic!(
                            "An update came in for {subnet} which is unfeasible so a source table entry must exist"
                        )
                    });

                debug!(
                    "Sending seqno_request to {} for seqno {} of {}",
                    source_peer.connection_identifier(),
                    fd.seqno() + 1,
                    update.subnet(),
                );
                if let Err(e) = source_peer.send_control_packet(
                    SeqNoRequest::new(
                        fd.seqno() + 1,
                        existing_entry.source().router_id(),
                        update.subnet(),
                    )
                    .into(),
                ) {
                    error!(
                        "Failed to send seqno request to {}: {e}",
                        source_peer.connection_identifier()
                    );
                }
                return;
            }

            // Otherwise we just update the entry.
            if existing_entry.metric().is_infinite() && update.metric().is_infinite() {
                // Retraction for retracted route, don't do anything. If we don't filter this
                // retracted routes can stay stuck if peer keep sending retractions to eachother
                // for this route.
                return;
            }
            existing_entry.update_seqno(seqno);
            existing_entry.update_metric(metric);
            existing_entry.update_router_id(router_id);
            let rk = RouteKey::new(subnet, source_peer);
            inner_w.append(RouterOpLogEntry::UpdateRouteEntry(
                rk.clone(),
                seqno,
                metric,
                router_id,
            ));
            // If the update is unfeasible the route must be unselected.
            if existing_entry.selected() && !update_feasible {
                existing_entry.set_selected(false);
                inner_w.append(RouterOpLogEntry::UnselectRoute(rk));
            }
        } else {
            // If there is no entry yet ignore unfeasible updates and retractions.
            if metric.is_infinite() || !update_feasible {
                debug!("Received unfeasible update | retraction for unknown route - neighbour");
                return;
            }

            // Create new entry in the route table
            let re = RouteEntry::new(
                SourceKey::new(subnet, router_id),
                source_peer.clone(),
                metric,
                seqno,
                false,
            );
            routing_table_entries.push(re.clone());
            inner_w.append(RouterOpLogEntry::InsertRoute(
                RouteKey::new(subnet, source_peer),
                re,
            ));
        }

        // Now that we applied the update, run route selection.
        let new_selected_route =
            self.find_best_route(&routing_table_entries, old_selected_route.as_ref());
        if let Some(nbr) = new_selected_route {
            // Install this route in the routing table. We don't update the local copy anymore as
            // we don't use it afterwards.
            inner_w.append(RouterOpLogEntry::SelectRoute(RouteKey::new(
                subnet,
                nbr.neighbour().clone(),
            )));
        }

        // Already publish here, we won't make any other adjustments to the routing table.
        inner_w.publish();

        // At this point we are done, though we would like to understand if we need to send a
        // triggered update to our peers. This is done if there is a sufficiently large change. We
        // consider a sufficiently large change to be:
        // - change in router_id,
        // - aquired a route, i.e. previously there was no selected route but there is now,
        // - lost the route (i.e. it is retracted).
        // - significant metric change
        // What doesn't constitue a large change:
        // - small metric change
        // - seqno increase (unless it is requested by a peer)
        // TODO: we don't memorize seqno requests for now so consider broadcasting this anyway
        let trigger_update = match (&old_selected_route, new_selected_route) {
            (Some(old_route), Some(new_route)) => {
                if new_route.neighbour() != old_route.neighbour() {
                    info!(
                        "Selected route for {subnet} changed next-hop from {} to {}",
                        old_route.neighbour().connection_identifier(),
                        new_route.neighbour().connection_identifier()
                    );
                }
                // Router id changed.
                new_route.source().router_id() != old_route.source().router_id()
                // TODO: remove | seqno changed
                    || new_route.seqno().gt(&old_route.seqno())
                    || new_route.metric().delta(&old_route.metric()) > BIG_METRIC_CHANGE_TRESHOLD
            }
            (None, Some(new_route)) => {
                info!(
                    "Acquired route to {subnet} via {}",
                    new_route.neighbour().connection_identifier()
                );
                true
            }
            (Some(old_route), None) => {
                info!(
                    "Lost route to {subnet} via {}",
                    old_route.neighbour().connection_identifier()
                );
                true
            }
            (None, None) => false,
        };

        if trigger_update {
            debug!("Send triggered update for {subnet} in response to update");
            Router::trigger_update(&mut inner_w, subnet, router_id);
        }
    }

    /// Trigger an update for the given [`Subnet`].
    fn trigger_update(
        inner_w: &mut left_right::WriteHandle<RouterInner, RouterOpLogEntry>,
        subnet: Subnet,
        router_id: RouterId,
    ) {
        let ops = inner_w
            .enter()
            .expect("Deref through write handle never fails")
            .propagate_selected_route(subnet, router_id);
        inner_w.extend(ops);
        inner_w.publish();
    }

    fn route_key_is_from_static_route(&self, route_key: &RouteKey) -> bool {
        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");

        for sr in inner.static_routes.iter() {
            if sr.subnet == route_key.subnet() {
                return true;
            }
        }
        false
    }

    pub fn route_packet(&self, data_packet: DataPacket) {
        let node_tun_subnet = self.node_tun_subnet();

        trace!(
            "Incoming data packet {} -> {})",
            data_packet.src_ip,
            data_packet.dst_ip,
        );

        if node_tun_subnet.contains_ip(data_packet.dst_ip.into()) {
            if let Err(e) = self.node_tun().send(data_packet) {
                error!("Error sending data packet to TUN interface: {:?}", e);
            }
        } else {
            match self.select_best_route(IpAddr::V6(data_packet.dst_ip)) {
                Some(route_entry) => {
                    if let Err(e) = route_entry.neighbour().send_data_packet(data_packet) {
                        error!("Error sending data packet to peer: {:?}", e);
                    }
                }
                None => {
                    self.no_route_to_host(data_packet);
                }
            }
        }
    }

    async fn handle_incoming_data_packet(self, mut router_data_rx: Receiver<DataPacket>) {
        while let Some(mut data_packet) = router_data_rx.recv().await {
            if data_packet.hop_limit < 2 {
                self.time_exceeded(data_packet);
                continue;
            }
            data_packet.hop_limit -= 1;
            self.route_packet(data_packet);
        }
        warn!("Router data receiver stream ended");
    }

    /// Handle a packet who's TTL is too low.
    fn time_exceeded(&self, data_packet: DataPacket) {
        trace!("Refusing to forward expired packet");
        self.oob_icmp(
            Icmpv6Type::TimeExceeded(TimeExceededCode::HopLimitExceeded),
            data_packet,
        )
    }

    /// Handle a packet if we have no route for the destination address.
    fn no_route_to_host(&self, data_packet: DataPacket) {
        trace!(
            "Could not forward data packet, no route found for {}",
            data_packet.dst_ip
        );

        self.oob_icmp(
            Icmpv6Type::DestinationUnreachable(DestUnreachableCode::NoRoute),
            data_packet,
        )
    }

    fn oob_icmp(&self, icmp_type: Icmpv6Type, mut data_packet: DataPacket) {
        let src_ip = if let IpAddr::V6(ip) = self.node_tun_subnet.address() {
            ip
        } else {
            panic!("IPv4 not supported yet")
        };

        let icmp_header =
            etherparse::PacketBuilder::ipv6(src_ip.octets(), data_packet.src_ip.octets(), 64)
                .icmpv6(icmp_type);

        let mut pb = PacketBuffer::new();
        // Don't exceed MIN_MTU for the constructed packet
        // TODO: use proper consts
        if data_packet.raw_data.len() > (1280 - 48) {
            // Just drop raw_data, we don't need it anymore in this case and by doing this we have
            // a unified code path later. Also we release the no longer used memory just a tad bit
            // slower, though it's unlikely that this matters.
            data_packet.raw_data = vec![];
        }
        let serialized_icmp_size = icmp_header.size(data_packet.raw_data.len());
        pb.set_size(serialized_icmp_size + 16);
        pb.buffer_mut()[..16].copy_from_slice(&data_packet.dst_ip.octets());
        let mut ps = &mut pb.buffer_mut()[16..16 + serialized_icmp_size];
        if let Err(e) = icmp_header.write(&mut ps, &data_packet.raw_data) {
            error!("Failed to write ICMP packet {e}");
            return;
        }

        // TODO: import consts
        let mut header = pb.header_mut();
        header[0] = 1;
        header[1] = 2;

        // Get shared secret from node and dest address
        let shared_secret = match self.get_shared_secret_from_dest(data_packet.src_ip) {
            Some(ss) => ss,
            None => {
                debug!(
                    "No entry found for destination address {}, dropping packet",
                    data_packet.src_ip
                );
                return;
            }
        };

        let enc = shared_secret.encrypt(pb);

        self.route_packet(DataPacket {
            dst_ip: data_packet.src_ip,
            src_ip,
            hop_limit: 64,
            raw_data: enc,
        });
    }

    pub fn select_best_route(&self, dest_ip: IpAddr) -> Option<RouteEntry> {
        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");
        inner
            .routing_table
            .lookup_selected(dest_ip)
            .and_then(|entry| {
                if entry.metric().is_infinite() {
                    None
                } else {
                    Some(entry)
                }
            })
    }

    pub async fn propagate_static_route(self) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(ROUTE_PROPAGATION_INTERVAL)).await;

            trace!("Propagating static routes");

            let mut inner_w = self.inner_w.lock().expect("Mutex isn't poinsoned");
            let ops = inner_w
                .enter()
                .expect("We deref through a write handle so this enter never fails")
                .propagate_static_route(self.router_id);
            for op in ops {
                inner_w.append(op);
            }
            inner_w.publish();
        }
    }

    pub async fn propagate_selected_routes(self) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(ROUTE_PROPAGATION_INTERVAL)).await;

            trace!("Propagating selected routes");

            let mut inner_w = self.inner_w.lock().expect("Mutex isn't poinsoned");
            let ops = inner_w
                .enter()
                .expect("We deref through a write handle so this enter never fails")
                .propagate_selected_routes();
            for op in ops {
                inner_w.append(op);
            }
            inner_w.publish();
        }
    }

    async fn start_periodic_hello_sender(self) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(HELLO_INTERVAL as u64)).await;

            for peer in self.peer_interfaces().iter_mut() {
                let hello = ControlPacket::new_hello(peer, HELLO_INTERVAL);
                peer.set_time_last_received_hello(tokio::time::Instant::now());

                if let Err(error) = peer.send_control_packet(hello) {
                    error!("Error sending hello to peer: {}", error);
                }
            }
        }
    }
}

pub struct RouterInner {
    peer_interfaces: Vec<Peer>,
    routing_table: RoutingTable,
    source_table: SourceTable,
    router_seqno: SeqNo,
    last_seqno_bump: Instant,
    static_routes: Vec<StaticRoute>,
    // map that contains the overlay ips of peers and their respective public keys
    dest_pubkey_map: IpPubkeyMap,
    expired_source_key_sink: mpsc::Sender<SourceKey>,
    expired_route_entry_sink: mpsc::Sender<(RouteKey, RouteExpirationType)>,
}

impl RouterInner {
    pub fn new(
        expired_source_key_sink: mpsc::Sender<SourceKey>,
        expired_route_entry_sink: mpsc::Sender<(RouteKey, RouteExpirationType)>,
    ) -> Result<Self, Box<dyn Error>> {
        let router_inner = RouterInner {
            peer_interfaces: Vec::new(),
            routing_table: RoutingTable::new(),
            source_table: SourceTable::new(),
            router_seqno: SeqNo::default(),
            last_seqno_bump: Instant::now(),
            static_routes: vec![],
            dest_pubkey_map: IpPubkeyMap::new(),
            expired_source_key_sink,
            expired_route_entry_sink,
        };

        Ok(router_inner)
    }
    fn remove_peer_interface(&mut self, peer: &Peer) {
        self.peer_interfaces.retain(|p| p != peer);
    }

    fn send_update(&self, peer: &Peer, update: babel::Update) -> Option<RouterOpLogEntry> {
        // before sending an update, the source table might need to be updated
        let metric = update.metric();
        let seqno = update.seqno();
        let router_id = update.router_id();
        let subnet = update.subnet();

        let source_key = SourceKey::new(subnet, router_id);

        // Nothing to do on route retraction.
        let op = if metric.is_infinite() {
            None
        } else if let Some(source_entry) = self.source_table.get(&source_key) {
            // if seqno of the update is greater than the seqno in the source table, update the source table
            if !seqno.lt(&source_entry.seqno()) {
                Some(RouterOpLogEntry::InsertSourceEntry(
                    source_key,
                    FeasibilityDistance::new(metric, seqno),
                ))
            }
            // if seqno of the update is equal to the seqno in the source table, update the source table if the metric (of the update) is lower
            else if seqno == source_entry.seqno() && source_entry.metric() > metric {
                Some(RouterOpLogEntry::InsertSourceEntry(
                    source_key,
                    FeasibilityDistance::new(metric, source_entry.seqno()),
                ))
            } else {
                None
            }
        }
        // no entry for this source key, so insert it
        else {
            Some(RouterOpLogEntry::InsertSourceEntry(
                source_key,
                FeasibilityDistance::new(metric, seqno),
            ))
        };

        // send the update to the peer
        trace!("Sending update to peer");
        if let Err(e) = peer.send_control_packet(ControlPacket::Update(update)) {
            error!("Error sending update to peer: {:?}", e);
        }

        op
    }

    fn propagate_static_route(&self, router_id: RouterId) -> Vec<RouterOpLogEntry> {
        let mut updates = vec![];
        for peer in self.peer_interfaces.iter() {
            updates.extend(self.propagate_static_route_to_peer(peer, router_id))
        }

        updates
    }

    fn propagate_static_route_to_peer(
        &self,
        peer: &Peer,
        router_id: RouterId,
    ) -> Vec<RouterOpLogEntry> {
        let mut updates = vec![];

        for sr in self.static_routes.iter() {
            let update = babel::Update::new(
                UPDATE_INTERVAL,
                self.router_seqno, // updates receive the seqno of the router
                Metric::from(0),   // Static route has no further hop costs
                sr.subnet,
                router_id,
            );
            updates.push((peer.clone(), update));
        }

        updates
            .into_iter()
            .filter_map(|(peer, update)| self.send_update(&peer, update))
            .collect()
    }

    fn propagate_selected_route(
        &self,
        subnet: Subnet,
        router_id: RouterId,
    ) -> Vec<RouterOpLogEntry> {
        let mut updates = vec![];
        let (seqno, cost, router_id, maybe_neigh) =
            if let Some(sre) = self.routing_table.lookup_selected(subnet.address()) {
                (
                    sre.seqno(),
                    sre.metric() + Metric::from(sre.neighbour().link_cost()),
                    sre.source().router_id(),
                    Some(sre.neighbour().clone()),
                )
            } else {
                // TODO: fix this by retaining the route for some time after a retraction.
                info!("Retracting route for {subnet}");
                (self.router_seqno, Metric::infinite(), router_id, None)
            };

        for peer in self.peer_interfaces.iter() {
            // Don't send updates for a route to the next hop of the route, as that peer will never
            // select the route through us (that would caus a routing loop). The protocol can
            // handle this just fine, leaving this out is essentially an easy optimization.
            if let Some(ref neigh) = maybe_neigh {
                if peer == neigh {
                    continue;
                }
            }
            let update = babel::Update::new(UPDATE_INTERVAL, seqno, cost, subnet, router_id);
            debug!(
                "Propagating route update for {} to {}",
                subnet,
                peer.connection_identifier()
            );
            updates.push((peer.clone(), update));
        }

        updates
            .into_iter()
            .filter_map(|(peer, update)| self.send_update(&peer, update))
            .collect()
    }

    fn propagate_selected_routes_to_peer(&self, peer: &Peer) -> Vec<RouterOpLogEntry> {
        let mut updates = vec![];
        for (srk, sre) in self.routing_table.iter().filter(|(_, sre)| sre.selected()) {
            let neigh_link_cost = Metric::from(sre.neighbour().link_cost());
            // Don't send updates for a route to the next hop of the route, as that peer will never
            // select the route through us (that would caus a routing loop). The protocol can
            // handle this just fine, leaving this out is essentially an easy optimization.
            if peer == sre.neighbour() {
                continue;
            }
            let update = babel::Update::new(
                UPDATE_INTERVAL,
                sre.seqno(),
                // the cost of the route is the cost of the route + the cost of the link to the next-hop
                sre.metric() + neigh_link_cost,
                srk.subnet(),
                sre.source().router_id(),
            );
            debug!(
                "Propagating route update for {} to {} | D({}, {})",
                srk.subnet(),
                peer.connection_identifier(),
                sre.seqno(),
                sre.metric() + neigh_link_cost,
            );
            updates.push((peer.clone(), update));
        }

        updates
            .into_iter()
            .filter_map(|(peer, update)| self.send_update(&peer, update))
            .collect()
    }

    fn propagate_selected_routes(&self) -> Vec<RouterOpLogEntry> {
        let mut updates = vec![];
        for peer in self.peer_interfaces.iter() {
            updates.extend(self.propagate_selected_routes_to_peer(peer));
        }
        updates
    }
}

enum RouterOpLogEntry {
    /// Add a destination public key and shared secret for this router.
    AddDestPubkey(Subnet, PublicKey, SharedSecret),
    /// Add a new peer to the router.
    AddPeer(Peer),
    /// Removes a peer from the router.
    RemovePeer(Peer),
    /// Insert a new entry in the source table.
    InsertSourceEntry(SourceKey, FeasibilityDistance),
    /// Remove an entry from the source table.
    RemoveSourceEntry(SourceKey),
    /// Insert a new entry in the routing table.
    InsertRoute(RouteKey, RouteEntry),
    /// Removes a route with the given route key.
    RemoveRoute(RouteKey),
    /// Unselect the route defined by the route key.
    UnselectRoute(RouteKey),
    /// Select the route defined by the route key.
    SelectRoute(RouteKey),
    /// Update the route entry associated to the given route key in the fallback route table, if
    /// one exists
    UpdateRouteEntry(RouteKey, SeqNo, Metric, RouterId),
    /// Sets the static routes of the router to the provided value.
    SetStaticRoutes(Vec<StaticRoute>),
    /// Increment the sequence number of the router.
    BumpSequenceNumber(Instant),
}

impl left_right::Absorb<RouterOpLogEntry> for RouterInner {
    fn absorb_first(&mut self, operation: &mut RouterOpLogEntry, _: &Self) {
        match operation {
            RouterOpLogEntry::AddDestPubkey(dest, pk, ss) => {
                self.dest_pubkey_map.insert(*dest, *pk, ss.clone());
            }
            RouterOpLogEntry::AddPeer(peer) => self.peer_interfaces.push(peer.clone()),
            RouterOpLogEntry::RemovePeer(peer) => {
                self.remove_peer_interface(peer);
            }
            RouterOpLogEntry::InsertSourceEntry(sk, fd) => {
                self.source_table
                    .insert(*sk, *fd, self.expired_source_key_sink.clone());
            }
            RouterOpLogEntry::RemoveSourceEntry(sk) => {
                self.source_table.remove(sk);
            }
            RouterOpLogEntry::InsertRoute(rk, re) => {
                self.routing_table.insert(
                    rk.clone(),
                    re.clone(),
                    self.expired_route_entry_sink.clone(),
                );
            }
            RouterOpLogEntry::RemoveRoute(rk) => {
                self.routing_table.remove(rk);
            }
            RouterOpLogEntry::UnselectRoute(rk) => {
                self.routing_table.unselect_route(rk);
            }
            RouterOpLogEntry::SelectRoute(rk) => {
                self.routing_table.select_route(rk);
            }
            RouterOpLogEntry::UpdateRouteEntry(rk, seqno, metric, pk) => {
                if let Some(re) = self.routing_table.get_mut(rk) {
                    re.update_seqno(*seqno);
                    re.update_metric(*metric);
                    re.update_router_id(*pk);
                    self.routing_table
                        .reset_route_timer(rk, self.expired_route_entry_sink.clone());
                }
            }
            RouterOpLogEntry::SetStaticRoutes(static_routes) => {
                self.static_routes = static_routes.clone();
            }
            RouterOpLogEntry::BumpSequenceNumber(ts) => {
                self.router_seqno += 1;
                self.last_seqno_bump = *ts;
            }
        }
    }

    fn sync_with(&mut self, first: &Self) {
        *self = first.clone()
    }
}

impl Clone for RouterInner {
    fn clone(&self) -> Self {
        let RouterInner {
            peer_interfaces,
            routing_table,
            source_table,
            router_seqno,
            last_seqno_bump,
            static_routes,
            dest_pubkey_map,
            expired_source_key_sink,
            expired_route_entry_sink,
        } = self;
        let mut new_source_table = SourceTable::new();
        for (k, v) in source_table.iter() {
            new_source_table.insert(*k, *v, expired_source_key_sink.clone());
        }
        let mut new_routing_table = RoutingTable::new();
        for (k, v) in routing_table.iter() {
            new_routing_table.insert(k.clone(), v.clone(), expired_route_entry_sink.clone());
        }
        RouterInner {
            peer_interfaces: peer_interfaces.clone(),
            routing_table: new_routing_table,
            source_table: new_source_table,
            router_seqno: *router_seqno,
            last_seqno_bump: *last_seqno_bump,
            static_routes: static_routes.clone(),
            dest_pubkey_map: dest_pubkey_map.clone(),
            expired_source_key_sink: expired_source_key_sink.clone(),
            expired_route_entry_sink: expired_route_entry_sink.clone(),
        }
    }
}
