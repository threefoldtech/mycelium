use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use api::Http;
use bytes::BytesMut;
use data::DataPlane;
use endpoint::Endpoint;
use log::{error, info, warn};
use message::MessageStack;
use router::StaticRoute;
use subnet::Subnet;

pub mod api;
mod babel;
mod connection;
pub mod crypto;
pub mod data;
pub mod endpoint;
pub mod filters;
mod interval;
mod ip_pubkey;
pub mod message;
mod metric;
pub mod packet;
mod peer;
pub mod peer_manager;
pub mod router;
mod router_id;
mod routing_table;
mod sequence_number;
mod source_table;
pub mod subnet;
mod tun;

/// The prefix of the global subnet used.
pub const GLOBAL_SUBNET_ADDRESS: IpAddr = IpAddr::V6(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 0));
/// The prefix length of the global subnet used.
pub const GLOBAL_SUBNET_PREFIX_LEN: u8 = 7;

/// Config for a mycelium [`Stack`].
pub struct Config {
    /// The secret key of the node.
    pub node_key: crypto::SecretKey,
    /// Statically configured peers.
    pub peers: Vec<Endpoint>,
    /// Tun interface should be disabled.
    pub no_tun: bool,
    /// Listen port for TCP connections.
    pub tcp_listen_port: u16,
    /// Listen port for Quic connections.
    pub quic_listen_port: u16,
    /// Udp port for peer discovery.
    pub peer_discovery_port: Option<u16>,
    /// Name for the TUN device.
    pub tun_name: String,
    /// IP and port for the api address.
    pub api_addr: SocketAddr,
}

/// The Stack is the main structure in mycelium. It governs the entire data flow.
pub struct Stack {
    _router: router::Router,
    _pm: peer_manager::PeerManager,
    _ms: message::MessageStack,
    _api: api::Http,
}

impl Stack {
    /// Setup a new `Stack` with the provided [`Config`].
    pub async fn new(config: Config) -> Result<Self, Box<dyn std::error::Error>> {
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
            tun_tx,
            node_subnet,
            vec![StaticRoute::new(node_subnet)],
            (config.node_key, node_pub_key),
            vec![
                Box::new(filters::AllowedSubnet::new(
                    Subnet::new(GLOBAL_SUBNET_ADDRESS, GLOBAL_SUBNET_PREFIX_LEN)
                        .expect("Global subnet is properly defined; qed"),
                )),
                Box::new(filters::MaxSubnetSize::<64>),
                Box::new(filters::RouterIdOwnsSubnet),
            ],
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
            if let Some(port) = config.peer_discovery_port {
                port
            } else {
                0
            },
            config.peer_discovery_port.is_none(),
        )?;
        info!("Started peer manager");

        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let msg_receiver = tokio_stream::wrappers::ReceiverStream::new(rx);
        let msg_sender = tokio_util::sync::PollSender::new(tx);

        let data_plane = if config.no_tun {
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
            #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
            {
                panic!("On this platform, you can only run with --no-tun");
            }
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
            {
                let (rxhalf, txhalf) = tun::new(
                    &config.tun_name,
                    Subnet::new(node_addr.into(), 64)
                        .expect("64 is a valid subnet size for IPv6; qed"),
                    Subnet::new(GLOBAL_SUBNET_ADDRESS, GLOBAL_SUBNET_PREFIX_LEN)
                        .expect("Static configured TUN route is valid; qed"),
                )
                .await?;
                info!("Node overlay IP: {node_addr}");
                DataPlane::new(router.clone(), rxhalf, txhalf, msg_sender, tun_rx)
            }
        };

        let ms = MessageStack::new(data_plane, msg_receiver);

        let api = Http::spawn(router.clone(), pm.clone(), ms.clone(), &config.api_addr);

        Ok(Stack {
            _router: router,
            _pm: pm,
            _ms: ms,
            _api: api,
        })
    }

    #[cfg(target_family = "unix")]
    /// Dump internal state, temporary method.
    pub fn dump(&self) {
        println!("----------- Current selected routes -----------\n");
        self._router.print_selected_routes();
        println!("----------- Current fallback routes -----------\n");
        self._router.print_fallback_routes();
        println!("----------- Current source table -----------\n");
        self._router.print_source_table();
        println!("----------- Subnet origins -----------\n");
        self._router.print_subnet_origins();

        println!("\n----------- Current peers: -----------");
        for p in self._router.peer_interfaces() {
            println!(
                "Peer: {}, with link cost: {} (Read: {} / Written: {})",
                p.connection_identifier(),
                p.link_cost(),
                format_bytes(p.read()),
                format_bytes(p.written()),
            );
        }

        fn format_bytes(bytes: u64) -> String {
            /// 1 Tebibyte
            const TI_B: u64 = 1 << 40;
            /// 1 Gibibyte
            const GI_B: u64 = 1 << 30;
            /// 1 Mebibyte
            const MI_B: u64 = 1 << 20;
            /// 1 Kibibyte
            const KI_B: u64 = 1 << 10;
            match bytes {
                x if x > TI_B => format!("{}.{} TiB", x / TI_B, (x % TI_B) / (TI_B / 100)),
                x if x > GI_B => format!("{}.{} GiB", x / GI_B, (x % GI_B) / (GI_B / 100)),
                x if x > MI_B => format!("{}.{} MiB", x / MI_B, (x % MI_B) / (MI_B / 100)),
                x if x > KI_B => format!("{}.{} KiB", x / KI_B, (x % KI_B) / (KI_B / 100)),
                x => format!("{x} B"),
            }
        }

        println!("\n\n");
    }
}
