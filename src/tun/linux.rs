//! Linux specific tun interface setup.

use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use futures::{Sink, Stream, TryStreamExt};
use log::{error, info};
use rtnetlink::Handle;
use tokio::{select, sync::mpsc};
use tokio_tun::{Tun, TunBuilder};

use crate::crypto::PacketBuffer;

use super::IpPacket;

// TODO
const LINK_MTU: i32 = 1420;

/// Create a new tun interface and set required routes
///
/// # Panics
///
/// This function will panic if called outside of the context of a tokio runtime.
pub async fn new(
    name: &str,
    address: IpAddr,
    address_prefix_len: u8,
    route_address: IpAddr,
    route_prefix_len: u8,
) -> Result<
    (
        impl Stream<Item = io::Result<PacketBuffer>>,
        impl Sink<IpPacket, Error = impl std::error::Error>,
    ),
    Box<dyn std::error::Error>,
> {
    let tun = create_tun_interface(name)?;

    let (conn, handle, _) = rtnetlink::new_connection()?;
    let netlink_task_handle = tokio::spawn(conn);

    let tun_index = link_index_by_name(handle.clone(), name.to_string()).await?;

    add_address(handle.clone(), address, address_prefix_len, tun_index).await?;
    match route_address {
        IpAddr::V6(dest) => add_ipv6_route(handle, tun_index, dest, route_prefix_len).await?,
        IpAddr::V4(dest) => add_ipv4_route(handle, tun_index, dest, route_prefix_len).await?,
    }

    // We are done with our netlink connection, abort the task so we can properly clean up.
    netlink_task_handle.abort();

    let (tun_sink, mut sink_receiver) = mpsc::channel::<IpPacket>(1000);
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
                    if tun_stream.send(read_result.map(|n| {
                        buf.set_size(n);
                        buf
                    })).is_err() {
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
    addr: IpAddr,
    prefix_len: u8,
    link_index: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    Ok(handle
        .address()
        .add(link_index, addr, prefix_len)
        .execute()
        .await?)
}

/// Add an IpV4 route to an interface
async fn add_ipv4_route(
    handle: Handle,
    link_index: u32,
    destination: Ipv4Addr,
    prefix: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    handle
        .route()
        .add()
        .v4()
        .destination_prefix(destination, prefix)
        .output_interface(link_index)
        .execute()
        .await?;

    Ok(())
}

/// Add an IpV6 route to an interface
async fn add_ipv6_route(
    handle: Handle,
    link_index: u32,
    destination: Ipv6Addr,
    prefix: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    handle
        .route()
        .add()
        .v6()
        .destination_prefix(destination, prefix)
        .output_interface(link_index)
        .execute()
        .await?;

    Ok(())
}
