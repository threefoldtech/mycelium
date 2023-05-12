use crate::peer::Peer;
use crate::router::Router;
use serde::Deserialize;
use tokio::net::TcpListener;
use std::net::{Ipv4Addr, SocketAddr, IpAddr, Ipv6Addr};
use std::sync::{Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::{net::TcpStream};

pub const NODE_CONFIG_FILE_PATH: &str = "nodeconfig.toml";

#[derive(Deserialize)]
struct PeersConfig {
    peers: Vec<SocketAddr>,
}

#[derive(Clone)]
pub struct PeerManager {
    pub router: Arc<Router>,
}

impl PeerManager {

    pub fn new(router: Arc<Router>, static_peers_sockets: Vec<SocketAddr>) -> Self {

        let peer_manager = PeerManager { router };

        // Start a TCP listener. When a new connection is accepted, the reverse peer exchange is performed. 
        tokio::spawn(PeerManager::start_listener(peer_manager.clone()));
        // Reads the nodeconfig.toml file and connects to the peers in the file.
        tokio::spawn(PeerManager::get_peers_from_config(peer_manager.clone()));
        // Remote nodes can also be read from CLI arg
        tokio::spawn(PeerManager::get_peers_from_cli(peer_manager.clone(), static_peers_sockets));

        peer_manager
    }

    async fn get_peers_from_config(self) {
        if let Ok(file_content) = std::fs::read_to_string(NODE_CONFIG_FILE_PATH) {
            let config: PeersConfig = toml::from_str(&file_content).unwrap();

            for peer_addr in config.peers {
                if let Ok(mut peer_stream) = TcpStream::connect(peer_addr).await {
                    let mut buffer = [0u8; 17];
                    peer_stream.read_exact(&mut buffer).await.unwrap();
                    let received_overlay_ip = match buffer[0] {
                        0 => {
                            IpAddr::from(<&[u8] as TryInto<[u8;4]>>::try_into(&buffer[1..5]).unwrap())
                        },
                        1 => {
                            IpAddr::from(<&[u8] as TryInto<[u8;16]>>::try_into(&buffer[1..]).unwrap())
                        }
                        _ => {
                            eprintln!("Invalid address encoding byte");
                            continue
                        }
                    };

                    println!(
                        "Received overlay IP from other node: {:?}",
                        received_overlay_ip
                    );

                    let mut buf = [0u8; 17];
                    match self.router.get_node_tun_address() {
                        IpAddr::V4(tun_addr) => {
                            buf[0] = 0;
                            buf[1..5].copy_from_slice(&tun_addr.octets()[..]);
                        },
                        IpAddr::V6(tun_addr) => {
                            buf[0] = 1;
                            buf[1..].copy_from_slice(&tun_addr.octets()[..]);
                        }
                    }
                    peer_stream.write_all(&buf).await.unwrap();

                    let peer_stream_ip = peer_addr.ip();
                    if let Ok(new_peer) = Peer::new(
                        peer_stream_ip,
                        self.router.router_data_tx.clone(),
                        self.router.router_control_tx.clone(),
                        peer_stream,
                        received_overlay_ip,
                    ) {
                        self.router.add_directly_connected_peer(new_peer);
                    }
                }
            }
        } else {
            eprintln!("Error reading nodeconfig.toml file");
        }
    }

    async fn get_peers_from_cli(self, socket_addresses: Vec<SocketAddr>) {
        for peer_addr in socket_addresses {
            println!("connecting to: {}", peer_addr);

            if let Ok(mut peer_stream) = TcpStream::connect(peer_addr).await {
                println!("stream established");

                let mut buffer = [0u8; 17];
                peer_stream.read_exact(&mut buffer).await.unwrap();
                let received_overlay_ip = match buffer[0] {
                    0 => {
                        IpAddr::from(<&[u8] as TryInto<[u8;4]>>::try_into(&buffer[1..5]).unwrap())
                    },
                    1 => {
                        IpAddr::from(<&[u8] as TryInto<[u8;16]>>::try_into(&buffer[1..]).unwrap())
                    }
                    _ => {
                        eprintln!("Invalid address encoding byte");
                        continue
                    }
                };
                println!(
                    "3: Received overlay IP from other node: {:?}",
                    received_overlay_ip
                );

                let mut buf = [0u8; 17];

                match self.router.get_node_tun_address() {
                    IpAddr::V4(tun_addr) => {
                        buf[0] = 0;
                        buf[1..5].copy_from_slice(&tun_addr.octets()[..]);
                    },
                    IpAddr::V6(tun_addr) => {
                        buf[0] = 1;
                        buf[1..].copy_from_slice(&tun_addr.octets()[..]);
                    }
                }

                peer_stream.write_all(&buf).await.unwrap();

                let peer_stream_ip = peer_addr.ip();
                if let Ok(new_peer) = Peer::new(
                    peer_stream_ip,
                    self.router.router_data_tx.clone(),
                    self.router.router_control_tx.clone(),
                    peer_stream,
                    received_overlay_ip,
                ) {
                    self.router.add_directly_connected_peer(new_peer);
                }
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

        // Steps:
        // 1. Send own TUN address over the stream
        // 2. Read other node's TUN address from the stream


        let mut buf = [0u8; 17];

        match router.get_node_tun_address() {
            IpAddr::V4(tun_addr) => {
                buf[0] = 0;
                buf[1..5].copy_from_slice(&tun_addr.octets()[..]);
            },
            IpAddr::V6(tun_addr) => {
                buf[0] = 1;
                buf[1..].copy_from_slice(&tun_addr.octets()[..]);
            }
        }

        stream.write_all(&buf).await.unwrap();



        stream.read_exact(&mut buf).await.unwrap();
        let received_overlay_ip = match buf[0] {
            0 => {
                IpAddr::from(<&[u8] as TryInto<[u8;4]>>::try_into(&buf[1..5]).unwrap())
            },
            1 => {
                IpAddr::from(<&[u8] as TryInto<[u8;16]>>::try_into(&buf[1..]).unwrap())
            }
            _ => {
                eprintln!("Invalid address encoding byte");
                return; 
            }
        };
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
