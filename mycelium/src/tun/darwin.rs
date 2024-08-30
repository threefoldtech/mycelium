//! macos specific tun interface setup.

use std::{
    ffi::CString,
    io::{self, IoSlice},
    net::IpAddr,
    os::fd::AsRawFd,
    str::FromStr,
};

use futures::{Sink, Stream};
use nix::sys::socket::SockaddrIn6;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    select,
    sync::mpsc,
};
use tracing::{debug, error, info, warn};

use crate::crypto::PacketBuffer;
use crate::subnet::Subnet;
use crate::tun::TunConfig;

// TODO
const LINK_MTU: i32 = 1400;

/// The 4 byte packet header written before a packet is sent on the TUN
// TODO: figure out structure and values, but for now this seems to work.
const HEADER: [u8; 4] = [0, 0, 0, 30];

const IN6_IFF_NODAD: u32 = 0x0020; // netinet6/in6_var.h
const IN6_IFF_SECURED: u32 = 0x0400; // netinet6/in6_var.h
const ND6_INFINITE_LIFETIME: u32 = 0xFFFFFFFF; // netinet6/nd6.h

/// Wrapper for an OS-specific interface name
// Allways hold the max size of an interface. This includes the 0 byte for termination.
// repr transparent so this can be used with libc calls.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct IfaceName([libc::c_char; libc::IFNAMSIZ as _]);

/// Wrapped interface handle.
#[derive(Clone, Copy)]
struct Iface {
    /// Name of the interface
    iface_name: IfaceName,
}

/// Struct to add IPv6 route to interface
#[repr(C)]
pub struct IfaliasReq {
    ifname: IfaceName,
    addr: SockaddrIn6,
    dst_addr: SockaddrIn6,
    mask: SockaddrIn6,
    flags: u32,
    lifetime: AddressLifetime,
}

#[repr(C)]
pub struct AddressLifetime {
    /// Not used for userspace -> kernel space
    expire: libc::time_t,
    /// Not used for userspace -> kernel space
    preferred: libc::time_t,
    vltime: u32,
    pltime: u32,
}

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
    let tun_name = find_available_utun_name(&tun_config.name)?;

    let mut tun = match create_tun_interface(&tun_name) {
        Ok(tun) => tun,
        Err(e) => {
            error!(tun_name=%tun_name, err=%e, "Could not create TUN device. Make sure the name is not yet in use, and you have sufficient privileges to create a network device");
            return Err(e);
        }
    };
    let iface = Iface::by_name(&tun_name)?;
    iface.add_address(tun_config.node_subnet, tun_config.route_subnet)?;

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

/// Checks if a name is valid for a utun interface
///
/// Rules:
///   - must start with "utun"
///   - followed by only digits
///   - 15 chars total at most
fn validate_utun_name(input: &str) -> bool {
    if input.len() > 15 {
        return false;
    }
    if !input.starts_with("utun") {
        return false;
    }

    input
        .strip_prefix("utun")
        .expect("We just checked that name starts with 'utun' so this is always some")
        .parse::<u64>()
        .is_ok()
}

/// Validates the user-supplied TUN interface name
///
/// - If the name is valid and not in use, it will be the TUN name
/// - If the name is valid but already in use, an error will be thrown
/// - If the name is not valid, we try to find the first freely available TUN name
fn find_available_utun_name(preferred_name: &str) -> Result<String, io::Error> {
    // Get the list of existing utun interfaces.
    let interfaces = netdev::get_interfaces();
    let utun_interfaces: Vec<_> = interfaces
        .iter()
        .filter_map(|iface| {
            if iface.name.starts_with("utun") {
                Some(iface.name.as_str())
            } else {
                None
            }
        })
        .collect();

    // Check if the preferred name is valid and not in use.
    if validate_utun_name(preferred_name) && !utun_interfaces.contains(&preferred_name) {
        return Ok(preferred_name.to_string());
    }

    // If the preferred name is invalid or already in use, find the first available utun name.
    if !validate_utun_name(preferred_name) {
        warn!(tun_name=%preferred_name, "Invalid TUN name. Looking for the first available TUN name");
    } else {
        warn!(tun_name=%preferred_name, "TUN name already in use. Looking for the next available TUN name.");
    }

    // Extract and sort the utun numbers.
    let mut utun_numbers = utun_interfaces
        .iter()
        .filter_map(|iface| iface[4..].parse::<usize>().ok())
        .collect::<Vec<_>>();

    utun_numbers.sort_unstable();

    // Find the first available utun index.
    let mut first_free_index = 0;
    for (i, &num) in utun_numbers.iter().enumerate() {
        if num != i {
            first_free_index = i;
            break;
        }
        first_free_index = i + 1;
    }

    // Create new utun name based on the first free index.
    let new_utun_name = format!("utun{}", first_free_index);
    if validate_utun_name(&new_utun_name) {
        info!(tun_name=%new_utun_name, "Automatically assigned TUN name.");
        Ok(new_utun_name)
    } else {
        error!("No available TUN name found");
        Err(io::Error::new(
            io::ErrorKind::Other,
            "No available TUN name",
        ))
    }
}

