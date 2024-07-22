#![allow(non_snake_case)]

use dioxus::prelude::*;
use mycelium::peer_manager::PeerType;
use std::cmp::Ordering;
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
    let fetched_node_info = use_resource(move || get_node_info(SERVER_ADDR));
    match &*fetched_node_info.read_unchecked() {
        Some(Ok(info)) => rsx! {
            div { class: "node-info",
                h3 { "Node subnet: {info.node_subnet}" }
                h3 { "Node public key: {info.node_pubkey}" }
            }
            Outlet::<Route> {}
        },
        Some(Err(e)) => rsx! { div { "An error has occurred while fetching peers: {e}" }},
        None => rsx! { div { "Loading peers..." }},
    }
}

#[component]
fn Peers() -> Element {
    rsx! {
        div { class: "peers",
            h2 { "Peers" }
            { PeersTable() }
        }
        Outlet::<Route> {}
    }
}

#[component]
fn Routes() -> Element {
    rsx! {
        SelectedRoutesTable {}
        div { class: "fallback-routes",
            h2 { "Fallback Routes" }
            { FallbackRoutesTable() }
        }
        // FallbackRoutesTable {}
    }
}

pub struct PeerTypeWrapper(pub mycelium::peer_manager::PeerType);
impl Ord for PeerTypeWrapper {
    fn cmp(&self, other: &Self) -> Ordering {
        match (&self.0, &other.0) {
            (PeerType::Static, PeerType::Static) => Ordering::Equal,
            (PeerType::Static, _) => Ordering::Less,
            (PeerType::LinkLocalDiscovery, PeerType::Static) => Ordering::Greater,
            (PeerType::LinkLocalDiscovery, PeerType::LinkLocalDiscovery) => Ordering::Equal,
            (PeerType::LinkLocalDiscovery, PeerType::Inbound) => Ordering::Less,
            (PeerType::Inbound, PeerType::Inbound) => Ordering::Equal,
            (PeerType::Inbound, _) => Ordering::Greater,
        }
    }
}

