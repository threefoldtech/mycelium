#[cfg(feature = "private-network")]
use crate::connection::tls::TlsStream;
use crate::connection::{Quic, TcpStream};
use crate::endpoint::{Endpoint, Protocol};
use crate::metrics::Metrics;
use crate::peer::{Peer, PeerRef};
use crate::router::Router;
use crate::router_id::RouterId;
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
#[cfg(feature = "private-network")]
use openssl::ssl::{Ssl, SslAcceptor, SslConnector, SslMethod};
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{congestion, MtuDiscoveryConfig, ServerConfig, TransportConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr, SocketAddrV6};
#[cfg(target_os = "linux")]
use std::os::fd::AsFd;
#[cfg(feature = "private-network")]
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{collections::hash_map::Entry, future::IntoFuture};
use tokio::net::{TcpListener, UdpSocket};
use tokio::task::AbortHandle;
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{debug, error, info, instrument, trace, warn};

/// Magic bytes to identify a multicast UDP packet used in link local peer discovery.
const MYCELIUM_MULTICAST_DISCOVERY_MAGIC: &[u8; 8] = b"mycelium";
/// Size of a peer discovery beacon.
const PEER_DISCOVERY_BEACON_SIZE: usize = 8 + 2 + 40;
/// Link local peer discovery group joined by the UDP listener.
const LL_PEER_DISCOVERY_GROUP: &str = "ff02::cafe";
/// The time between sending consecutive link local discovery beacons.
const LL_PEER_DISCOVERY_BEACON_INTERVAL: Duration = Duration::from_secs(60);
/// The time between checking known peer liveness and trying to reconnect.
const PEER_CONNECT_INTERVAL: Duration = Duration::from_secs(5);
/// The maximum amount of successive failures allowed when connecting to a local discovered peer,
/// before it is forgotten.
const MAX_FAILED_LOCAL_PEER_CONNECTION_ATTEMPTS: usize = 3;
/// The amount of time allowed for a peer to finish the quic handshake when it connects to us. This
/// prevents a (mallicious) peer from hogging server resources. 10 seconds should be a reasonable
/// default for this, though it can certainly be made more strict if required.
const INBOUND_QUIC_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// The maximum amount of concurrent quic handshakes to process from peers connecting to us. We
/// want to find a middle ground where we don't stall valid peers from connecting beceause a single
/// misbehaving peer stalls the connection task, but we also don't want to accept thousands of
/// these in parralel. For now, 10 in parallel should be sufficient, though this can be
/// increased/decreased based on observations.
const MAX_INBOUND_CONCURRENT_QUICK_HANDSHAKES: usize = 10;

/// The PeerManager creates new peers by connecting to configured addresses, and setting up the
/// connection. Once a connection is established, the created [`Peer`] is handed over to the
/// [`Router`].
pub struct PeerManager<M> {
    inner: Arc<Inner<M>>,
    /// Handles to background tasks so we can abort them when the PeerManager is dropped.
    abort_handles: Vec<AbortHandle>,
}

/// Details how the PeerManager learned about a remote.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PeerType {
    /// Statically configured peer.
    Static,
    /// Peer found through link local discovery.
    LinkLocalDiscovery,
    /// A remote which initiated a connection to us.
    Inbound,
}

/// Local info about a peer.
struct PeerInfo {
    /// Details how we found out about this peer.
    pt: PeerType,
    /// Are we currently connecting to this peer?
    connecting: bool,
    /// The [`PeerRef`] used to check liveliness.
    pr: PeerRef,
    /// Amount of failed times we tried to connect to this peer. This is reset after a successful
    /// connection.
    connection_attempts: usize,
    /// Keep track of the amount of bytes we've sent to and received from this peer.
    con_traffic: ConnectionTraffic,
    /// The moment in time we learned about this peer.
    discovered: Instant,
    /// The moment we last connected to this peer.
    connected: Option<Instant>,
}

/// Counters for the amount of traffic written to and received from a [`Peer`].
#[derive(Debug, Clone)]
struct ConnectionTraffic {
    /// Amount of bytes transmitted to this peer.
    tx_bytes: Arc<AtomicU64>,
    /// Amount of bytes received from this peer.
    rx_bytes: Arc<AtomicU64>,
}

/// General state about a connection to a [`Peer`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionState {
    /// There is a working connection to the [`Peer`].
    Alive,
    /// The system is currently in the process of establishing a new connection to the [`Peer`].
    Connecting,
    /// There is no connection, or the existing connection is no longer functional.
    Dead,
}

/// Identification and information/statistics for a specific [`Peer`]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerStats {
    /// The endpoint of the [`Peer`].
    pub endpoint: Endpoint,
    /// The [`Type`](PeerType) of the [`Peer`].
    #[serde(rename = "type")]
    pub pt: PeerType,
    /// State of the connection to this [`Peer`]
    pub connection_state: ConnectionState,
    /// Amount of bytes transmitted to this [`Peer`].
    pub tx_bytes: u64,
    /// Amount of bytes received from this [`Peer`].
    pub rx_bytes: u64,
    /// Amount of time which passed since the system learned about this [`Peer`], in seconds.
    pub discovered: u64,
    /// Amount of seconds since the last succesfull connection to this [`Peer`].
    pub last_connected: Option<u64>,
}

impl PeerInfo {
    /// Return the amount of bytes read from this peer.
    #[inline]
    fn read(&self) -> u64 {
        self.con_traffic.rx_bytes.load(Ordering::Relaxed)
    }

    /// Return the amount of bytes written to this peer.
    #[inline]
    fn written(&self) -> u64 {
        self.con_traffic.tx_bytes.load(Ordering::Relaxed)
    }
}

/// Marker error to indicate a [`peer`](Endpoint) is already known.
#[derive(Debug)]
pub struct PeerExists;

/// Marker error to indicate a [`peer`](Endpoint) is not known.
#[derive(Debug)]
pub struct PeerNotFound;

