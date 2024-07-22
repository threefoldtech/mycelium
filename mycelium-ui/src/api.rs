use std::net::SocketAddr;

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
