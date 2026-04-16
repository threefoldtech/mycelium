use mycelium::peer_manager::PeerStats;
use prettytable::{row, Table};
use std::path::Path;
use tracing::error;

use crate::rpc_client::rpc_call;

/// List the peers the current node is connected to
pub async fn list_peers(
    socket_path: &Path,
    json_print: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = rpc_call(socket_path, "getPeers", serde_json::json!({})).await?;

    if json_print {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let peers: Vec<PeerStats> = serde_json::from_value(result).map_err(|e| {
            error!("Failed to parse peers response: {e}");
            e
        })?;

        let mut table = Table::new();
        table.add_row(row![
            "Protocol",
            "Socket",
            "Type",
            "Connection",
            "Rx total",
            "Tx total",
            "Discovered",
            "Last connection"
        ]);
        for peer in peers.iter() {
            table.add_row(row![
                peer.endpoint.proto(),
                peer.endpoint.address(),
                peer.pt,
                peer.connection_state,
                format_bytes(peer.rx_bytes),
                format_bytes(peer.tx_bytes),
                format_seconds(peer.discovered),
                peer.last_connected
                    .map(format_seconds)
                    .unwrap_or("Never connected".to_string()),
            ]);
        }
        table.printstd();
    }

    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    let byte = byte_unit::Byte::from_u64(bytes);
    let adjusted_byte = byte.get_appropriate_unit(byte_unit::UnitType::Binary);
    format!(
        "{:.2} {}",
        adjusted_byte.get_value(),
        adjusted_byte.get_unit()
    )
}

/// Convert an amount of seconds into a human readable string.
fn format_seconds(total_seconds: u64) -> String {
    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = (total_seconds / 3600) % 60;
    let days = (total_seconds / 86400) % 60;

    if days > 0 {
        format!("{days}d {hours}h {minutes}m {seconds}s")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// Remove peer(s) by (underlay) IP
pub async fn remove_peers(
    socket_path: &Path,
    peers: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    for peer in peers.iter() {
        rpc_call(
            socket_path,
            "deletePeer",
            serde_json::json!({ "endpoint": peer }),
        )
        .await
        .map_err(|e| {
            error!("Failed to delete peer {peer}: {e}");
            e
        })?;
    }
    Ok(())
}

/// Add peer(s) by (underlay) IP
pub async fn add_peers(
    socket_path: &Path,
    peers: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    for peer in peers.into_iter() {
        rpc_call(
            socket_path,
            "addPeer",
            serde_json::json!({ "endpoint": peer }),
        )
        .await
        .map_err(|e| {
            error!("Failed to add peer {peer}: {e}");
            e
        })?;
    }
    Ok(())
}
