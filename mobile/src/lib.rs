use std::convert::TryFrom;
use std::io;

use tracing::{error, info};

use metrics::Metrics;
use mycelium::endpoint::Endpoint;
use mycelium::{crypto, metrics, Config, Node};

#[cfg(target_os = "android")]
fn setup_logging() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(tracing_android::layer("mycelium").expect("failed to setup logger"))
        .init();
}

#[cfg(target_os = "ios")]
fn setup_logging() {
    use tracing_oslog::OsLogger;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(OsLogger::new("mycelium", "default"))
        .init();
}

use once_cell::sync::Lazy;
use tokio::sync::{mpsc, Mutex};

// Declare the channel globally so we can use it on the start & stop mycelium functions
#[allow(clippy::type_complexity)]
static CHANNEL: Lazy<(Mutex<mpsc::Sender<()>>, Mutex<mpsc::Receiver<()>>)> = Lazy::new(|| {
    let (tx, rx) = mpsc::channel::<()>(1);
    (Mutex::new(tx), Mutex::new(rx))
});

#[tokio::main]
#[allow(unused_variables)] // because tun_fd is only used in android and ios
pub async fn start_mycelium(peers: Vec<String>, tun_fd: i32, priv_key: Vec<u8>) {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    setup_logging();

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
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        tun_name: "tun0".to_string(),

        metrics: NoMetrics,
        private_network_config: None,
        firewall_mark: None,
        #[cfg(any(target_os = "android", target_os = "ios"))]
        tun_fd: Some(tun_fd),
    };
    let _node = Node::new(config).await;

    match _node {
        Ok(_) => info!("node successfully created"),
        Err(err) => error!("failed to create mycelium node: {err}"),
    };

    let mut rx = CHANNEL.1.lock().await;
    tokio::select! {
        _ = tokio::signal::ctrl_c()  => {
            info!("Received SIGINT, stopping mycelium node");
        }
       _ = rx.recv() => {
            info!("Received stop channel, stopping mycelium node");
        }
    }
    info!("mycelium stopped");
}

#[tokio::main]
pub async fn stop_mycelium() {
    info!("stopping mycelium by sending stop channel");
    // TODO: check what happens if we send multiple times?
    // it is currently OK to have this implementation because
    // we prevent multiple calls to stop_mycelium from the UI side.
    let tx = CHANNEL.0.lock().await;
    tx.send(()).await.unwrap();
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
