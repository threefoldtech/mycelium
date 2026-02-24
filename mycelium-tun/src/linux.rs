//! Linux TUN device implementation using ioctls and tokio's AsyncFd.

use std::io;
use std::net::Ipv6Addr;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};

use nix::net::if_::InterfaceFlags;
use tokio::io::unix::AsyncFd;
use tracing::debug;

/// `TUNSETIFF` ioctl request code. Not exposed by libc or nix.
const TUNSETIFF: libc::c_ulong = 0x400454ca;

nix::ioctl_write_ptr_bad!(
    /// Configure a TUN device (set name and flags).
    tunsetiff,
    TUNSETIFF,
    libc::ifreq
);

nix::ioctl_write_ptr_bad!(
    /// Set the MTU on a network interface.
    siocsifmtu,
    libc::SIOCSIFMTU as libc::c_ulong,
    libc::ifreq
);

nix::ioctl_read_bad!(
    /// Get the index of a network interface by name.
    siocgifindex,
    libc::SIOCGIFINDEX as libc::c_ulong,
    libc::ifreq
);

nix::ioctl_write_ptr_bad!(
    /// Set an address on a network interface.
    siocsifaddr,
    libc::SIOCSIFADDR as libc::c_ulong,
    libc::in6_ifreq
);

/// A Linux TUN device.
///
/// The device is opened with `IFF_TUN | IFF_NO_PI` flags, meaning it operates at the IP layer
/// (layer 3) with no packet information header prepended.
///
/// All read/write operations are async, backed by tokio's [`AsyncFd`].
pub struct Tun {
    fd: AsyncFd<OwnedFd>,
    name: String,
}

impl Tun {
    /// Create a new TUN device.
    ///
    /// If `name` is empty, the kernel assigns a name (typically "tun0", "tun1", ...). The actual
    /// assigned name can be retrieved with [`Tun::name`].
    ///
    /// # Panics
    ///
    /// Panics if called outside of a tokio runtime context.
    pub fn new(name: &str) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/net/tun")?;

        let mut ifr = new_ifreq();

        // Copy the requested name, leaving room for the NUL terminator.
        for (dst, &src) in ifr.ifr_name[..libc::IFNAMSIZ - 1]
            .iter_mut()
            .zip(name.as_bytes())
        {
            *dst = src as libc::c_char;
        }

        let flags = InterfaceFlags::IFF_TUN | InterfaceFlags::IFF_NO_PI;
        ifr.ifr_ifru.ifru_flags = flags.bits() as libc::c_short;

        // SAFETY: file is a valid fd, ifr is properly initialized.
        unsafe { tunsetiff(file.as_raw_fd(), &ifr) }.map_err(io::Error::from)?;

        let actual_name = ifreq_name(&ifr)?;
        debug!(name = %actual_name, "created TUN device");

        // Transfer ownership of the fd from File to OwnedFd without closing it.
        // SAFETY: raw_fd is valid, we just obtained it from file.
        let owned_fd = unsafe { OwnedFd::from_raw_fd(file.into_raw_fd()) };
        let async_fd = AsyncFd::new(owned_fd)?;

        Ok(Tun {
            fd: async_fd,
            name: actual_name,
        })
    }

    /// Returns the name of the TUN interface.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Set the MTU on the TUN interface.
    pub fn set_mtu(&self, mtu: u32) -> io::Result<()> {
        let sock = ioctl_socket()?;

        let mut ifr = self.new_ifreq_with_name();
        ifr.ifr_ifru.ifru_mtu = mtu as libc::c_int;

        // SAFETY: sock is a valid fd, ifr is properly initialized.
        unsafe { siocsifmtu(sock.as_raw_fd(), &ifr) }.map_err(io::Error::from)?;

        debug!(name = %self.name, mtu, "set TUN MTU");
        Ok(())
    }

    /// Set an IPv6 address and prefix length on the TUN interface.
    pub fn set_addr(&self, addr: Ipv6Addr, prefix_len: u8) -> io::Result<()> {
        let sock = ioctl_socket()?;
        let if_index = self.interface_index()?;

        let ifr6 = libc::in6_ifreq {
            ifr6_addr: libc::in6_addr {
                s6_addr: addr.octets(),
            },
            ifr6_prefixlen: prefix_len as u32,
            ifr6_ifindex: if_index,
        };

        // SAFETY: sock is a valid AF_INET6 fd, ifr6 is properly initialized.
        unsafe { siocsifaddr(sock.as_raw_fd(), &ifr6) }.map_err(io::Error::from)?;

        debug!(name = %self.name, %addr, prefix_len, "set TUN address");
        Ok(())
    }

    /// Get the kernel interface index for this TUN device.
    fn interface_index(&self) -> io::Result<libc::c_int> {
        let sock = ioctl_socket()?;
        let mut ifr = self.new_ifreq_with_name();

        // SAFETY: sock is a valid fd, ifr is properly initialized with the interface name.
        unsafe { siocgifindex(sock.as_raw_fd(), &mut ifr) }.map_err(io::Error::from)?;

        // SAFETY: siocgifindex populates ifr_ifru.ifru_ifindex on success.
        Ok(unsafe { ifr.ifr_ifru.ifru_ifindex })
    }

    /// Create an `ifreq` with this device's name already filled in.
    fn new_ifreq_with_name(&self) -> libc::ifreq {
        let mut ifr = new_ifreq();
        for (dst, &src) in ifr.ifr_name[..libc::IFNAMSIZ - 1]
            .iter_mut()
            .zip(self.name.as_bytes())
        {
            *dst = src as libc::c_char;
        }
        ifr
    }

    /// Read a packet from the TUN device into `buf`.
    ///
    /// Returns the number of bytes read. The caller should use `&buf[..n]` to access the packet
    /// data.
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|inner| {
                // SAFETY: inner is a valid fd, buf is valid for buf.len() bytes.
                let n = unsafe {
                    libc::read(inner.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }

    /// Write a packet to the TUN device.
    ///
    /// Returns the number of bytes written.
    pub async fn write(&self, buf: &[u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|inner| {
                // SAFETY: inner is a valid fd, buf is valid for buf.len() bytes.
                let n = unsafe {
                    libc::write(inner.as_raw_fd(), buf.as_ptr().cast(), buf.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }
}

/// Create a zero-initialized `ifreq`.
fn new_ifreq() -> libc::ifreq {
    // SAFETY: ifreq is a C struct where all-zeros is a valid representation.
    unsafe { std::mem::zeroed() }
}

/// Extract the interface name from an `ifreq` as a Rust `String`.
fn ifreq_name(ifr: &libc::ifreq) -> io::Result<String> {
    let nul_pos = ifr
        .ifr_name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(libc::IFNAMSIZ);
    let name_bytes: Vec<u8> = ifr.ifr_name[..nul_pos]
        .iter()
        .map(|&c| c as u8)
        .collect();
    String::from_utf8(name_bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid interface name"))
}

/// Create a temporary socket for ioctl operations.
///
/// An IPv6 UDP socket is used since all ioctls in this module work with any socket type, and
/// `SIOCSIFADDR` for IPv6 addresses requires an `AF_INET6` socket.
fn ioctl_socket() -> io::Result<OwnedFd> {
    // SAFETY: standard socket creation.
    let fd = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_DGRAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is valid, we just created it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
