use std::{
    error::Error, 
    net::{Ipv4Addr},
};

use clap::Parser;
use tokio::sync::mpsc;
use tokio::net::TcpListener;
use tokio::net::TcpStream;


mod node_setup;
mod peer;
mod peer_manager;
mod packet_control;

use peer::Peer;
use peer_manager::PeerManager;

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
        Ok(tun)=> {
            println!("Node setup complete");
            tun
        },
        Err(e) => {
            panic!("Error setting up node: {}", e);
        }
    };

    // Create an unbounded channel for this node
    let (to_tun, mut from_peers) = mpsc::unbounded_channel::<Vec<u8>>();

    // Create the PeerManager: an interface to all peers this node is connected to
    // Each node should include itself as a peer
    // Additional static peers are obtained through the nodeconfig.toml file
    let myself = Peer{id: "0".to_string(), to_peer: to_tun.clone()}; 
    let peer_manager = PeerManager::new(myself);

    let peer_man_clone = peer_manager.clone();
    tokio::spawn(async move {
        peer_man_clone.get_peers_from_config(to_tun.clone()).await; // --> here we create peer by TcpStream connect

        // listen for inbound request --> "to created the reverse peer object" --> here we reverse create peer be listener.accept'ing
        tokio::spawn(async move {
            match TcpListener::bind("[::]:9651").await {
                Ok(listener) => {
                    // loop to accept the inbound requests
                    loop {
                        match listener.accept().await {
                            Ok((stream, _)) => {
                                println!("Got inbound request from: {}", stream.peer_addr().unwrap().to_string());
                                // "reverse peer add"
                                let peer_id = stream.peer_addr().unwrap().to_string();
                                match Peer::new(peer_id, to_tun.clone(), stream) {
                                    Ok(new_peer) => {
                                        peer_man_clone.known_peers.lock().unwrap().push(new_peer);
                                    },
                                    Err(e) => {
                                        eprintln!("Error creating 'reverse' peer: {}", e);
                                    }
                                }
                            },
                            Err(e) => {
                                eprintln!("Error accepting TCP listener: {}", e);
                            }
                       }
                    }
                }, 
                Err(e) => {
                    eprintln!("Error binding TCP listener: {}", e);
                }
            }
        })
    });

    // Loop to read the 'from_peers' receiver and foward it toward the TUN interface
    let node_tun_clone = node_tun.clone();
    tokio::spawn(async move{
        loop {
            while let Some(packet) = from_peers.recv().await {
                match node_tun_clone.send(&packet).await {
                    Ok(packet) => {
                        println!("Received from 'from_peers': {:?}", packet);
                    },
                    Err(e) => {
                        eprintln!("Error sending to TUN interface: {}", e);
                    }
                }
            }
        }
    });

    // TODO: Loop to read from the TUN interface 
    // ??? to send: and forward it towards to correct destination peer (by selecting the correct to_peer sender)
    let node_tun_clone = node_tun.clone();

    let peer_man_clone = peer_manager.clone();


    tokio::spawn(async move{
        loop {
            let link_mtu = 1500;
            let mut buf = vec![0u8; link_mtu];
            match node_tun_clone.recv(&mut buf).await{
                Ok(n) => {

                    buf.truncate(n);
                   println!("Got this from TUN: {:?}", buf); // ZAL ROUTER SOLICITATIONS TONEN

                    // TEMPORARY - TEMPORARY - **************************************$$
                    // TEMP TESTING: we hebben nu gewoon static 1 node toegevoegd van A naar B
                    // en B voegt A automatisch ook als peer toe
                    // laten we nu voorlopig gewoon zeggen --> packetmanager eerste peer in lijst
                    // en daarmee verder testen



                    if let Some(first_peer) = &peer_man_clone.known_peers.lock().unwrap().get(1) {
                        first_peer.to_peer.send(buf).unwrap();
                    }



                    



                    // TEMPORARY - TEMPORARY - **************************************$$
                     



                },
                Err(e) => {
                    eprintln!("Error reading from TUN interface: {}", e);
                }
            }
            // WE NEED TO WORK WITH FRAMED HERE

            
            

        }
    });


    tokio::time::sleep(std::time::Duration::from_secs(60 * 60 * 24)).await;
    Ok(())
}