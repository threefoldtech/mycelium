use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;

use rsbinder::StatusCode;
use tracing::{error, info, warn};

use crate::node::{parse_proxy_remote, peer_stats_to_aidl, NodeHandle, DEFAULT_SOCKS_PORT};
use crate::tech::threefold::mycelium::IMyceliumService::IMyceliumService;
use crate::tech::threefold::mycelium::NodeInfo::NodeInfo;
use crate::tech::threefold::mycelium::PacketStatEntry::PacketStatEntry;
use crate::tech::threefold::mycelium::PacketStats::PacketStats;
use crate::tech::threefold::mycelium::PeerInfo::PeerInfo;
use crate::tech::threefold::mycelium::QueriedSubnet::QueriedSubnet;
use crate::tech::threefold::mycelium::Route::Route;

// ── Service struct ─────────────────────────────────────────────────────────────

/// Implements the `IMyceliumService` AIDL interface.
///
/// Binder thread pool threads call into this struct. Each method either
/// answers immediately (pure computation) or uses `rt_handle.block_on` to
/// drive an async operation against the running mycelium node.
pub struct MyceliumService {
    /// Running node handle, present once `start()` has been called.
    handle: Mutex<Option<NodeHandle>>,
}

impl MyceliumService {
    pub fn new() -> Self {
        MyceliumService {
            handle: Mutex::new(None),
        }
    }
}

impl rsbinder::Interface for MyceliumService {}

#[allow(non_snake_case)]
impl IMyceliumService for MyceliumService {
    fn start(
        &self,
        peers: &[String],
        priv_key: &[u8],
        enable_dns: bool,
    ) -> rsbinder::status::Result<bool> {
        let key: [u8; 32] = priv_key
            .try_into()
            .map_err(|_| rsbinder::Status::from(StatusCode::BadValue))?;

        info!("starting mycelium node, {} peers", peers.len());

        match NodeHandle::start(peers.to_vec(), key, enable_dns) {
            Ok(h) => {
                *self.handle.lock().unwrap() = Some(h);
                Ok(true)
            }
            Err(e) => {
                error!("failed to start node: {e}");
                Err(rsbinder::Status::from(StatusCode::Unknown))
            }
        }
    }

    fn stop(&self) -> rsbinder::status::Result<()> {
        let mut guard = self.handle.lock().unwrap();
        if let Some(h) = guard.as_mut() {
            info!("stopping mycelium node");
            h.stop();
        } else {
            warn!("stop() called but node is not running");
        }
        Ok(())
    }

