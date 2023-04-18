use futures::{SinkExt, StreamExt};
use std::{error::Error, net::{IpAddr, Ipv4Addr}};
use tokio::{
    net::TcpStream,
    select,
    sync::{mpsc},
};
use tokio_util::codec::{Framed, Decoder};

use crate::packet_control::{DataPacket, Packet, PacketCodec};

#[derive(Debug)]
pub struct Peer {
    pub stream_ip: IpAddr, 
    pub to_peer: mpsc::UnboundedSender<Packet>,
    pub overlay_ip: Ipv4Addr,
}

impl Peer {
    pub fn new(stream_ip: IpAddr, to_routing: mpsc::UnboundedSender<Packet>, stream: TcpStream, overlay_ip: Ipv4Addr) -> Result<Self, Box<dyn Error>> {

        // Create a Framed for each peer
        let mut framed = Framed::new(stream, PacketCodec::new());
        // Create an unbounded channel for each peer
        let (to_peer, mut from_routing) = mpsc::unbounded_channel::<Packet>();

        tokio::spawn(async move {
            loop {
                select! {
                    // received from peer
                    frame = framed.next() => {
                        match frame {
                            Some(Ok(packet)) => {
                               // Send to TUN interface
                               // toekomst: nog een een tussenstap
                               println!("3: I'm the peer instance that got the message from the TCP stream");
                                match packet {
                                    Packet::DataPacket(packet) => {
                                        if let Err(error) = to_routing.send(Packet::DataPacket(packet)){
                                         eprintln!("Error sending to TUN: {}", error);
                                        }

                                    }
                                    // Packet::ControlPacket(packet) => {
                                        // TODO: control packet
                                    // }
                                }

                            },
                            Some(Err(e)) => {
                                eprintln!("Error from framed: {}", e);
                            },
                            None => {
                                println!("Stream is closed.");
                                return
                            }
                        }
                    }
                    // receive from from_routing
                    Some(packet) = from_routing.recv() => { 
                        println!("Receiver from from_routing, sending it over the TCP stream");
                        // Send it over the TCP stream
                        if let Err(e) = framed.send(packet).await {
                            eprintln!("Error writing to stream: {}", e);
                        }
                    }
                }
            }
        });

        Ok(Self { stream_ip, to_peer, overlay_ip })
    }
}
