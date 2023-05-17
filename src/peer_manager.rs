use crate::peer::Peer;
use crate::router::Router;
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::net::TcpStream;

pub const NODE_CONFIG_FILE_PATH: &str = "nodeconfig.toml";

#[derive(Deserialize)]
struct PeersConfig {
    peers: Vec<SocketAddr>,
}

#[derive(Clone)]
pub struct PeerManager {
    pub router: Router,
    pub initial_peers: Vec<SocketAddr>,
}

impl PeerManager {
    pub fn new(router: Router, static_peers_sockets: Vec<SocketAddr>) -> Self {
        let peer_manager = PeerManager { 
                router,
                initial_peers: static_peers_sockets.clone(), 
            };
        // Start a TCP listener. When a new connection is accepted, the reverse peer exchange is performed.
        tokio::spawn(PeerManager::start_listener(peer_manager.clone()));
        // Reads the nodeconfig.toml file and connects to the peers in the file.
        tokio::spawn(PeerManager::get_peers_from_config(peer_manager.clone()));
        // Remote nodes can also be read from CLI arg
        tokio::spawn(PeerManager::get_peers_from_cli(
            peer_manager.clone(),
            static_peers_sockets,
        ));

        tokio::spawn(PeerManager::reconnect_to_initial_peers(peer_manager.clone()));

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
                        0 => IpAddr::from(
                            <&[u8] as TryInto<[u8; 4]>>::try_into(&buffer[1..5]).unwrap(),
                        ),
                        1 => IpAddr::from(
                            <&[u8] as TryInto<[u8; 16]>>::try_into(&buffer[1..]).unwrap(),
                        ),
                        _ => {
                            eprintln!("Invalid address encoding byte");
                            continue;
                        }
                    };

                    println!(
                        "Received overlay IP from other node: {:?}",
                        received_overlay_ip
                    );

                    let mut buf = [0u8; 17];
                    match self.router.node_tun_addr() {
                        IpAddr::V4(tun_addr) => {
                            buf[0] = 0;
                            buf[1..5].copy_from_slice(&tun_addr.octets()[..]);
                        }
                        IpAddr::V6(tun_addr) => {
                            buf[0] = 1;
                            buf[1..].copy_from_slice(&tun_addr.octets()[..]);
                        }
                    }
                    peer_stream.write_all(&buf).await.unwrap();

