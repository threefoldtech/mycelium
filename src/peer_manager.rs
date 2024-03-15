use crate::connection::Quic;
use crate::endpoint::{Endpoint, Protocol};
use crate::peer::{Peer, PeerRef};
use crate::router::Router;
use crate::router_id::RouterId;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use log::{debug, error, info, trace, warn};
use network_interface::NetworkInterfaceConfig;
use quinn::{MtuDiscoveryConfig, ServerConfig, TransportConfig};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::net::{TcpListener, UdpSocket};
use tokio::time::MissedTickBehavior;

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

/// The PeerManager creates new peers by connecting to configured addresses, and setting up the
/// connection. Once a connection is established, the created [`Peer`] is handed over to the
/// [`Router`].
#[derive(Clone)]
pub struct PeerManager {
    inner: Arc<Inner>,
}

/// Details how the PeerManager learned about a remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerStats {
    /// The endpoint of the [`Peer`].
    pub endpoint: Endpoint,
    /// The (Type)[`PeerType`] of the [`Peer`].
    #[serde(rename = "type")]
    pub pt: PeerType,
    /// State of the connection to this [`Peer`]
    pub connection_state: ConnectionState,
    /// Amount of bytes transmitted on the current connection.
    pub connection_tx_bytes: u64,
    /// Amount of bytes received on the cureent conntection.
    pub connection_rx_bytes: u64,
}

/// Marker error to indicate a [`peer`](Endpoint) is already known.
#[derive(Debug)]
pub struct PeerExists;

/// Marker error to indicate a [`peer`](Endpoint) is not known.
#[derive(Debug)]
pub struct PeerNotFound;

struct Inner {
    /// Router is unfortunately wrapped in a Mutex, because router is not Sync.
    router: Mutex<Router>,
    peers: Mutex<HashMap<Endpoint, PeerInfo>>,
    /// Listen port for new peer connections
    tcp_listen_port: u16,
    quic_socket: quinn::Endpoint,
}

impl PeerManager {
    pub fn new(
        router: Router,
        static_peers_sockets: Vec<Endpoint>,
        tcp_listen_port: u16,
        quic_listen_port: u16,
        peer_discovery_port: u16,
        disable_peer_discovery: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let quic_socket = make_quic_endpoint(router.router_id(), quic_listen_port)?;

        let peer_manager = PeerManager {
            inner: Arc::new(Inner {
                router: Mutex::new(router),
                peers: Mutex::new(
                    static_peers_sockets
                        .into_iter()
                        // These peers are not alive, but we say they are because the reconnect
                        // loop will perform the actual check and figure out they are dead, then
                        // (re)connect.
                        .map(|s| {
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
                                },
                            )
                        })
                        .collect(),
                ),
                tcp_listen_port,
                quic_socket,
            }),
        };

        // Start listeners for inbound connections.
        tokio::spawn(peer_manager.inner.clone().tcp_listener());
        tokio::spawn(peer_manager.inner.clone().quic_listener());

        // Start (re)connecting to outbound/local peers
        tokio::spawn(peer_manager.inner.clone().connect_to_peers());

        // Discover local peers, this does not actually connect to them. That is handle by the
        // connect_to_peers task.
        if !disable_peer_discovery {
            tokio::spawn(
                peer_manager
                    .inner
                    .clone()
                    .local_discovery(peer_discovery_port),
            );
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
            let (connection_state, connection_tx_bytes, connection_rx_bytes) =
                match peer_info.pr.upgrade() {
                    None if !peer_info.connecting => (ConnectionState::Dead, 0, 0),
                    None => (ConnectionState::Connecting, 0, 0),
                    Some(peer) => (ConnectionState::Alive, peer.written(), peer.read()),
                };
            pi.push(PeerStats {
                endpoint: *endpoint,
                pt: peer_info.pt.clone(),
                connection_state,
                connection_tx_bytes,
                connection_rx_bytes,
            });
        }
        pi
    }
}

