use std::{io, ops::Deref, sync::Arc};

use futures::{Sink, Stream};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::tun::TunConfig;
use crate::{crypto::PacketBuffer, subnet::Subnet};

// TODO
const LINK_MTU: usize = 1400;

/// Type of the tunnel used, specified when creating the tunnel.
const WINDOWS_TUNNEL_TYPE: &str = "Mycelium";

pub async fn new(
    tun_config: TunConfig,
) -> Result<
    (
        impl Stream<Item = io::Result<PacketBuffer>>,
        impl Sink<PacketBuffer, Error = impl std::error::Error> + Clone,
    ),
    Box<dyn std::error::Error>,
> {
    // SAFETY: for now we assume a valid wintun.dll file exists in the root directory when we are
    // running this.
    let wintun = unsafe { wintun::load() }?;
    let wintun_version = match wintun::get_running_driver_version(&wintun) {
        Ok(v) => format!("{v}"),
        Err(e) => {
            warn!("Failed to read wintun.dll version: {e}");
            "Unknown".to_string()
        }
    };
    info!("Loaded wintun.dll - running version {wintun_version}");
    let tun = wintun::Adapter::create(&wintun, &tun_config.name, WINDOWS_TUNNEL_TYPE, None)?;
    info!("Created wintun tunnel interface");
    // Configure created network adapter.
    set_adapter_mtu(&tun_config.name, LINK_MTU)?;
    // Set address, this will use a `netsh` command under the hood unfortunately.
    // TODO: fix in library
    // tun.set_network_addresses_tuple(node_subnet.address(), route_subnet.mask(), None)?;
    add_address(
        &tun_config.name,
        tun_config.node_subnet,
        tun_config.route_subnet,
    )?;
    // Build 2 separate sessions - one for receiving, one for sending.
    let rx_session = Arc::new(tun.start_session(wintun::MAX_RING_CAPACITY)?);
    let tx_session = rx_session.clone();

    let (tun_sink, mut sink_receiver) = mpsc::channel::<PacketBuffer>(1000);
    let (tun_stream, stream_receiver) = mpsc::unbounded_channel();

    // Ingress path
    tokio::task::spawn_blocking(move || {
        loop {
            let packet = rx_session
                .receive_blocking()
                .map(|tun_packet| {
                    let mut buffer = PacketBuffer::new();
                    // SAFETY: The configured MTU is smaller than the static PacketBuffer size.
                    let packet_len = tun_packet.bytes().len();
                    buffer.buffer_mut()[..packet_len].copy_from_slice(tun_packet.bytes());
                    buffer.set_size(packet_len);
                    buffer
                })
                .map_err(wintun_to_io_error);

            if tun_stream.send(packet).is_err() {
                error!("Could not forward data to tun stream, receiver is gone");
                break;
            };
        }

        info!("Stop reading from tun interface");
    });

    // Egress path
    tokio::task::spawn_blocking(move || {
        loop {
            match sink_receiver.blocking_recv() {
                None => break,
                Some(data) => {
                    let mut tun_packet =
                        match tx_session.allocate_send_packet(data.deref().len() as u16) {
                            Ok(tun_packet) => tun_packet,
                            Err(e) => {
                                error!("Could not allocate packet on TUN: {e}");
                                break;
                            }
                        };
                    // SAFETY: packet allocation is done on the length of &data.
                    tun_packet.bytes_mut().copy_from_slice(&data);
                    tx_session.send_packet(tun_packet);
                }
            }
        }
        info!("Stop writing to tun interface");
    });

    Ok((
        tokio_stream::wrappers::UnboundedReceiverStream::new(stream_receiver),
        tokio_util::sync::PollSender::new(tun_sink),
    ))
}

/// Helper method to convert a [`wintun::Error`] to a [`std::io::Error`].
fn wintun_to_io_error(err: wintun::Error) -> io::Error {
    match err {
        wintun::Error::Io(e) => e,
        _ => io::Error::new(io::ErrorKind::Other, "unknown wintun error"),
    }
}

/// Set an address on an interface by shelling out to `netsh`
///
/// We assume this is an IPv6 address.
fn add_address(adapter_name: &str, subnet: Subnet, route_subnet: Subnet) -> Result<(), io::Error> {
    let exit_code = std::process::Command::new("netsh")
        .args([
            "interface",
            "ipv6",
            "set",
            "address",
            adapter_name,
            &format!("{}/{}", subnet.address(), route_subnet.prefix_len()),
        ])
        .spawn()?
        .wait()?;

    match exit_code.code() {
        Some(0) => Ok(()),
        Some(x) => Err(io::Error::from_raw_os_error(x)),
        None => {
            warn!("Failed to determine `netsh` exit status");
            Ok(())
        }
    }
}

fn set_adapter_mtu(name: &str, mtu: usize) -> Result<(), io::Error> {
    let args = &[
        "interface",
        "ipv6",
        "set",
        "subinterface",
        &format!("\"{}\"", name),
        &format!("mtu={}", mtu),
        "store=persistent",
    ];

    let exit_code = std::process::Command::new("netsh")
        .args(args)
        .spawn()?
        .wait()?;

    match exit_code.code() {
        Some(0) => Ok(()),
        Some(x) => Err(io::Error::from_raw_os_error(x)),
        None => {
            warn!("Failed to determine `netsh` exit status");
            Ok(())
        }
    }
}