                    let peer_stream_ip = peer_addr.ip();
                    if let Ok(new_peer) = Peer::new(
                        peer_stream_ip,
                        self.router.router_data_tx(),
                        self.router.router_control_tx(),
                        peer_stream,
                        received_overlay_ip,
                    ) {
                        self.router.add_peer_interface(new_peer);
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
                        IpAddr::from(<&[u8] as TryInto<[u8; 4]>>::try_into(&buffer[1..5]).unwrap())
                    }
                    1 => {
                        IpAddr::from(<&[u8] as TryInto<[u8; 16]>>::try_into(&buffer[1..]).unwrap())
                    }
                    _ => {
                        eprintln!("Invalid address encoding byte");
                        continue;
                    }
                };
                println!(
                    "3: Received overlay IP from other node: {:?}",
                    received_overlay_ip
                );

                let mut buf = [0u8; 17];

                match self.router.node_tun_addr() {
                    IpAddr::V4(tun_addr) => {
                        buf[0] = 0;
                        buf[1..5].copy_from_slice(&tun_addr.octets()[..]);
                    }
                    IpAddr::V6(tun_addr) => {
                        buf[0] = 1;
                        buf[1..].copy_from_slice(&tun_addr.octets()[..]);
                    }
                }

                peer_stream.write_all(&buf).await.unwrap();

                let peer_stream_ip = peer_addr.ip();
                if let Ok(new_peer) = Peer::new(
                    peer_stream_ip,
                    self.router.router_data_tx(),
                    self.router.router_control_tx(),
                    peer_stream,
                    received_overlay_ip,
                ) {
                    self.router.add_peer_interface(new_peer);
                }
            }
        }
    }

    // this is used to reconnect to the provided static peers in case the connection is lost
    async fn reconnect_to_initial_peers(self) {

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

            // check if there is an entry for the peer in the router's peer list
            for peer in self.initial_peers.iter() {
                if !self.router.peer_exists(peer.ip()) {
                    if let Ok(mut peer_stream) = TcpStream::connect(peer).await {
                        let mut buffer = [0u8; 17];
                        peer_stream.read_exact(&mut buffer).await.unwrap();
                        let received_overlay_ip = match buffer[0] {
                            0 => IpAddr::from(
                                <&[u8] as TryInto<[u8; 4]>>::try_into(&buffer[1..5]).unwrap(),
                            ),
                            1 => IpAddr::from(
                                <&[u8] as TryInto<[u8; 16]>>::try_into(&buffer[1..]).unwrap(),
                            ),
                            _ => {
                                eprintln!("Invalid address encoding byte");
                                continue;
                            }
                        };

                        println!(
                            "Received overlay IP from other node: {:?}",
                            received_overlay_ip
                        );

                        let mut buf = [0u8; 17];
                        match self.router.node_tun_addr() {
                            IpAddr::V4(tun_addr) => {
                                buf[0] = 0;
                                buf[1..5].copy_from_slice(&tun_addr.octets()[..]);
                            }
                            IpAddr::V6(tun_addr) => {
                                buf[0] = 1;
                                buf[1..].copy_from_slice(&tun_addr.octets()[..]);
                            }
                        }
                        peer_stream.write_all(&buf).await.unwrap();

                        let peer_stream_ip = peer.ip();
                        if let Ok(new_peer) = Peer::new(
                            peer_stream_ip,
                            self.router.router_data_tx(),
                            self.router.router_control_tx(),
                            peer_stream,
                            received_overlay_ip,
                        ) {
                            self.router.add_peer_interface(new_peer);
                        }
                    }
                }
            } 
        }
        
    }

    async fn start_listener(self) {
        match TcpListener::bind("[::]:9651").await {
            Ok(listener) => loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        PeerManager::start_reverse_peer_exchange(stream, self.router.clone()).await;
                    }
                    Err(e) => {
                        eprintln!("Error accepting connection: {}", e);
                    }
                }
            },
            Err(e) => {
                eprintln!("Error starting listener: {}", e);
            }
        }
    }

    async fn start_reverse_peer_exchange(mut stream: TcpStream, router: Router) {
        // Steps:
        // 1. Send own TUN address over the stream
        // 2. Read other node's TUN address from the stream

        let mut buf = [0u8; 17];

        match router.node_tun_addr() {
            IpAddr::V4(tun_addr) => {
                buf[0] = 0;
                buf[1..5].copy_from_slice(&tun_addr.octets()[..]);
            }
            IpAddr::V6(tun_addr) => {
                buf[0] = 1;
                buf[1..].copy_from_slice(&tun_addr.octets()[..]);
            }
        }

        stream.write_all(&buf).await.unwrap();

        stream.read_exact(&mut buf).await.unwrap();
        let received_overlay_ip = match buf[0] {
            0 => IpAddr::from(<&[u8] as TryInto<[u8; 4]>>::try_into(&buf[1..5]).unwrap()),
            1 => IpAddr::from(<&[u8] as TryInto<[u8; 16]>>::try_into(&buf[1..]).unwrap()),
            _ => {
                eprintln!("Invalid address encoding byte");
                return;
            }
        };
        println!(
            "Received overlay IP from other node: {:?}",
            received_overlay_ip
        );

        // Create new Peer instance
        let peer_stream_ip = stream.peer_addr().unwrap().ip();
        let new_peer = Peer::new(
            peer_stream_ip,
            router.router_data_tx(),
            router.router_control_tx(),
            stream,
            received_overlay_ip,
        );
        match new_peer {
            Ok(new_peer) => {
                router.add_peer_interface(new_peer);
            }
            Err(e) => {
                eprintln!("Error creating peer: {}", e);
            }
        }
    }

}
