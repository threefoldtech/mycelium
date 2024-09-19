use std::net::{IpAddr, Ipv6Addr};
#[cfg(feature = "message")]
use std::{future::Future, time::Duration};

use crate::tun::TunConfig;
use bytes::BytesMut;
use data::DataPlane;
use endpoint::Endpoint;
#[cfg(feature = "message")]
use message::{
    MessageId, MessageInfo, MessagePushResponse, MessageStack, PushMessageError, ReceivedMessage,
};
use metrics::Metrics;
use peer_manager::{PeerExists, PeerNotFound, PeerStats, PrivateNetworkKey};
use routing_table::RouteEntry;
use subnet::Subnet;
use tracing::{error, info, warn};

mod babel;
mod connection;
pub mod crypto;
pub mod data;
pub mod endpoint;
pub mod filters;
mod interval;
#[cfg(feature = "message")]
pub mod message;
mod metric;
pub mod metrics;
pub mod packet;
mod peer;
pub mod peer_manager;
pub mod router;
mod router_id;
mod routing_table;
mod seqno_cache;
mod sequence_number;
mod source_table;
pub mod subnet;
pub mod task;
mod tun;

/// The prefix of the global subnet used.
pub const GLOBAL_SUBNET_ADDRESS: IpAddr = IpAddr::V6(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 0));
/// The prefix length of the global subnet used.
pub const GLOBAL_SUBNET_PREFIX_LEN: u8 = 7;

/// Config for a mycelium [`Node`].
pub struct Config<M> {
    /// The secret key of the node.
    pub node_key: crypto::SecretKey,
    /// Statically configured peers.
    pub peers: Vec<Endpoint>,
    /// Tun interface should be disabled.
    pub no_tun: bool,
    /// Listen port for TCP connections.
    pub tcp_listen_port: u16,
    /// Listen port for Quic connections.
    pub quic_listen_port: Option<u16>,
    /// Udp port for peer discovery.
    pub peer_discovery_port: Option<u16>,
    /// Name for the TUN device.
    #[cfg(any(
        target_os = "linux",
        all(target_os = "macos", not(feature = "mactunfd")),
        target_os = "windows"
    ))]
    pub tun_name: String,

    /// Configuration for a private network, if run in that mode. To enable private networking,
    /// this must be a name + a PSK.
    pub private_network_config: Option<(String, PrivateNetworkKey)>,
    /// Implementation of the `Metrics` trait, used to expose information about the system
    /// internals.
    pub metrics: M,
    /// Mark that's set on all packets that we send on the underlying network
    pub firewall_mark: Option<u32>,

    // tun_fd is android, iOS, macos on appstore specific option
    // We can't create TUN device from the Rust code in android, iOS, and macos on appstore.
    // So, we create the TUN device on Kotlin(android) or Swift(iOS, macos) then pass
    // the TUN's file descriptor to mycelium.
    #[cfg(any(
        target_os = "android",
        target_os = "ios",
        all(target_os = "macos", feature = "mactunfd"),
    ))]
    pub tun_fd: Option<i32>,

    /// The maount of worker tasks spawned to process updates. Up to this amound of updates can be
    /// processed in parallel. Because processing an update is a CPU bound task, it is pointless to
    /// set this to a value which is higher than the amount of logical CPU cores available to the
    /// system.
    pub update_workers: usize,
}

/// The Node is the main structure in mycelium. It governs the entire data flow.
pub struct Node<M> {
    router: router::Router<M>,
    peer_manager: peer_manager::PeerManager<M>,
    #[cfg(feature = "message")]
    message_stack: message::MessageStack<M>,
}

/// General info about a node.
pub struct NodeInfo {
    /// The overlay subnet in use by the node.
    pub node_subnet: Subnet,
    /// The public key of the node
    pub node_pubkey: crypto::PublicKey,
}

