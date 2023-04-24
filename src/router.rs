use std::{net::{IpAddr, Ipv4Addr}, sync::{Mutex, Arc}};
use tokio::sync::mpsc::UnboundedSender;
use tokio_tun::Tun;
use crate::{peer::Peer, packet::{ControlPacket, ControlPacketType, DataPacket}};

#[derive(Debug, Clone)]
pub struct Router {
    directly_connected_peers: Arc<Mutex<Vec<Peer>>>,
}

impl Router {
    pub fn new() -> Self {
        Router {
            directly_connected_peers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn add_directly_connected_peer(&self, peer: Peer) {
        self.directly_connected_peers.lock().unwrap().push(peer);
    }

    pub fn send_hello(&self) {
        let hello_message = ControlPacket {
            message_type: ControlPacketType::Hello,
            body_length: 0,
            body: Some(crate::packet::ControlPacketBody::Hello { flags: 100u16, seqno: 200u16, interval: 300u16 }),
        };

        // Send Hello to all directly connected peers 
        for peer in self.directly_connected_peers.lock().unwrap().iter() {
            println!("Hello {}", peer.overlay_ip);
            if let Err(e)  = peer.to_peer_control.send(hello_message.clone()) {
                eprintln!("Error sending hello message to peer: {:?}", e);
            }
        }
    }

    pub fn route_data_packet(&self, data_packet: DataPacket, node_tun: Arc<Tun>, to_tun: UnboundedSender<DataPacket>) {
        let dest_ip = data_packet.dest_ip;
        // If the destination IP of the packets matches this node's overlay IP, we should forward it to the TUN interface.
        // This is done by sending it to the to_tun sender halve of the channel. The receiver halve is read in the main loop
        // and further forwards the packet towards the actual TUN interface.
        if dest_ip == node_tun.address().unwrap() {
            if let Err(e) = to_tun.send(data_packet) {
                eprintln!("Error sendign packet to to_tun: {}", e);
            }
        } else {
            // If the destination IP of the packet does not match this node's overlay IP, we should forward it to the
            // correct peer. This is done by sending the packet to the to_peer_data sender halve of the channel. The receiver 
            // halve is read in the peer select statement and further forwards the packet towards packet over the TCP stream.
            let peer = self.get_peer_from_ip(dest_ip);
            if let Some(peer) = peer {
                if let Err(e) = peer.to_peer_data.send(data_packet) {
                    eprintln!("Error sending packet to peer: {}", e);
                }
            } 
        }
    }

    fn get_peer_from_ip (&self, peer_ip: Ipv4Addr) -> Option<Peer> {
        for peer in self.directly_connected_peers.lock().unwrap().iter() {
            if peer.overlay_ip == peer_ip {
                Some(peer.clone());
            }
        }
        None
    }
}

struct Route {
    prefix: u8,
    plen: u8,
    neighbour: Peer,
}

struct RouteEntry {
    source: (u8, u8, u16), // source (prefix, plen, router-id) for which this route is advertised
    neighbour: Peer, // neighbour that advertised this route
    metric: u16, // metric of this route as advertised by the neighbour 
    seqno: u16, // sequence number of this route as advertised by the neighbour
    next_hop: IpAddr, // next-hop for this route
    selected: bool, // whether this route is selected

    // each route table entry needs a route expiry timer
    // each route has two distinct (seqno, metric) pairs associated with it:
    // 1. (seqno, metric): describes the route's distance
    // 2. (seqno, metric): describes the feasibility distance (should be stored in source table and shared between all routes with the same source)
}