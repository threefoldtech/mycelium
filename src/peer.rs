use futures::{SinkExt, StreamExt};
use std::{
    error::Error,
    net::{IpAddr, Ipv4Addr},
};
use tokio::{net::TcpStream, select, sync::mpsc};
use tokio_util::codec::Framed;

use crate::packet::{ControlPacket, DataPacket, ControlStruct};
use crate::{codec::PacketCodec, packet::Packet};

#[derive(Debug, Clone)]
pub struct Peer {
    pub stream_ip: IpAddr,
    pub to_peer_data: mpsc::UnboundedSender<DataPacket>,
    pub to_peer_control: mpsc::UnboundedSender<ControlPacket>,
    pub overlay_ip: Ipv4Addr, 

    pub last_sent_hello_seqno: u16,
    pub link_cost: u16,
}

impl Peer {
    pub fn new(
        stream_ip: IpAddr,
        router_data_tx: mpsc::UnboundedSender<DataPacket>,
        router_control_tx: mpsc::UnboundedSender<ControlStruct>,
        stream: TcpStream,
        overlay_ip: Ipv4Addr,

    ) -> Result<Self, Box<dyn Error>> {

        // Framed for peer
        // Used to send and receive packets from a TCP stream
        let mut framed = Framed::new(stream, PacketCodec::new());
        // Data channel for peer 
        let (to_peer_data, mut from_routing_data) = mpsc::unbounded_channel::<DataPacket>();
        // Control channel for peer
        let (to_peer_control, mut from_routing_control) = mpsc::unbounded_channel::<ControlPacket>();
        // Control reply channel for peer
        let (control_reply_tx, mut control_reply_rx) = mpsc::unbounded_channel::<ControlPacket>();

        // Initialize last_sent_hello_seqno to 0
        let last_sent_hello_seqno = 0;
        // Initialize last_path_cost to infinity
        let link_cost = 65535;

        tokio::spawn(async move {
            loop {
                select! {
             
                // Received over the TCP stream
                frame = framed.next() => {
                    match frame {
                        Some(Ok(packet)) => {
                            match packet {
                                Packet::DataPacket(packet) => {
                                    if let Err(error) = router_data_tx.send(packet){
                                     eprintln!("Error sending to to_routing_data: {}", error);
                                    }

                                }
                                Packet::ControlPacket(packet) => {
                                    // Parse the DataPacket into a ControlStruct as the to_routing_control channel expects 
                                    let control_struct = ControlStruct {
                                        control_packet: packet,
                                        control_reply_tx: control_reply_tx.clone(),
                                        src_overlay_ip: IpAddr::V4(overlay_ip),
                                        // Note: although this control packet is received from the TCP stream
                                        // we set the src_overlay_ip to the overlay_ip of the peer
                                        // as we 'arrived' in the peer instance of representing the sending node on this current node
                                    };
                                    if let Err(error) = router_control_tx.send(control_struct){
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

                Some(packet) = from_routing_data.recv() => {
                    // Send it over the TCP stream
                    if let Err(e) = framed.send(Packet::DataPacket(packet)).await {
                        eprintln!("Error writing to stream: {}", e);
                    }
                }

                Some(packet) = from_routing_control.recv() => {
                    // Send it over the TCP stream
                    if let Err(e) = framed.send(Packet::ControlPacket(packet)).await {
                        eprintln!("Error writing to stream: {}", e);
                    }
                }
                Some(packet) = control_reply_rx.recv() => {
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
            last_sent_hello_seqno,
            link_cost,
        })
    }

    pub fn increase_hello_seqno(&mut self) {
        self.last_sent_hello_seqno = self.last_sent_hello_seqno + 1;
        println!("last hello seqno increasted to {}", self.last_sent_hello_seqno);
    }
}