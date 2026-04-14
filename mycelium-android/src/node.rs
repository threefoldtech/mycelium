use std::net::SocketAddr;
use std::sync::Arc;

use mycelium::peer_manager::{PeerDiscoveryMode, PeerStats};
use mycelium::{crypto, metrics::Metrics, Config, Node};
use tokio::sync::Mutex;
use tracing::{error, info};

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum NodeError {
    ThreadPanic,
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

// ── NoMetrics ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct NoMetrics;
impl Metrics for NoMetrics {}

// ── TUN setup (Android only) ──────────────────────────────────────────────────

/// On Android the `tun` crate expects an already-opened file descriptor.
/// This function opens `/dev/tun`, configures the interface with TUNSETIFF,
/// and returns the raw fd. Requires `CAP_NET_ADMIN`.
///
/// NOTE: If we continue with this approach, it's better to change the TUN code inside mycelium to do this, similar to linux.
#[cfg(target_os = "android")]
fn create_tun_fd(tun_name: &str) -> Result<i32, std::io::Error> {
    // TUNSETIFF = _IOW('T', 202, int) = 0x400454ca
    const TUNSETIFF: libc::c_ulong = 0x400454ca;
    const IFF_TUN: libc::c_short = 0x0001;
    const IFF_NO_PI: libc::c_short = 0x1000;

    let fd = unsafe { libc::open(b"/dev/tun\0".as_ptr() as *const libc::c_char, libc::O_RDWR) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // struct ifreq: char[16] name, then union starting with short flags
    let mut ifr = [0u8; 40];
    let name_bytes = tun_name.as_bytes();
    let len = name_bytes.len().min(15); // IFNAMSIZ - 1, leave room for null
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

// ── NodeHandle ─────────────────────────────────────────────────────────────────

/// Handle to a running mycelium node. Holds a reference to the node for direct
/// method calls and the Tokio runtime handle to drive them from Binder threads.
pub struct NodeHandle {
    node: Arc<Mutex<Node<NoMetrics>>>,
    rt_handle: tokio::runtime::Handle,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl NodeHandle {
    /// Spawn the Tokio runtime and mycelium node in a background thread.
    /// Blocks the calling thread until the node is ready (or fails to start).
    pub fn start(
        peers: Vec<String>,
        priv_key: [u8; 32],
        enable_dns: bool,
    ) -> Result<Self, NodeError> {
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("Failed to build Tokio runtime: {e}");
                    // Can't send NodeCreate error here since we don't have the box yet,
                    // but ThreadPanic is close enough for a runtime build failure.
                    let _ = result_tx.send(Err(NodeError::ThreadPanic));
                    return;
                }
            };

            let handle = rt.handle().clone();

            rt.block_on(async move {
                let endpoints = peers
                    .iter()
                    .filter_map(|p| p.parse().ok())
                    .collect::<Vec<_>>();

                #[cfg(target_os = "android")]
                let tun_fd = match create_tun_fd("tun0") {
                    Ok(fd) => fd,
                    Err(e) => {
                        error!("Failed to create TUN fd: {e}");
                        let _ = result_tx.send(Err(NodeError::NodeCreate(e.to_string())));
                        return;
                    }
                };

                let config = Config {
                    node_key: crypto::SecretKey::from(priv_key),
                    peers: endpoints,
                    no_tun: false,
                    #[cfg(any(
                        target_os = "linux",
                        all(target_os = "macos", not(feature = "mactunfd")),
                        target_os = "windows"
                    ))]
                    tun_name: "tun0".to_string(),
                    #[cfg(any(
                        target_os = "android",
                        target_os = "ios",
                        all(target_os = "macos", feature = "mactunfd"),
                    ))]
                    tun_fd: Some(tun_fd),
                    tcp_listen_port: 9651,
                    quic_listen_port: Some(9651),
                    peer_discovery_port: 9650,
                    peer_discovery_mode: PeerDiscoveryMode::Disabled,
                    metrics: NoMetrics,
                    private_network_config: None,
                    firewall_mark: None,
                    update_workers: 1,
                    cdn_cache: None,
                    enable_dns,
                };

                match Node::new(config).await {
                    Err(e) => {
                        error!("Failed to create node: {e}");
                        let _ = result_tx.send(Err(NodeError::NodeCreate(e.to_string())));
                    }
                    Ok(node) => {
                        info!("mycelium node started");
                        let node = Arc::new(Mutex::new(node));
                        let _ = result_tx.send(Ok((Arc::clone(&node), handle)));
                        // Keep the runtime alive until stop() is called.
                        let _ = shutdown_rx.await;
                        info!("mycelium node stopped");
                    }
                }
            });
        });

        let (node, rt_handle) = result_rx.recv().map_err(|_| NodeError::ThreadPanic)??;

        Ok(NodeHandle {
            node,
            rt_handle,
            shutdown_tx: Some(shutdown_tx),
        })
    }

    /// Signal the node to shut down. Non-blocking; the background thread will
    /// exit asynchronously.
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }

    pub fn is_running(&self) -> bool {
        self.shutdown_tx.is_some()
    }

    pub fn node(&self) -> &Arc<Mutex<Node<NoMetrics>>> {
        &self.node
    }

    pub fn rt(&self) -> &tokio::runtime::Handle {
        &self.rt_handle
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert a [`PeerStats`] to the AIDL-generated `PeerInfo` parcelable.
// rsbinder-aidl generates a sub-module with the same name as the parcelable,
// so the type is PeerInfo::PeerInfo.
pub fn peer_stats_to_aidl(p: PeerStats) -> crate::tech::threefold::mycelium::PeerInfo::PeerInfo {
    crate::tech::threefold::mycelium::PeerInfo::PeerInfo {
        protocol: p.endpoint.proto().to_string(),
        address: p.endpoint.address().to_string(),
        peerType: p.pt.to_string(),
        connectionState: p.connection_state.to_string(),
        rxBytes: p.rx_bytes as i64,
        txBytes: p.tx_bytes as i64,
        discoveredSeconds: p.discovered as i64,
        lastConnectedSeconds: p.last_connected.map(|v| v as i64).unwrap_or(-1),
    }
}

/// Parse a proxy address string ("ip:port") into a [`SocketAddr`].
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
