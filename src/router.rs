use crate::{
    babel,
    crypto::{PublicKey, SecretKey, SharedSecret},
    metric::Metric,
    packet::{ControlPacket, ControlStruct, DataPacket},
    peer::Peer,
    routing_table::{RouteEntry, RouteKey, RoutingTable},
    sequence_number::SeqNo,
    source_table::{FeasibilityDistance, SourceKey, SourceTable},
    subnet::Subnet,
};
use left_right::{ReadHandle, WriteHandle};
use log::{debug, error, info, trace, warn};
use std::{
    collections::HashMap,
    error::Error,
    fmt::Debug,
    net::{IpAddr, Ipv6Addr},
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender};

const HELLO_INTERVAL: u16 = 4;
const IHU_INTERVAL: u16 = HELLO_INTERVAL * 3;
const UPDATE_INTERVAL: u16 = HELLO_INTERVAL * 4;

#[derive(Debug, Clone, Copy)]
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
    router_id: PublicKey,
    node_keypair: (SecretKey, PublicKey),
    router_data_tx: Sender<DataPacket>,
    router_control_tx: UnboundedSender<ControlStruct>,
    node_tun: UnboundedSender<Vec<u8>>,
    node_tun_addr: Ipv6Addr,
}

impl Router {
    pub fn new(
        node_tun: UnboundedSender<Vec<u8>>,
        node_tun_addr: Ipv6Addr,
        static_routes: Vec<StaticRoute>,
        node_keypair: (SecretKey, PublicKey),
    ) -> Result<Self, Box<dyn Error>> {
        // Tx is passed onto each new peer instance. This enables peers to send control packets to the router.
        let (router_control_tx, router_control_rx) = mpsc::unbounded_channel::<ControlStruct>();
        // Tx is passed onto each new peer instance. This enables peers to send data packets to the router.
        let (router_data_tx, router_data_rx) = mpsc::channel::<DataPacket>(1000);

        let router_inner = RouterInner::new()?;
        let (mut inner_w, inner_r) = left_right::new_from_empty(router_inner);
        inner_w.append(RouterOpLogEntry::SetStaticRoutes(static_routes));
        inner_w.publish();

        let router = Router {
            inner_w: Arc::new(Mutex::new(inner_w)),
            inner_r,
            router_id: node_keypair.1,
            node_keypair,
            router_data_tx,
            router_control_tx,
            node_tun,
            node_tun_addr,
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
            router.node_keypair.1,
        ));

