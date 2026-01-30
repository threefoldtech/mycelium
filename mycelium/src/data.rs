use std::net::{IpAddr, Ipv6Addr};

use etherparse::{
    icmpv6::{DestUnreachableCode, TimeExceededCode},
    Icmpv6Type, PacketBuilder,
};
use futures::{Sink, SinkExt, Stream, StreamExt};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, error, trace, warn};

use crate::{
    crypto::PacketBuffer,
    metrics::Metrics,
    packet::DataPacket,
    packet_queue::{IncomingPacketQueue, UnencryptedPacket},
    router::{Router, SharedSecretResult},
    subnet::Subnet,
};

/// Current version of the user data header.
const USER_DATA_VERSION: u8 = 1;

/// Type value indicating L3 data in the user data header.
const USER_DATA_L3_TYPE: u8 = 0;

/// Type value indicating a user message in the data header.
const USER_DATA_MESSAGE_TYPE: u8 = 1;

/// Type value indicating an ICMP packet not returned as regular IPv6 traffic. This is needed when
/// intermediate nodes send back icmp data, as the original data is encrypted.
const USER_DATA_OOB_ICMP: u8 = 2;

/// Minimum size in bytes of an IPv6 header.
const IPV6_MIN_HEADER_SIZE: usize = 40;

/// Size of an ICMPv6 header.
const ICMP6_HEADER_SIZE: usize = 8;

/// Minimum MTU for IPV6 according to https://www.rfc-editor.org/rfc/rfc8200#section-5.
/// For ICMP, the packet must not be greater than this value. This is specified in
/// https://datatracker.ietf.org/doc/html/rfc4443#section-2.4, section (c).
const MIN_IPV6_MTU: usize = 1280;

/// Mask applied to the first byte of an IP header to extract the version.
const IP_VERSION_MASK: u8 = 0b1111_0000;

/// Version byte of an IP header indicating IPv6. Since the version is only 4 bits, the lower bits
/// must be masked first.
const IPV6_VERSION_BYTE: u8 = 0b0110_0000;

/// Default hop limit for message packets. For now this is set to 64 hops.
///
/// For regular l3 packets, we copy the hop limit from the packet itself. We can't do that here, so
/// 64 is used as sane default.
const MESSAGE_HOP_LIMIT: u8 = 64;

/// The DataPlane manages forwarding/receiving of local data packets to the [`Router`], and the
/// encryption/decryption of them.
///
/// DataPlane itself can be cloned, but this is not cheap on the router and should be avoided.
pub struct DataPlane<M> {
    router: Router<M>,
}

