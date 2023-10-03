use bytes::BytesMut;
use clap::{Parser, Subcommand};
use crypto::PublicKey;
use log::{debug, error, info};
use mycelium::crypto;
use mycelium::data::DataPlane;
use mycelium::filters;
use mycelium::peer_manager;
use mycelium::router;
use mycelium::router::StaticRoute;
use mycelium::subnet::Subnet;
use serde::Serialize;
use std::{
    error::Error,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    path::PathBuf,
};
use tokio::signal::{self, unix::SignalKind};
mod tun;

/// The default port on the inderlay to listen on.
const DEFAULT_LISTEN_PORT: u16 = 9651;

const DEFAULT_KEY_FILE: &str = "priv_key.bin";

/// Default name of tun interface
const TUN_NAME: &str = "tun0";
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    pretty_env_logger::init_timed();

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
        }
    }

    // Generate the node's IPv6 address from its public key
    let node_addr = node_pub_key.address();
    info!("Node address: {}", node_addr);

    let (rxhalf, txhalf) = tun::new(
        TUN_NAME,
        Subnet::new(node_addr.into(), 64).expect("64 is a valid subnet size for IPv6; qed"),
        Subnet::new(TUN_ROUTE_DEST.into(), TUN_ROUTE_PREFIX)
            .expect("Static configured TUN route is valid; qed"),
    )
    .await?;

    debug!("Node public key: {:?}", node_keypair.1);

    let static_peers = cli.static_peers;

    let (tun_tx, tun_rx) = tokio::sync::mpsc::unbounded_channel();

    // Creating a new Router instance
    let router = match router::Router::new(
        tun_tx,
        Subnet::new(node_addr.into(), 64).expect("64 is a valid IPv6 prefix size; qed"),
        vec![StaticRoute::new(
            Subnet::new(node_addr.into(), 64).expect("64 is a valid IPv6 prefix size; qed"),
        )],
        node_keypair.clone(),
        vec![
            Box::new(filters::AllowedSubnet::new(
                Subnet::new(GLOBAL_SUBNET_ADDRESS, GLOBAL_SUBNET_PREFIX_LEN)
                    .expect("Global subnet is properly defined; qed"),
            )),
            Box::new(filters::MaxSubnetSize::<64>),
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

    let _data_plane = DataPlane::new(
        router.clone(),
        rxhalf,
        txhalf,
        // TODO: proper sink to message handler
        futures::sink::drain(),
        tun_rx,
    );

    // TODO: put in dedicated file so we can only rely on certain signals on unix platforms
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

            println!("\n----------- Current peers: -----------");
            for p in router.peer_interfaces() {
                println!(
                    "Peer: {:?}, with link cost: {}",
                    p.overlay_ip(),
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
