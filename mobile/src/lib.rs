use std::convert::TryFrom;
use std::io;

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
pub async fn start_mycelium(peers: Vec<String>, tun_fd: i32, priv_key: Vec<u8>) {
    let endpoints: Vec<Endpoint> = peers
        .into_iter()
        .filter_map(|peer| peer.parse().ok())
        .collect();

    #[cfg(target_os = "android")]
    setup_the_logger();

    let secret_key = build_secret_key(priv_key).await.unwrap();

    let config = Config {
        node_key: secret_key,
        peers: endpoints,
        no_tun: false,
        tcp_listen_port: DEFAULT_TCP_LISTEN_PORT,
        quic_listen_port: DEFAULT_QUIC_LISTEN_PORT,
        peer_discovery_port: Some(DEFAULT_PEER_DISCOVERY_PORT),
        tun_name: "tun0".to_string(),
        metrics: NoMetrics,
        private_network_config: None,
        firewall_mark: None,
        tun_fd: Some(tun_fd),
    };
    let _node = Node::new(config).await;

    match _node {
        Ok(_) => info!("node successfully created"),
        // use info! here because error! is not printed
        Err(err) => info!("failed to create stack: {err}"),
    };

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
