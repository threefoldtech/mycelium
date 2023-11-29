use crate::packet::{ControlPacket, DataPacket};
use crate::peer::{Peer, PeerRef};
use crate::router::Router;
use crate::router_id::RouterId;
use log::{debug, error, info, trace, warn};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc::{self, Sender, UnboundedSender};
use tokio::time::MissedTickBehavior;

/// Magic bytes to identify a multicast UDP packet used in link local peer discovery.
const MYCELIUM_MULTICAST_DISCOVERY_MAGIC: &[u8; 8] = b"mycelium";
/// Size of a peer discovery beacon.
const PEER_DISCOVERY_BEACON_SIZE: usize = 8 + 2 + 40;
/// Link local peer discovery group joined by the UDP listener.
const LL_PEER_DISCOVERY_GROUP: &str = "ff02::cafe";
/// The time between sending consecutive link local discovery beacons.
const LL_PEER_DISCOVERY_BEACON_INTERVAL: Duration = Duration::from_secs(60);

/// The PeerManager creates new peers by connecting to configured addresses, and setting up the
/// connection. Once a connection is established, the created [`Peer`] is handed over to the
/// [`Router`].
#[derive(Clone)]
pub struct PeerManager {
    inner: Arc<Inner>,
}

/// Details how the PeerManager learned about a remote.
#[derive(PartialEq, Eq)]
enum PeerType {
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
}

struct Inner {
    /// Router is unfortunately wrapped in a Mutex, because router is not Sync.
    router: Mutex<Router>,
    peers: Mutex<HashMap<SocketAddr, PeerInfo>>,
    /// Listen port for new peer connections
    listen_port: u16,
}

impl PeerManager {
    pub fn new(router: Router, static_peers_sockets: Vec<SocketAddr>, listen_port: u16) -> Self {
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
                                },
                            )
                        })
                        .collect(),
                ),
                listen_port,
            }),
        };
        // Start a TCP listener. When a new connection is accepted, the reverse peer exchange is performed.
        tokio::spawn(Inner::start_listener(peer_manager.inner.clone()));

        tokio::spawn(Inner::connect_to_peers(peer_manager.inner.clone()));

        tokio::spawn(Inner::local_discovery(peer_manager.inner.clone(), 9651));

        peer_manager
    }
}

