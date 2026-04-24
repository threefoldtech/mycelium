//! Implementation of the `IMyceliumService` AIDL interface.

use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;

use binder::{ExceptionCode, Status};
use tracing::{error, info, warn};

use crate::metrics::NoMetrics;
use crate::node_handle::{parse_proxy_remote, NodeHandle, DEFAULT_SOCKS_PORT};
use crate::peer_manager::PeerDiscoveryMode;
use crate::{crypto, Config};

use mycelium_aidl_interface::aidl::tech::threefold::mycelium::{
    IMyceliumService::IMyceliumService, NodeInfo::NodeInfo, PacketStatEntry::PacketStatEntry,
    PacketStats::PacketStats, PeerInfo::PeerInfo, QueriedSubnet::QueriedSubnet, Route::Route,
    StartConfig::StartConfig,
};

/// Implements the `IMyceliumService` AIDL interface.
///
/// Binder thread pool threads call into this struct. Each method either
/// answers immediately (pure computation) or uses `rt_handle.block_on` to
/// drive an async operation against the running mycelium node.
pub struct MyceliumService {
    handle: Mutex<Option<NodeHandle>>,
}

impl Default for MyceliumService {
    fn default() -> Self {
        MyceliumService {
            handle: Mutex::new(None),
        }
    }
}

impl MyceliumService {
    pub fn new() -> Self {
        Self::default()
    }
}

impl binder::Interface for MyceliumService {}

fn invalid_op() -> Status {
    Status::new_exception(ExceptionCode::ILLEGAL_STATE, None)
}

fn bad_value() -> Status {
    Status::new_exception(ExceptionCode::ILLEGAL_ARGUMENT, None)
}

fn unknown(msg: &str) -> Status {
    Status::new_service_specific_error_str(-1, Some(msg))
}

fn parse_port(value: i32, field: &str) -> binder::Result<u16> {
    u16::try_from(value).map_err(|_| {
        error!("invalid {field}: {value} (expected 0..=65535)");
        bad_value()
    })
}

fn parse_discovery_mode(
    mode: &str,
    interfaces: &[String],
) -> binder::Result<PeerDiscoveryMode> {
    match mode {
        "" | "all" => Ok(PeerDiscoveryMode::All),
        "disabled" => Ok(PeerDiscoveryMode::Disabled),
        "filtered" => Ok(PeerDiscoveryMode::Filtered(interfaces.to_vec())),
        other => {
            error!("unknown peerDiscoveryMode: {other}");
            Err(bad_value())
        }
    }
}

