use base64::engine::{GeneralPurpose, GeneralPurposeConfig};
use base64::{alphabet, Engine};
use bytes::BytesMut;
use clap::{Parser, Subcommand};
use crypto::PublicKey;
use log::{debug, error, info};
use log::{warn, LevelFilter};
use mycelium::api::{
    Http, MessageDestination, MessageReceiveInfo, MessageSendInfo, PushMessageResponse,
};
use mycelium::crypto;
use mycelium::data::DataPlane;
use mycelium::filters;
use mycelium::message::{MessageId, MessageStack};
use mycelium::peer_manager;
use mycelium::router;
use mycelium::router::StaticRoute;
use mycelium::subnet::Subnet;
use serde::{Serialize, Serializer};
use std::io::Write;
use std::mem;
use std::net::Ipv4Addr;
use std::{
    error::Error,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    path::PathBuf,
};
#[cfg(target_family = "unix")]
use tokio::signal::{self, unix::SignalKind};
mod tun;

/// The default port on the inderlay to listen on.
const DEFAULT_LISTEN_PORT: u16 = 9651;
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
/// Global route of overlay network
const TUN_ROUTE_DEST: Ipv6Addr = Ipv6Addr::new(0x200, 0, 0, 0, 0, 0, 0, 0);
/// Global route prefix of overlay network
const TUN_ROUTE_PREFIX: u8 = 7;

/// The prefix of the global subnet used.
const GLOBAL_SUBNET_ADDRESS: IpAddr = IpAddr::V6(Ipv6Addr::new(0x200, 0, 0, 0, 0, 0, 0, 0));
/// THe prefix lenght of the global subnet used.
const GLOBAL_SUBNET_PREFIX_LEN: u8 = 7;

#[derive(Parser)]
struct Cli {
    /// Peers to connect to.
    #[arg(long = "peers", num_args = 1..)]
    static_peers: Vec<SocketAddr>,

    /// Port to listen on.
    #[arg(short = 'p', long = "port", default_value_t = DEFAULT_LISTEN_PORT)]
    port: u16,

    /// Path to the private key file. This will be created if it does not exist. Default
    /// [priv_key.bin].
    #[arg(short = 'k', long = "key-file")]
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
    let node_keypair = (node_secret_key, node_pub_key);

    if let Some(cmd) = cli.command {
        match cmd {
            Command::Inspect { json, key } => {
                let key = if let Some(key) = key {
                    PublicKey::try_from(key.as_str())?
                } else {
                    node_pub_key
                };
                inspect(key, json)?;

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
                    return send_msg(
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
                } => return recv_msg(timeout, topic, msg_path, raw, cli.api_addr).await,
            },
        }
    }

    // Generate the node's IPv6 address from its public key
    let node_addr = node_pub_key.address();
    info!("Node address: {}", node_addr);

    debug!("Node public key: {:?}", node_keypair.1);

    let static_peers = cli.static_peers;

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
        node_keypair.clone(),
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
    let _peer_manager: peer_manager::PeerManager =
        peer_manager::PeerManager::new(router.clone(), static_peers, cli.port);

    let (tx, rx) = tokio::sync::mpsc::channel(100);
    let msg_receiver = tokio_stream::wrappers::ReceiverStream::new(rx);
    let msg_sender = tokio_util::sync::PollSender::new(tx);

    let data_plane = if cli.no_tun {
        warn!("Starting data plane witout TUN interface, L3 functionality disabled");
        DataPlane::new(
            router.clone(),
            // No tun so create a dummy stream for l3 packets which never yields
            tokio_stream::pending(),
            // Similarly, create a sink which just discards every packet we would receive
            futures::sink::drain(),
            msg_sender,
            tun_rx,
        )
    } else {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            panic!("On this platform, you can only run with --no-tun");
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let (rxhalf, txhalf) = tun::new(
                &cli.tun_name,
                Subnet::new(node_addr.into(), 64).expect("64 is a valid subnet size for IPv6; qed"),
                Subnet::new(TUN_ROUTE_DEST.into(), TUN_ROUTE_PREFIX)
                    .expect("Static configured TUN route is valid; qed"),
            )
            .await?;
            DataPlane::new(router.clone(), rxhalf, txhalf, msg_sender, tun_rx)
        }
    };

    let ms = MessageStack::new(data_plane, msg_receiver);

    let _api = Http::spawn(ms, &cli.api_addr);

    // TODO: put in dedicated file so we can only rely on certain signals on unix platforms
    #[cfg(target_family = "unix")]
    {
        let mut sigusr1 =
            signal::unix::signal(SignalKind::user_defined1()).expect("Can install SIGUSR1 handler");
        let mut sigint =
            signal::unix::signal(SignalKind::interrupt()).expect("Can install SIGINT handler");
        let mut sigterm =
            signal::unix::signal(SignalKind::terminate()).expect("Can install SIGTERM handler");

        // print info on SIGUSR1
        tokio::spawn(async move {
            while let Some(()) = sigusr1.recv().await {
                println!("----------- Current selected routes -----------\n");
                router.print_selected_routes();
                println!("----------- Current fallback routes -----------\n");
                router.print_fallback_routes();
                println!("----------- Current source table -----------\n");
                router.print_source_table();
                println!("----------- Subnet origins -----------\n");
                router.print_subnet_origins();

                println!("\n----------- Current peers: -----------");
                for p in router.peer_interfaces() {
                    println!(
                        "Peer: {}, with link cost: {}",
                        p.underlay_ip(),
                        p.link_cost()
                    );
                }

                println!("\n\n");
            }
        });

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

    Ok(())
}

