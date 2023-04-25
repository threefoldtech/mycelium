use std::{net::{IpAddr, Ipv4Addr}, sync::{Mutex, Arc}};
use tokio::sync::mpsc::{UnboundedSender, UnboundedReceiver, self};
use tokio_tun::Tun;
use crate::{peer::Peer, packet::{ControlPacket, ControlPacketType, DataPacket, ControlStruct}};

#[derive(Clone)]
pub struct Router {
    directly_connected_peers: Arc<Mutex<Vec<Peer>>>,
    pub router_control_tx: UnboundedSender<ControlStruct>,
    pub router_data_tx: UnboundedSender<DataPacket>,
    pub node_tun: Arc<Tun>,
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
        };

        tokio::spawn(Router::start_hello_sender(router.clone()));
        tokio::spawn(Router::handle_incoming_control_packets(router_control_rx));
        tokio::spawn(Router::handle_incoming_data_packets(router.clone(), router_data_rx));

        router
    }

    async fn handle_incoming_control_packets(mut router_control_rx: UnboundedReceiver<ControlStruct>) {
        loop {
            while let Some(control_struct) = router_control_rx.recv().await {
                match control_struct.control_packet.message_type {
                    ControlPacketType::Hello => {
                        let dest_ip = control_struct.src_overlay_ip;
                        control_struct.reply(ControlPacket::new_ihu(10, 1000, dest_ip));
                        println!("IHU {}", dest_ip);
                    },
                    ControlPacketType::IHU => {
                        // Upon receiving an IHU, nothing parrticular needs to be done.
                    },
                    _ => {
                        eprintln!("Unknown control packet type");
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
                }
                else {
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

    async fn start_hello_sender(self) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            let hello_message = ControlPacket {
                message_type: ControlPacketType::Hello,
                body_length: 0,
                body: Some(crate::packet::ControlPacketBody::Hello { flags: 100u16, seqno: 200u16, interval: 300u16 }),
            };
            for peer in self.get_directly_connected_peers() {
                println!("Hello {}", peer.overlay_ip);
                if let Err(e)  = peer.to_peer_control.send(hello_message.clone()) {
                    eprintln!("Error sending hello message to peer: {:?}", e);
                }
            }
        }
    }

    pub fn get_directly_connected_peers(&self) -> Vec<Peer> {
        self.directly_connected_peers.lock().unwrap().clone()
    }

    pub fn add_directly_connected_peer(&self, peer: Peer) {
        self.directly_connected_peers.lock().unwrap().push(peer);
    }

    fn get_peer_by_ip (&self, peer_ip: Ipv4Addr) -> Option<Peer> {
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