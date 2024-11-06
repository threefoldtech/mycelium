use bytes::BytesMut;
use tokio_util::codec::{Decoder as _, Encoder as _};

use crate::packet::{self, Packet};

pub struct Sctp {
    stream: tokio_sctp::SctpStream,
}

impl Sctp {
    pub fn new(stream: tokio_sctp::SctpStream) -> Self {
        Self { stream }
    }
}

impl super::Connection for Sctp {
    async fn feed_data_packet(&mut self, packet: crate::packet::DataPacket) -> std::io::Result<()> {
        let mut codec = packet::Codec::new();
        let mut buffer = BytesMut::with_capacity(1500);
        codec.encode(Packet::DataPacket(packet), &mut buffer)?;

        self.stream
            .sendmsg(
                &buffer,
                None,
                &tokio_sctp::SendOptions {
                    ppid: 0,
                    // 1 -> SCTP_UNORDERED
                    flags: 1,
                    stream: 0,
                    ttl: 0,
                },
            )
            .await
            .map(|_| ())
    }

    async fn feed_control_packet(
        &mut self,
        packet: crate::packet::ControlPacket,
    ) -> std::io::Result<()> {
        let mut codec = packet::Codec::new();
        let mut buffer = BytesMut::with_capacity(1500);
        codec.encode(Packet::ControlPacket(packet), &mut buffer)?;

        self.stream
            .sendmsg(
                &buffer,
                None,
                &tokio_sctp::SendOptions {
                    ppid: 0,
                    flags: 0,
                    stream: 1,
                    ttl: 0,
                },
            )
            .await
            .map(|_| ())
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    async fn receive_packet(&mut self) -> Option<std::io::Result<crate::packet::Packet>> {
        let mut buffer = BytesMut::with_capacity(1500);
        let (size, info, flags) = match self.stream.recvmsg_buf(&mut buffer).await {
            Ok(r) => r,
            Err(e) => return Some(Err(e)),
        };

        let mut codec = packet::Codec::new();
        match codec.decode(&mut buffer) {
            Ok(Some(packet)) => Some(Ok(packet)),
            // Partial? packet read. We consider this to be a stream hangup
            // TODO: verify
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }

    fn identifier(&self) -> Result<String, std::io::Error> {
        Ok(format!(
            "SCTP: {} <-> {}",
            self.stream.local_addr()?,
            self.stream.peer_addr()?
        ))
    }

    fn static_link_cost(&self) -> Result<u16, std::io::Error> {
        Ok(20)
    }
}
