//! Packet queue for holding packets waiting for route discovery.
//!
//! When a packet arrives for a destination with no known route, instead of blocking the data
//! plane, the packet is enqueued here. When a route becomes available, packets are dequeued
//! and forwarded.

use crate::{metrics::Metrics, packet::DataPacket, subnet::Subnet};
use dashmap::DashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::task::AbortHandle;
use tracing::{debug, trace, warn};

/// Default maximum packets per subnet.
const DEFAULT_MAX_PER_SUBNET: usize = 10;

/// Default maximum total packets across all subnets.
const DEFAULT_MAX_TOTAL: usize = 1000;

/// A packet waiting in the queue along with its timeout handle.
struct QueuedPacket {
    packet: DataPacket,
    /// Handle to abort the timeout task when the packet is dequeued early.
    timeout_handle: AbortHandle,
}

/// A queue for packets waiting for route discovery.
///
/// Packets are organized by destination subnet. When a route for a subnet becomes available,
/// all packets for that subnet can be dequeued and forwarded.
#[derive(Clone)]
pub struct PacketQueue {
    /// Map from subnet to list of queued packets.
    queue: Arc<DashMap<Subnet, Vec<QueuedPacket>>>,
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

    /// Enqueue a packet for the given subnet.
    ///
    /// The `on_timeout` callback will be called if the packet times out before a route is found.
    /// Returns `Ok(())` if the packet was enqueued, `Err(packet)` if the queue is full,
    /// returning the packet so the caller can handle it (e.g., send ICMP unreachable).
    ///
    /// # Arguments
    /// * `subnet` - The destination subnet for the packet
    /// * `packet` - The data packet to queue
    /// * `timeout` - How long to wait before timing out
    /// * `on_timeout` - Callback to execute on timeout (typically sends ICMP unreachable)
    /// * `metrics` - Metrics implementation for tracking
    pub fn enqueue<M, F>(
        &self,
        subnet: Subnet,
        packet: DataPacket,
        timeout: std::time::Duration,
        on_timeout: F,
        metrics: &M,
    ) -> Result<(), DataPacket>
    where
        M: Metrics,
        F: FnOnce(DataPacket) + Send + 'static,
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
            return Err(packet);
        }

        // Check per-subnet limit
        let mut entry = self.queue.entry(subnet).or_insert_with(Vec::new);
        if entry.len() >= self.max_per_subnet {
            trace!(
                subnet = %subnet,
                count = entry.len(),
                max = self.max_per_subnet,
                "Packet queue full for subnet"
            );
            metrics.router_packet_queue_full_subnet();
            return Err(packet);
        }

        // Spawn the timeout task
        let queue = self.queue.clone();
        let total_count = self.total_count.clone();

        let timeout_task = tokio::spawn(async move {
            tokio::time::sleep(timeout).await;

            // Try to remove the packet from the queue
            // We identify it by checking if it's still there after timeout
            let removed = if let Some(mut packets) = queue.get_mut(&subnet) {
                // Find and remove the timed out packet
                // Since we're removing after timeout, we remove the oldest packet (first in queue)
                if !packets.is_empty() {
                    let qp = packets.remove(0);
                    total_count.fetch_sub(1, Ordering::Relaxed);
                    Some(qp.packet)
                } else {
                    None
                }
            } else {
                None
            };

            // Execute timeout callback if we found the packet
            if let Some(packet) = removed {
                debug!(
                    subnet = %subnet,
                    dst = %packet.dst_ip,
                    "Packet timed out waiting for route"
                );
                on_timeout(packet);
            }
        });

        let timeout_handle = timeout_task.abort_handle();

        // Add the packet to the queue
        entry.push(QueuedPacket {
            packet,
            timeout_handle,
        });
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
    /// This cancels any pending timeout tasks for the dequeued packets.
    /// Returns the list of packets that were waiting.
    pub fn dequeue<M>(&self, subnet: Subnet, metrics: &M) -> Vec<DataPacket>
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

        // Cancel all timeout tasks and extract packets
        let data_packets: Vec<DataPacket> = packets
            .into_iter()
            .map(|qp| {
                qp.timeout_handle.abort();
                qp.packet
            })
            .collect();

        metrics.router_packets_dequeued(count);

        debug!(
            subnet = %subnet,
            count = count,
            "Dequeued packets after route installed"
        );

        data_packets
    }

    /// Drop all packets for the given subnet.
    ///
    /// This is used when a route query times out (transitions to NoRoute state).
    /// Returns the number of packets that were dropped.
    pub fn drop_subnet<M>(&self, subnet: Subnet, metrics: &M) -> usize
    where
        M: Metrics,
    {
        let packets = if let Some((_, queued)) = self.queue.remove(&subnet) {
            queued
        } else {
            return 0;
        };

        if packets.is_empty() {
            return 0;
        }

        let count = packets.len();
        self.total_count.fetch_sub(count, Ordering::Relaxed);

        // Cancel all timeout tasks (packets are dropped, not forwarded)
        for qp in packets {
            qp.timeout_handle.abort();
        }

        metrics.router_packets_dropped_no_route(count);

        warn!(
            subnet = %subnet,
            count = count,
            "Dropped queued packets - no route found"
        );

        count
    }

    /// Check if there are any packets queued for the given subnet.
    pub fn has_packets(&self, subnet: &Subnet) -> bool {
        self.queue
            .get(subnet)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Get the total number of packets currently queued.
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
    use std::net::Ipv6Addr;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

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

    #[tokio::test]
    async fn test_enqueue_dequeue() {
        let queue = PacketQueue::new();
        let metrics = TestMetrics;
        let subnet = Subnet::new("2001:db8::".parse().unwrap(), 64).unwrap();
        let packet = make_test_packet("2001:db8::1".parse().unwrap());

        let timeout_called = Arc::new(AtomicBool::new(false));
        let timeout_called_clone = timeout_called.clone();

        assert!(queue
            .enqueue(
                subnet,
                packet,
                Duration::from_secs(5),
                move |_| {
                    timeout_called_clone.store(true, Ordering::SeqCst);
                },
                &metrics
            )
            .is_ok());

        assert_eq!(queue.total_count(), 1);
        assert!(queue.has_packets(&subnet));

        let packets = queue.dequeue(subnet, &metrics);
        assert_eq!(packets.len(), 1);
        assert_eq!(queue.total_count(), 0);
        assert!(!queue.has_packets(&subnet));

        // Give time for abort to propagate
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!timeout_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_per_subnet_limit() {
        let queue = PacketQueue::with_limits(2, 100);
        let metrics = TestMetrics;
        let subnet = Subnet::new("2001:db8::".parse().unwrap(), 64).unwrap();

        // Should succeed
        assert!(queue
            .enqueue(
                subnet,
                make_test_packet("2001:db8::1".parse().unwrap()),
                Duration::from_secs(5),
                |_| {},
                &metrics
            )
            .is_ok());
        assert!(queue
            .enqueue(
                subnet,
                make_test_packet("2001:db8::2".parse().unwrap()),
                Duration::from_secs(5),
                |_| {},
                &metrics
            )
            .is_ok());

        // Should fail - per-subnet limit reached
        assert!(queue
            .enqueue(
                subnet,
                make_test_packet("2001:db8::3".parse().unwrap()),
                Duration::from_secs(5),
                |_| {},
                &metrics
            )
            .is_err());

        assert_eq!(queue.total_count(), 2);
    }

    #[tokio::test]
    async fn test_global_limit() {
        let queue = PacketQueue::with_limits(10, 2);
        let metrics = TestMetrics;
        let subnet1 = Subnet::new("2001:db8:1::".parse().unwrap(), 64).unwrap();
        let subnet2 = Subnet::new("2001:db8:2::".parse().unwrap(), 64).unwrap();
        let subnet3 = Subnet::new("2001:db8:3::".parse().unwrap(), 64).unwrap();

        assert!(queue
            .enqueue(
                subnet1,
                make_test_packet("2001:db8:1::1".parse().unwrap()),
                Duration::from_secs(5),
                |_| {},
                &metrics
            )
            .is_ok());
        assert!(queue
            .enqueue(
                subnet2,
                make_test_packet("2001:db8:2::1".parse().unwrap()),
                Duration::from_secs(5),
                |_| {},
                &metrics
            )
            .is_ok());

        // Should fail - global limit reached
        assert!(queue
            .enqueue(
                subnet3,
                make_test_packet("2001:db8:3::1".parse().unwrap()),
                Duration::from_secs(5),
                |_| {},
                &metrics
            )
            .is_err());

        assert_eq!(queue.total_count(), 2);
    }

    #[tokio::test]
    async fn test_drop_subnet() {
        let queue = PacketQueue::new();
        let metrics = TestMetrics;
        let subnet = Subnet::new("2001:db8::".parse().unwrap(), 64).unwrap();

        let _ = queue.enqueue(
            subnet,
            make_test_packet("2001:db8::1".parse().unwrap()),
            Duration::from_secs(5),
            |_| {},
            &metrics,
        );
        let _ = queue.enqueue(
            subnet,
            make_test_packet("2001:db8::2".parse().unwrap()),
            Duration::from_secs(5),
            |_| {},
            &metrics,
        );

        assert_eq!(queue.total_count(), 2);

        let dropped = queue.drop_subnet(subnet, &metrics);
        assert_eq!(dropped, 2);
        assert_eq!(queue.total_count(), 0);
    }
}
