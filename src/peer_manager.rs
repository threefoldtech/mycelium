use crate::packet::{ControlPacket, DataPacket};
use crate::peer::{Peer, PeerRef};
use crate::router::Router;
use log::{error, info};
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, Sender, UnboundedSender};
use tokio::time::MissedTickBehavior;

/// The PeerManager creates new peers by connecting to configured addresses, and setting up the
/// connection. Once a connection is established, the created [`Peer`] is handed over to the
/// [`Router`].
#[derive(Clone)]
pub struct PeerManager {
    inner: Arc<Inner>,
}

/// State of the peers we are tracking.
#[derive(PartialEq, Eq)]
enum PeerState {
    /// Peer was alive last time we checked.
    Alive,
    /// Peer is currently in the process of being connected to.
    Connecting,
}

struct Inner {
    /// Router is unfortunately wrapped in a Mutex, because router is not Sync.
    router: Mutex<Router>,
    peers: Mutex<HashMap<SocketAddr, (PeerState, PeerRef)>>,
}

impl PeerManager {
    pub fn new(router: Router, static_peers_sockets: Vec<SocketAddr>, port: u16) -> Self {
        let peer_manager = PeerManager {
            inner: Arc::new(Inner {
                router: Mutex::new(router),
                peers: Mutex::new(
                    static_peers_sockets
                        .into_iter()
                        // These peers are not alive, but we say they are because the reconnect
                        // loop will perform the actual check and figure out they are dead, then
                        // (re)connect.
                        .map(|s| (s, (PeerState::Alive, PeerRef::new())))
                        .collect(),
                ),
            }),
        };
        // Start a TCP listener. When a new connection is accepted, the reverse peer exchange is performed.
        tokio::spawn(Inner::start_listener(peer_manager.inner.clone(), port));

        tokio::spawn(Inner::reconnect_to_initial_peers(
            peer_manager.inner.clone(),
        ));

        peer_manager
    }
}

impl Inner {
    // this is used to reconnect to the provided static peers in case the connection is lost
    async fn reconnect_to_initial_peers(self: Arc<Self>) {
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
                    if let Some((state, old_peer)) = self.peers.lock().unwrap().get_mut(&socket) {
                        // Set this to alive regardless. Either it is, and it's correct, or it
                        // failed, in which case setting this to alive will trigger a reconnect next time
                        // the interval fires.
                        *state = PeerState::Alive;
                        if let Some(peer) = maybe_new_peer {
                            // We did find a new Peer, insert into router and keep track of it
                            *old_peer = peer.refer();
                            self.router.lock().unwrap().add_peer_interface(peer);
                        }
                    }
                },
                }
                }
                _ = peer_check_interval.tick() => {
                    // check if there is an entry for the peer in the router's peer list
                    for (socket, (state, peer)) in self.peers.lock().unwrap().iter_mut() {
                        if state == &PeerState::Alive && !peer.alive() {
                            // Mark that we are connecting to the peer.
                            *state = PeerState::Connecting;
                            tokio::spawn({
                                let np_tx = np_tx.clone();
                                let router_data_tx = router_data_tx.clone();
                                let router_control_tx = router_control_tx.clone();
                                let dead_peer_sink = dead_peer_sink.clone();
                                let socket = *socket;

                                async move {
                                    if let Ok(mut peer_stream) = TcpStream::connect(socket).await {
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
                                    }
                            }});
                        }
                    }
                }
            }
        }
    }

    async fn start_listener(self: Arc<Self>, port: u16) {
        let node_tun_addr =
            if let IpAddr::V6(ip) = self.router.lock().unwrap().node_tun_subnet().address() {
                ip
            } else {
                panic!("Non IPv6 node tun not support currently")
            };
        let router_data_tx = self.router.lock().unwrap().router_data_tx();
        let router_control_tx = self.router.lock().unwrap().router_control_tx();
        let dead_peer_sink = self.router.lock().unwrap().dead_peer_sink().clone();

        match TcpListener::bind(("::", port)).await {
            Ok(listener) => loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
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
                                self.router.lock().unwrap().add_peer_interface(peer);
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
}
