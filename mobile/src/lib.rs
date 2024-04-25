use std::convert::TryFrom;
use std::io;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;

use clap::{Args, Parser};
use log::info;
#[cfg(target_family = "unix")]
use tokio::signal::{self, unix::SignalKind};

use metrics::Metrics;
use mycelium::endpoint::Endpoint;
use mycelium::{crypto, metrics, Config, Node};

#[cfg(target_os = "android")]
fn setup_the_logger() {
    use log::LevelFilter;
    android_logger::init_once(android_logger::Config::default().with_max_level(LevelFilter::Trace));
}
#[tokio::main]
pub async fn start_mycelium(peer: String, tun_fd: i32, priv_key: Vec<u8>) {
    let input = vec!["", "--peers", peer.as_str()];
    let cli = Cli::parse_from(input);

    #[cfg(target_os = "android")]
    setup_the_logger();

    let secret_key = build_secret_key(priv_key).await.unwrap();

    let config = Config {
        node_key: secret_key,
        peers: cli.node_args.static_peers,
        no_tun: cli.node_args.no_tun,
        tcp_listen_port: cli.node_args.tcp_listen_port,
        quic_listen_port: cli.node_args.quic_listen_port,
        peer_discovery_port: if cli.node_args.disable_peer_discovery {
            None
        } else {
            Some(cli.node_args.peer_discovery_port)
        },
        tun_name: cli.node_args.tun_name,
        metrics: NoMetrics,
        private_network_config: None,
        //api_addr: cli.node_args.api_addr,
        firewall_mark: cli.node_args.firewall_mark,
        tun_fd: Some(tun_fd),
    };
    let _node = Node::new(config).await;

    match _node {
        Ok(_) => info!("node successfully created"),
        // use info! here because error! is not printed
        Err(err) => info!("failed to create stack: {err}"),
    };

    // TODO: put in dedicated file so we can only rely on certain signals on unix platforms
    // TODO: check what is the better way in Android and iOS
    #[cfg(target_family = "unix")]
    {
        let mut sigint =
            signal::unix::signal(SignalKind::interrupt()).expect("Can install SIGINT handler");
        let mut sigterm =
            signal::unix::signal(SignalKind::terminate()).expect("Can install SIGTERM handler");

        tokio::select! {
            _ = sigint.recv() => { }
            _ = sigterm.recv() => { }
        }
    }
    #[cfg(not(target_family = "unix"))]
    {
        if let Err(e) = tokio::signal::ctrl_c().await {
            log::error!("Failed to wait for SIGINT: {e}");
        }
    }
}

#[derive(Clone)]
pub struct NoMetrics;
impl Metrics for NoMetrics {}

/// The default port on the underlay to listen on for incoming TCP connections.
const DEFAULT_TCP_LISTEN_PORT: u16 = 9651;
/// The default port on the underlay to listen on for incoming Quic connections.
const DEFAULT_QUIC_LISTEN_PORT: u16 = 9651;
/// The default port to use for IPv6 link local peer discovery (UDP).
const DEFAULT_PEER_DISCOVERY_PORT: u16 = 9650;
/// The default listening address for the HTTP API.
const DEFAULT_HTTP_API_SERVER_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8989);

/// Default name of tun interface
const TUN_NAME: &str = "tun0";

#[derive(Debug, Args)]
struct NodeArguments {
    /// Peers to connect to.
    #[arg(long = "peers", num_args = 1..)]
    static_peers: Vec<Endpoint>,

    /// Port to listen on for tcp connections.
    #[arg(short = 't', long = "tcp-listen-port", default_value_t = DEFAULT_TCP_LISTEN_PORT)]
    tcp_listen_port: u16,

    /// Port to listen on for quic connections.
    #[arg(short = 'q', long = "quic-listen-port", default_value_t = DEFAULT_QUIC_LISTEN_PORT)]
    quic_listen_port: u16,

    /// Port to use for link local peer discovery. This uses the UDP protocol.
    #[arg(long = "peer-discovery-port", default_value_t = DEFAULT_PEER_DISCOVERY_PORT)]
    peer_discovery_port: u16,

    /// Disable peer discovery.
    ///
    /// If this flag is passed, the automatic link local peer discovery will not be enabled, and
    /// peers must be configured manually. If this is disabled on all local peers, communication
    /// between them will go over configured external peers.
    #[arg(long = "disable-peer-discovery", default_value_t = false)]
    disable_peer_discovery: bool,

    /// Address of the HTTP API server.
    #[arg(long = "api-addr", default_value_t = DEFAULT_HTTP_API_SERVER_ADDRESS)]
    api_addr: SocketAddr,

    /// Run without creating a TUN interface.
    ///
    /// The system will participate in the network as usual, but won't be able to send out L3
    /// packets. Inbound L3 traffic will be silently discarded. The message subsystem will still
    /// work however.
    #[arg(long = "no-tun", default_value_t = false)]
    no_tun: bool,

    /// Name to use for the TUN interface, if one is created.
    ///
    /// Setting this only matters if a TUN interface is actually created, i.e. if the `--no-tun`
    /// flag is **not** set. The name set here must be valid for the current platform, e.g. on OSX,
    /// the name must start with `utun` and be followed by digits.
    #[arg(long = "tun-name", default_value = TUN_NAME)]
    tun_name: String,

    /// The firewall mark to set on the mycelium sockets.
    ///
    /// This allows to identify packets that contain encapsulated mycelium packets so that
    /// different routing policies can be applied to them.
    /// This option only has an effect on Linux.
    #[arg(long = "firewall-mark")]
    firewall_mark: Option<u32>,
}
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Path to the private key file. This will be created if it does not exist. Default
    /// [priv_key.bin].
    //#[arg(short = 'k', long = "key-file", global = true)]
    //key_file: Option<PathBuf>,

    /// Enable debug logging. Does nothing if `--silent` is set.
    #[arg(short = 'd', long = "debug", default_value_t = false)]
    debug: bool,

    /// Disable all logs except error logs.
    #[arg(long = "silent", default_value_t = false)]
    silent: bool,

    #[clap(flatten)]
    node_args: NodeArguments,
}

fn convert_slice_to_array32(slice: &[u8]) -> Result<[u8; 32], std::array::TryFromSliceError> {
    <[u8; 32]>::try_from(slice)
}
async fn build_secret_key<T>(bin: Vec<u8>) -> Result<T, io::Error>
where
    T: From<[u8; 32]>,
{
    Ok(T::from(convert_slice_to_array32(bin.as_slice()).unwrap()))
}

// generate secret key
// it is used by android & ios app
pub fn generate_secret_key() -> Vec<u8> {
    crypto::SecretKey::new().as_bytes().into()
}

pub fn address_from_secret_key(data: Vec<u8>) -> String {
    let data = convert_slice_to_array32(data.as_slice()).unwrap();
    let secret_key = crypto::SecretKey::from(data);
    crypto::PublicKey::from(&secret_key).address().to_string()
}
