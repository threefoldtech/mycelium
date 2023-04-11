use std::collections::HashMap;
use tokio::{sync::mpsc::UnboundedSender, net::TcpStream};
use tokio_util::codec::Framed;

use crate::peer::{Peer, self};
use std::sync::{Arc, Mutex};
use serde::Deserialize;
use std::net::SocketAddr; 

pub const NODE_CONFIG_FILE_PATH: &str = "nodeconfig.toml";


#[derive(Deserialize)]
struct PeersConfig {
    peers: Vec<SocketAddr>,
}

pub struct PeerManager {
    pub known_peers: Mutex<Vec<Peer>>,
}

impl PeerManager {
    pub fn new(myself_peer: Peer) -> Self {
        let mut known_peers: Vec<Peer> = Vec::new();
        // add itself
        known_peers.push(myself_peer);

        Self {
            known_peers: Mutex::new(known_peers),
        }
    }

    pub async fn get_peers_from_config(&self, to_tun: UnboundedSender<Vec<u8>>) {
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
                            let peer_id = peer_addr.to_string();
                            match Peer::new(peer_id, to_tun.clone(), peer_stream) {
                                Ok(new_peer) => {
                                    // Add peer to known_peers
                                    let mut known_peers = self.known_peers.lock().unwrap();
                                    known_peers.push(new_peer);
                                },
                                Err(e) => {
                                    eprintln!("Error creating peer: {}", e);
                                }
                            }
                        },
                        Err(e) => {
                            eprintln!("Error connecting to TCP stream for {}: {}", peer_addr.to_string(), e);
                        }
                    }
                }

            },
            Err(e) => {
                eprintln!("Error reading nodeconfig.toml file: {}", e);
            }
       }
    }
}