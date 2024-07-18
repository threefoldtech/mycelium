#![allow(non_snake_case)]

use dioxus::prelude::*;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tracing::Level;

const SERVER_ADDR: std::net::SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8989);

#[derive(Clone, Routable, Debug, PartialEq)]
#[rustfmt::skip]
enum Route {
    #[layout(Sidebar)]
        #[route("/")]
        Home {},
        #[route("/peers")]
        Peers {},
        #[route("/routes")]
        Routes {},
    #[end_layout]
    #[route("/..route")]
    PageNotFound { route: Vec<String> },
}

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

pub async fn get_node_info(server_addr: SocketAddr) -> Result<(), reqwest::Error> {
    todo!()
}

fn main() {
    // Init logger
    dioxus_logger::init(Level::INFO).expect("failed to init logger");
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        div { class: "app-container",
            Header {}
            Router::<Route> {}
        }
    }
}

#[component]
fn Header() -> Element {
    rsx! {
        header {
            h1 { "Mycelium Network Dashboard" }
        }
    }
}

#[component]
fn Sidebar() -> Element {
    rsx! {
        nav { class: "sidebar",
            ul {
                li { Link { to: Route::Home {}, "Home" } }
                li { Link { to: Route::Peers {}, "Peers" } }
                li { Link { to: Route::Routes {}, "Routes" } }
            }
        }
        Outlet::<Route> {}
    }
}

#[component]
fn Home() -> Element {
    rsx! {
        div {
            h2 { "Welcome to the Mycelium Network Dashboard" }
            p { "Select an option from the sidebar to view different information." }
        }
        Outlet::<Route> {}
    }
}

#[component]
fn Peers() -> Element {
    rsx! {
        div {
            h2 { "Peers" }
            { PeersTable() }
        }
        Outlet::<Route> {}
    }
}

#[component]
fn Routes() -> Element {
    rsx! {
        div { class: "selected-routes",
            h2 { "Selected Routes" }
            { SelectedRoutesTable() }
        }
        div { class: "fallback-routes",
            h2 { "Fallback Routes" }
            { FallbackRoutesTable() }
        }
    }
}

#[component]
fn PeersTable() -> Element {
    let fetched_peers = use_resource(move || get_peers(SERVER_ADDR));

    match &*fetched_peers.read_unchecked() {
        Some(Ok(peers)) => create_peer_table(peers),
        Some(Err(e)) => rsx! { div { "An error has occurred while fetching peers: {e}" }},
        None => rsx! { div { "Loading peers..." }},
    }
}

#[component]
fn SelectedRoutesTable() -> Element {
    let fetched_selected_routes = use_resource(move || get_selected_routes(SERVER_ADDR));

    match &*fetched_selected_routes.read_unchecked() {
        Some(Ok(peers)) => create_routes_table(peers),
        Some(Err(e)) => rsx! { div { "An error has occurred while fetching selected routes: {e}" }},
        None => rsx! { div { "Loading selected routes..." }},
    }
}

#[component]
fn FallbackRoutesTable() -> Element {
    let fetched_fallback_routes = use_resource(move || get_selected_routes(SERVER_ADDR));

    match &*fetched_fallback_routes.read_unchecked() {
        Some(Ok(peers)) => create_routes_table(peers),
        Some(Err(e)) => rsx! { div { "An error has occurred while fetching fallback routes: {e}" }},
        None => rsx! { div { "Loading fallback routes..." }},
    }
}

#[component]
fn PageNotFound(route: Vec<String>) -> Element {
    rsx! {
        p { "Page not found"}
    }
}

fn create_peer_table(peers: &Vec<mycelium::peer_manager::PeerStats>) -> Element {
    rsx! {
        table {
            thead {
                tr {
                    th { "Endpoint" }
                    th { "Type" }
                    th { "Connection State" }
                    th { "Tx bytes" }
                    th { "Rx bytes" }
                }
            }
            tbody {
                for peer in peers {
                    tr {
                        td { "{peer.endpoint}" }
                        td { "{peer.pt}" }
                        td { "{peer.connection_state}" }
                        td { "{peer.tx_bytes}" }
                        td { "{peer.rx_bytes}" }
                    }
                }
            }
        }
    }
}

fn create_routes_table(routes: &Vec<mycelium_api::Route>) -> Element {
    rsx! {
        table {
            thead {
                tr {
                    th { "Subnet" }
                    th { "Next-hop" }
                    th { "Metric" }
                    th { "Seqno" }
                }
            }
            tbody {
                for route in routes {
                    tr {
                        td { "{route.subnet}" }
                        td { "{route.next_hop}" }
                        td { "{route.metric}" }
                        td { "{route.seqno}" }
                    }
                }
            }
        }
    }
}