impl<M> Node<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    /// Setup a new `Node` with the provided [`Config`].
    pub async fn new(config: Config<M>) -> Result<Self, Box<dyn std::error::Error>> {
        // If a private network is configured, validate network name
        if let Some((net_name, _)) = &config.private_network_config {
            if net_name.len() < 2 || net_name.len() > 64 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "network name must be between 2 and 64 characters",
                )
                .into());
            }
        }
        let node_pub_key = crypto::PublicKey::from(&config.node_key);
        let node_addr = node_pub_key.address();
        let (tun_tx, tun_rx) = tokio::sync::mpsc::unbounded_channel();

        let node_subnet = Subnet::new(
            // Truncate last 64 bits of address.
            // TODO: find a better way to do this.
            Subnet::new(node_addr.into(), 64)
                .expect("64 is a valid IPv6 prefix size; qed")
                .network(),
            64,
        )
        .expect("64 is a valid IPv6 prefix size; qed");

        // Creating a new Router instance
        let router = match router::Router::new(
            config.update_workers,
            tun_tx,
            node_subnet,
            vec![node_subnet],
            (config.node_key, node_pub_key),
            vec![
                Box::new(filters::AllowedSubnet::new(
                    Subnet::new(GLOBAL_SUBNET_ADDRESS, GLOBAL_SUBNET_PREFIX_LEN)
                        .expect("Global subnet is properly defined; qed"),
                )),
                Box::new(filters::MaxSubnetSize::<64>),
                Box::new(filters::RouterIdOwnsSubnet),
            ],
            config.metrics.clone(),
        ) {
            Ok(router) => {
                info!(
                    "Router created. Pubkey: {:x}",
                    BytesMut::from(&router.node_public_key().as_bytes()[..])
                );
                router
            }
            Err(e) => {
                error!("Error creating router: {e}");
                panic!("Error creating router: {e}");
            }
        };

        // Creating a new PeerManager instance
        let pm = peer_manager::PeerManager::new(
            router.clone(),
            config.peers,
            config.tcp_listen_port,
            config.quic_listen_port,
            config.peer_discovery_port.unwrap_or_default(),
            config.peer_discovery_port.is_none(),
            config.private_network_config,
            config.metrics,
            config.firewall_mark,
        )?;
        info!("Started peer manager");

        #[cfg(feature = "message")]
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        #[cfg(feature = "message")]
        let msg_receiver = tokio_stream::wrappers::ReceiverStream::new(rx);
        #[cfg(feature = "message")]
        let msg_sender = tokio_util::sync::PollSender::new(tx);
        #[cfg(not(feature = "message"))]
        let msg_sender = futures::sink::drain();

        let _data_plane = if config.no_tun {
            warn!("Starting data plane without TUN interface, L3 functionality disabled");
            DataPlane::new(
                router.clone(),
                // No tun so create a dummy stream for L3 packets which never yields
                tokio_stream::pending(),
                // Similarly, create a sink which just discards every packet we would receive
                futures::sink::drain(),
                msg_sender,
                tun_rx,
            )
        } else {
            #[cfg(not(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "windows",
                target_os = "android",
                target_os = "ios"
            )))]
            {
                panic!("On this platform, you can only run with --no-tun");
            }
            #[cfg(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "windows",
                target_os = "android",
                target_os = "ios"
            ))]
            {
                #[cfg(any(
                    target_os = "linux",
                    all(target_os = "macos", not(feature = "mactunfd")),
                    target_os = "windows"
                ))]
                let tun_config = TunConfig {
                    name: config.tun_name.clone(),
                    node_subnet: Subnet::new(node_addr.into(), 64)
                        .expect("64 is a valid subnet size for IPv6; qed"),
                    route_subnet: Subnet::new(GLOBAL_SUBNET_ADDRESS, GLOBAL_SUBNET_PREFIX_LEN)
                        .expect("Static configured TUN route is valid; qed"),
                };
                #[cfg(any(
                    target_os = "android",
                    target_os = "ios",
                    all(target_os = "macos", feature = "mactunfd"),
                ))]
                let tun_config = TunConfig {
                    tun_fd: config.tun_fd.unwrap(),
                };

                let (rxhalf, txhalf) = tun::new(tun_config).await?;

                info!("Node overlay IP: {node_addr}");
                DataPlane::new(router.clone(), rxhalf, txhalf, msg_sender, tun_rx)
            }
        };

        #[cfg(feature = "message")]
        let ms = MessageStack::new(_data_plane, msg_receiver);

        Ok(Node {
            router,
            peer_manager: pm,
            #[cfg(feature = "message")]
            message_stack: ms,
        })
    }

    /// Get information about the running `Node`
    pub fn info(&self) -> NodeInfo {
        NodeInfo {
            node_subnet: self.router.node_tun_subnet(),
            node_pubkey: self.router.node_public_key(),
        }
    }

    /// Get information about the current peers in the `Node`
    pub fn peer_info(&self) -> Vec<PeerStats> {
        self.peer_manager.peers()
    }

    /// Add a new peer to the system identified by an [`Endpoint`].
    pub fn add_peer(&self, endpoint: Endpoint) -> Result<(), PeerExists> {
        self.peer_manager.add_peer(endpoint)
    }

    /// Remove an existing peer identified by an [`Endpoint`] from the system.
    pub fn remove_peer(&self, endpoint: Endpoint) -> Result<(), PeerNotFound> {
        self.peer_manager.delete_peer(&endpoint)
    }

    /// List all selected [`routes`](RouteEntry) in the system.
    pub fn selected_routes(&self) -> Vec<RouteEntry> {
        self.router.load_selected_routes()
    }

    /// List all fallback [`routes`](RouteEntry) in the system.
    pub fn fallback_routes(&self) -> Vec<RouteEntry> {
        self.router.load_fallback_routes()
    }

    /// Get public key from the IP of `Node`
    pub fn get_pubkey_from_ip(&self, ip: IpAddr) -> Option<crypto::PublicKey> {
        self.router.get_pubkey(ip)
    }
}

