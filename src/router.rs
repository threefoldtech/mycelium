use crate::{
    packet::{
        BabelPacketBody, BabelPacketHeader, BabelTLV, BabelTLVType, ControlPacket, ControlStruct,
        DataPacket,
    },
    peer::Peer,
};
use std::{
    net::{Ipv4Addr, IpAddr},
    sync::{Arc, Mutex}, collections::HashMap,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_tun::Tun;

#[derive(Clone)]
pub struct Router {
    directly_connected_peers: Arc<Mutex<Vec<Peer>>>,
    pub router_control_tx: UnboundedSender<ControlStruct>,
    pub router_data_tx: UnboundedSender<DataPacket>,
    pub node_tun: Arc<Tun>,

    pub sent_hello_timestamps: Arc<Mutex<HashMap<IpAddr, tokio::time::Instant>>>
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
        };

        tokio::spawn(Router::start_hello_sender(router.clone(), router.sent_hello_timestamps.clone()));
        tokio::spawn(Router::handle_incoming_control_packets(router_control_rx, router.sent_hello_timestamps.clone()));
        tokio::spawn(Router::handle_incoming_data_packets(
            router.clone(),
            router_data_rx,
        ));

        router
    }

    async fn handle_incoming_control_packets(
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
                            if let Some(timestamp) = sent_hello_timestamps.lock().unwrap().get(&control_struct.src_overlay_ip) {
                                let time_diff = tokio::time::Instant::now().duration_since(*timestamp);
                                println!("Time difference (ms): {}", time_diff.as_millis());
                            } else {
                                eprintln!("No matching Hello message found for received IHU");
                            }
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

    async fn start_hello_sender(self, sent_hello_timestamps: Arc<Mutex<HashMap<IpAddr, tokio::time::Instant>>>) {
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
                sent_hello_timestamps.lock().unwrap().insert(IpAddr::V4(peer.overlay_ip), current_timestamp);

                println!("Hello({}) {}", peer.last_sent_hello_seqno, peer.overlay_ip);
                if let Err(e) = peer.to_peer_control.send(hello_message.clone()) {
                    eprintln!("Error sending hello message to peer: {:?}", e);
                }

                peer.increase_hello_seqno();
            }
        }
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
}

// struct Route {
//     prefix: u8,
//     plen: u8,
//     neighbour: Peer,
// }

// struct RouteEntry {
//     source: (u8, u8, u16), // source (prefix, plen, router-id) for which this route is advertised
//     neighbour: Peer, // neighbour that advertised this route
//     metric: u16, // metric of this route as advertised by the neighbour
//     seqno: u16, // sequence number of this route as advertised by the neighbour
//     next_hop: IpAddr, // next-hop for this route
//     selected: bool, // whether this route is selected

//     // each route table entry needs a route expiry timer
//     // each route has two distinct (seqno, metric) pairs associated with it:
//     // 1. (seqno, metric): describes the route's distance
//     // 2. (seqno, metric): describes the feasibility distance (should be stored in source table and shared between all routes with the same source)
// }
