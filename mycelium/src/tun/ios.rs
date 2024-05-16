//! ios specific tun interface setup.

use std::io::{self, IoSlice};

use futures::{Sink, Stream};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    select,
    sync::mpsc,
};
use tracing::{error, info};

use crate::crypto::PacketBuffer;
use crate::tun::TunConfig;

// TODO
const LINK_MTU: i32 = 1400;

/// The 4 byte packet header written before a packet is sent on the TUN
// TODO: figure out structure and values, but for now this seems to work.
const HEADER: [u8; 4] = [0, 0, 0, 30];

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
    let mut tun = create_tun_interface(tun_config.tun_fd)?;

    let (tun_sink, mut sink_receiver) = mpsc::channel::<PacketBuffer>(1000);
    let (tun_stream, stream_receiver) = mpsc::unbounded_channel();

    // Spawn a single task to manage the TUN interface
    tokio::spawn(async move {
        let mut buf_hold = None;
        loop {
            let mut buf = if let Some(buf) = buf_hold.take() {
                buf
            } else {
                PacketBuffer::new()
            };

            select! {
                data = sink_receiver.recv() => {
                    match data {
                        None => return,
                        Some(data) => {
                            // We need to append a 4 byte header here
                            if let Err(e) = tun.write_vectored(&[IoSlice::new(&HEADER), IoSlice::new(&data)]).await {
                                error!("Failed to send data to tun interface {e}");
                            }
                        }
                    }
                    // Save the buffer as we didn't  use it
                    buf_hold = Some(buf);
                }
                read_result = tun.read(buf.buffer_mut()) => {
                    let rr = read_result.map(|n| {
                        buf.set_size(n);
                        // Trim header
                        buf.buffer_mut().copy_within(4.., 0);
                        buf.set_size(n-4);
                        buf
                    });


                    if tun_stream.send(rr).is_err() {
                        error!("Could not forward data to tun stream, receiver is gone");
                        break;
                    };
                }
            }
        }
        info!("Stop reading from / writing to tun interface");
    });

    Ok((
        tokio_stream::wrappers::UnboundedReceiverStream::new(stream_receiver),
        tokio_util::sync::PollSender::new(tun_sink),
    ))
}

/// Create a new TUN interface
fn create_tun_interface(tun_fd: i32) -> Result<tun::AsyncDevice, Box<dyn std::error::Error>> {
    let mut config = tun::Configuration::default();
    config
        .layer(tun::Layer::L3)
        .mtu(LINK_MTU)
        .queues(1)
        .raw_fd(tun_fd)
        .up();
    let tun = tun::create_as_async(&config)?;

    Ok(tun)
}
