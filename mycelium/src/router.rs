use crate::{
    babel::{self, Hello, Ihu, RouteRequest, SeqNoRequest, Update},
    crypto::{PacketBuffer, PublicKey, SecretKey, SharedSecret},
    filters::RouteUpdateFilter,
    metric::Metric,
    metrics::Metrics,
    packet::{ControlPacket, DataPacket},
    peer::Peer,
    router_id::RouterId,
    routing_table::{
        NoRouteSubnet, QueriedSubnet, RouteEntry, RouteKey, RouteList, Routes, RoutingTable,
    },
    rr_cache::RouteRequestCache,
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
    hash::{Hash, Hasher},
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
/// Time between selected route announcements to peers.
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
const RETRACTED_ROUTE_HOLD_TIME: Duration = Duration::from_secs(6);

/// The interval specified in updates if the update won't be repeated.
const INTERVAL_NOT_REPEATING: Duration = Duration::from_millis(6);

/// The maximum generation of a [`RouteRequest`] we are still willing to transmit.
const MAX_RR_GENERATION: u8 = 16;

/// Give a route query 5 seconds to resolve, this should be plenty generous.
const ROUTE_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Amount of time to wait before checking if a queried route resolved.
// TODO: Remove once proper feedback is in place
const QUERY_CHECK_DURATION: Duration = Duration::from_millis(100);

/// The threshold for route expiry under which we want to send a route requets for a subnet if it is used.
const ROUTE_ALMOST_EXPIRED_TRESHOLD: tokio::time::Duration = Duration::from_secs(15);

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
    rr_cache: RouteRequestCache,
    update_workers: usize,
    metrics: M,
}

