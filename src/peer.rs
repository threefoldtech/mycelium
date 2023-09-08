use futures::{SinkExt, StreamExt};
use log::{error, info, trace};
use std::{
    error::Error,
    net::IpAddr,
    sync::{Arc, RwLock},
};
use tokio::{net::TcpStream, select, sync::mpsc};
use tokio_util::codec::Framed;

use crate::{codec::PacketCodec, packet::Packet};
use crate::{
    packet::{ControlPacket, ControlStruct, DataPacket},
    sequence_number::SeqNo,
};

#[derive(Debug, Clone)]
/// A peer represents a directly connected participant in the network.
pub struct Peer {
    inner: Arc<RwLock<PeerInner>>,
    to_peer_data: mpsc::UnboundedSender<DataPacket>,
    to_peer_control: mpsc::UnboundedSender<ControlPacket>,
    /// Used to identify peer based on its connection params
    stream_ip: IpAddr,
    // TODO: not needed
    overlay_ip: IpAddr,
}

impl Peer {
    pub fn new(
        stream_ip: IpAddr,
        router_data_tx: mpsc::Sender<DataPacket>,
        router_control_tx: mpsc::UnboundedSender<(ControlStruct, Peer)>,
        stream: TcpStream,
        overlay_ip: IpAddr,
    ) -> Result<Self, Box<dyn Error>> {
        // Data channel for peer
        let (to_peer_data, mut from_routing_data) = mpsc::unbounded_channel::<DataPacket>();
        // Control channel for peer
        let (to_peer_control, mut from_routing_control) =
            mpsc::unbounded_channel::<ControlPacket>();
        // Make sure Nagle's algorithm is disabeld as it can cause latency spikes.
        stream.set_nodelay(true)?;
        let peer = Peer {
            inner: Arc::new(RwLock::new(PeerInner::new())),
            to_peer_data,
            to_peer_control,
            stream_ip,
            overlay_ip,
        };

        // Framed for peer
        // Used to send and receive packets from a TCP stream
        let mut framed = Framed::new(stream, PacketCodec::new());

        {
            let peer = peer.clone();

            tokio::spawn(async move {
                loop {
                    select! {
                        // Received over the TCP stream
                        frame = framed.next() => {
                            match frame {
                                Some(Ok(packet)) => {
                                    match packet {
                                        Packet::DataPacket(packet) => {
                                            if let Err(error) = router_data_tx.send(packet).await{
                                                error!("Error sending to to_routing_data: {}", error);
                                            }
                                        }
                                        Packet::ControlPacket(packet) => {
                                            // Parse the DataPacket into a ControlStruct as the to_routing_control channel expects
                                            let control_struct = ControlStruct {
                                                control_packet: packet,
                                                src_overlay_ip: overlay_ip,
                                                // Note: although this control packet is received from the TCP stream
                                                // we set the src_overlay_ip to the overlay_ip of the peer
                                                // as we 'arrived' in the peer instance of representing the sending node on this current node
                                            };
                                            if let Err(error) = router_control_tx.send((control_struct, peer.clone())) {
                                                error!("Error sending to to_routing_control: {}", error);
                                            }

                                        }
                                    }
                                }
                                Some(Err(e)) => {
                                    error!("Error from framed: {}", e);
                                },
                                None => {
                                    info!("Stream is closed.");
                                    return
                                }
                            }
                        }

                        Some(packet) = from_routing_data.recv() => {
                            let mut packet_buf: [_; 5] = std::array::from_fn(|_| None);
                            let mut packets_received = 1;
                            packet_buf[0] = Some(packet);
                            for buf_slot in packet_buf.iter_mut().skip(1) {
                                // There can be 2 cases of errors here, empty channel and no more
                                // senders. In both cases we don't really care at this point
                                *buf_slot = if let Ok(packet) = from_routing_data.try_recv() {
                                    trace!("Instantly queued ready packet to transfer to peer");
                                    packets_received += 1;
                                    Some(packet)
                                } else { break }
                            }
                            let mut packet_stream = futures::stream::iter(
                                packet_buf
                                    .into_iter()
                                    .take(packets_received)
                                    .filter_map(|item| item.map(|item| Ok(Packet::DataPacket(item)))));
                            if let Err(e) = framed.send_all(&mut packet_stream).await {
                                error!("Error writing to stream: {}", e);
                            }
                        }

                        Some(packet) = from_routing_control.recv() => {
                            // Send it over the TCP stream
                            if let Err(e) = framed.send(Packet::ControlPacket(packet)).await {
                                error!("Error writing to stream: {}", e);
                            }
                        }
                    }
                }
            });
        }

        Ok(peer)
    }