        Ok(router)
    }

    pub fn router_control_tx(&self) -> UnboundedSender<ControlStruct> {
        self.router_control_tx.clone()
    }

    pub fn router_data_tx(&self) -> Sender<DataPacket> {
        self.router_data_tx.clone()
    }

    pub fn node_tun_addr(&self) -> Ipv6Addr {
        self.node_tun_addr
    }

    pub fn node_tun(&self) -> UnboundedSender<Vec<u8>> {
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

    pub fn peer_by_ip(&self, peer_ip: IpAddr) -> Option<Peer> {
        self.inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles")
            .peer_by_ip(peer_ip)
    }

    pub fn peer_exists(&self, peer_underlay_ip: IpAddr) -> bool {
        self.inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles")
            .peer_exists(peer_underlay_ip)
    }

    pub fn source_peer_from_control_struct(&self, control_struct: ControlStruct) -> Option<Peer> {
        let peers = self.peer_interfaces();
        let matching_peer = peers
            .iter()
            .find(|peer| peer.overlay_ip() == control_struct.src_overlay_ip);

        matching_peer.map(Clone::clone)
    }

    pub fn node_secret_key(&self) -> SecretKey {
        self.node_keypair.0.clone()
    }

    pub fn node_public_key(&self) -> PublicKey {
        self.node_keypair.1
    }

    /// Add a new destination [`PublicKey`] to the destination map. This will also compute and store
    /// the [`SharedSecret`] from the `Router`'s [`SecretKey`].
    pub fn add_dest_pubkey_map_entry(&self, dest: Ipv6Addr, pubkey: PublicKey) {
        let ss = self.node_keypair.0.shared_secret(&pubkey);

        self.inner_w
            .lock()
            .expect("Mutex isn't poisoned")
            .append(RouterOpLogEntry::AddDestPubkey(dest, pubkey, ss))
            .publish();
    }

    /// Gets the cached [`SharedSecret`] for the remote.
    pub fn get_shared_secret_from_dest(&self, dest: &Ipv6Addr) -> Option<SharedSecret> {
        self.inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles")
            .dest_pubkey_map
            .get(dest)
            .map(|(_, ss)| ss.clone())
    }

    /// Gets the cached [`SharedSecret`] based on the associated [`PublicKey`] of the remote.
    pub fn get_shared_secret_by_pubkey(&self, dest: &PublicKey) -> Option<SharedSecret> {
        for (pk, ss) in self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles")
            .dest_pubkey_map
            .values()
        {
            if pk == dest {
                return Some(ss.clone());
            }
        }

        None
    }

    pub fn print_selected_routes(&self) {
        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");

        let routing_table = &inner.selected_routing_table;
        for (route_key, route_entry) in routing_table.table.iter() {
            println!("Route key: {:?}", route_key);
            println!(
                "Route: {} (with next-hop: {:?}, metric: {}, selected: {})",
                route_key.subnet(),
                route_entry.next_hop(),
                route_entry.metric(),
                route_entry.selected()
            );
            // println!("As advertised by: {:?}", route.1.source.router_id);
        }
    }

    // pub fn print_fallback_routes(&self) {
    //     let inner = self.inner.read().unwrap();

    //     let routing_table = &inner.fallback_routing_table;
    //     for route in routing_table.table.iter() {
    //         println!("Route key: {:?}", route.0);
    //         println!(
    //             "Route: {:?}/{:?} (with next-hop: {:?}, metric: {}, selected: {})",
    //             route.0.prefix, route.0.plen, route.1.next_hop, route.1.metric, route.1.selected
    //         );
    //         println!("As advertised by: {:?}", route.1.source.router_id);
    //     }
    // }

    // pub fn print_source_table(&self) {
    //     let inner = self.inner.read().unwrap();

    //     let source_table = &inner.source_table;
    //     for (sk, se) in source_table.table.iter() {
    //         println!("Source key: {:?}", sk);
    //         println!("Source entry: {:?}", se);
    //         println!("\n");
    //     }
    // }

    async fn check_for_dead_peers(self, router_id: PublicKey) {
        let ihu_threshold = tokio::time::Duration::from_secs(8);

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
        mut router_control_rx: UnboundedReceiver<ControlStruct>,
    ) {
        while let Some(control_struct) = router_control_rx.recv().await {
            // println!(
            //     "received control packet from {:?}",
            //     control_struct.src_overlay_ip
            // );
            match control_struct.control_packet {
                babel::Tlv::Hello(_) => self.handle_incoming_hello(control_struct),
                babel::Tlv::Ihu(_) => self.handle_incoming_ihu(control_struct),
                babel::Tlv::Update(_) => self.handle_incoming_update(control_struct),
            }
        }
    }

    fn handle_incoming_hello(&self, control_struct: ControlStruct) {
        // let destination_ip = control_struct.src_overlay_ip;
        // control_struct.reply(ControlPacket::new_ihu(IHU_INTERVAL, destination_ip));

        // Upon receiving and Hello message from a peer, this node has to send a IHU back
        if let Some(source_peer) = self.source_peer_from_control_struct(control_struct) {
            let ihu = ControlPacket::new_ihu(IHU_INTERVAL, source_peer.overlay_ip());
            match source_peer.send_control_packet(ihu) {
                Ok(()) => {}
                Err(e) => {
                    error!("Error sending IHU to peer: {e}");
                }
            }
        }
    }

    fn handle_incoming_ihu(&self, control_struct: ControlStruct) {
        if let Some(source_peer) = self.source_peer_from_control_struct(control_struct) {
            // reset the IHU timer associated with the peer
            // source_peer.reset_ihu_timer(tokio::time::Duration::from_secs(IHU_INTERVAL as u64));
            // measure time between Hello and and IHU and set the link cost
            let time_diff = tokio::time::Instant::now()
                .duration_since(source_peer.time_last_received_hello())
                .as_millis();

            source_peer.set_link_cost(time_diff as u16);

            // set the last_received_ihu for this peer
            source_peer.set_time_last_received_ihu(tokio::time::Instant::now());
        }
    }

    fn handle_incoming_update(&self, update_packet: ControlStruct) {
        match update_packet.control_packet {
            babel::Tlv::Update(update) => {
                let metric = update.metric();
                let router_id = update.router_id();
                let seqno = update.seqno();
                let subnet = update.subnet();

                // convert prefix to ipv6 address
                if let IpAddr::V6(prefix_as_ipv6addr) = subnet.address() {
                    // add it the mapping
                    self.add_dest_pubkey_map_entry(prefix_as_ipv6addr, router_id);
                }

                // create route key from incoming update control struct
                // we need the address of the neighbour; this corresponds to the source ip of the control struct as the update is received from the neighbouring peer
                let neighbor_ip = update_packet.src_overlay_ip;
                let route_key_from_update = RouteKey::new(subnet, neighbor_ip);
                // used later to filter out static route
                if self.route_key_is_from_static_route(&route_key_from_update) {
                    return;
                }

                let mut inner_w = self.inner_w.lock().expect("Mutex isn't poisoned");

                // check if a route entry with the same route key exists in both routing tables
                let (route_entry_exists, update_feasible, hop_peer) = {
                    let inner = inner_w
                        .enter()
                        .expect("We deref through a write handle so this enter never fails");

                    (
                        inner
                            .selected_routing_table
                            .table
                            .contains_key(&route_key_from_update)
                            || inner
                                .fallback_routing_table
                                .table
                                .contains_key(&route_key_from_update),
                        inner.source_table.is_update_feasible(&update),
                        inner
                            .peer_by_ip(neighbor_ip)
                            .expect("peer by ip is known to add route entry"),
                    )
                };

                // if no entry exists (based on prefix, plen AND neighbor field)
                if !route_entry_exists {
                    if metric.is_infinite() || !update_feasible {
                        debug!("Received retraction for unknown route");
                    } else {
                        // this means that the update is feasible and the metric is not infinite
                        // create a new route entry and add it to the routing table (which requires a new source entry to be created as well)
                        info!("Learned route for previously unknown subnet {}", subnet);

                        let source_key = SourceKey::new(subnet, router_id);
                        let fd = FeasibilityDistance::new(metric, seqno);
                        inner_w.append(RouterOpLogEntry::InsertSourceEntry(source_key, fd));

                        let route_key = RouteKey::new(subnet, neighbor_ip);
                        let route_entry =
                            RouteEntry::new(source_key, hop_peer, metric, seqno, true);

                        let mut to_remove = None;
                        let mut add_fallback = false;
                        for (rk, re) in inner_w
                            .enter()
                            .expect("We deref through a write handle so this enter never fails")
                            .selected_routing_table
                            .table
                            .iter()
                        {
                            if rk.subnet() == subnet {
                                if metric < re.metric() {
                                    //to_remove.push(r.0.clone());
                                    to_remove = Some(rk.clone());
                                    break; // we can break, as there will be max 1 better route in selected table at any point in time (hence 'selected')
                                           // metric of update is greater than entry's metric
                                } else if metric >= re.metric() {
                                    // this means that there is already a better route in our selected routing table,
                                    // so we should add it to fallback instead
                                    add_fallback = true;
                                }
                            }
                        }

                        if add_fallback {
                            debug!(
                                "Added new route for {subnet} via {neighbor_ip} as fallback route"
                            );
                            inner_w.append(RouterOpLogEntry::InsertFallbackRoute(
                                route_key,
                                route_entry,
                            ));
                            return;
                        }

                        if let Some(rk) = to_remove {
                            debug!("Moved existing route for {subnet} to fallback route",);
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
                        debug!("Received retraction for {subnet} from {neighbor_ip}");
                        // if the update is a retraction, we remove the entry from the routing tables
                        // we also remove the corresponding source entry???
                        inner_w.append(RouterOpLogEntry::RemoveSelectedRoute(
                            route_key_from_update.clone(),
                        ));
                        inner_w
                            .append(RouterOpLogEntry::RemoveFallbackRoute(route_key_from_update));
                        // remove the corresponding source entry
                        let source_key = SourceKey::new(subnet, router_id);
                        inner_w.append(RouterOpLogEntry::RemoveSourceEntry(source_key));
                        return;
                    }
                    // if the entry is currently selected, the update is unfeasible, and the router-id of the update is equal
                    // to the router-id of the entry, then we ignore the update
                    let possible_entry = inner_w
                        .enter()
                        .expect("We deref through a write handle so this enter never fails")
                        .selected_routing_table
                        .table
                        .get(&route_key_from_update)
                        // clone to get rid of borrow into read handle
                        .cloned();

                    if let Some(route_entry) = possible_entry {
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
                            inner_w.append(RouterOpLogEntry::RemoveSelectedRoute(
                                route_key_from_update,
                            ));
                            // TODO: should we remove it from the selected and add it to fallback here???
                        }
                    }
                }
                inner_w.publish();
            }
            _ => {
                panic!("Received update with wrong TLV type");
            }
        }
    }

    // // incoming update can only be received by a Peer this node has a direct link to
    // fn handle_incoming_update(&self, update: ControlStruct) {
    //     match update.control_packet.body.tlv {
    //         BabelTLV::Update { plen, interval: _, seqno, metric, prefix, router_id } => {

    //             // create route key from incoming update control struct
    //             // we need the address of the neighbour; this corresponds to the source ip of the control struct as the update is received from the neighbouring peer
    //             let neighbor_ip = update.src_overlay_ip;
    //             let route_key_from_update = RouteKey {
    //                 neighbor: neighbor_ip,
    //                 plen,
    //                 prefix,
    //             };

    //             // used later to filter out static route
    //             if self.route_key_is_from_static_route(&route_key_from_update) {
    //                 return;
    //             }

    //             let mut inner = self.inner.write().unwrap();

    //             // check if a route entry with the same route key exists in both routing tables
    //             let route_entry_exists = inner.selected_routing_table.table.contains_key(&route_key_from_update) || inner.fallback_routing_table.table.contains_key(&route_key_from_update);

    //             // if no entry exists (based on prefix, plen AND neighbor field)
    //             if !route_entry_exists {
    //                 // if the update is unfeasible, or the metric is inifinite, we ignore the update
    //                 if metric == u16::MAX || !self.update_feasible(&update, &inner.source_table) {
    //                     return;
    //                 }
    //                 else {
    //                     // this means that the update is feasible and the metric is not infinite
    //                     // create a new route entry and add it to the routing table (which requires a new source entry to be created as well)

    //                     let source_key = SourceKey { prefix, plen, router_id };
    //                     let fd = FeasibilityDistance{ metric, seqno };
    //                     inner.source_table.insert(source_key, fd);

    //                     let route_key = RouteKey {
    //                         prefix,
    //                         plen,
    //                         neighbor: neighbor_ip,
    //                     };
    //                     let route_entry = RouteEntry {
    //                         source: source_key,
    //                         neighbor: inner.peer_by_ip(neighbor_ip).unwrap(),
    //                         metric,
    //                         seqno,
    //                         next_hop: neighbor_ip,
    //                         selected: true,
    //                     };

    //                     // Collect keys of routes to be removed
    //                     let mut to_remove = Vec::new();
    //                     for r in inner.selected_routing_table.table.iter() {
    //                         // filter based on prefix and plen, skipping neighbor
    //                         if r.0.plen == plen && r.0.prefix == prefix {
    //                             // metric of update is smaller than entry's metric
    //                             if metric < r.1.metric {
    //                                 // this means we should remove the entry from the selected routing table
    //                                 to_remove.push(r.0.clone());
    //                                 break; // we can break, as there will be max 1 better route in selected table at any point in time (hence 'selected')
    //                             // metric of update is greater than entry's metric
    //                             } else if metric >= r.1.metric {
    //                                 // this means that there is already a better route in our selected routing table,
    //                                 // so we should add it to fallback instead
    //                                 // we also need to set the selected field of the route entry to false
    //                                 let mut fallback_route_entry = route_entry.clone();
    //                                 fallback_route_entry.selected = false;
    //                                 inner.fallback_routing_table.table.insert(route_key.clone(), fallback_route_entry);
    //                                 return; // quit the function, work is done here
    //                             }
    //                         }
    //                     }
    //                     // Remove better routes from selected and insert into fallback
    //                     for rk in to_remove {
    //                         if let Some(mut old_selected) = inner.selected_routing_table.remove(&rk) {
    //                             // change the old selected route to have selected = false
    //                             old_selected.selected = false;
    //                             inner.fallback_routing_table.insert(rk, old_selected);
    //                         }
    //                     }
    //                     //buggy:
    //                     // insert the route into selected (we might have placed one other route, that was previously the best, in the fallback)
    //                     inner.selected_routing_table.table.insert(route_key.clone(), route_entry.clone());

    //                     {
    //                         let current_route_entry = inner.selected_routing_table.table.remove(&route_key);
    //                         let fallback_route_entry = inner.fallback_routing_table.table.remove(&route_key);

    //                         match (current_route_entry, fallback_route_entry) {
    //                             (Some(mut current_route), Some(mut fallback_route)) => {
    //                                 if fallback_route.metric < current_route.metric {
    //                                     std::mem::swap(&mut current_route, &mut fallback_route);
    //                                     current_route.selected = true;
    //                                     fallback_route.selected = false;
    //                                 }
    //                                 inner.selected_routing_table.table.insert(route_key.clone(), current_route);
    //                                 inner.fallback_routing_table.table.insert(route_key, fallback_route);
    //                             }
    //                             (Some(route), None) => {
    //                                 inner.selected_routing_table.table.insert(route_key, route);
    //                             }
    //                             (None, Some(route)) => {
    //                                 inner.fallback_routing_table.table.insert(route_key, route);
    //                             }
    //                             (None, None) => {}
    //                         }
    //                     }
    //                 }
    //             }
    //             // entry exists
    //             else {
    //                 // check if update is a retraction
    //                 if self.update_feasible(&update, &inner.source_table) && metric == u16::MAX {
    //                     // if the update is a retraction, we remove the entry from the routing tables
    //                     // we also remove the corresponding source entry???
    //                     if inner.selected_routing_table.table.contains_key(&route_key_from_update) {
    //                         inner.selected_routing_table.remove(&route_key_from_update);
    //                     }
    //                     if inner.fallback_routing_table.table.contains_key(&route_key_from_update) {
    //                         inner.fallback_routing_table.remove(&route_key_from_update);
    //                     }
    //                     // remove the corresponding source entry
    //                     let source_key = SourceKey { prefix, plen, router_id };
    //                     inner.source_table.remove(&source_key);

    //                     return;
    //                 }
    //                 // if the entry is currently selected, the update is unfeasible, and the router-id of the update is equal
    //                 // to the router-id of the entry, then we ignore the update
    //                 if inner.selected_routing_table.table.contains_key(&route_key_from_update) {
    //                     let route_entry = inner.selected_routing_table.table.get(&route_key_from_update).unwrap();
    //                     if !self.update_feasible(&update, &inner.source_table) && route_entry.source.router_id == router_id {
    //                         return;
    //                     }
    //                     // update the entry's seqno, metric and router-id
    //                     let route_entry = inner.selected_routing_table.table.get_mut(&route_key_from_update).unwrap();
    //                     route_entry.update_seqno(seqno);
    //                     route_entry.update_metric(metric);
    //                     route_entry.update_router_id(router_id);
    //                 }
    //                 // otherwise
    //                 else {
    //                     let route_entry = inner.fallback_routing_table.table.get_mut(&route_key_from_update).unwrap();
    //                     // update the entry's seqno, metric and router-id
    //                     route_entry.update_seqno(seqno);
    //                     route_entry.update_metric(metric);
    //                     route_entry.update_router_id(router_id);

    //                     if !self.update_feasible(&update, &inner.source_table) {
    //                         // if the update is unfeasible, we remove the entry from the selected routing table
    //                         inner.selected_routing_table.table.remove(&route_key_from_update);
    //                         // should we remove it from the selected and add it to fallback here???
    //                     }
    //                 }
    //             }
    //         },
    //         _ => {
    //             panic!("Received update with wrong TLV type");
    //         }
    //     }

    // }

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

    pub fn route_packet(&self, data_packet: DataPacket) -> Result<(), ()> {
        let node_tun_addr = self.node_tun_addr();

        trace!(
            "Incoming data packet, with dest_ip: {} (side node, this node's tun addr is: {})",
            data_packet.dest_ip,
            node_tun_addr
        );

        if data_packet.dest_ip == node_tun_addr {
            // decrypt & send to TUN interface
            let pubkey_sender = data_packet.pubkey;
            let shared_secret = match self.get_shared_secret_by_pubkey(&pubkey_sender) {
                Some(ss) => ss,
                None => self.node_secret_key().shared_secret(&pubkey_sender),
            };
            let decrypted_raw_data = match shared_secret.decrypt(&data_packet.raw_data) {
                Ok(data) => data,
                Err(_) => {
                    log::debug!("Dropping data packet with invalid encrypted content");
                    return Err(());
                }
            };
            match self.node_tun().send(decrypted_raw_data) {
                Ok(_) => Ok(()),
                Err(e) => {
                    error!("Error sending data packet to TUN interface: {:?}", e);
                    Err(())
                }
            }
        } else {
            // send to peer with matching overlay IP
            let best_route = self.select_best_route(IpAddr::V6(data_packet.dest_ip));
            match best_route {
                Some(route_entry) => {
                    let peer = self.peer_by_ip(route_entry.next_hop());
                    // drop the packet if the peer is couldn't be found
                    match peer {
                        Some(peer) => {
                            if let Err(e) = peer.send_data_packet(data_packet) {
                                error!("Error sending data packet to peer: {:?}", e);
                                Err(())
                            } else {
                                Ok(())
                            }
                        }
                        None => {
                            // route but no peer
                            warn!("Dropping data packet, no peer found");
                            Err(())
                        }
                    }
                }
                None => {
                    trace!("Error sending data packet, no route found");
                    Err(())
                }
            }
        }
    }

    async fn handle_incoming_data_packet(self, mut router_data_rx: Receiver<DataPacket>) {
        while let Some(data_packet) = router_data_rx.recv().await {
            let _ = self.route_packet(data_packet);
        }
        warn!("Router data receiver stream ended");
    }

    pub fn select_best_route(&self, dest_ip: IpAddr) -> Option<RouteEntry> {
        let inner = self
            .inner_r
            .enter()
            .expect("Write handle is saved on router so it is not dropped before the read handles");
        let mut best_route = None;
        // first look in the selected routing table for a match on the prefix of dest_ip
        for (route_key, route_entry) in inner.selected_routing_table.table.iter() {
            if route_key.subnet().address() == dest_ip {
                best_route = Some(route_entry.clone());
            }
        }
        // if no match was found, look in the fallback routing table
        if best_route.is_none() {
            trace!("no match in selected routing table, looking in fallback routing table");
            for (route_key, route_entry) in inner.fallback_routing_table.table.iter() {
                if route_key.subnet().address() == dest_ip {
                    best_route = Some(route_entry.clone());
                }
            }
        }

        // println!("\n\n best route towards {}: {:?}", dest_ip, best_route);

        best_route
    }

    pub async fn propagate_static_route(self) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

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
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

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
    dest_pubkey_map: HashMap<Ipv6Addr, (PublicKey, SharedSecret)>,
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
            dest_pubkey_map: HashMap::new(),
        };

        Ok(router_inner)
    }
    fn remove_peer_interface(&mut self, peer: Peer) {
        self.peer_interfaces.retain(|p| p != &peer);
    }

    fn peer_by_ip(&self, peer_ip: IpAddr) -> Option<Peer> {
        let matching_peer = self
            .peer_interfaces
            .iter()
            .find(|peer| peer.overlay_ip() == peer_ip);

        matching_peer.map(Clone::clone)
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

    fn propagate_static_route(&self, router_id: PublicKey) -> Vec<RouterOpLogEntry> {
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

    fn propagate_selected_routes(&self, router_id: PublicKey) -> Vec<RouterOpLogEntry> {
        let mut updates = vec![];
        for sr in self.selected_routing_table.table.iter() {
            for peer in self.peer_interfaces.iter() {
                let peer_link_cost = peer.link_cost();

                // convert sr.0.prefix to ipv6 addr
                if let IpAddr::V6(prefix) = sr.0.subnet().address() {
                    let og_sender_pubkey_option = self.dest_pubkey_map.get(&prefix);
                    // if the prefix is not in the dest_pubkey_map, then we use the router_id of the node itself
                    let og_sender_pubkey = match og_sender_pubkey_option {
                        Some((pubkey, _)) => *pubkey,
                        None => router_id,
                    };

                    let update = babel::Update::new(
                        UPDATE_INTERVAL,
                        self.router_seqno, // updates receive the seqno of the router
                        sr.1.metric() + Metric::from(peer_link_cost),
                        // the cost of the route is the cost of the route + the cost of the link to the peer
                        sr.0.subnet(),
                        // we looked for the router_id, which is a public key, in the dest_pubkey_map
                        // if the router_id is not in the map, then the route came from the node itself
                        og_sender_pubkey,
                    );
                    // println!(
                    //     "\n\n\n\nPropagting route update to: {}\n {:?}\n\n",
                    //     peer.overlay_ip(),
                    //     update
                    // );
                    updates.push((peer.clone(), update));
                }
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
    AddDestPubkey(Ipv6Addr, PublicKey, SharedSecret),
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
    UpdateFallbackRouteEntry(RouteKey, SeqNo, Metric, PublicKey),
    /// Update the route entry associated to the given route key in the selected route table, if
    /// one exists
    UpdateSelectedRouteEntry(RouteKey, SeqNo, Metric, PublicKey),
    /// Sets the static routes of the router to the provided value.
    SetStaticRoutes(Vec<StaticRoute>),
}

impl left_right::Absorb<RouterOpLogEntry> for RouterInner {
    fn absorb_first(&mut self, operation: &mut RouterOpLogEntry, _: &Self) {
        match operation {
            RouterOpLogEntry::AddDestPubkey(dest, pk, ss) => {
                self.dest_pubkey_map.insert(*dest, (*pk, ss.clone()));
            }
            RouterOpLogEntry::AddPeer(peer) => self.peer_interfaces.push(peer.clone()),
            RouterOpLogEntry::RemovePeer(peer) => {
                self.remove_peer_interface(peer.clone());
                // remove the peer's routes from all routing tables (= all the peers that use the peer as next-hop)
                self.selected_routing_table
                    .table
                    .retain(|_, route_entry| route_entry.next_hop() != peer.overlay_ip());
                self.fallback_routing_table
                    .table
                    .retain(|_, route_entry| route_entry.next_hop() != peer.overlay_ip());
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
            RouterOpLogEntry::UpdateFallbackRouteEntry(rk, seqno, metric, pk) => {
                if let Some(re) = self.fallback_routing_table.table.get_mut(rk) {
                    re.update_seqno(*seqno);
                    re.update_metric(*metric);
                    re.update_router_id(*pk);
                }
            }
            RouterOpLogEntry::UpdateSelectedRouteEntry(rk, seqno, metric, pk) => {
                if let Some(re) = self.selected_routing_table.table.get_mut(rk) {
                    re.update_seqno(*seqno);
                    re.update_metric(*metric);
                    re.update_router_id(*pk);
                }
            }
            RouterOpLogEntry::SetStaticRoutes(static_routes) => {
                self.static_routes = static_routes.clone();
            }
        }
    }

    fn sync_with(&mut self, first: &Self) {
        *self = first.clone()
    }
}
