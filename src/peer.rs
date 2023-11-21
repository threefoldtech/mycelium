use futures::{SinkExt, StreamExt};
use log::{debug, error, info, trace};
use std::{
    error::Error,
    net::IpAddr,
    sync::{Arc, RwLock},
};
use tokio::{net::TcpStream, select, sync::mpsc};
use tokio_util::codec::Framed;

use crate::packet::{self, Packet};
use crate::{
    packet::{ControlPacket, DataPacket},
    sequence_number::SeqNo,
};

/// The maximum amount of packets to immediately send if they are ready when the first one is
/// received.
const PACKET_COALESCE_WINDOW: usize = 5;

/// Cost to add to the peer_link_cost for "local processing".
///
/// The current peer link cost is calculated from a HELLO rtt. This is great to measure link
/// latency, since packets are processed in order. However, on local idle links, this value will
/// likely be 0 since we round down (from the amount of ms it took to process), which does not
/// accurately reflect the fact that there is in fact a cost associated with using a peer, even on
/// these local links.
const PACKET_PROCESSING_COST: u16 = 1;

/// The default link cost assigned to new peers before their actual cost is known.
///
/// In theory, the best value would be U16::MAX - 1, however this value would take too long to be
/// flushed out of the smoothed metric. A default of a 50 (50 ms) is still large enough, and
/// also has a lower impact on the initial link cost when a peer connects for the route metrics.
const DEFAULT_LINK_COST: u16 = 50;

/// Multiplier for smoothed metric calculation of the existing smoothed metric.
const EXISTING_METRIC_FACTOR: u32 = 9;
/// Divisor for smoothed metric calcuation of the combined metric
const TOTAL_METRIC_DIVISOR: u32 = 10;

#[derive(Debug, Clone)]
/// A peer represents a directly connected participant in the network.
pub struct Peer {
    inner: Arc<PeerInner>,
}

