use mycelium_api::PacketStatistics;
use prettytable::{row, Table};
use std::net::SocketAddr;

use tracing::{debug, error};

pub async fn list_packet_stats(
    server_addr: SocketAddr,
    json_print: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let request_url = format!("http://{server_addr}/api/v1/admin/stats/packets");
    match reqwest::get(&request_url).await {
        Err(e) => {
            error!("Failed to retrieve packet statistics");
            return Err(e.into());
        }
        Ok(resp) => {
            debug!("Listing packet statistics");

            if json_print {
                let stats = resp.text().await?;
                println!("{stats}");
            } else {
                let stats: PacketStatistics = resp.json().await?;

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
        }
    }

    Ok(())
}
