//! Linux specific tun interface setup.

use std::{io, net::IpAddr};

use futures::{Sink, Stream, TryStreamExt};
use log::{error, info};
use mycelium::subnet::Subnet;
use rtnetlink::Handle;
use tokio::{select, sync::mpsc};
use tokio_tun::{Tun, TunBuilder};

use crate::crypto::PacketBuffer;

// TODO
const LINK_MTU: i32 = 1400;

/// Create a new tun interface and set required routes
///
/// # Panics
///
/// This function will panic if called outside of the context of a tokio runtime.
pub async fn new(
    name: &str,
    node_subnet: Subnet,
    route_subnet: Subnet,
) -> Result<
    (
        impl Stream<Item = io::Result<PacketBuffer>>,
        impl Sink<PacketBuffer, Error = impl std::error::Error>,
    ),
    Box<dyn std::error::Error>,
> {
    let tun = create_tun_interface(name)?;

    let (conn, handle, _) = rtnetlink::new_connection()?;
    let netlink_task_handle = tokio::spawn(conn);

    let tun_index = link_index_by_name(handle.clone(), name.to_string()).await?;

    add_address(handle.clone(), tun_index, node_subnet).await?;
    add_route(handle.clone(), tun_index, route_subnet).await?;

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
        .tap(false)
        .mtu(LINK_MTU)
        .packet_info(false)
        .up()
        .try_build()?;

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

/// Add a route to an interface
async fn add_route(
    handle: Handle,
    link_index: u32,
    subnet: Subnet,
) -> Result<(), Box<dyn std::error::Error>> {
    let base = handle.route().add();
    match subnet.address() {
        IpAddr::V4(addr) => {
            base.v4()
                .destination_prefix(addr, subnet.prefix_len())
                .output_interface(link_index)
                .execute()
                .await
        }
        IpAddr::V6(addr) => {
            base.v6()
                .destination_prefix(addr, subnet.prefix_len())
                .output_interface(link_index)
                .execute()
                .await
        }
    }?;
    Ok(())
}
