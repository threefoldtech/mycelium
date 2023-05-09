use crate::{
    packet::{
        BabelPacketBody, BabelPacketHeader, BabelTLV, BabelTLVType, ControlPacket, ControlStruct,
        DataPacket,
    },
    peer::Peer,
    routing_table::{RouteEntry, RouteKey, RoutingTable},
    source_table::{SourceEntry, SourceKey, SourceTable},
};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_tun::Tun;

#[derive(Clone)]
pub struct Router {
    directly_connected_peers: Arc<Mutex<Vec<Peer>>>,
    pub router_control_tx: UnboundedSender<ControlStruct>,
    pub router_data_tx: UnboundedSender<DataPacket>,
    pub node_tun: Arc<Tun>,

    pub sent_hello_timestamps: Arc<Mutex<HashMap<IpAddr, tokio::time::Instant>>>,
    pub routing_table: Arc<Mutex<RoutingTable>>,
    pub source_table: Arc<Mutex<SourceTable>>,
}

impl Router {
    pub fn new(node_tun: Arc<Tun>) -> Self {
        // Tx is passed onto each new peer instance. This enables peers to send control packets to the router.
        let (router_control_tx, router_control_rx) = mpsc::unbounded_channel::<ControlStruct>();
        // Tx is passed onto each new peer instance. This enables peers to send data packets to the router.
        let (router_data_tx, router_data_rx) = mpsc::unbounded_channel::<DataPacket>();

        let router = Router {
            directly_connected_peers: Arc::new(Mutex::new(Vec::new())),
            router_control_tx,
            router_data_tx,
            node_tun,
            sent_hello_timestamps: Arc::new(Mutex::new(HashMap::new())),
            routing_table: Arc::new(Mutex::new(RoutingTable::new())),
            source_table: Arc::new(Mutex::new(SourceTable::new())),
        };

        tokio::spawn(Router::start_hello_sender(
            router.clone(),
            router.sent_hello_timestamps.clone(),
        ));
        tokio::spawn(Router::handle_incoming_control_packets(
            router.clone(),
            router_control_rx,
            router.sent_hello_timestamps.clone(),
        ));
        tokio::spawn(Router::handle_incoming_data_packets(
            router.clone(),
            router_data_rx,
        ));
        tokio::spawn(Router::start_routing_table_updater(router.clone()));

        tokio::spawn(Router::propagate_updates(router.clone()));

        router
    }