impl<M> Router<M>
where
    M: Metrics + Clone + Send + 'static,
{
    /// Create a new `Router`.
    ///
    /// # Panics
    ///
    /// If update_workers is not in the range of [1..255], this will panic.
    pub fn new(
        update_workers: usize,
        node_tun: UnboundedSender<DataPacket>,
        node_tun_subnet: Subnet,
        static_routes: Vec<Subnet>,
        node_keypair: (SecretKey, PublicKey),
        update_filters: Vec<Box<dyn RouteUpdateFilter + Send + Sync>>,
        metrics: M,
    ) -> Result<Self, Box<dyn Error>> {
        // We could use a NonZeroU8 here, but for now just handle this manually as this might get
        // changed in the future.
        if !(1..255).contains(&update_workers) {
            panic!("update workers must be at least 1 and at most 255");
        }

        // Tx is passed onto each new peer instance. This enables peers to send control packets to the router.
        let (router_control_tx, router_control_rx) = mpsc::unbounded_channel();
        // Tx is passed onto each new peer instance. This enables peers to send data packets to the router.
        let (router_data_tx, router_data_rx) = mpsc::channel::<DataPacket>(1000);
        let (expired_source_key_sink, expired_source_key_stream) = mpsc::channel(1);
        let (expired_route_entry_sink, expired_route_entry_stream) = mpsc::channel(1);
        let (dead_peer_sink, dead_peer_stream) = mpsc::channel(100);

        let routing_table = RoutingTable::new(expired_route_entry_sink);

        let router_id = RouterId::new(node_keypair.1);

        let seqno_cache = SeqnoCache::new();
        let rr_cache = RouteRequestCache::new(ROUTE_ALMOST_EXPIRED_TRESHOLD);

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
            rr_cache,
            update_filters: Arc::new(update_filters),
            update_workers,
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
        // Make sure to set the timers to current values in case lock acquisition takes time. Otherwise these
        // might immediately cause timeout timers to fire.
        peer.set_time_last_received_hello(tokio::time::Instant::now());
        peer.set_time_last_received_ihu(tokio::time::Instant::now());
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
        if let Routes::Exist(routes) = self.routing_table.best_routes(ip) {
            if routes.is_empty() {
                None
            } else {
                Some(routes[0].source().router_id().to_pubkey())
            }
        } else {
            None
        }
    }

    /// Gets the cached [`SharedSecret`] for the remote.
    pub fn get_shared_secret_from_dest(&self, dest: IpAddr) -> Option<SharedSecret> {
        // TODO: Make properly async
        for _ in 0..50 {
            match self.routing_table.best_routes(dest) {
                Routes::Exist(routes) => return Some(routes.shared_secret().clone()),
                Routes::Queried => {}
                Routes::NoRoute => return None,
                Routes::None => {
                    // NOTE: we request the full /64 subnet
                    self.send_route_request(
                        Subnet::new(dest, 64)
                            .expect("64 is a valid subnet size for an IPv6 address; qed"),
                    );
                }
            }

            tokio::task::block_in_place(|| std::thread::sleep(QUERY_CHECK_DURATION));
        }

        None
    }

    /// Get a [`SharedSecret`] for a remote, if a selected route exists to the remote.
    // TODO: Naming
    pub fn get_shared_secret_if_selected(&self, dest: IpAddr) -> Option<SharedSecret> {
        // TODO: Make properly async
        for _ in 0..50 {
            match self.routing_table.best_routes(dest) {
                Routes::Exist(routes) => {
                    if routes.selected().is_some() {
                        return Some(routes.shared_secret().clone());
                    } else {
                        // Optimistically try to fetch a new route for the subnet for next time.
                        // TODO: this can likely be handled better, but that relies on continuously
                        // quering routes in use, and handling unfeasible routes
                        if let Some(route_entry) = routes.iter().next() {
                            // We have a fallback route, use the source key from that to do a seqno
                            // request. Since the next hop might be dead, just do a broadcast. This
                            // might be blocked by the seqno request cache.
                            self.send_seqno_request(route_entry.source(), None, None);
                        } else {
                            // We don't have any routes, so send a route request. this might fail due
                            // to the source table.
                            self.send_route_request(
                                Subnet::new(dest, 64)
                                    .expect("64 is a valid subnet size for an IPv6 address; qed"),
                            );
                        }

                        return None;
                    }
                }
                Routes::Queried => {}
                Routes::NoRoute => {
                    return None;
                }
                Routes::None => {
                    // NOTE: we request the full /64 subnet
                    self.send_route_request(
                        Subnet::new(dest, 64)
                            .expect("64 is a valid subnet size for an IPv6 address; qed"),
                    );
                }
            }

            tokio::task::block_in_place(|| std::thread::sleep(QUERY_CHECK_DURATION));
        }

        None
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
        peer.died();

        let mut peers = self.peer_interfaces.write().unwrap();
        let old_peers = peers.len();
        peers.retain(|p| p != peer);

        let removed = old_peers - peers.len();
        for _ in 0..removed {
            self.metrics.router_peer_removed();
        }

        if removed > 1 {
            warn!(
                "REMOVED {removed} peers from peer list while called with {}",
                peer.connection_identifier()
            );
        }
    }

    /// Get a list of all selected route entries.
    pub fn load_selected_routes(&self) -> Vec<RouteEntry> {
        self.routing_table
            .read()
            .iter()
            .filter_map(|(_, rl)| rl.selected().cloned())
            .collect()
    }

    /// Get a list of all fallback route entries.
    pub fn load_fallback_routes(&self) -> Vec<RouteEntry> {
        self.routing_table
            .read()
            .iter()
            .flat_map(|(_, rl)| rl.iter().cloned().collect::<Vec<_>>())
            .filter(|re| !re.selected())
            .collect()
    }

    /// Get a list of all [`queried subnets`](QueriedSubnet).
    pub fn load_queried_subnets(&self) -> Vec<QueriedSubnet> {
        self.routing_table.read().iter_queries().collect()
    }

    pub fn load_no_route_entries(&self) -> Vec<NoRouteSubnet> {
        self.routing_table.read().iter_no_route().collect()
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

            self.handle_dead_peer(&dead_peers);
        }
    }

    /// Remove a dead peer from the router.
    pub fn handle_dead_peer(&self, dead_peers: &[Peer]) {
        for dead_peer in dead_peers {
            self.metrics.router_peer_died();
            debug!(
                "Cleaning up peer {} which is reportedly dead",
                dead_peer.connection_identifier()
            );
            self.remove_peer_interface(dead_peer);
        }

        // Scope for routing table write access.
        let subnets_to_select = {
            let mut rt_write = self.routing_table.write();
            let mut rt_write = rt_write.iter_mut();

            let mut subnets_to_select = Vec::new();

            while let Some((subnet, mut rl)) = rt_write.next() {
                rl.update_routes(|routes, eres, ct| {
                    for dead_peer in dead_peers {
                        let Some(mut re) = routes.iter_mut().find(|re| re.neighbour() == dead_peer)
                        else {
                            continue;
                        };

                        if re.selected() {
                            subnets_to_select.push(subnet);

                            // Don't clear selected flag yet, running route selection does that for us.
                            re.set_metric(Metric::infinite());
                            re.set_expires(
                                tokio::time::Instant::now() + RETRACTED_ROUTE_HOLD_TIME,
                                eres.clone(),
                                ct.clone(),
                            );
                        } else {
                            routes.remove(dead_peer);
                        }
                    }
                });
            }

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

        let Some(mut routes) = self.routing_table.routes_mut(subnet) else {
            // Subnet not known
            return;
        };

        // If there is no selected route there is nothing to do here. We keep expired routes in the
        // table for a while so updates of those should already have propagated to peers.
        let route_list = routes.routes();
        // If we have a new selected route we must have at least 1 item in the route list so
        // accessing the 0th list element here is fine.
        if let Some(new_selected) = self.find_best_route(&route_list).cloned() {
            if new_selected.neighbour() == route_list[0].neighbour() && route_list[0].selected() {
                debug!(
                    "New selected route for {subnet} is the same as the route alreayd installed"
                );
                return;
            }

            if new_selected.metric().is_infinite()
                && route_list[0].metric().is_infinite()
                && route_list[0].selected()
            {
                debug!("New selected route for {subnet} is retracted, like the previously selected route");
                return;
            }

            routes.set_selected(new_selected.neighbour());
        } else if !route_list.is_empty() && route_list[0].selected() {
            // This means we went from a selected route to a non-selected route. Unselect route and
            // trigger update.
            // At this point we also send a seqno request to all peers which advertised this route
            // to us, to try and get an updated entry. This uses the source key of the unselected
            // entry.
            self.send_seqno_request(route_list[0].source(), None, None);
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
            let subnet = rk.subnet();
            debug!(route.subnet = %subnet, "Got expiration event for route");
            // Scope mutable access to routes.
            // TODO: Is this really needed?
            {
                // Load current key
                let Some(mut routes) = self.routing_table.routes_mut(rk.subnet()) else {
                    // Subnet now known anymore. This means an expiration timer fired while the entry
                    // itself is gone already.
                    warn!(%subnet, "Route key expired for unknown subnet");
                    continue;
                };
                let route_selection =
                    routes.update_routes(|routes, eres, ct| {
                        let Some(mut entry) = routes
                            .iter_mut()
                            .find(|re| re.neighbour() == rk.neighbour())
                        else {
                            return false;
                        };
                        self.metrics.router_route_key_expired(!entry.selected());
                    if entry.selected() {
                        debug!(%subnet, peer = rk.neighbour().connection_identifier(), "Selected route expired, increasing metric to infinity");
                        entry.set_metric(Metric::infinite());
                        entry.set_expires(tokio::time::Instant::now() + RETRACTED_ROUTE_HOLD_TIME, eres.clone(), ct.clone());
                    } else {
                        debug!(%subnet, peer = rk.neighbour().connection_identifier(), "Unselected route expired, removing fallback route");
                        routes.remove(rk.neighbour());
                        // Removing a fallback route does not require any further changes. It does
                        // not affect the selected route and by extension does not require a
                        // triggered udpate.
                        return false;
                    }
                        true

                    });

                if !route_selection {
                    continue;
                }

                // Re run route selection if this was the selected route. We should do this before
                // publishing to potentially select a new route, however a time based expiraton of a
                // selected route generally means no other routes are viable anyway, so the short lived
                // black hole this could create is not really a concern.
                self.metrics.router_selected_route_expired();
                // Only inject selected route if we are simply retracting it, otherwise it is
                // actually already removed.
                debug!("Rerun route selection after expiration event");
                if let Some(r) = self.find_best_route(&routes.routes()).cloned() {
                    routes.set_selected(r.neighbour());
                } else {
                    debug!("Route selection did not find a viable route, unselect existing routes");
                    routes.unselect();
                }
            }

            // TODO: Is this _always_ needed?
            self.trigger_update(subnet, None);
        }
        warn!("Expired route key processing halted");
    }

    /// Process notifications about peers who are dead. This allows peers who can self-diagnose
    /// connection states to notify us, and allow for more efficient cleanup.
    async fn process_dead_peers(self, mut dead_peer_stream: mpsc::Receiver<Peer>) {
        let mut tx_buf = Vec::with_capacity(100);
        loop {
            let received = dead_peer_stream.recv_many(&mut tx_buf, 100).await;
            if received == 0 {
                break;
            }
            self.handle_dead_peer(&tx_buf[..received]);
            tx_buf.clear();
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
        let (update_tx, update_rx) = mpsc::channel(1_000_000);
        let (rr_tx, rr_rx) = mpsc::unbounded_channel();
        let (sn_tx, sn_rx) = mpsc::channel(100_000);

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
                    if let Err(e) = update_tx.try_send((update, source_peer)) {
                        match e {
                            mpsc::error::TrySendError::Closed(_) => {
                                self.metrics.router_tlv_discarded();
                                break;
                            }
                            mpsc::error::TrySendError::Full(update) => {
                                // If the metric is directly connected (0), always process the
                                // update.
                                if update.0.metric().is_direct() {
                                    // Channel disconnected
                                    if update_tx.send(update).await.is_err() {
                                        self.metrics.router_tlv_discarded();
                                        break;
                                    }
                                } else {
                                    self.metrics.router_tlv_discarded();
                                }
                            }
                        }
                    };
                }
                babel::Tlv::RouteRequest(route_request) => {
                    if rr_tx.send((route_request, source_peer)).is_err() {
                        break;
                    };
                }
                babel::Tlv::SeqNoRequest(seqno_request) => {
                    if let Err(e) = sn_tx.try_send((seqno_request, source_peer)) {
                        match e {
                            mpsc::error::TrySendError::Closed(_) => {
                                self.metrics.router_tlv_discarded();
                                break;
                            }
                            mpsc::error::TrySendError::Full(_) => {
                                self.metrics.router_tlv_discarded();
                            }
                        }
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
    async fn update_processor(self, mut update_rx: Receiver<(Update, Peer)>) {
        let mut senders = Vec::with_capacity(self.update_workers);
        for _ in 0..self.update_workers {
            let router = self.clone();
            let (tx, mut rx) = mpsc::channel::<(_, Peer)>(1_000_000);

            tokio::task::spawn_blocking(move || {
                while let Some((update, source_peer)) = rx.blocking_recv() {
                    let start = std::time::Instant::now();

                    if !source_peer.alive() {
                        trace!("Dropping Update TLV since sender is dead.");
                        router.metrics.router_tlv_source_died();
                        continue;
                    }

                    router.handle_incoming_update(update, source_peer);

                    router
                        .metrics
                        .router_time_spent_handling_tlv(start.elapsed(), "update");
                }
                warn!("Update processor task exitted");
            });

            senders.push(tx);
        }

        while let Some(item) = update_rx.recv().await {
            let mut hasher = ahash::AHasher::default();
            item.0.subnet().network().hash(&mut hasher);
            let slot = hasher.finish() as usize % self.update_workers;

            if let Err(e) = senders[slot].try_send(item) {
                match e {
                    mpsc::error::TrySendError::Closed(_) => {
                        self.metrics.router_tlv_discarded();
                        break;
                    }

                    mpsc::error::TrySendError::Full(update) => {
                        // If the metric is directly connected (0), always process the
                        // update.
                        if update.0.metric().is_direct() {
                            // Channel disconnected
                            if senders[slot].send(update).await.is_err() {
                                self.metrics.router_tlv_discarded();
                                break;
                            }
                        } else {
                            self.metrics.router_tlv_discarded();
                        }
                    }
                }
            };
        }
        warn!("Update processor coordinator exitted");
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
    async fn seqno_request_processor(self, mut sn_rx: Receiver<(SeqNoRequest, Peer)>) {
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
    fn handle_incoming_route_request(
        &self,
        mut route_request: babel::RouteRequest,
        source_peer: Peer,
    ) {
        self.metrics
            .router_process_route_request(route_request.prefix().is_none());
        // Handle the case of a single subnet.
        if let Some(subnet) = route_request.prefix() {
            // Check if these are local routes by chance
            let update = if let Some(static_route) = self
                .static_routes
                .iter()
                .find(|sr| sr.contains_subnet(&subnet))
            {
                debug!(
                    "Advertising static route {static_route} in response to route request for {subnet}"
                );
                babel::Update::new(
                    UPDATE_INTERVAL, // Static route is advertised with the default interval
                    self.router_seqno.read().unwrap().0, // Updates receive the seqno of the router
                    Metric::from(0), // Static route has no further hop costs
                    *static_route,
                    self.router_id,
                )
            } else {
                // If we have existing routes, attempt to send the selected route
                match self.routing_table.routes(subnet) {
                    Routes::Exist(routes) => {
                        if let Some(sre) = routes.selected() {
                            trace!("Advertising selected route for {subnet} after route request");
                            // Optimization: Don't send an update if the selected route next-hop is the peer
                            // who requested the route, as per the babel protocol the update will never be
                            // accepted.
                            if sre.neighbour() == &source_peer {
                                trace!(
                                "Not advertising route since the next-hop is the requesting peer"
                            );
                                return;
                            }
                            babel::Update::new(
                                advertised_update_interval(sre),
                                sre.seqno(),
                                sre.metric() + Metric::from(sre.neighbour().link_cost()),
                                subnet,
                                sre.source().router_id(),
                            )
                        } else {
                            // We don't have a selected route but we do have routes. It seems all routes
                            // are currently unfeasible. Don't send an update, but also don't send a
                            // retraction since we know the subnet exists
                            return;
                        }
                    }
                    // If the route is queried already we don't have to do anything.
                    // TODO: metric
                    Routes::Queried => return,
                    Routes::NoRoute => {
                        // We explicitly don't have a route for this subnet, so send a retraction to
                        // notify our peer as soon as possible not to expect anything.
                        babel::Update::new(
                            INTERVAL_NOT_REPEATING,
                            self.router_seqno.read().unwrap().0, // Retractions receive the seqno of the router
                            Metric::infinite(), // Static route has no further hop costs
                            subnet,             // Advertise the exact subnet requested
                            self.router_id,     // Our own router ID, since we advertise this
                        )
                    }
                    Routes::None => {
                        // We don't have a route, but we also don't have confirmation locally that the
                        // route does not exist. Forward the route request to the remainder of our
                        // peers and search for the route.
                        //
                        // First check the generation of the route request, if it is too high we simply
                        // ignore it. Check against the eventually bumped generation.
                        if route_request.generation() + 1 >= MAX_RR_GENERATION {
                            // Drop route request
                            // TODO: metric
                            return;
                        }
                        route_request.inc_generation();

                        debug!(
                            rr.subnet = %subnet,
                            rr.generation = route_request.generation(),
                            "Forwarding route request to peers"
                        );

                        self.routing_table.mark_queried(
                            subnet,
                            tokio::time::Instant::now() + ROUTE_QUERY_TIMEOUT,
                        );

                        let received_peers =
                            if let Some((gen, received_peers)) = self.rr_cache.info(subnet) {
                                // If this route request has a smaller generation than the one we
                                // recently sent, send regardless
                                if route_request.generation() < gen {
                                    vec![]
                                } else {
                                    received_peers
                                }
                            } else {
                                vec![]
                            };

                        let peers = self.peer_interfaces();

                        // Now transmit to all our peers, except the sender
                        for peer in peers.iter().filter(|p| *p != &source_peer) {
                            if received_peers.contains(peer) {
                                trace!(%subnet, peer = peer.connection_identifier(), "Not forwarding route request to peer who recently got a request from us");
                                continue;
                            }

                            if peer
                                .send_control_packet(route_request.clone().into())
                                .is_err()
                            {
                                debug!(
                                    rr.subnet = %subnet,
                                    rr.generation = route_request.generation(),
                                    peer = peer.connection_identifier(),
                                    "Can't forward route request to dead peer"
                                );
                            }
                        }

                        self.rr_cache.sent_route_request(route_request, peers);

                        return;
                    }
                }
            };

            self.update_source_table(&update);
            self.send_update(&source_peer, update);
        } else {
            // TODO: Is this still needed?
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
        if let Some((sent_at, _)) = self.seqno_cache.info(&SeqnoRequestCacheKey {
            router_id: seqno_request.router_id(),
            subnet: seqno_request.prefix(),
            seqno: seqno_request.seqno(),
        }) {
            // Only forward 1 request per minute for a given subnet request
            if sent_at.elapsed() <= Duration::from_secs(60) {
                self.metrics.router_seqno_request_unhandled();
                return;
            }
        };

        // If we have a selected route for the prefix, and its router id is different from the
        // requested router id, or the router id is the same and the entries sequence number is
        // not smaller than the requested sequence number, send an update for the route
        // to the peer (triggered update).
        if let Some(route_entry) = self.routing_table.routes(seqno_request.prefix()).selected() {
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

                self.update_source_table(&update);

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

            if let Routes::Exist(possible_routes) =
                self.routing_table.routes(seqno_request.prefix())
            {
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
                self.metrics.router_update_denied_by_filter();
                return;
            }
        }

        let metric = update.metric();
        let router_id = update.router_id();
        let seqno = update.seqno();
        let subnet = update.subnet();

        // Make sure we don't process an update for one of our own subnets.
        if self.is_static_subnet(subnet) {
            return;
        }

        // We accepted the update, check if we have a seqno request sent for this update
        let interested_peers = self.seqno_cache.get(&SeqnoRequestCacheKey {
            router_id,
            subnet,
            seqno,
        });

        let update_feasible = self
            .source_table
            .read()
            .unwrap()
            .is_update_feasible(&update);

        // We load all routes here for the subnet. Because we hold the mutex for the
        // writer, this view is accurate and we can't diverge until the mutex is released.
        let mut routing_table_entries = {
            if let Some(rte) = self.routing_table.routes_mut(subnet) {
                rte
            } else {
                if !update_feasible || metric.is_infinite() {
                    if !update_feasible {
                        self.send_seqno_request(
                            SourceKey::new(update.subnet(), update.router_id()),
                            Some(source_peer),
                            None,
                        );
                    }
                    debug!(%subnet, "Ignore unfeasible update | retraction for unknown subnet");
                    self.metrics.router_update_not_interested();
                    return;
                }
                let ss = self.node_keypair.0.shared_secret(&router_id.to_pubkey());
                self.routing_table.add_subnet(subnet, ss)
            }
        };

        let mut old_selected_route = None;
        let route_selection = routing_table_entries.update_routes(|routes, eres, ct| {
            // Take a deep copy of the old selected route if there is one, deep copy since we will
            // potentially mutate the route list so we can't keep a reference to it.
            old_selected_route = routes.selected().cloned();

            let maybe_existing_entry = routes.iter_mut().find(|re| re.neighbour() == &source_peer);

            debug!(
                subnet = %subnet,
                metric = %metric,
                seqno = %seqno,
                router_id = %router_id,
                entry_exists = maybe_existing_entry.is_some(),
                update_feasible = update_feasible,
                peer = source_peer.connection_identifier(),
                "Processing update packet",
            );

            if let Some(mut existing_entry) = maybe_existing_entry {
                // Unfeasible updates to the selected route are not applied, but we do request a seqno
                // bump.
                if existing_entry.selected()
                    && !update_feasible
                    && existing_entry.source().router_id() == router_id
                {
                    self.send_seqno_request(
                        existing_entry.source(),
                        Some(source_peer.clone()),
                        None,
                    );
                    return false;
                }

                // Otherwise we just update the entry.
                if existing_entry.metric().is_infinite() && update.metric().is_infinite() {
                    // Retraction for retracted route, don't do anything. If we don't filter this
                    // retracted routes can stay stuck if peer keep sending retractions to eachother
                    // for this route.
                    return false;
                }
                existing_entry.set_seqno(seqno);
                existing_entry.set_metric(metric);
                existing_entry.set_router_id(router_id);

                // Only reset the timer if the update is feasible, this will allow unfeasible updates
                // to be flushed out naturally. Note that a retraction is always feasbile.
                if update_feasible {
                    existing_entry.set_expires(
                        tokio::time::Instant::now() + route_hold_time(&update),
                        eres.clone(),
                        ct.clone(),
                    );
                }

                // If the route is not selected, and the update is unfeasible, the route will not be
                // selected by a subsequent route selection, so we can skip it and avoid wasting time
                // here.
                if !existing_entry.selected() && !update_feasible {
                    trace!("Ignoring route selection for unfeasible update to unselected route");
                    return false;
                }

                // If the update is unfeasible the route must be unselected.
                if existing_entry.selected() && !update_feasible {
                    existing_entry.set_selected(false);
                }
            } else {
                // If there is no entry yet ignore unfeasible updates and retractions.
                if metric.is_infinite() || !update_feasible {
                    // If the update is not feasible, and we don't have a selected route for the
                    // subnet, request a seqno bump.
                    if !update_feasible && routes.selected().is_none() {
                        self.send_seqno_request(
                            SourceKey::new(update.subnet(), update.router_id()),
                            Some(source_peer.clone()),
                            None,
                        );
                    }
                    debug!("Received unfeasible update | retraction for unknown route - neighbour");
                    return false;
                }

                // Create new entry in the route table
                routes.insert(
                    RouteEntry::new(
                        SourceKey::new(subnet, router_id),
                        source_peer.clone(),
                        metric,
                        seqno,
                        false,
                        tokio::time::Instant::now() + route_hold_time(&update),
                    ),
                    eres.clone(),
                    ct.clone(),
                );
            }

            true
        });

        if !route_selection {
            self.metrics.router_update_skipped_route_selection();
            return;
        }

        // Now that we applied the update, run route selection.
        let new_selected_route = self
            .find_best_route(&routing_table_entries.routes())
            .cloned();
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

            routing_table_entries.unselect();
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
        let trigger_update = match (&old_selected_route, new_selected_route) {
            (Some(old_route), Some(new_route)) => {
                if new_route.neighbour() != old_route.neighbour() {
                    debug!(
                        subnet = %subnet,
                        old_next_hop = old_route.neighbour().connection_identifier(),
                        new_next_hop = new_route.neighbour().connection_identifier(),
                        "Selected route changed next-hop",
                    );
                }
                // Router id changed.
                new_route.source().router_id() != old_route.source().router_id()
                    || new_route.metric().delta(&old_route.metric()) > BIG_METRIC_CHANGE_TRESHOLD
            }
            (None, Some(new_route)) => {
                info!(
                    subnet = %subnet,
                    peer = new_route.neighbour().connection_identifier(),
                    "Acquired route",
                );
                true
            }
            (Some(old_route), None) => {
                info!(
                    subnet = %subnet,
                    peer = old_route.neighbour().connection_identifier(),
                    "Lost route",
                );
                true
            }
            (None, None) => false,
        };

        if trigger_update {
            debug!(subnet = %subnet, "Send triggered update in response to update");
            self.trigger_update(subnet, None);
        } else if interested_peers.is_some() {
            debug!(
                subnet = %subnet,
                "Send update to peers who registered interest through a seqno request"
            );
            // If we have some interest in an update because we forwarded a seqno request but we
            // aren't triggering an update, notify just the interested peers.
            self.trigger_update(subnet, interested_peers);
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

            let known_routes = self.routing_table.routes(source.subnet());

            // Make sure a broadcast only happens in case the local node originated the request.
            if known_routes.is_none() && request_origin.is_none() {
                // If we don't know the route just ask all our peers.
                self.peer_interfaces
                    .read()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect()
            } else if let Routes::Exist(known_routes) = known_routes {
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
            } else {
                vec![]
            }
        };

        // Don't overload peers with request for the same source key
        let already_received = self.seqno_cache.get(&srck).unwrap_or_default();

        for peer in targets {
            if already_received.contains(&peer) {
                trace!(
                    peer = peer.connection_identifier(),
                    "Don't send seqno request to peer which already received it recently"
                );
                continue;
            }

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
    #[inline]
    fn is_static_subnet(&self, subnet: Subnet) -> bool {
        self.static_routes.contains(&subnet)
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
            match self.routing_table.best_routes(data_packet.dst_ip.into()) {
                Routes::Exist(routes) => match routes.selected() {
                    Some(route_entry) => {
                        // Prematurely send a route request if the route is almost expired
                        if route_entry
                            .expires()
                            .duration_since(tokio::time::Instant::now())
                            < ROUTE_ALMOST_EXPIRED_TRESHOLD
                        {
                            self.send_route_request(
                                Subnet::new(data_packet.dst_ip.into(), 64)
                                    .expect("64 is a valid subnet size for IPv6, qed;"),
                            );
                        }

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
                },
                Routes::Queried => {
                    // Wait for query resolution
                    // TODO: proper fix
                    tokio::task::block_in_place(|| std::thread::sleep(QUERY_CHECK_DURATION));
                    self.route_packet(data_packet);
                }
                Routes::NoRoute => {
                    self.metrics.router_route_packet_no_route();
                    self.no_route_to_host(data_packet);
                }
                Routes::None => {
                    // Send route request
                    // NOTE: we request the full /64 subnet
                    self.send_route_request(
                        Subnet::new(data_packet.dst_ip.into(), 64)
                            .expect("64 is a valid subnet size for IPv6 addresses"),
                    );
                    // TODO: proper fix
                    tokio::task::block_in_place(|| std::thread::sleep(QUERY_CHECK_DURATION));
                    self.route_packet(data_packet);
                }
            }
        }
    }

    /// Send a [`RouteRequest`] for the [`Subnet`] to all peers.
    fn send_route_request(&self, subnet: Subnet) {
        let route_request = RouteRequest::new(Some(subnet), 0);

        self.routing_table
            .mark_queried(subnet, tokio::time::Instant::now() + ROUTE_QUERY_TIMEOUT);

        // Don't care about generation here
        let received_peers = if let Some((gen, receivers)) = self.rr_cache.info(subnet) {
            // If the previous request was a higher generation, send the reques with lower
            // generation to all our peers again
            if gen > 0 {
                vec![]
            } else {
                receivers
            }
        } else {
            vec![]
        };

        let peers = self.peer_interfaces();

        for peer in peers.iter() {
            if received_peers.contains(peer) {
                trace!(%subnet, peer = peer.connection_identifier(), "Not forwarding route request to peer who recently got a request from us");
                continue;
            }

            if peer
                .send_control_packet(route_request.clone().into())
                .is_err()
            {
                debug!(subnet = %subnet, "Could not send route request to peer");
            }
        }

        self.rr_cache.sent_route_request(route_request, peers);
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

    /// Propagate all static routes to all known peers.
    fn propagate_static_routes_to_peers(&self) {
        for peer in self.peer_interfaces.read().unwrap().iter() {
            self.propagate_static_route_to_peer(peer);
        }
    }

    /// Applies the effect of an [`Update`] to the [`SourceTable`].
    fn update_source_table(&self, update: &Update) {
        let metric = update.metric();
        let seqno = update.seqno();

        let source_key = SourceKey::new(update.subnet(), update.router_id());

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

    /// Send a control packet to a peer.
    ///
    /// Errors are not propagated to the caller.
    fn send_update(&self, peer: &Peer, update: Update) {
        trace!("Sending update to peer");

        // Sanity check, verify what we are doing is actually usefull
        if !peer.alive() {
            trace!("Cowardly refusing to sent update to peer which we know is dead");
            self.metrics.router_update_dead_peer();
            return;
        }

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
            self.update_source_table(&update);

            // send the update to the peer
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
                debug!(subnet = %subnet, "Retracting route");
                let update = babel::Update::new(
                    UPDATE_INTERVAL,
                    self.router_seqno.read().unwrap().0,
                    Metric::infinite(),
                    subnet,
                    self.router_id,
                );
                (update, None)
            };

        self.update_source_table(&update);

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
                subnet = %subnet,
                peer = peer.connection_identifier(),
                "Propagating route update",
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
            .filter_map(|(subnet, route_list)| route_list.selected().map(|sr| (subnet, sr.clone())))
        {
            let neigh_link_cost = Metric::from(sre.neighbour().link_cost());
            // Don't send updates for a route to the next hop of the route, as that peer will never
            // select the route through us (that would caus a routing loop). The protocol can
            // handle this just fine, leaving this out is essentially an easy optimization.
            if peer == sre.neighbour() {
                continue;
            }
            let update = babel::Update::new(
                advertised_update_interval(&sre),
                sre.seqno(),
                // the cost of the route is the cost of the route + the cost of the link to the next-hop
                sre.metric() + neigh_link_cost,
                subnet,
                sre.source().router_id(),
            );
            debug!(
                subnet = %subnet,
                metric = %sre.metric() + neigh_link_cost,
                seqno = %sre.seqno(),
                peer = peer.connection_identifier(),
                "Propagating route update",
            );

            self.update_source_table(&update);

            // send the update to the peer
            self.send_update(peer, update);
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
            rr_cache: self.rr_cache.clone(),
            update_workers: self.update_workers,
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
        time::Duration,
    };

    use tokio::sync::mpsc;

    use crate::{
        babel::Update, connection::DuplexStream, crypto::PublicKey, metric::Metric, peer::Peer,
        router_id::RouterId, sequence_number::SeqNo, source_table::SourceKey, subnet::Subnet,
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
            DuplexStream::new(con1),
            dead_peer_sink,
        )
        .expect("Can create a dummy peer");
        let subnet = Subnet::new(IpAddr::V6(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 0)), 64)
            .expect("Valid subnet definition");
        let router_id = RouterId::new(PublicKey::from([0; 32]));
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
        );
        let advertised_interval = super::advertised_update_interval(&re);
        assert_eq!(advertised_interval, super::INTERVAL_NOT_REPEATING);

        // Check that the interval is properly capped
        let expiration = tokio::time::Instant::now() + Duration::from_secs(600);
        let re = super::RouteEntry::new(source, neighbor, metric, seqno, selected, expiration);
        // We can't match exactly here since everything takes a non instant amount of time to do,
        // but basically verify that the calculated interval is within expected parameters.
        let advertised_interval = super::advertised_update_interval(&re);
        assert_eq!(advertised_interval, super::UPDATE_INTERVAL);
    }
}