/// PSK used to set up a shared network. Currently 32 bytes though this might change in the future.
pub type PrivateNetworkKey = [u8; 32];

struct Inner<M> {
    /// Router is unfortunately wrapped in a Mutex, because router is not Sync.
    router: Mutex<Router<M>>,
    peers: Mutex<HashMap<Endpoint, PeerInfo>>,
    /// Listen port for new peer connections
    tcp_listen_port: u16,
    quic_socket: Option<quinn::Endpoint>,
    /// Identity and name of a private network, if one exists
    private_network_config: Option<(String, [u8; 32])>,
    metrics: M,
    firewall_mark: Option<u32>,
}

impl<M> PeerManager<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        router: Router<M>,
        static_peers_sockets: Vec<Endpoint>,
        tcp_listen_port: u16,
        quic_listen_port: Option<u16>,
        peer_discovery_port: u16,
        disable_peer_discovery: bool,
        private_network_config: Option<(String, PrivateNetworkKey)>,
        metrics: M,
        firewall_mark: Option<u32>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let is_private_net = private_network_config.is_some();

        // Currently we don't support Quic when a private network is used.
        let quic_socket = if !is_private_net {
            if let Some(quic_listen_port) = quic_listen_port {
                Some(make_quic_endpoint(
                    router.router_id(),
                    quic_listen_port,
                    firewall_mark,
                )?)
            } else {
                None
            }
        } else {
            None
        };

        // Set the initially configured peer count in metrics.
        metrics.peer_manager_known_peers(static_peers_sockets.len());

        let mut peer_manager = PeerManager {
            inner: Arc::new(Inner {
                router: Mutex::new(router),
                peers: Mutex::new(
                    static_peers_sockets
                        .into_iter()
                        // These peers are not alive, but we say they are because the reconnect
                        // loop will perform the actual check and figure out they are dead, then
                        // (re)connect.
                        .map(|s| {
                            let now = tokio::time::Instant::now();
                            (
                                s,
                                PeerInfo {
                                    pt: PeerType::Static,
                                    connecting: false,
                                    pr: PeerRef::new(),
                                    connection_attempts: 0,
                                    con_traffic: ConnectionTraffic {
                                        tx_bytes: Arc::new(AtomicU64::new(0)),
                                        rx_bytes: Arc::new(AtomicU64::new(0)),
                                    },
                                    discovered: now,
                                    connected: None,
                                },
                            )
                        })
                        .collect(),
                ),
                tcp_listen_port,
                quic_socket,
                private_network_config,
                metrics,
                firewall_mark,
            }),
            abort_handles: vec![],
        };

        // Start listeners for inbound connections.
        // Start the tcp listener, in case we are running a private network the tcp listener will
        // actually be a tls listener.
        let handle = tokio::spawn(peer_manager.inner.clone().tcp_listener());
        peer_manager.abort_handles.push(handle.abort_handle());
        if is_private_net {
            info!("Enabled private network mode");
        } else if peer_manager.inner.quic_socket.is_some() {
            // Currently quic is not supported in private network mode.
            let handle = tokio::spawn(peer_manager.inner.clone().quic_listener());
            peer_manager.abort_handles.push(handle.abort_handle());
        };

        // Start (re)connecting to outbound/local peers
        let handle = tokio::spawn(peer_manager.inner.clone().connect_to_peers());
        peer_manager.abort_handles.push(handle.abort_handle());

        // Discover local peers, this does not actually connect to them. That is handle by the
        // connect_to_peers task.
        if !disable_peer_discovery {
            let handle = tokio::spawn(
                peer_manager
                    .inner
                    .clone()
                    .local_discovery(peer_discovery_port),
            );
            peer_manager.abort_handles.push(handle.abort_handle());
        }

        Ok(peer_manager)
    }

    /// Add a new peer to the system.
    ///
    /// The peer starts of as a dead peer, and connecting is handled in the reconnect loop.
    ///
    /// # Errors
    ///
    /// This function returns an error if the [`Endpoint`] is already known.
    pub fn add_peer(&self, peer: Endpoint) -> Result<(), PeerExists> {
        let mut peer_map = self.inner.peers.lock().unwrap();
        if peer_map.contains_key(&peer) {
            return Err(PeerExists);
        }
        let now = tokio::time::Instant::now();
        peer_map.insert(
            peer,
            PeerInfo {
                pt: PeerType::Static,
                connecting: false,
                pr: PeerRef::new(),
                connection_attempts: 0,
                con_traffic: ConnectionTraffic {
                    tx_bytes: Arc::new(AtomicU64::new(0)),
                    rx_bytes: Arc::new(AtomicU64::new(0)),
                },
                discovered: now,
                connected: None,
            },
        );

        Ok(())
    }

    /// Delete a peer from the system.
    ///
    /// The peer will be disconnected if it is currently connected.
    ///
    /// # Errors
    ///
    /// Returns an error if there is no peer identified by the given [`Endpoint`].
    pub fn delete_peer(&self, endpoint: &Endpoint) -> Result<(), PeerNotFound> {
        let mut peer_map = self.inner.peers.lock().unwrap();
        peer_map.remove(endpoint).ok_or(PeerNotFound).map(|pi| {
            // Make sure we kill the peer connection if one exists
            if let Some(peer) = pi.pr.upgrade() {
                peer.died();
            }
        })
    }

    /// Get a view of all known peers and their stats.
    pub fn peers(&self) -> Vec<PeerStats> {
        let peer_map = self.inner.peers.lock().unwrap();
        let mut pi = Vec::with_capacity(peer_map.len());
        for (endpoint, peer_info) in peer_map.iter() {
            let connection_state = if peer_info.connecting {
                ConnectionState::Connecting
            } else if peer_info.pr.alive() {
                ConnectionState::Alive
            } else {
                ConnectionState::Dead
            };
            pi.push(PeerStats {
                endpoint: *endpoint,
                pt: peer_info.pt.clone(),
                connection_state,
                tx_bytes: peer_info.written(),
                rx_bytes: peer_info.read(),
                discovered: peer_info.discovered.elapsed().as_secs(),
                last_connected: peer_info.connected.map(|i| i.elapsed().as_secs()),
            });
        }
        pi
    }
}

