use crate::{
    packet::{BabelTLV, BabelTLVType, ControlPacket, ControlStruct, DataPacket},
    peer::Peer,
    routing_table::{RouteEntry, RouteKey, RoutingTable},
    source_table::{self, FeasibilityDistance, SourceKey, SourceTable},
};
use rand::Rng;
use std::{
    error::Error,
    fmt::Debug,
    net::IpAddr,
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
    seqno: u16,
}

impl StaticRoute {
    pub fn new(prefix: IpAddr) -> Self {
        Self {
            plen: 32,
            prefix,
            seqno: 0,
        }
    }
}

#[derive(Clone)]
pub struct Router {
    inner: Arc<RwLock<RouterInner>>,
}

impl Router {
    pub fn new(
        node_tun: Arc<Tun>,
        static_routes: Vec<StaticRoute>,
    ) -> Result<Self, Box<dyn Error>> {
        // Tx is passed onto each new peer instance. This enables peers to send control packets to the router.
        let (router_control_tx, router_control_rx) = mpsc::unbounded_channel::<ControlStruct>();
        // Tx is passed onto each new peer instance. This enables peers to send data packets to the router.
        let (router_data_tx, router_data_rx) = mpsc::unbounded_channel::<DataPacket>();

        let router = Router {
            inner: Arc::new(RwLock::new(RouterInner::new(
                node_tun,
                static_routes,
                router_data_tx,
                router_control_tx,
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
        //tokio::spawn(Router::propagate_routes(router.clone()));

        Ok(router)
    }

    pub fn router_id(&self) -> u64 {
        self.inner.read().unwrap().router_id
    }

    pub fn router_control_tx(&self) -> UnboundedSender<ControlStruct> {
        self.inner.read().unwrap().router_control_tx.clone()
    }

    pub fn router_data_tx(&self) -> UnboundedSender<DataPacket> {
        self.inner.read().unwrap().router_data_tx.clone()
    }

    pub fn node_tun_addr(&self) -> IpAddr {
        IpAddr::V4(self.inner.read().unwrap().node_tun.address().unwrap())
    }

    pub fn node_tun(&self) -> Arc<Tun> {
        self.inner.read().unwrap().node_tun.clone()
    }

    pub fn router_seqno(&self) -> u16 {
        self.inner.read().unwrap().router_seqno
    }

    pub fn increment_router_seqno(&self) {
        self.inner.write().unwrap().router_seqno += 1;
    }

    pub fn peer_interfaces(&self) -> Vec<Peer> {
        self.inner.read().unwrap().peer_interfaces.clone()
    }

    pub fn add_peer_interface(&self, peer: Peer) {
        self.inner.write().unwrap().peer_interfaces.push(peer);
    }

    pub fn remove_peer_interface(&self, peer: Peer) {
        let mut peer_interfaces = self.inner.write().unwrap().peer_interfaces.clone();
        peer_interfaces.retain(|p| p != &peer);
        self.inner.write().unwrap().peer_interfaces = peer_interfaces;
    }

    pub fn static_routes(&self) -> Vec<StaticRoute> {
        self.inner.read().unwrap().static_routes.clone()
    }

    pub fn peer_by_ip(&self, peer_ip: IpAddr) -> Option<Peer> {
        self.inner.read().unwrap().peer_by_ip(peer_ip)
    }

    pub fn source_peer_from_control_struct(&self, control_struct: ControlStruct) -> Option<Peer> {
        let peers = self.peer_interfaces();
        let matching_peer = peers
            .iter()
            .find(|peer| peer.overlay_ip() == control_struct.src_overlay_ip);

        matching_peer.map(Clone::clone)
    }

    // pub fn select_best_route(&self, dest_ip: ipaddr) -> option<routeentry> {

    //     let inner = self.inner.read().unwrap();

    //     // first look in the routing table for all routekeys where prefix == dest_ip
    //     let routing_table = &inner.routing_table;
    //     let mut matching_routes: vec<&routeentry> = vec::new();

    //     for route in routing_table.table.iter() {
    //         if route.0.prefix == dest_ip {
    //             // note -- this is not correct, we need to check if the dest_ip is in the prefix range
    //             matching_routes.push(route.1);
    //             println!("found matching route with next-hop: {:?}", route.1.next_hop);
    //         }
    //     }

    //     // now we have all routes that match the destination ip
    //     // we need to select the route with the lowest metric
    //     let mut best_route: option<&routeentry> = none;
    //     let mut best_metric: u16 = u16::max;

    //     for route in matching_routes.iter() {
    //         if route.metric < best_metric {
    //             best_route = some(route);
    //             best_metric = route.metric;
    //         }
    //     }

    //     match best_route {
    //         some(route) => some(route.clone()),
    //         none => none,
    //     }
    // }

    pub fn print_selected_routes(&self) {
        let inner = self.inner.read().unwrap();

        let routing_table = &inner.selected_routing_table;
        for route in routing_table.table.iter() {
            println!("Route key: {:?}", route.0);
            println!(
                "Route: {:?}/{:?} (with next-hop: {:?}, metric: {}, selected: {})",
                route.0.prefix, route.0.plen, route.1.next_hop, route.1.metric, route.1.selected
            );
            println!("As advertised by: {:?}", route.1.source.router_id);
        }
    }

    pub fn print_source_table(&self) {
        let inner = self.inner.read().unwrap();

        let source_table = &inner.source_table;
        for (sk, se) in source_table.table.iter() {
            println!("Source key: {:?}", sk);
            println!("Source entry: {:?}", se);
            println!("\n");
        }
    }

    async fn handle_incoming_control_packet(
        self,
        mut router_control_rx: UnboundedReceiver<ControlStruct>,
    ) {
        while let Some(control_struct) = router_control_rx.recv().await {
            match control_struct.control_packet.body.tlv_type {
                BabelTLVType::AckReq => todo!(),
                BabelTLVType::Ack => todo!(),
                BabelTLVType::Hello => Self::handle_incoming_hello(control_struct),
                BabelTLVType::IHU => Self::handle_incoming_ihu(&self, control_struct),
                BabelTLVType::NextHop => todo!(),
                BabelTLVType::Update => Self::handle_incoming_update(&self, control_struct),
                BabelTLVType::RouteReq => todo!(),
                BabelTLVType::SeqnoReq => todo!(),
            }
        }
    }

    fn handle_incoming_hello(control_struct: ControlStruct) {
        let destination_ip = control_struct.src_overlay_ip;
        control_struct.reply(ControlPacket::new_ihu(IHU_INTERVAL, destination_ip));
    }

    fn handle_incoming_ihu(&self, control_struct: ControlStruct) {
        if let Some(source_peer) = self.source_peer_from_control_struct(control_struct) {
            // reset the IHU timer associated with the peer
            source_peer.reset_ihu_timer(tokio::time::Duration::from_secs(IHU_INTERVAL as u64));
            // measure time between Hello and and IHU and set the link cost
            let time_diff = tokio::time::Instant::now()
                .duration_since(source_peer.time_last_received_hello())
                .as_millis();

            source_peer.set_link_cost(time_diff as u16);
        }
    }

    // incoming update can only be received by a Peer this node has a direct link to
    fn handle_incoming_update(&self, update: ControlStruct) {
        println!("entering handle_incoming_update");

        match update.control_packet.body.tlv {
            BabelTLV::Update { plen, interval: _, seqno, metric, prefix, router_id } => {
                    
                // create route key from incoming update control struct
                // we need the address of the neighbour; this corresponds to the source ip of the control struct as the update is received from the neighbouring peer
                let neighbor_ip = update.src_overlay_ip;
                let route_key_from_update = RouteKey { 
                    neighbor: neighbor_ip, 
                    plen, 
                    prefix,
                };

                let mut inner = self.inner.write().unwrap();

                // check if a route entry with the same route key exists in both routing tables
                let mut route_entry_exists = false;
                for (route_key, _) in inner.selected_routing_table
                    .table
                    .iter()
                    .zip(inner.fallback_routing_table.table.iter())
                {
                    if route_key.0 == &route_key_from_update {
                        route_entry_exists = true;
                    }
                }

                // if no entry exists
                if !route_entry_exists {
                    // if the update is unfeasible, or the metric is inifinite, we ignore the update
                    if metric == u16::MAX || !self.update_feasible(&update, &inner.source_table) {
                        return;
                    }
                    else {
                        // this means that the update is feasible and the metric is not infinite
                        // create a new route entry and add it to the routing table (which requires a new source entry to be created as well)

                        let source_key = SourceKey { prefix, plen, router_id };
                        let fd = FeasibilityDistance{ metric, seqno };
                        inner.source_table.insert(source_key, fd);

                        let route_key = RouteKey {
                            prefix,
                            plen,
                            neighbor: neighbor_ip,
                        };
                        let route_entry = RouteEntry {
                            source: source_key,
                            neighbor: inner.peer_by_ip(neighbor_ip).unwrap(),
                            metric,
                            seqno,
                            next_hop: neighbor_ip, // CHECK IF THIS IS CORRECT!!!
                            selected: true,
                        };
                        // if no entry exists, it should be added to the selected routing table as new entries are always selected
                        inner.selected_routing_table.table.insert(route_key, route_entry);

                    }
                }
                // entry exists
                else {
                    // if the entry is currently selected, the update is unfeasible, and the router-id of the update is equal
                    // to the router-id of the entry, then we ignore the update
                    if inner.selected_routing_table.table.contains_key(&route_key_from_update) {
                        let route_entry = inner.selected_routing_table.table.get(&route_key_from_update).unwrap();
                        if !self.update_feasible(&update, &inner.source_table) && route_entry.source.router_id == router_id {
                            return;
                        }
                    }
                    // otherwise
                    else {
                        let route_entry = inner.fallback_routing_table.table.get_mut(&route_key_from_update).unwrap();
                        // update the entry's seqno, metric and router-id
                        route_entry.update_seqno(seqno);
                        route_entry.update_metric(metric);
                        route_entry.update_router_id(router_id);

                        if !self.update_feasible(&update, &inner.source_table) {
                            // if the update is unfeasible, we remove the entry from the selected routing table
                            inner.selected_routing_table.table.remove(&route_key_from_update);
                            // TODO: if the updated caused the router ID of the entry to change, we should also sent triggered update
                        }
                    }
                }
                // RUN ROUTE SELECTION
                
            },
            _ => {
                panic!("Received update with wrong TLV type");
            }
        }

    }

    // we gebruiken self niet in de functie --> daarop functie eigenllijk beter op de source table implementeren
    fn update_feasible(&self, update: &ControlStruct, source_table: &SourceTable) -> bool {
        // Before an update is accepted it should be checked against the feasbility condition
        // If an entry in the source table with the same source key exists, we perform the feasbility check
        // If no entry exists yet, the update is accepted as there is no better alternative available (yet)
        match update.control_packet.body.tlv {
            BabelTLV::Update {
                plen,
                interval: _,
                seqno,
                metric,
                prefix,
                router_id,
            } => {
                let source_key = SourceKey {
                    prefix,
                    plen,
                    router_id,
                };
                match source_table.get(&source_key) {
                    Some(&entry) => {
                        return seqno > entry.seqno
                            || (seqno == entry.seqno && metric < entry.metric);
                    }
                    None => return true,
                }
            }
            _ => {
                eprintln!("Error accepting update, control struct did not match update packet");
                return false;
            }
        }
    }

    async fn handle_incoming_data_packet(self, mut router_data_rx: UnboundedReceiver<DataPacket>) {
        // If destination IP of packet is same as TUN interface IP, send to TUN interface
        // If destination IP of packet is not same as TUN interface IP, send to peer with matching overlay IP
        let node_tun = self.node_tun();
        let node_tun_addr = node_tun.address().unwrap();
        loop {
            while let Some(data_packet) = router_data_rx.recv().await {
                match data_packet.dest_ip {
                    x if x == node_tun_addr => match node_tun.send(&data_packet.raw_data).await {
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("Error sending data packet to TUN interface: {:?}", e)
                        }
                    },
                    _ => {
                        // let best_route = self.select_best_route(IpAddr::V4(data_packet.dest_ip));
                        // let peer = self.peer_by_ip(best_route.unwrap().next_hop).unwrap();
                        // if let Err(e) = peer.send_data_packet(data_packet) {
                        //     eprintln!("Error sending data packet to peer: {:?}", e);
                        // }
                    }
                }
            }
        }
    }

    pub async fn propagate_static_route(self) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            let mut inner = self.inner.write().unwrap();
            inner.propagate_static_route();
        }
    }

    pub async fn propagate_routes(self) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            let mut inner = self.inner.write().unwrap();
            inner.propagate_routes();
        }
    }

    // routing table updates are send periodically to all directly connected peers
    // updates are used to advertise new routes or the retract existing routes (retracting when the metric is set to 0xFFFF)
    // this function is run when the route_update timer expires
    // pub async fn propagate_selected_routes(self) {

    //     loop {
    //         tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    //         let mut inner = self.inner.write().unwrap();

    //         let router_id = inner.router_id;
    //         let router_seqno = inner.router_seqno;

    //         let static_routes = inner.static_routes.clone();
    //         for (key, entry) in inner.routing_table.table.iter_mut().filter(|(key, _)| {
    //             // Filter out the static routes
    //             for sr in static_routes.iter() {
    //                 if key.prefix == sr.prefix && key.plen == sr.plen {
    //                     return false;
    //                 }
    //             }
    //             true
    //         }) {
    //             entry.seqno += 1;
    //             for peer in peers.iter() {
    //                 let link_cost = peer.link_cost();

    //                 // DEBUG purposes
    //                 if entry.metric > u16::MAX - 1 - link_cost {
    //                     println!("SENDING UPDATE WITH METRIC: {}", u16::MAX - 1);
    //                 } else {
    //                     println!("SENDING UPDATE WITH METRIC: {}", entry.metric + link_cost);
    //                 }

    //                 let update = ControlPacket::new_update(
    //                     key.plen,
    //                     UPDATE_INTERVAL as u16,
    //                     entry.seqno,
    //                     if entry.metric > u16::MAX - 1 - link_cost {
    //                         u16::MAX - 1
    //                     } else {
    //                         entry.metric + link_cost
    //                     },
    //                     key.prefix,
    //                     entry.source.router_id,
    //                 );

    //                 if peer.send_control_packet(update).is_err() {
    //                     println!("route update packet dropped");
    //                 }
    //             }
    //         }

    //         // FILTER VOOR SELECTED ROUTE NODIG --> we sturen nu alle routes maar eignelijk moeten we enkel de selected routes gaan propagaten
    //     }
    // }

    async fn start_periodic_hello_sender(self) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(HELLO_INTERVAL as u64)).await;

            for peer in self.peer_interfaces().iter_mut() {
                let hello = ControlPacket::new_hello(peer, HELLO_INTERVAL);
                peer.set_time_last_received_hello(tokio::time::Instant::now());

                if let Err(error) = peer.send_control_packet(hello) {
                    eprintln!("Error sending hello to peer: {}", error);
                }
            }
        }
    }
}

