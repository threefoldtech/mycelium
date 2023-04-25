use crate::peer::Peer;
use crate::router::Router;
use serde::Deserialize;
use tokio::net::TcpListener;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::{net::TcpStream};

pub const NODE_CONFIG_FILE_PATH: &str = "nodeconfig.toml";

#[derive(Deserialize)]
struct PeersConfig {
    peers: Vec<SocketAddr>,
}

#[derive(Clone)]
pub struct PeerManager {
    pub known_peers: Arc<Mutex<Vec<Peer>>>, // --> moet eigenlijk in router
    pub router: Arc<Router>,
}

impl PeerManager {

    pub fn new(router: Arc<Router>) -> Self {
        let known_peers: Vec<Peer> = Vec::new();

        let peer_manager = PeerManager {
            known_peers: Arc::new(Mutex::new(known_peers)),
            router,
        };

        // Start a TCP listener. When a new connection is accepted, the reverse peer exchange is performed. 
        tokio::spawn(PeerManager::start_listener(peer_manager.clone()));
        // Reads the nodeconfig.toml file and connects to the peers in the file.
        tokio::spawn(PeerManager::get_peers_from_config(peer_manager.clone()));

        peer_manager
    }

    // Each node has a nodeconfig.toml file which contains the underlay socket addresses of other nodes.
    async fn get_peers_from_config(self) {

        // Read from the nodeconfig.toml file
        match std::fs::read_to_string(NODE_CONFIG_FILE_PATH) {
            Ok(file_content) => {
                // Create a PeersConfig based on the file content
                let config: PeersConfig = toml::from_str(&file_content).unwrap();
                for peer_addr in config.peers {
                    match TcpStream::connect(peer_addr).await {
                        Ok(mut peer_stream) => {

                            // 2. Read other node's TUN address from the stream
                            let mut buffer = [0u8; 4];
                            peer_stream.read_exact(&mut buffer).await.unwrap();
                            let received_overlay_ip = Ipv4Addr::from(buffer);
                            println!(
                                "Received overlay IP from other node: {:?}",
                                received_overlay_ip
                            );

                            // 3. Send own TUN address over the stream
                            let ip_bytes = self.router.get_node_tun_address().octets();
                            peer_stream.write_all(&ip_bytes).await.unwrap();

                            // Create peer instance
                            let peer_stream_ip = peer_addr.ip();
                            match Peer::new(
                                peer_stream_ip,
                                self.router.router_data_tx.clone(),
                                self.router.router_control_tx.clone(),
                                peer_stream,
                                received_overlay_ip,
                            ) {
                                Ok(new_peer) => {
                                    self.router.add_directly_connected_peer(new_peer);
                                }
                                Err(e) => {
                                    eprintln!("Error creating peer: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Error connecting to TCP stream for {}: {}", peer_addr.to_string(), e);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Error reading nodeconfig.toml file: {}", e);
            }
        }
    }

    async fn start_listener(self) {
        match TcpListener::bind("[::]:9651").await {
            Ok(listener) => {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            PeerManager::start_reverse_peer_exchange(stream, self.router.clone()).await;
                        }
                        Err(e) => {
                            eprintln!("Error accepting connection: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Error starting listener: {}", e);
            }
        }
    } 

    async fn start_reverse_peer_exchange(mut stream: TcpStream, router: Arc<Router>) {

        // 1. Send own TUN address over the stream
        let ip_bytes = router.get_node_tun_address().octets();
        stream.write_all(&ip_bytes).await.unwrap();

        // 2. Read other node's TUN address from the stream
        let mut buffer = [0u8; 4];
        stream.read_exact(&mut buffer).await.unwrap();
        let received_overlay_ip = Ipv4Addr::from(buffer);
        println!("Received overlay IP from other node: {:?}", received_overlay_ip);

        // Create new Peer instance
        let peer_stream_ip = stream.peer_addr().unwrap().ip();
        let new_peer = Peer::new(
            peer_stream_ip,
            router.router_data_tx.clone(),
            router.router_control_tx.clone(),
            stream,
            received_overlay_ip,
        );
        match new_peer {
            Ok(new_peer) => {
                router.add_directly_connected_peer(new_peer);
            }
            Err(e) => {
                eprintln!("Error creating peer: {}", e);
            }
        }
    }
}
