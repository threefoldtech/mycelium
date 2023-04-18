use std::{
    error::Error,
    net::{Ipv4Addr},
};

use bytes::BytesMut;
use clap::Parser;
use etherparse::{IpHeader, PacketHeaders, SlicedPacket, InternetSlice};
use packet_control::{DataPacket, Packet, PacketCodec};
use tokio::{net::{TcpListener}, sync::mpsc, io::AsyncReadExt};

mod node_setup;
mod packet_control;
mod peer;
mod peer_manager;
mod routing;

use peer::Peer;
use peer_manager::PeerManager;
use tokio::io::AsyncWriteExt;
use tokio_util::codec::{Encoder};

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
        peer_man_clone.get_peers_from_config(to_routing_clone, cli.tun_addr).await; // --> here we create peer by TcpStream connect
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
                        Ok((mut stream, _)) => {

                            // TEMPORARY: as we do not work with Babel yet, we will send to overlay ip (= addr of TUN) manually
                            // The packet flow looks like this:
                            // Listener accepts a TCP connect call here and send it's overlay IP over the stream
                            // In the peer_manager.rs at the place where we are connected we should manually add the overlay IP to the peer instance
                            
                            // 1. Send own TUN address over the stream
                            let ip_bytes = cli.tun_addr.octets();
                            stream.write_all(&ip_bytes).await.unwrap();

                            // 4. Read other node's TUN address from the stream
                            let mut buffer = [0u8; 4];
                            stream.read_exact(&mut buffer).await.unwrap();
                            let received_overlay_ip = Ipv4Addr::from(buffer);
                            println!("Received overlay IP from other node: {:?}", received_overlay_ip);


                            // "reverse peer add"
                            let peer_stream_ip= stream.peer_addr().unwrap().ip();
                            match Peer::new(peer_stream_ip, to_routing_clone_clone, stream, received_overlay_ip) {
                                Ok(new_peer) => {
                                    //println!("adding new peer to known_peers: {:?}", new_peer);
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

    // Loop to read the 'from_routing' receiver and foward it toward the TUN interface
    // TODO: you will only get DataPackets on TUN so the channel should only accept DataPackets (and not just Packet)
    let node_tun_clone = node_tun.clone();
    tokio::spawn(async move {
        loop {
            while let Some(packet) = from_routing.recv().await {
                let data_packet = if let Packet::DataPacket(p) = packet{
                    p
                } else {
                    continue;
                };
                match node_tun_clone.send(&data_packet.raw_data).await {
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

                    println!("Got packet on my TUN");

                    // Remainder: if we read from TUN we will only need to parse them into DataPackets 

                    // Extract the destination IP address
                    let mut dest_ip: Option<Ipv4Addr> = None;
                    match SlicedPacket::from_ip(&buf) {
                        Ok(sliced_packet) => {
                            match sliced_packet.ip {
                                Some(ip) => {
                                    match ip {
                                        InternetSlice::Ipv4(header_slice, _) => {
                                            dest_ip = Some(header_slice.destination_addr());
                                            println!("Got destination IP addr: {}", dest_ip.unwrap());
                                        },
                                        InternetSlice::Ipv6(_, _) => {}
                                    }
                                }, None => {
                                    eprintln!("Error obtaining IP from sliced packet");
                                }
                            }
                        }, Err(e) => {
                            eprintln!("Error getting IP: {}", e);
                        }
                    } 

                    // Create a DataPacket and set it to to_routing
                    let data_packet = DataPacket {
                        raw_data: buf.to_vec(),
                        dest_ip: dest_ip.unwrap(),
                    };
                    to_routing_clone.send(Packet::DataPacket(data_packet));
                }
                Err(e) => {
                    eprintln!("Error reading from TUN: {}", e);
                }
            }
        }
    });

    // Loop to read from from_node reeiver and route the packet further
    // the route_packet function will send the packet towards the correct to_peer (based on dest ip of packet)
    // or towards this own node's TUN interface (if dest ip of packet is this node's TUN addr)
    let peer_man_clone = peer_manager.clone();
    let node_tun_clone = node_tun.clone();
    let to_tun_sender_clone = to_tun.clone();
    tokio::spawn(async move {
        loop {
            let node_tun_inner_clone = node_tun_clone.clone();
            let to_tun_sender_inner_clone = to_tun_sender_clone.clone();
            while let Some(packet) = from_node.recv().await {
                //println!("Read message from from_node, sending it to route_packet function");
                peer_man_clone.route_packet(packet, node_tun_inner_clone.clone(), to_tun_sender_inner_clone.clone());
            }
        }
    });
    

    tokio::time::sleep(std::time::Duration::from_secs(60 * 60 * 24)).await;
    Ok(())
}
