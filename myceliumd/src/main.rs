use std::io;
use std::net::Ipv4Addr;
use std::path::Path;
use std::{
    error::Error,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};
use std::{fmt::Display, str::FromStr};

use clap::{Args, Parser, Subcommand};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(target_family = "unix")]
use tokio::signal::{self, unix::SignalKind};
use tracing::{debug, error, warn};

use crypto::PublicKey;
use mycelium::endpoint::Endpoint;
use mycelium::{crypto, Node};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// The default port on the underlay to listen on for incoming TCP connections.
const DEFAULT_TCP_LISTEN_PORT: u16 = 9651;
/// The default port on the underlay to listen on for incoming Quic connections.
const DEFAULT_QUIC_LISTEN_PORT: u16 = 9651;
/// The default port to use for IPv6 link local peer discovery (UDP).
const DEFAULT_PEER_DISCOVERY_PORT: u16 = 9650;
/// The default listening address for the HTTP API.
const DEFAULT_HTTP_API_SERVER_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8989);

const DEFAULT_KEY_FILE: &str = "priv_key.bin";

/// Default name of tun interface
#[cfg(not(target_os = "macos"))]
const TUN_NAME: &str = "tun0";
/// Default name of tun interface
#[cfg(target_os = "macos")]
const TUN_NAME: &str = "utun3";

/// The logging formats that can be selected.
#[derive(Clone, PartialEq, Eq)]
enum LoggingFormat {
    Compact,
    Logfmt,
}

impl Display for LoggingFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                LoggingFormat::Compact => "compact",
                LoggingFormat::Logfmt => "logfmt",
            }
        )
    }
}

impl FromStr for LoggingFormat {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "compact" => LoggingFormat::Compact,
            "logfmt" => LoggingFormat::Logfmt,
            _ => return Err("invalid logging format"),
        })
    }
}