#[cfg(feature = "message")]
impl<M> Node<M>
where
    M: Metrics + Clone + Send + 'static,
{
    /// Wait for a messsage to arrive in the message stack.
    ///
    /// An the optional `topic` is provided, only messages which have exactly the same value in
    /// `topic` will be returned. The `pop` argument decides if the message is removed from the
    /// internal queue or not. If `pop` is `false`, the same message will be returned on the next
    /// call (with the same topic).
    ///
    /// This method returns a future which will wait indefinitely until a message is received. It
    /// is generally a good idea to put a limit on how long to wait by wrapping this in a [`tokio::time::timeout`].
    pub fn get_message(
        &self,
        pop: bool,
        topic: Option<Vec<u8>>,
    ) -> impl Future<Output = ReceivedMessage> + '_ {
        // First reborrow only the message stack from self, then manually construct a future. This
        // avoids a lifetime issue on the router, which is not sync. If a regular 'async' fn would
        // be used here, we can't specify that at this point sadly.
        let ms = &self.message_stack;
        async move { ms.message(pop, topic).await }
    }

    /// Push a new message to the message stack.
    ///
    /// The system will attempt to transmit the message for `try_duration`. A message is considered
    /// transmitted when the receiver has indicated it completely received the message. If
    /// `subscribe_reply` is `true`, the second return value will be [`Option::Some`], with a
    /// watcher which will resolve if a reply for this exact message comes in. Since this relies on
    /// the receiver actually sending a reply, ther is no guarantee that this will eventually
    /// resolve.
    pub fn push_message(
        &self,
        dst: IpAddr,
        data: Vec<u8>,
        topic: Option<Vec<u8>>,
        try_duration: Duration,
        subscribe_reply: bool,
    ) -> Result<MessagePushResponse, PushMessageError> {
        self.message_stack.new_message(
            dst,
            data,
            topic.unwrap_or_default(),
            try_duration,
            subscribe_reply,
        )
    }

    /// Get the status of a message sent previously.
    ///
    /// Returns [`Option::None`] if no message is found with the given id. Message info is only
    /// retained for a limited time after a message has been received, or after the message has
    /// been aborted due to a timeout.
    pub fn message_status(&self, id: MessageId) -> Option<MessageInfo> {
        self.message_stack.message_info(id)
    }

    /// Send a reply to a previously received message.
    pub fn reply_message(
        &self,
        id: MessageId,
        dst: IpAddr,
        data: Vec<u8>,
        try_duration: Duration,
    ) -> MessageId {
        self.message_stack
            .reply_message(id, dst, data, try_duration)
    }
}
