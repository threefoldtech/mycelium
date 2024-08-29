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
use serde::{Deserialize, Deserializer};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(target_family = "unix")]
use tokio::signal::{self, unix::SignalKind};
use tracing::{debug, error, info, warn};

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
const TUN_NAME: &str = "mycelium";
/// Default name of tun interface
#[cfg(target_os = "macos")]
const TUN_NAME: &str = "utun0";

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

    // Configuration file
    #[arg(short = 'c', long = "config-file", global = true)]
    config_file: Option<PathBuf>,

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

    /// Disable quic protocol for connecting to peers
    #[arg(long = "disable-quic", default_value_t = false)]
    disable_quic: bool,

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
    #[arg(long = "tun-name")]
    tun_name: Option<String>,

    /// Enable a private network, with this name.
    ///
    /// If this flag is set, the system will run in "private network mode", and use Tls connections
    /// instead of plain Tcp connections. The name provided here is used as the network name, other
    /// nodes must use the same name or the connection will be rejected. Note that the name is
    /// public, and is communicated when connecting to a remote. Do not put confidential data here.
    #[arg(long = "network-name", requires = "network_key_file")]
    network_name: Option<String>,

    /// The path to the file with the key to use for the private network.
    ///
    /// The key is expected to be exactly 32 bytes. The key must be shared between all nodes
    /// participating in the network, and is secret. If the key leaks, anyone can then join the
    /// network.
    #[arg(long = "network-key-file", requires = "network_name")]
    network_key_file: Option<PathBuf>,

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

    /// The amount of worker tasks to spawn to handle updates.
    ///
    /// By default, updates are processed on a single task only. This is sufficient for most use
    /// cases. In case you notice that the node can't keep up with the incoming updates (typically
    /// because you are running a public node with a lot of connections), this value can be
    /// increased to process updates in parallel.
    #[arg(long = "update-workers", default_value_t = 1)]
    update_workers: usize,
}

#[derive(Debug, Deserialize)]
pub struct MergedNodeConfig {
    peers: Vec<Endpoint>,
    tcp_listen_port: u16,
    disable_quic: bool,
    quic_listen_port: u16,
    peer_discovery_port: u16,
    disable_peer_discovery: bool,
    api_addr: SocketAddr,
    no_tun: bool,
    tun_name: String,
    metrics_api_address: Option<SocketAddr>,
    network_key_file: Option<PathBuf>,
    network_name: Option<String>,
    firewall_mark: Option<u32>,
    update_workers: usize,
}

#[derive(Debug, Deserialize, Default)]
struct MyceliumConfig {
    #[serde(deserialize_with = "deserialize_optional_endpoint_str_from_toml")]
    peers: Option<Vec<Endpoint>>,
    tcp_listen_port: Option<u16>,
    disable_quic: Option<bool>,
    quic_listen_port: Option<u16>,
    no_tun: Option<bool>,
    tun_name: Option<String>,
    disable_peer_discovery: Option<bool>,
    peer_discovery_port: Option<u16>,
    api_addr: Option<SocketAddr>,
    metrics_api_address: Option<SocketAddr>,
    network_name: Option<String>,
    network_key_file: Option<PathBuf>,
    firewall_mark: Option<u32>,
    update_workers: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    // Init default configuration
    let mut mycelium_config = MyceliumConfig::default();

    // Load configuration file
    if let Some(config_file_path) = &cli.config_file {
        if Path::new(config_file_path).exists() {
            let config = config::Config::builder()
                .add_source(config::File::new(
                    config_file_path.to_str().unwrap(),
                    config::FileFormat::Toml,
                ))
                .build()?;

            mycelium_config = config.try_deserialize()?;
        } else {
            let error_msg = format!("Config file {:?} not found", config_file_path);
            return Err(io::Error::new(io::ErrorKind::NotFound, error_msg).into());
        }
    } else if let Some(mut conf) = dirs::config_dir() {
        // Windows: %APPDATA%/ThreeFold Tech/Mycelium/mycelium.conf
        #[cfg(target_os = "windows")]
        {
            conf = conf
                .join("ThreeFold Tech")
                .join("Mycelium")
                .join("mycelium.toml")
        };
        // Linux: $HOME/.config/mycelium/mycelium.conf
        #[cfg(target_os = "linux")]
        {
            conf = conf.join("mycelium").join("mycelium.toml")
        };
        // MacOS: $HOME/Library/Application Support/ThreeFold Tech/Mycelium/mycelium.conf
        #[cfg(target_os = "macos")]
        {
            conf = conf
                .join("ThreeFold Tech")
                .join("Mycelium")
                .join("mycelium.toml")
        };

        if conf.exists() {
            info!(
                conf_dir = conf.to_str().unwrap(),
                "Mycelium is starting with configuration file",
            );
            let config = config::Config::builder()
                .add_source(config::File::new(
                    conf.to_str().unwrap(),
                    config::FileFormat::Toml,
                ))
                .build()?;
            mycelium_config = config.try_deserialize()?;
        }
    }

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
            let merged_config = merge_config(cli.node_args, mycelium_config);

            let private_network_config =
                match (merged_config.network_name, merged_config.network_key_file) {
                    (Some(network_name), Some(network_key_file)) => {
                        let net_key = load_key_file(&network_key_file).await?;

                        Some((network_name, net_key))
                    }
                    _ => None,
                };

