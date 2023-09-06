use crate::router::StaticRoute;
use crate::{packet::DataPacket, subnet::Subnet};
use bytes::BytesMut;
use clap::{Parser, Subcommand};
use crypto::PublicKey;
use etherparse::IpHeader;
use futures::{SinkExt, StreamExt};
use log::{debug, error, info, trace, warn};
use serde::Serialize;
use std::{
    error::Error,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    path::PathBuf,
};
use tokio::signal::{self, unix::SignalKind};

mod babel;
mod codec;
mod crypto;
mod interval;
mod metric;
mod packet;
mod peer;
mod peer_manager;
mod router;
mod routing_table;
mod sequence_number;
mod source_table;
mod subnet;
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
    //console_subscriber::init();
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

    let (mut rxhalf, mut txhalf) = tun::new(
        TUN_NAME,
        node_addr.into(),
        64,
        TUN_ROUTE_DEST.into(),
        TUN_ROUTE_PREFIX,
    )
    .await?;

    debug!("Node public key: {:?}", node_keypair.1);

    let static_peers = cli.static_peers;

    let (tun_tx, mut tun_rx) = tokio::sync::mpsc::unbounded_channel();

    // Creating a new Router instance
    let router = match router::Router::new(
        tun_tx,
        node_addr,
        vec![StaticRoute::new(
            Subnet::new(node_addr.into(), 64).expect("64 is a valid IPv6 prefix size; qed"),
        )],
        node_keypair.clone(),
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

    // Read packets from the TUN interface (originating from the kernel) and send them to the router
    // Note: we will never receive control packets from the kernel, only data packets
    {
        let router = router.clone();
        let router_data_tx = router.router_data_tx();

        tokio::spawn(async move {
            while let Some(packet) = rxhalf.next().await {
                let packet = match packet {
                    Err(e) => {
                        error!("Failed to read packet from TUN interface {e}");
                        continue;
                    }
                    Ok(packet) => packet,
                };

                trace!("Received packet from tun");
                let headers = match etherparse::IpHeader::from_slice(&packet) {
                    Ok(header) => header,
                    Err(e) => {
                        warn!("Could not parse IP header from tun packet: {e}");
                        continue;
                    }
                };
                let dest_addr = if let IpHeader::Version6(header, _) = headers.0 {
                    Ipv6Addr::from(header.destination)
                } else {
                    debug!("Drop non ipv6 packet");
                    continue;
                };

                trace!("Received packet from TUN with dest addr: {:?}", dest_addr);

                // Check if destination address is in 200::/7 range
                let first_byte = dest_addr.segments()[0] >> 8; // get the first byte
                if !(0x02..=0x3F).contains(&first_byte) {
                    debug!("Dropping packet which is not destined for 200::/7");
                    continue;
                }

                // Get shared secret from node and dest address
                let shared_secret = match router.get_shared_secret_from_dest(&dest_addr) {
                    Some(ss) => ss,
                    None => {
                        debug!(
                            "No entry found for destination address {}, dropping packet",
                            dest_addr
                        );
                        continue;
                    }
                };

                let node_pk = router.node_public_key();
                // inject own pubkey
                if let Err(e) = router_data_tx
                    .send(DataPacket {
                        dest_ip: dest_addr,
                        pubkey: node_pk,
                        // encrypt data with shared secret
                        raw_data: shared_secret.encrypt(&packet),
                    })
                    .await
                {
                    error!("Could not forward TUN data to router: {e}");
                }
            }
            warn!("tun stream is done");
        });
    }

    {
        tokio::spawn(async move {
            loop {
                while let Some(packet) = tun_rx.recv().await {
                    trace!("received packet from tun_rx");
                    if let Err(e) = txhalf.send(packet.clone().into()).await {
                        error!(
                            "Failed to send packet on local TUN interface: {e} - {}",
                            faster_hex::hex_string(&packet)
                        );
                        continue;
                    }
                    trace!("Sent packet on tun interface");
                }
            }
        });
    }

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