pub struct RouterInner {
    pub router_id: u64,
    peer_interfaces: Vec<Peer>,
    router_control_tx: UnboundedSender<ControlStruct>,
    router_data_tx: UnboundedSender<DataPacket>,
    node_tun: Arc<Tun>,
    selected_routing_table: RoutingTable,
    fallback_routing_table: RoutingTable,
    source_table: SourceTable,
    router_seqno: u16,
    static_routes: Vec<StaticRoute>,
}

impl RouterInner {
    pub fn new(
        node_tun: Arc<Tun>,
        static_routes: Vec<StaticRoute>,
        router_data_tx: UnboundedSender<DataPacket>,
        router_control_tx: UnboundedSender<ControlStruct>,
    ) -> Result<Self, Box<dyn Error>> {
        let router_inner = RouterInner {
            router_id: rand::thread_rng().gen(),
            peer_interfaces: Vec::new(),
            router_control_tx,
            router_data_tx,
            node_tun: node_tun,
            selected_routing_table: RoutingTable::new(),
            fallback_routing_table: RoutingTable::new(),
            source_table: SourceTable::new(),
            router_seqno: 0,
            static_routes: static_routes,
        };

        Ok(router_inner)
    }

    fn peer_by_ip(&self, peer_ip: IpAddr) -> Option<Peer> {
        let matching_peer = self
            .peer_interfaces
            .iter()
            .find(|peer| peer.overlay_ip() == peer_ip);

        matching_peer.map(Clone::clone)
    }