/// Create a new TUN interface
fn create_tun_interface(name: &str) -> Result<tun::AsyncDevice, Box<dyn std::error::Error>> {
    let mut config = tun::Configuration::default();
    config
        .name(name)
        .layer(tun::Layer::L3)
        .mtu(LINK_MTU)
        .queues(1)
        .up();
    let tun = tun::create_as_async(&config)?;

    Ok(tun)
}

impl IfaceName {
    fn as_ptr(&self) -> *const libc::c_char {
        self.0.as_ptr()
    }
}

impl FromStr for IfaceName {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Equal len is not allowed because we need to add the 0 byte terminator.
        if s.len() >= libc::IFNAMSIZ {
            return Err("Interface name too long");
        }

        // TODO: Is this err possible in a &str?
        let raw_name = CString::new(s).map_err(|_| "Interface name contains 0 byte")?;
        let mut backing = [0; libc::IFNAMSIZ];
        let name_bytes = raw_name.to_bytes_with_nul();
        backing[..name_bytes.len()].copy_from_slice(name_bytes);
        // SAFETY: This doesn't do any weird things with the bits when converting from u8 to i8
        let backing = unsafe { std::mem::transmute::<[u8; 16], [i8; 16]>(backing) };
        Ok(Self(backing))
    }
}

impl Iface {
    /// Retrieve the link index of an interface with the given name
    fn by_name(name: &str) -> Result<Iface, Box<dyn std::error::Error>> {
        let iface_name: IfaceName = name.parse()?;
        match unsafe { libc::if_nametoindex(iface_name.as_ptr()) } {
            0 => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "interface not found",
            ))?,
            _ => Ok(Iface { iface_name }),
        }
    }

    /// Add an address to an interface.
    ///
    /// # Panics
    ///
    /// Only IPv6 is supported, this function will panic when adding an IPv4 subnet.
    fn add_address(
        &self,
        subnet: Subnet,
        route_subnet: Subnet,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let addr = if let IpAddr::V6(addr) = subnet.address() {
            addr
        } else {
            panic!("IPv4 subnets are not supported");
        };
        let mask_addr = if let IpAddr::V6(mask) = route_subnet.mask() {
            mask
        } else {
            // We already know we are IPv6 here
            panic!("IPv4 routes are not supported");
        };

        let sock_addr = SockaddrIn6::from(std::net::SocketAddrV6::new(addr, 0, 0, 0));
        let mask = SockaddrIn6::from(std::net::SocketAddrV6::new(mask_addr, 0, 0, 0));

        let req = IfaliasReq {
            ifname: self.iface_name,
            addr: sock_addr,
            // SAFETY: kernel expects this to be fully zeroed
            dst_addr: unsafe { std::mem::zeroed() },
            mask,
            flags: IN6_IFF_NODAD | IN6_IFF_SECURED,
            lifetime: AddressLifetime {
                expire: 0,
                preferred: 0,
                vltime: ND6_INFINITE_LIFETIME,
                pltime: ND6_INFINITE_LIFETIME,
            },
        };

        let sock = random_socket()?;

        match unsafe { siocaifaddr_in6(sock.as_raw_fd(), &req) } {
            Err(e) => {
                error!("Failed to add ipv6 addresst to interface {e}");
                Err(std::io::Error::last_os_error())?
            }
            Ok(_) => {
                debug!("Added {subnet} to tun interfacel");
                Ok(())
            }
        }
    }
}
// Create a socket to talk to the kernel.
fn random_socket() -> Result<std::net::UdpSocket, std::io::Error> {
    std::net::UdpSocket::bind("[::1]:0")
}

nix::ioctl_write_ptr!(
    /// Add an IPv6 subnet to an interface.
    siocaifaddr_in6,
    b'i',
    26,
    IfaliasReq
);
