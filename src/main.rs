use clap::{Parser, Subcommand};
use crypto::PublicKey;
use log::LevelFilter;
use mycelium::endpoint::Endpoint;
use mycelium::{crypto, Stack};
use std::net::Ipv4Addr;
use std::{
    error::Error,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};
#[cfg(target_family = "unix")]
use tokio::signal::{self, unix::SignalKind};

mod cli;

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

#[derive(Parser)]
#[command(version)]
struct Cli {
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
    /// Path to the private key file. This will be created if it does not exist. Default
    /// [priv_key.bin].
    #[arg(short = 'k', long = "key-file", global = true)]
    key_file: Option<PathBuf>,

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

    /// Enable debug logging
    #[arg(short = 'd', long = "debug", default_value_t = false)]
    debug: bool,

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
        /// Optional topic of the message. Receivers can filter on this to only recieve messages
        /// for a chosen topic.
        #[arg(short = 't', long = "topic")]
        topic: Option<String>,
        /// Optional file to use as messge body.
        #[arg(long = "msg-path")]
        msg_path: Option<PathBuf>,
        /// Optional message ID to reply to.
        #[arg(long = "reply-to")]
        reply_to: Option<String>,
        /// Destination of the message, either a hex encoded public key, or an IPv6 address in the
        /// 200::/7 range.
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    pretty_env_logger::formatted_timed_builder()
        .filter_module(
            "mycelium",
            if cli.debug {
                LevelFilter::Debug
            } else {
                LevelFilter::Info
            },
        )
        .init();

    let key_path = if let Some(path) = cli.key_file {
        path
    } else {
        PathBuf::from(DEFAULT_KEY_FILE)
    };

    // Load the keypair for this node, or generate a new one if the file does not exist.
    let node_secret_key = if key_path.exists() {
        crypto::SecretKey::load_file(&key_path).await?
    } else {
        let secret_key = crypto::SecretKey::new();
        secret_key.save_file(&key_path).await?;
        secret_key
    };
    let node_pub_key = crypto::PublicKey::from(&node_secret_key);

    if let Some(cmd) = cli.command {
        match cmd {
            Command::Inspect { json, key } => {
                let key = if let Some(key) = key {
                    PublicKey::try_from(key.as_str())?
                } else {
                    node_pub_key
                };
                cli::inspect(key, json)?;

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
                    return cli::send_msg(
                        destination,
                        message,
                        wait,
                        timeout,
                        reply_to,
                        topic,
                        msg_path,
                        cli.api_addr,
                    )
                    .await
                }
                MessageCommand::Receive {
                    timeout,
                    topic,
                    msg_path,
                    raw,
                } => return cli::recv_msg(timeout, topic, msg_path, raw, cli.api_addr).await,
            },
        }
    }

    let config = mycelium::Config {
        node_key: node_secret_key,
        peers: cli.static_peers,
        no_tun: cli.no_tun,
        tcp_listen_port: cli.tcp_listen_port,
        quic_listen_port: cli.quic_listen_port,
        peer_discovery_port: if cli.disable_peer_discovery {
            None
        } else {
            Some(cli.peer_discovery_port)
        },
        tun_name: cli.tun_name,
        api_addr: cli.api_addr,
    };

    // We set up the stack twice to avoid an unused variable warning

    // TODO: put in dedicated file so we can only rely on certain signals on unix platforms
    #[cfg(target_family = "unix")]
    {
        let stack = Stack::new(config).await?;
        let mut sigusr1 =
            signal::unix::signal(SignalKind::user_defined1()).expect("Can install SIGUSR1 handler");
        let mut sigint =
            signal::unix::signal(SignalKind::interrupt()).expect("Can install SIGINT handler");
        let mut sigterm =
            signal::unix::signal(SignalKind::terminate()).expect("Can install SIGTERM handler");

        // print info on SIGUSR1
        tokio::spawn(async move {
            while let Some(()) = sigusr1.recv().await {
                stack.dump();
            }
        });

        tokio::select! {
            _ = sigint.recv() => { }
            _ = sigterm.recv() => { }
        }
    }
    #[cfg(not(target_family = "unix"))]
    {
        let _ = Stack::new(config).await?;
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("Failed to wait for SIGINT: {e}");
        }
    }

    Ok(())
}
