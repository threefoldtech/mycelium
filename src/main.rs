use crate::packet::DataPacket;
use crate::router::StaticRoute;
use bytes::BytesMut;
use clap::Parser;
use etherparse::{IpHeader, PacketHeaders};
use log::{debug, error, info, trace};
use std::{
    error::Error,
    net::{Ipv6Addr, SocketAddr},
    path::Path,
};
use tokio::io::AsyncBufReadExt;

mod babel;
mod codec;
mod crypto;
mod interval;
mod metric;
mod node_setup;
mod packet;
mod peer;
mod peer_manager;
mod router;
mod routing_table;
mod sequence_number;
mod source_table;
mod x25519;

const LINK_MTU: usize = 1420;

#[derive(Parser)]
struct Cli {
    #[arg(short = 'p', long = "peers", num_args = 1..)]
    static_peers: Vec<SocketAddr>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    pretty_env_logger::init();

    // Generate a new keypair for this node, panic if it fails
    let node_secret_key = crypto::SecretKey::load_file(Path::new("keys.txt")).await?;
    let node_pub_key = crypto::PublicKey::from(&node_secret_key);
    let node_keypair = (node_secret_key, node_pub_key);

    // Generate the node's IPv6 address from its public key
    let node_addr = node_pub_key.address();
    info!("Node address: {}", node_addr);

    // Create TUN interface and add static route
    let node_tun = match node_setup::setup_node(node_addr).await {
        Ok(tun) => {
            info!("Node setup complete");
            tun
        }
        Err(e) => {
            error!("Error setting up node: {e}");
            panic!("Eror setting up node: {e}")
        }
    };

    debug!("Node public key: {:?}", node_keypair.1);

    let static_peers = cli.static_peers;

    // Creating a new Router instance
    let router = match router::Router::new(
        node_tun.clone(),
        node_addr,
        vec![StaticRoute::new(node_addr.into())],
        node_keypair.clone(),
    ) {
        Ok(router) => {
            info!(
                "Router created. Pubkey: {:x}",
                BytesMut::from(&router.node_public_key().as_bytes()[..])
            );
            router
        }
        Err(e) => {
            error!("Error creating router: {e}");
            panic!("Error creating router: {e}");
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
        let own_secret = node_keypair.0;

        tokio::spawn(async move {
            loop {
                let mut buf = BytesMut::zeroed(LINK_MTU);

                match node_tun.recv(&mut buf).await {
                    Ok(n) => {
                        buf.truncate(n);
                    }
                    Err(e) => {
                        error!("Error reading from TUN: {e}");
                        continue;
                    }
                }

                let packet = match PacketHeaders::from_ip_slice(&buf) {
                    Ok(packet) => packet,
                    Err(e) => {
                        eprintln!("Error from_ip_slice: {e}");
                        continue;
                    }
                };

                let dest_addr = if let Some(IpHeader::Version6(header, _)) = packet.ip {
                    Ipv6Addr::from(header.destination)
                } else {
                    continue;
                };

                trace!("Received packet from TUN with dest addr: {:?}", dest_addr);

                // Check if destination address is in 200::/7 range
                let first_byte = dest_addr.segments()[0] >> 8; // get the first byte
                if !(0x02..=0x3F).contains(&first_byte) {
                    continue;
                }

                // create shared secret between node and dest_addr
                let pubkey_recipient = match router.get_pubkey_from_dest(dest_addr) {
                    Some(pubkey) => pubkey,
                    None => {
                        eprintln!("No pubkey found for dest addr: {:?}", dest_addr);
                        continue;
                    }
                };

                let shared_secret = own_secret.shared_secret(&pubkey_recipient);

                // println!(
                //     "encryption with pubkey of recipient: {:?}",
                //     pubkey_recipient
                // );
                // println!("encryption with {:?}", shared_secret.as_bytes());

                // inject own pubkey
                let data_packet = DataPacket {
                    dest_ip: dest_addr,
                    pubkey: router.node_public_key(),
                    // encrypt data with shared secret
                    raw_data: shared_secret.encrypt(&buf),
                };

                if router.router_data_tx().send(data_packet).is_err() {
                    error!("Failed to send data_packet, router is gone");
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

                    println!("\n----------- Current peers: -----------");
                    for p in router.peer_interfaces() {
                        println!(
                            "Peer: {:?}, with link cost: {}",
                            p.overlay_ip(),
                            p.link_cost()
                        );
                    }

                    println!("\n\n");
                }
                Err(e) => {
                    eprintln!("Error reading line: {}", e);
                    return;
                }
            }
        }
    });

    let quit_signal = tokio::signal::ctrl_c();

    tokio::select! {
        _ = read_handle => { /* The read task completed (this should never happen) */ }
        _ = quit_signal => { /* The user pressed ctrl+c (the program should exit here) */ }
    }

    Ok(())
}