impl<M> Drop for PeerManager<M> {
    fn drop(&mut self) {
        // Cancel all background tasks
        for ah in &self.abort_handles {
            ah.abort();
        }
    }
}

impl<M> Inner<M>
where
    M: Metrics + Clone + Send + 'static,
{
    /// Connect and if needed reconnect to known peers.
    async fn connect_to_peers(self: Arc<Self>) {
        let mut peer_check_interval = tokio::time::interval(PEER_CONNECT_INTERVAL);
        // Avoid trying to spam connections. Since we track if we are connecting to a peer this
        // won't be that bad, but this avoid unnecessary lock contention.
        peer_check_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        // A list of pending connection futures. Like this, we don't have to spawn a new future for
        // every connection task.
        let mut connection_futures = FuturesUnordered::new();

        loop {
            tokio::select! {
                // We don't care about the none case, that can happen when we aren't connecting to
                // any peers.
                Some((endpoint, maybe_new_peer)) = connection_futures.next() => {
                    // Only insert the possible new peer if we actually still care about it
                    let mut peers = self.peers.lock().unwrap();
                    if let Some(pi) = peers.get_mut(&endpoint) {
                        // Regardless of what happened, we are no longer connecting.
                        self.metrics.peer_manager_connection_finished();
                        pi.connecting = false;
                        if let Some(peer) = maybe_new_peer {
                            // We did find a new Peer, insert into router and keep track of it
                            // Use fully qualified call to aid compiler in type inference.
                            pi.pr = Peer::refer(&peer);
                            self.router.lock().unwrap().add_peer_interface(peer);

                            // We successfully connected, reset the connection_attempts counter to 0
                            pi.connection_attempts = 0;
                            pi.connected = Some(tokio::time::Instant::now());
                        } else {
                            // Only log with error level on the first connection failure, to avoid spamming the logs
                            if pi.connection_attempts == 0 {
                                error!(endpoint.address=%endpoint.address(), endpoint.proto=%endpoint.proto(), "Couldn't connect to endpoint, turn on debug logging for more details");
                            } else {
                                debug!(endpoint.address=%endpoint.address(), endpoint.proto=%endpoint.proto(), attempt=%pi.connection_attempts+1, "Couldn't connect to endpoint")
                            }

                            // Connection failed, add a failed attempt and forget about the peer if
                            // needed.
                            pi.connection_attempts += 1;
                            if pi.pt == PeerType::LinkLocalDiscovery
                                && pi.connection_attempts >= MAX_FAILED_LOCAL_PEER_CONNECTION_ATTEMPTS {
                                info!(endpoint.address=%endpoint.address(), endpoint.proto=%endpoint.proto(), "Forgetting about locally discovered peer after failing to connect to it");
                                peers.remove(&endpoint);
                            }
                        }
                    }
                }
                _ = peer_check_interval.tick() => {
                    // Remove dead inbound peers
                    self.peers.lock().unwrap().retain(|_, v| v.pt != PeerType::Inbound || v.pr.alive());
                    debug!("Looking for dead peers");
                    // check if there is an entry for the peer in the router's peer list
                    for (endpoint, pi) in self.peers.lock().unwrap().iter_mut() {
                        if !pi.connecting && !pi.pr.alive() {
                            debug!(endpoint.address=%endpoint.address(), endpoint.proto=%endpoint.proto(), "Found dead peer");
                            if pi.pt == PeerType::Inbound {
                                debug!(endpoint.address=%endpoint.address(), endpoint.proto=%endpoint.proto(), "Refusing to reconnect to inbound peer");
                                continue
                            }
                            // Mark that we are connecting to the peer.
                            pi.connecting = true;
                            connection_futures.push(self.clone().connect_peer(*endpoint, pi.con_traffic.clone()));
                            self.metrics.peer_manager_connection_attempted();
                        }
                    }
                }
            }
        }
    }

    /// Create a new connection to a remote peer
    #[instrument(skip_all, fields(endpoint.proto=%endpoint.proto(), endpoint.address=%endpoint.address()))]
    async fn connect_peer(
        self: Arc<Self>,
        endpoint: Endpoint,
        ct: ConnectionTraffic,
    ) -> (Endpoint, Option<Peer>) {
        debug!("Connecting");
        match endpoint.proto() {
            Protocol::Tcp | Protocol::Tls => self.connect_tcp_peer(endpoint, ct).await,
            Protocol::Quic => self.connect_quic_peer(endpoint, ct).await,
        }
    }

    async fn connect_tcp_peer(
        self: Arc<Self>,
        endpoint: Endpoint,
        ct: ConnectionTraffic,
    ) -> (Endpoint, Option<Peer>) {
        match (endpoint.proto(), &self.private_network_config) {
            (Protocol::Tcp, Some(_)) => {
                warn!("Attempting to connect over Tcp while a private network is configured, connection will be upgraded to Tls")
            }
            (Protocol::Tls, None) => {
                warn!("Attempting to connect over Tls while a private network is not enabled, refusing to connect. Use \"Tcp\" instead");
                return (endpoint, None);
            }
            _ => {}
        }

        #[cfg(feature = "private-network")]
        let connector = if let Some((net_name, net_key)) = self.private_network_config.clone() {
            let mut connector = SslConnector::builder(SslMethod::tls_client()).unwrap();
            connector.set_psk_client_callback(move |_, _, id, key| {
                // Note: identity must be passed in as a 0-terminated C string.
                if id.len() < net_name.len() + 1 {
                    error!("Can't pass in identity to SSL connector");
                    return Ok(0);
                }
                id[..net_name.len()].copy_from_slice(net_name.as_bytes());
                id[net_name.len()] = 0;
                if key.len() < 32 {
                    error!("Can't pass in key to SSL acceptor");
                    return Ok(0);
                }
                // Copy key
                key[..32].copy_from_slice(&net_key[..]);

                Ok(32)
            });
            Some(connector.build())
        } else {
            None
        };

        match tokio::net::TcpStream::connect(endpoint.address())
            .map(|result| result.and_then(|socket| set_fw_mark(socket, self.firewall_mark)))
            .await
        {
            Ok(peer_stream) => {
                debug!("Opened connection");
                // Make sure Nagle's algorithm is disabled as it can cause latency spikes.
                if let Err(e) = peer_stream.set_nodelay(true) {
                    debug!(err=%e, "Couldn't disable Nagle's algorithm on stream");
                    return (endpoint, None);
                }

                // Scope the MutexGuard, if we don't do this the future won't be Send
                let (router_data_tx, router_control_tx, dead_peer_sink) = {
                    let router = self.router.lock().unwrap();
                    (
                        router.router_data_tx(),
                        router.router_control_tx(),
                        router.dead_peer_sink().clone(),
                    )
                };

                #[cfg(feature = "private-network")]
                let res = {
                    if let Some(connector) = connector {
                        let ssl = match Ssl::new(connector.context()) {
                            Ok(ssl) => ssl,
                            Err(e) => {
                                debug!(err=%e, "Failed to create SSL object from acceptor after connecting to remote");
                                return (endpoint, None);
                            }
                        };
                        let mut ssl_stream = match tokio_openssl::SslStream::new(ssl, peer_stream) {
                            Ok(ssl_stream) => ssl_stream,
                            Err(e) => {
                                debug!(err=%e, "Failed to create TLS stream from tcp connection to endpoint");
                                return (endpoint, None);
                            }
                        };

                        // Pin here is needed to call `connect`.
                        let pinned_stream = Pin::new(&mut ssl_stream);
                        if let Err(e) = pinned_stream.connect().await {
                            // Error here is likely a misconfigured server.
                            debug!(err=%e, "Could not initiate TLS stream");
                            return (endpoint, None);
                        }
                        debug!("Completed TLS handshake");

                        let tls_stream = match TlsStream::new(ssl_stream, ct.rx_bytes, ct.tx_bytes)
                        {
                            Ok(tls_stream) => tls_stream,
                            Err(err) => {
                                error!(%err, "Failed to create wrapped Tls stream");
                                return (endpoint, None);
                            }
                        };

                        Peer::new(
                            router_data_tx,
                            router_control_tx,
                            tls_stream,
                            dead_peer_sink,
                        )
                    } else {
                        let peer_stream =
                            match TcpStream::new(peer_stream, ct.rx_bytes, ct.tx_bytes) {
                                Ok(ps) => ps,
                                Err(err) => {
                                    error!(%err, "Failed to create wrapped tcp stream");
                                    return (endpoint, None);
                                }
                            };

                        Peer::new(
                            router_data_tx,
                            router_control_tx,
                            peer_stream,
                            dead_peer_sink,
                        )
                    }
                };

                #[cfg(not(feature = "private-network"))]
                let res = {
                    let peer_stream = match TcpStream::new(peer_stream, ct.rx_bytes, ct.tx_bytes) {
                        Ok(ps) => ps,
                        Err(err) => {
                            error!(%err, "Failed to create wrapped tcp stream");
                            return (endpoint, None);
                        }
                    };

                    Peer::new(
                        router_data_tx,
                        router_control_tx,
                        peer_stream,
                        dead_peer_sink,
                    )
                };

                match res {
                    Ok(new_peer) => {
                        info!("Connected to new peer");
                        (endpoint, Some(new_peer))
                    }
                    Err(e) => {
                        debug!(err=%e, "Failed to spawn peer");
                        (endpoint, None)
                    }
                }
            }
            Err(e) => {
                debug!(err=%e, "Couldn't connect");
                (endpoint, None)
            }
        }
    }

    async fn connect_quic_peer(
        self: Arc<Self>,
        endpoint: Endpoint,
        ct: ConnectionTraffic,
    ) -> (Endpoint, Option<Peer>) {
        let quic_socket = if let Some(quic_socket) = &self.quic_socket {
            quic_socket
        } else {
            debug!("Attempting to connect to quic peer while quic is disabled");
            return (endpoint, None);
        };
        let provider = rustls::crypto::CryptoProvider::get_default()
            .expect("We have a quic socket so there is a crypto provider installed");
        let qcc = match QuicClientConfig::try_from(
            rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(SkipServerVerification::new(provider.clone()))
                .with_no_client_auth(),
        ) {
            Ok(qcc) => qcc,
            Err(err) => {
                debug!(%err, "Failed to build quic client config");
                return (endpoint, None);
            }
        };
        let mut config = quinn::ClientConfig::new(Arc::new(qcc));
        // Todo: tweak transport config
        let mut transport_config = TransportConfig::default();
        transport_config.max_concurrent_uni_streams(0_u8.into());
        // Larger than needed for now, just in case
        transport_config.max_concurrent_bidi_streams(5_u8.into());
        // Connection timeout, set to higher than Hello interval to ensure connection does not randomly
        // time out.
        transport_config.max_idle_timeout(Some(Duration::from_secs(60).try_into().unwrap()));
        transport_config.mtu_discovery_config(Some(MtuDiscoveryConfig::default()));
        transport_config.keep_alive_interval(Some(Duration::from_secs(20)));
        // we don't use datagrams.
        transport_config.datagram_receive_buffer_size(Some(16 << 20));
        transport_config.datagram_send_buffer_size(16 << 20);
        transport_config.initial_mtu(1500);
        config.transport_config(Arc::new(transport_config));

        match quic_socket.connect_with(config, endpoint.address(), "dummy.mycelium") {
            Ok(connecting) => match connecting.await {
                Ok(con) => match con.open_bi().await {
                    Ok((tx, rx)) => {
                        let q_con = Quic::new(tx, rx, con, ct.tx_bytes, ct.rx_bytes);
                        let res = {
                            let router = self.router.lock().unwrap();
                            let router_data_tx = router.router_data_tx();
                            let router_control_tx = router.router_control_tx();
                            let dead_peer_sink = router.dead_peer_sink().clone();

                            Peer::new(router_data_tx, router_control_tx, q_con, dead_peer_sink)
                        };
                        match res {
                            Ok(new_peer) => {
                                info!("Connected to new peer");
                                (endpoint, Some(new_peer))
                            }
                            Err(e) => {
                                debug!(err=%e, "Failed to spawn peer");
                                (endpoint, None)
                            }
                        }
                    }
                    Err(e) => {
                        debug!(err=%e, "Couldn't open bidirectional quic stream");
                        (endpoint, None)
                    }
                },
                Err(e) => {
                    debug!(err=%e, "Couldn't complete quic connection");
                    (endpoint, None)
                }
            },
            Err(e) => {
                debug!(err=%e, "Couldn't initiate connection");
                (endpoint, None)
            }
        }
    }

    /// Start listening for new peers on a tcp socket. If a private network is configured, this
    /// will instead listen for incoming tls connections.
    async fn tcp_listener(self: Arc<Self>) {
        // Setup TLS acceptor for private network, if required.
        #[cfg(feature = "private-network")]
        let acceptor = if let Some((net_name, net_key)) = self.private_network_config.clone() {
            let mut acceptor = SslAcceptor::mozilla_modern_v5(SslMethod::tls_server()).unwrap();
            acceptor.set_psk_server_callback(move |_ssl_ref, id, key| {
                if let Some(id) = id {
                    if id != net_name.as_bytes() {
                        debug!("id given by client does not match configured private network name");
                        return Ok(0);
                    }
                } else {
                    debug!("No name indicated by client");
                    return Ok(0);
                }
                if key.len() < 32 {
                    warn!("Can't pass in key to SSL acceptor");
                    return Ok(0);
                }
                // Copy key
                key[..32].copy_from_slice(&net_key[..]);

                Ok(32)
            });
            Some(acceptor.build())
        } else {
            None
        };

        // Take a copy of every channel here first so we avoid lock contention in the loop later.
        let router_data_tx = self.router.lock().unwrap().router_data_tx();
        let router_control_tx = self.router.lock().unwrap().router_control_tx();
        let dead_peer_sink = self.router.lock().unwrap().dead_peer_sink().clone();

        let listener = TcpListener::bind(("::", self.tcp_listen_port))
            .map(|result| result.and_then(|listener| set_fw_mark(listener, self.firewall_mark)));

        match listener.await {
            Ok(listener) => loop {
                match listener.accept().await {
                    Ok((stream, remote)) => {
                        let tx_bytes = Arc::new(AtomicU64::new(0));
                        let rx_bytes = Arc::new(AtomicU64::new(0));

                        if let Err(e) = stream.set_nodelay(true) {
                            error!(err=%e, "Couldn't disable Nagle's algorithm on stream");
                            return;
                        }

                        #[cfg(feature = "private-network")]
                        let new_peer = if let Some(acceptor) = &acceptor {
                            let ssl = match Ssl::new(acceptor.context()) {
                                Ok(ssl) => ssl,
                                Err(e) => {
                                    error!(%remote, err=%e, "Failed to create SSL object from acceptor after remote connected");
                                    continue;
                                }
                            };
                            let mut ssl_stream = match tokio_openssl::SslStream::new(ssl, stream) {
                                Ok(ssl_stream) => ssl_stream,
                                Err(e) => {
                                    error!(%remote, err=%e, "Failed to create TLS stream from tcp connection");
                                    continue;
                                }
                            };

                            // Pin here is needed to call `accept`.
                            let pinned_stream = Pin::new(&mut ssl_stream);
                            if let Err(e) = pinned_stream.accept().await {
                                // An error at this point generally means the handshake failed,
                                // client error.
                                debug!(%remote, err=%e, "Could not accept TLS stream");
                                continue;
                            }
                            debug!(%remote, "Accepted TLS handshake");

                            let tls_stream = match TlsStream::new(
                                ssl_stream,
                                rx_bytes.clone(),
                                tx_bytes.clone(),
                            ) {
                                Ok(tls_stream) => tls_stream,
                                Err(err) => {
                                    error!(%err, "Failed to create wrapped Tls stream");
                                    continue;
                                }
                            };

                            Peer::new(
                                router_data_tx.clone(),
                                router_control_tx.clone(),
                                tls_stream,
                                dead_peer_sink.clone(),
                            )
                        } else {
                            let new_stream =
                                match TcpStream::new(stream, rx_bytes.clone(), tx_bytes.clone()) {
                                    Ok(ns) => ns,
                                    Err(err) => {
                                        error!(%err, "Failed to create wrapped tcp stream");
                                        continue;
                                    }
                                };

                            Peer::new(
                                router_data_tx.clone(),
                                router_control_tx.clone(),
                                new_stream,
                                dead_peer_sink.clone(),
                            )
                        };

                        #[cfg(not(feature = "private-network"))]
                        let new_peer = {
                            let new_stream =
                                match TcpStream::new(stream, rx_bytes.clone(), tx_bytes.clone()) {
                                    Ok(ns) => ns,
                                    Err(err) => {
                                        error!(%err, "Failed to create wrapped tcp stream");
                                        continue;
                                    }
                                };

                            Peer::new(
                                router_data_tx.clone(),
                                router_control_tx.clone(),
                                new_stream,
                                dead_peer_sink.clone(),
                            )
                        };

                        let new_peer = {
                            match new_peer {
                                Ok(peer) => peer,
                                Err(e) => {
                                    error!(err=%e, "Failed to spawn peer");
                                    continue;
                                }
                            }
                        };

                        info!("Accepted new inbound peer");
                        self.add_peer(
                            Endpoint::new(
                                if self.private_network_config.is_some() {
                                    Protocol::Tls
                                } else {
                                    Protocol::Tcp
                                },
                                remote,
                            ),
                            PeerType::Inbound,
                            ConnectionTraffic { tx_bytes, rx_bytes },
                            Some(new_peer),
                        );
                    }
                    Err(e) => {
                        error!(err=%e, "Error accepting connection");
                    }
                }
            },
            Err(e) => {
                error!(err=%e, "Error starting listener");
            }
        }
    }

    /// Start listening on the configured quic socket for new inbound peers.
    ///
    /// # Panics
    ///
    /// This method panics if `self.quic_socket` is [`None`].
    async fn quic_listener(self: Arc<Self>) {
        // SAFETY: This is safe because this method only get's called if we have a quic socket.
        let quic_socket = self.quic_socket.as_ref().unwrap();
        // Take a copy of every channel here first so we avoid lock contention in the loop later.
        let router_data_tx = self.router.lock().unwrap().router_data_tx();
        let router_control_tx = self.router.lock().unwrap().router_control_tx();
        let dead_peer_sink = self.router.lock().unwrap().dead_peer_sink().clone();

        let mut quic_con_futures = FuturesUnordered::new();

        let mut pending_quic_handshakes = 0;

        loop {
            tokio::select! {
                // FuturesUnordered always returns Some(...).
                Some(handshake_result) = quic_con_futures.next() => {
                    pending_quic_handshakes -=1;
                    // Since tyhpe inference failed here, use a fully quallified function call
                    if Result::<(),tokio::time::error::Elapsed>::is_err(&handshake_result) {
                        debug!("Dropping connection to peer who's handshake timed out");
                    }
                }
                maybe_con = quic_socket.accept(), if pending_quic_handshakes < MAX_INBOUND_CONCURRENT_QUICK_HANDSHAKES => {
                    let Some(con) = maybe_con else {
                        break
                    };


                    let con_future = async  {
                        let con = match con.into_future().await {
                            Ok(con) => con,
                            Err(e) => {
                                debug!(err=%e, "Failed to accept quic connection");
                                return;
                            }
                        };
                        let remote_address = con.remote_address();


                        let tx_bytes = Arc::new(AtomicU64::new(0));
                        let rx_bytes = Arc::new(AtomicU64::new(0));

                        let quic_peer = match con.accept_bi().await {
                            Ok((tx, rx)) => Quic::new(tx, rx, con, rx_bytes.clone(), tx_bytes.clone()),
                            Err(e) => {
                                debug!(err=%e, "Failed to accept bidirectional quic stream");
                                return;
                            }
                        };

                        let new_peer = match Peer::new(
                            router_data_tx.clone(),
                            router_control_tx.clone(),
                            quic_peer,
                            dead_peer_sink.clone(),
                        ) {
                            Ok(peer) => peer,
                            Err(e) => {
                                error!(err=%e, "Failed to spawn peer");
                                return;
                            }
                        };
                        info!(remote=%remote_address, "Accepted new inbound quic peer");
                        self.add_peer(
                            Endpoint::new(Protocol::Quic, remote_address),
                            PeerType::Inbound,
                            ConnectionTraffic { tx_bytes, rx_bytes },
                            Some(new_peer),
                        );
                    };

                    pending_quic_handshakes += 1;
                    quic_con_futures.push(tokio::time::timeout(INBOUND_QUIC_HANDSHAKE_TIMEOUT,con_future));
                }
            }
        }

        info!("Shutting down closed quic listener");
    }

    /// Add a new peer identifier we discovered.
    #[instrument(skip_all,fields(peer.endpoint=%endpoint))]
    fn add_peer(
        &self,
        endpoint: Endpoint,
        discovery_type: PeerType,
        con_traffic: ConnectionTraffic,
        peer: Option<Peer>,
    ) {
        self.metrics.peer_manager_peer_added(discovery_type.clone());
        let mut peers = self.peers.lock().unwrap();
        // Filter out link local IP's we already know (because of reverse detection)
        if discovery_type == PeerType::LinkLocalDiscovery {
            if let IpAddr::V6(ip) = endpoint.address().ip() {
                if ip.octets()[..8] == [0xfe, 0x80, 0, 0, 0, 0, 0, 0] {
                    for known_endpoint in peers.keys() {
                        if known_endpoint.address().ip() == endpoint.address().ip()
                            && known_endpoint.proto() == endpoint.proto()
                        {
                            trace!(peer.known_endpoint=%known_endpoint, "Refusing to add link local discovered address as there already is a reverse connection");
                            return;
                        }
                    }
                }
            }
        }
        // Only if we don't know it yet.
        let now = tokio::time::Instant::now();
        if let Entry::Vacant(e) = peers.entry(endpoint) {
            e.insert(PeerInfo {
                pt: discovery_type,
                connecting: false,
                pr: if let Some(p) = &peer {
                    p.refer()
                } else {
                    PeerRef::new()
                },
                connection_attempts: 0,
                con_traffic,
                discovered: now,
                connected: if peer.is_some() { Some(now) } else { None },
            });
            if let Some(p) = peer {
                self.router.lock().unwrap().add_peer_interface(p);
            }
            info!("Added new peer");
        } else if discovery_type == PeerType::Inbound {
            // We got an inbound peer with a duplicate entry. This is possible if the sending port
            // is the same as the previous one, which generally happens with our Quic setup. In
            // this case, the old connection needs to be replaced.
            let discovered = peers
                .get(&endpoint)
                .map(|epi| epi.discovered)
                .unwrap_or(now);
            let old_peer_info = peers.insert(
                endpoint,
                PeerInfo {
                    pt: discovery_type,
                    connecting: false,
                    pr: if let Some(p) = &peer {
                        p.refer()
                    } else {
                        PeerRef::new()
                    },
                    connection_attempts: 0,
                    con_traffic,
                    discovered,
                    connected: Some(now),
                },
            );
            // If we have a new peer notify insert the new one in the router, then notify it that
            // the old one is dead.
            if let Some(p) = peer {
                let router = self.router.lock().unwrap();
                router.add_peer_interface(p);
                if let Some(old_peer) = old_peer_info
                    .expect("We already checked the entry was occupied so this is always Some; qed")
                    .pr
                    .upgrade()
                {
                    router.handle_dead_peer(&[old_peer]);
                } else {
                    warn!("Added duplicate inbound entry but did not kill old peer");
                }
            }
            info!("Replaced existing inbound peer");
        } else {
            debug!("Ignoring request to add as it already exists");
        }
        self.metrics.peer_manager_known_peers(peers.len());
    }

    /// Use multicast discovery to find local peers.
    async fn local_discovery(self: Arc<Self>, peer_discovery_port: u16) {
        let rid = self.router.lock().unwrap().router_id();

        let multicast_destination = LL_PEER_DISCOVERY_GROUP
            .parse()
            .expect("Link local discovery group address is properly defined");
        let sock = match UdpSocket::bind(SocketAddr::new(
            "::".parse().expect("Valid all interface IPv6 designator"),
            peer_discovery_port,
        ))
        .map(|result| result.and_then(|sock| set_fw_mark(sock, self.firewall_mark)))
        .await
        {
            Ok(sock) => sock,
            Err(e) => {
                // We won't participate in link local discovery
                error!(err=%e, "Failed to bind multicast discovery socket");
                warn!("Link local peer discovery disabled");
                return;
            }
        };

        info!(
            bind_address=%sock.local_addr().expect("can look up our own address"),
            "Bound multicast discovery interface",
        );

        // Keep track of which interfaces we are already a part of.
        let mut joined_interfaces = HashSet::new();
        // Join the multicast discovery group on newly detected interfaces.
        let join_new_interfaces = |joined_interfaces: &mut HashSet<_>| {
            let ipv6_nics = list_ipv6_interface_ids()?;
            // Keep the existing interfaces, removing interface ids we previously joined but are no
            // longer found when listing ids. We simply discard unknown ids, and assume if the
            // interface is gone (or it's IPv6), that we also implicitly left the group (i.e. no
            // cleanup is needed on our end).
            let kept_interfaces = joined_interfaces.intersection(&ipv6_nics);
            *joined_interfaces = kept_interfaces.copied().collect();
            // Since [`HashSet::difference`] keeps a reference to both sets we need to exhaust the
            // iterator first so we can later mutate the firs set.
            let new_interfaces = ipv6_nics
                .difference(joined_interfaces)
                .copied()
                .collect::<Vec<_>>();
            for new_iface in new_interfaces {
                match sock.join_multicast_v6(&multicast_destination, new_iface) {
                    Err(e) if e.kind() == tokio::io::ErrorKind::AddrInUse => {
                        // This could happen if the multicast listener is already bound but we
                        // somehow forgot about it.
                        debug!(%new_iface, "Multicast group on interface already in use, consider it to be joined");
                        joined_interfaces.insert(new_iface);
                    }
                    Err(e) => {
                        warn!(%new_iface, err=%e, "Failed to join multicast group on interface");
                    }
                    Ok(()) => {
                        debug!(%new_iface, "Joined multicast group on interface");
                        joined_interfaces.insert(new_iface);
                    }
                }
            }

            // A user likely wants to know this. If there is intentionally no IPv6 enabled
            // interface, then the peer discovery can be disabled entirely to save some CPU cycles
            // and silence this.
            if joined_interfaces.is_empty() {
                warn!("Link local peer discovery enabled but discovery group is not joined on any interface");
            }

            Ok::<_, Box<dyn std::error::Error>>(())
        };

        // We don't care about our own multicast beacons
        if let Err(e) = sock.set_multicast_loop_v6(false) {
            warn!("Could not disable multicast loop: {e}");
        }

        let mut beacon = [0; PEER_DISCOVERY_BEACON_SIZE];
        beacon[..8].copy_from_slice(MYCELIUM_MULTICAST_DISCOVERY_MAGIC);
        beacon[8..10].copy_from_slice(&self.tcp_listen_port.to_be_bytes());
        beacon[10..50].copy_from_slice(&rid.as_bytes());

        let mut send_timer = tokio::time::interval(LL_PEER_DISCOVERY_BEACON_INTERVAL);
        send_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            let mut buf = [0; PEER_DISCOVERY_BEACON_SIZE];
            tokio::select! {
                _ = send_timer.tick() => {
                    if let Err(e) = join_new_interfaces(&mut joined_interfaces) {
                        error!(err=%e, "Issue while joining new IPv6 multicast interfaces");
                    };
                    for iface in &joined_interfaces {
                        let dst = SocketAddrV6::new(multicast_destination, peer_discovery_port, 0, *iface);
                        debug!(%dst, %iface, "Sending multicast discovery beacon");
                        if let Err(e) = sock.send_to(
                            &beacon,
                            dst,
                        )
                        .await {
                            error!(iface=%iface, err=%e,"Could not send multicast discovery beacon on interface");
                        }
                    }
                },
                recv_res = sock.recv_from(&mut buf) => {
                    match recv_res {
                        Err(e) => {
                            warn!(err=%e, "Failed to receive multicast message");
                            continue;
                        }
                        Ok((n, remote)) => {
                            trace!(%remote, bytes_received=%n, "Received bytes from remote");
                            self.handle_discovery_packet(&buf[..n], remote);
                        }
                    }
                }
            }
        }
    }

    /// Validates an incoming discovery packet. If the packet is valid, the peer is added to the
    /// `PeerManager`.
    fn handle_discovery_packet(&self, packet: &[u8], mut remote: SocketAddr) {
        if let IpAddr::V6(ip) = remote.ip() {
            // Dumb subnet validation, we only want to discover link local addresses,
            // i.e. part of fe80::/64
            if ip.octets()[..8] != [0xfe, 0x80, 0, 0, 0, 0, 0, 0] {
                trace!("Ignoring non link local IPv6 discovery packet from {remote}");
                return;
            }
        } else {
            trace!("Ignoring non IPv6 discovery packet from {remote}");
            return;
        }
        if packet.len() != PEER_DISCOVERY_BEACON_SIZE {
            trace!("Ignore invalid sized multicast announcement");
            return;
        }
        if &packet[..8] != MYCELIUM_MULTICAST_DISCOVERY_MAGIC {
            trace!("Ignore announcement with invalid multicast magic");
            return;
        }
        let port = u16::from_be_bytes(
            packet[8..10]
                .try_into()
                .expect("Slice size is valid for u16"),
        );
        let remote_rid = RouterId::from(
            <&[u8] as TryInto<[u8; 40]>>::try_into(&packet[10..50])
                .expect("Slice size is valid for RouterId"),
        );
        let rid = self.router.lock().unwrap().router_id();
        if remote_rid == rid {
            debug!("Ignore discovery beacon we sent earlier");
            return;
        }
        // Override the port. Care must be taken since link local IPv6 expects the
        // scope_id to be set.
        remote.set_port(port);
        self.add_peer(
            Endpoint::new(
                if self.private_network_config.is_some() {
                    Protocol::Tls
                } else {
                    Protocol::Tcp
                },
                remote,
            ),
            PeerType::LinkLocalDiscovery,
            ConnectionTraffic {
                tx_bytes: Arc::new(AtomicU64::new(0)),
                rx_bytes: Arc::new(AtomicU64::new(0)),
            },
            None,
        );
    }
}

