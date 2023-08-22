//! Linux specific tun interface setup.

use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    pin::Pin,
};

use futures::{Sink, Stream, TryStreamExt};
use rtnetlink::Handle;
use tokio::io::{ReadHalf, WriteHalf};
use tokio_tun::{Tun, TunBuilder};
use tokio_util::codec::{FramedRead, FramedWrite};

use super::{IpPacket, IpPacketCodec};

// TODO
const LINK_MTU: i32 = 1420;

/// A sender half of a tun interface.
pub struct TxHalf {
    inner: FramedWrite<WriteHalf<Tun>, IpPacketCodec>,
}

/// A receiver half of a tun interface.
pub struct RxHalf {
    inner: FramedRead<ReadHalf<Tun>, IpPacketCodec>,
}

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
) -> Result<(RxHalf, TxHalf), Box<dyn std::error::Error>> {
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

    let (rxhalf, txhalf) = tokio::io::split(tun);

    Ok((
        RxHalf {
            inner: FramedRead::new(rxhalf, IpPacketCodec::new()),
        },
        TxHalf {
            inner: FramedWrite::new(txhalf, IpPacketCodec::new()),
        },
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

impl Sink<IpPacket> for TxHalf {
    type Error = io::Error;

    fn poll_ready(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner).poll_ready(cx)
    }

    fn start_send(mut self: std::pin::Pin<&mut Self>, item: IpPacket) -> Result<(), Self::Error> {
        Pin::new(&mut self.inner).start_send(item)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_close(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

impl Stream for RxHalf {
    type Item = Result<IpPacket, io::Error>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}
