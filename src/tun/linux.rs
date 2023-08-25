//! Linux specific tun interface setup.

use std::{
    io, mem,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    pin::Pin,
    task::Poll,
};

use futures::{Future, Sink, Stream, TryStreamExt};
use log::{debug, trace};
use rtnetlink::Handle;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf, ReadHalf, WriteHalf};
use tokio_tun::{Tun, TunBuilder};

use super::IpPacket;

// TODO
const LINK_MTU: i32 = 1420;

/// A sender half of a tun interface.
pub struct TxHalf {
    inner: WriteHalf<Tun>,
    state: TxState,
}

/// A receiver half of a tun interface.
pub struct RxHalf {
    inner: ReadHalf<Tun>,
    buffer: Vec<u8>,
    mtu: usize,
}

enum TxState {
    Idle,
    Closed,
    Sending(IpPacket),
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

    // TODO: see if we can work around it
    let (tun_rx, tun_tx) = tokio::io::split(tun);

    Ok((
        RxHalf {
            inner: tun_rx,
            buffer: vec![0; LINK_MTU as usize],
            mtu: LINK_MTU as usize,
        },
        TxHalf {
            inner: tun_tx,
            state: TxState::Idle,
        },
    ))
}

/// Create a new TUN interface
fn create_tun_interface(name: &str) -> Result<Tun, Box<dyn std::error::Error>> {
    let mut tun = TunBuilder::new()
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
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        let tx_half = self.get_mut();
        match tx_half.state {
            // If we are idle, just say we are ready.
            TxState::Idle => Poll::Ready(Ok(())),
            TxState::Closed => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "TxHalf is closed",
            ))),
            TxState::Sending(ref item) => {
                let res = Pin::new(&mut tx_half.inner)
                    .poll_write(cx, &item.0)
                    .map_ok(|_| ());
                if res.is_ready() {
                    tx_half.state = TxState::Idle;
                }
                res
            }
        }
    }

    fn start_send(mut self: std::pin::Pin<&mut Self>, item: IpPacket) -> Result<(), Self::Error> {
        if matches!(self.state, TxState::Closed) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "TxHalf is closed",
            ));
        }
        // Per the contract of the Sink trait, this can only be called at the start or after
        // Sink::poll_ready returned Poll::ready(()), so state here should always be idle. If this
        // is violated, items will be lost.
        self.state = TxState::Sending(item);
        Ok(())
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // Since we buffer only up to 1 item, we can delegate this to poll_ready.
        self.poll_ready(cx)
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // Since poll_ready consumes the pin we can't delegate this, as a state change in tx state
        // happens after the delegated call. So instead perform the same logic here and properly
        // update the state.
        let tx_half = self.get_mut();
        match tx_half.state {
            // If we are idle, just say we are ready.
            TxState::Idle => Poll::Ready(Ok(())),
            TxState::Closed => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "TxHalf is closed",
            ))),
            TxState::Sending(ref item) => {
                let res = Pin::new(&mut tx_half.inner)
                    .poll_write(cx, &item.0)
                    .map_ok(|_| ());
                if res.is_ready() {
                    tx_half.state = TxState::Closed;
                }
                res
            }
        }
    }
}

impl Stream for RxHalf {
    type Item = Result<IpPacket, io::Error>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        trace!("Poll tun receiving half, buffer size {}", self.buffer.len());
        let RxHalf {
            ref mut inner,
            ref mut buffer,
            mtu,
        } = self.get_mut();

        // Assign poll result to a temporary variable to appease the borrow checker. Failure to do
        // so will result in a compilation error because there are 2 borrows on buffer.
        let mut buf = ReadBuf::new(buffer);
        let tmp = Pin::new(inner).poll_read(cx, &mut buf)?;
        match tmp {
            Poll::Pending => {
                trace!("Tun read is Poll::pending");
                Poll::Pending
            }
            Poll::Ready(()) => {
                // We recreate the buffer everytime, since a packet is always read in its entirety.
                // Therefore the start len is always 0.
                let buf_len = buf.filled().len();
                trace!("Read {buf_len} bytes packet from tun in poll method.");
                if buf_len == 0 {
                    // EOF reached
                    debug!("Stream read 0 bytes, closing");
                    return Poll::Ready(None);
                }
                buffer.truncate(buf_len);
                // Create new buffer.
                let mut new_buffer = vec![0; *mtu];
                mem::swap(buffer, &mut new_buffer);
                Poll::Ready(Some(Ok(IpPacket(new_buffer))))
            }
        }
    }
}