impl PartialOrd for PeerTypeWrapper {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for PeerTypeWrapper {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for PeerTypeWrapper {}

#[derive(Clone)]
enum SortDirection {
    Ascending,
    Descending,
}

#[component]
fn PeersTable() -> Element {
    let fetched_peers = use_resource(move || get_peers(SERVER_ADDR));
    let mut current_page = use_signal(|| 0);
    let items_per_page = 20;

    let mut change_page = move |delta: i32| {
        current_page.set(current_page + delta as usize);
    };

    // Sorting
    let mut sort_column = use_signal(|| "Type".to_string());
    let mut sort_direction = use_signal(|| SortDirection::Ascending);

    let sort_peers = move |column: String| {
        if column == *sort_column.read() {
            let new_sort_direction = match *sort_direction.read() {
                SortDirection::Ascending => SortDirection::Descending,
                SortDirection::Descending => SortDirection::Ascending,
            };
            sort_direction.set(new_sort_direction);
        } else {
            sort_column.set(column);
            sort_direction.set(SortDirection::Ascending);
        }
    };

    match &*fetched_peers.read_unchecked() {
        Some(Ok(peers)) => {
            // Sort peers based on the current sort column and sort direction
            let mut sorted_peers = peers.clone();
            sorted_peers.sort_by(|a, b| {
                let cmp = match sort_column.read().as_str() {
                    // "Endpoint" => a.endpoint.cmp(&b.endpoint),
                    "Type" => PeerTypeWrapper(a.pt.clone()).cmp(&PeerTypeWrapper(b.pt.clone())),
                    _ => std::cmp::Ordering::Equal,
                };
                match *sort_direction.read() {
                    SortDirection::Descending => cmp.reverse(),
                    SortDirection::Ascending => cmp,
                }
            });

            let start = current_page * items_per_page;
            let end = (start + items_per_page).min(peers.len());
            let current_peers = &sorted_peers[start..end];
            rsx! {
                {create_peer_table(current_peers, sort_column, sort_direction, sort_peers)}
                div { class: "pagination",
                    button {
                        disabled: *current_page.read() == 0,
                        onclick: move |_| change_page(-1),
                        "Previous"
                    }
                    span { "Page {current_page + 1}"}
                    button {
                        disabled: (current_page + 1) * items_per_page >= peers.len(),
                        onclick: move |_| change_page(1),
                        "Next"
                    }
                }

            }
        }
        Some(Err(e)) => rsx! { div { "An error has occurred while fetching peers: {e}" }},
        None => rsx! { div { "Loading peers..." }},
    }
}

#[component]
fn SelectedRoutesTable() -> Element {
    let fetched_selected_routes = use_resource(move || get_selected_routes(SERVER_ADDR));

    match &*fetched_selected_routes.read_unchecked() {
        Some(Ok(routes)) => {
            rsx! { { RoutesTable(routes.clone(), "Selected".to_string())  } }
        }
        Some(Err(e)) => rsx! { div { "An error has occurred while fetching selected routes: {e}" }},
        None => rsx! { div { "Loading selected routes..." }},
    }
}

#[component]
fn FallbackRoutesTable() -> Element {
    let fetched_fallback_routes = use_resource(move || get_selected_routes(SERVER_ADDR));

    match &*fetched_fallback_routes.read_unchecked() {
        Some(Ok(routes)) => {
            rsx! { { RoutesTable(routes.clone(), "Fallback".to_string()) } }
        }
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

fn get_sort_indicator(
    sort_column: Signal<String>,
    sort_direction: Signal<SortDirection>,
    column: String,
) -> String {
    if *sort_column.read() == column {
        match *sort_direction.read() {
            SortDirection::Ascending => " ↑".to_string(),
            SortDirection::Descending => " ↓".to_string(),
        }
    } else {
        "".to_string()
    }
}

fn create_peer_table(
    peers: &[mycelium::peer_manager::PeerStats],
    sort_column: Signal<String>,
    sort_direction: Signal<SortDirection>,
    mut sort_peers: impl FnMut(String) + Clone + 'static,
) -> Element {
    rsx! {
        table {
            thead {
                tr {
                    // th { "Endpoint" }
                    th {
                        onclick: {
                            let mut sort_peers = sort_peers.clone();
                            move |_| {
                                sort_peers("Endpoint".to_string());
                            }
                        },
                        "Endpoint {get_sort_indicator(sort_column, sort_direction, \"Endpoint\".to_string())}"
                    }
                    th {
                        onclick: move |_| {
                            sort_peers("Type".to_string());
                        },
                        "Type {get_sort_indicator(sort_column, sort_direction, \"Type\".to_string())}"
                    }
                    // th { "Type" }
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

fn sort_routes(routes: &mut [mycelium_api::Route], column: &str, direction: &SortDirection) {
    routes.sort_by(|a, b| {
        let cmp = match column {
            "Subnet" => a.subnet.cmp(&b.subnet),
            "Next-hop" => a.next_hop.cmp(&b.next_hop),
            "Metric" => a.metric.cmp(&b.metric),
            "Seqno" => a.seqno.cmp(&b.seqno),
            _ => Ordering::Equal,
        };
        match direction {
            SortDirection::Ascending => cmp,
            SortDirection::Descending => cmp.reverse(),
        }
    });
}

fn RoutesTable(routes: Vec<mycelium_api::Route>, table_name: String) -> Element {
    let mut current_page = use_signal(|| 0);
    let items_per_page = 20;
    let mut sort_column = use_signal(|| "Subnet".to_string());
    let mut sort_direction = use_signal(|| SortDirection::Descending);
    let routes_len = routes.len();

    let mut change_page = move |delta: i32| {
        let cur_page = *current_page.read() as i32;
        current_page.set(
            (cur_page + delta)
                .max(0)
                .min((routes_len - 1) as i32 / items_per_page as i32) as usize,
        );
    };

    let mut sort_routes_signal = move |column: String| {
        if column == *sort_column.read() {
            let new_sort_direction = match *sort_direction.read() {
                SortDirection::Ascending => SortDirection::Descending,
                SortDirection::Descending => SortDirection::Ascending,
            };
            sort_direction.set(new_sort_direction);
        } else {
            sort_column.set(column);
            sort_direction.set(SortDirection::Ascending);
        }
    };

    let sorted_routes = use_memo(move || {
        let mut sorted = routes.clone();
        sort_routes(&mut sorted, &sort_column.read(), &sort_direction.read());
        sorted
    });

    let start = current_page * items_per_page;
    let end = (start + items_per_page).min(routes_len);
    let current_routes = &sorted_routes.read()[start..end];

    rsx! {
        div { class: "{table_name.to_lowercase()}-routes",
            h2 { "{table_name} Routes" }
            table {
                thead {
                    tr {
                        th {
                            onclick: move |_| sort_routes_signal("Subnet".to_string()),
                            "Subnet {get_sort_indicator(sort_column, sort_direction, \"Subnet\".to_string())}"
                        }
                        th {
                            onclick: move |_| sort_routes_signal("Next-hop".to_string()),
                            "Next-hop {get_sort_indicator(sort_column, sort_direction, \"Next-hop\".to_string())}"
                        }
                        th {
                            onclick: move |_| sort_routes_signal("Metric".to_string()),
                            "Metric {get_sort_indicator(sort_column, sort_direction, \"Metric\".to_string())}"
                        }
                        th {
                            onclick: move |_| sort_routes_signal("Seqno".to_string()),
                            "Seqno {get_sort_indicator(sort_column, sort_direction, \"Seqno\".to_string())}"
                        }
                    }
                }
                tbody {
                    for route in current_routes {
                        tr {
                            td { "{route.subnet}" }
                            td { "{route.next_hop}" }
                            td { "{route.metric}" }
                            td { "{route.seqno}" }
                        }
                    }
                }
            }
            div { class: "pagination",
                button {
                    disabled: *current_page.read() == 0,
                    onclick: move |_| change_page(-1),
                    "Previous"
                }
                span { "Page {current_page + 1}" }
                button {
                    disabled: (current_page + 1) * items_per_page >= routes_len,
                    onclick: move |_| change_page(1),
                    "Next"
                }
            }
        }
    }
}
