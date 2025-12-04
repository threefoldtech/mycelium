use std::{
    io,
    net::SocketAddr,
    sync::{atomic::AtomicU64, Arc},
};

use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

use crate::{
    connection::Tracked,
    packet::{self, Packet},
};

/// A wrapper around an asynchronous TLS stream.
pub struct TlsStream {
    framed: Framed<Tracked<tokio_openssl::SslStream<TcpStream>>, packet::Codec>,
    local_addr: SocketAddr,
    peer_addr: SocketAddr,
}

impl TlsStream {
    /// Create a new wrapped [`TlsStream`] which implements the [`Connection`](super::Connection) trait.
    pub fn new(
        tls_stream: tokio_openssl::SslStream<TcpStream>,
        read: Arc<AtomicU64>,
        write: Arc<AtomicU64>,
    ) -> io::Result<Self> {
        Ok(Self {
            local_addr: tls_stream.get_ref().local_addr()?,
            peer_addr: tls_stream.get_ref().peer_addr()?,
            framed: Framed::new(Tracked::new(read, write, tls_stream), packet::Codec::new()),
        })
    }
}

impl super::Connection for TlsStream {
    type ReadHalf = TlsStreamReadHalf;
    type WriteHalf = TlsStreamWriteHalf;

    async fn feed_data_packet(&mut self, packet: crate::packet::DataPacket) -> io::Result<()> {
        self.framed.feed(Packet::DataPacket(packet)).await
    }

    async fn feed_control_packet(
        &mut self,
        packet: crate::packet::ControlPacket,
    ) -> io::Result<()> {
        self.framed.feed(Packet::ControlPacket(packet)).await
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.framed.flush().await
    }

    async fn receive_packet(&mut self) -> Option<io::Result<crate::packet::Packet>> {
        self.framed.next().await
    }

    fn identifier(&self) -> Result<String, io::Error> {
        Ok(format!("TLS {} <-> {}", self.local_addr, self.peer_addr))
    }

    fn static_link_cost(&self) -> Result<u16, io::Error> {
        Ok(match self.peer_addr {
            SocketAddr::V4(_) => super::PACKET_PROCESSING_COST_IP4_TCP,
            SocketAddr::V6(ip) if ip.ip().to_ipv4_mapped().is_some() => {
                super::PACKET_PROCESSING_COST_IP4_TCP
            }
            SocketAddr::V6(_) => super::PACKET_PROCESSING_COST_IP6_TCP,
        })
    }

    fn split(self) -> (Self::ReadHalf, Self::WriteHalf) {
        let (tx, rx) = self.framed.split();

        (
            TlsStreamReadHalf { framed: rx },
            TlsStreamWriteHalf { framed: tx },
        )
    }
}

pub struct TlsStreamReadHalf {
    framed: SplitStream<Framed<Tracked<tokio_openssl::SslStream<TcpStream>>, packet::Codec>>,
}

pub struct TlsStreamWriteHalf {
    framed: SplitSink<
        Framed<Tracked<tokio_openssl::SslStream<TcpStream>>, packet::Codec>,
        packet::Packet,
    >,
}

impl super::ConnectionReadHalf for TlsStreamReadHalf {
    async fn receive_packet(&mut self) -> Option<io::Result<crate::packet::Packet>> {
        self.framed.next().await
    }
}

impl super::ConnectionWriteHalf for TlsStreamWriteHalf {
    async fn feed_data_packet(&mut self, packet: crate::packet::DataPacket) -> io::Result<()> {
        self.framed.feed(Packet::DataPacket(packet)).await
    }

    async fn feed_control_packet(
        &mut self,
        packet: crate::packet::ControlPacket,
    ) -> io::Result<()> {
        self.framed.feed(Packet::ControlPacket(packet)).await
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.framed.flush().await
    }
}