            let node_keys = get_node_keys(&key_path).await?;
            let node_secret_key = if let Some((node_secret_key, _)) = node_keys {
                node_secret_key
            } else {
                warn!("Node key file {key_path:?} not found, generating new keys");
                let secret_key = crypto::SecretKey::new();
                save_key_file(&secret_key, &key_path).await?;
                secret_key
            };

            let _api = if let Some(metrics_api_addr) = merged_config.metrics_api_address {
                let metrics = mycelium_metrics::PrometheusExporter::new();
                let config = mycelium::Config {
                    node_key: node_secret_key,
                    peers: merged_config.peers,
                    no_tun: merged_config.no_tun,
                    tcp_listen_port: merged_config.tcp_listen_port,
                    quic_listen_port: if merged_config.disable_quic {
                        None
                    } else {
                        Some(merged_config.quic_listen_port)
                    },
                    peer_discovery_port: if merged_config.disable_peer_discovery {
                        None
                    } else {
                        Some(merged_config.peer_discovery_port)
                    },
                    tun_name: merged_config.tun_name,
                    private_network_config,
                    metrics: metrics.clone(),
                    firewall_mark: merged_config.firewall_mark,
                    update_workers: merged_config.update_workers,
                };
                metrics.spawn(metrics_api_addr);
                let node = Node::new(config).await?;
                mycelium_api::Http::spawn(node, merged_config.api_addr)
            } else {
                let config = mycelium::Config {
                    node_key: node_secret_key,
                    peers: merged_config.peers,
                    no_tun: merged_config.no_tun,
                    tcp_listen_port: merged_config.tcp_listen_port,
                    quic_listen_port: if merged_config.disable_quic {
                        None
                    } else {
                        Some(merged_config.quic_listen_port)
                    },
                    peer_discovery_port: if merged_config.disable_peer_discovery {
                        None
                    } else {
                        Some(merged_config.peer_discovery_port)
                    },
                    tun_name: merged_config.tun_name,
                    private_network_config,
                    metrics: mycelium_metrics::NoMetrics,
                    firewall_mark: merged_config.firewall_mark,
                    update_workers: merged_config.update_workers,
                };
                let node = Node::new(config).await?;
                mycelium_api::Http::spawn(node, merged_config.api_addr)
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

fn merge_config(cli_args: NodeArguments, file_config: MyceliumConfig) -> MergedNodeConfig {
    MergedNodeConfig {
        peers: if !cli_args.static_peers.is_empty() {
            cli_args.static_peers
        } else {
            file_config.peers.unwrap_or_default()
        },
        tcp_listen_port: if cli_args.tcp_listen_port != DEFAULT_TCP_LISTEN_PORT {
            cli_args.tcp_listen_port
        } else {
            file_config
                .tcp_listen_port
                .unwrap_or(DEFAULT_TCP_LISTEN_PORT)
        },
        disable_quic: cli_args.disable_quic || file_config.disable_quic.unwrap_or(false),
        quic_listen_port: if cli_args.quic_listen_port != DEFAULT_QUIC_LISTEN_PORT {
            cli_args.quic_listen_port
        } else {
            file_config
                .quic_listen_port
                .unwrap_or(DEFAULT_QUIC_LISTEN_PORT)
        },
        peer_discovery_port: if cli_args.peer_discovery_port != DEFAULT_PEER_DISCOVERY_PORT {
            cli_args.peer_discovery_port
        } else {
            file_config
                .peer_discovery_port
                .unwrap_or(DEFAULT_PEER_DISCOVERY_PORT)
        },
        disable_peer_discovery: cli_args.disable_peer_discovery
            || file_config.disable_peer_discovery.unwrap_or(false),
        api_addr: if cli_args.api_addr != DEFAULT_HTTP_API_SERVER_ADDRESS {
            cli_args.api_addr
        } else {
            file_config
                .api_addr
                .unwrap_or(DEFAULT_HTTP_API_SERVER_ADDRESS)
        },
        no_tun: cli_args.no_tun || file_config.no_tun.unwrap_or(false),
        tun_name: if let Some(tun_name_cli) = cli_args.tun_name {
            tun_name_cli
        } else if let Some(tun_name_config) = file_config.tun_name {
            tun_name_config
        } else {
            TUN_NAME.to_string()
        },
        metrics_api_address: cli_args
            .metrics_api_address
            .or(file_config.metrics_api_address),
        network_name: cli_args.network_name.or(file_config.network_name),
        network_key_file: cli_args.network_key_file.or(file_config.network_key_file),
        firewall_mark: cli_args.firewall_mark.or(file_config.firewall_mark),
        update_workers: if cli_args.update_workers != 1 {
            cli_args.update_workers
        } else {
            file_config.update_workers.unwrap_or(1)
        },
    }
}

/// Deserialize an optional list of endpoints from TOML format. The endpoints can be provided
/// either as a list `[...]`, or in case there is only 1 endpoint, it can also be provided as a
/// single string element. If no value is provided, it returns None.
fn deserialize_optional_endpoint_str_from_toml<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<Endpoint>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        String(String),
        Vec(Vec<String>),
    }

    Ok(match Option::<StringOrVec>::deserialize(deserializer)? {
        Some(StringOrVec::Vec(v)) => Some(
            v.into_iter()
                .map(|s| {
                    <Endpoint as std::str::FromStr>::from_str(&s).map_err(serde::de::Error::custom)
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Some(StringOrVec::String(s)) => Some(vec![
            <Endpoint as std::str::FromStr>::from_str(&s).map_err(serde::de::Error::custom)?
        ]),
        None => None,
    })
}
