//! Linux specific tun interface setup.

use std::io;

use futures::{Sink, Stream, TryStreamExt};
use rtnetlink::Handle;
use tokio::{select, sync::mpsc};
use tokio_tun::{Tun, TunBuilder};
use tracing::{error, info};

use crate::crypto::PacketBuffer;
use crate::subnet::Subnet;
use crate::tun::TunConfig;

// TODO
const LINK_MTU: i32 = 1400;

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
    let tun = match create_tun_interface(&tun_config.name) {
        Ok(tun) => tun,
        Err(e) => {
            error!(
                "Could not create tun device named \"{}\", make sure the name is not yet in use, and you have sufficient privileges to create a network device",
                tun_config.name,
            );
            return Err(e);
        }
    };

    let (conn, handle, _) = rtnetlink::new_connection()?;
    let netlink_task_handle = tokio::spawn(conn);

    let tun_index = link_index_by_name(handle.clone(), tun_config.name).await?;

    if let Err(e) = add_address(
        handle.clone(),
        tun_index,
        Subnet::new(
            tun_config.node_subnet.address(),
            tun_config.route_subnet.prefix_len(),
        )
        .unwrap(),
    )
    .await
    {
        error!(
            "Failed to add address {0} to TUN interface: {e}",
            tun_config.node_subnet
        );
        return Err(e);
    }

    // We are done with our netlink connection, abort the task so we can properly clean up.
    netlink_task_handle.abort();

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
                            if let Err(e) = tun.send(&data).await {
                                error!("Failed to send data to tun interface {e}");
                            }
                        }
                    }
                    // Save the buffer as we didn't  use it
                    buf_hold = Some(buf);
                }
                read_result = tun.recv(buf.buffer_mut()) => {
                    let rr = read_result.map(|n| {
                        buf.set_size(n);
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
fn create_tun_interface(name: &str) -> Result<Tun, Box<dyn std::error::Error>> {
    let tun = TunBuilder::new()
        .name(name)
        .mtu(LINK_MTU)
        .queues(1)
        .up()
        .build()?
        .pop()
        .expect("Succesfully build tun interface has 1 queue");

    Ok(tun)
}

/// Retrieve the link index of an interface with the given name
async fn link_index_by_name(
    handle: Handle,
    name: String,
) -> Result<u32, Box<dyn std::error::Error>> {
    handle
        .link()
        .get()
        .match_name(name)
        .execute()
        .try_next()
        .await?
        .map(|link_message| link_message.header.index)
        .ok_or(io::Error::new(io::ErrorKind::NotFound, "link not found").into())
}

/// Add an address to an interface.
///
/// The kernel will automatically add a route entry for the subnet assigned to the interface.
async fn add_address(
    handle: Handle,
    link_index: u32,
    subnet: Subnet,
) -> Result<(), Box<dyn std::error::Error>> {
    Ok(handle
        .address()
        .add(link_index, subnet.address(), subnet.prefix_len())
        .execute()
        .await?)
}
