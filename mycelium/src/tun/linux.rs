//! Linux specific tun interface setup.

use std::io;
use std::net::IpAddr;

use futures::{Sink, Stream};
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::crypto::PacketBuffer;
use crate::tun::TunConfig;

const LINK_MTU: u32 = 1400;

/// Maximum number of packets that can be delivered from a single GRO-coalesced read.
const READ_BATCH_SIZE: usize = 64;

/// Maximum number of packets to coalesce for a single GSO write.
const WRITE_BATCH_SIZE: usize = 64;

/// Maximum number of packets that can be buffered between the TUN reader task and the DataPlane.
/// This provides backpressure on the ingress path: if the DataPlane can't keep up with TUN reads,
/// the reader task will block until space is available, which in turn stops reading from the
/// kernel TUN device.
const TUN_READ_CHANNEL_CAPACITY: usize = 1000;

/// Create a new tun interface and set required routes
///
/// # Panics
///
/// This function will panic if called outside of the context of a tokio runtime.
pub async fn new(
    tun_config: TunConfig,
) -> Result<
    (
        impl Stream<Item = io::Result<PacketBuffer>>,
        impl Sink<PacketBuffer, Error = impl std::error::Error> + Clone,
    ),
    Box<dyn std::error::Error>,
> {
    let tun = match mycelium_tun::Tun::new(&tun_config.name) {
        Ok(tun) => tun,
        Err(e) => {
            error!(
                "Could not create tun device named \"{}\", make sure the name is not yet in use, and you have sufficient privileges to create a network device",
                tun_config.name,
            );
            return Err(e.into());
        }
    };

    tun.set_mtu(LINK_MTU)?;

    let addr = match tun_config.node_subnet.address() {
        IpAddr::V6(addr) => addr,
        IpAddr::V4(_) => {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "expected IPv6 address").into(),
            );
        }
    };
    tun.set_addr(addr, tun_config.route_subnet.prefix_len())?;
    tun.set_up()?;

    let (mut read_half, mut write_half) = tun.split()?;

    let (tun_sink, mut sink_receiver) = mpsc::channel::<PacketBuffer>(1000);
    let (tun_stream, stream_receiver) = mpsc::channel(TUN_READ_CHANNEL_CAPACITY);

    // Spawn a task to read packets from the TUN device.
    // The kernel may deliver GRO-coalesced super-packets, which are split into individual
    // packets by the read call.
    tokio::spawn(async move {
        let mut packet_bufs: Vec<PacketBuffer> =
            (0..READ_BATCH_SIZE).map(|_| PacketBuffer::new()).collect();
        let mut sizes = [0usize; READ_BATCH_SIZE];

        loop {
            let mut bufs: Vec<&mut [u8]> =
                packet_bufs.iter_mut().map(|pb| pb.buffer_mut()).collect();

            match read_half.read(&mut bufs, &mut sizes).await {
                Ok(n) => {
                    // Drop the borrow on packet_bufs before moving packets out.
                    drop(bufs);

                    // Reserve capacity for the entire batch at once, avoiding
                    // per-packet async overhead on the channel.
                    let permits = match tun_stream.reserve_many(n).await {
                        Ok(permits) => permits,
                        Err(_) => {
                            error!("Could not forward data to tun stream, receiver is gone");
                            return;
                        }
                    };
                    for (i, permit) in permits.enumerate() {
                        let mut pkt = std::mem::take(&mut packet_bufs[i]);
                        pkt.set_size(sizes[i]);
                        permit.send(Ok(pkt));
                    }
                }
                Err(e) => {
                    error!("Failed to read from tun interface: {e}");
                    drop(bufs);
                }
            }
        }
    });

    // Spawn a task to write packets to the TUN device.
    // Batches packets from the channel for GSO coalescing.
    tokio::spawn(async move {
        let mut batch: Vec<PacketBuffer> = Vec::with_capacity(WRITE_BATCH_SIZE);

        loop {
            // Wait for at least one packet.
            match sink_receiver.recv().await {
                Some(data) => batch.push(data),
                None => break,
            }

            // Drain any additional immediately-available packets up to the batch limit.
            while batch.len() < WRITE_BATCH_SIZE {
                match sink_receiver.try_recv() {
                    Ok(data) => batch.push(data),
                    Err(_) => break,
                }
            }

            let pkts: Vec<&[u8]> = batch.iter().map(|pb| &**pb).collect();
            if let Err(e) = write_half.write(&pkts).await {
                error!("Failed to send data to tun interface: {e}");
            }

            batch.clear();
        }
        info!("Stop writing to tun interface");
    });

    Ok((
        tokio_stream::wrappers::ReceiverStream::new(stream_receiver),
        tokio_util::sync::PollSender::new(tun_sink),
    ))
}
