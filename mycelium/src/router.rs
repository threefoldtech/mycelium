use crate::{
    babel::{self, Hello, Ihu, RouteRequest, SeqNoRequest, Update},
    crypto::{PacketBuffer, PublicKey, SecretKey, SharedSecret},
    filters::RouteUpdateFilter,
    metric::Metric,
    metrics::Metrics,
    packet::{ControlPacket, DataPacket},
    peer::Peer,
    router_id::RouterId,
    routing_table::{RouteEntry, RouteKey, RouteList, RoutingTable},
    seqno_cache::{SeqnoCache, SeqnoRequestCacheKey},
    sequence_number::SeqNo,
    source_table::{FeasibilityDistance, SourceKey, SourceTable},
    subnet::Subnet,
};
use etherparse::{
    icmpv6::{DestUnreachableCode, TimeExceededCode},
    Icmpv6Type,
};
use std::{
    error::Error,
    net::IpAddr,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use tokio::sync::mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender};
use tracing::{debug, error, info, trace, warn};

/// Time between HELLO messags, in seconds
const HELLO_INTERVAL: u64 = 20;
/// Time filled in in IHU packet
const IHU_INTERVAL: Duration = Duration::from_secs(HELLO_INTERVAL * 3);
/// Max time used in UPDATE packets. For local (static) routes this is the timeout they are
/// advertised with.
const UPDATE_INTERVAL: Duration = Duration::from_secs(HELLO_INTERVAL * 3 * 5);
/// Time between route table dumps to peers.
const ROUTE_PROPAGATION_INTERVAL: Duration = UPDATE_INTERVAL;
/// Amount of seconds that can elapse before we consider a [`Peer`] as dead from the routers POV.
/// Since IHU's are sent in response to HELLO packets, this MUST be greater than the
/// [`HELLO_INTERVAL`].
///
/// We allow missing 1 hello, + some latency, so 2 HELLO's + 3 seconds for latency.
const DEAD_PEER_THRESHOLD: Duration = Duration::from_secs(HELLO_INTERVAL * 2 + 3);
/// The duration between checks for dead peers in the router. This check only looks for peers where
/// time since the last IHU exceeds DEAD_PEER_THRESHOLD.
const DEAD_PEER_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// Amount of time to wait between consecutive seqno bumps of the local router seqno.
const SEQNO_BUMP_TIMEOUT: Duration = Duration::from_secs(4);

/// Metric change of more than 10 is considered a large change.
const BIG_METRIC_CHANGE_TRESHOLD: Metric = Metric::new(10);

/// The amount a metric of a route needs to improve before we will consider switching to it.
const SIGNIFICANT_METRIC_IMPROVEMENT: Metric = Metric::new(10);

/// Hold retracted routes for 1 minute before purging them from the [`RoutingTable`].
const RETRACTED_ROUTE_HOLD_TIME: Duration = Duration::from_secs(60);

/// The interval specified in updates if the update won't be repeated.
const INTERVAL_NOT_REPEATING: Duration = Duration::from_millis(0);

pub struct Router<M> {
    routing_table: RoutingTable,
    peer_interfaces: Arc<RwLock<Vec<Peer>>>,
    source_table: Arc<RwLock<SourceTable>>,
    // Router SeqNo and last time it was bumped
    router_seqno: Arc<RwLock<(SeqNo, Instant)>>,
    static_routes: Vec<Subnet>,
    router_id: RouterId,
    node_keypair: (SecretKey, PublicKey),
    router_data_tx: Sender<DataPacket>,
    router_control_tx: UnboundedSender<(ControlPacket, Peer)>,
    node_tun: UnboundedSender<DataPacket>,
    node_tun_subnet: Subnet,
    update_filters: Arc<Vec<Box<dyn RouteUpdateFilter + Send + Sync>>>,
    /// Channel injected into peers, so they can notify the router if they exit.
    dead_peer_sink: mpsc::Sender<Peer>,
    /// Channel to notify the router of expired SourceKey's.
    expired_source_key_sink: mpsc::Sender<SourceKey>,
    seqno_cache: SeqnoCache,
    metrics: M,
}

