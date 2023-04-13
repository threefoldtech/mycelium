use futures::{SinkExt, StreamExt};
use std::{error::Error};
use tokio::{
    net::TcpStream,
    select,
    sync::{mpsc},
};
use tokio_util::codec::Framed;

use crate::packet_control::{DataPacket, Packet, PacketCodec};

#[derive(Debug)]
pub struct Peer {
    pub id: String,
    pub to_peer: mpsc::UnboundedSender<Vec<u8>>,
}

impl Peer {
    pub fn new(
        id: String,
        to_tun: mpsc::UnboundedSender<Vec<u8>>,
        stream: TcpStream,
    ) -> Result<Self, Box<dyn Error>> {
        let mut framed = Framed::new(stream, PacketCodec::new());

        // Create channel for each peer
        let (to_peer, mut from_tun) = mpsc::unbounded_channel::<Vec<u8>>();

        tokio::spawn(async move {
            loop {
                select! {
                    // received from peer
                    frame = framed.next() => {
                        match frame {
                            Some(Ok(packet)) => {
                               // Send to TUN interface
                               // toekomst: nog een een tussenstap
                                match packet {
                                    Packet::DataPacket(packet) => {
                                        if let Err(error) = to_tun.send(packet.raw_data) {
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
                                println!("stream is closed.");
                                return
                            }
                        }
                    }
                    // receive from tun (send to peer)
                    Some(raw_data) = from_tun.recv() => {
                        // if received from TUN, always data packet
                        let data_packet = Packet::DataPacket(DataPacket {raw_data});

                        if let Err(e) = framed.send(data_packet).await {
                            eprintln!("Error writing to stream: {}", e);
                        }
                    }
                }
            }
        });

        Ok(Self { id, to_peer })
    }
}