impl Peer {
    pub fn new(
        stream_ip: IpAddr,
        router_data_tx: mpsc::Sender<DataPacket>,
        router_control_tx: mpsc::UnboundedSender<(ControlPacket, Peer)>,
        stream: TcpStream,
        overlay_ip: IpAddr,
        dead_peer_sink: mpsc::Sender<Peer>,
    ) -> Result<Self, Box<dyn Error>> {
        // Data channel for peer
        let (to_peer_data, mut from_routing_data) = mpsc::unbounded_channel::<DataPacket>();
        // Control channel for peer
        let (to_peer_control, mut from_routing_control) =
            mpsc::unbounded_channel::<ControlPacket>();
        // Make sure Nagle's algorithm is disabeld as it can cause latency spikes.
        stream.set_nodelay(true)?;
        let peer = Peer {
            inner: Arc::new(PeerInner {
                state: RwLock::new(PeerState::new()),
                to_peer_data,
                to_peer_control,
                stream_ip,
                overlay_ip,
            }),
        };

        // Framed for peer
        // Used to send and receive packets from a TCP stream
        let mut framed = Framed::new(stream, packet::Codec::new());

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
                                            if let Err(error) = router_control_tx.send((packet, peer.clone())) {
                                                error!("Error sending to to_routing_control: {}", error);
                                            }

                                        }
                                    }
                                }
                                Some(Err(e)) => {
                                    error!("Error from framed: {}", e);
                                    break;
                                },
                                None => {
                                    info!("Stream is closed.");
                                    break;
                                }
                            }
                        }

                        Some(packet) = from_routing_data.recv() => {
                            let mut packet_buf: [_; PACKET_COALESCE_WINDOW] = std::array::from_fn(|_| None);
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
                                break;
                            }
                        }

                        Some(packet) = from_routing_control.recv() => {
                            // Send it over the TCP stream
                            if let Err(e) = framed.send(Packet::ControlPacket(packet)).await {
                                error!("Error writing to stream: {}", e);
                                break;
                            }
                        }
                    }
                }

                // Notify router we are dead
                let remote_id = peer.underlay_ip();
                debug!("Notifying router peer {remote_id} is dead");
                if let Err(e) = dead_peer_sink.send(peer).await {
                    error!("Peer {remote_id} could not notify router of termination: {e}");
                }
            });
        }

        Ok(peer)
    }

    /// Get current sequence number for this peer.
    pub fn hello_seqno(&self) -> SeqNo {
        self.inner.state.read().unwrap().hello_seqno
    }

    /// Adds 1 to the sequence number of this peer .
    pub fn increment_hello_seqno(&self) {
        self.inner.state.write().unwrap().hello_seqno += 1;
    }

    pub fn time_last_received_hello(&self) -> tokio::time::Instant {
        self.inner.state.read().unwrap().time_last_received_hello
    }

    pub fn set_time_last_received_hello(&self, time: tokio::time::Instant) {
        self.inner.state.write().unwrap().time_last_received_hello = time
    }

    /// Get overlay IP for this peer
    pub fn overlay_ip(&self) -> IpAddr {
        self.inner.overlay_ip
    }

    /// For sending data packets towards a peer instance on this node.
    /// It's send over the to_peer_data channel and read from the corresponding receiver.
    /// The receiver sends the packet over the TCP stream towards the destined peer instance on another node
    pub fn send_data_packet(&self, data_packet: DataPacket) -> Result<(), Box<dyn Error>> {
        Ok(self.inner.to_peer_data.send(data_packet)?)
    }

    /// For sending control packets towards a peer instance on this node.
    /// It's send over the to_peer_control channel and read from the corresponding receiver.
    /// The receiver sends the packet over the TCP stream towards the destined peer instance on another node
    pub fn send_control_packet(&self, control_packet: ControlPacket) -> Result<(), Box<dyn Error>> {
        Ok(self.inner.to_peer_control.send(control_packet)?)
    }

    /// Get the cost to use the peer, i.e. the additional impact on the [`crate::metric::Metric`]
    /// for using this `Peer`.
    ///
    /// This is a smoothed value, which is calculated over the recent history of link cost.
    pub fn link_cost(&self) -> u16 {
        self.inner.state.read().unwrap().link_cost + PACKET_PROCESSING_COST
    }

    /// Sets the link cost based on the provided value.
    ///
    /// The link cost is not set to the given value, but rather to an average of recent values.
    /// This makes sure short-lived, hard spikes of the link cost of a peer don't influence the
    /// routing.
    pub fn set_link_cost(&self, new_link_cost: u16) {
        // Calculate new link cost by multiplying (i.e. scaling) old and new link cost and
        // averaging them.
        let mut inner = self.inner.state.write().unwrap();
        inner.link_cost = (((inner.link_cost as u32) * EXISTING_METRIC_FACTOR
            + (new_link_cost as u32) * (TOTAL_METRIC_DIVISOR - EXISTING_METRIC_FACTOR))
            / TOTAL_METRIC_DIVISOR) as u16;
    }

    pub fn underlay_ip(&self) -> IpAddr {
        self.inner.stream_ip
    }

    pub fn time_last_received_ihu(&self) -> tokio::time::Instant {
        self.inner.state.read().unwrap().time_last_received_ihu
    }

    pub fn set_time_last_received_ihu(&self, time: tokio::time::Instant) {
        self.inner.state.write().unwrap().time_last_received_ihu = time
    }

    /// Checks if the connection to this `Peer` is alive and useable. If it is not, this `Peer`
    /// instance is dead and should be disposed of.
    pub fn connection_alive(&self) -> bool {
        self.inner.state.read().unwrap().connection_alive
    }
}

impl PartialEq for Peer {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

#[derive(Debug)]
struct PeerInner {
    state: RwLock<PeerState>,
    to_peer_data: mpsc::UnboundedSender<DataPacket>,
    to_peer_control: mpsc::UnboundedSender<ControlPacket>,
    /// Used to identify peer based on its connection params
    stream_ip: IpAddr,
    // TODO: not needed
    overlay_ip: IpAddr,
}

#[derive(Debug)]
struct PeerState {
    hello_seqno: SeqNo,
    time_last_received_hello: tokio::time::Instant,
    link_cost: u16,
    time_last_received_ihu: tokio::time::Instant,
    connection_alive: bool,
}

impl PeerState {
    /// Create a new `PeerInner`, holding the mutable state of a [`Peer`]
    pub fn new() -> Self {
        // Initialize last_sent_hello_seqno to 0
        let hello_seqno = SeqNo::default();
        let link_cost = DEFAULT_LINK_COST;
        // Initialize time_last_received_hello to now
        let time_last_received_hello = tokio::time::Instant::now();
        // Initialiwe time_last_send_ihu
        let time_last_received_ihu = tokio::time::Instant::now();

        Self {
            hello_seqno,
            link_cost,
            time_last_received_ihu,
            time_last_received_hello,
            connection_alive: true,
        }
    }
}
