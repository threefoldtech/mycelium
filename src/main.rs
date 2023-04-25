use bytes::BytesMut;
use clap::Parser;
use etherparse::{IpHeader, PacketHeaders};
use packet::DataPacket;
use std::{error::Error, net::Ipv4Addr, sync::Arc};

mod codec;
mod node_setup;
mod packet;
mod peer;
mod peer_manager;
mod router;

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
    
    // Creating a new Router instance 
    let router = Arc::new(router::Router::new(node_tun.clone()));
    // Creating a new PeerManager instance
    let _peer_manager = peer_manager::PeerManager::new(router.clone());


    // The TUN interface will only receive data packets. This loops reads data packets from the TUN interface and forwards them to the router.
    // NOTE: only the kernel can put data packets on the TUN interface. This application never puts data packets on the TUN interface for itself.
    {
        let node_tun = node_tun.clone();

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
                                    router.router_data_tx.send(data_packet).unwrap();
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

    tokio::time::sleep(std::time::Duration::from_secs(60 * 60 * 24)).await;
    Ok(())
}
