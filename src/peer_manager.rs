use crate::packet::{ControlStruct, DataPacket};
use crate::peer::Peer;
use crate::router::Router;
use log::{debug, error, info};
use serde::Deserialize;
use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{Sender, UnboundedSender};

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
    pub fn new(router: Router, static_peers_sockets: Vec<SocketAddr>, port: u16) -> Self {
        let peer_manager = PeerManager {
            router,
            initial_peers: static_peers_sockets.clone(),
        };
        // Start a TCP listener. When a new connection is accepted, the reverse peer exchange is performed.
        tokio::spawn(PeerManager::start_listener(peer_manager.clone(), port));
        // Reads the nodeconfig.toml file and connects to the peers in the file.
        tokio::spawn(PeerManager::get_peers_from_config(peer_manager.clone()));
        // Remote nodes can also be read from CLI arg
        tokio::spawn(PeerManager::get_peers_from_cli(
            peer_manager.clone(),
            static_peers_sockets,
        ));

        tokio::spawn(PeerManager::reconnect_to_initial_peers(
            peer_manager.clone(),
        ));

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
                            debug!("Invalid address encoding byte");
                            continue;
                        }
                    };

                    let mut buf = [0u8; 17];
                    // only using IPv6
                    buf[0] = 1;
                    buf[1..].copy_from_slice(&self.router.node_tun_addr().octets()[..]);

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
            error!("Error reading nodeconfig.toml file");
        }
    }

    async fn get_peers_from_cli(self, socket_addresses: Vec<SocketAddr>) {
        for peer_addr in socket_addresses {
            if let Ok(mut peer_stream) = TcpStream::connect(peer_addr).await {
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
                        error!("Invalid address encoding byte");
                        continue;
                    }
                };

                let mut buf = [0u8; 17];
                // only using IPv6
                buf[0] = 1;
                buf[1..].copy_from_slice(&self.router.node_tun_addr().octets()[..]);

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
                                error!("Invalid address encoding byte");
                                continue;
                            }
                        };

                        let mut buf = [0u8; 17];
                        // only using IPv6
                        buf[0] = 1;
                        buf[1..].copy_from_slice(&self.router.node_tun_addr().octets()[..]);

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

    async fn start_listener(self, port: u16) {
        let node_tun_addr = self.router.node_tun_addr();
        let router_data_tx = self.router.router_data_tx();
        let router_control_tx = self.router.router_control_tx();

        match TcpListener::bind(("::", port)).await {
            Ok(listener) => loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let new_peer = Self::start_reverse_peer_exchange(
                            stream,
                            node_tun_addr,
                            router_data_tx.clone(),
                            router_control_tx.clone(),
                        )
                        .await;
                        match new_peer {
                            Ok(peer) => {
                                info!("Accepted new peer {}", peer.overlay_ip());
                                self.router.add_peer_interface(peer);
                            }
                            Err(e) => {
                                error!("Failed to accept new peer {e}");
                            }
                        }
                    }
                    Err(e) => {
                        error!("Error accepting connection: {}", e);
                    }
                }
            },
            Err(e) => {
                error!("Error starting listener: {}", e);
            }
        }
    }

    async fn start_reverse_peer_exchange(
        mut stream: TcpStream,
        node_tun_addr: Ipv6Addr,
        router_data_tx: Sender<DataPacket>,
        router_control_tx: UnboundedSender<(ControlStruct, Peer)>,
    ) -> Result<Peer, Box<dyn std::error::Error>> {
        // Steps:
        // 1. Send own TUN address over the stream
        // 2. Read other node's TUN address from the stream

        let mut buf = [0u8; 17];
        // only using IPv6
        buf[0] = 1;
        buf[1..].copy_from_slice(&node_tun_addr.octets()[..]);

        // Step 1
        stream.write_all(&buf).await.unwrap();
        // Step 2
        stream.read_exact(&mut buf).await.unwrap();
        let received_overlay_ip = match buf[0] {
            0 => IpAddr::from(<&[u8] as TryInto<[u8; 4]>>::try_into(&buf[1..5]).unwrap()),
            1 => IpAddr::from(<&[u8] as TryInto<[u8; 16]>>::try_into(&buf[1..]).unwrap()),
            _ => {
                error!("Invalid address encoding byte");
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid encoding byte received",
                )
                .into());
            }
        };

        // Create new Peer instance
        let peer_stream_ip = stream.peer_addr().unwrap().ip();
        Peer::new(
            peer_stream_ip,
            router_data_tx,
            router_control_tx,
            stream,
            received_overlay_ip,
        )
    }
}