#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Path to the private key file. This will be created if it does not exist. Default
    /// [priv_key.bin].
    #[arg(short = 'k', long = "key-file", global = true)]
    key_file: Option<PathBuf>,

    /// Enable debug logging. Does nothing if `--silent` is set.
    #[arg(short = 'd', long = "debug", default_value_t = false)]
    debug: bool,

    /// Disable all logs except error logs.
    #[arg(long = "silent", default_value_t = false)]
    silent: bool,

    /// The logging format to use. `logfmt` and `compact` is supported.
    #[arg(long = "log-format", default_value_t = LoggingFormat::Compact)]
    logging_format: LoggingFormat,

    #[clap(flatten)]
    node_args: NodeArguments,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Inspect a public key provided in hex format, or export the local public key if no key is
    /// given.
    Inspect {
        /// Output in json format.
        #[arg(long = "json")]
        json: bool,

        /// The key to inspect.
        key: Option<String>,
    },

    /// Actions on the message subsystem
    Message {
        #[command(subcommand)]
        command: MessageCommand,
    },

    /// Actions related to peers (list, remove, add)
    Peers {
        #[command(subcommand)]
        command: PeersCommand,
    },

    /// Actions related to routes (selected, fallback)
    Routes {
        #[command(subcommand)]
        command: RoutesCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum MessageCommand {
    Send {
        /// Wait for a reply from the receiver.
        #[arg(short = 'w', long = "wait", default_value_t = false)]
        wait: bool,
        /// An optional timeout to wait for. This does nothing if the `--wait` flag is not set. If
        /// `--wait` is set and this flag isn't, wait forever for a reply.
        #[arg(long = "timeout")]
        timeout: Option<u64>,
        /// Optional topic of the message. Receivers can filter on this to only receive messages
        /// for a chosen topic.
        #[arg(short = 't', long = "topic")]
        topic: Option<String>,
        /// Optional file to use as message body.
        #[arg(long = "msg-path")]
        msg_path: Option<PathBuf>,
        /// Optional message ID to reply to.
        #[arg(long = "reply-to")]
        reply_to: Option<String>,
        /// Destination of the message, either a hex encoded public key, or an IPv6 address in the
        /// 400::/7 range.
        destination: String,
        /// The message to send. This is required if `--msg_path` is not set
        message: Option<String>,
    },
    Receive {
        /// An optional timeout to wait for a message. If this is not set, wait forever.
        #[arg(long = "timeout")]
        timeout: Option<u64>,
        /// Optional topic of the message. Only messages with this topic will be received by this
        /// command.
        #[arg(short = 't', long = "topic")]
        topic: Option<String>,
        /// Optional file in which the message body will be saved.
        #[arg(long = "msg-path")]
        msg_path: Option<PathBuf>,
        /// Don't print the metadata
        #[arg(long = "raw")]
        raw: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum PeersCommand {
    /// List the connected peers
    List {
        /// Print the peers list in JSON format
        #[arg(long = "json", default_value_t = false)]
        json: bool,
    },
    /// Add peer(s)
    Add { peers: Vec<String> },
    /// Remove peer(s)
    Remove { peers: Vec<String> },
}

#[derive(Debug, Subcommand)]
pub enum RoutesCommand {
    /// Print all selected routes
    Selected {
        /// Print selected routes in JSON format
        #[arg(long = "json", default_value_t = false)]
        json: bool,
    },
    /// Print all fallback routes
    Fallback {
        /// Print fallback routes in JSON format
        #[arg(long = "json", default_value_t = false)]
        json: bool,
    },
}

#[derive(Debug, Args)]
pub struct NodeArguments {
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

    /// The address on which to expose prometheus metrics, if desired.
    ///
    /// Setting this flag will attempt to start an HTTP server on the provided address, to serve
    /// prometheus metrics on the /metrics endpoint. If this flag is not set, metrics are also not
    /// collected.
    #[arg(long = "metrics-api-address")]
    metrics_api_address: Option<SocketAddr>,

    /// The firewall mark to set on the mycelium sockets.
    ///
    /// This allows to identify packets that contain encapsulated mycelium packets so that
    /// different routing policies can be applied to them.
    /// This option only has an effect on Linux.
    #[arg(long = "firewall-mark")]
    firewall_mark: Option<u32>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    let level = if cli.silent {
        tracing::Level::ERROR
    } else if cli.debug {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };

    tracing_subscriber::registry()
        .with(
            EnvFilter::builder()
                .with_default_directive(level.into())
                .from_env()
                .expect("invalid RUST_LOG"),
        )
        .with(
            (cli.logging_format == LoggingFormat::Compact)
                .then(|| tracing_subscriber::fmt::Layer::new().compact()),
        )
        .with((cli.logging_format == LoggingFormat::Logfmt).then(|| tracing_logfmt::layer()))
        .init();

    let key_path = cli
        .key_file
        .unwrap_or_else(|| PathBuf::from(DEFAULT_KEY_FILE));

    match cli.command {
        None => {
            let node_keys = get_node_keys(&key_path).await?;
            let node_secret_key = if let Some((node_secret_key, _)) = node_keys {
                node_secret_key
            } else {
                warn!("Node key file {key_path:?} not found, generating new keys");
                let secret_key = crypto::SecretKey::new();
                save_key_file(&secret_key, &key_path).await?;
                secret_key
            };

            let _api = if let Some(metrics_api_addr) = cli.node_args.metrics_api_address {
                let metrics = mycelium_metrics::PrometheusExporter::new();
                let config = mycelium::Config {
                    node_key: node_secret_key,
                    peers: cli.node_args.static_peers,
                    no_tun: cli.node_args.no_tun,
                    tcp_listen_port: cli.node_args.tcp_listen_port,
                    quic_listen_port: Some(cli.node_args.quic_listen_port),
                    peer_discovery_port: if cli.node_args.disable_peer_discovery {
                        None
                    } else {
                        Some(cli.node_args.peer_discovery_port)
                    },
                    tun_name: cli.node_args.tun_name,
                    private_network_config: None,
                    metrics: metrics.clone(),
                    firewall_mark: cli.node_args.firewall_mark,
                };
                metrics.spawn(metrics_api_addr);
                let node = Node::new(config).await?;
                mycelium_api::Http::spawn(node, cli.node_args.api_addr)
            } else {
                let config = mycelium::Config {
                    node_key: node_secret_key,
                    peers: cli.node_args.static_peers,
                    no_tun: cli.node_args.no_tun,
                    tcp_listen_port: cli.node_args.tcp_listen_port,
                    quic_listen_port: Some(cli.node_args.quic_listen_port),
                    peer_discovery_port: if cli.node_args.disable_peer_discovery {
                        None
                    } else {
                        Some(cli.node_args.peer_discovery_port)
                    },
                    tun_name: cli.node_args.tun_name,
                    private_network_config: None,
                    metrics: mycelium_metrics::NoMetrics,
                    firewall_mark: cli.node_args.firewall_mark,
                };
                let node = Node::new(config).await?;
                mycelium_api::Http::spawn(node, cli.node_args.api_addr)
            };

            // TODO: put in dedicated file so we can only rely on certain signals on unix platforms
            #[cfg(target_family = "unix")]
            {
                let mut sigint = signal::unix::signal(SignalKind::interrupt())
                    .expect("Can install SIGINT handler");
                let mut sigterm = signal::unix::signal(SignalKind::terminate())
                    .expect("Can install SIGTERM handler");

                tokio::select! {
                    _ = sigint.recv() => { }
                    _ = sigterm.recv() => { }
                }
            }
            #[cfg(not(target_family = "unix"))]
            {
                if let Err(e) = tokio::signal::ctrl_c().await {
                    error!("Failed to wait for SIGINT: {e}");
                }
            }
        }
        Some(cmd) => match cmd {
            Command::Inspect { json, key } => {
                let node_keys = get_node_keys(&key_path).await?;
                let key = if let Some(key) = key {
                    PublicKey::try_from(key.as_str())?
                } else if let Some((_, node_pub_key)) = node_keys {
                    node_pub_key
                } else {
                    error!("No key to inspect provided and no key found at {key_path:?}");
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "no key to inspect and key file not found",
                    )
                    .into());
                };
                mycelium_cli::inspect(key, json)?;

                return Ok(());
            }
            Command::Message { command } => match command {
                MessageCommand::Send {
                    wait,
                    timeout,
                    topic,
                    msg_path,
                    reply_to,
                    destination,
                    message,
                } => {
                    return mycelium_cli::send_msg(
                        destination,
                        message,
                        wait,
                        timeout,
                        reply_to,
                        topic,
                        msg_path,
                        cli.node_args.api_addr,
                    )
                    .await
                }
                MessageCommand::Receive {
                    timeout,
                    topic,
                    msg_path,
                    raw,
                } => {
                    return mycelium_cli::recv_msg(
                        timeout,
                        topic,
                        msg_path,
                        raw,
                        cli.node_args.api_addr,
                    )
                    .await
                }
            },
            Command::Peers { command } => match command {
                PeersCommand::List { json } => {
                    return mycelium_cli::list_peers(cli.node_args.api_addr, json).await;
                }
                PeersCommand::Add { peers } => {
                    return mycelium_cli::add_peers(cli.node_args.api_addr, peers).await;
                }
                PeersCommand::Remove { peers } => {
                    return mycelium_cli::remove_peers(cli.node_args.api_addr, peers).await;
                }
            },
            Command::Routes { command } => match command {
                RoutesCommand::Selected { json } => {
                    return mycelium_cli::list_selected_routes(cli.node_args.api_addr, json).await;
                }
                RoutesCommand::Fallback { json } => {
                    return mycelium_cli::list_fallback_routes(cli.node_args.api_addr, json).await;
                }
            },
        },
    }

    Ok(())
}

async fn get_node_keys(
    key_path: &PathBuf,
) -> Result<Option<(crypto::SecretKey, crypto::PublicKey)>, io::Error> {
    if key_path.exists() {
        let sk = load_key_file(key_path).await?;
        let pk = crypto::PublicKey::from(&sk);
        debug!("Loaded key file at {key_path:?}");
        Ok(Some((sk, pk)))
    } else {
        Ok(None)
    }
}

async fn load_key_file<T>(path: &Path) -> Result<T, io::Error>
where
    T: From<[u8; 32]>,
{
    let mut file = File::open(path).await?;
    let mut secret_bytes = [0u8; 32];
    file.read_exact(&mut secret_bytes).await?;

    Ok(T::from(secret_bytes))
}

async fn save_key_file(key: &crypto::SecretKey, path: &Path) -> io::Result<()> {
    #[cfg(target_family = "unix")]
    {
        use tokio::fs::OpenOptions;

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600) // rw by the owner, not readable by group or others
            .open(path)
            .await?;
        file.write_all(key.as_bytes()).await?;
    }
    #[cfg(not(target_family = "unix"))]
    {
        let mut file = File::create(path).await?;
        file.write_all(key.as_bytes()).await?;
    }

    Ok(())
}
