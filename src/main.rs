use bytes::BytesMut;
use clap::Parser;
use etherparse::{IpHeader, PacketHeaders};
use packet::{ControlPacket, ControlPacketType, ControlStruct, DataPacket};
use std::{error::Error, net::Ipv4Addr, sync::Arc};
use tokio::{io::AsyncReadExt, io::AsyncWriteExt, net::TcpListener, sync::mpsc};

mod codec;
mod node_setup;
mod packet;
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

    // The sender halve (to_routing_control) is passed to each Peer instance.
    // When a Peer instance receives a ControlPacket from another remote Peer instance (over TCP stream) it will create a ControlStruct and send it over this channel (see peer.rs).
    // The receiver halve (from_node_control) is read (in a loop) in the main thread.
    let (to_routing_control, mut from_node_control) = mpsc::unbounded_channel::<ControlStruct>();
    
    // The sender halve (to_routing_data) is passed to each Peer instance.
    // When a Peer instance receives a DataPacket from another remote Peer instance (over TCP stream) it will send it over this channel (see peer.rs).
    // The receiver halve (from_node_data) is read (in a loop) in the main thread.
    // 2 things might happen when a DataPacket is received (see route_packet function):
    //      1. The DataPacket is a packet for the local node (e.g. a ping request) --> send it to the TUN interface (through the to_tun channel)
    //      2. The DataPacket is a packet for a remote node --> send it to the corresponding Peer instance (through the to_peer_data channel)
    let (to_routing_data, mut from_node_data) = mpsc::unbounded_channel::<DataPacket>();
    
    // The sender (to_tun) is passed to the route_packet function.
    // When a DataPacket is received and it is a packet for the local node it will be sent over this channel.
    // The receiver (from_routing_data) is read (in a loop) in the main thread.
    let (to_tun, mut from_routing_data) = mpsc::unbounded_channel::<DataPacket>();

    
    let router = Arc::new(router::Router::new());
    let peer_manager = PeerManager::new();


    {
        let router = router.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await; // beter use Timer
                router.send_hello();
            }
        });

        // This loop reads control messages from the from_node_control channel receiver Halve.
        // This means that this loop will be executed whenever a ControlPacket is received from a remote Peer instance (aka another Node). 
        // HANDLING THE DIFFERENT CONTROL MESSAGES --> MOET ERGENS ANDERS GEBEUREN!
        tokio::spawn(async move {
            loop {
                while let Some(packet) = from_node_control.recv().await {
                    match packet.control_packet.message_type {
                        ControlPacketType::Hello => {
                            let dst_ip = packet.src_overlay_ip;
                            packet.reply(ControlPacket::new_ihu(10, 1000, dst_ip));
                            println!("IHU {}", dst_ip);
                        }
                        ControlPacketType::IHU => {
                            // Upon receiving an IHU message, nothing particular should happen
                        }
                        _ => {
                            println!("Received unknown control packet");
                        }
                    }
                }
            }
        });
    }

    // Create static peers from the nodeconfig.toml file
    {
        let peer_manager = peer_manager.clone();
        let to_routing_data = to_routing_data.clone();
        let to_routing_control = to_routing_control.clone();
        let router = router.clone();
        tokio::spawn(async move {
            peer_manager
                .get_peers_from_config(to_routing_data, to_routing_control, cli.tun_addr, router)
                .await; // --> here we create peer by TcpStream connect
        });
    }

    {
        let to_routing_data = to_routing_data.clone();
        let to_routing_control = to_routing_control.clone();
        let router = router.clone();

        // listen for inbound request --> "to created the reverse peer object" --> here we reverse create peer be listener.accept'ing
        tokio::spawn(async move {
            match TcpListener::bind("[::]:9651").await {
                Ok(listener) => {
                    // loop to accept the inbound requests
                    loop {
                        let to_routing_data = to_routing_data.clone();
                        let to_routing_control = to_routing_control.clone();
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
                                    to_routing_data,
                                    to_routing_control,
                                    stream,
                                    received_overlay_ip,
                                ) {
                                    Ok(new_peer) => {
                                        router.add_directly_connected_peer(new_peer);
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
    }

    // Loop to read the from_routing_data receiver which contains data packets with a destination IP of this node. 
    // These packets are forwarded to the actual TUN interface.
    {
        let node_tun = node_tun.clone();
        tokio::spawn(async move {
            loop {
                while let Some(data_packet) = from_routing_data.recv().await {
                    if let Err(e) = node_tun.send(&data_packet.raw_data).await {
                        eprintln!("Error sending to TUN interface: {}", e);
                    }
                }
            }
        });
    }

    // Loop to read from node's TUN interface and send it to to_routing_data sender halve.
    // Note: a packet will only arrive on the TUN interface if it has been put there by the kernel.
    // This means that the application will never read packets from the TUN interface that it has sent itself.
    {
        let node_tun = node_tun.clone();
        let to_routing_data = to_routing_data.clone();

        tokio::spawn(async move {
            loop {
                let mut buf = BytesMut::zeroed(LINK_MTU);
                match node_tun.recv(&mut buf).await {
                    Ok(n) => {
                        buf.truncate(n);

                        match PacketHeaders::from_ip_slice(&buf) {
                            Ok(packet) => {
                                if let Some(IpHeader::Version4(header, _)) = packet.ip {
                                    let dest_addr = Ipv4Addr::from(header.destination);
                                    println!("Destination IPv4 address: {}", dest_addr);

                                    let data_packet = DataPacket {
                                        dest_ip: dest_addr,
                                        raw_data: buf.to_vec(),
                                    };

                                    match to_routing_data.send(data_packet) {
                                        Ok(_) => {
                                            println!("packet sent to to_routing_data");
                                        }
                                        Err(e) => {
                                            eprintln!("Error sending packet to to_routing_data: {}", e);
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
    }

    // Loop to read from from_node_data receiver and to pass the packet further towards
    // the route_packet function which will send the packet towards the correct to_peer 
    // or towards this node's TUN interface (if destination IP of packet match this node's TUN address).
    {
        // let peer_manager = peer_manager.clone();
        let node_tun = node_tun.clone();
        let to_tun = to_tun.clone();
        let router = router.clone();
        tokio::spawn(async move {
            loop {
                while let Some(data_packet) = from_node_data.recv().await {
                    router.route_data_packet(data_packet, node_tun.clone(), to_tun.clone());
                } 
            }
        });
    }

    tokio::time::sleep(std::time::Duration::from_secs(60 * 60 * 24)).await;
    Ok(())
}
