use std::{
    io,
    sync::{atomic::AtomicU64, Arc},
};

use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use tokio_util::codec::Framed;

use crate::{
    connection::Tracked,
    packet::{self, ControlPacket, DataPacket, Packet},
};

/// Cost of using a vsock connection. Vsock is a local virtual channel between hypervisor and VM,
/// so it has a lower cost than any IP-based transport.
const PACKET_PROCESSING_COST_VSOCK: u16 = 5;

/// A wrapper around a vsock stream implementing the [`Connection`](super::Connection) trait.
pub struct VsockStream {
    framed: Framed<Tracked<tokio_vsock::VsockStream>, packet::Codec>,
    local_addr: tokio_vsock::VsockAddr,
    peer_addr: tokio_vsock::VsockAddr,
}

impl VsockStream {
    /// Create a new wrapped [`VsockStream`] which implements the [`Connection`](super::Connection) trait.
    pub fn new(
        stream: tokio_vsock::VsockStream,
        read: Arc<AtomicU64>,
        write: Arc<AtomicU64>,
    ) -> io::Result<Self> {
        Ok(Self {
            local_addr: stream.local_addr()?,
            peer_addr: stream.peer_addr()?,
            framed: Framed::new(Tracked::new(read, write, stream), packet::Codec::new()),
        })
    }
}

impl super::Connection for VsockStream {
    type ReadHalf = VsockStreamReadHalf;
    type WriteHalf = VsockStreamWriteHalf;

    async fn feed_data_packet(&mut self, packet: DataPacket) -> io::Result<()> {
        self.framed.feed(Packet::DataPacket(packet)).await
    }

    async fn feed_control_packet(&mut self, packet: ControlPacket) -> io::Result<()> {
        self.framed.feed(Packet::ControlPacket(packet)).await
    }

    async fn receive_packet(&mut self) -> Option<io::Result<Packet>> {
        self.framed.next().await
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.framed.flush().await
    }

    fn identifier(&self) -> Result<String, io::Error> {
        Ok(format!(
            "VSOCK {}:{} <-> {}:{}",
            self.local_addr.cid(),
            self.local_addr.port(),
            self.peer_addr.cid(),
            self.peer_addr.port(),
        ))
    }

    fn static_link_cost(&self) -> Result<u16, io::Error> {
        Ok(PACKET_PROCESSING_COST_VSOCK)
    }

    fn split(self) -> (Self::ReadHalf, Self::WriteHalf) {
        let (tx, rx) = self.framed.split();
        (
            VsockStreamReadHalf { framed: rx },
            VsockStreamWriteHalf { framed: tx },
        )
    }
}

pub struct VsockStreamReadHalf {
    framed: SplitStream<Framed<Tracked<tokio_vsock::VsockStream>, packet::Codec>>,
}

impl super::ConnectionReadHalf for VsockStreamReadHalf {
    async fn receive_packet(&mut self) -> Option<io::Result<Packet>> {
        self.framed.next().await
    }
}

pub struct VsockStreamWriteHalf {
    framed: SplitSink<Framed<Tracked<tokio_vsock::VsockStream>, packet::Codec>, packet::Packet>,
}

impl super::ConnectionWriteHalf for VsockStreamWriteHalf {
    async fn feed_data_packet(&mut self, packet: DataPacket) -> io::Result<()> {
        self.framed.feed(Packet::DataPacket(packet)).await
    }

    async fn feed_control_packet(&mut self, packet: ControlPacket) -> io::Result<()> {
        self.framed.feed(Packet::ControlPacket(packet)).await
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.framed.flush().await
    }
}
