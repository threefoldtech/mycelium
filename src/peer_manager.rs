use crate::{packet_control::Packet, peer::Peer};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::{net::TcpStream, sync::mpsc::UnboundedSender};

pub const NODE_CONFIG_FILE_PATH: &str = "nodeconfig.toml";

#[derive(Deserialize)]
struct PeersConfig {
    peers: Vec<SocketAddr>,
}

#[derive(Debug, Clone)]
pub struct PeerManager {
    pub known_peers: Arc<Mutex<Vec<Peer>>>,
}

impl PeerManager {
    pub fn new() -> Self {
        let mut known_peers: Vec<Peer> = Vec::new();

        Self {
            known_peers: Arc::new(Mutex::new(known_peers)),
        }
    }

    pub async fn get_peers_from_config(&self, to_tun: UnboundedSender<Packet>) {
        // Read from the nodeconfig.toml file
        match std::fs::read_to_string(NODE_CONFIG_FILE_PATH) {
            Ok(file_content) => {
                // Create a PeersConfig based on the file content
                let config: PeersConfig = toml::from_str(&file_content).unwrap();
                for peer_addr in config.peers {
                    match TcpStream::connect(peer_addr).await {
                        Ok(peer_stream) => {
                            println!("TCP stream connected: {}", peer_addr);
                            // Create peer instance
                            let peer_overlay_ip = peer_addr.ip();
                            match Peer::new(peer_overlay_ip, to_tun.clone(), peer_stream) {
                                Ok(new_peer) => {
                                    // Add peer to known_peers
                                    let mut known_peers = self.known_peers.lock().unwrap();
                                    known_peers.push(new_peer);
                                }
                                Err(e) => {
                                    eprintln!("Error creating peer: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "Error connecting to TCP stream for {}: {}",
                                peer_addr.to_string(),
                                e
                            );
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Error reading nodeconfig.toml file: {}", e);
            }
        }
    }

    pub fn route_packet(&self, packet: Packet) {

        // TESTING: print all peer_overlay_ip we got in known_peers
        println!("PRINTING ALL CURRENT KNOWN PEERS");
        let known_peers = self.known_peers.lock().unwrap();
        for peer in known_peers.iter() {
            println!("Peer: {}", peer.overlay_ip);
        }
        println!("PACKET DESTINATION IP: {}", packet.get_dest_ip().unwrap());

        // extract the IP from the Packet and look in the known_peers which peer ID matches with the destination IP
        if packet.get_dest_ip().is_some() {
            let mut known_peers = self.known_peers.lock().unwrap();
            for peer in known_peers.iter_mut() {
                if peer.overlay_ip == packet.get_dest_ip().unwrap() {
                    println!("Found matching peer! {}", peer.overlay_ip);
                    peer.to_peer.send(packet);
                    break;
                } else {
                    println!("No peer with matches, continue searching...");
                }
            }
        } else {
            eprintln!("Cannot route packet as we have not destination IP set in the packet");
        }
    }
}
