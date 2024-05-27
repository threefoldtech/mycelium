use std::{io, net::SocketAddr};

use tokio::net::TcpStream;

impl super::Connection for tokio_openssl::SslStream<TcpStream> {
    fn identifier(&self) -> Result<String, io::Error> {
        Ok(format!(
            "TLS {} <-> {}",
            self.get_ref().local_addr()?,
            self.get_ref().peer_addr()?
        ))
    }

    fn static_link_cost(&self) -> Result<u16, io::Error> {
        Ok(match self.get_ref().peer_addr()? {
            SocketAddr::V4(_) => super::PACKET_PROCESSING_COST_IP4_TCP,
            SocketAddr::V6(ip) if ip.ip().to_ipv4_mapped().is_some() => {
                super::PACKET_PROCESSING_COST_IP4_TCP
            }
            SocketAddr::V6(_) => super::PACKET_PROCESSING_COST_IP6_TCP,
        })
    }
}