impl Inner {
    /// Connect and if needed reconnect to known peers.
    async fn connect_to_peers(self: Arc<Self>) {
        let node_tun_addr =
            if let IpAddr::V6(ip) = self.router.lock().unwrap().node_tun_subnet().address() {
                ip
            } else {
                panic!("Non IPv6 node tun not support currently")
            };

        let (np_tx, mut np_rx) = mpsc::channel::<(SocketAddr, Option<Peer>)>(1);

        let router_data_tx = self.router.lock().unwrap().router_data_tx();
        let router_control_tx = self.router.lock().unwrap().router_control_tx();
        let dead_peer_sink = self.router.lock().unwrap().dead_peer_sink().clone();

        let mut peer_check_interval = tokio::time::interval(Duration::from_secs(5));
        // Avoid trying to spam connections. Since we track if we are connecting to a peer this
        // won't be that bad, but this avoid unnecessary lock contention.
        peer_check_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                mnp = np_rx.recv() => {
                    match mnp {
                        None => {
                            error!("Can't reconnect to new peers anymore!");
                            return
                        },
                        Some((socket, maybe_new_peer)) => {
                            // Only insert the possible new peer if we actually still care about it
                            if let Some(pi) = self.peers.lock().unwrap().get_mut(&socket) {
                                // Regardless of what happened, we are no longer connecting.
                                pi.connecting = false;
                                if let Some(peer) = maybe_new_peer {
                                    // We did find a new Peer, insert into router and keep track of it
                                    pi.pr = peer.refer();
                                    self.router.lock().unwrap().add_peer_interface(peer);
                                }
                            }
                        },
                    }
                }
                _ = peer_check_interval.tick() => {
                    // Remove dead inbound peers
                    self.peers.lock().unwrap().retain(|_, v| v.pt != PeerType::Inbound || v.pr.alive());
                    debug!("Looking for dead peers");
                    // check if there is an entry for the peer in the router's peer list
                    for (socket, pi) in self.peers.lock().unwrap().iter_mut() {
                        if !pi.connecting && !pi.pr.alive() {
                            debug!("Found dead peer {socket}");
                            if pi.pt == PeerType::Inbound {
                                debug!("Refusing to reconnect to inbound peer");
                                continue
                            }
                            // Mark that we are connecting to the peer.
                            pi.connecting = true;
                            tokio::spawn({
                                let np_tx = np_tx.clone();
                                let router_data_tx = router_data_tx.clone();
                                let router_control_tx = router_control_tx.clone();
                                let dead_peer_sink = dead_peer_sink.clone();
                                let socket = *socket;

                                async move {
                                    debug!("Connecting to {socket}");
                                    match TcpStream::connect(socket).await {
                                        Ok(mut peer_stream) => {
                                            debug!("Opened connection to {socket}");
                                            let mut buffer = [0u8; 17];
                                            if let Err(e) = peer_stream.read_exact(&mut buffer).await {
                                                info!("Failed to read hanshake from peer: {e}");
                                                if let Err(e) = np_tx.send((socket, None)).await {
                                                    error!("Could not notify peer manager of connection failure: {e}");
                                                }
                                                return;
                                            }
                                            let _ = match buffer[0] {
                                                0 => IpAddr::from(
                                                    <&[u8] as TryInto<[u8; 4]>>::try_into(&buffer[1..5]).unwrap(),
                                                ),
                                                1 => IpAddr::from(
                                                    <&[u8] as TryInto<[u8; 16]>>::try_into(&buffer[1..]).unwrap(),
                                                ),
                                                _ => {
                                                    error!("Invalid address encoding byte");
                                                    if let Err(e) = np_tx.send((socket, None)).await {
                                                        error!("Could not notify peer manager of connection failure: {e}");
                                                    }
                                                    return;
                                                }
                                            };

                                            let mut buf = [0u8; 17];
                                            // only using IPv6
                                            buf[0] = 1;
                                            buf[1..].copy_from_slice(&node_tun_addr.octets()[..]);

                                            if let Err(e) = peer_stream.write_all(&buf).await {
                                                info!("Failed to read hanshake from peer: {e}");
                                                if let Err(e) = np_tx.send((socket, None)).await {
                                                    error!("Could not notify peer manager of connection failure: {e}");
                                                }
                                                return;
                                            };

                                            // Make sure Nagle's algorithm is disabeld as it can cause latency spikes.
                                            if let Err(e) = peer_stream.set_nodelay(true) {
                                                error!("Couldn't disable Naglle's algorithm on stream {e}");
                                                if let Err(e) = np_tx.send((socket, None)).await {
                                                    error!("Could not notify peer manager of connection failure: {e}");
                                                }
                                                return
                                            }

                                            let peer_stream_ip = socket.ip();
                                            let new_peer = Peer::new(
                                                peer_stream_ip,
                                                router_data_tx.clone(),
                                                router_control_tx.clone(),
                                                peer_stream,
                                                dead_peer_sink.clone(),
                                            );

                                            info!("Connected to new peer {}", new_peer.underlay_ip());
                                            if let Err(e) = np_tx.send((socket, Some(new_peer))).await {
                                                error!("Could not notify peer manager of new connection: {e}");
                                            }
                                        },
                                        Err(e) => {
                                            error!("Couldn't connect to to remote {e}");
                                            if let Err(e) = np_tx.send((socket, None)).await {
                                                error!("Could not notify peer manager of connection failure: {e}");
                                            }
                                        },
                            }}});
                        }
                    }
                }
            }
        }
    }

    async fn start_listener(self: Arc<Self>) {
        let node_tun_addr =
            if let IpAddr::V6(ip) = self.router.lock().unwrap().node_tun_subnet().address() {
                ip
            } else {
                panic!("Non IPv6 node tun not support currently")
            };
        let router_data_tx = self.router.lock().unwrap().router_data_tx();
        let router_control_tx = self.router.lock().unwrap().router_control_tx();
        let dead_peer_sink = self.router.lock().unwrap().dead_peer_sink().clone();

        match TcpListener::bind(("::", self.listen_port)).await {
            Ok(listener) => loop {
                match listener.accept().await {
                    Ok((stream, remote)) => {
                        info!("New peer connected from {remote}");
                        let new_peer = Self::start_reverse_peer_exchange(
                            stream,
                            node_tun_addr,
                            router_data_tx.clone(),
                            router_control_tx.clone(),
                            dead_peer_sink.clone(),
                        )
                        .await;
                        match new_peer {
                            Ok(peer) => {
                                info!("Accepted new peer {}", peer.underlay_ip());
                                self.add_peer(remote, PeerType::Inbound, Some(peer));
                            }
                            Err(e) => {
                                error!("Failed to accept new peer {e}");
                            }
                        }
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

    async fn start_reverse_peer_exchange(
        mut stream: TcpStream,
        node_tun_addr: Ipv6Addr,
        router_data_tx: Sender<DataPacket>,
        router_control_tx: UnboundedSender<(ControlPacket, Peer)>,
        dead_peer_sink: Sender<Peer>,
    ) -> Result<Peer, Box<dyn std::error::Error>> {
        // Steps:
        // 1. Send own TUN address over the stream
        // 2. Read other node's TUN address from the stream

        let mut buf = [0u8; 17];
        // only using IPv6
        buf[0] = 1;
        buf[1..].copy_from_slice(&node_tun_addr.octets()[..]);

        // Step 1
        stream.write_all(&buf).await?;
        // Step 2
        stream.read_exact(&mut buf).await?;
        let _ = match buf[0] {
            0 => IpAddr::from(<&[u8] as TryInto<[u8; 4]>>::try_into(&buf[1..5]).unwrap()),
            1 => IpAddr::from(<&[u8] as TryInto<[u8; 16]>>::try_into(&buf[1..]).unwrap()),
            _ => {
                error!("Invalid address encoding byte");
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid encoding byte received",
                )
                .into());
            }
        };

        // Create new Peer instance
        let peer_stream_ip = stream.peer_addr().unwrap().ip();

        // Make sure Nagle's algorithm is disabeld as it can cause latency spikes.
        stream.set_nodelay(true)?;

        Ok(Peer::new(
            peer_stream_ip,
            router_data_tx,
            router_control_tx,
            stream,
            dead_peer_sink,
        ))
    }

    /// Add a new peer identifier we discovered.
    fn add_peer(&self, addr: SocketAddr, discovery_type: PeerType, peer: Option<Peer>) {
        let mut peers = self.peers.lock().unwrap();
        // Only if we don't know it yet.
        if let Entry::Vacant(e) = peers.entry(addr) {
            e.insert(PeerInfo {
                pt: discovery_type,
                connecting: false,
                pr: if let Some(p) = &peer {
                    p.refer()
                } else {
                    PeerRef::new()
                },
            });
            if let Some(p) = peer {
                self.router.lock().unwrap().add_peer_interface(p);
            }
            info!("Added new peer {addr}");
        } else {
            debug!("Ignoring request to add {addr} as it already exists");
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
        if let Err(e) = sock.join_multicast_v6(&multicast_destination, 0) {
            error!("Failed to join multicast group {e}");
            warn!("Link local peer discovery disabled");
            return;
        }

        let mut beacon = [0; PEER_DISCOVERY_BEACON_SIZE];
        beacon[..8].copy_from_slice(MYCELIUM_MULTICAST_DISCOVERY_MAGIC);
        beacon[8..10].copy_from_slice(&self.listen_port.to_be_bytes());
        beacon[10..50].copy_from_slice(&rid.as_bytes());

        let mut send_timer = tokio::time::interval(LL_PEER_DISCOVERY_BEACON_INTERVAL);
        send_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            let mut buf = [0; PEER_DISCOVERY_BEACON_SIZE];
            tokio::select! {
                _ = send_timer.tick() => {
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
        self.add_peer(remote, PeerType::LinkLocalDiscovery, None);
    }
}
