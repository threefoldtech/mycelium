use prettytable::{row, Table};
use std::net::{Ipv6Addr, SocketAddr};
use tracing::{debug, error};

#[derive(serde::Serialize)]
struct ConnectProxyRequest {
    remote: Option<String>,
}

/// List known valid proxy IPv6 addresses discovered by the node.
pub async fn list_proxies(
    server_addr: SocketAddr,
    json_print: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let request_url = format!("http://{server_addr}/api/v1/admin/proxy");
    match reqwest::get(&request_url).await {
        Err(e) => {
            error!("Failed to retrieve proxies");
            return Err(e.into());
        }
        Ok(resp) => {
            debug!("Listing known proxies");
            if json_print {
                let body = resp.text().await?;
                println!("{body}");
            } else {
                let proxies: Vec<Ipv6Addr> = resp.json().await?;
                let mut table = Table::new();
                table.add_row(row!["IPv6 Address"]);
                for ip in proxies {
                    table.add_row(row![ip]);
                }
                table.printstd();
            }
        }
    }
    Ok(())
}

/// Connect to a proxy. When `remote` is None, the node will auto-select the best known proxy.
pub async fn connect_proxy(
    server_addr: SocketAddr,
    remote: Option<SocketAddr>,
    json_print: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let request_url = format!("http://{server_addr}/api/v1/admin/proxy");
    let payload = ConnectProxyRequest {
        remote: remote.map(|r| r.to_string()),
    };

    let res = client.post(&request_url).json(&payload).send().await;
    match res {
        Err(e) => {
            error!("Failed to send connect proxy request: {e}");
            Err(e.into())
        }
        Ok(resp) => {
            if resp.status().is_success() {
                if json_print {
                    let body = resp.text().await?;
                    println!("{body}");
                } else {
                    let addr: SocketAddr = resp.json().await?;
                    println!("{addr}");
                }
                Ok(())
            } else {
                if resp.status().as_u16() == 404 {
                    error!("No valid proxy available or connection failed");
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "No valid proxy available or connection failed",
                    )
                    .into())
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    error!("Proxy connect failed, status {status}, body: {body}");
                    Err(
                        std::io::Error::new(std::io::ErrorKind::Other, format!("HTTP {status}"))
                            .into(),
                    )
                }
            }
        }
    }
}

/// Disconnect from the current proxy, if any.
pub async fn disconnect_proxy(server_addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let request_url = format!("http://{server_addr}/api/v1/admin/proxy");
    if let Err(e) = client
        .delete(&request_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        error!("Failed to disconnect proxy: {e}");
        return Err(e.into());
    }
    Ok(())
}

/// Start background probing for proxies.
pub async fn start_proxy_probe(server_addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let request_url = format!("http://{server_addr}/api/v1/admin/proxy/probe");
    if let Err(e) = reqwest::get(&request_url)
        .await
        .and_then(|r| r.error_for_status())
    {
        error!("Failed to start proxy probe: {e}");
        return Err(e.into());
    }
    Ok(())
}

/// Stop background probing for proxies.
pub async fn stop_proxy_probe(server_addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let request_url = format!("http://{server_addr}/api/v1/admin/proxy/probe");
    if let Err(e) = client
        .delete(&request_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        error!("Failed to stop proxy probe: {e}");
        return Err(e.into());
    }
    Ok(())
}
