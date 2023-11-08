use crate::{
    babel::{self, SeqNoRequest},
    crypto::{PublicKey, SecretKey, SharedSecret},
    filters::RouteUpdateFilter,
    ip_pubkey::IpPubkeyMap,
    metric::Metric,
    packet::{ControlPacket, DataPacket},
    peer::Peer,
    router_id::RouterId,
    routing_table::{RouteEntry, RouteKey, RoutingTable},
    sequence_number::SeqNo,
    source_table::{FeasibilityDistance, SourceKey, SourceTable},
    subnet::Subnet,
};
use left_right::{ReadHandle, WriteHandle};
use log::{debug, error, info, trace, warn};
use std::{
    error::Error,
    fmt::Debug,
    net::{IpAddr, Ipv6Addr},
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender};

const HELLO_INTERVAL: u16 = 4;
const IHU_INTERVAL: u16 = HELLO_INTERVAL * 3;
const UPDATE_INTERVAL: u16 = HELLO_INTERVAL * 4;
const ROUTE_PROPAGATION_INTERVAL: u64 = 3;
const DEAD_PEER_TRESHOLD: u64 = 8;

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

        let router_inner = RouterInner::new()?;
        let (mut inner_w, inner_r) = left_right::new_from_empty(router_inner);
        inner_w.append(RouterOpLogEntry::SetStaticRoutes(static_routes));
        inner_w.publish();

        let router = Router {
            inner_w: Arc::new(Mutex::new(inner_w)),
            inner_r,
            router_id: RouterId::new(node_keypair.1),
            node_keypair,
            router_data_tx,
            router_control_tx,
            node_tun,
            node_tun_subnet,
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

        tokio::spawn(Router::check_for_dead_peers(
            router.clone(),
            RouterId::new(router.node_keypair.1),
        ));

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
        debug!("Adding peer {} to router", peer.overlay_ip());
        self.inner_w
            .lock()
            .expect("Mutex isn't poinsoned")
            .append(RouterOpLogEntry::AddPeer(peer))
            .publish();
    }

    pub fn peer_exists(&self, peer_underlay_ip: IpAddr) -> bool {
        self.inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles")
            .peer_exists(peer_underlay_ip)
    }

    pub fn node_secret_key(&self) -> SecretKey {
        self.node_keypair.0.clone()
    }

    pub fn node_public_key(&self) -> PublicKey {
        self.node_keypair.1
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

    pub fn print_selected_routes(&self) {
        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");

        let routing_table = &inner.selected_routing_table;
        for (route_key, route_entry) in routing_table.iter() {
            println!("Route key: {}", route_key);
            println!(
                "Route: {} (with next-hop: {}, metric: {}, seqno: {}, selected: {})",
                route_key.subnet(),
                route_entry.neighbour().underlay_ip(),
                route_entry.metric(),
                route_entry.seqno(),
                route_entry.selected()
            );
            // println!("As advertised by: {:?}", route.1.source.router_id);
        }
    }

    pub fn print_fallback_routes(&self) {
        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");

        let routing_table = &inner.fallback_routing_table;
        for (route_key, route_entry) in routing_table.iter() {
            println!("Route key: {}", route_key);
            println!(
                "Route: {} (with next-hop: {:?}, metric: {}, seqno: {}, selected: {})",
                route_key.subnet(),
                route_entry.neighbour().underlay_ip(),
                route_entry.metric(),
                route_entry.seqno(),
                route_entry.selected()
            );
            //println!("As advertised by: {:?}", route.1.source.router_id);
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

    async fn check_for_dead_peers(self, router_id: RouterId) {
        let ihu_threshold = tokio::time::Duration::from_secs(DEAD_PEER_TRESHOLD);

        loop {
            // check for dead peers every second
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            trace!("Checking for dead peers");

            let mut inner_w = self.inner_w.lock().expect("Mutex is not poisoned");

            let dead_peers = {
                // a peer is assumed dead when the peer's last sent ihu exceeds a threshold
                let mut dead_peers = Vec::new();
                for peer in inner_w
                    .enter()
                    .expect("We deref through a write handle so this enter never fails")
                    .peer_interfaces
                    .iter()
                {
                    // check if the peer's last_received_ihu is greater than the threshold
                    if peer.time_last_received_ihu().elapsed() > ihu_threshold {
                        // peer is dead
                        info!("Peer {:?} is dead", peer.overlay_ip());
                        dead_peers.push(peer.clone());
                    }
                }
                dead_peers
            };

            // vec to store retraction update that need to be sent
            let mut retraction_updates = Vec::<ControlPacket>::with_capacity(dead_peers.len());

            // remove the peer from the peer_interfaces and the routes
            for dead_peer in dead_peers {
                inner_w.append(RouterOpLogEntry::RemovePeer(dead_peer.clone()));

                info!("Sending retraction for {} to peers", dead_peer.overlay_ip());

                // create retraction update for each dead peer
                let retraction_update = ControlPacket::new_update(
                    UPDATE_INTERVAL,
                    inner_w.enter().expect("Write handle is saved on router so it is not dropped before the read handles").router_seqno,
                    Metric::infinite(),
                    Subnet::new(dead_peer.overlay_ip(), 64).expect("TODO: fix this"), 
                    router_id,
                );
                retraction_updates.push(retraction_update);
            }

            // Flush now, so when we aquire the next read handle the dead peers will have been
            // removed.
            inner_w.publish();

            // send retraction update for the dead peer
            // when other nodes receive this update (with metric 0XFFFF), they should also remove the routing tables entries with that peer as neighbor
            for peer in inner_w
                .enter()
                .expect("We deref through a write handle so this enter never fails")
                .peer_interfaces
                .iter()
            {
                for ru in retraction_updates.iter() {
                    if let Err(e) = peer.send_control_packet(ru.clone()) {
                        error!("Error sending retraction update to peer: {e}");
                    }
                }
            }
        }
    }

    async fn handle_incoming_control_packet(
        self,
        mut router_control_rx: UnboundedReceiver<(ControlPacket, Peer)>,
    ) {
        while let Some((control_packet, source_peer)) = router_control_rx.recv().await {
            trace!("Received control packet from {}", source_peer.underlay_ip());
            match control_packet {
                babel::Tlv::Hello(hello) => self.handle_incoming_hello(hello, source_peer),
                babel::Tlv::Ihu(ihu) => self.handle_incoming_ihu(ihu, source_peer),
                babel::Tlv::Update(update) => self.handle_incoming_update(update, source_peer),
                babel::Tlv::SeqNoRequest(seqno_request) => {
                    self.handle_incoming_seqno_request(seqno_request, source_peer)
                }
            }
        }
    }

    fn handle_incoming_hello(&self, _: babel::Hello, source_peer: Peer) {
        // Upon receiving and Hello message from a peer, this node has to send a IHU back
        let ihu = ControlPacket::new_ihu(IHU_INTERVAL, source_peer.overlay_ip());
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

    fn handle_incoming_seqno_request(&self, mut seqno_request: SeqNoRequest, source_peer: Peer) {
        // According to the babel rfc, we shoudl maintain a table of recent SeqNo requests and
        // periodically retry requests without reply. We will however not do this for now and rely
        // on the fact that we have stable links in general.

        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");

        // If we have a selected route for the prefix, and its router id is different from the
        // requested router id, or the router id is the same and the requested sequence number is
        // not smaller than the sequence number of the selected route, send an update for the route
        // to the peer (triggered update).
        if let Some(route_entry) = inner
            .selected_routing_table
            .lookup(seqno_request.prefix().address())
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
            // Bump router seqno
            // TODO: should we only send an update to the peer who sent the seqno request
            // instad of updating all our peers?
            drop(inner);
            let mut inner_w = self.inner_w.lock().expect("Mutex isn't poisoned");
            debug!("Bumping local router sequence number");
            inner_w.append(RouterOpLogEntry::BumpSequenceNumber);
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

            let possible_routes = self.find_all_routes(seqno_request.prefix());

            // First only consider feasible routes.
            for re in &possible_routes {
                if !re.metric().is_infinite() && re.neighbour() != &source_peer {
                    debug!(
                        "Forwarding seqno request {} for {} to {}",
                        seqno_request.seqno(),
                        seqno_request.prefix(),
                        re.neighbour().underlay_ip()
                    );
                    if let Err(e) = re.neighbour().send_control_packet(seqno_request.into()) {
                        error!("Failed to foward seqno request: {e}");
                    }
                    return;
                }
            }

            // Finally consider infeasible routes as well.
            for re in possible_routes {
                if re.neighbour() != &source_peer {
                    debug!(
                        "Forwarding seqno request {} for {} to {}",
                        seqno_request.seqno(),
                        seqno_request.prefix(),
                        re.neighbour().underlay_ip()
                    );
                    if let Err(e) = re.neighbour().send_control_packet(seqno_request.into()) {
                        error!("Failed to foward seqno request: {e}");
                    }
                    return;
                }
            }
        }
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
        // we need the address of the neighbour; this corresponds to the source ip of the control struct as the update is received from the neighbouring peer
        let route_key_from_update = RouteKey::new(subnet, source_peer.clone());
        // used later to filter out static route
        if self.route_key_is_from_static_route(&route_key_from_update) {
            return;
        }

        let mut inner_w = self.inner_w.lock().expect("Mutex isn't poisoned");

        // check if a route entry with the same route key exists in both routing tables
        let (route_entry_exists, update_feasible) = {
            let inner = inner_w
                .enter()
                .expect("We deref through a write handle so this enter never fails");

            (
                inner
                    .selected_routing_table
                    .get(&route_key_from_update)
                    .is_some()
                    || inner
                        .fallback_routing_table
                        .get(&route_key_from_update)
                        .is_some(),
                inner.source_table.is_update_feasible(&update),
            )
        };

        debug!(
            "Got update packet from {} for subnet {} with metric {} and seqno {} and router-id {}. Route entry exists: {route_entry_exists} and update is feasible: {update_feasible}",
            source_peer.underlay_ip(),
            update.subnet(),
            update.metric(),
            update.seqno(),
            update.router_id()
        );

        // if no entry exists (based on prefix, plen AND neighbor field)
        if !route_entry_exists {
            if metric.is_infinite() || !update_feasible {
                debug!("Received retraction for unknown route");
            } else {
                // this means that the update is feasible and the metric is not infinite
                // create a new route entry and add it to the routing table (which requires a new source entry to be created as well)
                info!(
                    "Learned route for previously unknown subnet {} and neighbour {}",
                    subnet,
                    source_peer.underlay_ip(),
                );

                let source_key = SourceKey::new(subnet, router_id);
                let fd = FeasibilityDistance::new(metric, seqno);
                inner_w.append(RouterOpLogEntry::InsertSourceEntry(source_key, fd));

                let route_key = RouteKey::new(subnet, source_peer.clone());
                let route_entry =
                    RouteEntry::new(source_key, source_peer.clone(), metric, seqno, true);

                let mut to_remove = None;
                let mut add_fallback = false;
                if let Some(re) = inner_w
                    .enter()
                    .expect("We deref through a write handle so this enter never fails")
                    .selected_routing_table
                    .lookup(subnet.address())
                {
                    if metric < re.metric() {
                        // TODO: find better way to get the RouteKey instead of reconstructing it
                        to_remove = Some(RouteKey::new(subnet, re.neighbour().clone()))
                    } else {
                        add_fallback = true;
                    }
                }

                if add_fallback {
                    debug!(
                        "Added new route for {subnet} via {} as fallback route",
                        source_peer.underlay_ip()
                    );
                    inner_w.append(RouterOpLogEntry::InsertFallbackRoute(
                        route_key,
                        route_entry,
                    ));
                    return;
                }

                if let Some(rk) = to_remove {
                    debug!(
                        "Moved existing route for {subnet} via {} to fallback route",
                        rk.neighbour().underlay_ip()
                    );
                    inner_w.append(RouterOpLogEntry::UnselectRoute(rk));
                }

                // insert the route into selected (we might have placed one other route, that was previously the best, in the fallback)
                inner_w.append(RouterOpLogEntry::InsertSelectedRoute(
                    route_key,
                    route_entry,
                ));
            }
        } else {
            // check if the update is a retraction
            if update_feasible && metric.is_infinite() {
                debug!(
                    "Received retraction for {subnet} from {}",
                    source_peer.overlay_ip()
                );
                // if the update is a retraction, we remove the entry from the routing tables
                // we also remove the corresponding source entry???
                inner_w.append(RouterOpLogEntry::RemoveSelectedRoute(
                    route_key_from_update.clone(),
                ));
                inner_w.append(RouterOpLogEntry::RemoveFallbackRoute(route_key_from_update));
                // remove the corresponding source entry
                let source_key = SourceKey::new(subnet, router_id);
                inner_w.append(RouterOpLogEntry::RemoveSourceEntry(source_key));
                return;
            }

            let possible_entry = inner_w
                .enter()
                .expect("We deref through a write handle so this enter never fails")
                .selected_routing_table
                .get(&route_key_from_update)
                // clone to get rid of borrow into read handle
                .cloned();

            if let Some(route_entry) = possible_entry {
                // If the update is unfeasible, and it is for the selected route, send a seqno
                // request.
                if !update_feasible {
                    let fd = *inner_w
                        .enter()
                        .expect("We deref through a write handle so this enter never fails")
                        .source_table
                        .get(&route_entry.source())
                        .unwrap_or_else(|| {
                            panic!(
                                "A selected route for {} exists so a source entry must also exist",
                                route_entry.source().subnet()
                            )
                        });

                    debug!(
                        "Sending seqno_request to {} for seqno {} of {}",
                        source_peer.underlay_ip(),
                        fd.seqno() + 1,
                        update.subnet(),
                    );
                    if let Err(e) = source_peer.send_control_packet(
                        SeqNoRequest::new(
                            fd.seqno() + 1,
                            route_entry.source().router_id(),
                            update.subnet(),
                        )
                        .into(),
                    ) {
                        error!(
                            "Failed to send seqno request to {}: {e}",
                            source_peer.underlay_ip()
                        );
                    }
                }
                // if the entry is currently selected, the update is unfeasible, and the router-id of the update is equal
                // to the router-id of the entry, then we ignore the update
                if !update_feasible && route_entry.source().router_id() == router_id {
                    return;
                }

                inner_w.append(RouterOpLogEntry::UpdateSelectedRouteEntry(
                    route_key_from_update,
                    seqno,
                    metric,
                    router_id,
                ));
            }
            // otherwise
            else {
                inner_w.append(RouterOpLogEntry::UpdateFallbackRouteEntry(
                    route_key_from_update.clone(),
                    seqno,
                    metric,
                    router_id,
                ));

                if !update_feasible {
                    // if the update is unfeasible, we remove the entry from the selected routing table
                    inner_w.append(RouterOpLogEntry::RemoveSelectedRoute(route_key_from_update));
                    // TODO: should we remove it from the selected and add it to fallback here???
                }
            }
        }
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
            "Incoming data packet, with dest_ip: {} (side node, this node's tun addr is: {})",
            data_packet.dst_ip,
            node_tun_subnet
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
                    trace!("Error sending data packet, no route found");
                }
            }
        }
    }

    async fn handle_incoming_data_packet(self, mut router_data_rx: Receiver<DataPacket>) {
        while let Some(data_packet) = router_data_rx.recv().await {
            self.route_packet(data_packet);
        }
        warn!("Router data receiver stream ended");
    }

    pub fn select_best_route(&self, dest_ip: IpAddr) -> Option<RouteEntry> {
        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");
        let mut best_route;
        // first look in the selected routing table for a match on the prefix of dest_ip
        best_route = inner.selected_routing_table.lookup(dest_ip);
        // If there was no entry in the selected routing table, check for one in the fallback
        // routing table.
        // TODO: Is this even possible?
        if best_route.is_none() {
            best_route = inner.fallback_routing_table.lookup(dest_ip);
            if best_route.is_some() {
                // TODO: verify above todo and adapt as required.
                warn!(
                    "Found route for dest {:?} in fallback table but not in selected table",
                    dest_ip
                );
            }
        }

        // println!("\n\n best route towards {}: {:?}", dest_ip, best_route);

        best_route
    }

    /// Find all routes in both the selected and fallback routing table for a destination.
    fn find_all_routes(&self, subnet: Subnet) -> Vec<RouteEntry> {
        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");

        let mut routes = vec![];
        if let Some(re) = inner.selected_routing_table.lookup(subnet.address()) {
            routes.push(re);
        }

        for entry in inner.fallback_routing_table.lookup_all(subnet.address()) {
            routes.push(entry);
        }

        routes
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
                .propagate_selected_routes(self.router_id);
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

#[derive(Clone)]
pub struct RouterInner {
    peer_interfaces: Vec<Peer>,
    selected_routing_table: RoutingTable,
    fallback_routing_table: RoutingTable,
    source_table: SourceTable,
    router_seqno: SeqNo,
    static_routes: Vec<StaticRoute>,
    // map that contains the overlay ips of peers and their respective public keys
    dest_pubkey_map: IpPubkeyMap,
}

impl RouterInner {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let router_inner = RouterInner {
            peer_interfaces: Vec::new(),
            selected_routing_table: RoutingTable::new(),
            fallback_routing_table: RoutingTable::new(),
            source_table: SourceTable::new(),
            router_seqno: SeqNo::default(),
            static_routes: vec![],
            dest_pubkey_map: IpPubkeyMap::new(),
        };

        Ok(router_inner)
    }
    fn remove_peer_interface(&mut self, peer: Peer) {
        self.peer_interfaces.retain(|p| p != &peer);
    }

    fn send_update(&self, peer: &Peer, update: babel::Update) -> Option<RouterOpLogEntry> {
        // before sending an update, the source table might need to be updated
        let seqno = update.seqno();
        let metric = update.metric();
        let router_id = update.router_id();
        let subnet = update.subnet();

        let source_key = SourceKey::new(subnet, router_id);

        let op = if let Some(source_entry) = self.source_table.get(&source_key) {
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
        for sr in self.static_routes.iter() {
            for peer in self.peer_interfaces.iter() {
                let update = babel::Update::new(
                    UPDATE_INTERVAL,
                    self.router_seqno, // updates receive the seqno of the router
                    peer.link_cost().into(), // direct connection to other peer, so the only cost is the cost towards the peer
                    sr.subnet,
                    router_id,
                );
                updates.push((peer.clone(), update));
            }
        }

        updates
            .into_iter()
            .filter_map(|(peer, update)| self.send_update(&peer, update))
            .collect()
    }

    fn propagate_selected_routes(&self, router_id: RouterId) -> Vec<RouterOpLogEntry> {
        let mut updates = vec![];
        for (sre, srk) in self.selected_routing_table.iter() {
            for peer in self.peer_interfaces.iter() {
                let peer_link_cost = peer.link_cost();

                // convert sr.0.prefix to ipv6 addr
                let addr = if let IpAddr::V6(addr) = sre.subnet().address() {
                    addr
                } else {
                    continue;
                };

                let og_sender_pubkey = if let Some((pubkey, _)) = self.dest_pubkey_map.lookup(addr)
                {
                    RouterId::new(*pubkey)
                } else {
                    // if the prefix is not in the dest_pubkey_map, then we use the router_id of the node itself
                    router_id
                };

                let update = babel::Update::new(
                    UPDATE_INTERVAL,
                    srk.seqno(), // updates receive the seqno of the router
                    srk.metric() + Metric::from(peer_link_cost),
                    // the cost of the route is the cost of the route + the cost of the link to the peer
                    sre.subnet(),
                    // we looked for the router_id, which is a public key, in the dest_pubkey_map
                    // if the router_id is not in the map, then the route came from the node itself
                    og_sender_pubkey,
                );
                debug!(
                    "Propagating route update for {} to {}",
                    sre.subnet(),
                    peer.underlay_ip()
                );
                updates.push((peer.clone(), update));
            }
        }

        updates
            .into_iter()
            .filter_map(|(peer, update)| self.send_update(&peer, update))
            .collect()
    }

    fn peer_exists(&self, peer_underlay_ip: IpAddr) -> bool {
        self.peer_interfaces
            .iter()
            .any(|peer| peer.underlay_ip() == peer_underlay_ip)
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
    /// Insert a new entry in the fallback routing table.
    InsertFallbackRoute(RouteKey, RouteEntry),
    /// Insert a new entry in the selected routing table.
    InsertSelectedRoute(RouteKey, RouteEntry),
    /// Removes a source entry with the given source key.
    RemoveSourceEntry(SourceKey),
    /// Removes a fallback route with the given route key.
    RemoveFallbackRoute(RouteKey),
    /// Removes a selected route with the given route key.
    RemoveSelectedRoute(RouteKey),
    /// Move a previously selected route from the selected routing table to the fallback routing
    /// table
    UnselectRoute(RouteKey),
    /// Update the route entry associated to the given route key in the fallback route table, if
    /// one exists
    UpdateFallbackRouteEntry(RouteKey, SeqNo, Metric, RouterId),
    /// Update the route entry associated to the given route key in the selected route table, if
    /// one exists
    UpdateSelectedRouteEntry(RouteKey, SeqNo, Metric, RouterId),
    /// Sets the static routes of the router to the provided value.
    SetStaticRoutes(Vec<StaticRoute>),
    /// Increment the sequence number of the router.
    BumpSequenceNumber,
}

impl left_right::Absorb<RouterOpLogEntry> for RouterInner {
    fn absorb_first(&mut self, operation: &mut RouterOpLogEntry, _: &Self) {
        match operation {
            RouterOpLogEntry::AddDestPubkey(dest, pk, ss) => {
                self.dest_pubkey_map.insert(*dest, *pk, ss.clone());
            }
            RouterOpLogEntry::AddPeer(peer) => self.peer_interfaces.push(peer.clone()),
            RouterOpLogEntry::RemovePeer(peer) => {
                self.remove_peer_interface(peer.clone());
                // remove the peer's routes from all routing tables (= all the peers that use the peer as next-hop)
                self.selected_routing_table.remove_peer(peer.clone());
                self.fallback_routing_table.remove_peer(peer.clone());
            }
            RouterOpLogEntry::InsertSourceEntry(sk, fd) => {
                self.source_table.insert(*sk, *fd);
            }
            RouterOpLogEntry::InsertFallbackRoute(rk, re) => {
                self.fallback_routing_table.insert(rk.clone(), re.clone());
            }
            RouterOpLogEntry::InsertSelectedRoute(rk, re) => {
                self.selected_routing_table.insert(rk.clone(), re.clone());
            }
            RouterOpLogEntry::UnselectRoute(rk) => {
                if let Some(mut old_selected) = self.selected_routing_table.remove(rk) {
                    old_selected.set_selected(false);
                    self.fallback_routing_table.insert(rk.clone(), old_selected);
                }
            }
            RouterOpLogEntry::RemoveSourceEntry(se) => self.source_table.remove(se),
            RouterOpLogEntry::RemoveFallbackRoute(rk) => {
                self.fallback_routing_table.remove(rk);
            }
            RouterOpLogEntry::RemoveSelectedRoute(rk) => {
                self.selected_routing_table.remove(rk);
            }
            // TODO: this is very inneficient as it might delete the routing table set
            RouterOpLogEntry::UpdateFallbackRouteEntry(rk, seqno, metric, pk) => {
                if let Some(mut re) = self.fallback_routing_table.remove(rk) {
                    re.update_seqno(*seqno);
                    re.update_metric(*metric);
                    re.update_router_id(*pk);
                    self.fallback_routing_table.insert(rk.clone(), re)
                }
            }
            // TODO: this is very inneficient as it might delete the routing table set
            RouterOpLogEntry::UpdateSelectedRouteEntry(rk, seqno, metric, pk) => {
                if let Some(mut re) = self.selected_routing_table.remove(rk) {
                    re.update_seqno(*seqno);
                    re.update_metric(*metric);
                    re.update_router_id(*pk);
                    self.selected_routing_table.insert(rk.clone(), re);
                }
            }
            RouterOpLogEntry::SetStaticRoutes(static_routes) => {
                self.static_routes = static_routes.clone();
            }
            RouterOpLogEntry::BumpSequenceNumber => {
                self.router_seqno += 1;
            }
        }
    }

    fn sync_with(&mut self, first: &Self) {
        *self = first.clone()
    }
}
