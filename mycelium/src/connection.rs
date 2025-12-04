use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use crate::packet::{self, ControlPacket, DataPacket, Packet};

use bytes::{Bytes, BytesMut};
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use tokio::io::{AsyncRead, AsyncWrite};

mod tracked;
use tokio_util::codec::{Decoder, Encoder, Framed};
pub use tracked::Tracked;

#[cfg(feature = "private-network")]
pub mod tls;

/// Cost to add to the peer_link_cost for "local processing", when peers are connected over IPv6.
///
/// The current peer link cost is calculated from a HELLO rtt. This is great to measure link
/// latency, since packets are processed in order. However, on local idle links, this value will
/// likely be 0 since we round down (from the amount of ms it took to process), which does not
/// accurately reflect the fact that there is in fact a cost associated with using a peer, even on
/// these local links.
const PACKET_PROCESSING_COST_IP6_TCP: u16 = 10;

/// Cost to add to the peer_link_cost for "local processing", when peers are connected over IPv6.
///
/// This is similar to [`PACKET_PROCESSING_COST_IP6`], but slightly higher so we skew towards IPv6
/// connections if peers are connected over both IPv4 and IPv6.
const PACKET_PROCESSING_COST_IP4_TCP: u16 = 15;

// TODO
const PACKET_PROCESSING_COST_IP6_QUIC: u16 = 7;
// TODO
const PACKET_PROCESSING_COST_IP4_QUIC: u16 = 12;

pub trait ConnectionReadHalf: Send {
    /// Receive a packet from the remote end.
    fn receive_packet(&mut self) -> impl Future<Output = Option<io::Result<Packet>>> + Send;
}

pub trait ConnectionWriteHalf: Send {
    /// Feeds a data packet on the connection. Depending on the connection you might need to call
    /// [`Connection::flush`] before the packet is actually sent.
    fn feed_data_packet(
        &mut self,
        packet: DataPacket,
    ) -> impl Future<Output = io::Result<()>> + Send;

    /// Feeds a control packet on the connection. Depending on the connection you might need to call
    /// [`Connection::flush`] before the packet is actually sent.
    fn feed_control_packet(
        &mut self,
        packet: ControlPacket,
    ) -> impl Future<Output = io::Result<()>> + Send;

    /// Flush the connection. This sends all buffered packets which haven't beend sent yet.
    fn flush(&mut self) -> impl Future<Output = io::Result<()>> + Send;
}

pub trait Connection {
    type ReadHalf: ConnectionReadHalf;
    type WriteHalf: ConnectionWriteHalf;

    /// Feeds a data packet on the connection. Depending on the connection you might need to call
    /// [`Connection::flush`] before the packet is actually sent.
    fn feed_data_packet(
        &mut self,
        packet: DataPacket,
    ) -> impl Future<Output = io::Result<()>> + Send;

    /// Feeds a control packet on the connection. Depending on the connection you might need to call
    /// [`Connection::flush`] before the packet is actually sent.
    fn feed_control_packet(
        &mut self,
        packet: ControlPacket,
    ) -> impl Future<Output = io::Result<()>> + Send;

    /// Flush the connection. This sends all buffered packets which haven't beend sent yet.
    fn flush(&mut self) -> impl Future<Output = io::Result<()>> + Send;

    /// Receive a packet from the remote end.
    fn receive_packet(&mut self) -> impl Future<Output = Option<io::Result<Packet>>> + Send;

    /// Get an identifier for this connection, which shows details about the remote
    fn identifier(&self) -> Result<String, io::Error>;

    /// The static cost of using this connection
    fn static_link_cost(&self) -> Result<u16, io::Error>;

    /// Split the connection in a read and write half which can be used independently
    fn split(self) -> (Self::ReadHalf, Self::WriteHalf);
}

/// A wrapper about an asynchronous (non blocking) tcp stream.
pub struct TcpStream {
    framed: Framed<Tracked<tokio::net::TcpStream>, packet::Codec>,
    local_addr: SocketAddr,
    peer_addr: SocketAddr,
}

impl TcpStream {
    /// Create a new wrapped [`TcpStream`] which implements the [`Connection`] trait.
    pub fn new(
        tcp_stream: tokio::net::TcpStream,
        read: Arc<AtomicU64>,
        write: Arc<AtomicU64>,
    ) -> io::Result<Self> {
        Ok(Self {
            local_addr: tcp_stream.local_addr()?,
            peer_addr: tcp_stream.peer_addr()?,
            framed: Framed::new(Tracked::new(read, write, tcp_stream), packet::Codec::new()),
        })
    }
}

