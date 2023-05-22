use crate::packet::DataPacket;
use crate::router::StaticRoute;
use bytes::BytesMut;
use clap::Parser;
use etherparse::{IpHeader, PacketHeaders};
use std::{
    error::Error,
    net::{Ipv4Addr, SocketAddr},
};
use tokio::io::AsyncBufReadExt;

mod codec;
mod node_setup;
mod packet;
mod peer;
mod peer_manager;
mod router;
mod routing_table;
mod source_table;

const LINK_MTU: usize = 1420;

#[derive(Parser)]
struct Cli {
    #[arg(short = 'a', long = "tun-addr")]
    tun_addr: Ipv4Addr,
    #[arg(short = 'p', long = "peers", num_args = 1..)]
    static_peers: Vec<SocketAddr>,
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

    let static_peers = cli.static_peers;

    // Creating a new Router instance
    let router = match router::Router::new(
        node_tun.clone(),
        vec![StaticRoute::new(cli.tun_addr.into())],
    ) {
        Ok(router) => {
            println!("Router created. ID: {}", router.router_id());
            router
        }
        Err(e) => {
            panic!("Error creating router: {}", e);
        }
    };

    // Creating a new PeerManager instance
    let _peer_manager: peer_manager::PeerManager =
        peer_manager::PeerManager::new(router.clone(), static_peers);

    // Read packets from the TUN interface (originating from the kernel) and send them to the router
    // Note: we will never receive control packets from the kernel, only data packets
    {
        let router = router.clone();
        let node_tun = node_tun.clone();

        tokio::spawn(async move {
            loop {
                let mut buf = BytesMut::zeroed(LINK_MTU);

                match node_tun.recv(&mut buf).await {
                    Ok(n) => {
                        buf.truncate(n);
                    }
                    Err(e) => {
                        eprintln!("Error reading from TUN: {}", e);
                        continue;
                    }
                }

                let packet = match PacketHeaders::from_ip_slice(&buf) {
                    Ok(packet) => packet,
                    Err(e) => {
                        println!("buffer: {:?}", buf);
                        eprintln!("Error from_ip_slice: {}", e);
                        continue;
                    }
                };

                if let Some(IpHeader::Version4(header, _)) = packet.ip {
                    let dest_addr = Ipv4Addr::from(header.destination);
                    println!("Destination IPv4 address: {}", dest_addr);

                    let data_packet = DataPacket {
                        dest_ip: dest_addr,
                        raw_data: buf.to_vec(),
                    };
                    if router.router_data_tx().send(data_packet).is_err() {
                        eprintln!("Failed to send data_packet");
                    }
                } else {
                    println!("Non-IPv4 packet received, ignoring...");
                }
            }
        });
    }

    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    let mut line = String::new();

    let read_handle = tokio::spawn(async move {
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => return, // EOF, quit
                Ok(_) => {
                    // Remove trailing newline
                    line.pop();
                    println!("----------- Current selected routes -----------{}\n", line);
                    router.print_selected_routes();
                    println!("----------- Current fallback routes -----------{}\n", line);
                    router.print_fallback_routes();

                    println!("\n----------- Current peers: -----------");
                    for p in router.peer_interfaces() {
                        println!(
                            "Peer: {:?}, with link cost: {}",
                            p.overlay_ip(),
                            p.link_cost()
                        );
                    }

                    println!("\n----------- Current source table: -----------");
                    router.print_source_table();

                    println!("\n\n");
                }
                Err(e) => {
                    eprintln!("Error reading line: {}", e);
                    return;
                }
            }
        }
    });

    let sleep_handle = tokio::spawn(async move {
        // Just die after 1 day, you've probably leaked memory by then anyway
        tokio::time::sleep(tokio::time::Duration::from_secs(60 * 60 * 24)).await;
    });

    tokio::select! {
        _ = read_handle => { /* The read task completed (this should never happen) */ }
        _ = sleep_handle => { /* The sleep task completed (the program should exit here) */ }
    }

    Ok(())
}