    /// Get current sequence number for this peer.
    pub fn hello_seqno(&self) -> SeqNo {
        self.inner.read().unwrap().hello_seqno
    }

    /// Adds 1 to the sequence number of this peer .
    pub fn increment_hello_seqno(&self) {
        self.inner.write().unwrap().hello_seqno += 1;
    }

    pub fn time_last_received_hello(&self) -> tokio::time::Instant {
        self.inner.read().unwrap().time_last_received_hello
    }

    pub fn set_time_last_received_hello(&self, time: tokio::time::Instant) {
        self.inner.write().unwrap().time_last_received_hello = time
    }

    /// Get overlay IP for this peer
    pub fn overlay_ip(&self) -> IpAddr {
        self.overlay_ip
    }

    /// For sending data packets towards a peer instance on this node.
    /// It's send over the to_peer_data channel and read from the corresponding receiver.
    /// The receiver sends the packet over the TCP stream towards the destined peer instance on another node
    pub fn send_data_packet(&self, data_packet: DataPacket) -> Result<(), Box<dyn Error>> {
        Ok(self.to_peer_data.send(data_packet)?)
    }

    /// For sending control packets towards a peer instance on this node.
    /// It's send over the to_peer_control channel and read from the corresponding receiver.
    /// The receiver sends the packet over the TCP stream towards the destined peer instance on another node
    pub fn send_control_packet(&self, control_packet: ControlPacket) -> Result<(), Box<dyn Error>> {
        Ok(self.to_peer_control.send(control_packet)?)
    }

    pub fn link_cost(&self) -> u16 {
        self.inner.read().unwrap().link_cost
    }

    pub fn set_link_cost(&self, link_cost: u16) {
        self.inner.write().unwrap().link_cost = link_cost
    }

    pub fn underlay_ip(&self) -> IpAddr {
        self.stream_ip
    }

    pub fn time_last_received_ihu(&self) -> tokio::time::Instant {
        self.inner.read().unwrap().time_last_received_ihu
    }

    pub fn set_time_last_received_ihu(&self, time: tokio::time::Instant) {
        self.inner.write().unwrap().time_last_received_ihu = time
    }
}

impl PartialEq for Peer {
    fn eq(&self, other: &Self) -> bool {
        self.overlay_ip() == other.overlay_ip()
    }
}

#[derive(Debug)]
struct PeerInner {
    hello_seqno: SeqNo,
    time_last_received_hello: tokio::time::Instant,
    link_cost: u16,
    time_last_received_ihu: tokio::time::Instant,
}

impl PeerInner {
    /// Create a new `PeerInner`, holding the mutable state of a [`Peer`]
    pub fn new() -> Self {
        // Initialize last_sent_hello_seqno to 0
        let hello_seqno = SeqNo::default();
        // Initialize last_path_cost to infinity - 1
        let link_cost = u16::MAX - 1;
        // Initialize time_last_received_hello to now
        let time_last_received_hello = tokio::time::Instant::now();
        // Initialiwe time_last_send_ihu
        let time_last_received_ihu = tokio::time::Instant::now();

        Self {
            hello_seqno,
            link_cost,
            time_last_received_ihu,
            time_last_received_hello,
        }
    }
}
