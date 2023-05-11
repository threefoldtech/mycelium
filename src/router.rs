use crate::{
    packet::{
        BabelPacketBody, BabelPacketHeader, BabelTLV, BabelTLVType, ControlPacket, ControlStruct,
        DataPacket,
    },
    peer::Peer,
    routing_table::{RouteEntry, RouteKey, RoutingTable, self},
    source_table::{SourceKey, SourceTable, FeasibilityDistance, self}, timers::{Timer, self},
};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
};
use rand::Rng;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_tun::Tun;

const HELLO_INTERVAL: u16 = 4;
const IHU_INTERVAL: u16 = HELLO_INTERVAL * 3;
const UPDATE_INTERVAL: u16 = HELLO_INTERVAL * 4;

#[derive(Clone)]
pub struct Router {
    pub router_id: u64,
    // The peer interfaces are the known neighbors of this node
    peer_interfaces: Arc<Mutex<Vec<Peer>>>,
    pub router_control_tx: UnboundedSender<ControlStruct>,
    pub router_data_tx: UnboundedSender<DataPacket>,
    pub node_tun: Arc<Tun>,
    pub routing_table: Arc<Mutex<RoutingTable>>,
    pub source_table: Arc<Mutex<SourceTable>>,
    pub router_seqno: u16,
}

impl Router {
    pub fn new(node_tun: Arc<Tun>) -> Self {

        // Tx is passed onto each new peer instance. This enables peers to send control packets to the router.
        let (router_control_tx, router_control_rx) = mpsc::unbounded_channel::<ControlStruct>();
        // Tx is passed onto each new peer instance. This enables peers to send data packets to the router.
        let (router_data_tx, router_data_rx) = mpsc::unbounded_channel::<DataPacket>();

        let router = Router {
            router_id: rand::thread_rng().gen(),
            peer_interfaces: Arc::new(Mutex::new(Vec::new())),
            router_control_tx,
            router_data_tx,
            node_tun,
            routing_table: Arc::new(Mutex::new(RoutingTable::new())),
            source_table: Arc::new(Mutex::new(SourceTable::new())),
            router_seqno: 0,
        };

        tokio::spawn(Router::handle_incoming_control_packet(router.clone(), router_control_rx));
        tokio::spawn(Router::handle_incoming_data_packets(router.clone(), router_data_rx));
        tokio::spawn(Router::start_periodic_hello_sender(router.clone()));

        // loops over all peers and adds routing table entries for each peer
        tokio::spawn(Router::initialize_peer_route_entries(router.clone()));


        // propagate routes
        tokio::spawn(Router::propagate_routes(router.clone()));

        router
    }

    async fn handle_incoming_control_packet(self, mut router_control_rx: UnboundedReceiver<ControlStruct>) {
        loop {
            while let Some(control_struct) = router_control_rx.recv().await {
                match control_struct.control_packet.body.tlv_type {
                    BabelTLVType::AckReq => todo!(),
                    BabelTLVType::Ack => todo!(),
                    BabelTLVType::Hello => Self::handle_incoming_hello(control_struct),
                    BabelTLVType::IHU => Self::handle_incoming_ihu(&self, control_struct),
                    BabelTLVType::NextHop => todo!(), 
                    BabelTLVType::Update => {
                        let mut source_table = self.source_table.lock().unwrap();
                        let mut routing_table = self.routing_table.lock().unwrap();
                        Self::handle_incoming_update(self.clone(), &mut source_table, &mut routing_table, control_struct); 
                    },
                    BabelTLVType::RouteReq => todo!(),
                    BabelTLVType::SeqnoReq => todo!(),
                }
            }
        } 
    }

