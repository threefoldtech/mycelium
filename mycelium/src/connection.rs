use std::{io, net::SocketAddr, pin::Pin};

use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};

mod tracked;
pub use tracked::Tracked;

#[cfg(feature = "private-network")]
mod tls;

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

pub trait Connection: AsyncRead + AsyncWrite {
    /// Get an identifier for this connection, which shows details about the remote
    fn identifier(&self) -> Result<String, io::Error>;

    /// The static cost of using this connection
    fn static_link_cost(&self) -> Result<u16, io::Error>;
}

/// A wrapper around a quic send and quic receive stream, implementing the [`Connection`] trait.
pub struct Quic {
    tx: quinn::SendStream,
    rx: quinn::RecvStream,
    con: quinn::Connection,
}

impl Quic {
    /// Create a new wrapper around Quic streams.
    pub fn new(tx: quinn::SendStream, rx: quinn::RecvStream, con: quinn::Connection) -> Self {
        Quic { tx, rx, con }
    }
}

impl Connection for TcpStream {
    fn identifier(&self) -> Result<String, io::Error> {
        Ok(format!(
            "TCP {} <-> {}",
            self.local_addr()?,
            self.peer_addr()?
        ))
    }

    fn static_link_cost(&self) -> Result<u16, io::Error> {
        Ok(match self.peer_addr()? {
            SocketAddr::V4(_) => PACKET_PROCESSING_COST_IP4_TCP,
            SocketAddr::V6(ip) if ip.ip().to_ipv4_mapped().is_some() => {
                PACKET_PROCESSING_COST_IP4_TCP
            }
            SocketAddr::V6(_) => PACKET_PROCESSING_COST_IP6_TCP,
        })
    }
}

impl AsyncRead for Quic {
    #[inline]
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        Pin::new(&mut self.rx).poll_read(cx, buf)
    }
}

impl AsyncWrite for Quic {
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
}

#[cfg(test)]
use tokio::io::DuplexStream;

#[cfg(test)]
impl Connection for DuplexStream {
    fn identifier(&self) -> Result<String, io::Error> {
        Ok("Memory pipe".to_string())
    }

    fn static_link_cost(&self) -> Result<u16, io::Error> {
        Ok(1)
    }
}
