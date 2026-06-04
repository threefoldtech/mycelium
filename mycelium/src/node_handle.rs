//! Synchronous wrapper around a [`Node`] running on a background Tokio runtime.
//!
//! This module provides [`NodeHandle`], which spawns a mycelium node in a
//! dedicated thread with its own Tokio runtime. All access goes through the
//! handle, which exposes blocking methods suitable for use from non-async
//! contexts (AIDL Binder threads, C FFI, etc.).

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{debug, error, info};

use crate::metrics::NoMetrics;
use crate::{Config, Node};

// ── Error type ──────────────────────────────────────────────────────────────

/// Errors that can occur when starting a [`NodeHandle`].
#[derive(Debug)]
pub enum NodeError {
    /// The background thread panicked or failed to start the Tokio runtime.
    ThreadPanic,
    /// The mycelium [`Node`] could not be created.
    NodeCreate(String),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeError::ThreadPanic => write!(f, "node thread panicked"),
            NodeError::NodeCreate(e) => write!(f, "failed to create node: {e}"),
        }
    }
}

impl std::error::Error for NodeError {}

// ── TUN setup (Android only) ────────────────────────────────────────────────

/// Open and fully configure a TUN file descriptor on Android.
///
/// The `tun/android.rs` data-plane code expects an already-configured fd
/// (the same contract the `mobile/` crate satisfies via `VpnService.Builder`).
/// For the FFI path there is no VpnService doing that work, so this function
/// does it: opens `/dev/tun`, sets `IFF_TUN | IFF_NO_PI` via `TUNSETIFF`,
/// assigns `node_addr` with `prefix_len` (`/7` causes the kernel to install
/// the on-link route for the global mycelium subnet), sets the MTU, and
/// brings the interface up.
///
/// Requires `CAP_NET_ADMIN`.
#[cfg(target_os = "android")]
pub fn create_tun_fd(
    tun_name: &str,
    node_addr: std::net::Ipv6Addr,
    prefix_len: u8,
    mtu: i32,
) -> Result<i32, std::io::Error> {
    const TUNSETIFF: libc::c_ulong = 0x400454ca;
    const IFF_TUN: libc::c_short = 0x0001;
    const IFF_NO_PI: libc::c_short = 0x1000;

    // SAFETY: the path is a static NUL-terminated byte string; `open` has no
    // other preconditions.
    let fd = unsafe { libc::open(b"/dev/tun\0".as_ptr() as *const libc::c_char, libc::O_RDWR) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut ifr = [0u8; 40];
    let name_bytes = tun_name.as_bytes();
    let len = name_bytes.len().min(15);
    ifr[..len].copy_from_slice(&name_bytes[..len]);
    let flags: i16 = IFF_TUN | IFF_NO_PI;
    ifr[16] = (flags & 0xff) as u8;
    ifr[17] = ((flags >> 8) & 0xff) as u8;

    // SAFETY: `fd` is a valid file descriptor that we just opened. `ifr` is a
    // 40-byte buffer matching the kernel's `struct ifreq` layout, with name
    // bytes (offset 0..15), a NUL terminator (offset 15, left at zero), and
    // `ifr_flags` (offset 16..18) populated; remaining bytes are zero, which
    // is what TUNSETIFF expects for unused union members.
    let ret = unsafe { libc::ioctl(fd, TUNSETIFF as i32, ifr.as_ptr()) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        // SAFETY: `fd` is the open fd from the `open` call above; nothing else
        // can reference it because it has not escaped this function.
        unsafe { libc::close(fd) };
        return Err(err);
    }

    if let Err(e) = configure_tun_interface(tun_name, node_addr, prefix_len, mtu) {
        // SAFETY: `fd` is the open fd from the `open` call above; nothing else
        // can reference it because it has not escaped this function.
        unsafe { libc::close(fd) };
        return Err(e);
    }

    Ok(fd)
}

/// Assign address, set MTU and bring the named interface up via an
/// `AF_INET6` control socket. `SIOCSIFADDR` for IPv6 requires `AF_INET6`.
#[cfg(target_os = "android")]
fn configure_tun_interface(
    tun_name: &str,
    node_addr: std::net::Ipv6Addr,
    prefix_len: u8,
    mtu: i32,
) -> std::io::Result<()> {
    // SAFETY: `socket(2)` has no preconditions; the arguments are valid
    // constants exported by libc.
    let sock = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let result = (|| -> std::io::Result<()> {
        // SAFETY: `libc::ifreq` is a `repr(C)` aggregate of POD types and a
        // C union; the all-zero bit pattern is a valid value of every
        // variant (the union members are all integers or fixed-size byte
        // arrays), and the kernel ignores fields not consumed by the
        // specific ioctl request.
        let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
        let name_bytes = tun_name.as_bytes();
        let copy_len = name_bytes.len().min(libc::IFNAMSIZ - 1);
        for (dst, &src) in ifr.ifr_name[..copy_len].iter_mut().zip(name_bytes) {
            *dst = src as libc::c_char;
        }

        // SAFETY: `sock` is a valid socket fd opened above; `ifr` is fully
        // initialised with the interface name in `ifr_name` (NUL-terminated:
        // we copied at most IFNAMSIZ-1 bytes into a zero-initialised buffer)
        // — the layout expected by SIOCGIFINDEX.
        if unsafe { libc::ioctl(sock, libc::SIOCGIFINDEX as _, &mut ifr) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: SIOCGIFINDEX populates the `ifru_ifindex` union variant on
        // success; the ioctl returned >= 0 above.
        let ifindex = unsafe { ifr.ifr_ifru.ifru_ifindex };

        let ifr6 = libc::in6_ifreq {
            ifr6_addr: libc::in6_addr {
                s6_addr: node_addr.octets(),
            },
            ifr6_prefixlen: prefix_len as u32,
            ifr6_ifindex: ifindex,
        };
        // SAFETY: `sock` is a valid AF_INET6 socket fd (required for IPv6
        // SIOCSIFADDR); `ifr6` is a fully initialised `in6_ifreq` with the
        // address, prefix length, and interface index expected by the ioctl.
        if unsafe { libc::ioctl(sock, libc::SIOCSIFADDR as _, &ifr6) } < 0 {
            return Err(std::io::Error::last_os_error());
        }

        ifr.ifr_ifru.ifru_mtu = mtu;
        // SAFETY: `sock` is a valid socket fd; `ifr` has the interface name
        // set in `ifr_name` and the MTU written into the `ifru_mtu` union
        // variant — the layout SIOCSIFMTU expects.
        if unsafe { libc::ioctl(sock, libc::SIOCSIFMTU as _, &ifr) } < 0 {
            return Err(std::io::Error::last_os_error());
        }

        // SAFETY: `sock` is a valid socket fd; `ifr_name` is still set from
        // the SIOCGIFINDEX call above (the kernel only writes the `ifr_ifru`
        // union on success, leaving `ifr_name` intact).
        if unsafe { libc::ioctl(sock, libc::SIOCGIFFLAGS as _, &mut ifr) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: SIOCGIFFLAGS populates the `ifru_flags` union variant on
        // success; the ioctl returned >= 0 above.
        ifr.ifr_ifru.ifru_flags = unsafe { ifr.ifr_ifru.ifru_flags } | libc::IFF_UP as libc::c_short;
        // SAFETY: `sock` is a valid socket fd; `ifr` has the interface name
        // set and the updated flags written into the `ifru_flags` union
        // variant.
        if unsafe { libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &ifr) } < 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(())
    })();

    // SAFETY: `sock` is the fd from the `socket` call above; it has not
    // escaped this function and is no longer used.
    unsafe { libc::close(sock) };
    result
}

// ── NodeHandle ──────────────────────────────────────────────────────────────

/// Upper bound on how long node teardown waits for the background Tokio
/// runtime to shut down. Asynchronous tasks (including the TUN reader/writer)
/// are cancelled immediately when the runtime is dropped; this timeout only
/// bounds the wait on any outstanding `spawn_blocking` work so a stuck
/// blocking task cannot hang teardown forever.
const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Handle to a running mycelium node. Holds a reference to the node for direct
/// method calls and the Tokio runtime handle to drive them from non-async
/// threads (e.g. Binder threads, C FFI callbacks).
pub struct NodeHandle {
    node: Arc<Mutex<Node<NoMetrics>>>,
    rt_handle: tokio::runtime::Handle,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Background OS thread that owns the Tokio runtime. Joined on drop so
    /// teardown is synchronous: once the join completes the runtime is gone
    /// and any TUN interface the node created has been removed.
    thread: Option<std::thread::JoinHandle<()>>,
    /// On Android the `tun` crate does not close the TUN file descriptor on
    /// drop (it assumes the fd is owned by Android's `VpnService`). Mycelium
    /// opens this fd itself via [`create_tun_fd`], so it owns it and must
    /// close it during teardown — otherwise the kernel keeps the
    /// non-persistent TUN interface alive. Closed in [`Drop`], after the
    /// background runtime has been joined.
    #[cfg(target_os = "android")]
    tun_fd: Option<i32>,
}

impl NodeHandle {
    /// Spawn the Tokio runtime and mycelium node in a background thread.
    ///
    /// Blocks the calling thread until the node is ready (or fails to start).
    /// The caller provides a fully constructed [`Config`].
    pub fn start(config: Config<NoMetrics>) -> Result<Self, NodeError> {
        // On Android mycelium opens the TUN fd itself (see `create_tun_fd`),
        // so the handle owns it and is responsible for closing it on drop.
        #[cfg(target_os = "android")]
        let tun_fd = config.tun_fd;

        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let thread = std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("Failed to build Tokio runtime: {e}");
                    let _ = result_tx.send(Err(NodeError::ThreadPanic));
                    return;
                }
            };

            let handle = rt.handle().clone();

            rt.block_on(async move {
                match Node::new(config).await {
                    Err(e) => {
                        error!("Failed to create node: {e}");
                        let _ = result_tx.send(Err(NodeError::NodeCreate(e.to_string())));
                    }
                    Ok(node) => {
                        info!("mycelium node started");
                        let node = Arc::new(Mutex::new(node));
                        let _ = result_tx.send(Ok((Arc::clone(&node), handle)));
                        let _ = shutdown_rx.await;
                        info!("mycelium node stopped");
                    }
                }
            });

            // Explicit, bounded teardown. Dropping the runtime cancels every
            // asynchronous task and drops the TUN device, then waits up to
            // SHUTDOWN_TIMEOUT for any `spawn_blocking` work to finish.
            debug!("shutting down node runtime");
            rt.shutdown_timeout(SHUTDOWN_TIMEOUT);
            debug!("node runtime shut down");
        });

        let (node, rt_handle) = match result_rx.recv() {
            Ok(Ok(pair)) => pair,
            other => {
                // The node failed to start. Wait for the background runtime
                // to finish tearing down, then release the TUN fd we opened
                // (on Android nothing else will close it).
                let _ = thread.join();
                #[cfg(target_os = "android")]
                if let Some(fd) = tun_fd {
                    if fd >= 0 {
                        // SAFETY: `fd` came from `create_tun_fd`; the runtime
                        // that used it has been joined, so it is unreferenced.
                        unsafe { libc::close(fd) };
                    }
                }
                return Err(match other {
                    Ok(Err(e)) => e,
                    _ => NodeError::ThreadPanic,
                });
            }
        };

        Ok(NodeHandle {
            node,
            rt_handle,
            shutdown_tx: Some(shutdown_tx),
            thread: Some(thread),
            #[cfg(target_os = "android")]
            tun_fd,
        })
    }

    /// Signal the node to shut down. Non-blocking; the background thread will
    /// exit asynchronously.
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }

    /// Returns `true` if the node has not been stopped.
    pub fn is_running(&self) -> bool {
        self.shutdown_tx.is_some()
    }

    /// Reference to the underlying [`Node`], wrapped in an async [`Mutex`].
    pub fn node(&self) -> &Arc<Mutex<Node<NoMetrics>>> {
        &self.node
    }

    /// Handle to the background Tokio runtime. Use this with
    /// [`block_on`](tokio::runtime::Handle::block_on) to drive async node
    /// methods from synchronous code.
    pub fn rt(&self) -> &tokio::runtime::Handle {
        &self.rt_handle
    }
}