    async fn handle_incoming_data_packets(self, mut router_data_rx: UnboundedReceiver<DataPacket>) {
        // If the destination IP of the data packet matches with the IP address of this node's TUN interface
        // we should forward the data packet towards the TUN interface.
        // If the destination IP doesn't match, we need to lookup if we have a matching peer instance
        // where the destination IP matches with the peer's overlay IP. If we do, we should forward the
        // data packet to the peer's to_peer_data channel.

        let tun_addr = self.node_tun.address().unwrap();
        loop {
            while let Some(data_packet) = router_data_rx.recv().await {
                let dest_ip = data_packet.dest_ip;

                if dest_ip == tun_addr {
                    match self.node_tun.send(&data_packet.raw_data).await {
                        Ok(_) => {},
                        Err(e) => eprintln!("Error sending data packet to TUN interface: {:?}", e),
                    }
                } else {
                    match self.get_peer_by_ip(IpAddr::V4(dest_ip)) {
                        Some(peer) => {
                            match peer.to_peer_data.send(data_packet) {
                                Ok(_) => {},
                                Err(e) => eprintln!("Error sending data packet to peer: {:?}", e),
                            }
                        },
                        None => eprintln!("No matching peer found for data packet"),
                    }
                }
            }
        }
    }


    async fn start_periodic_hello_sender(self) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(HELLO_INTERVAL as u64)).await;
            for peer in self.peer_interfaces.lock().unwrap().iter_mut() {
                // create a new hello packet for this peer
                let hello = ControlPacket::new_hello(peer, HELLO_INTERVAL);
                // set the last hello timestamp for this peer
                peer.time_last_received_hello = tokio::time::Instant::now();
                // send the hello packet to the peer
                println!("Sending hello to peer: {:?}", peer.overlay_ip);
                if let Err(error) = peer.to_peer_control.send(hello) {
                    eprintln!("Error sending hello to peer: {}", error);
                }
            } 
        }
    }

    fn handle_incoming_hello(control_struct: ControlStruct) {
        let destination_ip = control_struct.src_overlay_ip;
        control_struct.reply(ControlPacket::new_ihu(IHU_INTERVAL, destination_ip));
    }

    fn handle_incoming_ihu(&self, control_struct: ControlStruct) {
        let mut source_peer = self.get_source_peer_from_control_struct(control_struct);
        // reset the IHU timer associated with the peer
        source_peer.ihu_timer.reset(tokio::time::Duration::from_secs(IHU_INTERVAL as u64));
        // measure time between Hello and and IHU and set the link cost
        source_peer.link_cost = tokio::time::Instant::now().duration_since(source_peer.time_last_received_hello).as_millis() as u16;
        println!("Link cost for peer {:?} set to {}", source_peer.overlay_ip, source_peer.link_cost);

        println!("IHU timer for peer {:?} reset", source_peer.overlay_ip);
    }

    fn handle_incoming_update(router: Router, source_table: &mut SourceTable, routing_table: &mut RoutingTable, update: ControlStruct) {
        if source_table.is_feasible(&update) {
            println!("incoming update is feasible");
            source_table.update(&update);


            // get routing table entry for the source of the update
            match update.control_packet.body.tlv {
                BabelTLV::Update { plen, interval, seqno, metric, prefix, router_id } => {


                    let source_ip = update.src_overlay_ip;

                    // get RouteEntry for the source of the update
                    if let Some(route_entry) = routing_table.table.get_mut(&RouteKey { prefix, plen, neighbor: source_ip }) {
                        route_entry.update(update);
                    } else {
                        let source_key = SourceKey {
                            prefix,
                            plen, 
                            router_id, 
                        };
                        let feas_dist = FeasibilityDistance(metric, seqno); 
    
                        // create the source table entry
                        source_table.insert(source_key.clone(), feas_dist);
    
                        // now we can create the routing table entry
                        let route_key = RouteKey {
                            prefix, // prefix is peer that ANNOUNCED the route
                            plen, 
                            neighbor: update.src_overlay_ip,
                        };

                        let peer = router.get_peer_by_ip(update.src_overlay_ip).unwrap();

                        let route_entry = RouteEntry {
                            source: source_key,
                            metric: metric, // + peer.link_cost, --> TODO!!!
                            seqno: seqno, 
                            neighbor: peer, 
                            next_hop: update.src_overlay_ip,
                            selected: true, // set selected always to true for now as we have manually decided the topology to only have p2p links
                        };
                        // create the routing table entry
                        routing_table.insert(route_key, route_entry);
                    }
                },
                _ => {},
            }

        } else {
            println!("incoming update is NOT feasible");
            // received update is not feasible
            // unselect to route if it was selected 
        }
    }

    pub fn add_directly_connected_peer(&self, peer: Peer) {
        self.peer_interfaces.lock().unwrap().push(peer);
    }

    pub fn get_node_tun_address(&self) -> Ipv4Addr {
        self.node_tun.address().unwrap()
    }

    pub fn get_peer_interfaces(&self) -> Vec<Peer> {
        self.peer_interfaces.lock().unwrap().clone()
    }

    pub fn get_peer_by_ip(&self, peer_ip: IpAddr) -> Option<Peer> {
        let peers = self.get_peer_interfaces();
        let matching_peer = peers.iter().find(|peer| peer.overlay_ip == peer_ip);

        match matching_peer {
            Some(peer) => Some(peer.clone()),
            None => None,
        }
    }

    fn get_source_peer_from_control_struct(&self, control_struct: ControlStruct) -> Peer {
        let source = control_struct.src_overlay_ip;

        self.get_peer_by_ip(source).unwrap()
    }

    pub fn print_routes(&self) {
        let routing_table = self.routing_table.lock().unwrap();
        for route in routing_table.table.iter() {
            println!("Route: {:?}/{:?} (with next-hop: {:?})", route.0.prefix, route.0.plen, route.1.next_hop);
            println!("As advertised by: {:?}", route.1.source.router_id);
        }
    }

    // loop over the directly connected peers and create routing table entries for them
    // this is done by looking at the currently set link cost. as the cost gets initialized to 65535, we can use this to check if
    // the link cost has been set to a lower value, indicating that the peer is reachable and a routing table entry exits already
    pub async fn initialize_peer_route_entries(self) {
        // we wait for 10 seconds before we start initializing the routing table entries
        // possible optimization: only run this when necassary (e.g. when a new peer is added)
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            for peer in self.peer_interfaces.lock().unwrap().iter_mut() {
                // we check for the u16::MAX - 1 value, as this is the value that the link cost is initialized to
                if peer.link_cost == (u16::MAX - 1) {
                    // before we can create a routing table entry, we need to create a source table entry
                    let source_key = SourceKey {
                        prefix: IpAddr::V4(peer.overlay_ip),
                        plen: 32, // we set the prefix length to 32 for now, this means that each peer is a /32 network (so only route to the peer itself)
                        router_id: 0, // we set all router ids to 0 temporarily
                    };
                    let feas_dist = FeasibilityDistance(peer.link_cost, 0); 

                    // create the source table entry
                    self.source_table
                        .lock()
                        .unwrap()
                        .insert(source_key.clone(), feas_dist);

                    // now we can create the routing table entry
                    let route_key = RouteKey {
                        prefix: IpAddr::V4(peer.overlay_ip),
                        plen: 32, // we set the prefix length to 32 for now, this means that each peer is a /32 network (so only route to the peer itself)
                        neighbor: IpAddr::V4(peer.overlay_ip),
                    };
                    let route_entry = RouteEntry {
                        source: source_key,
                        neighbor: peer.clone(),
                        metric: peer.link_cost,
                        seqno: 0, // we set the seqno to 0 for now
                        next_hop: IpAddr::V4(peer.overlay_ip),
                        selected: true, // set selected always to true for now as we have manually decided the topology to only have p2p links
                    };
                    // create the routing table entry
                    self.routing_table.lock().unwrap().insert(route_key, route_entry);
                }
            }
        }

    }

    // routing table updates are send periodically to all directly connected peers
    // updates are used to advertise new routes or the retract existing routes (retracting when the metric is set to 0xFFFF)
    // this function is run when the route_update timer expires
    pub async fn propagate_routes(self) {
        
        loop {
            // routes are propagated every 10 secs
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

            let router_table = self.routing_table.lock().unwrap();
            let peers = self.peer_interfaces.lock().unwrap();

            for (key, entry) in router_table.table.iter() {
                for peer in peers.iter() {
                    let update = ControlPacket::new_update (
                        key.plen,
                        UPDATE_INTERVAL as u16,
                        entry.seqno,
                        entry.metric,
                        key.prefix,
                        entry.source.router_id,
                    );

                    peer.to_peer_control.send(update).unwrap();
                } 
            }
        }
    }
}


