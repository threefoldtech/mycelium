use prettytable::{row, Table};
use std::net::{Ipv6Addr, SocketAddr};
use std::path::Path;
use tracing::error;

use crate::rpc_client::rpc_call;

/// List known valid proxy IPv6 addresses discovered by the node.
pub async fn list_proxies(
    socket_path: &Path,
    json_print: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = rpc_call(socket_path, "getProxies", serde_json::json!({})).await?;

    if json_print {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let proxies: Vec<Ipv6Addr> = serde_json::from_value(result).map_err(|e| {
            error!("Failed to parse proxies response: {e}");
            e
        })?;
        let mut table = Table::new();
        table.add_row(row!["IPv6 Address"]);
        for ip in proxies {
            table.add_row(row![ip]);
        }
        table.printstd();
    }

    Ok(())
}

/// Connect to a proxy. When `remote` is None, the node will auto-select the best known proxy.
pub async fn connect_proxy(
    socket_path: &Path,
    remote: Option<SocketAddr>,
    json_print: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = if let Some(addr) = remote {
        serde_json::json!({ "remote": addr.to_string() })
    } else {
        serde_json::json!({})
    };

    let result = rpc_call(socket_path, "connectProxy", params).await.map_err(|e| {
        error!("Failed to send connect proxy request: {e}");
        e
    })?;

    if json_print {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        // result is the connected proxy address as a string or null
        if result.is_null() {
            println!("No proxy connected");
        } else {
            let addr_str = result
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| result.to_string());
            println!("{addr_str}");
        }
    }

    Ok(())
}

/// Disconnect from the current proxy, if any.
pub async fn disconnect_proxy(socket_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    rpc_call(socket_path, "disconnectProxy", serde_json::json!({}))
        .await
        .map_err(|e| {
            error!("Failed to disconnect proxy: {e}");
            e
        })?;
    Ok(())
}

/// Start background probing for proxies.
pub async fn start_proxy_probe(socket_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    rpc_call(socket_path, "startProxyProbe", serde_json::json!({}))
        .await
        .map_err(|e| {
            error!("Failed to start proxy probe: {e}");
            e
        })?;
    Ok(())
}

/// Stop background probing for proxies.
pub async fn stop_proxy_probe(socket_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    rpc_call(socket_path, "stopProxyProbe", serde_json::json!({}))
        .await
        .map_err(|e| {
            error!("Failed to stop proxy probe: {e}");
            e
        })?;
    Ok(())
}