    fn isRunning(&self) -> rsbinder::status::Result<bool> {
        Ok(self
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.is_running())
            .unwrap_or(false))
    }

    fn getNodeInfo(&self) -> rsbinder::status::Result<NodeInfo> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
        h.rt().block_on(async {
            let info = h.node().lock().await.info();
            Ok(NodeInfo {
                subnet: info.node_subnet.to_string(),
                pubkey: info.node_pubkey.to_string(),
            })
        })
    }

    fn getPublicKeyFromIp(&self, ip: &str) -> rsbinder::status::Result<String> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
        let ip: IpAddr = ip
            .parse()
            .map_err(|_| rsbinder::Status::from(StatusCode::BadValue))?;
        h.rt().block_on(async {
            Ok(h.node()
                .lock()
                .await
                .get_pubkey_from_ip(ip)
                .map(|pk| pk.to_string())
                .unwrap_or_default())
        })
    }

    fn getPeers(&self) -> rsbinder::status::Result<Vec<PeerInfo>> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
        h.rt().block_on(async {
            Ok(h.node()
                .lock()
                .await
                .peer_info()
                .into_iter()
                .map(peer_stats_to_aidl)
                .collect())
        })
    }

    fn addPeer(&self, endpoint: &str) -> rsbinder::status::Result<bool> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
        let endpoint = endpoint
            .parse::<mycelium::endpoint::Endpoint>()
            .map_err(|_| rsbinder::Status::from(StatusCode::BadValue))?;
        h.rt()
            .block_on(async { Ok(h.node().lock().await.add_peer(endpoint).is_ok()) })
    }

    fn removePeer(&self, endpoint: &str) -> rsbinder::status::Result<bool> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
        let endpoint = endpoint
            .parse::<mycelium::endpoint::Endpoint>()
            .map_err(|_| rsbinder::Status::from(StatusCode::BadValue))?;
        h.rt()
            .block_on(async { Ok(h.node().lock().await.remove_peer(endpoint).is_ok()) })
    }

    fn getSelectedRoutes(&self) -> rsbinder::status::Result<Vec<Route>> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
        h.rt().block_on(async {
            Ok(h.node()
                .lock()
                .await
                .selected_routes()
                .into_iter()
                .map(|r| Route {
                    subnet: r.source().subnet().to_string(),
                    nextHop: r.neighbour().connection_identifier().clone(),
                    metric: if r.metric().is_infinite() {
                        -1
                    } else {
                        u16::from(r.metric()) as i64
                    },
                    seqno: u16::from(r.seqno()) as i32,
                })
                .collect())
        })
    }

    fn getFallbackRoutes(&self) -> rsbinder::status::Result<Vec<Route>> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
        h.rt().block_on(async {
            Ok(h.node()
                .lock()
                .await
                .fallback_routes()
                .into_iter()
                .map(|r| Route {
                    subnet: r.source().subnet().to_string(),
                    nextHop: r.neighbour().connection_identifier().clone(),
                    metric: if r.metric().is_infinite() {
                        -1
                    } else {
                        u16::from(r.metric()) as i64
                    },
                    seqno: u16::from(r.seqno()) as i32,
                })
                .collect())
        })
    }

    fn getQueriedSubnets(&self) -> rsbinder::status::Result<Vec<QueriedSubnet>> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
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

    fn getPacketStats(&self) -> rsbinder::status::Result<PacketStats> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
        h.rt().block_on(async {
            let stats = h.node().lock().await.packet_statistics();
            Ok(PacketStats {
                bySource: stats
                    .by_source
                    .into_iter()
                    .map(|e| PacketStatEntry {
                        ip: e.ip.to_string(),
                        packetCount: e.packet_count as i64,
                        byteCount: e.byte_count as i64,
                    })
                    .collect(),
                byDestination: stats
                    .by_destination
                    .into_iter()
                    .map(|e| PacketStatEntry {
                        ip: e.ip.to_string(),
                        packetCount: e.packet_count as i64,
                        byteCount: e.byte_count as i64,
                    })
                    .collect(),
            })
        })
    }

    fn generateSecretKey(&self) -> rsbinder::status::Result<Vec<u8>> {
        Ok(mycelium::crypto::SecretKey::new().as_bytes().to_vec())
    }

    fn addressFromSecretKey(&self, key: &[u8]) -> rsbinder::status::Result<String> {
        let arr: [u8; 32] = key
            .try_into()
            .map_err(|_| rsbinder::Status::from(StatusCode::BadValue))?;
        let secret = mycelium::crypto::SecretKey::from(arr);
        Ok(mycelium::crypto::PublicKey::from(&secret)
            .address()
            .to_string())
    }

    fn startProxyProbe(&self) -> rsbinder::status::Result<bool> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
        h.rt().block_on(async {
            h.node().lock().await.start_proxy_scan();
            Ok(true)
        })
    }

    fn stopProxyProbe(&self) -> rsbinder::status::Result<bool> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
        h.rt().block_on(async {
            h.node().lock().await.stop_proxy_scan();
            Ok(true)
        })
    }

    fn listProxies(&self) -> rsbinder::status::Result<Vec<String>> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
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

    fn proxyConnect(&self, remote: &str) -> rsbinder::status::Result<String> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;

        let remote_addr =
            parse_proxy_remote(remote).map_err(|_| rsbinder::Status::from(StatusCode::BadValue))?;

        h.rt().block_on(async {
            let future = h.node().lock().await.connect_proxy(remote_addr);
            // MutexGuard released before await
            match future.await {
                Ok(addr) => Ok(addr.to_string()),
                Err(e) => {
                    error!("proxy connect failed: {e}");
                    Err(rsbinder::Status::from(StatusCode::Unknown))
                }
            }
        })
    }

    fn proxyDisconnect(&self) -> rsbinder::status::Result<()> {
        let guard = self.handle.lock().unwrap();
        let h = guard
            .as_ref()
            .ok_or_else(|| rsbinder::Status::from(StatusCode::InvalidOperation))?;
        h.rt().block_on(async {
            h.node().lock().await.disconnect_proxy();
            Ok(())
        })
    }
}
