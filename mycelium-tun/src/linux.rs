//! Linux TUN device implementation using ioctls and tokio's AsyncFd.
//!
//! The device is opened with `IFF_VNET_HDR` to enable GSO/GRO offloading. Each read/write on
//! the fd carries a 10-byte `virtio_net_hdr` prefix, handled internally. Callers only see
//! normal IP packets.

use std::io;
use std::net::Ipv6Addr;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};

use nix::net::if_::InterfaceFlags;
use tokio::io::unix::AsyncFd;
use tracing::debug;

use crate::offload::{self, VIRTIO_NET_HDR_LEN, VirtioNetHdr};

/// `TUNSETIFF` ioctl request code. Not exposed by libc or nix.
const TUNSETIFF: libc::c_ulong = 0x400454ca;
/// `TUNSETOFFLOAD` ioctl request code.
const TUNSETOFFLOAD: libc::c_ulong = 0x400454d0;

/// `IFF_VNET_HDR` — prepend virtio_net_hdr to every packet.
const IFF_VNET_HDR: libc::c_short = 0x4000;

// TUNSETOFFLOAD feature flags.
const TUN_F_CSUM: libc::c_ulong = 0x01;
const TUN_F_TSO4: libc::c_ulong = 0x02;
const TUN_F_TSO6: libc::c_ulong = 0x04;
const TUN_F_USO4: libc::c_ulong = 0x20;
const TUN_F_USO6: libc::c_ulong = 0x40;

/// Maximum read buffer: 65535 (max IP packet) + 12 (virtio header).
const READ_BUF_SIZE: usize = 65535 + VIRTIO_NET_HDR_LEN;

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

nix::ioctl_write_ptr_bad!(
    /// Set the transmit queue length on a network interface.
    siocsiftxqlen,
    libc::SIOCSIFTXQLEN as libc::c_ulong,
    libc::ifreq
);

nix::ioctl_read_bad!(
    /// Get the flags on a network interface.
    siocgifflags,
    libc::SIOCGIFFLAGS as libc::c_ulong,
    libc::ifreq
);

nix::ioctl_write_ptr_bad!(
    /// Set the flags on a network interface.
    siocsifflags,
    libc::SIOCSIFFLAGS as libc::c_ulong,
    libc::ifreq
);

/// A Linux TUN device.
///
/// The device is opened with `IFF_TUN | IFF_NO_PI | IFF_VNET_HDR` flags, meaning it operates
/// at the IP layer (layer 3) with no packet information header but with a virtio_net_hdr
/// prefix for GSO/GRO offloading.
///
/// Use [`Tun::split`] to obtain a [`ReadHalf`] and [`WriteHalf`] that can be used concurrently
/// from separate tasks without locking.
pub struct Tun {
    fd: OwnedFd,
    name: String,
    uso_enabled: bool,
}

impl Tun {
    /// Create a new TUN device.
    ///
    /// If `name` is empty, the kernel assigns a name (typically "tun0", "tun1", ...). The actual
    /// assigned name can be retrieved with [`Tun::name`].
    ///
    /// The device is configured with `IFF_VNET_HDR` and TCP/UDP segmentation offload is enabled
    /// via `TUNSETOFFLOAD`.
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
        ifr.ifr_ifru.ifru_flags = flags.bits() as libc::c_short | IFF_VNET_HDR;

        // SAFETY: file is a valid fd, ifr is properly initialized.
        unsafe { tunsetiff(file.as_raw_fd(), &ifr) }.map_err(io::Error::from)?;

        let actual_name = ifreq_name(&ifr)?;
        debug!(name = %actual_name, "created TUN device");

