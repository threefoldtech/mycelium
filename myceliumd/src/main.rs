use std::error::Error;

use clap::Parser;
use myceliumd_common::{Cli, MyceliumConfig, NodeArguments};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::<NodeArguments>::parse();

    let mycelium_config: MyceliumConfig = myceliumd_common::load_config_file(&cli.config_file)?;

    myceliumd_common::init_logging(cli.silent, cli.debug, &cli.logging_format);

    let key_path = myceliumd_common::resolve_key_path(cli.key_file);

    match cli.command {
        None => {
            let merged_config = myceliumd_common::merge_config(cli.node_args, mycelium_config);
            myceliumd_common::run_node(merged_config, None, key_path).await
        }
        Some(cmd) => {
            myceliumd_common::dispatch_subcommand(cmd, key_path, cli.node_args.api_addr).await
        }
    }
}
