//! Packet queue for holding packets waiting for route discovery.
//!
//! When a packet arrives for a destination with no known route, instead of blocking the data
//! plane, the packet is enqueued here. When a route becomes available, packets are dequeued
//! and forwarded. If the route query times out, packets are returned for ICMP generation.

use crate::{crypto::PacketBuffer, metrics::Metrics, packet::DataPacket, subnet::Subnet};
use dashmap::DashMap;
use std::{
    net::Ipv6Addr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tracing::{debug, trace, warn};

/// Default maximum packets per subnet.
const DEFAULT_MAX_PER_SUBNET: usize = 10_000; // 1.5KB * 10K -> 15MB

/// Default maximum total packets across all subnets.
const DEFAULT_MAX_TOTAL: usize = 1_000_000; // 1.5KB * 1M -> 1.5GB. In practice this won't happen

/// An unencrypted packet waiting to be encrypted and routed once a route is discovered.
#[derive(Debug)]
pub struct UnencryptedPacket {
    pub src_ip: Ipv6Addr,
    pub dst_ip: Ipv6Addr,
    pub hop_limit: u8,
    pub packet: PacketBuffer,
}

/// A packet waiting in the queue - either encrypted (from router) or unencrypted (from data plane).
pub enum QueuedPacketData {
    /// An already-encrypted packet from the router layer.
    Encrypted(DataPacket),
    /// An unencrypted packet from the data plane that needs encryption when dequeued.
    Unencrypted(UnencryptedPacket),
}

/// A queue for packets waiting for route discovery.
///
/// Packets are organized by destination subnet. When a route for a subnet becomes available,
/// all packets for that subnet can be dequeued and forwarded. Timeout is handled at the
/// subnet level by the routing table's query timeout mechanism.
#[derive(Clone)]
pub struct PacketQueue {
    /// Map from subnet to list of queued packets.
    queue: Arc<DashMap<Subnet, Vec<QueuedPacketData>>>,
    /// Maximum packets allowed per subnet.
    max_per_subnet: usize,
    /// Maximum total packets across all subnets.
    max_total: usize,
    /// Current total packet count.
    total_count: Arc<AtomicUsize>,
}

impl PacketQueue {
    /// Create a new packet queue with default limits.
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_MAX_PER_SUBNET, DEFAULT_MAX_TOTAL)
    }

    /// Create a new packet queue with custom limits.
    pub fn with_limits(max_per_subnet: usize, max_total: usize) -> Self {
        Self {
            queue: Arc::new(DashMap::new()),
            max_per_subnet,
            max_total,
            total_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Enqueue an encrypted packet for the given subnet.
    ///
    /// Returns `Ok(())` if the packet was enqueued, `Err(packet)` if the queue is full.
    /// Packets will be dequeued when a route becomes available, or returned via
    /// `drop_subnet` when the route query times out.
    pub fn enqueue_encrypted<M>(
        &self,
        subnet: Subnet,
        packet: DataPacket,
        metrics: &M,
    ) -> Result<(), DataPacket>
    where
        M: Metrics,
    {
        self.enqueue_inner(subnet, QueuedPacketData::Encrypted(packet), metrics)
            .map_err(|data| match data {
                QueuedPacketData::Encrypted(p) => p,
                _ => unreachable!(),
            })
    }

    /// Enqueue an unencrypted packet for the given subnet.
    ///
    /// Returns `Ok(())` if the packet was enqueued, `Err(packet)` if the queue is full.
    /// Packets will be dequeued when a route becomes available, or returned via
    /// `drop_subnet` when the route query times out.
    pub fn enqueue_unencrypted<M>(
        &self,
        subnet: Subnet,
        packet: UnencryptedPacket,
        metrics: &M,
    ) -> Result<(), UnencryptedPacket>
    where
        M: Metrics,
    {
        self.enqueue_inner(subnet, QueuedPacketData::Unencrypted(packet), metrics)
            .map_err(|data| match data {
                QueuedPacketData::Unencrypted(p) => p,
                _ => unreachable!(),
            })
    }

    /// Internal enqueue implementation.
    fn enqueue_inner<M>(
        &self,
        subnet: Subnet,
        data: QueuedPacketData,
        metrics: &M,
    ) -> Result<(), QueuedPacketData>
    where
        M: Metrics,
    {
        // Check global limit
        let current_total = self.total_count.load(Ordering::Relaxed);
        if current_total >= self.max_total {
            trace!(
                subnet = %subnet,
                total = current_total,
                max = self.max_total,
                "Packet queue full (global limit)"
            );
            metrics.router_packet_queue_full_global();
            return Err(data);
        }

        // Check per-subnet limit
        let mut entry = self.queue.entry(subnet).or_default();
        if entry.len() >= self.max_per_subnet {
            trace!(
                subnet = %subnet,
                count = entry.len(),
                max = self.max_per_subnet,
                "Packet queue full for subnet"
            );
            metrics.router_packet_queue_full_subnet();
            return Err(data);
        }

        // Add the packet to the queue
        entry.push(data);
        self.total_count.fetch_add(1, Ordering::Relaxed);

        metrics.router_packet_enqueued();

        trace!(
            subnet = %subnet,
            queue_len = entry.len(),
            total = self.total_count.load(Ordering::Relaxed),
            "Packet enqueued waiting for route"
        );

        Ok(())
    }

    /// Dequeue all packets for the given subnet.
    ///
    /// Returns the list of packets that were waiting. Called when a route becomes available.
    pub fn dequeue<M>(&self, subnet: Subnet, metrics: &M) -> Vec<QueuedPacketData>
    where
        M: Metrics,
    {
        let packets = if let Some((_, queued)) = self.queue.remove(&subnet) {
            queued
        } else {
            return Vec::new();
        };

        if packets.is_empty() {
            return Vec::new();
        }

        let count = packets.len();
        self.total_count.fetch_sub(count, Ordering::Relaxed);

        metrics.router_packets_dequeued(count);

        debug!(
            subnet = %subnet,
            count = count,
            "Dequeued packets after route installed"
        );

        packets
    }

    /// Remove all packets for the given subnet when the route query times out.
    ///
    /// This is used when a route query times out (transitions to NoRoute state).
    /// Returns the packets so they can be used for ICMP generation.
    pub fn drop_subnet<M>(&self, subnet: Subnet, metrics: &M) -> Vec<QueuedPacketData>
    where
        M: Metrics,
    {
        let packets = if let Some((_, queued)) = self.queue.remove(&subnet) {
            queued
        } else {
            return Vec::new();
        };

        if packets.is_empty() {
            return Vec::new();
        }

        let count = packets.len();
        self.total_count.fetch_sub(count, Ordering::Relaxed);

        metrics.router_packets_dropped_no_route(count);

        warn!(
            subnet = %subnet,
            count = count,
            "Returning queued packets for ICMP - no route found"
        );

        packets
    }

    /// Check if there are any packets queued for the given subnet.
    #[cfg(test)]
    pub fn has_packets(&self, subnet: &Subnet) -> bool {
        self.queue
            .get(subnet)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Get the total number of packets currently queued.
    #[cfg(test)]
    pub fn total_count(&self) -> usize {
        self.total_count.load(Ordering::Relaxed)
    }
}

impl Default for PacketQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct TestMetrics;
    impl Metrics for TestMetrics {}

    fn make_test_packet(dst: Ipv6Addr) -> DataPacket {
        DataPacket {
            raw_data: vec![1, 2, 3, 4],
            hop_limit: 64,
            src_ip: "::1".parse().unwrap(),
            dst_ip: dst,
        }
    }

    fn make_unencrypted_packet(dst: Ipv6Addr) -> UnencryptedPacket {
        let mut pb = PacketBuffer::new();
        pb.set_size(64);
        UnencryptedPacket {
            src_ip: "::1".parse().unwrap(),
            dst_ip: dst,
            hop_limit: 64,
            packet: pb,
        }
    }

    #[test]
    fn test_enqueue_dequeue_encrypted() {
        let queue = PacketQueue::new();
        let metrics = TestMetrics;
        let subnet = Subnet::new("2001:db8::".parse().unwrap(), 64).unwrap();
        let packet = make_test_packet("2001:db8::1".parse().unwrap());

        assert!(queue.enqueue_encrypted(subnet, packet, &metrics).is_ok());

        assert_eq!(queue.total_count(), 1);
        assert!(queue.has_packets(&subnet));

        let packets = queue.dequeue(subnet, &metrics);
        assert_eq!(packets.len(), 1);
        assert!(matches!(packets[0], QueuedPacketData::Encrypted(_)));
        assert_eq!(queue.total_count(), 0);
        assert!(!queue.has_packets(&subnet));
    }

    #[test]
    fn test_enqueue_dequeue_unencrypted() {
        let queue = PacketQueue::new();
        let metrics = TestMetrics;
        let subnet = Subnet::new("2001:db8::".parse().unwrap(), 64).unwrap();
        let packet = make_unencrypted_packet("2001:db8::1".parse().unwrap());

        assert!(queue.enqueue_unencrypted(subnet, packet, &metrics).is_ok());

        assert_eq!(queue.total_count(), 1);

        let packets = queue.dequeue(subnet, &metrics);
        assert_eq!(packets.len(), 1);
        assert!(matches!(packets[0], QueuedPacketData::Unencrypted(_)));
        assert_eq!(queue.total_count(), 0);
    }

    #[test]
    fn test_mixed_packet_types() {
        let queue = PacketQueue::new();
        let metrics = TestMetrics;
        let subnet = Subnet::new("2001:db8::".parse().unwrap(), 64).unwrap();

        // Enqueue one encrypted and one unencrypted
        let _ = queue.enqueue_encrypted(
            subnet,
            make_test_packet("2001:db8::1".parse().unwrap()),
            &metrics,
        );
        let _ = queue.enqueue_unencrypted(
            subnet,
            make_unencrypted_packet("2001:db8::2".parse().unwrap()),
            &metrics,
        );

        assert_eq!(queue.total_count(), 2);

        let packets = queue.dequeue(subnet, &metrics);
        assert_eq!(packets.len(), 2);
        assert!(matches!(packets[0], QueuedPacketData::Encrypted(_)));
        assert!(matches!(packets[1], QueuedPacketData::Unencrypted(_)));
    }

    #[test]
    fn test_per_subnet_limit() {
        let queue = PacketQueue::with_limits(2, 100);
        let metrics = TestMetrics;
        let subnet = Subnet::new("2001:db8::".parse().unwrap(), 64).unwrap();

        // Should succeed
        assert!(queue
            .enqueue_encrypted(
                subnet,
                make_test_packet("2001:db8::1".parse().unwrap()),
                &metrics
            )
            .is_ok());
        assert!(queue
            .enqueue_encrypted(
                subnet,
                make_test_packet("2001:db8::2".parse().unwrap()),
                &metrics
            )
            .is_ok());

        // Should fail - per-subnet limit reached
        assert!(queue
            .enqueue_encrypted(
                subnet,
                make_test_packet("2001:db8::3".parse().unwrap()),
                &metrics
            )
            .is_err());

        assert_eq!(queue.total_count(), 2);
    }

    #[test]
    fn test_global_limit() {
        let queue = PacketQueue::with_limits(10, 2);
        let metrics = TestMetrics;
        let subnet1 = Subnet::new("2001:db8:1::".parse().unwrap(), 64).unwrap();
        let subnet2 = Subnet::new("2001:db8:2::".parse().unwrap(), 64).unwrap();
        let subnet3 = Subnet::new("2001:db8:3::".parse().unwrap(), 64).unwrap();

        assert!(queue
            .enqueue_encrypted(
                subnet1,
                make_test_packet("2001:db8:1::1".parse().unwrap()),
                &metrics
            )
            .is_ok());
        assert!(queue
            .enqueue_encrypted(
                subnet2,
                make_test_packet("2001:db8:2::1".parse().unwrap()),
                &metrics
            )
            .is_ok());

        // Should fail - global limit reached
        assert!(queue
            .enqueue_encrypted(
                subnet3,
                make_test_packet("2001:db8:3::1".parse().unwrap()),
                &metrics
            )
            .is_err());

        assert_eq!(queue.total_count(), 2);
    }

    #[test]
    fn test_drop_subnet() {
        let queue = PacketQueue::new();
        let metrics = TestMetrics;
        let subnet = Subnet::new("2001:db8::".parse().unwrap(), 64).unwrap();

        let _ = queue.enqueue_encrypted(
            subnet,
            make_test_packet("2001:db8::1".parse().unwrap()),
            &metrics,
        );
        let _ = queue.enqueue_encrypted(
            subnet,
            make_test_packet("2001:db8::2".parse().unwrap()),
            &metrics,
        );

        assert_eq!(queue.total_count(), 2);

        let dropped = queue.drop_subnet(subnet, &metrics);
        assert_eq!(dropped.len(), 2);
        assert_eq!(queue.total_count(), 0);
    }
}
