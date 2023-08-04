use crate::{
    babel,
    crypto::{PublicKey, SecretKey, SharedSecret},
    metric::Metric,
    packet::{ControlPacket, ControlStruct, DataPacket},
    peer::Peer,
    routing_table::{RouteEntry, RouteKey, RoutingTable},
    sequence_number::SeqNo,
    source_table::{FeasibilityDistance, SourceKey, SourceTable},
};
use log::{error, info, trace, warn};
use std::{
    collections::HashMap,
    error::Error,
    fmt::Debug,
    net::{IpAddr, Ipv6Addr},
    sync::{Arc, RwLock},
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_tun::Tun;

const HELLO_INTERVAL: u16 = 4;
const IHU_INTERVAL: u16 = HELLO_INTERVAL * 3;
const UPDATE_INTERVAL: u16 = HELLO_INTERVAL * 4;

#[derive(Debug, Clone, Copy)]
pub struct StaticRoute {
    plen: u8,
    prefix: IpAddr,
}

impl StaticRoute {
    pub fn new(prefix: IpAddr) -> Self {
        Self { plen: 64, prefix }
    }
}

#[derive(Clone)]
pub struct Router {
    inner: Arc<RwLock<RouterInner>>,
}

impl Router {
    pub fn new(
        node_tun: Arc<Tun>,
        node_tun_addr: Ipv6Addr,
        static_routes: Vec<StaticRoute>,
        node_keypair: (SecretKey, PublicKey),
    ) -> Result<Self, Box<dyn Error>> {
        // Tx is passed onto each new peer instance. This enables peers to send control packets to the router.
        let (router_control_tx, router_control_rx) = mpsc::unbounded_channel::<ControlStruct>();
        // Tx is passed onto each new peer instance. This enables peers to send data packets to the router.
        let (router_data_tx, router_data_rx) = mpsc::unbounded_channel::<DataPacket>();

        let router = Router {
            inner: Arc::new(RwLock::new(RouterInner::new(
                node_tun,
                node_tun_addr,
                static_routes,
                router_data_tx,
                router_control_tx,
                node_keypair,
            )?)),
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

        Ok(router)
    }

    pub fn router_control_tx(&self) -> UnboundedSender<ControlStruct> {
        self.inner.read().unwrap().router_control_tx.clone()
    }

    pub fn router_data_tx(&self) -> UnboundedSender<DataPacket> {
        self.inner.read().unwrap().router_data_tx.clone()
    }

    pub fn node_tun_addr(&self) -> Ipv6Addr {
        self.inner.read().unwrap().node_tun_addr
    }

    pub fn node_tun(&self) -> Arc<Tun> {
        self.inner.read().unwrap().node_tun.clone()
    }

    pub fn peer_interfaces(&self) -> Vec<Peer> {
        self.inner.read().unwrap().peer_interfaces.clone()
    }

    pub fn add_peer_interface(&self, peer: Peer) {
        self.inner.write().unwrap().peer_interfaces.push(peer);
    }

    pub fn peer_by_ip(&self, peer_ip: IpAddr) -> Option<Peer> {
        self.inner.read().unwrap().peer_by_ip(peer_ip)
    }

    pub fn peer_exists(&self, peer_underlay_ip: IpAddr) -> bool {
        self.inner.read().unwrap().peer_exists(peer_underlay_ip)
    }

    pub fn source_peer_from_control_struct(&self, control_struct: ControlStruct) -> Option<Peer> {
        let peers = self.peer_interfaces();
        let matching_peer = peers
            .iter()
            .find(|peer| peer.overlay_ip() == control_struct.src_overlay_ip);

        matching_peer.map(Clone::clone)
    }

    pub fn node_secret_key(&self) -> SecretKey {
        self.inner.read().unwrap().node_keypair.0.clone()
    }

    pub fn node_public_key(&self) -> PublicKey {
        self.inner.read().unwrap().node_keypair.1
    }

    /// Add a new destination [`PublicKey`] to the destination map. This will also compute and store
    /// the [`SharedSecret`] from the `Router`'s [`SecretKey`].
    pub fn add_dest_pubkey_map_entry(&self, dest: Ipv6Addr, pubkey: PublicKey) {
        let mut inner = self.inner.write().unwrap();
        let ss = inner.node_keypair.0.shared_secret(&pubkey);

        inner.dest_pubkey_map.insert(dest, (pubkey, ss));
    }

    /// Gets the cached [`SharedSecret`] for the remote.
    pub fn get_shared_secret_from_dest(&self, dest: &Ipv6Addr) -> Option<SharedSecret> {
        self.inner
            .read()
            .unwrap()
            .dest_pubkey_map
            .get(dest)
            .map(|(_, ss)| ss.clone())
    }

    /// Gets the cached [`SharedSecret`] based on the associated [`PublicKey`] of the remote.
    pub fn get_shared_secret_by_pubkey(&self, dest: &PublicKey) -> Option<SharedSecret> {
        for (pk, ss) in self.inner.read().unwrap().dest_pubkey_map.values() {
            if pk == dest {
                return Some(ss.clone());
            }
        }

        None
    }

    pub fn print_selected_routes(&self) {
        let inner = self.inner.read().unwrap();

        let routing_table = &inner.selected_routing_table;
        for route in routing_table.table.iter() {
            println!("Route key: {:?}", route.0);
            println!(
                "Route: {:?}/{:?} (with next-hop: {:?}, metric: {}, selected: {})",
                route.0.prefix(),
                route.0.plen(),
                route.1.next_hop(),
                route.1.metric(),
                route.1.selected()
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

    async fn check_for_dead_peers(self) {
        let ihu_threshold = tokio::time::Duration::from_secs(8);

        loop {
            // check for dead peers every second
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            let mut inner = self.inner.write().unwrap();

            let dead_peers = {
                // a peer is assumed dead when the peer's last sent ihu exceeds a threshold
                let mut dead_peers = Vec::new();
                for peer in inner.peer_interfaces.iter() {
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
            let mut retraction_updates = Vec::<ControlPacket>::new();

            // remove the peer from the peer_interfaces and the routes
            for dead_peer in dead_peers {
                inner.remove_peer_interface(dead_peer.clone());
                // remove the peer's routes from all routing tables (= all the peers that use the peer as next-hop)
                inner
                    .selected_routing_table
                    .table
                    .retain(|_, route_entry| route_entry.next_hop() != dead_peer.overlay_ip());
                inner
                    .fallback_routing_table
                    .table
                    .retain(|_, route_entry| route_entry.next_hop() != dead_peer.overlay_ip());

                // create retraction update for each dead peer
                let retraction_update = ControlPacket::new_update(
                    32,
                    UPDATE_INTERVAL,
                    inner.router_seqno,
                    Metric::infinite(),
                    dead_peer.overlay_ip(), // todo: fix to use actual prefix, not IP
                    inner.router_id,
                );
                retraction_updates.push(retraction_update);
            }

            // send retraction update for the dead peer
            // when other nodes receive this update (with metric 0XFFFF), they should also remove the routing tables entries with that peer as neighbor
            for peer in inner.peer_interfaces.iter() {
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
            match control_struct.control_packet.body.tlv {
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
        match update_packet.control_packet.body.tlv {
            babel::Tlv::Update(update) => {
                let metric = update.metric();
                let plen = update.plen();
                let router_id = update.router_id();
                let prefix = update.prefix();
                let seqno = update.seqno();

                // convert prefix to ipv6 address
                if let IpAddr::V6(prefix_as_ipv6addr) = prefix {
                    // add it the mapping
                    self.add_dest_pubkey_map_entry(prefix_as_ipv6addr, router_id);
                }

                // create route key from incoming update control struct
                // we need the address of the neighbour; this corresponds to the source ip of the control struct as the update is received from the neighbouring peer
                let neighbor_ip = update_packet.src_overlay_ip;
                let route_key_from_update = RouteKey::new(prefix, plen, neighbor_ip);
                // used later to filter out static route
                if self.route_key_is_from_static_route(&route_key_from_update) {
                    return;
                }

                let mut inner = self.inner.write().unwrap();

                // check if a route entry with the same route key exists in both routing tables
                let route_entry_exists = inner
                    .selected_routing_table
                    .table
                    .contains_key(&route_key_from_update)
                    || inner
                        .fallback_routing_table
                        .table
                        .contains_key(&route_key_from_update);

                // if no entry exists (based on prefix, plen AND neighbor field)
                if !route_entry_exists {
                    if metric.is_infinite() || !inner.source_table.is_update_feasible(&update) {
                    } else {
                        // this means that the update is feasible and the metric is not infinite
                        // create a new route entry and add it to the routing table (which requires a new source entry to be created as well)

                        let source_key = SourceKey::new(prefix, plen, router_id);
                        let fd = FeasibilityDistance::new(metric, seqno);
                        inner.source_table.insert(source_key, fd);

                        let route_key = RouteKey::new(prefix, plen, neighbor_ip);
                        let route_entry = RouteEntry::new(
                            source_key,
                            inner
                                .peer_by_ip(neighbor_ip)
                                .expect("peer by ip is known to add route entry"),
                            metric,
                            seqno,
                            neighbor_ip,
                            true,
                        );

                        let mut to_remove = Vec::new();
                        for r in inner.selected_routing_table.table.iter() {
                            if r.0.plen() == plen && r.0.prefix() == prefix {
                                if metric < r.1.metric() {
                                    to_remove.push(r.0.clone());
                                    break; // we can break, as there will be max 1 better route in selected table at any point in time (hence 'selected')
                                           // metric of update is greater than entry's metric
                                } else if metric >= r.1.metric() {
                                    // this means that there is already a better route in our selected routing table,
                                    // so we should add it to fallback instead
                                    inner
                                        .fallback_routing_table
                                        .table
                                        .insert(route_key.clone(), route_entry.clone());
                                    return; // quit the function, work is done here
                                }
                            }
                        }
                        for rk in to_remove {
                            if let Some(mut old_selected) = inner.selected_routing_table.remove(&rk)
                            {
                                old_selected.set_selected(false);
                                inner.fallback_routing_table.insert(rk, old_selected);
                            }
                        }
                        // insert the route into selected (we might have placed one other route, that was previously the best, in the fallback)
                        inner
                            .selected_routing_table
                            .table
                            .insert(route_key, route_entry);
                    }
                } else {
                    // check if the update is a retraction
                    if inner.source_table.is_update_feasible(&update) && metric.is_infinite() {
                        // if the update is a retraction, we remove the entry from the routing tables
                        // we also remove the corresponding source entry???
                        if inner
                            .selected_routing_table
                            .table
                            .contains_key(&route_key_from_update)
                        {
                            inner.selected_routing_table.remove(&route_key_from_update);
                        }
                        if inner
                            .fallback_routing_table
                            .table
                            .contains_key(&route_key_from_update)
                        {
                            inner.fallback_routing_table.remove(&route_key_from_update);
                        }
                        // remove the corresponding source entry
                        let source_key = SourceKey::new(prefix, plen, router_id);
                        inner.source_table.remove(&source_key);
                        return;
                    }
                    // if the entry is currently selected, the update is unfeasible, and the router-id of the update is equal
                    // to the router-id of the entry, then we ignore the update
                    if inner
                        .selected_routing_table
                        .table
                        .contains_key(&route_key_from_update)
                    {
                        let route_entry = inner
                            .selected_routing_table
                            .table
                            .get(&route_key_from_update)
                            .unwrap();
                        if !inner.source_table.is_update_feasible(&update)
                            && route_entry.source().router_id() == router_id
                        {
                            return;
                        }
                        // update the entry's seqno, metric and router-id
                        let route_entry = inner
                            .selected_routing_table
                            .table
                            .get_mut(&route_key_from_update)
                            .unwrap();
                        route_entry.update_seqno(seqno);
                        route_entry.update_metric(metric);
                        route_entry.update_router_id(router_id);
                    }
                    // otherwise
                    else {
                        let route_entry = inner
                            .fallback_routing_table
                            .table
                            .get_mut(&route_key_from_update)
                            .unwrap();
                        // update the entry's seqno, metric and router-id
                        route_entry.update_seqno(seqno);
                        route_entry.update_metric(metric);
                        route_entry.update_router_id(router_id);

                        if !inner.source_table.is_update_feasible(&update) {
                            // if the update is unfeasible, we remove the entry from the selected routing table
                            inner
                                .selected_routing_table
                                .table
                                .remove(&route_key_from_update);
                            // should we remove it from the selected and add it to fallback here???
                        }
                    }
                }
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
        let inner = self.inner.read().unwrap();

        for sr in inner.static_routes.iter() {
            if sr.plen == route_key.plen() && sr.prefix == route_key.prefix() {
                return true;
            }
        }
        false
    }

    async fn handle_incoming_data_packet(self, mut router_data_rx: UnboundedReceiver<DataPacket>) {
        // If destination IP of packet is same as TUN interface IP, send to TUN interface
        // If destination IP of packet is not same as TUN interface IP, send to peer with matching overlay IP
        let node_tun = self.node_tun();
        let node_tun_addr = self.node_tun_addr();
        loop {
            while let Some(data_packet) = router_data_rx.recv().await {
                trace!("Incoming data packet, with dest_ip: {} (side node, this node's tun addr is: {})", data_packet.dest_ip, node_tun_addr);

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
                            continue;
                        }
                    };
                    match node_tun.send(&decrypted_raw_data).await {
                        Ok(_) => {}
                        Err(e) => {
                            error!("Error sending data packet to TUN interface: {:?}", e)
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
                                    }
                                }
                                None => {
                                    // route but no peer
                                    warn!("Dropping data packet, no peer found");
                                }
                            }
                        }
                        None => {
                            trace!("Error sending data packet, no route found");
                        }
                    }
                }
            }
        }
    }

    pub fn select_best_route(&self, dest_ip: IpAddr) -> Option<RouteEntry> {
        let inner = self.inner.read().unwrap();
        let mut best_route = None;
        // first look in the selected routing table for a match on the prefix of dest_ip
        for (route_key, route_entry) in inner.selected_routing_table.table.iter() {
            if route_key.prefix() == dest_ip {
                best_route = Some(route_entry.clone());
            }
        }
        // if no match was found, look in the fallback routing table
        if best_route.is_none() {
            trace!("no match in selected routing table, looking in fallback routing table");
            for (route_key, route_entry) in inner.fallback_routing_table.table.iter() {
                if route_key.prefix() == dest_ip {
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

            let mut inner = self.inner.write().unwrap();
            inner.propagate_static_route();
        }
    }

    pub async fn propagate_selected_routes(self) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            let mut inner = self.inner.write().unwrap();
            inner.propagate_selected_routes();
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
    router_id: PublicKey,
    peer_interfaces: Vec<Peer>,
    router_control_tx: UnboundedSender<ControlStruct>,
    router_data_tx: UnboundedSender<DataPacket>,
    node_tun: Arc<Tun>,
    node_tun_addr: Ipv6Addr,
    selected_routing_table: RoutingTable,
    fallback_routing_table: RoutingTable,
    source_table: SourceTable,
    router_seqno: SeqNo,
    static_routes: Vec<StaticRoute>,
    node_keypair: (SecretKey, PublicKey),
    // map that contains the overlay ips of peers and their respective public keys
    dest_pubkey_map: HashMap<Ipv6Addr, (PublicKey, SharedSecret)>,
}

impl RouterInner {
    pub fn new(
        node_tun: Arc<Tun>,
        node_tun_addr: Ipv6Addr,
        static_routes: Vec<StaticRoute>,
        router_data_tx: UnboundedSender<DataPacket>,
        router_control_tx: UnboundedSender<ControlStruct>,
        node_keypair: (SecretKey, PublicKey),
    ) -> Result<Self, Box<dyn Error>> {
        let router_inner = RouterInner {
            router_id: node_keypair.1,
            peer_interfaces: Vec::new(),
            router_control_tx,
            router_data_tx,
            node_tun,
            selected_routing_table: RoutingTable::new(),
            fallback_routing_table: RoutingTable::new(),
            source_table: SourceTable::new(),
            router_seqno: SeqNo::default(),
            static_routes,
            node_keypair,
            node_tun_addr,
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

    fn send_update(&mut self, peer: &Peer, update_packet: ControlPacket) {
        // before sending an update, the source table might need to be updated
        match update_packet.body.tlv {
            babel::Tlv::Update(ref update) => {
                let plen = update.plen();
                let seqno = update.seqno();
                let metric = update.metric();
                let prefix = update.prefix();
                let router_id = update.router_id();

                let source_key = SourceKey::new(prefix, plen, router_id);

                if let Some(source_entry) = self.source_table.get(&source_key) {
                    // if seqno of the update is greater than the seqno in the source table, update the source table
                    if !seqno.lt(&source_entry.seqno()) {
                        self.source_table
                            .insert(source_key, FeasibilityDistance::new(metric, seqno));
                    }
                    // if seqno of the update is equal to the seqno in the source table, update the source table if the metric (of the update) is lower
                    else if seqno == source_entry.seqno() && source_entry.metric() > metric {
                        self.source_table.insert(
                            source_key,
                            FeasibilityDistance::new(metric, source_entry.seqno()),
                        );
                    }
                }
                // no entry for this source key, so insert it
                else {
                    self.source_table
                        .insert(source_key, FeasibilityDistance::new(metric, seqno));
                }

                // send the update to the peer
                if let Err(e) = peer.send_control_packet(update_packet) {
                    error!("Error sending update to peer: {:?}", e);
                }
            }
            _ => {
                panic!("Control packet is not a correct Update packet");
            }
        }
    }

    fn propagate_static_route(&mut self) {
        let mut updates = vec![];
        for sr in self.static_routes.iter() {
            for peer in self.peer_interfaces.iter() {
                let update = ControlPacket::new_update(
                    sr.plen, // static routes have plen 32
                    UPDATE_INTERVAL,
                    self.router_seqno, // updates receive the seqno of the router
                    peer.link_cost().into(), // direct connection to other peer, so the only cost is the cost towards the peer
                    sr.prefix, // the prefix of a static route corresponds to the TUN addr of the node
                    self.router_id,
                );
                updates.push((peer.clone(), update));
            }
        }
        for (peer, update) in updates {
            self.send_update(&peer, update);
        }
    }

    fn propagate_selected_routes(&mut self) {
        let mut updates = vec![];
        for sr in self.selected_routing_table.table.iter() {
            for peer in self.peer_interfaces.iter() {
                let peer_link_cost = peer.link_cost();

                // convert sr.0.prefix to ipv6 addr
                if let IpAddr::V6(prefix) = sr.0.prefix() {
                    let og_sender_pubkey_option = self.dest_pubkey_map.get(&prefix);
                    // if the prefix is not in the dest_pubkey_map, then we use the router_id of the node itself
                    let og_sender_pubkey = match og_sender_pubkey_option {
                        Some((pubkey, _)) => *pubkey,
                        None => self.router_id,
                    };

                    let update = ControlPacket::new_update(
                        sr.0.plen(),
                        UPDATE_INTERVAL,
                        self.router_seqno, // updates receive the seqno of the router
                        sr.1.metric() + Metric::from(peer_link_cost),
                        // the cost of the route is the cost of the route + the cost of the link to the peer
                        sr.0.prefix(), // the prefix of a static route corresponds to the TUN addr of the node
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

        for (peer, update) in updates {
            self.send_update(&peer, update);
        }
    }

    fn peer_exists(&self, peer_underlay_ip: IpAddr) -> bool {
        self.peer_interfaces
            .iter()
            .any(|peer| peer.underlay_ip() == peer_underlay_ip)
    }
}