impl Connection for TcpStream {
    type ReadHalf = TcpStreamReadHalf;
    type WriteHalf = TcpStreamWriteHalf;

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
        Ok(format!("TCP {} <-> {}", self.local_addr, self.peer_addr))
    }

    fn static_link_cost(&self) -> Result<u16, io::Error> {
        Ok(match self.peer_addr {
            SocketAddr::V4(_) => PACKET_PROCESSING_COST_IP4_TCP,
            SocketAddr::V6(ip) if ip.ip().to_ipv4_mapped().is_some() => {
                PACKET_PROCESSING_COST_IP4_TCP
            }
            SocketAddr::V6(_) => PACKET_PROCESSING_COST_IP6_TCP,
        })
    }

    fn split(self) -> (Self::ReadHalf, Self::WriteHalf) {
        let (tx, rx) = self.framed.split();

        (
            TcpStreamReadHalf { framed: rx },
            TcpStreamWriteHalf { framed: tx },
        )
    }
}

pub struct TcpStreamReadHalf {
    framed: SplitStream<Framed<Tracked<tokio::net::TcpStream>, packet::Codec>>,
}

impl ConnectionReadHalf for TcpStreamReadHalf {
    async fn receive_packet(&mut self) -> Option<io::Result<Packet>> {
        self.framed.next().await
    }
}

pub struct TcpStreamWriteHalf {
    framed: SplitSink<Framed<Tracked<tokio::net::TcpStream>, packet::Codec>, packet::Packet>,
}

impl ConnectionWriteHalf for TcpStreamWriteHalf {
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

/// A wrapper around a quic send and quic receive stream, implementing the [`Connection`] trait.
pub struct Quic {
    framed: Framed<Tracked<QuicStream>, packet::Codec>,
    con: quinn::Connection,
    read: Arc<AtomicU64>,
    write: Arc<AtomicU64>,
}

struct QuicStream {
    tx: quinn::SendStream,
    rx: quinn::RecvStream,
}

impl Quic {
    /// Create a new wrapper around Quic streams.
    pub fn new(
        tx: quinn::SendStream,
        rx: quinn::RecvStream,
        con: quinn::Connection,
        read: Arc<AtomicU64>,
        write: Arc<AtomicU64>,
    ) -> Self {
        Quic {
            framed: Framed::new(
                Tracked::new(read.clone(), write.clone(), QuicStream { tx, rx }),
                packet::Codec::new(),
            ),
            con,
            read,
            write,
        }
    }
}

impl AsyncRead for QuicStream {
    #[inline]
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        Pin::new(&mut self.rx).poll_read(cx, buf)
    }
}

impl AsyncWrite for QuicStream {
    #[inline]
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.tx)
            .poll_write(cx, buf)
            .map_err(From::from)
    }

    #[inline]
    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), io::Error>> {
        Pin::new(&mut self.tx).poll_flush(cx)
    }

    #[inline]
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), io::Error>> {
        Pin::new(&mut self.tx).poll_shutdown(cx)
    }

    #[inline]
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> std::task::Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.tx).poll_write_vectored(cx, bufs)
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        self.tx.is_write_vectored()
    }
}

impl Connection for Quic {
    type ReadHalf = QuicReadHalf;

    type WriteHalf = QuicWriteHalf;

    async fn feed_data_packet(&mut self, packet: DataPacket) -> io::Result<()> {
        let mut codec = packet::Codec::new();
        let mut buffer = BytesMut::with_capacity(1500);
        codec.encode(Packet::DataPacket(packet), &mut buffer)?;

        let data: Bytes = buffer.into();
        let tx_len = data.len();
        self.write.fetch_add(tx_len as u64, Ordering::Relaxed);

        self.con.send_datagram(data).map_err(io::Error::other)
    }

    async fn feed_control_packet(&mut self, packet: ControlPacket) -> io::Result<()> {
        self.framed.feed(Packet::ControlPacket(packet)).await
    }

    async fn receive_packet(&mut self) -> Option<io::Result<Packet>> {
        tokio::select! {
            datagram = self.con.read_datagram() => {
                let datagram_bytes = match datagram {
                    Ok(buffer) => buffer,
                    Err(e) => return Some(Err(e.into())),
                };
                let recv_len = datagram_bytes.len();
                self.read.fetch_add(recv_len as u64, Ordering::Relaxed);
                let mut codec = packet::Codec::new();
                match codec.decode(&mut datagram_bytes.into()) {
                    Ok(Some(packet)) => Some(Ok(packet)),
                    // Partial? packet read. We consider this to be a stream hangup
                    // TODO: verify
                    Ok(None) => None,
                    Err(e) => Some(Err(e)),
                }
            },
            packet = self.framed.next() => {
                packet
            }

        }
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.framed.flush().await
    }

    fn identifier(&self) -> Result<String, io::Error> {
        Ok(format!("QUIC -> {}", self.con.remote_address()))
    }

