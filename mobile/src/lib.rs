use std::convert::TryFrom;
use std::io;

use tracing::{error, info};

use metrics::Metrics;
use mycelium::endpoint::Endpoint;
use mycelium::{crypto, metrics, Config, Node};
use once_cell::sync::Lazy;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep, timeout, Duration};

const CHANNEL_MSG_OK: &str = "ok";
const CHANNEL_TIMEOUT: u64 = 2;

#[cfg(target_os = "android")]
fn setup_logging() {
    use tracing::level_filters::LevelFilter;
    use tracing_subscriber::filter::Targets;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let targets = Targets::new()
        .with_default(LevelFilter::INFO)
        .with_target("mycelium::router", LevelFilter::WARN);
    tracing_subscriber::registry()
        .with(tracing_android::layer("mycelium").expect("failed to setup logger"))
        .with(targets)
        .init();
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
fn setup_logging() {
    use tracing::level_filters::LevelFilter;
    use tracing_oslog::OsLogger;
    use tracing_subscriber::filter::Targets;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let targets = Targets::new()
        .with_default(LevelFilter::INFO)
        .with_target("mycelium::router", LevelFilter::WARN);
    tracing_subscriber::registry()
        .with(OsLogger::new("mycelium", "default"))
        .with(targets)
        .init();
}

#[cfg(any(target_os = "android", target_os = "ios", target_os = "macos"))]
static INIT_LOG: Lazy<()> = Lazy::new(|| {
    setup_logging();
});

#[cfg(any(target_os = "android", target_os = "ios", target_os = "macos"))]
fn setup_logging_once() {
    // Accessing the Lazy value will ensure setup_logging is called exactly once
    let _ = &*INIT_LOG;
}

// Declare the channel globally so we can use it on the start & stop mycelium functions
type CommandChannelType = (Mutex<mpsc::Sender<Cmd>>, Mutex<mpsc::Receiver<Cmd>>);
static COMMAND_CHANNEL: Lazy<CommandChannelType> = Lazy::new(|| {
    let (tx_cmd, rx_cmd) = mpsc::channel::<Cmd>(1);
    (Mutex::new(tx_cmd), Mutex::new(rx_cmd))
});

type ResponseChannelType = (
    Mutex<mpsc::Sender<Response>>,
    Mutex<mpsc::Receiver<Response>>,
);
static RESPONSE_CHANNEL: Lazy<ResponseChannelType> = Lazy::new(|| {
    let (tx_resp, rx_resp) = mpsc::channel::<Response>(1);
    (Mutex::new(tx_resp), Mutex::new(rx_resp))
});

#[tokio::main]
#[allow(unused_variables)] // because tun_fd is only used in android and ios
pub async fn start_mycelium(peers: Vec<String>, tun_fd: i32, priv_key: Vec<u8>) {
    #[cfg(any(target_os = "android", target_os = "ios", target_os = "macos"))]
    setup_logging_once();

    info!("starting mycelium");
    let endpoints: Vec<Endpoint> = peers
        .into_iter()
        .filter_map(|peer| peer.parse().ok())
        .collect();

    let secret_key = build_secret_key(priv_key).await.unwrap();

    let config = Config {
        node_key: secret_key,
        peers: endpoints,
        no_tun: false,
        tcp_listen_port: DEFAULT_TCP_LISTEN_PORT,
        quic_listen_port: None,
        peer_discovery_port: None, // disable multicast discovery
        #[cfg(any(
            target_os = "linux",
            all(target_os = "macos", not(feature = "mactunfd")),
            target_os = "windows"
        ))]
        tun_name: "tun0".to_string(),

        metrics: NoMetrics,
        private_network_config: None,
        firewall_mark: None,
        #[cfg(any(
            target_os = "android",
            target_os = "ios",
            all(target_os = "macos", feature = "mactunfd"),
        ))]
        tun_fd: Some(tun_fd),
        update_workers: 1,
    };
    let _node = match Node::new(config).await {
        Ok(node) => {
            info!("node successfully created");
            node
        }
        Err(err) => {
            error!("failed to create mycelium node: {err}");
            return;
        }
    };

    let mut rx = COMMAND_CHANNEL.1.lock().await;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c()  => {
                info!("Received SIGINT, stopping mycelium node");
                break;
            }
           cmd = rx.recv() => {
                match cmd.unwrap().cmd {
                    CmdType::Stop => {
                        info!("Received stop command, stopping mycelium node");
                        send_response(vec![CHANNEL_MSG_OK.to_string()]).await;
                        break;
                    }
                    CmdType::Status => {
                        let mut vec: Vec<String> = Vec::new();
                        for info in _node.peer_info() {
                            vec.push(info.endpoint.proto().to_string() + ","+ info.endpoint.address().to_string().as_str()+","+ &info.connection_state.to_string());
                        }
                        send_response(vec).await;
                    }
                }
            }
        }
    }
    info!("mycelium stopped");
}

