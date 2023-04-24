use futures::{SinkExt, StreamExt};
use std::{
    error::Error,
    net::{IpAddr, Ipv4Addr},
};
use tokio::{net::TcpStream, select, sync::mpsc};
use tokio_util::codec::Framed;

use crate::packet::{ControlPacket, DataPacket, ControlStruct};
use crate::{codec::PacketCodec, packet::Packet};

// IS A NEIGHBOR IN THE IDEA OF BABEL

#[derive(Debug)]
pub struct Peer {
    pub stream_ip: IpAddr,
    pub to_peer_data: mpsc::UnboundedSender<DataPacket>,
    pub to_peer_control: mpsc::UnboundedSender<ControlPacket>, // RFC: the local node's interface over which this neighbour is reachable
    pub overlay_ip: Ipv4Addr, // RFC: address of the neigbouring interface

                              // additional fields according to babel RFC
                              // pub txcost: u16,
}

impl Peer {
    pub fn new(
        stream_ip: IpAddr,
        to_routing_data: mpsc::UnboundedSender<DataPacket>,
        to_routing_control: mpsc::UnboundedSender<ControlStruct>,
        stream: TcpStream,
        overlay_ip: Ipv4Addr,
    ) -> Result<Self, Box<dyn Error>> {
        // Create a Framed for each peer
        let mut framed = Framed::new(stream, PacketCodec::new());
        // Create an unbounded channel for each peer
        let (to_peer_data, mut from_routing_data) = mpsc::unbounded_channel::<DataPacket>();
        // Create control channel
        let (to_peer_control, mut from_routing_control) = mpsc::unbounded_channel::<ControlPacket>();
        let (control_reply_tx, mut control_reply_rx) = mpsc::unbounded_channel::<ControlPacket>();

        tokio::spawn(async move {
            loop {
                select! {
                // received from peer

                frame = framed.next() => {
                    match frame {
                        Some(Ok(packet)) => {
                            match packet {
                                Packet::DataPacket(packet) => {
                                    if let Err(error) = to_routing_data.send(packet){
                                     eprintln!("Error sending to to_routing_data: {}", error);
                                    }

                                }
                                Packet::ControlPacket(packet) => {
                                    // create control struct
                                    let control_struct = ControlStruct {
                                        control_packet: packet,
                                        response_tx: control_reply_tx.clone(),
                                        src_overlay_ip: IpAddr::V4(overlay_ip),
                                    };
                                    if let Err(error) = to_routing_control.send(control_struct){
                                     eprintln!("Error sending to to_routing_control: {}", error);
                                    }

                                }
                            }
                        }
                        Some(Err(e)) => {
                            eprintln!("Error from framed: {}", e);
                        },
                        None => {
                            println!("Stream is closed.");
                            return
                        }
                        }
                    }

                // received from routing (both data and control channels)

                Some(packet) = from_routing_data.recv() => {
                    println!("Received DATA from routing, sending it over the TCP stream");
                    // Send it over the TCP stream
                    if let Err(e) = framed.send(Packet::DataPacket(packet)).await {
                        eprintln!("Error writing to stream: {}", e);
                    }
                }

                Some(packet) = from_routing_control.recv() => {
                    println!("Received CONTROL from routing, sending it over the TCP stream");
                    // Send it over the TCP stream
                    if let Err(e) = framed.send(Packet::ControlPacket(packet)).await {
                        eprintln!("Error writing to stream: {}", e);
                    }
                }
                Some(packet) = control_reply_rx.recv() => {
                    println!("Received CONTROL REPLY from routing, sending it over the TCP stream");
                    // Send it over the TCP stream
                    if let Err(e) = framed.send(Packet::ControlPacket(packet)).await {
                        eprintln!("Error writing to stream: {}", e);
                    }
                } 
                }
            }
        });
        Ok(Self {
            stream_ip,
            to_peer_data,
            to_peer_control,
            overlay_ip,
        })
    }
}