    async fn handle_incoming_control_packets(
        self,
        mut router_control_rx: UnboundedReceiver<ControlStruct>,
        sent_hello_timestamps: Arc<Mutex<HashMap<IpAddr, tokio::time::Instant>>>,
    ) {
        loop {
            while let Some(control_struct) = router_control_rx.recv().await {
                if let Some(body) = &control_struct.control_packet.body {
                    match body.tlv_type {
                        BabelTLVType::Hello => {
                            let dest_ip = control_struct.src_overlay_ip;
                            control_struct.reply(ControlPacket::new_ihu(10, 1000, dest_ip));
                            println!("IHU {}", dest_ip);
                        }
                        BabelTLVType::IHU => {
                            // Upon receiving an IHU message, we should read the time difference between the
                            // time the Hello message (with the same seqno) was sent and the time the IHU message
                            // was received. This time difference is the link cost between this node and the peer that
                            // sent the IHU message.
                            if let Some(timestamp) = sent_hello_timestamps
                                .lock()
                                .unwrap()
                                .get(&control_struct.src_overlay_ip)
                            {
                                let time_diff =
                                    tokio::time::Instant::now().duration_since(*timestamp);
                                let sender_peer_ip = control_struct
                                    .src_overlay_ip
                                    .to_string()
                                    .parse::<Ipv4Addr>()
                                    .unwrap();
                                self.update_peer_link_cost(
                                    sender_peer_ip,
                                    time_diff.as_millis() as u16,
                                );
                            } else {
                                eprintln!("No matching Hello message found for received IHU");
                            }
                        }
                        BabelTLVType::Update => {
                            // get the metric from the update message


                            // upon receiving an update message, we should check if the update is feasible
                            // check if there is already a route (based on the index) in the routing table
                            println!("Received update from by boy {}! with metric ", control_struct.src_overlay_ip);
                        }
                        _ => {
                            eprintln!("Unknown control packet type");
                        }
                    }
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
        loop {
            while let Some(data_packet) = router_data_rx.recv().await {
                let dest_ip = data_packet.dest_ip;
                if dest_ip == self.node_tun.address().unwrap() {
                    if let Err(e) = self.node_tun.send(&data_packet.raw_data).await {
                        eprintln!("Error sending data packet to TUN interface: {:?}", e);
                    }
                } else {
                    let matching_peer = self.get_peer_by_ip(dest_ip);
                    if let Some(peer) = matching_peer {
                        if let Err(e) = peer.to_peer_data.send(data_packet) {
                            eprintln!("Error sending data packet to peer: {:?}", e);
                        }
                    } else {
                        eprintln!("No matching peer found for data packet");
                    }
                }
            }
        }
    }

    async fn start_hello_sender(
        self,
        sent_hello_timestamps: Arc<Mutex<HashMap<IpAddr, tokio::time::Instant>>>,
    ) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

            for peer in self.directly_connected_peers.lock().unwrap().iter_mut() {
                let hello_message = ControlPacket {
                    header: BabelPacketHeader::new(7),
                    body: Some(BabelPacketBody {
                        tlv_type: BabelTLVType::Hello,
                        length: 7,
                        body: BabelTLV::Hello {
                            flags: 99u16,
                            seqno: peer.last_sent_hello_seqno,
                            interval: 299u16,
                        },
                    }),
                };
                // Store the current timestamp for this Hello message.
                let current_timestamp = tokio::time::Instant::now();
                sent_hello_timestamps
                    .lock()
                    .unwrap()
                    .insert(IpAddr::V4(peer.overlay_ip), current_timestamp);

                println!("Hello({}) {}", peer.last_sent_hello_seqno, peer.overlay_ip);
                if let Err(e) = peer.to_peer_control.send(hello_message.clone()) {
                    eprintln!("Error sending hello message to peer: {:?}", e);
                }

                peer.increase_hello_seqno();
            }
        }
    }

    // create the function for a task that will loop over the directly connected peers and create routing table entries for them
    // this is done by looking at the currently set link cost. as the cost gets initialized to 65535, we can use this to check if
    // the link cost has been set to a lower value, indicating that the peer is reachable and a routing table entry exits already
    async fn start_routing_table_updater(self) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

            for peer in self.directly_connected_peers.lock().unwrap().iter_mut() {
                if peer.link_cost == 65535 {
                    // before we can create a routing table entry, we need to create a source table entry
                    let source_key = SourceKey {
                        prefix: IpAddr::V4(peer.overlay_ip),
                        plen: 32, // we set the prefix length to 32 for now, this means that each peer is a /32 network (so only route to the peer itself)
                        router_id: 0, // we set all router ids to 0 temporarily
                    };
                    let source_entry = SourceEntry {
                        metric: peer.link_cost,
                        seqno: 0, // we set the seqno to 0 for now
                    };
                    // create the source table entry
                    let source_key_clone = source_key.clone();
                    self.source_table
                        .lock()
                        .unwrap()
                        .insert(source_key, source_entry);

                    // now we can create the routing table entry
                    let route_key = RouteKey {
                        prefix: IpAddr::V4(peer.overlay_ip),
                        plen: 32, // we set the prefix length to 32 for now, this means that each peer is a /32 network (so only route to the peer itself)
                        neighbor: IpAddr::V4(peer.overlay_ip),
                    };
                    let route_entry = RouteEntry {
                        source: source_key_clone,
                        neighbor: peer.clone(),
                        metric: peer.link_cost,
                        seqno: 0, // we set the seqno to 0 for now
                        next_hop: IpAddr::V4(peer.overlay_ip),
                        selected: true, // set selected always to true for now as we have manually decided the topology to only have p2p links
                    };
                    // create the routing table entry
                    self.routing_table
                        .lock()
                        .unwrap()
                        .insert(route_key, route_entry);
                }
            }
        }
    }

    // routing table updates are send periodically to all directly connected peers
    // they can also be send when a change in the network topology occurs
    // updates are used to advertise new routes or the retract existing routes (retracting when the metric is set to 0xFFFF)
    async fn propagate_updates(self) {
        // loop {
        //     tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

        //     let connected_peers = self.directly_connected_peers.lock().unwrap();
        //     let connected_peers_cloned: Vec<&Peer> = connected_peers.iter().collect();

        //     let routing_table = self.routing_table.lock().unwrap();
        //     for (key, value) in routing_table.table.iter() {
        //         // create update message and send to all peers

        //         for peer in &connected_peers_cloned {
        //             let update_message = ControlPacket {
        //                 header: BabelPacketHeader::new(14),
        //                 body: Some(BabelPacketBody {
        //                     tlv_type: BabelTLVType::Update,
        //                     length: 14,
        //                     body: BabelTLV::Update {
        //                         address_encoding: 0, // AE of 1 indicated IPv4
        //                         prefix_length: 32, // temp value - working with full IPv4 for now, not prefix-based
        //                         interval: 999,     // temp value
        //                         seqno: 0,          // temp value
        //                         //metric: value.metric, // this is wrong i think?
        //                         metric: peer.link_cost, 
        //                         prefix: key.prefix,
        //                     },
        //                 }),
        //             };
        //             println!("Sending route as control packet to {}", peer.overlay_ip);
        //             peer.to_peer_control.send(update_message).unwrap();
        //         }
        //     }
        // }
    }

    pub fn get_directly_connected_peers(&self) -> Vec<Peer> {
        self.directly_connected_peers.lock().unwrap().clone()
    }

    pub fn add_directly_connected_peer(&self, peer: Peer) {
        self.directly_connected_peers.lock().unwrap().push(peer);
    }

    fn get_peer_by_ip(&self, peer_ip: Ipv4Addr) -> Option<Peer> {
        let peers = self.get_directly_connected_peers();
        let matching_peer = peers.iter().find(|peer| peer.overlay_ip == peer_ip);

        match matching_peer {
            Some(peer) => Some(peer.clone()),
            None => None,
        }
    }

    pub fn get_node_tun_address(&self) -> Ipv4Addr {
        self.node_tun.address().unwrap()
    }

    pub fn update_peer_link_cost(&self, peer_ip: Ipv4Addr, link_cost: u16) {
        let mut peers = self.directly_connected_peers.lock().unwrap();
        let matching_peer = peers.iter_mut().find(|peer| peer.overlay_ip == peer_ip);

        match matching_peer {
            Some(peer) => {
                // Only update the link cost if the new link cost is lower than the current link cost
                if link_cost < peer.link_cost {
                    peer.link_cost = link_cost;
                    println!("Link cost to {} updated to {}", peer_ip, link_cost);
                }
            }
            None => {
                eprintln!("No matching peer found for link cost update");
            }
        }
    }

    pub fn is_update_feasible(
        // update is tuple of (prefix, prefix_length, router_id, seqno, metric)
        received_update: &(IpAddr, u8, u64, u16, u16),
        source_table: &SourceTable,
    ) -> bool {
        let (prefix, plen, router_id, seqno, metric) = received_update;
        let source_key = SourceKey {
            prefix: *prefix,
            plen: *plen,
            router_id: *router_id,
        };

        // If the received metric is infinite, it's always feasible
        // An update with an infinite metric is considered a retraction
        // Retractions are used to notify neighbors that a previously advertised route is no longer available
        if *metric == u16::MAX {
            return true;
        }

        match source_table.get(&source_key) {
            None => {
                // If there's no entry in the source table, the update is feasible
                true
            }
            Some(source_entry) => {
                // If an entry exists in the source table, compare seqno and metric
                let (seqno_2, metric_2) = (&source_entry.seqno, &source_entry.metric);

                // Check feasibility conditions
                seqno > seqno_2 || (seqno == seqno_2 && metric < metric_2)
            }
        }
    }
}