impl Drop for NodeHandle {
    /// Shut the node down and block until its background runtime — and any
    /// network interface it created — has been fully torn down.
    ///
    /// NOTE: this joins the background OS thread, so it must not be invoked
    /// from within the node's own Tokio runtime (a thread cannot join
    /// itself). Calling it from an external thread — as the C FFI layer
    /// does — is safe.
    fn drop(&mut self) {
        // Signal shutdown if `stop()` has not already done so.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Block until the background thread has finished tearing down the
        // runtime. Dropping the runtime cancels every task, including the
        // TUN reader/writer that owns the tun device.
        if let Some(thread) = self.thread.take() {
            if let Err(e) = thread.join() {
                error!("node background thread panicked during shutdown: {e:?}");
            }
        }
        // On Android the `tun` crate does not close the TUN fd on drop (it
        // assumes Android's `VpnService` owns it). Mycelium opened this fd
        // itself via `create_tun_fd`, so it must close it here — after the
        // join above guarantees the runtime, and the tun device using the
        // fd, are gone. The interface is non-persistent, so closing the last
        // fd makes the kernel remove it.
        #[cfg(target_os = "android")]
        if let Some(fd) = self.tun_fd.take() {
            if fd >= 0 {
                // SAFETY: `fd` came from `create_tun_fd`; the runtime that
                // used it has been joined, so nothing else references it.
                unsafe { libc::close(fd) };
            }
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Parse a proxy address string (`"ip:port"`) into a [`SocketAddr`].
/// Returns `None` for an empty string (auto-select).
pub fn parse_proxy_remote(remote: &str) -> Result<Option<SocketAddr>, String> {
    if remote.is_empty() {
        Ok(None)
    } else {
        remote
            .parse::<SocketAddr>()
            .map(Some)
            .map_err(|e| e.to_string())
    }
}

/// Default SOCKS5 port used when listing known proxies.
pub const DEFAULT_SOCKS_PORT: u16 = 1080;