        // Enable offloading: CSUM + TSO4 + TSO6 are mandatory.
        let offload_flags = TUN_F_CSUM | TUN_F_TSO4 | TUN_F_TSO6;
        // SAFETY: file is a valid TUN fd.
        let ret = unsafe { libc::ioctl(file.as_raw_fd(), TUNSETOFFLOAD, offload_flags) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        debug!(name = %actual_name, "enabled TSO offload");

        // Attempt USO4/USO6 — requires Linux 6.2+. Failure is non-fatal.
        let uso_flags = offload_flags | TUN_F_USO4 | TUN_F_USO6;
        // SAFETY: file is a valid TUN fd.
        let ret = unsafe { libc::ioctl(file.as_raw_fd(), TUNSETOFFLOAD, uso_flags) };
        let uso_enabled = ret == 0;
        if uso_enabled {
            debug!(name = %actual_name, "enabled USO offload");
        }

        // Transfer ownership of the fd from File to OwnedFd without closing it.
        // SAFETY: raw_fd is valid, we just obtained it from file.
        let owned_fd = unsafe { OwnedFd::from_raw_fd(file.into_raw_fd()) };

        // Set non-blocking now.
        // If we later call dup(), the flag is inherited.
        set_nonblocking(&owned_fd)?;

        Ok(Tun {
            fd: owned_fd,
            name: actual_name,
            uso_enabled,
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

    /// Set the transmit queue length on the TUN interface.
    pub fn set_txqueuelen(&self, qlen: u32) -> io::Result<()> {
        let sock = ioctl_socket()?;

        let mut ifr = self.new_ifreq_with_name();
        // The kernel defines ifr_qlen as ifr_ifru.ifru_ivalue which is a c_int at the same
        // offset as ifru_mtu. libc does not expose ifru_ivalue, but ifru_mtu is the same type
        // and occupies the same union slot.
        ifr.ifr_ifru.ifru_mtu = qlen as libc::c_int;

        // SAFETY: sock is a valid fd, ifr is properly initialized.
        unsafe { siocsiftxqlen(sock.as_raw_fd(), &ifr) }.map_err(io::Error::from)?;

        debug!(name = %self.name, qlen, "set TUN txqueuelen");
        Ok(())
    }

    /// Bring the TUN interface up.
    pub fn set_up(&self) -> io::Result<()> {
        let sock = ioctl_socket()?;
        let mut ifr = self.new_ifreq_with_name();

        // Read current flags.
        // SAFETY: sock is a valid fd, ifr is properly initialized with the interface name.
        unsafe { siocgifflags(sock.as_raw_fd(), &mut ifr) }.map_err(io::Error::from)?;

        // Set IFF_UP without clobbering existing flags.
        // SAFETY: siocgifflags populates ifr_ifru.ifru_flags on success.
        ifr.ifr_ifru.ifru_flags =
            unsafe { ifr.ifr_ifru.ifru_flags } | libc::IFF_UP as libc::c_short;

        // SAFETY: sock is a valid fd, ifr is properly initialized.
        unsafe { siocsifflags(sock.as_raw_fd(), &ifr) }.map_err(io::Error::from)?;

        debug!(name = %self.name, "brought TUN interface up");
        Ok(())
    }

    /// Split the TUN device into a read half and a write half.
    ///
    /// Each half wraps an independent file descriptor (created via `dup()`), so they can be used
    /// concurrently from separate tasks without any locking.
    ///
    /// # Panics
    ///
    /// Panics if called outside of a tokio runtime context.
    pub fn split(self) -> io::Result<(ReadHalf, WriteHalf)> {
        // Duplicate the fd so each half has its own independent descriptor.
        // SAFETY: self.fd is a valid fd.
        let dup_fd = unsafe { libc::dup(self.fd.as_raw_fd()) };
        if dup_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: dup_fd is valid, we just created it.
        let write_fd = unsafe { OwnedFd::from_raw_fd(dup_fd) };

        let read_half = ReadHalf {
            fd: AsyncFd::new(self.fd)?,
            raw_buf: vec![0u8; READ_BUF_SIZE],
        };
        let write_half = WriteHalf {
            fd: AsyncFd::new(write_fd)?,
            write_buf: vec![0u8; READ_BUF_SIZE],
            uso_enabled: self.uso_enabled,
        };

        Ok((read_half, write_half))
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
}

/// The read half of a split TUN device.
///
/// Obtained from [`Tun::split`]. Can be used concurrently with [`WriteHalf`].
pub struct ReadHalf {
    fd: AsyncFd<OwnedFd>,
    /// Internal buffer for reading raw data (virtio_net_hdr + payload) from the kernel.
    raw_buf: Vec<u8>,
}

impl ReadHalf {
    /// Read one or more packets from the TUN device.
    ///
    /// The kernel may deliver a GRO-coalesced super-packet, which is split into individual IP
    /// packets written into `bufs`. The length of each packet is recorded in the corresponding
    /// entry of `sizes`. Returns the number of packets written.
    ///
    /// `bufs` and `sizes` must have the same length.
    pub async fn read(&mut self, bufs: &mut [&mut [u8]], sizes: &mut [usize]) -> io::Result<usize> {
        assert_eq!(
            bufs.len(),
            sizes.len(),
            "bufs and sizes must have the same length"
        );

        let n = self.read_raw().await?;
        offload::gso_split(&self.raw_buf[..n], bufs, sizes)
    }

    /// Perform a single raw read from the TUN fd into the internal buffer.
    async fn read_raw(&mut self) -> io::Result<usize> {
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|inner| {
                // SAFETY: inner is a valid fd, raw_buf is valid for raw_buf.len() bytes.
                let n = unsafe {
                    libc::read(
                        inner.as_raw_fd(),
                        self.raw_buf.as_mut_ptr().cast(),
                        self.raw_buf.len(),
                    )
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

/// The write half of a split TUN device.
///
/// Obtained from [`Tun::split`]. Can be used concurrently with [`ReadHalf`].
pub struct WriteHalf {
    fd: AsyncFd<OwnedFd>,
    /// Internal buffer for building coalesced packets (virtio_net_hdr + payload).
    write_buf: Vec<u8>,
    /// Whether USO (UDP Segmentation Offload) is available on this device.
    uso_enabled: bool,
}

impl WriteHalf {
    /// Write one or more packets to the TUN device.
    ///
    /// Compatible packets are coalesced with GSO for fewer syscalls. Each entry in `pkts` must
    /// be a complete IP packet.
    pub async fn write(&mut self, pkts: &[&[u8]]) -> io::Result<()> {
        if pkts.is_empty() {
            return Ok(());
        }

        let mut remaining = pkts;

        while remaining.len() > 1 {
            match offload::gro_coalesce(
                remaining,
                &mut self.write_buf[VIRTIO_NET_HDR_LEN..],
                self.uso_enabled,
            ) {
                Ok((len, vhdr, consumed)) => {
                    vhdr.encode(&mut self.write_buf[..VIRTIO_NET_HDR_LEN]);
                    self.write_raw(&self.write_buf[..VIRTIO_NET_HDR_LEN + len])
                        .await?;
                    remaining = &remaining[consumed..];
                }
                Err(_) => {
                    // First packet can't be coalesced — write it individually and advance.
                    let pkt = remaining[0];
                    let total = VIRTIO_NET_HDR_LEN + pkt.len();
                    VirtioNetHdr::none().encode(&mut self.write_buf[..VIRTIO_NET_HDR_LEN]);
                    self.write_buf[VIRTIO_NET_HDR_LEN..total].copy_from_slice(pkt);
                    self.write_raw(&self.write_buf[..total]).await?;
                    remaining = &remaining[1..];
                }
            }
        }

        // Write any single remaining packet.
        if let Some(pkt) = remaining.first() {
            let total = VIRTIO_NET_HDR_LEN + pkt.len();
            VirtioNetHdr::none().encode(&mut self.write_buf[..VIRTIO_NET_HDR_LEN]);
            self.write_buf[VIRTIO_NET_HDR_LEN..total].copy_from_slice(pkt);
            self.write_raw(&self.write_buf[..total]).await?;
        }

        Ok(())
    }

    /// Perform a single raw write to the TUN fd.
    async fn write_raw(&self, buf: &[u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|inner| {
                // SAFETY: inner is a valid fd, buf is valid for buf.len() bytes.
                let n = unsafe { libc::write(inner.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
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
    let name_bytes: Vec<u8> = ifr.ifr_name[..nul_pos].iter().map(|&c| c as u8).collect();
    String::from_utf8(name_bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid interface name"))
}

/// Set a file descriptor to non-blocking mode.
fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: fd is valid.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is valid, flags is the current flags with O_NONBLOCK added.
    let ret = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
