use crate::{packet_control::Packet, peer::Peer};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::net::{SocketAddr, Ipv4Addr};
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

    pub async fn get_peers_from_config(&self, to_tun: UnboundedSender<Packet>, tun_addr_own_node: Ipv4Addr) {
        // Read from the nodeconfig.toml file
        match std::fs::read_to_string(NODE_CONFIG_FILE_PATH) {
            Ok(file_content) => {
                // Create a PeersConfig based on the file content
                let config: PeersConfig = toml::from_str(&file_content).unwrap();
                for peer_addr in config.peers {
                    match TcpStream::connect(peer_addr).await {
                        Ok(mut peer_stream) => {
                            println!("TCP stream connected: {}", peer_addr);

                            
                            // 2. Read other node's TUN address from the stream
                            let mut buffer = [0u8; 4];
                            peer_stream.read_exact(&mut buffer).await.unwrap();
                            let received_overlay_ip = Ipv4Addr::from(buffer);
                            println!("Received overlay IP from other node: {:?}", received_overlay_ip);

                            // 3. Send own TUN address over the stream
                            let ip_bytes = tun_addr_own_node.octets();
                            peer_stream.write_all(&ip_bytes).await.unwrap();


                            // Create peer instance
                            let peer_stream_ip= peer_addr.ip();
                            match Peer::new(peer_stream_ip, to_tun.clone(), peer_stream, received_overlay_ip) {
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

        // extract the IP from the Packet and look in the known_peers which peer ID matches with the destination IP
        let mut known_peers = self.known_peers.lock().unwrap();
        if packet.get_dest_ip().is_some() {
            for peer in known_peers.iter_mut() {
                if peer.overlay_ip == packet.get_dest_ip().unwrap() { // FIX NEEDED --> == is NOT CORRECT!
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
