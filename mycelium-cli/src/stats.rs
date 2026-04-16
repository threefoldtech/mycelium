use mycelium_api::PacketStatistics;
use prettytable::{row, Table};
use std::path::Path;
use tracing::error;

use crate::rpc_client::rpc_call;

pub async fn list_packet_stats(
    socket_path: &Path,
    json_print: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = rpc_call(socket_path, "getPacketStatistics", serde_json::json!({})).await?;

    if json_print {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let stats: PacketStatistics = serde_json::from_value(result).map_err(|e| {
            error!("Failed to parse packet statistics response: {e}");
            e
        })?;

        println!("Statistics by Source IP:");
        let mut table = Table::new();
        table.add_row(row!["Source IP", "Packets", "Bytes"]);
        for entry in stats.by_source.iter() {
            table.add_row(row![entry.ip, entry.packet_count, entry.byte_count,]);
        }
        table.printstd();

        println!("\nStatistics by Destination IP:");
        let mut table = Table::new();
        table.add_row(row!["Destination IP", "Packets", "Bytes"]);
        for entry in stats.by_destination.iter() {
            table.add_row(row![entry.ip, entry.packet_count, entry.byte_count,]);
        }
        table.printstd();
    }

    Ok(())
}