#[derive(Debug, Serialize)]
struct InspectOutput {
    #[serde(rename = "publicKey")]
    public_key: PublicKey,
    address: IpAddr,
}

/// Inspect the given pubkey, or the local key if no pubkey is given
fn inspect(pubkey: PublicKey, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let address = pubkey.address().into();
    if json {
        let out = InspectOutput {
            public_key: pubkey,
            address,
        };

        let out_string = serde_json::to_string_pretty(&out)?;
        println!("{out_string}");
    } else {
        println!("Public key: {pubkey}");
        println!("Address: {address}");
    }

    Ok(())
}

enum Payload {
    Readable(String),
    NotReadable(Vec<u8>),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CliMessage {
    id: MessageId,
    src_ip: IpAddr,
    src_pk: PublicKey,
    dst_ip: IpAddr,
    dst_pk: PublicKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "serialize_payload")]
    topic: Option<Payload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "serialize_payload")]
    payload: Option<Payload>,
}

const B64ENGINE: GeneralPurpose = base64::engine::general_purpose::GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new(),
);
fn serialize_payload<S: Serializer>(p: &Option<Payload>, s: S) -> Result<S::Ok, S::Error> {
    let base64 = match p {
        None => None,
        Some(Payload::Readable(data)) => Some(data.clone()),
        Some(Payload::NotReadable(data)) => Some(B64ENGINE.encode(data)),
    };
    <Option<String>>::serialize(&base64, s)
}

/// Encode arbitrary data in standard base64.
pub fn encode_base64(input: &[u8]) -> String {
    B64ENGINE.encode(input)
}

/// Send a message to a receiver.
#[allow(clippy::too_many_arguments)]
async fn send_msg(
    destination: String,
    msg: Option<String>,
    wait: bool,
    timeout: Option<u64>,
    reply_to: Option<String>,
    topic: Option<String>,
    msg_path: Option<PathBuf>,
    server_addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    if reply_to.is_some() && wait {
        error!("Can't wait on a reply for a reply, either use --reply-to or --wait");
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Only one of --reply-to or --wait is allowed",
        )
        .into());
    }
    let destination = if destination.len() == 64 {
        // Public key in hex format
        match PublicKey::try_from(&*destination) {
            Err(_) => {
                error!("{destination} is not a valid hex encoded public key");
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Invalid hex encoded public key",
                )
                .into());
            }
            Ok(pk) => MessageDestination::Pk(pk),
        }
    } else {
        match destination.parse() {
            Err(e) => {
                error!("{destination} is not a valid IPv6 address: {e}");
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Invalid IPv6 address",
                )
                .into());
            }
            Ok(ip) => {
                let global_subnet =
                    Subnet::new(GLOBAL_SUBNET_ADDRESS, GLOBAL_SUBNET_PREFIX_LEN).unwrap();
                if !global_subnet.contains_ip(ip) {
                    error!("{destination} is not a part of {global_subnet}");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "IPv6 address is not part of the mycelium subnet",
                    )
                    .into());
                }
                MessageDestination::Ip(ip)
            }
        }
    };

    // Load msg, files have prio.
    let msg = if let Some(path) = msg_path {
        match tokio::fs::read(&path).await {
            Err(e) => {
                error!("Could not read file at {:?}: {e}", path);
                return Err(e.into());
            }
            Ok(data) => data,
        }
    } else if let Some(msg) = msg {
        msg.into_bytes()
    } else {
        error!("Message is a required argument if `--msg-path` is not provided");
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Message is a required argument if `--msg-path` is not provided",
        )
        .into());
    };

    let mut url = format!("http://{server_addr}/api/v1/messages");
    if let Some(reply_to) = reply_to {
        url.push_str(&format!("/reply/{reply_to}"));
    }
    if wait {
        // A year should be sufficient to wait
        let reply_timeout = timeout.unwrap_or(60 * 60 * 24 * 365);
        url.push_str(&format!("?reply_timeout={reply_timeout}"));
    }

    match reqwest::Client::new()
        .post(url)
        .json(&MessageSendInfo {
            dst: destination,
            topic: topic.map(String::into_bytes),
            payload: msg,
        })
        .send()
        .await
    {
        Err(e) => {
            error!("Failed to send request: {e}");
            return Err(e.into());
        }
        Ok(res) => {
            if res.status() == STATUSCODE_NO_CONTENT {
                return Ok(());
            }
            match res.json::<PushMessageResponse>().await {
                Err(e) => {
                    error!("Failed to load response body {e}");
                    return Err(e.into());
                }
                Ok(resp) => {
                    match resp {
                        PushMessageResponse::Id(id) => {
                            let _ = serde_json::to_writer(std::io::stdout(), &id);
                        }
                        PushMessageResponse::Reply(mri) => {
                            let cm = CliMessage {
                                id: mri.id,

                                topic: mri.topic.map(|topic| {
                                    if let Ok(s) = String::from_utf8(topic.clone()) {
                                        Payload::Readable(s)
                                    } else {
                                        Payload::NotReadable(topic)
                                    }
                                }),
                                src_ip: mri.src_ip,
                                src_pk: mri.src_pk,
                                dst_ip: mri.dst_ip,
                                dst_pk: mri.dst_pk,
                                payload: Some({
                                    if let Ok(s) = String::from_utf8(mri.payload.clone()) {
                                        Payload::Readable(s)
                                    } else {
                                        Payload::NotReadable(mri.payload)
                                    }
                                }),
                            };
                            let _ = serde_json::to_writer(std::io::stdout(), &cm);
                        }
                    }
                    println!();
                }
            }
        }
    }

    Ok(())
}

