use std::error::Error;
use std::path::PathBuf;

use clap::{Args, Parser};
use myceliumd_common::{Cli, MyceliumConfig, NodeArguments, PrivateNetworkKey};
use serde::Deserialize;

#[derive(Debug, Args)]
pub struct PrivateNodeArguments {
    #[clap(flatten)]
    pub base: NodeArguments,

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
}

/// Extended file config with private-network fields.
#[derive(Debug, Deserialize, Default)]
struct PrivateMyceliumConfig {
    #[serde(flatten)]
    base: MyceliumConfig,
    network_name: Option<String>,
    network_key_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::<PrivateNodeArguments>::parse();

    let private_config: PrivateMyceliumConfig =
        myceliumd_common::load_config_file(&cli.config_file)?;

    myceliumd_common::init_logging(cli.silent, cli.debug, &cli.logging_format);

    let key_path = myceliumd_common::resolve_key_path(cli.key_file);

    match cli.command {
        None => {
            // Resolve private network config from CLI args or file config
            let network_name = cli.node_args.network_name.or(private_config.network_name);
            let network_key_file = cli
                .node_args
                .network_key_file
                .or(private_config.network_key_file);

            let private_network_config = match (network_name, network_key_file) {
                (Some(name), Some(key_file)) => {
                    let net_key: PrivateNetworkKey =
                        myceliumd_common::load_key_file(&key_file).await?;
                    Some((name, net_key))
                }
                _ => None,
            };

            let merged_config =
                myceliumd_common::merge_config(cli.node_args.base, private_config.base);
            myceliumd_common::run_node(merged_config, private_network_config, key_path).await
        }
        Some(cmd) => {
            myceliumd_common::dispatch_subcommand(cmd, key_path, cli.node_args.base.api_addr).await
        }
    }
}
