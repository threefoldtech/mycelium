use std::{io, net::SocketAddr};

use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};

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

pub trait Connection: AsyncRead + AsyncWrite {
    /// Get an identifier for this connection, which shows details about the remote
    fn identifier(&self) -> Result<String, io::Error>;

    /// The static cost of using this connection
    fn static_link_cost(&self) -> Result<u16, io::Error>;
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