/// Spawn a quic socket which can be used to both receive quic connections and initiate new quic
/// connections to remotes.
fn make_quic_endpoint(
    router_id: RouterId,
    quic_listen_port: u16,
    firewall_mark: Option<u32>,
) -> Result<quinn::Endpoint, Box<dyn std::error::Error>> {
    // Install ring crypto provider for rustls
    rustls::crypto::CryptoProvider::install_default(rustls::crypto::ring::default_provider())
        .expect("Crypto provider has not been installed yet");
    // Generate self signed certificate certificate.
    // TODO: sign with router keys
    let cert = rcgen::generate_simple_self_signed(vec![format!("{router_id}")])?;
    let certificate_der = CertificateDer::from(cert.cert);
    let private_key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    let certificate_chain = vec![certificate_der];

    let mut server_config = ServerConfig::with_single_cert(certificate_chain, private_key.into())?;
    // We can unwrap this since it's the only current instance.
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    // We don't use unidirectional streams.
    transport_config.max_concurrent_uni_streams(0_u8.into());
    // Larger than needed for now, just in case
    transport_config.max_concurrent_bidi_streams(5_u8.into());
    // Connection timeout, set to higher than Hello interval to ensure connection does not randomly
    // time out.
    transport_config.max_idle_timeout(Some(Duration::from_secs(60).try_into()?));
    transport_config.mtu_discovery_config(Some(MtuDiscoveryConfig::default()));
    transport_config.keep_alive_interval(Some(Duration::from_secs(20)));
    transport_config.datagram_receive_buffer_size(Some(16 << 20));
    transport_config.datagram_send_buffer_size(16 << 20);
    transport_config.initial_mtu(1500);
    transport_config.enable_segmentation_offload(true);
    transport_config.send_window((8 * (10u32 << 20)).into());
    transport_config.stream_receive_window((10u32 << 20).into());
    let mut congestion_controller = congestion::CubicConfig::default();
    congestion_controller.initial_window(1 << 22); // 4MiB
                                                   // TODO: further tweak this.

    let socket = std::net::UdpSocket::bind(("::", quic_listen_port))
        .and_then(|socket| set_fw_mark(socket, firewall_mark))?;
    debug!("Bound UDP socket for Quic");

    // TODO: tweak or confirm
    let endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server_config),
        socket,
        quinn::default_runtime()
            .expect("We are inside the tokio-runtime so this always returns Some(); qed"),
    )?;

    Ok(endpoint)
}