impl<M> Router<M>
where
    M: Metrics + Clone + Send + 'static,
{
    pub fn new(
        node_tun: UnboundedSender<DataPacket>,
        node_tun_subnet: Subnet,
        static_routes: Vec<Subnet>,
        node_keypair: (SecretKey, PublicKey),
        update_filters: Vec<Box<dyn RouteUpdateFilter + Send + Sync>>,
        metrics: M,
    ) -> Result<Self, Box<dyn Error>> {
        // Tx is passed onto each new peer instance. This enables peers to send control packets to the router.
        let (router_control_tx, router_control_rx) = mpsc::unbounded_channel();
        // Tx is passed onto each new peer instance. This enables peers to send data packets to the router.
        let (router_data_tx, router_data_rx) = mpsc::channel::<DataPacket>(1000);
        let (expired_source_key_sink, expired_source_key_stream) = mpsc::channel(1);
        let (expired_route_entry_sink, expired_route_entry_stream) = mpsc::channel(1);
        let (dead_peer_sink, dead_peer_stream) = mpsc::channel(1);

        let routing_table = RoutingTable::new(expired_route_entry_sink);

        let router_id = RouterId::new(node_keypair.1);

        let seqno_cache = SeqnoCache::new();

        let router = Router {
            routing_table,
            peer_interfaces: Arc::new(RwLock::new(Vec::new())),
            source_table: Arc::new(RwLock::new(SourceTable::new())),
            router_seqno: Arc::new(RwLock::new((SeqNo::new(), Instant::now()))),
            static_routes,
            router_id,
            node_keypair,
            router_data_tx,
            router_control_tx,
            node_tun,
            node_tun_subnet,
            dead_peer_sink,
            expired_source_key_sink,
            seqno_cache,
            update_filters: Arc::new(update_filters),
            metrics,
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
        tokio::spawn(Router::propagate_static_routes(router.clone()));
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

    /// Get all peer interfaces known on the router.
    pub fn peer_interfaces(&self) -> Vec<Peer> {
        self.peer_interfaces.read().unwrap().clone()
    }

    /// Add a peer interface to the router.
    pub fn add_peer_interface(&self, peer: Peer) {
        debug!("Adding peer {} to router", peer.connection_identifier());
        self.peer_interfaces.write().unwrap().push(peer.clone());
        self.metrics.router_peer_added();

        // Request route table dump of peer
        debug!(
            "Requesting route table dump from {}",
            peer.connection_identifier()
        );
        if let Err(e) = peer.send_control_packet(RouteRequest::new(None).into()) {
            error!(
                "Failed to request route table dump from {}: {e}",
                peer.connection_identifier()
            );
        }
    }

    /// Get the public key used by the router
    pub fn node_public_key(&self) -> PublicKey {
        self.node_keypair.1
    }

    /// Get the [`RouterId`] of the `Router`.
    pub fn router_id(&self) -> RouterId {
        self.router_id
    }

    /// Get the [`PublicKey`] for an [`IpAddr`] if a route exists to the IP.
    pub fn get_pubkey(&self, ip: IpAddr) -> Option<PublicKey> {
        self.routing_table
            .best_routes(ip)
            // We can index here safely since we always have at least 1 route if we get
            // Option::Some.
            .map(|rl| rl[0].source().router_id().to_pubkey())
    }

    /// Gets the cached [`SharedSecret`] for the remote.
    pub fn get_shared_secret_from_dest(&self, dest: IpAddr) -> Option<SharedSecret> {
        self.routing_table
            .best_routes(dest)
            // We can index here safely since we always have at least 1 route if we get
            // Option::Some.
            .map(|rl| rl[0].shared_secret().clone())
    }

    /// Gets the cached [`SharedSecret`] based on the associated [`PublicKey`] of the remote.
    #[inline]
    pub fn get_shared_secret_by_pubkey(&self, dest: &PublicKey) -> Option<SharedSecret> {
        self.get_shared_secret_from_dest(dest.address().into())
    }

    /// Get a reference to this `Router`s' dead peer sink.
    pub fn dead_peer_sink(&self) -> &mpsc::Sender<Peer> {
        &self.dead_peer_sink
    }

    /// Remove a peer from the Router.
    fn remove_peer_interface(&self, peer: &Peer) {
        debug!(
            "Removing peer {} from the router",
            peer.connection_identifier()
        );
        self.peer_interfaces.write().unwrap().retain(|p| p != peer);
        self.metrics.router_peer_removed();
    }

    /// Get a list of all selected route entries.
    pub fn load_selected_routes(&self) -> Vec<RouteEntry> {
        self.routing_table
            .read()
            .iter()
            .filter_map(|(_, rl)| rl.selected())
            .cloned()
            .collect()
    }

    /// Get a list of all fallback route entries.
    pub fn load_fallback_routes(&self) -> Vec<RouteEntry> {
        self.routing_table
            .read()
            .iter()
            .flat_map(|(_, rl)| rl.iter())
            .filter(|re| !re.selected())
            .cloned()
            .collect()
    }

    /// Task which periodically checks for dead peers in the Router.
    async fn check_for_dead_peers(self) {
        loop {
            // check for dead peers every second
            tokio::time::sleep(DEAD_PEER_CHECK_INTERVAL).await;

            trace!("Checking for dead peers");

            let dead_peers = {
                // a peer is assumed dead when the peer's last sent ihu exceeds a threshold
                let mut dead_peers = Vec::new();
                for peer in self.peer_interfaces.read().unwrap().iter() {
                    // check if the peer's last_received_ihu is greater than the threshold
                    if peer.time_last_received_ihu().elapsed() > DEAD_PEER_THRESHOLD {
                        // peer is dead
                        info!("Peer {} is dead", peer.connection_identifier());
                        // Notify peer it's dead in case it's not aware of that yet.
                        peer.died();
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
    pub fn handle_dead_peer(&self, dead_peer: Peer) {
        self.metrics.router_peer_died();
        debug!(
            "Cleaning up peer {} which is reportedly dead",
            dead_peer.connection_identifier()
        );

        // Scope for routing table write access.
        let subnets_to_select = {
            let mut rt_write = self.routing_table.write();
            let mut subnets_to_select = Vec::new();

            for (subnet, mut rl) in rt_write.iter_mut() {
                let entry = rl.entry_mut(&dead_peer);
                if entry.is_none() {
                    continue;
                }
                let mut re = entry.unwrap();

                if re.selected() {
                    subnets_to_select.push(subnet);

                    // Don't clear selected flag yet, running route selection does that for us.
                    re.set_metric(Metric::infinite());
                    re.set_expires(tokio::time::Instant::now() + RETRACTED_ROUTE_HOLD_TIME);
                } else {
                    re.delete();
                }
            }

            self.remove_peer_interface(&dead_peer);

            subnets_to_select
        };

        // And run required route selection
        for subnet in subnets_to_select {
            self.route_selection(subnet);
        }
    }

    /// Run route selection for a given subnet.
    ///
    /// This will cause a triggered update if needed.
    fn route_selection(&self, subnet: Subnet) {
        self.metrics.router_route_selection_ran();
        debug!("Running route selection for {subnet}");

        let mut routes = self.routing_table.routes_mut(subnet);

        // No routes for subnet, nothing to do here.
        if routes.is_empty() {
            return;
        }

        // If there is no selected route there is nothing to do here. We keep expired routes in the
        // table for a while so updates of those should already have propagated to peers.
        if let Some(new_selected) = self.find_best_route(&routes).cloned() {
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

            routes.set_selected(new_selected.neighbour());
        } else if routes[0].selected() {
            // This means we went from a selected route to a non-selected route. Unselect route and
            // trigger update.
            // At this point we also send a seqno request to all peers which advertised this route
            // to us, to try and get an updated entry. This uses the source key of the unselected
            // entry.
            self.send_seqno_request(routes[0].source(), None, None);
            routes.unselect();
        }

        drop(routes);

        self.trigger_update(subnet, None);
    }

    /// Remove expired source keys from the router state.
    async fn process_expired_source_keys(
        self,
        mut expired_source_key_stream: mpsc::Receiver<SourceKey>,
    ) {
        while let Some(sk) = expired_source_key_stream.recv().await {
            debug!("Removing expired source entry {sk}");
            self.source_table.write().unwrap().remove(&sk);
            self.metrics.router_source_key_expired();
        }
        warn!("Expired source key processing halted");
    }

    /// Remove expired route keys from the router state.
    async fn process_expired_route_keys(
        self,
        mut expired_route_key_stream: mpsc::Receiver<RouteKey>,
    ) {
        while let Some(rk) = expired_route_key_stream.recv().await {
            debug!("Got expiration event for route {rk}");
            let subnet = rk.subnet();
            // Load current key
            let mut routes = self.routing_table.routes_mut(rk.subnet());
            // Bit of weird scoping for the mutable reference to entries.
            let was_selected = {
                let entry = routes.entry_mut(&rk);
                if entry.is_none() {
                    continue;
                }
                let mut entry = entry.unwrap();
                self.metrics.router_route_key_expired(!entry.selected());
                if !entry.metric().is_infinite() {
                    debug!("Route {rk} expired, increasing metric to infinity");
                    entry.set_metric(Metric::infinite());
                    entry.set_expires(tokio::time::Instant::now() + RETRACTED_ROUTE_HOLD_TIME);
                    entry.selected()
                } else {
                    debug!("Route {rk} expired, removing retracted route");
                    let selected = entry.selected();
                    entry.delete();
                    selected
                }
            };

            // Re run route selection if this was the selected route. We should do this before
            // publishing to potentially select a new route, however a time based expiraton of a
            // selected route generally means no other routes are viable anyway, so the short lived
            // black hole this could create is not really a concern.
            if was_selected {
                self.metrics.router_selected_route_expired();
                // Only inject selected route if we are simply retracting it, otherwise it is
                // actually already removed.
                debug!("Rerun route selection after expiration event");
                if let Some(r) = self.find_best_route(&routes).cloned() {
                    routes.set_selected(r.neighbour());

                    drop(routes);

                    // If the entry wasn't retracted yet, notify our peers.
                    if !r.metric().is_infinite() {
                        self.trigger_update(subnet, None);
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

    /// Task which ingests and processes control packets. This spawns another background task for
    /// every TLV type, and forwards the inbound packets to the proper background task.
    async fn handle_incoming_control_packet(
        self,
        mut router_control_rx: UnboundedReceiver<(ControlPacket, Peer)>,
    ) {
        let (hello_tx, hello_rx) = mpsc::unbounded_channel();
        let (ihu_tx, ihu_rx) = mpsc::unbounded_channel();
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let (rr_tx, rr_rx) = mpsc::unbounded_channel();
        let (sn_tx, sn_rx) = mpsc::unbounded_channel();

        tokio::spawn(self.clone().hello_processor(hello_rx));
        tokio::spawn(self.clone().ihu_processor(ihu_rx));
        tokio::spawn(self.clone().update_processor(update_rx));
        tokio::spawn(self.clone().route_request_processor(rr_rx));
        tokio::spawn(self.clone().seqno_request_processor(sn_rx));

        while let Some((control_packet, source_peer)) = router_control_rx.recv().await {
            // First update metrics with the remaining outstanding TLV's
            self.metrics.router_received_tlv();
            trace!(
                "Received control packet from {}",
                source_peer.connection_identifier()
            );

            // Route packet to proper work queue.
            match control_packet {
                babel::Tlv::Hello(hello) => {
                    if hello_tx.send((hello, source_peer)).is_err() {
                        break;
                    };
                }
                babel::Tlv::Ihu(ihu) => {
                    if ihu_tx.send((ihu, source_peer)).is_err() {
                        break;
                    };
                }
                babel::Tlv::Update(update) => {
                    if update_tx.send((update, source_peer)).is_err() {
                        break;
                    };
                }
                babel::Tlv::RouteRequest(route_request) => {
                    if rr_tx.send((route_request, source_peer)).is_err() {
                        break;
                    };
                }
                babel::Tlv::SeqNoRequest(seqno_request) => {
                    if sn_tx.send((seqno_request, source_peer)).is_err() {
                        break;
                    };
                }
            }
        }
    }

    /// Background task to process hello TLV's.
    async fn hello_processor(self, mut hello_rx: UnboundedReceiver<(Hello, Peer)>) {
        while let Some((hello, source_peer)) = hello_rx.recv().await {
            let start = std::time::Instant::now();

            if !source_peer.alive() {
                trace!("Dropping Hello TLV since sender is dead.");
                self.metrics.router_tlv_source_died();
                continue;
            }

            self.handle_incoming_hello(hello, source_peer);

            self.metrics
                .router_time_spent_handling_tlv(start.elapsed(), "hello");
        }
    }

    /// Background task to process IHU TLV's.
    async fn ihu_processor(self, mut ihu_rx: UnboundedReceiver<(Ihu, Peer)>) {
        while let Some((ihu, source_peer)) = ihu_rx.recv().await {
            let start = std::time::Instant::now();

            if !source_peer.alive() {
                trace!("Dropping IHU TLV since sender is dead.");
                self.metrics.router_tlv_source_died();
                continue;
            }

            self.handle_incoming_ihu(ihu, source_peer);

            self.metrics
                .router_time_spent_handling_tlv(start.elapsed(), "ihu");
        }
    }

    /// Background task to process Update TLV's.
    async fn update_processor(self, mut update_rx: UnboundedReceiver<(Update, Peer)>) {
        while let Some((update, source_peer)) = update_rx.recv().await {
            let start = std::time::Instant::now();

            if !source_peer.alive() {
                trace!("Dropping Update TLV since sender is dead.");
                self.metrics.router_tlv_source_died();
                continue;
            }

            self.handle_incoming_update(update, source_peer);

            self.metrics
                .router_time_spent_handling_tlv(start.elapsed(), "update");
        }
    }

    /// Background task to process Route Request TLV's.
    async fn route_request_processor(self, mut rr_rx: UnboundedReceiver<(RouteRequest, Peer)>) {
        while let Some((rr, source_peer)) = rr_rx.recv().await {
            let start = std::time::Instant::now();

            if !source_peer.alive() {
                trace!("Dropping Route request TLV since sender is dead.");
                self.metrics.router_tlv_source_died();
                continue;
            }

            self.handle_incoming_route_request(rr, source_peer);

            self.metrics
                .router_time_spent_handling_tlv(start.elapsed(), "route_request");
        }
    }

    /// Background task to process Seqno Request TLV's.
    async fn seqno_request_processor(self, mut sn_rx: UnboundedReceiver<(SeqNoRequest, Peer)>) {
        while let Some((sn, source_peer)) = sn_rx.recv().await {
            let start = std::time::Instant::now();

            if !source_peer.alive() {
                trace!("Dropping Route request TLV since sender is dead.");
                self.metrics.router_tlv_source_died();
                continue;
            }

            self.handle_incoming_seqno_request(sn, source_peer);

            self.metrics
                .router_time_spent_handling_tlv(start.elapsed(), "seqno");
        }
    }

    /// Handle a received hello TLV
    fn handle_incoming_hello(&self, _: babel::Hello, source_peer: Peer) {
        self.metrics.router_process_hello();
        // Upon receiving and Hello message from a peer, this node has to send a IHU back
        // TODO: properly calculate RX cost, for now just set the link cost.
        let ihu = ControlPacket::new_ihu(source_peer.link_cost().into(), IHU_INTERVAL, None);
        if source_peer.send_control_packet(ihu).is_err() {
            trace!(
                "Failed to send IHU reply to peer: {}",
                source_peer.connection_identifier()
            );
        }
    }

    /// Handle a received IHU TLV
    fn handle_incoming_ihu(&self, _: babel::Ihu, source_peer: Peer) {
        self.metrics.router_process_ihu();
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
        self.metrics
            .router_process_route_request(route_request.prefix().is_none());
        // Handle the case of a single subnet.
        if let Some(subnet) = route_request.prefix() {
            let update = if let Some(sre) = self
                .routing_table
                .routes(subnet)
                .as_ref()
                .and_then(|rl| rl.selected())
            {
                trace!("Advertising selected route for {subnet} after route request");
                // Optimization: Don't send an update if the selected route next-hop is the peer
                // who requested the route, as per the babel protocol the update will never be
                // accepted.
                if sre.neighbour() == &source_peer {
                    trace!("Not advertising route since the next-hop is the requesting peer");
                    return;
                }
                babel::Update::new(
                    advertised_update_interval(sre),
                    sre.seqno(),
                    sre.metric() + Metric::from(sre.neighbour().link_cost()),
                    subnet,
                    sre.source().router_id(),
                )
            }
            // Could be a request for a static route/subnet.
            else if let Some(static_route) = self
                .static_routes
                .iter()
                .find(|sr| sr.contains_subnet(&subnet))
            {
                trace!(
                    "Advertising static route {static_route} in response to route request for {subnet}"
                );
                babel::Update::new(
                    UPDATE_INTERVAL, // Static route is advertised with the default interval
                    self.router_seqno.read().unwrap().0, // Updates receive the seqno of the router
                    Metric::from(0), // Static route has no further hop costs
                    *static_route,
                    self.router_id,
                )
            }
            // If the requested route is not present, send a retraction
            else {
                trace!(
                    "Sending retraction for unknown subnet {subnet} in response to route request"
                );
                babel::Update::new(
                    INTERVAL_NOT_REPEATING,
                    self.router_seqno.read().unwrap().0, // Retractions receive the seqno of the router
                    Metric::infinite(),                  // Static route has no further hop costs
                    subnet,                              // Advertise the exact subnet requested
                    self.router_id, // Our own router ID, since we advertise this
                )
            };

            self.send_update(&source_peer, update);
        } else {
            // Requested a full route table dump
            trace!("Dumping route table after wildcard route request");
            self.propagate_selected_routes_to_peer(&source_peer);
            self.propagate_static_route_to_peer(&source_peer);
        }
    }

    /// Handle a received SeqNo request TLV.
    fn handle_incoming_seqno_request(&self, mut seqno_request: SeqNoRequest, source_peer: Peer) {
        self.metrics.router_process_seqno_request();
        // According to the babel rfc, we shoudl maintain a table of recent SeqNo requests and
        // periodically retry requests without reply. We will however not do this for now and rely
        // on the fact that we have stable links in general.

        // If we have a selected route for the prefix, and its router id is different from the
        // requested router id, or the router id is the same and the entries sequence number is
        // not smaller than the requested sequence number, send an update for the route
        // to the peer (triggered update).
        if let Some(route_entry) = self
            .routing_table
            .routes(seqno_request.prefix())
            .as_ref()
            .and_then(|rl| rl.selected())
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
                let update = babel::Update::new(
                    advertised_update_interval(route_entry),
                    route_entry.seqno(), // updates receive the seqno of the router
                    route_entry.metric() + Metric::from(source_peer.link_cost()),
                    // the cost of the route is the cost of the route + the cost of the link to the peer
                    route_entry.source().subnet(),
                    // we looked for the router_id, which is a public key, in the dest_pubkey_map
                    // if the router_id is not in the map, then the route came from the node itself
                    route_entry.source().router_id(),
                );

                self.send_update(&source_peer, update);

                self.metrics.router_seqno_request_reply_local();

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
        let (router_seqno, last_seqno_bump) = *self.router_seqno.read().unwrap();
        if seqno_request.router_id() == self.router_id
            && seqno_request.seqno().gt(&router_seqno)
            && self.static_routes.contains(&seqno_request.prefix())
        {
            if last_seqno_bump.elapsed() >= SEQNO_BUMP_TIMEOUT {
                trace!("Ignoring seqno bump request which happened too fast");
                return;
            }
            // Bump router seqno
            // TODO: should we only send an update to the peer who sent the seqno request
            // instad of updating all our peers?
            debug!("Bumping local router sequence number");
            // Scope the write lock on seqno
            {
                let mut router_seqno = self.router_seqno.write().unwrap();
                // First check again if we should bump
                if router_seqno.1.elapsed() >= SEQNO_BUMP_TIMEOUT {
                    trace!("Ignoring seqno bump request which happened too fast");
                    return;
                }
                // Bump seqno
                router_seqno.0 += 1;
                // Set last modified time
                router_seqno.1 = Instant::now();
            }

            self.propagate_static_routes_to_peers();

            self.metrics.router_seqno_request_bump_seqno();

            return;
        }

        // Otherwise, if the router-id from the request is not our own, we check the hop count
        // field. If it is at least 2, we decrement it by 1, and forward the packet. To do so, we
        // try to find a route to the subnet. First we check for a feasible route and send the
        // packet there if the next hop is not the sender of this packet. Otherwise, we check for
        // any route which might potentially be unfeasible, which also did not originate the
        // packet.
        if seqno_request.hop_count() < 2 {
            self.metrics.router_seqno_request_dropped_ttl();
            return;
        }

        if seqno_request.router_id() != self.router_id {
            seqno_request.decrement_hop_count();

            let srck = SeqnoRequestCacheKey {
                router_id: seqno_request.router_id(),
                subnet: seqno_request.prefix(),
                seqno: seqno_request.seqno(),
            };

            let mut visited_peers = vec![];
            if let Some((last_sent, visited)) = self.seqno_cache.info(&srck) {
                if last_sent.elapsed() < SEQNO_BUMP_TIMEOUT {
                    visited_peers = visited;
                }
            }

            if let Some(possible_routes) = self.routing_table.routes(seqno_request.prefix()) {
                {
                    let source_table = self.source_table.read().unwrap();
                    // First only consider feasible routes.
                    for re in possible_routes.iter().filter(|re| {
                        !visited_peers.contains(re.neighbour())
                            && source_table.route_feasible(re)
                            && re.neighbour() != &source_peer
                            && re.neighbour().alive()
                            && !re.metric().is_infinite()
                    }) {
                        debug!(
                            "Forwarding seqno request {} for {} to {}",
                            seqno_request.seqno(),
                            seqno_request.prefix(),
                            re.neighbour().connection_identifier()
                        );
                        if re
                            .neighbour()
                            .send_control_packet(seqno_request.clone().into())
                            .is_err()
                        {
                            trace!(
                                "Failed to foward seqno request to {}",
                                re.neighbour().connection_identifier(),
                            );
                            continue;
                        }

                        self.seqno_cache
                            .forward(srck, re.neighbour().clone(), Some(source_peer));

                        self.metrics.router_seqno_request_forward_feasible();

                        return;
                    }
                }

                // Finally consider infeasible routes as well.
                for re in possible_routes.iter().filter(|re| {
                    !visited_peers.contains(re.neighbour())
                        && re.neighbour() != &source_peer
                        && re.neighbour().alive()
                        && !re.metric().is_infinite()
                }) {
                    debug!(
                        "Forwarding seqno request {} for {} to {}",
                        seqno_request.seqno(),
                        seqno_request.prefix(),
                        re.neighbour().connection_identifier()
                    );
                    if re
                        .neighbour()
                        .send_control_packet(seqno_request.clone().into())
                        .is_err()
                    {
                        trace!(
                            "Failed to foward seqno request to infeasible peer {}",
                            re.neighbour().connection_identifier(),
                        );
                        continue;
                    }

                    self.seqno_cache
                        .forward(srck, re.neighbour().clone(), Some(source_peer));

                    self.metrics.router_seqno_request_forward_unfeasible();

                    return;
                }
            }
        }

        self.metrics.router_seqno_request_unhandled();
    }

    /// Finds the best feasible route from a list of [`route entries`](RouteEntry). It is possible
    /// for this method to select a retracted route. In this case, retraction updates should be
    /// send out.
    ///
    /// This method only selects a different best route if it is significantly better compared to
    /// the current route.
    fn find_best_route<'a>(&self, routes: &'a RouteList) -> Option<&'a RouteEntry> {
        let source_table = self.source_table.read().unwrap();
        let current = routes.selected();
        let best = routes
            .iter()
            // Infinite metrics are technically feasible, but for route selection we explicitly
            // don't want infinite metrics as those routes are unreachable.
            .filter(|re| !re.metric().is_infinite() && source_table.route_feasible(re))
            .min_by_key(|re| re.metric() + Metric::from(re.neighbour().link_cost()));

        if let (Some(best), Some(current)) = (best, current) {
            // If we swap to an actually different route, only do so if the metric is
            // significantly better OR if it is directly connected (metric 0).
            if (best.source() != current.source() || best.neighbour() != current.neighbour())
                && !(best.metric() + Metric::from(best.neighbour().link_cost())
                    < current.metric() + Metric::from(current.neighbour().link_cost())
                        - SIGNIFICANT_METRIC_IMPROVEMENT
                    || best.metric().is_direct())
            {
                debug!("maintaining currently selected route since new route is not significantly better");
                return Some(current);
            }
        }

        best
    }

    /// Handle a received update TLV
    fn handle_incoming_update(&self, update: babel::Update, source_peer: Peer) {
        self.metrics.router_process_update();
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

        // create route key from incoming update control struct
        let update_route_key = RouteKey::new(subnet, source_peer.clone());
        // used later to filter out static route
        if self.route_key_is_from_static_route(&update_route_key) {
            return;
        }

        // We accepted the update, check if we have a seqno request sent for this update
        let interested_peers = self.seqno_cache.remove(&SeqnoRequestCacheKey {
            router_id,
            subnet,
            seqno,
        });

        // We load all routes here from the routing table in memory. Because we hold the mutex for the
        // writer, this view is accurate and we can't diverge until the mutex is released. We will
        // then apply every action on the list of route entries both to the local list, and as a
        // RouterOpLogEntry. This is needed because publishing intermediate results might cause
        // readers to observe intermediate state, which could lead to problems.
        let (mut routing_table_entries, update_feasible) = {
            (
                self.routing_table.routes_mut(subnet),
                self.source_table
                    .read()
                    .unwrap()
                    .is_update_feasible(&update),
            )
        };

        // Take a deep copy of the old selected route if there is one, deep copy since we will
        // potentially mutate the route list so we can't keep a reference to it.
        let old_selected_route = routing_table_entries.selected().cloned();

        let maybe_existing_entry = routing_table_entries.entry_mut(&update_route_key);

        debug!(
             "Got update packet from {} for subnet {subnet} with metric {metric} and seqno {seqno} and router-id {router_id}. Route entry exists: {} and update is feasible: {update_feasible}",
             source_peer.connection_identifier(),
             maybe_existing_entry.is_some(),
         );

        // Track if we unselected the current existing route. This is required to avoid an issue
        // where we try to unselect the selected route twice if it is lost.
        let mut existing_route_unselected = false;

        if maybe_existing_entry.is_some() {
            let mut existing_entry = maybe_existing_entry.unwrap();
            // Unfeasible updates to the selected route are not applied, but we do request a seqno
            // bump.
            if existing_entry.selected()
                && !update_feasible
                && existing_entry.source().router_id() == router_id
            {
                self.send_seqno_request(existing_entry.source(), Some(source_peer), None);
                return;
            }

            // Otherwise we just update the entry.
            if existing_entry.metric().is_infinite() && update.metric().is_infinite() {
                // Retraction for retracted route, don't do anything. If we don't filter this
                // retracted routes can stay stuck if peer keep sending retractions to eachother
                // for this route.
                return;
            }
            existing_entry.set_seqno(seqno);
            existing_entry.set_metric(metric);
            existing_entry.set_router_id(router_id);
            existing_entry.set_expires(tokio::time::Instant::now() + route_hold_time(&update));
            // If the update is unfeasible the route must be unselected.
            if existing_entry.selected() && !update_feasible {
                existing_route_unselected = true;
                existing_entry.set_selected(false);
            }
        } else {
            // If there is no entry yet ignore unfeasible updates and retractions.
            if metric.is_infinite() || !update_feasible {
                debug!("Received unfeasible update | retraction for unknown route - neighbour");
                return;
            }

            // Create new entry in the route table
            let ss = self.node_keypair.0.shared_secret(&router_id.to_pubkey());
            maybe_existing_entry.or_insert(RouteEntry::new(
                SourceKey::new(subnet, router_id),
                source_peer.clone(),
                metric,
                seqno,
                false,
                tokio::time::Instant::now() + route_hold_time(&update),
                ss,
            ));
        }

        // Now that we applied the update, run route selection.
        let new_selected_route = self.find_best_route(&routing_table_entries).cloned();
        if let Some(ref nbr) = new_selected_route {
            // Install this route in the routing table.
            routing_table_entries.set_selected(nbr.neighbour());
        } else if let Some(ref osr) = old_selected_route {
            // If there is no new selected route, but there was one previously, update the routing
            // table. This is not covered above, as there only unfeasible updates cause a selected
            // route to be unselected. However, this may also be the result of a feasible update
            // (e.g. retraction with no feasible fallback routes).
            // Only do this if we did not unselect the existing route previously.
            // Regardless if we unselect the route here or not, we lost a selected route for the
            // subnet, so we try to refresh the route by sending out a seqno request to all
            // neigbours who advertised the route at some point.
            self.send_seqno_request(osr.source(), None, None);

            if !existing_route_unselected {
                // Since we know for sure a selected route existed we could technically unwrap
                // here.
                if let Some(mut selected_route) = routing_table_entries.selected_mut() {
                    selected_route.set_selected(false);
                }
            }
        };

        // Drop these here so the update gets reflected in the routing table, which is needed for a
        // possible triggered update later on.
        drop(routing_table_entries);

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
            self.trigger_update(subnet, None);
        } else if interested_peers.is_some() {
            debug!(
                "Send update to peers who registered interest through a seqno request for {subnet}"
            );
            self.trigger_update(subnet, interested_peers);
            // If we have some interest in an update because we forwarded a seqno request but we
            // aren't triggering an update, notify just the interested peers.
        }
    }

    /// Trigger an update for the given [`Subnet`]. If `peers` is [`None`], send the update to all
    /// peers the `Router` knows.
    fn trigger_update(&self, subnet: Subnet, peers: Option<Vec<Peer>>) {
        self.metrics.router_triggered_update();
        self.propagate_selected_route(subnet, peers);
    }

    /// Send a seqno request for a subnet. This can be sent to a given peer, or to all peers for
    /// the subnet if no peer is given.
    ///
    /// The SourceKey must exist in the source table.
    fn send_seqno_request(
        &self,
        source: SourceKey,
        to: Option<Peer>,
        request_origin: Option<Peer>,
    ) {
        let fd = match self.source_table.read().unwrap().get(&source) {
            Some(fd) => *fd,
            None => {
                // This can happen if you only have 1 peer, or in case we haven't announced our
                // subnets yet.
                debug!("Requesting seqno for source key {source} which does not exist in the source table");
                // Since we don't know the real seqno, just use the default. Either the peer will
                // reply if it has an adequate route, or it will forward the request.
                FeasibilityDistance::new(Metric::new(0), SeqNo::new())
            }
        };

        let sn: ControlPacket =
            SeqNoRequest::new(fd.seqno() + 1, source.router_id(), source.subnet()).into();

        let srck = SeqnoRequestCacheKey {
            router_id: source.router_id(),
            subnet: source.subnet(),
            seqno: fd.seqno() + 1,
        };

        let seqno_info = self.seqno_cache.info(&srck);

        let targets = if let Some(target) = to {
            vec![target]
        } else {
            // If we don't have a dedicated peer to send to, just send to every peer which
            // announced the subnet. But avoid repetitions. Additionally, if we don't have any
            // peer which announced the subnet (because all anounces are unfeasible)
            let mut peers_sent = vec![];
            if let Some((last_sent, visited)) = seqno_info {
                // If it's more than some time since we last sent an update to any peer for this
                // seqno request, send it again. We use the seqno bump timeout here, since that is
                // the quickest time between bumps from a peer.
                if last_sent.elapsed() < SEQNO_BUMP_TIMEOUT {
                    peers_sent = visited;
                }
            };

            let known_routes = self
                .routing_table
                .routes(source.subnet())
                .unwrap_or_default();

            // Make sure a broadcast only happens in case the local node originated the request.
            if known_routes.is_empty() && request_origin.is_none() {
                // If we don't know the route just ask all our peers.
                self.peer_interfaces
                    .read()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect()
            } else {
                known_routes
                    .iter()
                    .filter_map(|re| {
                        if !peers_sent.contains(re.neighbour()) {
                            Some(re.neighbour().clone())
                        } else {
                            None
                        }
                    })
                    .collect()
            }
        };

        for peer in targets {
            debug!(
                "Sending seqno_request to {} for seqno {} of {}",
                peer.connection_identifier(),
                fd.seqno() + 1,
                source.subnet(),
            );

            if peer.send_control_packet(sn.clone()).is_err() {
                trace!(
                    "Failed to send seqno request to {}",
                    peer.connection_identifier()
                );
                continue;
            }

            self.seqno_cache.forward(srck, peer, request_origin.clone());
        }
    }

    /// Checks if a route key is an exact match for a static route.
    fn route_key_is_from_static_route(&self, route_key: &RouteKey) -> bool {
        for sr in self.static_routes.iter() {
            if sr == &route_key.subnet() {
                return true;
            }
        }
        false
    }

    pub fn route_packet(&self, mut data_packet: DataPacket) {
        let node_tun_subnet = self.node_tun_subnet();

        trace!(
            "Incoming data packet {} -> {}",
            data_packet.src_ip,
            data_packet.dst_ip,
        );

        if data_packet.hop_limit < 2 {
            self.metrics.router_route_packet_ttl_expired();
            self.time_exceeded(data_packet);
            return;
        }
        data_packet.hop_limit -= 1;

        if node_tun_subnet.contains_ip(data_packet.dst_ip.into()) {
            self.metrics.router_route_packet_local();
            if let Err(e) = self.node_tun().send(data_packet) {
                error!("Error sending data packet to TUN interface: {:?}", e);
            }
        } else {
            match self.routing_table.selected_route(data_packet.dst_ip.into()) {
                Some(route_entry) => {
                    self.metrics.router_route_packet_forward();
                    if let Err(e) = route_entry.neighbour().send_data_packet(data_packet) {
                        error!(
                            "Error sending data packet to peer {}: {:?}",
                            route_entry.neighbour().connection_identifier(),
                            e
                        );
                    }
                }
                None => {
                    self.metrics.router_route_packet_no_route();
                    self.no_route_to_host(data_packet);
                }
            }
        }
    }

    /// Handle a received data packet.
    async fn handle_incoming_data_packet(self, mut router_data_rx: Receiver<DataPacket>) {
        while let Some(data_packet) = router_data_rx.recv().await {
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

    /// Send an oob icmp packet of the specified type in reply to the given DataPakcet.
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
        let shared_secret = match self.get_shared_secret_from_dest(data_packet.src_ip.into()) {
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

    /// Task to propagate the static routes periodically
    async fn propagate_static_routes(self) {
        loop {
            tokio::time::sleep(ROUTE_PROPAGATION_INTERVAL).await;

            trace!("Propagating static routes");

            for peer in self.peer_interfaces.read().unwrap().iter() {
                self.propagate_static_route_to_peer(peer)
            }
        }
    }

    /// Propagate all static routes to all known peers.
    fn propagate_static_routes_to_peers(&self) {
        for peer in self.peer_interfaces.read().unwrap().iter() {
            self.propagate_static_route_to_peer(peer);
        }
    }

    /// Task to propagate selected routes periodically
    async fn propagate_selected_routes(self) {
        loop {
            tokio::time::sleep(ROUTE_PROPAGATION_INTERVAL).await;

            trace!("Propagating selected routes");

            let start = Instant::now();
            self.propagate_selected_routes_to_peers();
            self.metrics
                .router_time_spent_periodic_propagating_selected_routes(start.elapsed());
        }
    }

    /// Task which periodically sends a Hello TLV to all known peers
    async fn start_periodic_hello_sender(self) {
        let hello_interval = Duration::from_secs(HELLO_INTERVAL);
        loop {
            tokio::time::sleep(hello_interval).await;

            for peer in self.peer_interfaces.read().unwrap().iter() {
                let hello = ControlPacket::new_hello(peer, hello_interval);
                peer.set_time_last_received_hello(tokio::time::Instant::now());

                if peer.send_control_packet(hello).is_err() {
                    trace!(
                        "Failed to send Hello TLV to dead peer {}",
                        peer.connection_identifier()
                    );
                }
            }
        }
    }

    /// Propagates the static routes to all known peers.
    fn propagate_selected_routes_to_peers(&self) {
        for peer in self.peer_interfaces.read().unwrap().iter() {
            self.propagate_selected_routes_to_peer(peer);
        }
    }

    /// Propagate the static routes to a single peer
    fn propagate_static_route_to_peer(&self, peer: &Peer) {
        for sr in self.static_routes.iter() {
            let update = babel::Update::new(
                UPDATE_INTERVAL,
                self.router_seqno.read().unwrap().0, // updates receive the seqno of the router
                Metric::from(0),                     // Static route has no further hop costs
                *sr,
                self.router_id,
            );
            self.send_update(peer, update);
        }
    }

    /// Propagate a selected route. Unless peers are specified, all knwon peers in the router are
    /// used.
    fn propagate_selected_route(&self, subnet: Subnet, peers: Option<Vec<Peer>>) {
        let (update, maybe_neigh) =
            if let Some(sre) = self.routing_table.selected_route(subnet.address()) {
                let update = babel::Update::new(
                    advertised_update_interval(&sre),
                    sre.seqno(),
                    sre.metric() + Metric::from(sre.neighbour().link_cost()),
                    sre.source().subnet(),
                    sre.source().router_id(),
                );
                (update, Some(sre.neighbour().clone()))
            } else {
                // This can happen if the only feasible route gets an infinite metric, as those are
                // never selected.
                info!("Retracting route for {subnet}");
                let update = babel::Update::new(
                    UPDATE_INTERVAL,
                    self.router_seqno.read().unwrap().0,
                    Metric::infinite(),
                    subnet,
                    self.router_id,
                );
                (update, None)
            };

        let send_update = |peer: &Peer| {
            // Don't send updates for a route to the next hop of the route, as that peer will never
            // select the route through us (that would caus a routing loop). The protocol can
            // handle this just fine, leaving this out is essentially an easy optimization.
            if let Some(ref neigh) = maybe_neigh {
                if peer == neigh {
                    return;
                }
            }
            debug!(
                "Propagating route update for {} to {}",
                subnet,
                peer.connection_identifier()
            );
            self.send_update(peer, update.clone());
        };

        if let Some(peers) = peers {
            for peer in peers {
                send_update(&peer);
            }
        } else {
            for peer in self.peer_interfaces.read().unwrap().iter() {
                send_update(peer);
            }
        };
    }

    /// Propagate all selected routes to all peers known in the router.
    fn propagate_selected_routes_to_peer(&self, peer: &Peer) {
        for (subnet, sre) in self
            .routing_table
            .read()
            .iter()
            .filter_map(|(subnet, route_list)| route_list.selected().map(|sr| (subnet, sr)))
        {
            let neigh_link_cost = Metric::from(sre.neighbour().link_cost());
            // Don't send updates for a route to the next hop of the route, as that peer will never
            // select the route through us (that would caus a routing loop). The protocol can
            // handle this just fine, leaving this out is essentially an easy optimization.
            if peer == sre.neighbour() {
                continue;
            }
            let update = babel::Update::new(
                advertised_update_interval(sre),
                sre.seqno(),
                // the cost of the route is the cost of the route + the cost of the link to the next-hop
                sre.metric() + neigh_link_cost,
                subnet,
                sre.source().router_id(),
            );
            debug!(
                "Propagating route update for {} to {} | D({}, {})",
                subnet,
                peer.connection_identifier(),
                sre.seqno(),
                sre.metric() + neigh_link_cost,
            );
            self.send_update(peer, update);
        }
    }

    /// Send an update to a peer.
    ///
    /// This updates updates the source table before sending the udpate as described in the RFC.
    fn send_update(&self, peer: &Peer, update: babel::Update) {
        // Sanity check, verify what we are doing is actually usefull
        if !peer.alive() {
            trace!("Cowardly refusing to sent update to peer which we know is dead");
            self.metrics.router_update_dead_peer();
            return;
        }

        // Before sending an update, the source table might need to be updated
        let metric = update.metric();
        let seqno = update.seqno();
        let router_id = update.router_id();
        let subnet = update.subnet();

        let source_key = SourceKey::new(subnet, router_id);
        let mut source_table = self.source_table.write().unwrap();

        if let Some(source_entry) = source_table.get(&source_key) {
            // if seqno of the update is greater than the seqno in the source table, update the source table
            if seqno.gt(&source_entry.seqno()) {
                source_table.insert(
                    source_key,
                    FeasibilityDistance::new(metric, seqno),
                    self.expired_source_key_sink.clone(),
                );
            }
            // if seqno of the update is equal to the seqno in the source table, update the source table if the metric (of the update) is lower
            else if seqno == source_entry.seqno() && source_entry.metric() > metric {
                source_table.insert(
                    source_key,
                    // Technically the seqno in the feasibility distance comes from the source
                    // entry, but that gives a borrow conflict so we use seqno from the update,
                    // which we just verified is the same.
                    FeasibilityDistance::new(metric, seqno),
                    self.expired_source_key_sink.clone(),
                )
            }
            // We also reset the garbage collection timer (unless the update is a retraction)
            else if !metric.is_infinite() {
                source_table.reset_timer(source_key, self.expired_source_key_sink.clone());
            }
        }
        // no entry for this source key, so insert it
        else {
            source_table.insert(
                source_key,
                FeasibilityDistance::new(metric, seqno),
                self.expired_source_key_sink.clone(),
            )
        };

        // send the update to the peer
        trace!("Sending update to peer");
        if peer
            .send_control_packet(ControlPacket::Update(update))
            .is_err()
        {
            // An error indicates the peer is dead
            trace!(
                "Failed to send update to dead peer {}",
                peer.connection_identifier()
            );
        }
    }
}

/// Manual clone implementation to avoid placing a where bound on `Router`, which would in turn
/// require a where bound on all structs which end up containing Router.
impl<M> Clone for Router<M>
where
    M: Clone,
{
    fn clone(&self) -> Self {
        Self {
            routing_table: self.routing_table.clone(),
            peer_interfaces: self.peer_interfaces.clone(),
            source_table: self.source_table.clone(),
            router_seqno: self.router_seqno.clone(),
            static_routes: self.static_routes.clone(),
            router_id: self.router_id,
            node_keypair: self.node_keypair.clone(),
            router_data_tx: self.router_data_tx.clone(),
            router_control_tx: self.router_control_tx.clone(),
            node_tun: self.node_tun.clone(),
            node_tun_subnet: self.node_tun_subnet,
            update_filters: self.update_filters.clone(),
            dead_peer_sink: self.dead_peer_sink.clone(),
            expired_source_key_sink: self.expired_source_key_sink.clone(),
            seqno_cache: self.seqno_cache.clone(),
            metrics: self.metrics.clone(),
        }
    }
}

/// Calculate the hold time for a [`RouteEntry`] from an [`Update`](babel::Update) .
fn route_hold_time(update: &babel::Update) -> Duration {
    // According to https://datatracker.ietf.org/doc/html/rfc8966#section-appendix.b a good value
    // would be 3.5 times the update inteval.
    // In case of a retracted route: in general this should not be added to the routing table, so
    // the only reason this is called is because a route was retracted through an update. Even if
    // the peer won't send this again, hold the route for some time so it can get flushed properly.
    if update.metric().is_infinite() {
        RETRACTED_ROUTE_HOLD_TIME
    } else {
        // Route expiry time -> 3.5 times advertised Update interval.
        Duration::from_millis((update.interval().as_millis() * 7 / 2) as u64)
    }
}

/// Calculates the interval to use when announcing updates on (selected) routes.
fn advertised_update_interval(sre: &RouteEntry) -> Duration {
    // We actually just need to set the value of the update interval, since that is the upper bound
    // on when we will advertise the route again.
    // One caveat is an expired route. If an entry is expired, it means that it will change state
    // so: Infinite metric -> route entry will be removed. Finite metric -> route entry will be
    // retracted but will be announced again. In practice this shouldn't really happen anyway.
    if sre.metric().is_infinite() && sre.expires().elapsed() != Duration::from_millis(0) {
        INTERVAL_NOT_REPEATING
    } else {
        UPDATE_INTERVAL
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv6Addr},
        sync::{atomic::AtomicU64, Arc},
        time::Duration,
    };

    use tokio::sync::mpsc;

    use crate::{
        babel::Update,
        crypto::{PublicKey, SecretKey},
        metric::Metric,
        peer::Peer,
        router_id::RouterId,
        sequence_number::SeqNo,
        source_table::SourceKey,
        subnet::Subnet,
    };

    #[test]
    fn calculate_route_hold_time() {
        let router_id = RouterId::new(PublicKey::from([0; 32]));
        let seqno = SeqNo::new();
        let metric = Metric::new(0);
        let subnet = Subnet::new(
            IpAddr::V6(Ipv6Addr::new(
                0x400, 0x0123, 0x4567, 0x89AB, 0xCDEF, 0, 0, 0,
            )),
            64,
        )
        .expect("Valid subnet definition");
        let update = Update::new(Duration::from_secs(60), seqno, metric, subnet, router_id);
        assert_eq!(
            Duration::from_millis(210_000),
            super::route_hold_time(&update)
        );
        let update = Update::new(Duration::from_secs(1), seqno, metric, subnet, router_id);
        assert_eq!(
            Duration::from_millis(3_500),
            super::route_hold_time(&update)
        );
        // Since update is expressed in centiseconds, we lose precision and
        // Duration::from_milis(478) is equal to Duration::from_millis(470);
        let update = Update::new(Duration::from_millis(478), seqno, metric, subnet, router_id);
        assert_eq!(
            Duration::from_millis(1_645),
            super::route_hold_time(&update)
        );

        // Retractions are also held for some time
        let update = Update::new(
            Duration::from_millis(0),
            seqno,
            Metric::infinite(),
            subnet,
            router_id,
        );
        assert_eq!(
            super::RETRACTED_ROUTE_HOLD_TIME,
            super::route_hold_time(&update)
        );
    }

    #[tokio::test]
    async fn calculate_advertised_update_interval() {
        // Set up a dummy peer since that is needed to create a `RouteEntry`
        let (router_data_tx, _router_data_rx) = mpsc::channel(1);
        let (router_control_tx, _router_control_rx) = mpsc::unbounded_channel();
        let (dead_peer_sink, _dead_peer_stream) = mpsc::channel(1);
        let (con1, _con2) = tokio::io::duplex(1500);
        let neighbor = Peer::new(
            router_data_tx,
            router_control_tx,
            con1,
            dead_peer_sink,
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .expect("Can create a dummy peer");
        let subnet = Subnet::new(IpAddr::V6(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 0)), 64)
            .expect("Valid subnet definition");
        let router_id = RouterId::new(PublicKey::from([0; 32]));
        let secret_key = SecretKey::new();
        let source = SourceKey::new(subnet, router_id);
        let metric = Metric::new(0);
        let seqno = SeqNo::new();
        let selected = true;

        let expiration = tokio::time::Instant::now() + Duration::from_secs(15);
        let re = super::RouteEntry::new(
            source,
            neighbor.clone(),
            metric,
            seqno,
            selected,
            expiration,
            secret_key.shared_secret(&router_id.to_pubkey()),
        );
        // We can't match exactly here since everything takes a non instant amount of time to do,
        // but basically verify that the calculated interval is within expected parameters.
        let advertised_interval = super::advertised_update_interval(&re);
        assert_eq!(advertised_interval, super::UPDATE_INTERVAL);

        // Expired route with finite metric
        let expiration = tokio::time::Instant::now() + Duration::from_secs(0);
        let re = super::RouteEntry::new(
            source,
            neighbor.clone(),
            metric,
            seqno,
            selected,
            expiration,
            secret_key.shared_secret(&router_id.to_pubkey()),
        );
        let advertised_interval = super::advertised_update_interval(&re);
        assert_eq!(advertised_interval, super::UPDATE_INTERVAL);

        // Expired route with infinite metric
        let re = super::RouteEntry::new(
            source,
            neighbor.clone(),
            Metric::infinite(),
            seqno,
            selected,
            expiration,
            secret_key.shared_secret(&router_id.to_pubkey()),
        );
        let advertised_interval = super::advertised_update_interval(&re);
        assert_eq!(advertised_interval, super::INTERVAL_NOT_REPEATING);

        // Check that the interval is properly capped
        let expiration = tokio::time::Instant::now() + Duration::from_secs(600);
        let re = super::RouteEntry::new(
            source,
            neighbor,
            metric,
            seqno,
            selected,
            expiration,
            secret_key.shared_secret(&router_id.to_pubkey()),
        );
        // We can't match exactly here since everything takes a non instant amount of time to do,
        // but basically verify that the calculated interval is within expected parameters.
        let advertised_interval = super::advertised_update_interval(&re);
        assert_eq!(advertised_interval, super::UPDATE_INTERVAL);
    }
}
