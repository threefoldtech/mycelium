use mycelium::endpoint::Endpoint;
use mycelium_api::AddPeer;
use std::net::SocketAddr;
use urlencoding::encode;

pub async fn get_peers(
    server_addr: SocketAddr,
) -> Result<Vec<mycelium::peer_manager::PeerStats>, reqwest::Error> {
    let request_url = format!("http://{server_addr}/api/v1/admin/peers");
    match reqwest::get(&request_url).await {
        Err(e) => Err(e),
        Ok(resp) => match resp.json::<Vec<mycelium::peer_manager::PeerStats>>().await {
            Err(e) => Err(e),
            Ok(peers) => Ok(peers),
        },
    }
}

pub async fn get_selected_routes(
    server_addr: SocketAddr,
) -> Result<Vec<mycelium_api::Route>, reqwest::Error> {
    let request_url = format!("http://{server_addr}/api/v1/admin/routes/selected");
    match reqwest::get(&request_url).await {
        Err(e) => Err(e),
        Ok(resp) => match resp.json::<Vec<mycelium_api::Route>>().await {
            Err(e) => Err(e),
            Ok(selected_routes) => Ok(selected_routes),
        },
    }
}

pub async fn get_fallback_routes(
    server_addr: SocketAddr,
) -> Result<Vec<mycelium_api::Route>, reqwest::Error> {
    let request_url = format!("http://{server_addr}/api/v1/admin/routes/fallback");
    match reqwest::get(&request_url).await {
        Err(e) => Err(e),
        Ok(resp) => match resp.json::<Vec<mycelium_api::Route>>().await {
            Err(e) => Err(e),
            Ok(selected_routes) => Ok(selected_routes),
        },
    }
}

pub async fn get_node_info(server_addr: SocketAddr) -> Result<mycelium_api::Info, reqwest::Error> {
    let request_url = format!("http://{server_addr}/api/v1/admin");
    match reqwest::get(&request_url).await {
        Err(e) => Err(e),
        Ok(resp) => match resp.json::<mycelium_api::Info>().await {
            Err(e) => Err(e),
            Ok(node_info) => Ok(node_info),
        },
    }
}

pub async fn remove_peer(
    server_addr: SocketAddr,
    peer_endpoint: Endpoint,
) -> Result<(), reqwest::Error> {
    let full_endpoint = format!(
        "{}://{}",
        peer_endpoint.proto().to_string().to_lowercase(),
        peer_endpoint.address()
    );
    let encoded_full_endpoint = encode(&full_endpoint);
    let request_url = format!(
        "http://{}/api/v1/admin/peers/{}",
        server_addr, encoded_full_endpoint
    );

    let client = reqwest::Client::new();
    client
        .delete(request_url)
        .send()
        .await?
        .error_for_status()?;

    Ok(())
}

pub async fn add_peer(
    server_addr: SocketAddr,
    peer_endpoint: String,
) -> Result<(), reqwest::Error> {
    println!("adding peer: {peer_endpoint}");
    let client = reqwest::Client::new();
    let request_url = format!("http://{server_addr}/api/v1/admin/peers");
    client
        .post(request_url)
        .json(&AddPeer {
            endpoint: peer_endpoint,
        })
        .send()
        .await?
        .error_for_status()?;

    Ok(())
}
