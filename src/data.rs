use std::{marker::PhantomData, net::Ipv6Addr};

use futures::{Sink, SinkExt, Stream, StreamExt};
use log::{debug, error, trace, warn};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{crypto::PacketBuffer, packet::DataPacket, router::Router};

/// Current version of the user data header.
const USER_DATA_VERSION: u8 = 1;

/// Type value indicating L3 data in the user data header.
const USER_DATA_L3_TYPE: u8 = 0;

/// Minimum size in bytes of an IPv6 header.
const IPV6_MIN_HEADER_SIZE: usize = 40;

/// Mask applied to the first byte of an IP header to extract the version.
const IP_VERSION_MASK: u8 = 0b1111_0000;

/// Version byte of an IP header indicating IPv6. Since the version is only 4 bits, the lower bits
/// must be masked first.
const IPV6_VERSION_BYTE: u8 = 0b0110_0000;

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
    T: Sink<PacketBuffer> + Send + Unpin + 'static,
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
            let mut packet = match packet {
                Err(e) => {
                    error!("Failed to read packet from TUN interface {e}");
                    continue;
                }
                Ok(packet) => packet,
            };

            trace!("Received packet from tun");

            // Parse an IPv6 header. We don't care about the full header in reality. What we want
            // to know is:
            // - This is an IPv6 header
            // - Source address
            // - Destination address
            // This translates to the following requirements:
            // - at least 40 bytes of data, as that is the minimum size of an IPv6 header
            // - first 4 bits (version) are the constant 6 (0b0110)
            // - src is byte 9-24 (8-23 0 indexed).
            // - dst is byte 25-40 (24-39 0 indexed).

            if packet.len() < IPV6_MIN_HEADER_SIZE {
                trace!("Packet can't contain an IPv6 header");
                continue;
            }

            if packet[0] & IP_VERSION_MASK != IPV6_VERSION_BYTE {
                trace!("Packet is not IPv6");
                continue;
            }

            let src_ip = Ipv6Addr::from(
                <&[u8] as TryInto<[u8; 16]>>::try_into(&packet[8..24])
                    .expect("Static range bounds on slice are correct length"),
            );
            let dst_ip = Ipv6Addr::from(
                <&[u8] as TryInto<[u8; 16]>>::try_into(&packet[24..40])
                    .expect("Static range bounds on slice are correct length"),
            );

            trace!("Received packet from TUN with dest addr: {:?}", dst_ip);

            // Check if destination address is in 200::/7 range
            // TODO: make variable?
            let first_byte = dst_ip.segments()[0] >> 8; // get the first byte
            if !(0x02..=0x3F).contains(&first_byte) {
                debug!("Dropping packet which is not destined for 200::/7");
                continue;
            }

            let mut header = packet.header_mut();
            header[0] = USER_DATA_VERSION;
            header[1] = USER_DATA_L3_TYPE;

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
            let decrypted_packet = match shared_secret.decrypt(data_packet.raw_data) {
                Ok(data) => data,
                Err(_) => {
                    log::debug!("Dropping data packet with invalid encrypted content");
                    continue;
                }
            };

            // Check header
            let header = decrypted_packet.header();
            if header[0] != USER_DATA_VERSION {
                trace!("Dropping decrypted packet with unknown header version");
                continue;
            }

            // Route based on packet type.
            if header[1] != USER_DATA_L3_TYPE {
                trace!("Dropping decrypted packet with unknown protocol type");
                continue;
            }

            if let Err(e) = l3_packet_sink.send(decrypted_packet).await {
                error!("Failed to send packet on local TUN interface: {e}",);
                continue;
            }
        }

        warn!("Extract loop from router to host ended");
    }
}