const STATUSCODE_NO_CONTENT: u16 = 204;

async fn recv_msg(
    timeout: Option<u64>,
    topic: Option<String>,
    msg_path: Option<PathBuf>,
    raw: bool,
    server_addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    // One year timeout should be sufficient
    let timeout = timeout.unwrap_or(60 * 60 * 24 * 365);
    let mut url = format!("http://{server_addr}/api/v1/messages?timeout={timeout}");
    if let Some(ref topic) = topic {
        if topic.len() > 255 {
            error!("{topic} is longer than the maximum allowed topic length of 255");
            return Err(
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "Topic too long").into(),
            );
        }
        url.push_str(&format!("&topic={}", encode_base64(topic.as_bytes())));
    }
    let mut cm = match reqwest::get(url).await {
        Err(e) => {
            error!("Failed to wait for message: {e}");
            return Err(e.into());
        }
        Ok(resp) => {
            if resp.status() == STATUSCODE_NO_CONTENT {
                debug!("No message ready yet");
                return Ok(());
            }

            debug!("Received message response");
            match resp.json::<MessageReceiveInfo>().await {
                Err(e) => {
                    error!("Failed to load response json: {e}");
                    return Err(e.into());
                }
                Ok(mri) => CliMessage {
                    id: mri.id,
                    topic: mri.topic.map(|topic| {
                        if let Ok(s) = String::from_utf8(topic.clone()) {
                            Payload::Readable(s)
                        } else {
                            Payload::NotReadable(topic)
                        }
                    }),
                    src_ip: mri.src_ip,
                    src_pk: mri.src_pk,
                    dst_ip: mri.dst_ip,
                    dst_pk: mri.dst_pk,
                    payload: Some({
                        if let Ok(s) = String::from_utf8(mri.payload.clone()) {
                            Payload::Readable(s)
                        } else {
                            Payload::NotReadable(mri.payload)
                        }
                    }),
                },
            }
        }
    };

    if let Some(ref file_path) = msg_path {
        if let Err(e) = tokio::fs::write(
            &file_path,
            match mem::take(&mut cm.payload).unwrap() {
                Payload::Readable(ref s) => s as &dyn AsRef<[u8]>,
                Payload::NotReadable(ref v) => v,
            },
        )
        .await
        {
            error!("Failed to write response payload to file: {e}");
            return Err(e.into());
        }
    }

    if raw {
        // only print payload if not already written
        if msg_path.is_none() {
            let _ = std::io::stdout().write_all(match cm.payload.unwrap() {
                Payload::Readable(ref s) => s.as_bytes(),
                Payload::NotReadable(ref v) => v,
            });
            println!();
        }
    } else {
        let _ = serde_json::to_writer(std::io::stdout(), &cm);
        println!();
    }

    Ok(())
}