    fn static_link_cost(&self) -> Result<u16, io::Error> {
        Ok(match self.con.remote_address() {
            SocketAddr::V4(_) => PACKET_PROCESSING_COST_IP4_QUIC,
            SocketAddr::V6(ip) if ip.ip().to_ipv4_mapped().is_some() => {
                PACKET_PROCESSING_COST_IP4_QUIC
            }
            SocketAddr::V6(_) => PACKET_PROCESSING_COST_IP6_QUIC,
        })
    }

    fn split(self) -> (Self::ReadHalf, Self::WriteHalf) {
        let Self {
            framed,
            con,
            read,
            write,
        } = self;

        let (tx, rx) = framed.split();

        (
            QuicReadHalf {
                framed: rx,
                con: con.clone(),
                read,
            },
            QuicWriteHalf {
                framed: tx,
                con,
                write,
            },
        )
    }
}

pub struct QuicReadHalf {
    framed: SplitStream<Framed<Tracked<QuicStream>, packet::Codec>>,
    con: quinn::Connection,
    read: Arc<AtomicU64>,
}

pub struct QuicWriteHalf {
    framed: SplitSink<Framed<Tracked<QuicStream>, packet::Codec>, packet::Packet>,
    con: quinn::Connection,
    write: Arc<AtomicU64>,
}

impl ConnectionReadHalf for QuicReadHalf {
    async fn receive_packet(&mut self) -> Option<io::Result<Packet>> {
        tokio::select! {
            datagram = self.con.read_datagram() => {
                let datagram_bytes = match datagram {
                    Ok(buffer) => buffer,
                    Err(e) => return Some(Err(e.into())),
                };
                let recv_len = datagram_bytes.len();
                self.read.fetch_add(recv_len as u64, Ordering::Relaxed);
                let mut codec = packet::Codec::new();
                match codec.decode(&mut datagram_bytes.into()) {
                    Ok(Some(packet)) => Some(Ok(packet)),
                    // Partial? packet read. We consider this to be a stream hangup
                    // TODO: verify
                    Ok(None) => None,
                    Err(e) => Some(Err(e)),
                }
            },
            packet = self.framed.next() => {
                packet
            }

        }
    }
}

impl ConnectionWriteHalf for QuicWriteHalf {
    async fn feed_data_packet(&mut self, packet: DataPacket) -> io::Result<()> {
        let mut codec = packet::Codec::new();
        let mut buffer = BytesMut::with_capacity(1500);
        codec.encode(Packet::DataPacket(packet), &mut buffer)?;

        let data: Bytes = buffer.into();
        let tx_len = data.len();
        self.write.fetch_add(tx_len as u64, Ordering::Relaxed);

        self.con.send_datagram(data).map_err(io::Error::other)
    }

    async fn feed_control_packet(&mut self, packet: ControlPacket) -> io::Result<()> {
        self.framed.feed(Packet::ControlPacket(packet)).await
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.framed.flush().await
    }
}

#[cfg(test)]
/// Wrapper for an in-memory pipe implementing the [`Connection`] trait.
pub struct DuplexStream {
    framed: Framed<tokio::io::DuplexStream, packet::Codec>,
}

#[cfg(test)]
impl DuplexStream {
    /// Create a new in memory duplex stream.
    pub fn new(duplex: tokio::io::DuplexStream) -> Self {
        Self {
            framed: Framed::new(duplex, packet::Codec::new()),
        }
    }
}

#[cfg(test)]
impl Connection for DuplexStream {
    type ReadHalf = DuplexStreamReadHalf;
    type WriteHalf = DuplexStreamWriteHalf;

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
        Ok("Memory pipe".to_string())
    }

    fn static_link_cost(&self) -> Result<u16, io::Error> {
        Ok(1)
    }

    fn split(self) -> (Self::ReadHalf, Self::WriteHalf) {
        let (tx, rx) = self.framed.split();

        (
            DuplexStreamReadHalf { framed: rx },
            DuplexStreamWriteHalf { framed: tx },
        )
    }
}

#[cfg(test)]
pub struct DuplexStreamReadHalf {
    framed: SplitStream<Framed<tokio::io::DuplexStream, packet::Codec>>,
}

#[cfg(test)]
pub struct DuplexStreamWriteHalf {
    framed: SplitSink<Framed<tokio::io::DuplexStream, packet::Codec>, packet::Packet>,
}

#[cfg(test)]
impl ConnectionReadHalf for DuplexStreamReadHalf {
    async fn receive_packet(&mut self) -> Option<io::Result<Packet>> {
        self.framed.next().await
    }
}

#[cfg(test)]
impl ConnectionWriteHalf for DuplexStreamWriteHalf {
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
