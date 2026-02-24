//! Linux specific tun interface setup.

use std::io;
use std::net::IpAddr;

use futures::{Sink, Stream};
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::crypto::PacketBuffer;
use crate::tun::TunConfig;

const LINK_MTU: u32 = 1400;

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

    let (read_half, write_half) = tun.split()?;

    let (tun_sink, mut sink_receiver) = mpsc::channel::<PacketBuffer>(1000);
    let (tun_stream, stream_receiver) = mpsc::unbounded_channel();

    // Spawn a task to read packets from the TUN device.
    tokio::spawn(async move {
        loop {
            let mut buf = PacketBuffer::new();
            match read_half.read(buf.buffer_mut()).await {
                Ok(n) => {
                    buf.set_size(n);
                    if tun_stream.send(Ok(buf)).is_err() {
                        error!("Could not forward data to tun stream, receiver is gone");
                        break;
                    }
                }
                Err(e) => {
                    error!("Failed to read from tun interface: {e}");
                }
            }
        }
        info!("Stop reading from tun interface");
    });

    // Spawn a task to write packets to the TUN device.
    tokio::spawn(async move {
        while let Some(data) = sink_receiver.recv().await {
            if let Err(e) = write_half.write(&data).await {
                error!("Failed to send data to tun interface: {e}");
            }
        }
        info!("Stop writing to tun interface");
    });

    Ok((
        tokio_stream::wrappers::UnboundedReceiverStream::new(stream_receiver),
        tokio_util::sync::PollSender::new(tun_sink),
    ))
}
