use std::{marker::PhantomData, net::Ipv6Addr};

use etherparse::IpHeader;
use futures::{Sink, SinkExt, Stream, StreamExt};
use log::{debug, error, trace, warn};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{crypto::PacketBuffer, packet::DataPacket, router::Router};

/// The DataPlane manages forwarding/receiving of local data packets to the [`Router`], and the
/// encryption/decryption of them.
#[derive(Clone)]
pub struct DataPlane<S, T> {
    router: Router,
    _stream_marker: PhantomData<S>,
    _sink_marker: PhantomData<T>,
}

impl<S, T> DataPlane<S, T>
where
    S: Stream<Item = Result<PacketBuffer, std::io::Error>> + Send + Unpin + 'static,
    T: Sink<Vec<u8>> + Send + Unpin + 'static,
    T::Error: std::fmt::Display,
{
    /// Create a new `DataPlane` using the given [`Router`] for packet handling.
    ///
    /// `l3_packet_stream` is a stream of l3 packets from the host, usually read from a TUN interface.
    /// `l3_packet_sink` is a sink for l3 packets received from a romte, usually send to a TUN interface,
    pub fn new(
        router: Router,
        l3_packet_stream: S,
        l3_packet_sink: T,
        host_packet_source: UnboundedReceiver<DataPacket>,
    ) -> Self {
        tokio::spawn(Self::inject_l3_packet_loop(
            router.clone(),
            l3_packet_stream,
        ));
        tokio::spawn(Self::extract_l3_packet_loop(
            router.clone(),
            l3_packet_sink,
            host_packet_source,
        ));

        Self {
            router,
            _stream_marker: PhantomData,
            _sink_marker: PhantomData,
        }
    }

    /// Get a reference to the [`Router`] used.
    pub fn router(&self) -> &Router {
        &self.router
    }

    async fn inject_l3_packet_loop(router: Router, mut l3_packet_stream: S) {
        while let Some(packet) = l3_packet_stream.next().await {
            let packet = match packet {
                Err(e) => {
                    error!("Failed to read packet from TUN interface {e}");
                    continue;
                }
                Ok(packet) => packet,
            };

            trace!("Received packet from tun");
            let headers = match etherparse::IpHeader::from_slice(&packet) {
                Ok(header) => header,
                Err(e) => {
                    warn!("Could not parse IP header from tun packet: {e}");
                    continue;
                }
            };
            let (src_ip, dst_ip) = if let IpHeader::Version6(header, _) = headers.0 {
                (
                    Ipv6Addr::from(header.source),
                    Ipv6Addr::from(header.destination),
                )
            } else {
                debug!("Drop non ipv6 packet");
                continue;
            };

            trace!("Received packet from TUN with dest addr: {:?}", dst_ip);

            // Check if destination address is in 200::/7 range
            // TODO: make variable?
            let first_byte = dst_ip.segments()[0] >> 8; // get the first byte
            if !(0x02..=0x3F).contains(&first_byte) {
                debug!("Dropping packet which is not destined for 200::/7");
                continue;
            }

            // Get shared secret from node and dest address
            let shared_secret = match router.get_shared_secret_from_dest(dst_ip) {
                Some(ss) => ss,
                None => {
                    debug!(
                        "No entry found for destination address {}, dropping packet",
                        dst_ip
                    );
                    continue;
                }
            };

            // inject own pubkey
            router.route_packet(DataPacket {
                dst_ip,
                src_ip,
                raw_data: shared_secret.encrypt(packet),
            });
        }
        warn!("Data inject loop from host to router ended");
    }

    async fn extract_l3_packet_loop(
        router: Router,
        mut l3_packet_sink: T,
        mut host_packet_source: UnboundedReceiver<DataPacket>,
    ) {
        while let Some(data_packet) = host_packet_source.recv().await {
            // decrypt & send to TUN interface
            let shared_secret =
                if let Some(ss) = router.get_shared_secret_from_dest(data_packet.src_ip) {
                    ss
                } else {
                    trace!("Received packet from unknown sender");
                    continue;
                };
            let decrypted_raw_data = match shared_secret.decrypt(data_packet.raw_data) {
                Ok(data) => data,
                Err(_) => {
                    log::debug!("Dropping data packet with invalid encrypted content");
                    continue;
                }
            };

            if let Err(e) = l3_packet_sink.send(decrypted_raw_data).await {
                error!("Failed to send packet on local TUN interface: {e}",);
                continue;
            }
        }

        warn!("Extract loop from router to host ended");
    }
}