    fn send_update(&mut self, peer: &Peer, update: ControlPacket) {
        // before sending an update, the source table might need to be updated
        match update.body.tlv {
            BabelTLV::Update {
                plen,
                interval: _,
                seqno,
                metric,
                prefix,
                router_id,
            } => {
                let source_key = SourceKey {
                    prefix,
                    plen,
                    router_id,
                };

                if let Some(source_entry) = self.source_table.get(&source_key) {
                    // if seqno of the update is greater than the seqno in the source table, update the source table
                    if seqno > source_entry.metric {
                        self.source_table
                            .insert(source_key, FeasibilityDistance::new(metric, seqno));
                    }
                    // if seqno of the update is equal to the seqno in the source table, update the source table if the metric (of the update) is lower
                    else if seqno == source_entry.seqno && source_entry.metric > metric {
                        self.source_table.insert(
                            source_key,
                            FeasibilityDistance::new(metric, source_entry.seqno),
                        );
                    }
                }
                // no entry for this source key, so insert it
                else {
                    self.source_table
                        .insert(source_key, FeasibilityDistance::new(metric, seqno));
                }

                // send the update to the peer
                if let Err(e) = peer.send_control_packet(update) {
                    println!("Error sending update to peer: {:?}", e);
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
                    UPDATE_INTERVAL as u16,
                    self.router_seqno, // updates receive the seqno of the router
                    peer.link_cost(), // direct connection to other peer, so the only cost is the cost towards the peer
                    sr.prefix, // the prefix of a static route corresponds to the TUN addr of the node
                    self.router_id,
                );

                println!("Propagting static route update: {:?}", update);
                updates.push((peer.clone(), update));
            }
        }

        for (peer, update) in updates {
            self.send_update(&peer, update);
        }
    }

    fn propagate_routes(&self) {}
}