struct Cmd {
    cmd: CmdType,
}

enum CmdType {
    Stop,
    Status,
}

struct Response {
    response: Vec<String>,
}

// stop_mycelium returns string with the status of the command
#[tokio::main]
pub async fn stop_mycelium() -> String {
    if let Err(e) = send_command(CmdType::Stop).await {
        return e.to_string();
    }

    match recv_response().await {
        Ok(_) => CHANNEL_MSG_OK.to_string(),
        Err(e) => e.to_string(),
    }
}

// get_peer_status returns vector of string
// first element is always the status of the command (ok or error)
// next elements are the peer status
#[tokio::main]
pub async fn get_peer_status() -> Vec<String> {
    if let Err(e) = send_command(CmdType::Status).await {
        return vec![e.to_string()];
    }

    match recv_response().await {
        Ok(mut resp) => {
            resp.insert(0, CHANNEL_MSG_OK.to_string());
            resp
        }
        Err(e) => vec![e.to_string()],
    }
}

#[tokio::main]
pub async fn get_status() -> Result<String, NodeError> {
    Err(NodeError::NodeDead)
}

use thiserror::Error;
#[derive(Error, Debug)]
pub enum NodeError {
    #[error("err_node_dead")]
    NodeDead,

    #[error("err_node_timeout")]
    NodeTimeout,
}

async fn send_command(cmd_type: CmdType) -> Result<(), NodeError> {
    let tx = COMMAND_CHANNEL.0.lock().await;
    tokio::select! {
        _ = sleep(Duration::from_secs(CHANNEL_TIMEOUT)) => {
            Err(NodeError::NodeTimeout)
        }
        result = tx.send(Cmd { cmd: cmd_type }) => {
            match result {
                Ok(_) => Ok(()),
                Err(_) => Err(NodeError::NodeDead)
            }
        }
    }
}

async fn send_response(resp: Vec<String>) {
    let tx = RESPONSE_CHANNEL.0.lock().await;

    tokio::select! {
        _ = sleep(Duration::from_secs(CHANNEL_TIMEOUT)) => {
            error!("send_response timeout");
        }
        result = tx.send(Response { response: resp }) => {
            match result {
                Ok(_) => {},
                Err(_) =>{error!("send_response failed");},
            }
        }
    }
}

async fn recv_response() -> Result<Vec<String>, NodeError> {
    let mut rx = RESPONSE_CHANNEL.1.lock().await;
    let duration = Duration::from_secs(CHANNEL_TIMEOUT);
    match timeout(duration, rx.recv()).await {
        Ok(result) => match result {
            Some(resp) => Ok(resp.response),
            None => Err(NodeError::NodeDead),
        },
        Err(_) => Err(NodeError::NodeTimeout),
    }
}

#[derive(Clone)]
pub struct NoMetrics;
impl Metrics for NoMetrics {}

/// The default port on the underlay to listen on for incoming TCP connections.
const DEFAULT_TCP_LISTEN_PORT: u16 = 9651;

fn convert_slice_to_array32(slice: &[u8]) -> Result<[u8; 32], std::array::TryFromSliceError> {
    <[u8; 32]>::try_from(slice)
}

async fn build_secret_key<T>(bin: Vec<u8>) -> Result<T, io::Error>
where
    T: From<[u8; 32]>,
{
    Ok(T::from(convert_slice_to_array32(bin.as_slice()).unwrap()))
}

/// generate secret key
/// it is used by android & ios app
pub fn generate_secret_key() -> Vec<u8> {
    crypto::SecretKey::new().as_bytes().into()
}

/// generate node_address from secret key
pub fn address_from_secret_key(data: Vec<u8>) -> String {
    let data = <[u8; 32]>::try_from(data.as_slice()).unwrap();
    let secret_key = crypto::SecretKey::from(data);
    crypto::PublicKey::from(&secret_key).address().to_string()
}
