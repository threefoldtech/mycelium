use std::{net::IpAddr, sync::{Mutex, Arc}};

use crate::{peer::Peer, packet::{ControlPacket, ControlPacketType}};

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

    // NOTE: we gebruiken gewoon &self en niet &mut self want de mutex van directly_connected_peers fixt het feit dat er sws maar 1 iemand aankan
    pub fn add_directly_connected_peer(&self, peer: Peer) {
        self.directly_connected_peers.lock().unwrap().push(peer);
        // for debug: print connected peers once one get added
        println!("CURRENT DIRECTLY CONNECTED PEERS:");
        for peer in self.directly_connected_peers.lock().unwrap().iter() {
            println!("Peer: {:?}", peer);
        }
    }

    pub fn send_hello(&self) {
        let hello_message = ControlPacket {
            message_type: ControlPacketType::Hello,
            body_length: 0,
            body: Some(crate::packet::ControlPacketBody::Hello { flags: 100u16, seqno: 200u16, interval: 300u16 }),
        };


        // send the hello_message to all the directly connected peers
        for peer in self.directly_connected_peers.lock().unwrap().iter() {
            println!("CONTROL: Sending hello message to peer: {}", peer.overlay_ip.to_string());
            peer.to_peer_control.send(hello_message.clone());
        }
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