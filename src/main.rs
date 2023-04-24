use std::{error::Error, net::Ipv4Addr, sync::Arc};
use bytes::BytesMut;
use clap::Parser;
use etherparse::{IpHeader, PacketHeaders};
use packet::{DataPacket, ControlPacket, ControlPacketType};
use tokio::{io::AsyncReadExt, io::AsyncWriteExt, net::TcpListener, sync::mpsc};

mod node_setup;
mod packet;
mod codec;
mod peer;
mod peer_manager;
mod router;

use peer::Peer;
use peer_manager::PeerManager;

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
    

    // We will split the communication of DataPackets and ControlPacket into two different channels
    // --> this is because we will handle them differently 

    // CHANNEL FOR CONTROL PACKETS
    let (to_routing_control, mut from_node_control) = mpsc::unbounded_channel::<ControlPacket>();

    /* BABEL ADDITIONS */
    let router = Arc::new(router::Router::new());

    {
        let router_c = router.clone();
        tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(4)).await; // beter use Timer
            println!("sending hello");
            router_c.send_hello();
        }
        });

        // loop to read from_node_control
        tokio::spawn(async move {
            loop {
                while let Some(packet) = from_node_control.recv().await {
                    match packet.message_type {
                        // different type of control packets
                        ControlPacketType::Hello => {
                            println!("Received Hello");
                            println!("Should send IHU back");
                        }
                        _ => {
                            println!("Received unknown control packet");
                        }
                    }
                }
            }
        });
    }


    
    // CHANNELS USED FOR COMMUNICATION OF DATAPACKETS
    // Create an unbounded channel for this node
    let (to_tun, mut from_routing_data) = mpsc::unbounded_channel::<DataPacket>();
    let (to_routing_data, mut from_node_data) = mpsc::unbounded_channel::<DataPacket>();

    // Create the PeerManager: an interface to all peers this node is connected to
    // Additional static peers are obtained through the nodeconfig.toml file
    let peer_manager = PeerManager::new();

    // Create static peers from the nodeconfig.toml file
    let peer_man_clone = peer_manager.clone();
    let to_routing_data_clone = to_routing_data.clone();
    let to_routing_control_clone= to_routing_control.clone();
    let router_clone = router.clone();
    tokio::spawn(async move {
        peer_man_clone
            .get_peers_from_config(to_routing_data_clone, to_routing_control_clone, cli.tun_addr, router_clone)
            .await; // --> here we create peer by TcpStream connect
    });

    let peer_man_clone = peer_manager.clone();
    let to_routing_data_clone = to_routing_data.clone();
    let to_routing_control_clone = to_routing_control.clone();
    let router_clone = router.clone();
    // listen for inbound request --> "to created the reverse peer object" --> here we reverse create peer be listener.accept'ing
    tokio::spawn(async move {
        match TcpListener::bind("[::]:9651").await {
            Ok(listener) => {
                // loop to accept the inbound requests
                loop {
                    let to_routing_data_clone_clone = to_routing_data_clone.clone();
                    let to_routing_control_clone_clone = to_routing_control_clone.clone();
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
                            println!(
                                "Received overlay IP from other node: {:?}",
                                received_overlay_ip
                            );

                            // "reverse peer add"
                            let peer_stream_ip = stream.peer_addr().unwrap().ip();
                            match Peer::new(
                                peer_stream_ip,
                                to_routing_data_clone_clone,
                                to_routing_control_clone_clone,
                                stream,
                                received_overlay_ip,
                            ) {
                                Ok(new_peer) => {
                                    //println!("adding new peer to known_peers: {:?}", new_peer);
                                    //peer_man_clone.known_peers.lock().unwrap().push(new_peer);

                                    router_clone.add_directly_connected_peer(new_peer);
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
            while let Some(packet) = from_routing_data.recv().await {
                let data_packet = if let p = packet {
                    println!("LENTHEEE: {}", p.raw_data.len());
                    p
                } else {
                    continue;
                };
                match node_tun_clone.send(&data_packet.raw_data).await {
                    Ok(_) => {
                        println!("Sending it towards this node's TUN");
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
    let to_routing_clone = to_routing_data.clone();
    tokio::spawn(async move {
        loop {
            let mut buf = BytesMut::zeroed(LINK_MTU);
            match node_tun_clone.recv(&mut buf).await {
                Ok(n) => {
                    buf.truncate(n);

                    println!("Got packet on my TUN, byyes: {}", n);

                    // Remainder: if we read from TUN we will only need to parse them into DataPackets
                    // Extract the destination IP address using Etherparse
                    match PacketHeaders::from_ip_slice(&buf) {
                        Ok(packet) => {
                            if let Some(IpHeader::Version4(header, _)) = packet.ip {
                                let dest_addr = Ipv4Addr::from(header.destination);
                                println!("Destination IPv4 address: {}", dest_addr);

                                let data_packet = DataPacket {
                                    dest_ip: dest_addr,
                                    raw_data: buf.to_vec(),
                                };

                                println!("LEN: {}", data_packet.raw_data.len());

                                match to_routing_clone.send(data_packet) {
                                    Ok(_) => {
                                        println!("packet sent to to_routing");
                                    }
                                    Err(e) => {
                                        eprintln!("Error sending packet to to_routing: {}", e);
                                    }
                                }
                            } else {
                                println!("Non-IPv4 packet received, ignoring...");
                            }
                        }
                        Err(e) => {
                            println!("buffer: {:?}", buf);
                            eprintln!("Error from_ip_slice: {e}");
                        }
                    }
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
    let router_clone = router.clone();
    tokio::spawn(async move {
        loop {
            let router_inner_clone = router_clone.clone();
            let node_tun_inner_clone = node_tun_clone.clone();
            let to_tun_sender_inner_clone = to_tun_sender_clone.clone();
            while let Some(packet) = from_node_data.recv().await {
                //println!("Read message from from_node, sending it to route_packet function");
                peer_man_clone.route_packet(
                    packet,
                    node_tun_inner_clone.clone(),
                    to_tun_sender_inner_clone.clone(),
                    router_inner_clone.clone(),
                );
            }
        }
    });

    tokio::time::sleep(std::time::Duration::from_secs(60 * 60 * 24)).await;
    Ok(())
}
