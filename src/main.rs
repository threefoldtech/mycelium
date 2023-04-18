use std::{
    error::Error,
    net::{IpAddr, Ipv4Addr},
};

use bytes::BytesMut;
use clap::Parser;
use etherparse::{IpHeader, PacketHeaders};
use packet_control::{DataPacket, Packet, PacketCodec};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

mod node_setup;
mod packet_control;
mod peer;
mod peer_manager;
mod routing;

use peer::Peer;
use peer_manager::PeerManager;
use tokio::io::AsyncWriteExt;
use tokio_util::codec::{Decoder, Encoder};

const LINK_MTU: usize = 1500;

#[derive(Parser)]
struct Cli {
    #[arg(short = 'a', long = "tun-addr")]
    tun_addr: Ipv4Addr,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    // Create TUN interface and add static route
    let node_tun = match node_setup::setup_node(cli.tun_addr).await {
        Ok(tun) => {
            println!("Node setup complete");
            tun
        }
        Err(e) => {
            panic!("Error setting up node: {}", e);
        }
    };

    // Create an unbounded channel for this node
    let (to_tun, mut from_routing) = mpsc::unbounded_channel::<Packet>();
    let (to_routing, mut from_node) = mpsc::unbounded_channel::<Packet>();

    // Create the PeerManager: an interface to all peers this node is connected to
    // Additional static peers are obtained through the nodeconfig.toml file
    let peer_manager = PeerManager::new();

    // Create static peers from the nodeconfig.toml file
    let peer_man_clone = peer_manager.clone();
    let to_routing_clone = to_routing.clone();
    tokio::spawn(async move {
        peer_man_clone.get_peers_from_config(to_routing_clone).await; // --> here we create peer by TcpStream connect
    });

    let peer_man_clone = peer_manager.clone();
    let to_routing_clone = to_routing.clone();
    // listen for inbound request --> "to created the reverse peer object" --> here we reverse create peer be listener.accept'ing
    tokio::spawn(async move {
        match TcpListener::bind("[::]:9651").await {
            Ok(listener) => {
                // loop to accept the inbound requests
                loop {
                    let to_routing_clone_clone = to_routing_clone.clone();
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            println!(
                                "Got inbound request from: {}",
                                stream.peer_addr().unwrap().to_string()
                            );
                            // "reverse peer add"
                            let peer_overlay_ip= stream.peer_addr().unwrap().ip();
                            match Peer::new(peer_overlay_ip, to_routing_clone_clone, stream) {
                                Ok(new_peer) => {
                                    println!("adding new peer to known_peers: {:?}", new_peer);
                                    peer_man_clone.known_peers.lock().unwrap().push(new_peer);
                                }
                                Err(e) => {
                                    eprintln!("Error creating 'reverse' peer: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Error accepting TCP listener: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Error binding TCP listener: {}", e);
            }
        }
    });

    // tokio::spawn(async move {
    //     // loop to read from_peers and send to to_peer or TUN interface
    //     loop {
    //         while let Some(packet) = from_peers.recv().await {
    //             // packet for me: send to TUN

    //             // Er klopt toch iets niet? je lees van from_peers en zou sturen naar TUN maar loop hieronder leest al from routing en stuur naar TUN
    //             // ----> MOET HET NIET NAAR to_tun zijn in de plaats?

    //             // TODO: packet for other peer: send to to_peer
    //         }
    //     }
    // });

    // Loop to read the 'from_routing' receiver and foward it toward the TUN interface
    let node_tun_clone = node_tun.clone();
    tokio::spawn(async move {
        let mut packet_codec = PacketCodec::new();
        loop {
            while let Some(packet) = from_routing.recv().await {
                let mut packet_bytes = BytesMut::new();
                packet_codec.encode(packet, &mut packet_bytes);
                match node_tun_clone.send(&packet_bytes).await {
                    Ok(_) => {
                        //println!("Received from 'from_routing': {:?}", packet);
                    }
                    Err(e) => {
                        eprintln!("Error sending to TUN interface: {}", e);
                    }
                }
            }
        }
    });

    // Loop to read from node's TUN interface and send it to to_routing sender halve
    let node_tun_clone = node_tun.clone();
    let to_routing_clone = to_routing.clone();
    tokio::spawn(async move {
        let mut buf = BytesMut::zeroed(LINK_MTU);
        loop {
            match node_tun_clone.recv(&mut buf).await {
                Ok(n) => {
                    buf.truncate(n);

                    // Remainder: if we read from TUN we will only need to parse them into DataPackets 

                    // Extract the destination IP address
                    let dest_ip = match PacketHeaders::from_ip_slice(&buf) {
                        Ok(PacketHeaders {
                            ip: Some(ip_header),
                            ..
                        }) => match ip_header {
                            IpHeader::Version4(ipv4_header, _) => {
                                Some(std::net::IpAddr::V4(ipv4_header.destination.into()))
                            }
                            IpHeader::Version6(ipv6_header, _) => {
                                Some(std::net::IpAddr::V6(ipv6_header.destination.into()))
                            }
                        },
                        _ => None,
                    };

                    // Create a DataPacket and set it to to_routing
                    let data_packet = DataPacket {
                        raw_data: buf.to_vec(),
                        dest_ip,
                    };
                    to_routing_clone.send(Packet::DataPacket(data_packet));
                }
                Err(e) => {
                    eprintln!("Error reading from TUN: {}", e);
                }
            }
        }
    });

    let peer_man_clone = peer_manager.clone();
    tokio::spawn(async move {
        loop {
            while let Some(packet) = from_node.recv().await {
                println!("Read message from from_node, sending it to route_packet function");
                peer_man_clone.route_packet(packet);         
            }
        }
    });

    tokio::time::sleep(std::time::Duration::from_secs(60 * 60 * 24)).await;
    Ok(())
}
