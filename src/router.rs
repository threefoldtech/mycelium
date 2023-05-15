use crate::{
    packet::{BabelTLV, BabelTLVType, ControlPacket, ControlStruct, DataPacket},
    peer::Peer,
    routing_table::{RouteEntry, RouteKey, RoutingTable},
    source_table::{FeasibilityDistance, SourceKey, SourceTable},
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
        tokio::spawn(Router::propagate_routes(router.clone()));

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

    // pub fn static_routes(&self) -> Vec<StaticRoute> {
    //     self.inner.read().unwrap().static_routes.clone()
    // }

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

    pub fn select_best_route(&self, dest_ip: IpAddr) -> Option<RouteEntry> {

        let inner = self.inner.read().unwrap();

        // first look in the routing table for all routekeys where prefix == dest_ip
        let routing_table = &inner.routing_table;
        let mut matching_routes: Vec<&RouteEntry> = Vec::new();

        for route in routing_table.table.iter() {
            if route.0.prefix == dest_ip {
                // NOTE -- this is not correct, we need to check if the dest_ip is in the prefix range
                matching_routes.push(route.1);
                println!("Found matching route with next-hop: {:?}", route.1.next_hop);
            }
        }

        // now we have all routes that match the destination ip
        // we need to select the route with the lowest metric
        let mut best_route: Option<&RouteEntry> = None;
        let mut best_metric: u16 = u16::MAX;

        for route in matching_routes.iter() {
            if route.metric < best_metric {
                best_route = Some(route);
                best_metric = route.metric;
            }
        }

        match best_route {
            Some(route) => Some(route.clone()),
            None => None,
        }
    }



    pub fn print_routes(&self) {

        let inner = self.inner.read().unwrap();

        let routing_table = &inner.routing_table;
        for route in routing_table.table.iter() {
            println!("Route key: {:?}", route.0);
            println!(
                "Route: {:?}/{:?} (with next-hop: {:?}, metric: {}, selected: {})",
                route.0.prefix, route.0.plen, route.1.next_hop, route.1.metric, route.1.selected
            );
            println!("As advertised by: {:?}", route.1.source.router_id);
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
                BabelTLVType::Update => {
                    //let mut source_table = self.source_table();
                    //let mut routing_table = self.routing_table.lock().unwrap();
                    Self::handle_incoming_update(&self, control_struct);
                }
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

    fn handle_incoming_update(&self, update: ControlStruct) {
        
        let mut inner = self.inner.write().unwrap();

        if inner.source_table.is_feasible(&update) {
            println!("incoming update is feasible");
            inner.source_table.update(&update);

            // get routing table entry for the source of the update
            match update.control_packet.body.tlv {
                BabelTLV::Update {
                    plen,
                    interval: _,
                    seqno,
                    metric,
                    prefix,
                    router_id,
                } => {
                    println!(
                        "Received update from {} for {:?} with seqno {} and metric {}",
                        router_id, prefix, seqno, metric
                    );
                    let source_ip = update.src_overlay_ip;

                    // get RouteEntry for the source of the update
                    let route_key = &RouteKey {
                        prefix,
                        plen,
                        neighbor: source_ip,
                    };
                    if let Some(route_entry) = inner.routing_table.clone().table.get_mut(route_key)
                    {
                        // let should_be_selected = Router::set_incoming_update_selected(
                        //     routing_table,
                        //     route_key.clone(),
                        //     metric,
                        // );
                        route_entry.update(update);
                    } else {
                        let source_key = SourceKey {
                            prefix,
                            plen,
                            router_id,
                        };
                        let feas_dist = FeasibilityDistance(metric, seqno);

                        // create the source table entry
                        inner.source_table.insert(source_key.clone(), feas_dist);

                        // now we can create the routing table entry
                        let route_key = RouteKey {
                            prefix, // prefix is peer that ANNOUNCED the route
                            plen,
                            neighbor: update.src_overlay_ip,
                        };

                        let peer = inner.peer_by_ip(update.src_overlay_ip).unwrap();
                        let route_entry = RouteEntry::new(
                            source_key,
                            peer.clone(),
                            metric,
                            seqno,
                            update.src_overlay_ip,
                            true, // for new entries the route is always selected
                        );

                        // create the routing table entry
                        inner.routing_table.insert(route_key, route_entry);
                    }
                }
                _ => {}
            }
        } else {
            println!("incoming update is NOT feasible");
            // received update is not feasible
            // unselect to route if it was selected
        }
    }

    // fn set_incoming_update_selected(
    //     routing_table: &mut RoutingTable,
    //     route_key: RouteKey,
    //     new_metric: u16,
    // ) -> bool {
    //     // first check if the routing table has an entry for that key or not, if it has no entry return true
    //     if !routing_table.table.contains_key(&route_key) {
    //         println!("no entry for this route key, returning true");
    //         return true;
    //     } else {
    //         let route_entry = routing_table.table.get_mut(&route_key).unwrap();
    //         println!(
    //             "current metric: {}, new metric: {}",
    //             route_entry.metric, new_metric
    //         );
    //         if route_entry.metric < new_metric {
    //             return false;
    //         }
    //         return true;
    //     }
    // }

    async fn handle_incoming_data_packet(self, mut router_data_rx: UnboundedReceiver<DataPacket>) {
        // If destination IP of packet is same as TUN interface IP, send to TUN interface
        // If destination IP of packet is not same as TUN interface IP, send to peer with matching overlay IP
        let node_tun = self.node_tun();
        let node_tun_addr = node_tun.address().unwrap();
        loop {
            while let Some(data_packet) = router_data_rx.recv().await {
                match data_packet.dest_ip {
                    x if x == node_tun_addr => {
                        match node_tun.send(&data_packet.raw_data).await {
                            Ok(_) => {}
                            Err(e) => {
                                eprintln!("Error sending data packet to TUN interface: {:?}", e)
                            }
                        }
                    }
                    _ => {
                        let best_route = self.select_best_route(IpAddr::V4(data_packet.dest_ip));
                        let peer = self.peer_by_ip(best_route.unwrap().next_hop).unwrap();
                        if let Err(e) = peer.send_data_packet(data_packet) {
                            eprintln!("Error sending data packet to peer: {:?}", e);
                        }
                    }
                }
            }
        }
    }

    // routing table updates are send periodically to all directly connected peers
    // updates are used to advertise new routes or the retract existing routes (retracting when the metric is set to 0xFFFF)
    // this function is run when the route_update timer expires
    pub async fn propagate_routes(self) {


        loop {
            // routes are propagated every 3 secs
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            let mut inner = self.inner.write().unwrap();

            let router_id = inner.router_id;
            let peers = inner.peer_interfaces.clone();
            for route in inner.static_routes.iter_mut() {
                route.seqno += 1;
                for peer in peers.iter() {
                    let update = ControlPacket::new_update(
                        route.plen,
                        UPDATE_INTERVAL as u16,
                        route.seqno,
                        peer.link_cost(),
                        route.prefix,
                        router_id,
                    );

                    if peer.send_control_packet(update).is_err() {
                        println!("could not send static route to peer");
                    }
                }
            }

            let static_routes = inner.static_routes.clone();
            for (key, entry) in inner.routing_table.table.iter_mut().filter(|(key, _)| {
                // Filter out the static routes
                for sr in static_routes.iter() {
                    if key.prefix == sr.prefix && key.plen == sr.plen {
                        return false;
                    }
                }
                true
            }) {
                entry.seqno += 1;
                for peer in peers.iter() {
                    let link_cost = peer.link_cost();

                    // DEBUG purposes
                    if entry.metric > u16::MAX - 1 - link_cost {
                        println!("SENDING UPDATE WITH METRIC: {}", u16::MAX - 1);
                    } else {
                        println!("SENDING UPDATE WITH METRIC: {}", entry.metric + link_cost);
                    }

                    let update = ControlPacket::new_update(
                        key.plen,
                        UPDATE_INTERVAL as u16,
                        entry.seqno,
                        if entry.metric > u16::MAX - 1 - link_cost {
                            u16::MAX - 1
                        } else {
                            entry.metric + link_cost
                        },
                        key.prefix,
                        entry.source.router_id,
                    );

                    if peer.send_control_packet(update).is_err() {
                        println!("route update packet dropped");
                    }
                }
            }

            // FILTER VOOR SELECTED ROUTE NODIG --> we sturen nu alle routes maar eignelijk moeten we enkel de selected routes gaan propagaten
        }
    }

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
    routing_table: RoutingTable,
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
            routing_table: RoutingTable::new(),
            source_table: SourceTable::new(),
            router_seqno: 0,
            static_routes: static_routes,
        };

        Ok(router_inner)
    }

    fn peer_by_ip(&self, peer_ip: IpAddr) -> Option<Peer> {
        let matching_peer = self.peer_interfaces.iter().find(|peer| peer.overlay_ip() == peer_ip);

        matching_peer.map(Clone::clone)
    }
}