// Firewall marks are only supported on Linux
#[cfg(target_os = "linux")]
fn set_fw_mark<S: AsFd>(socket: S, mark: Option<u32>) -> io::Result<S> {
    use nix::sys::socket::{setsockopt, sockopt};

    if let Some(mark) = mark {
        setsockopt(&socket, sockopt::Mark, &mark)
            .map_or_else(|errno| Err(io::Error::other(errno)), |_| Ok(socket))
    } else {
        Ok(socket)
    }
}

#[cfg(not(target_os = "linux"))]
fn set_fw_mark<S>(socket: S, _mark: Option<u32>) -> io::Result<S> {
    Ok(socket)
}

/// Dummy certificate verifier that treats any certificate as valid.
#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new(provider: Arc<rustls::crypto::CryptoProvider>) -> Arc<Self> {
        Arc::new(Self(provider))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Get a list of the interface identifiers of every network interface with a local IPv6 IP.
fn list_ipv6_interface_ids() -> Result<HashSet<u32>, Box<dyn std::error::Error>> {
    let mut nics = HashSet::new();
    for nic in netdev::get_interfaces()
        .into_iter()
        // Filter out interfaces we don't care about.
        .filter(|nic| {
            !nic.is_loopback()
                && !nic.is_tun()
                && !nic.is_point_to_point()
                && nic.is_multicast()
                && nic.is_up()
        })
    {
        for addr in nic.ipv6 {
            if addr.addr().segments()[..4] == [0xfe80, 0, 0, 0] {
                nics.insert(nic.index);
            }
        }
    }

    Ok(nics)
}

impl fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Alive => "Alive",
            Self::Connecting => "Connecting",
            Self::Dead => "Dead",
        })
    }
}

impl fmt::Display for PeerType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Static => "Static",
            Self::Inbound => "Inbound",
            Self::LinkLocalDiscovery => "LinkLocalDiscovery",
        })
    }
}

impl fmt::Display for PeerExists {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Peer identified by endpoint already exists")
    }
}

impl std::error::Error for PeerExists {}

impl fmt::Display for PeerNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Peer identified by endpoint not known")
    }
}

impl std::error::Error for PeerNotFound {}
