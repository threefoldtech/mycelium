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

/// On Android the `tun` crate expects an already-opened file descriptor.
/// This function opens `/dev/tun`, configures the interface with TUNSETIFF,
/// and returns the raw fd. Requires `CAP_NET_ADMIN`.
#[cfg(target_os = "android")]
pub fn create_tun_fd(tun_name: &str) -> Result<i32, std::io::Error> {
    const TUNSETIFF: libc::c_ulong = 0x400454ca;
    const IFF_TUN: libc::c_short = 0x0001;
    const IFF_NO_PI: libc::c_short = 0x1000;

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

    let ret = unsafe { libc::ioctl(fd, TUNSETIFF as i32, ifr.as_ptr()) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }

    Ok(fd)
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