#[allow(non_snake_case)]
impl IMyceliumService for MyceliumService {
    fn start(&self, cfg: &StartConfig) -> binder::Result<bool> {
        let key: [u8; 32] = cfg.privKey.as_slice().try_into().map_err(|_| bad_value())?;

        info!("starting mycelium node, {} peers", cfg.peers.len());

        let endpoints = cfg.peers.iter().filter_map(|p| p.parse().ok()).collect();
        let tcp_listen_port = parse_port(cfg.tcpListenPort, "tcpListenPort")?;
        let quic_listen_port = match cfg.quicListenPort {
            0 => None,
            v => Some(parse_port(v, "quicListenPort")?),
        };
        let peer_discovery_port = parse_port(cfg.peerDiscoveryPort, "peerDiscoveryPort")?;
        let peer_discovery_mode =
            parse_discovery_mode(&cfg.peerDiscoveryMode, &cfg.peerDiscoveryInterfaces)?;

        #[cfg(target_os = "android")]
        let tun_fd = crate::node_handle::create_tun_fd(&cfg.tunName).map_err(|e| {
            error!("Failed to create TUN fd: {e}");
            unknown("failed to create tun fd")
        })?;

        let config = Config {
            node_key: crypto::SecretKey::from(key),
            peers: endpoints,
            no_tun: false,
            #[cfg(any(
                target_os = "linux",
                all(target_os = "macos", not(feature = "mactunfd")),
                target_os = "windows"
            ))]
            tun_name: cfg.tunName.clone(),
            #[cfg(any(
                target_os = "android",
                target_os = "ios",
                all(target_os = "macos", feature = "mactunfd"),
            ))]
            tun_fd: Some(tun_fd),
            tcp_listen_port,
            quic_listen_port,
            peer_discovery_port,
            peer_discovery_mode,
            metrics: NoMetrics,
            private_network_config: None,
            firewall_mark: None,
            update_workers: 1,
            cdn_cache: None,
            enable_dns: cfg.enableDns,
            #[cfg(feature = "message")]
            topic_config: None,
        };

        match NodeHandle::start(config) {
            Ok(h) => {
                *self.handle.lock().unwrap() = Some(h);
                Ok(true)
            }
            Err(e) => {
                error!("failed to start node: {e}");
                Err(unknown("failed to start node"))
            }
        }
    }

    fn stop(&self) -> binder::Result<()> {
        let mut guard = self.handle.lock().unwrap();
        if let Some(h) = guard.as_mut() {
            info!("stopping mycelium node");
            h.stop();
        } else {
            warn!("stop() called but node is not running");
        }
        Ok(())
    }

    fn isRunning(&self) -> binder::Result<bool> {
        Ok(self
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.is_running())
            .unwrap_or(false))
    }

    fn getNodeInfo(&self) -> binder::Result<NodeInfo> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        h.rt().block_on(async {
            let info = h.node().lock().await.info();
            Ok(NodeInfo {
                subnet: info.node_subnet.to_string(),
                pubkey: info.node_pubkey.to_string(),
            })
        })
    }

    fn getPublicKeyFromIp(&self, ip: &str) -> binder::Result<String> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        let ip: IpAddr = ip.parse().map_err(|_| bad_value())?;
        h.rt().block_on(async {
            Ok(h.node()
                .lock()
                .await
                .get_pubkey_from_ip(ip)
                .map(|pk| pk.to_string())
                .unwrap_or_default())
        })
    }

    fn getPeers(&self) -> binder::Result<Vec<PeerInfo>> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        h.rt().block_on(async {
            Ok(h.node()
                .lock()
                .await
                .peer_info()
                .into_iter()
                .map(PeerInfo::from)
                .collect())
        })
    }

    fn addPeer(&self, endpoint: &str) -> binder::Result<bool> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        let endpoint = endpoint
            .parse::<crate::endpoint::Endpoint>()
            .map_err(|_| bad_value())?;
        h.rt()
            .block_on(async { Ok(h.node().lock().await.add_peer(endpoint).is_ok()) })
    }

    fn removePeer(&self, endpoint: &str) -> binder::Result<bool> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        let endpoint = endpoint
            .parse::<crate::endpoint::Endpoint>()
            .map_err(|_| bad_value())?;
        h.rt()
            .block_on(async { Ok(h.node().lock().await.remove_peer(endpoint).is_ok()) })
    }

    fn getSelectedRoutes(&self) -> binder::Result<Vec<Route>> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        h.rt().block_on(async {
            Ok(h.node()
                .lock()
                .await
                .selected_routes()
                .into_iter()
                .map(Route::from)
                .collect())
        })
    }

    fn getFallbackRoutes(&self) -> binder::Result<Vec<Route>> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        h.rt().block_on(async {
            Ok(h.node()
                .lock()
                .await
                .fallback_routes()
                .into_iter()
                .map(Route::from)
                .collect())
        })
    }

    fn getQueriedSubnets(&self) -> binder::Result<Vec<QueriedSubnet>> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        h.rt().block_on(async {
            let now = tokio::time::Instant::now();
            Ok(h.node()
                .lock()
                .await
                .queried_subnets()
                .into_iter()
                .map(|qs| QueriedSubnet {
                    subnet: qs.subnet().to_string(),
                    expirationSeconds: qs.query_expires().saturating_duration_since(now).as_secs()
                        as i64,
                })
                .collect())
        })
    }

    fn getPacketStats(&self) -> binder::Result<PacketStats> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        h.rt().block_on(async {
            let stats = h.node().lock().await.packet_statistics();
            Ok(PacketStats {
                bySource: stats.by_source.into_iter().map(PacketStatEntry::from).collect(),
                byDestination: stats
                    .by_destination
                    .into_iter()
                    .map(PacketStatEntry::from)
                    .collect(),
            })
        })
    }

    fn generateSecretKey(&self) -> binder::Result<Vec<u8>> {
        Ok(crypto::SecretKey::new().as_bytes().to_vec())
    }

    fn addressFromSecretKey(&self, key: &[u8]) -> binder::Result<String> {
        let arr: [u8; 32] = key.try_into().map_err(|_| bad_value())?;
        let secret = crypto::SecretKey::from(arr);
        Ok(crypto::PublicKey::from(&secret).address().to_string())
    }

    fn startProxyProbe(&self) -> binder::Result<bool> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        h.rt().block_on(async {
            h.node().lock().await.start_proxy_scan();
            Ok(true)
        })
    }

    fn stopProxyProbe(&self) -> binder::Result<bool> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        h.rt().block_on(async {
            h.node().lock().await.stop_proxy_scan();
            Ok(true)
        })
    }

    fn listProxies(&self) -> binder::Result<Vec<String>> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        h.rt().block_on(async {
            let proxies: Vec<String> = h
                .node()
                .lock()
                .await
                .known_proxies()
                .into_iter()
                .map(|ip| SocketAddr::new(IpAddr::from(ip), DEFAULT_SOCKS_PORT).to_string())
                .collect();
            Ok(proxies)
        })
    }

    fn proxyConnect(&self, remote: &str) -> binder::Result<String> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;

        let remote_addr = parse_proxy_remote(remote).map_err(|_| bad_value())?;

        h.rt().block_on(async {
            let future = h.node().lock().await.connect_proxy(remote_addr);
            match future.await {
                Ok(addr) => Ok(addr.to_string()),
                Err(e) => {
                    error!("proxy connect failed: {e}");
                    Err(unknown("proxy connect failed"))
                }
            }
        })
    }

    fn proxyDisconnect(&self) -> binder::Result<()> {
        let guard = self.handle.lock().unwrap();
        let h = guard.as_ref().ok_or_else(invalid_op)?;
        h.rt().block_on(async {
            h.node().lock().await.disconnect_proxy();
            Ok(())
        })
    }
}