impl Inner {
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
                        pi.connecting = false;
                        if let Some(peer) = maybe_new_peer {
                            // We did find a new Peer, insert into router and keep track of it
                            // Use fully qualified call to aid compiler in type inference.
                            pi.pr = Peer::refer(&peer);
                            self.router.lock().unwrap().add_peer_interface(peer);
                        } else {
                            // Connection failed, add a failed attempt and forget about the peer if
                            // needed.
                            pi.connection_attempts += 1;
                            if pi.pt == PeerType::LinkLocalDiscovery
                                && pi.connection_attempts >= MAX_FAILED_LOCAL_PEER_CONNECTION_ATTEMPTS {
                                info!("Forgetting about locally discovered peer {endpoint} after failing to connect to it");
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
                            debug!("Found dead peer {endpoint}");
                            if pi.pt == PeerType::Inbound {
                                debug!("Refusing to reconnect to inbound peer");
                                continue
                            }
                            // Mark that we are connecting to the peer.
                            pi.connecting = true;
                            connection_futures.push(self.clone().connect_peer(*endpoint, pi.con_traffic.clone()));
                        }
                    }
                }
            }
        }
    }

    /// Create a new connection to a remote peer
    async fn connect_peer(
        self: Arc<Self>,
        endpoint: Endpoint,
        ct: ConnectionTraffic,
    ) -> (Endpoint, Option<Peer>) {
        debug!("Connecting to {endpoint}");
        match endpoint.proto() {
            Protocol::Tcp => self.connect_tcp_peer(endpoint, ct).await,
            Protocol::Quic => self.connect_quic_peer(endpoint, ct).await,
        }
    }

    async fn connect_tcp_peer(
        self: Arc<Self>,
        endpoint: Endpoint,
        ct: ConnectionTraffic,
    ) -> (Endpoint, Option<Peer>) {
        match TcpStream::connect(endpoint.address()).await {
            Ok(peer_stream) => {
                debug!("Opened connection to {endpoint}");
                // Make sure Nagle's algorithm is disabeld as it can cause latency spikes.
                if let Err(e) = peer_stream.set_nodelay(true) {
                    error!("Couldn't disable Nagle's algorithm on stream {e}");
                    return (endpoint, None);
                }

                // Scope the MutexGuard, if we don't do this the future won't be Send
                let res = {
                    let router = self.router.lock().unwrap();
                    let router_data_tx = router.router_data_tx();
                    let router_control_tx = router.router_control_tx();
                    let dead_peer_sink = router.dead_peer_sink().clone();

                    Peer::new(
                        router_data_tx,
                        router_control_tx,
                        peer_stream,
                        dead_peer_sink,
                        ct.tx_bytes,
                        ct.rx_bytes,
                    )
                };
                match res {
                    Ok(new_peer) => {
                        info!("Connected to new peer {}", endpoint);
                        (endpoint, Some(new_peer))
                    }
                    Err(e) => {
                        error!("Failed to spawn peer {endpoint}: {e}");
                        (endpoint, None)
                    }
                }
            }
            Err(e) => {
                error!("Couldn't connect to {endpoint}: {e}");
                (endpoint, None)
            }
        }
    }

    async fn connect_quic_peer(
        self: Arc<Self>,
        endpoint: Endpoint,
        ct: ConnectionTraffic,
    ) -> (Endpoint, Option<Peer>) {
        let mut config = quinn::ClientConfig::new(Arc::new(
            rustls::ClientConfig::builder()
                .with_safe_defaults()
                .with_custom_certificate_verifier(SkipServerVerification::new())
                .with_no_client_auth(),
        ));
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
        transport_config.datagram_receive_buffer_size(None);
        transport_config.datagram_send_buffer_size(0);
        config.transport_config(Arc::new(transport_config));

        match self
            .quic_socket
            .connect_with(config, endpoint.address(), "dummy.mycelium")
        {
            Ok(connecting) => match connecting.await {
                Ok(con) => match con.open_bi().await {
                    Ok((tx, rx)) => {
                        let q_con = Quic::new(tx, rx, endpoint.address());
                        let res = {
                            let router = self.router.lock().unwrap();
                            let router_data_tx = router.router_data_tx();
                            let router_control_tx = router.router_control_tx();
                            let dead_peer_sink = router.dead_peer_sink().clone();

                            Peer::new(
                                router_data_tx,
                                router_control_tx,
                                q_con,
                                dead_peer_sink,
                                ct.tx_bytes,
                                ct.rx_bytes,
                            )
                        };
                        match res {
                            Ok(new_peer) => {
                                info!("Connected to new peer {}", endpoint);
                                (endpoint, Some(new_peer))
                            }
                            Err(e) => {
                                error!("Failed to spawn peer {endpoint}: {e}");
                                (endpoint, None)
                            }
                        }
                    }
                    Err(e) => {
                        error!("Couldn't open bidirectional quic stream to {endpoint}: {e}");
                        (endpoint, None)
                    }
                },
                Err(e) => {
                    error!("Couldn't complete quic connection to {endpoint}: {e}");
                    (endpoint, None)
                }
            },
            Err(e) => {
                error!("Couldn't initiate connection to {endpoint}: {e}");
                (endpoint, None)
            }
        }
    }

    async fn tcp_listener(self: Arc<Self>) {
        // Take a copy of every channel here first so we avoid lock contention in the loop later.
        let router_data_tx = self.router.lock().unwrap().router_data_tx();
        let router_control_tx = self.router.lock().unwrap().router_control_tx();
        let dead_peer_sink = self.router.lock().unwrap().dead_peer_sink().clone();

        match TcpListener::bind(("::", self.tcp_listen_port)).await {
            Ok(listener) => loop {
                match listener.accept().await {
                    Ok((stream, remote)) => {
                        let tx_bytes = Arc::new(AtomicU64::new(0));
                        let rx_bytes = Arc::new(AtomicU64::new(0));
                        let new_peer = match Peer::new(
                            router_data_tx.clone(),
                            router_control_tx.clone(),
                            stream,
                            dead_peer_sink.clone(),
                            tx_bytes.clone(),
                            rx_bytes.clone(),
                        ) {
                            Ok(peer) => peer,
                            Err(e) => {
                                error!("Failed to spawn peer: {e}");
                                continue;
                            }
                        };
                        info!("Accepted new inbound peer {}", remote);
                        self.add_peer(
                            Endpoint::new(Protocol::Tcp, remote),
                            PeerType::Inbound,
                            ConnectionTraffic { tx_bytes, rx_bytes },
                            Some(new_peer),
                        );
                    }
                    Err(e) => {
                        error!("Error accepting connection: {}", e);
                    }
                }
            },
            Err(e) => {
                error!("Error starting listener: {}", e);
            }
        }
    }

    async fn quic_listener(self: Arc<Self>) {
        // Take a copy of every channel here first so we avoid lock contention in the loop later.
        let router_data_tx = self.router.lock().unwrap().router_data_tx();
        let router_control_tx = self.router.lock().unwrap().router_control_tx();
        let dead_peer_sink = self.router.lock().unwrap().dead_peer_sink().clone();

        loop {
            let con = if let Some(con) = self.quic_socket.accept().await {
                match con.await {
                    Ok(con) => con,
                    Err(e) => {
                        debug!("Failed to accept quic connection: {e}");
                        continue;
                    }
                }
            } else {
                // Con is closed
                info!("Shutting down closed quic listener");
                return;
            };

            let q = match con.accept_bi().await {
                Ok((tx, rx)) => Quic::new(tx, rx, con.remote_address()),
                Err(e) => {
                    error!("Failed to accept bidirectional quic stream: {e}");
                    continue;
                }
            };

            let tx_bytes = Arc::new(AtomicU64::new(0));
            let rx_bytes = Arc::new(AtomicU64::new(0));
            let new_peer = match Peer::new(
                router_data_tx.clone(),
                router_control_tx.clone(),
                q,
                dead_peer_sink.clone(),
                tx_bytes.clone(),
                rx_bytes.clone(),
            ) {
                Ok(peer) => peer,
                Err(e) => {
                    error!("Failed to spawn peer: {e}");
                    continue;
                }
            };
            info!("Accepted new inbound quic peer {}", con.remote_address());
            self.add_peer(
                Endpoint::new(Protocol::Quic, con.remote_address()),
                PeerType::Inbound,
                ConnectionTraffic { tx_bytes, rx_bytes },
                Some(new_peer),
            )
        }
    }

    /// Add a new peer identifier we discovered.
    fn add_peer(
        &self,
        endpoint: Endpoint,
        discovery_type: PeerType,
        con_traffic: ConnectionTraffic,
        peer: Option<Peer>,
    ) {
        let mut peers = self.peers.lock().unwrap();
        // Only if we don't know it yet.
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
            });
            if let Some(p) = peer {
                self.router.lock().unwrap().add_peer_interface(p);
            }
            info!("Added new peer {endpoint}");
        } else if discovery_type == PeerType::Inbound {
            // We got an inbound peer with a duplicate entry. This is possible if the sending port
            // is the same as the previous one, which generally happens with our Quic setup. In
            // this case, the old connection needs to be replaced.
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
                    router.handle_dead_peer(old_peer);
                }
            }
            info!("Replaced existing inbound peer {endpoint}");
        } else {
            debug!("Ignoring request to add {endpoint} as it already exists");
        }
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
        .await
        {
            Ok(sock) => sock,
            Err(e) => {
                // We won't participate in link local discovery
                error!("Failed to bind multicast discovery socket: {e}");
                warn!("Link local peer discovery disabled");
                return;
            }
        };

        info!(
            "Bound multicast discovery interface to {}",
            sock.local_addr().expect("can look up our own address")
        );

        // Keep track of which interfaces we are already a part of.
        let mut joined_interfaces = HashSet::new();
        // Join the multicast discovery group on newly detected interfaces.
        let mut join_new_interfaces = || {
            let ipv6_nics = list_ipv6_interface_ids()?;
            // Keep the existing interfaces, removing interface ids we previously joined but are no
            // longer found when listing ids. We simply discard unknown ids, and assume if the
            // interface is gone (or it's IPv6), that we also implicitly left the group (i.e. no
            // cleanup is needed on our end).
            let kept_interfaces = joined_interfaces.intersection(&ipv6_nics);
            joined_interfaces = kept_interfaces.copied().collect();
            // Since [`HashSet::difference`] keeps a reference to both sets we need to exhaust the
            // iterator first so we can later mutate the firs set.
            let new_interfaces = ipv6_nics
                .difference(&joined_interfaces)
                .copied()
                .collect::<Vec<_>>();
            for new_iface in new_interfaces {
                if let Err(e) = sock.join_multicast_v6(&multicast_destination, new_iface) {
                    warn!("Failed to join multicast group on interface {new_iface}: {e}");
                } else {
                    debug!("Joined multicast group on interface {new_iface}");
                    joined_interfaces.insert(new_iface);
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
                    if let Err(e) = join_new_interfaces() {
                        error!("Issue while joining new IPv6 multicast interfaces: {e}");
                    };
                    if let Err(e) = sock.send_to(
                        &beacon,
                        SocketAddr::new(multicast_destination.into(), peer_discovery_port),
                    )
                    .await {
                        error!("Could not send multicast discovery beacon {e}");
                    }
                },
                recv_res = sock.recv_from(&mut buf) => {
                    match recv_res {
                        Err(e) => {
                            warn!("Failed to receive multicast message {e}");
                            continue;
                        }
                        Ok((n, remote)) => {
                            trace!("Received {n} bytes from {remote}");
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
            Endpoint::new(Protocol::Tcp, remote),
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
) -> Result<quinn::Endpoint, Box<dyn std::error::Error>> {
    // Generate self signed certificate certificate.
    // TODO: sign with router keys
    let cert = rcgen::generate_simple_self_signed(vec![format!("{router_id}")])?;
    let certificate_der = cert.serialize_der()?;
    let private_key_der = cert.serialize_private_key_der();
    let private_key = rustls::PrivateKey(private_key_der);
    let certificate_chain = vec![rustls::Certificate(certificate_der)];

    let mut server_config = ServerConfig::with_single_cert(certificate_chain, private_key)?;
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
    // we don't use datagrams.
    transport_config.datagram_receive_buffer_size(None);
    transport_config.datagram_send_buffer_size(0);
    // TODO: further tweak this.

    let socket = std::net::UdpSocket::bind(("::", quic_listen_port))?;
    debug!("Bound UDP socket for Quic");

    //TODO tweak or confirm
    let endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server_config),
        socket,
        quinn::default_runtime()
            .expect("We are inside the tokio-runtime so this always returns Some(); qed"),
    )?;

    Ok(endpoint)
}

/// Dummy certificate verifier that treats any certificate as valid.
struct SkipServerVerification;

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::client::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}

/// Get a list of the interface identifiers of every network interface with a local IPv6 IP.
fn list_ipv6_interface_ids() -> Result<HashSet<u32>, Box<dyn std::error::Error>> {
    let mut nics = HashSet::new();
    for nic in network_interface::NetworkInterface::show()? {
        for addr in nic.addr {
            if let network_interface::Addr::V6(addr) = addr {
                // Check if the address is part of fe80::/64
                if addr.ip.segments()[..4] == [0xfe80, 0, 0, 0] {
                    nics.insert(nic.index);
                }
            }
        }
    }

    Ok(nics)
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
