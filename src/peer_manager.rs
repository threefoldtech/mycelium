use crate::packet::{DataPacket, ControlPacket};
use crate::{packet::Packet, peer::Peer};
use serde::Deserialize;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::{net::TcpStream, sync::mpsc::UnboundedSender};
use tokio_tun::Tun;
use crate::router::Router;

pub const NODE_CONFIG_FILE_PATH: &str = "nodeconfig.toml";

#[derive(Deserialize)]
struct PeersConfig {
    peers: Vec<SocketAddr>,
}

#[derive(Debug, Clone)]
pub struct PeerManager {
    pub known_peers: Arc<Mutex<Vec<Peer>>>, // --> moet eigenlijk in router
    pub router: Arc<Router>,
}

// PEERMANAGER HEEFT EGIENLIJK ENKEL DE TAAK OM TE CONNECTEN MET PEERS EN ZE VOLLEDIG AAN TE MAKEN
// --> ALS EEN PEER VOLLEDIG IS AANGEMAAKT MOET HIJ NAAR DE ROUTER WORDEN GESTUURD
// --> WANT DE ROUTER MOET UITEINDELIJK EEEN LIJST VAN CONNECTED_PEERS BIJHOUDEN
// --> PEERMANAGER WEL OM DE ZOVEEL TIJD EEN CHECK DOEN ALS DE PEERS NOG LEVEN (MAAR: GEBEURT MSS MET DE HELLO-IHU?)

impl PeerManager {
    pub fn new() -> Self {
        let mut known_peers: Vec<Peer> = Vec::new();

        Self {
            known_peers: Arc::new(Mutex::new(known_peers)),
            router: Arc::new(Router::new()),
        }
    }

    pub async fn get_peers_from_config(
        &self,
        to_routing_data: UnboundedSender<DataPacket>,
        to_routing_control: UnboundedSender<ControlPacket>,
        tun_addr_own_node: Ipv4Addr,
        router: Arc<Router>,
    ) {
        // Read from the nodeconfig.toml file
        match std::fs::read_to_string(NODE_CONFIG_FILE_PATH) {
            Ok(file_content) => {
                // Create a PeersConfig based on the file content
                let config: PeersConfig = toml::from_str(&file_content).unwrap();
                for peer_addr in config.peers {
                    match TcpStream::connect(peer_addr).await {
                        Ok(mut peer_stream) => {
                            //println!("TCP stream connected: {}", peer_addr);

                            // 2. Read other node's TUN address from the stream
                            let mut buffer = [0u8; 4];
                            peer_stream.read_exact(&mut buffer).await.unwrap();
                            let received_overlay_ip = Ipv4Addr::from(buffer);
                            println!(
                                "Received overlay IP from other node: {:?}",
                                received_overlay_ip
                            );

                            // 3. Send own TUN address over the stream
                            let ip_bytes = tun_addr_own_node.octets();
                            peer_stream.write_all(&ip_bytes).await.unwrap();

                            // Create peer instance
                            let peer_stream_ip = peer_addr.ip();
                            match Peer::new(
                                peer_stream_ip,
                                to_routing_data.clone(),
                                to_routing_control.clone(),
                                peer_stream,
                                received_overlay_ip,
                            ) {
                                Ok(new_peer) => {
                                    // Add peer to known_peers
                                    //let mut known_peers = self.known_peers.lock().unwrap();
                                    //known_peers.push(new_peer);
                                    // new
                                    // add new_peer to router directly_connected_peers
                                    router.directly_connected_peers.lock().unwrap().push(new_peer);
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

    // should eventually be moved to router.rs
    // SHOULD ONLY BE CALLED ON DATAPACKETS!!
    //      --> because control packets are only sent between directly connected nodes
    //      --> so they are not routed. If you receive a control packet, you know it's for you
    pub fn route_packet(
        &self,
        data_packet: DataPacket,
        own_node_tun: Arc<Tun>,
        to_tun_sender: UnboundedSender<DataPacket>,
    ) {
        // We first extract the IP from the Packet and look if the destination IP is our own overlay IP
        // So if --> forward packet to our own TUN interface
        // If not --> look in known_peers which peer's overlay_ip matches with destination IP

        let packet_dest_ip = data_packet.get_dest_ip();

        // Packet towards own node's TUN interface
        if packet_dest_ip == own_node_tun.address().unwrap() {
            println!("Packet got address of our own TUN --> so sending it to my own TUN");
            to_tun_sender.send(data_packet);
        // Packet towards other peer
        } else {
            let mut known_peers = self.known_peers.lock().unwrap();
            for peer in known_peers.iter_mut() {
                if peer.overlay_ip == packet_dest_ip {
                    println!("Routing packet towards: {}", peer.overlay_ip.to_string());
                    peer.to_peer_data.send(data_packet);
                    break;
                } else {
                    println!("No peer match found");
                }
            }
        }
    }
}