impl<M> DataPlane<M>
where
    M: Metrics + Clone + Send + 'static,
{
    /// Create a new `DataPlane` using the given [`Router`] for packet handling.
    ///
    /// `l3_packet_stream` is a stream of l3 packets from the host, usually read from a TUN interface.
    /// `l3_packet_sink` is a sink for l3 packets received from a romte, usually send to a TUN interface,
    /// `pending_packet_rx` is a receiver for unencrypted packets that were queued waiting for route
    /// discovery and are now ready to be encrypted and routed.
    /// `timeout_packet_rx` is a receiver for unencrypted packets that timed out waiting for route
    /// discovery and need ICMP unreachable responses.
    /// `incoming_packet_queue` is the queue for incoming encrypted packets waiting for the sender's route.
    /// `incoming_route_rx` is a receiver for notifications when routes become available.
    pub fn new<S, T, U>(
        router: Router<M>,
        l3_packet_stream: S,
        l3_packet_sink: T,
        message_packet_sink: U,
        host_packet_source: UnboundedReceiver<DataPacket>,
        pending_packet_rx: UnboundedReceiver<UnencryptedPacket>,
        timeout_packet_rx: UnboundedReceiver<UnencryptedPacket>,
        incoming_packet_queue: IncomingPacketQueue,
        incoming_route_rx: UnboundedReceiver<Subnet>,
    ) -> Self
    where
        S: Stream<Item = Result<PacketBuffer, std::io::Error>> + Send + Unpin + 'static,
        T: Sink<PacketBuffer> + Clone + Send + Unpin + 'static,
        T::Error: std::fmt::Display,
        U: Sink<(PacketBuffer, IpAddr, IpAddr)> + Send + Unpin + 'static,
        U::Error: std::fmt::Display,
    {
        let dp = Self { router };

        tokio::spawn(
            dp.clone()
                .inject_l3_packet_loop(l3_packet_stream, l3_packet_sink.clone()),
        );
        tokio::spawn(dp.clone().extract_packet_loop(
            l3_packet_sink.clone(),
            message_packet_sink,
            host_packet_source,
            incoming_packet_queue,
            incoming_route_rx,
        ));
        tokio::spawn(
            dp.clone()
                .process_pending_packets_loop(l3_packet_sink.clone(), pending_packet_rx),
        );
        tokio::spawn(
            dp.clone()
                .process_timeout_packets_loop(l3_packet_sink, timeout_packet_rx),
        );

        dp
    }

    /// Get a reference to the [`Router`] used.
    pub fn router(&self) -> &Router<M> {
        &self.router
    }

    async fn inject_l3_packet_loop<S, T>(self, mut l3_packet_stream: S, mut l3_packet_sink: T)
    where
        // TODO: no result
        // TODO: should IP extraction be handled higher up?
        S: Stream<Item = Result<PacketBuffer, std::io::Error>> + Send + Unpin + 'static,
        T: Sink<PacketBuffer> + Clone + Send + Unpin + 'static,
        T::Error: std::fmt::Display,
    {
        let node_subnet = self.router.node_tun_subnet();

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
            // - Hop limit
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

            let hop_limit = u8::from_be_bytes([packet[7]]);

            let src_ip = Ipv6Addr::from(
                <&[u8] as TryInto<[u8; 16]>>::try_into(&packet[8..24])
                    .expect("Static range bounds on slice are correct length"),
            );
            let dst_ip = Ipv6Addr::from(
                <&[u8] as TryInto<[u8; 16]>>::try_into(&packet[24..40])
                    .expect("Static range bounds on slice are correct length"),
            );

            // If this is a packet for our own Subnet, it means there is no local configuration for
            // the destination ip or /64 subnet, and the IP is unreachable
            if node_subnet.contains_ip(dst_ip.into()) {
                trace!(
                    "Replying to local packet for unexisting address: {}",
                    dst_ip
                );

                let mut icmp_packet = PacketBuffer::new();
                let host = self.router.node_public_key().address().octets();
                let icmp = PacketBuilder::ipv6(host, src_ip.octets(), 64).icmpv6(
                    Icmpv6Type::DestinationUnreachable(DestUnreachableCode::Address),
                );
                icmp_packet.set_size(icmp.size(packet.len().min(1280 - 48)));
                let mut writer = &mut icmp_packet.buffer_mut()[..];
                if let Err(e) = icmp.write(&mut writer, &packet[..packet.len().min(1280 - 48)]) {
                    error!("Failed to construct ICMP packet: {e}");
                    continue;
                }
                if let Err(e) = l3_packet_sink.send(icmp_packet).await {
                    error!("Failed to send ICMP packet to host: {e}");
                }
                continue;
            }

            trace!("Received packet from TUN with dest addr: {:?}", dst_ip);
            // Check if the source address is part of 400::/7
            let first_src_byte = src_ip.segments()[0] >> 8;
            if !(0x04..0x06).contains(&first_src_byte) {
                let mut icmp_packet = PacketBuffer::new();
                let host = self.router.node_public_key().address().octets();
                let icmp = PacketBuilder::ipv6(host, src_ip.octets(), 64).icmpv6(
                    Icmpv6Type::DestinationUnreachable(
                        DestUnreachableCode::SourceAddressFailedPolicy,
                    ),
                );
                icmp_packet.set_size(icmp.size(packet.len().min(1280 - 48)));
                let mut writer = &mut icmp_packet.buffer_mut()[..];
                if let Err(e) = icmp.write(&mut writer, &packet[..packet.len().min(1280 - 48)]) {
                    error!("Failed to construct ICMP packet: {e}");
                    continue;
                }
                if let Err(e) = l3_packet_sink.send(icmp_packet).await {
                    error!("Failed to send ICMP packet to host: {e}");
                }
                continue;
            }

            // No need to verify destination address, if it is not part of the global subnet there
            // should not be a route for it, and therefore the route step will generate the
            // appropriate ICMP.

            let mut header = packet.header_mut();
            header[0] = USER_DATA_VERSION;
            header[1] = USER_DATA_L3_TYPE;

            if let Some(icmp) = self.encrypt_and_route_packet(src_ip, dst_ip, hop_limit, packet) {
                if let Err(e) = l3_packet_sink.send(icmp).await {
                    error!("Could not forward icmp packet back to TUN interface {e}");
                }
            }
        }

        warn!("Data inject loop from host to router ended");
    }

    /// Inject a new packet where the content is a `message` fragment.
    pub fn inject_message_packet(
        &self,
        src_ip: Ipv6Addr,
        dst_ip: Ipv6Addr,
        mut packet: PacketBuffer,
    ) {
        let mut header = packet.header_mut();
        header[0] = USER_DATA_VERSION;
        header[1] = USER_DATA_MESSAGE_TYPE;

        self.encrypt_and_route_packet(src_ip, dst_ip, MESSAGE_HOP_LIMIT, packet);
    }

    /// Encrypt the content of a packet based on the destination key, and then inject the packet
    /// into the [`Router`] for processing.
    ///
    /// If no key exists for the destination yet (route is being resolved), the packet is queued
    /// and will be processed when the route becomes available.
    ///
    /// If no route exists (NoRoute), a packet is returned containing an ICMP packet.
    /// Note that a return value of [`Option::None`] does not mean the packet was successfully
    /// forwarded - it may have been queued.
    fn encrypt_and_route_packet(
        &self,
        src_ip: Ipv6Addr,
        dst_ip: Ipv6Addr,
        hop_limit: u8,
        packet: PacketBuffer,
    ) -> Option<PacketBuffer> {
        // If the packet only has a TTL of 1, we won't be able to route it to the destination
        // regardless, so just reply with an unencrypted TTL exceeded ICMP.
        if hop_limit < 2 {
            debug!(
                packet.ttl = hop_limit,
                packet.src = %src_ip,
                packet.dst = %dst_ip,
                "Attempting to route packet with insufficient TTL",
            );

            let mut pb = PacketBuffer::new();
            // From self to self
            let icmp = PacketBuilder::ipv6(src_ip.octets(), src_ip.octets(), hop_limit)
                .icmpv6(Icmpv6Type::TimeExceeded(TimeExceededCode::HopLimitExceeded));
            // Scale to max size if needed
            let orig_buf_end = packet
                .buffer()
                .len()
                .min(MIN_IPV6_MTU - IPV6_MIN_HEADER_SIZE - ICMP6_HEADER_SIZE);
            pb.set_size(icmp.size(orig_buf_end));
            let mut b = pb.buffer_mut();
            if let Err(e) = icmp.write(&mut b, &packet.buffer()[..orig_buf_end]) {
                error!("Failed to construct time exceeded ICMP packet {e}");
                return None;
            }

            return Some(pb);
        }

        // Get shared secret from node and dest address
        match self.router.get_shared_secret_if_selected(dst_ip.into()) {
            SharedSecretResult::Found(shared_secret) => {
                self.router.route_packet(DataPacket {
                    dst_ip,
                    src_ip,
                    hop_limit,
                    raw_data: shared_secret.encrypt(packet),
                });
                None
            }
            SharedSecretResult::Resolving => {
                // Route is being resolved - queue the packet for later processing
                debug!(
                    packet.src = %src_ip,
                    packet.dst = %dst_ip,
                    "Route being resolved, queueing packet for later",
                );

                let subnet = Subnet::new(dst_ip.into(), 64)
                    .expect("64 is a valid subnet size for IPv6 addresses");

                let unencrypted_packet = UnencryptedPacket {
                    src_ip,
                    dst_ip,
                    hop_limit,
                    packet,
                };

                // Try to queue the packet - on timeout, it will be sent to the timeout channel
                if let Err(returned_packet) = self
                    .router
                    .enqueue_unencrypted_packet(subnet, unencrypted_packet)
                {
                    // Queue is full - generate ICMP
                    debug!(
                        packet.src = %src_ip,
                        packet.dst = %dst_ip,
                        "Packet queue full, dropping packet",
                    );

                    let mut pb = PacketBuffer::new();
                    let icmp = PacketBuilder::ipv6(src_ip.octets(), src_ip.octets(), hop_limit)
                        .icmpv6(Icmpv6Type::DestinationUnreachable(
                            DestUnreachableCode::NoRoute,
                        ));
                    let orig_buf_end = returned_packet
                        .packet
                        .buffer()
                        .len()
                        .min(MIN_IPV6_MTU - IPV6_MIN_HEADER_SIZE - ICMP6_HEADER_SIZE);
                    pb.set_size(icmp.size(orig_buf_end));
                    let mut b = pb.buffer_mut();
                    if let Err(e) =
                        icmp.write(&mut b, &returned_packet.packet.buffer()[..orig_buf_end])
                    {
                        error!("Failed to construct no route ICMP packet {e}");
                        return None;
                    }
                    return Some(pb);
                }

                // Packet queued successfully
                None
            }
            SharedSecretResult::NoRoute => {
                // No route exists - send ICMP unreachable
                debug!(
                    packet.src = %src_ip,
                    packet.dst = %dst_ip,
                    "No route to destination, sending ICMP unreachable",
                );

                let mut pb = PacketBuffer::new();
                let icmp = PacketBuilder::ipv6(src_ip.octets(), src_ip.octets(), hop_limit).icmpv6(
                    Icmpv6Type::DestinationUnreachable(DestUnreachableCode::NoRoute),
                );
                let orig_buf_end = packet
                    .buffer()
                    .len()
                    .min(MIN_IPV6_MTU - IPV6_MIN_HEADER_SIZE - ICMP6_HEADER_SIZE);
                pb.set_size(icmp.size(orig_buf_end));
                let mut b = pb.buffer_mut();
                if let Err(e) = icmp.write(&mut b, &packet.buffer()[..orig_buf_end]) {
                    error!("Failed to construct no route to host ICMP packet {e}");
                    return None;
                }

                Some(pb)
            }
        }
    }

    /// Process packets that were queued waiting for route discovery.
    ///
    /// When a route becomes available, packets are sent here from the router's packet queue.
    /// We encrypt them and route them.
    async fn process_pending_packets_loop<T>(
        self,
        mut l3_packet_sink: T,
        mut pending_packet_rx: UnboundedReceiver<UnencryptedPacket>,
    ) where
        T: Sink<PacketBuffer> + Send + Unpin + 'static,
        T::Error: std::fmt::Display,
    {
        while let Some(packet) = pending_packet_rx.recv().await {
            debug!(
                src = %packet.src_ip,
                dst = %packet.dst_ip,
                "Processing pending packet after route discovery"
            );

            // The packet already has its header set from when it was originally queued
            if let Some(icmp) = self.encrypt_and_route_packet(
                packet.src_ip,
                packet.dst_ip,
                packet.hop_limit,
                packet.packet,
            ) {
                // If we still couldn't route (e.g., route disappeared), send ICMP
                if let Err(e) = l3_packet_sink.send(icmp).await {
                    error!("Could not forward ICMP packet back to TUN interface: {e}");
                }
            }
        }

        warn!("Pending packet processing loop ended");
    }

    /// Process packets that timed out waiting for route discovery.
    ///
    /// When a packet times out in the queue, it's sent here to generate an ICMP
    /// unreachable response back to the sender.
    async fn process_timeout_packets_loop<T>(
        self,
        mut l3_packet_sink: T,
        mut timeout_packet_rx: UnboundedReceiver<UnencryptedPacket>,
    ) where
        T: Sink<PacketBuffer> + Send + Unpin + 'static,
        T::Error: std::fmt::Display,
    {
        while let Some(packet) = timeout_packet_rx.recv().await {
            debug!(
                src = %packet.src_ip,
                dst = %packet.dst_ip,
                "Packet timed out waiting for route, sending ICMP unreachable"
            );

            // Generate ICMP no route to host
            let mut pb = PacketBuffer::new();
            let icmp = PacketBuilder::ipv6(
                packet.src_ip.octets(),
                packet.src_ip.octets(),
                packet.hop_limit,
            )
            .icmpv6(Icmpv6Type::DestinationUnreachable(
                DestUnreachableCode::NoRoute,
            ));
            let orig_buf_end = packet
                .packet
                .buffer()
                .len()
                .min(MIN_IPV6_MTU - IPV6_MIN_HEADER_SIZE - ICMP6_HEADER_SIZE);
            pb.set_size(icmp.size(orig_buf_end));
            let mut b = pb.buffer_mut();
            if let Err(e) = icmp.write(&mut b, &packet.packet.buffer()[..orig_buf_end]) {
                error!("Failed to construct ICMP no route packet: {e}");
                continue;
            }

            if let Err(e) = l3_packet_sink.send(pb).await {
                error!("Could not forward ICMP packet back to TUN interface: {e}");
            }
        }

        warn!("Timeout packet processing loop ended");
    }

    async fn extract_packet_loop<T, U>(
        self,
        mut l3_packet_sink: T,
        mut message_packet_sink: U,
        mut host_packet_source: UnboundedReceiver<DataPacket>,
        incoming_packet_queue: IncomingPacketQueue,
        mut incoming_route_rx: UnboundedReceiver<Subnet>,
    ) where
        T: Sink<PacketBuffer> + Send + Unpin + 'static,
        T::Error: std::fmt::Display,
        U: Sink<(PacketBuffer, IpAddr, IpAddr)> + Send + Unpin + 'static,
        U::Error: std::fmt::Display,
    {
        loop {
            // Collect packets to process - either from channel or from dequeued after route available
            let packets_to_process: Vec<DataPacket> = tokio::select! {
                // Handle incoming data packets
                maybe_packet = host_packet_source.recv() => {
                    let Some(data_packet) = maybe_packet else {
                        break;
                    };
                    vec![data_packet]
                }

                // Handle route availability notifications - process queued incoming packets
                Some(subnet) = incoming_route_rx.recv() => {
                    let packets = incoming_packet_queue.dequeue(subnet, self.router.metrics());
                    if !packets.is_empty() {
                        debug!(
                            subnet = %subnet,
                            count = packets.len(),
                            "Processing queued incoming packets after route became available"
                        );
                    }
                    packets
                }
            };

            // Process all collected packets
            for data_packet in packets_to_process {
                // decrypt & send to TUN interface
                let shared_secret = match self
                    .router
                    .get_shared_secret_from_dest(data_packet.src_ip.into())
                {
                    SharedSecretResult::Found(ss) => ss,
                    SharedSecretResult::Resolving => {
                        // Queue the packet - will be processed when route becomes available
                        let src_subnet = Subnet::new(data_packet.src_ip.into(), 64)
                            .expect("64 is a valid subnet size for IPv6 addresses");
                        debug!(
                            src = %data_packet.src_ip,
                            "Queueing incoming packet, route to sender being resolved"
                        );
                        if incoming_packet_queue
                            .enqueue(src_subnet, data_packet, self.router.metrics())
                            .is_err()
                        {
                            debug!("Incoming packet queue full, dropping packet");
                        }
                        continue;
                    }
                    SharedSecretResult::NoRoute => {
                        trace!("Received packet from sender with no route, dropping");
                        continue;
                    }
                };
                let mut decrypted_packet = match shared_secret.decrypt(data_packet.raw_data.clone())
                {
                    Ok(data) => data,
                    Err(_) => {
                        debug!("Dropping data packet with invalid encrypted content");
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
                match header[1] {
                    USER_DATA_L3_TYPE => {
                        let real_packet = decrypted_packet.buffer_mut();
                        if real_packet.len() < IPV6_MIN_HEADER_SIZE {
                            debug!(
                                "Decrypted packet is too short, can't possibly be a valid IPv6 packet"
                            );
                            continue;
                        }
                        // Adjust the hop limit in the decrypted packet to the new value.
                        real_packet[7] = data_packet.hop_limit;
                        if let Err(e) = l3_packet_sink.send(decrypted_packet).await {
                            error!("Failed to send packet on local TUN interface: {e}",);
                            continue;
                        }
                    }
                    USER_DATA_MESSAGE_TYPE => {
                        if let Err(e) = message_packet_sink
                            .send((
                                decrypted_packet,
                                IpAddr::V6(data_packet.src_ip),
                                IpAddr::V6(data_packet.dst_ip),
                            ))
                            .await
                        {
                            error!("Failed to send packet to message handler: {e}",);
                            continue;
                        }
                    }
                    USER_DATA_OOB_ICMP => {
                        let real_packet = &*decrypted_packet;
                        if real_packet.len() < IPV6_MIN_HEADER_SIZE + ICMP6_HEADER_SIZE + 16 {
                            debug!(
                                "Decrypted packet is too short, can't possibly be a valid IPv6 ICMP packet"
                            );
                            continue;
                        }
                        if real_packet.len() > MIN_IPV6_MTU + 16 {
                            debug!("Discarding ICMP packet which is too large");
                            continue;
                        }

                        let dec_ip = Ipv6Addr::from(
                            <&[u8] as TryInto<[u8; 16]>>::try_into(&real_packet[..16]).unwrap(),
                        );
                        trace!("ICMP for original target {dec_ip}");

                        let key = match self.router.get_shared_secret_from_dest(dec_ip.into()) {
                            SharedSecretResult::Found(k) => k,
                            _ => {
                                debug!("Can't decrypt OOB ICMP packet from unknown host");
                                continue;
                            }
                        };

                        let (_, body) = match etherparse::IpHeaders::from_slice(&real_packet[16..])
                        {
                            Ok(r) => r,
                            Err(e) => {
                                // This is a node which does not adhere to the protocol of sending back
                                // ICMP like this, or it is intentionally sending mallicious packets.
                                debug!(
                                    "Dropping malformed OOB ICMP packet from {} for {e}",
                                    data_packet.src_ip
                                );
                                continue;
                            }
                        };
                        let (header, body) = match etherparse::Icmpv6Header::from_slice(
                            body.payload,
                        ) {
                            Ok(r) => r,
                            Err(e) => {
                                // This is a node which does not adhere to the protocol of sending back
                                // ICMP like this, or it is intentionally sending mallicious packets.
                                debug!(
                                    "Dropping OOB ICMP packet from {} with malformed ICMP header ({e})",
                                    data_packet.src_ip
                                );
                                continue;
                            }
                        };

                        // Where are the leftover bytes coming from
                        let orig_pb = match key.decrypt(body[..body.len()].to_vec()) {
                            Ok(pb) => pb,
                            Err(e) => {
                                warn!("Failed to decrypt ICMP data body {e}");
                                continue;
                            }
                        };

                        let packet = etherparse::PacketBuilder::ipv6(
                            data_packet.src_ip.octets(),
                            data_packet.dst_ip.octets(),
                            data_packet.hop_limit,
                        )
                        .icmpv6(header.icmp_type);

                        let serialized_icmp = packet.size(orig_pb.len());
                        let mut rp = PacketBuffer::new();
                        rp.set_size(serialized_icmp);
                        if let Err(e) =
                            packet.write(&mut (&mut rp.buffer_mut()[..serialized_icmp]), &orig_pb)
                        {
                            error!("Could not reconstruct icmp packet {e}");
                            continue;
                        }
                        if let Err(e) = l3_packet_sink.send(rp).await {
                            error!("Failed to send packet on local TUN interface: {e}",);
                            continue;
                        }
                    }
                    _ => {
                        trace!("Dropping decrypted packet with unknown protocol type");
                        continue;
                    }
                }
            }
        }

        warn!("Extract loop from router to host ended");
    }
}

impl<M> Clone for DataPlane<M>
where
    M: Clone,
{
    fn clone(&self) -> Self {
        Self {
            router: self.router.clone(),
        }
    }
}
