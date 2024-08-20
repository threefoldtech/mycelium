use crate::get_sort_indicator;
use crate::{api, SearchState, ServerAddress, SortDirection, StopFetchingPeerSignal};
use dioxus::prelude::*;
use dioxus_charts::LineChart;
use human_bytes::human_bytes;
use mycelium::{
    endpoint::Endpoint,
    peer_manager::{PeerStats, PeerType},
};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    str::FromStr,
};
use tracing::{error, info};

const REFRESH_RATE_MS: u64 = 500;
const MAX_DATA_POINTS: usize = 8; // displays last 4 seconds

#[component]
pub fn Peers() -> Element {
    let server_addr = use_context::<Signal<ServerAddress>>().read().0;
    let error = use_signal(|| None::<String>);
    let peer_data = use_signal(HashMap::<Endpoint, PeerStats>::new);

    let _ = use_resource(move || async move {
        to_owned![server_addr, error, peer_data];
        loop {
            let stop_fetching_signal = use_context::<Signal<StopFetchingPeerSignal>>().read().0;
            if !stop_fetching_signal {
                match api::get_peers(server_addr).await {
                    Ok(fetched_peers) => {
                        peer_data.with_mut(|data| {
                            // Collect the endpoint from the fetched peers
                            let fetched_endpoints: HashSet<Endpoint> =
                                fetched_peers.iter().map(|peer| peer.endpoint).collect();

                            // Remove peers that are no longer in the fetched data
                            data.retain(|endpoint, _| fetched_endpoints.contains(endpoint));

                            // Insert or update the fetched peers
                            for peer in fetched_peers {
                                data.insert(peer.endpoint, peer);
                            }
                        });
                        error.set(None);
                    }
                    Err(e) => {
                        eprintln!("Error fetching peers: {}", e);
                        error.set(Some(format!(
                            "An error has occurred while fetching peers: {}",
                            e
                        )))
                    }
                }
            } else {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(REFRESH_RATE_MS)).await;
        }
    });

    rsx! {
        if let Some(err) = error.read().as_ref() {
            div { class: "error-message", "{err}" }
        } else {
            PeersTable { peer_data: peer_data }
        }
    }
}

#[component]
fn PeersTable(peer_data: Signal<HashMap<Endpoint, PeerStats>>) -> Element {
    let mut current_page = use_signal(|| 0);
    let items_per_page = 20;
    let mut sort_column = use_signal(|| "Protocol".to_string());
    let mut sort_direction = use_signal(|| SortDirection::Ascending);
    let peers_len = peer_data.read().len();

    // Pagination
    let mut change_page = move |delta: i32| {
        let cur_page = *current_page.read() as i32;
        current_page.set(
            (cur_page + delta)
                .max(0)
                .min((peers_len - 1) as i32 / items_per_page),
        );
    };

    // Sorting
    let mut sort_peers_signal = move |column: String| {
        if column == *sort_column.read() {
            let new_sort_direction = match *sort_direction.read() {
                SortDirection::Ascending => SortDirection::Descending,
                SortDirection::Descending => SortDirection::Ascending,
            };
            sort_direction.set(new_sort_direction);
        } else {
            sort_column.set(column);
            sort_direction.set(SortDirection::Descending);
        }
        // When sorting, we should jump back to the first page
        current_page.set(0);
    };

    let sorted_peers = use_memo(move || {
        let mut peers = peer_data.read().values().cloned().collect::<Vec<_>>();
        sort_peers(&mut peers, &sort_column.read(), &sort_direction.read());
        peers
    });

    // Searching
    let mut search_state = use_signal(|| SearchState {
        query: String::new(),
        column: "Protocol".to_string(),
    });

    let filtered_peers = use_memo(move || {
        let query = search_state.read().query.to_lowercase();
        let column = &search_state.read().column;
        let sorted_peers = sorted_peers.read();
        sorted_peers
            .iter()
            .filter(|peer| match column.as_str() {
                "Protocol" => peer
                    .endpoint
                    .proto()
                    .to_string()
                    .to_lowercase()
                    .contains(&query),
                "Address" => peer
                    .endpoint
                    .address()
                    .ip()
                    .to_string()
                    .to_lowercase()
                    .contains(&query),
                "Port" => peer
                    .endpoint
                    .address()
                    .port()
                    .to_string()
                    .to_lowercase()
                    .contains(&query),
                "Type" => peer.pt.to_string().to_lowercase().contains(&query),
                "Connection State" => peer
                    .connection_state
                    .to_string()
                    .to_lowercase()
                    .contains(&query),
                "Tx bytes" => peer.tx_bytes.to_string().to_lowercase().contains(&query),
                "Rx bytes" => peer.rx_bytes.to_string().to_lowercase().contains(&query),
                _ => false,
            })
            .cloned()
            .collect::<Vec<PeerStats>>()
    });

    let peers_len = filtered_peers.read().len();
    let start = current_page * items_per_page;
    let end = (start + items_per_page).min(peers_len as i32);
    let current_peers = filtered_peers.read()[start as usize..end as usize].to_vec();

    // Expanding peer to show rx/tx bytes graphs
    let mut expanded_rows = use_signal(|| ExpandedRows(HashSet::new()));
    let mut toggle_row_expansion = move |peer_endpoint: String| {
        expanded_rows.with_mut(|rows| {
            if rows.0.contains(&peer_endpoint) {
                rows.0.remove(&peer_endpoint);
            } else {
                rows.0.insert(peer_endpoint);
            }
        });
    };

    let toggle_add_peer_input = use_signal(|| true); //TODO: fix UX for adding peer
    let mut add_peer_error = use_signal(|| None::<String>);
    let add_peer = move |peer_endpoint: String| {
        spawn(async move {
            let server_addr = use_context::<Signal<ServerAddress>>().read().0;
            // Check correct endpoint format and add peer
            match Endpoint::from_str(&peer_endpoint) {
                Ok(_) => {
                    if let Err(e) = api::add_peer(server_addr, peer_endpoint.clone()).await {
                        error!("Error adding peer: {e}");
                        add_peer_error.set(Some(format!("Error adding peer: {}", e)));
                    } else {
                        info!("Succesfully added peer: {peer_endpoint}");
                    }
                }
                Err(e) => {
                    error!("Incorrect peer endpoint: {e}");
                    add_peer_error.set(Some(format!("Incorrect peer endpoint: {}", e)));
                }
            }
        });
    };
    let mut new_peer_endpoint = use_signal(|| "".to_string());

    rsx! {
        div { class: "peers-table",
            h2 { "Peers" }
            div { class: "search-and-add-container",
                div { class: "search-container",
                    input {
                        placeholder: "Search...",
                        value: "{search_state.read().query}",
                        oninput: move |evt| search_state.write().query.clone_from(&evt.value()),
                    }
                    select {
                        value: "{search_state.read().column}",
                        onchange: move |evt| search_state.write().column.clone_from(&evt.value()),
                        option { value: "Protocol", "Protocol" }
                        option { value: "Address", "Address" }
                        option { value: "Port", "Port" }
                        option { value: "Type", "Type" }
                        option { value: "Connection State", "Connection State" }
                        option { value: "Tx bytes", "Tx bytes" }
                        option { value: "Rx bytes", "Rx bytes" }
                    }
                }
                div { class: "add-peer-container",
                    div { class: "add-peer-input-button",
                        if *toggle_add_peer_input.read() {
                            div { class: "expanded-add-peer-container",
                                input {
                                    placeholder: "tcp://ipaddr:port",
                                    oninput: move |evt| new_peer_endpoint.set(evt.value())
                                }
                            }
                        }
                        button {
                            onclick: move |_| add_peer(new_peer_endpoint.read().to_string()),
                            "Add peer"
                        }
                    }
                    if let Some(error) = add_peer_error.read().as_ref() {
                        div { class: "add-peer-error", "{error}" }
                    }
                }
            }
            div { class: "table-container",
                table {
                    thead {
                        tr {
                            th { class: "protocol-column",
                                onclick: move |_| sort_peers_signal("Protocol".to_string()),
                                "Protocol {get_sort_indicator(sort_column, sort_direction, \"Protocol\".to_string())}"
                            }
                            th { class: "address-column",
                                onclick: move |_| sort_peers_signal("Address".to_string()),
                                "Address {get_sort_indicator(sort_column, sort_direction, \"Address\".to_string())}"
                            }
                            th { class: "port-column",
                                onclick: move |_| sort_peers_signal("Port".to_string()),
                                "Port {get_sort_indicator(sort_column, sort_direction, \"Port\".to_string())}"
                            }
                            th { class: "type-column",
                                onclick: move |_| sort_peers_signal("Type".to_string()),
                                "Type {get_sort_indicator(sort_column, sort_direction, \"Type\".to_string())}"
                            }
                            th { class: "connection-state-column",
                                onclick: move |_| sort_peers_signal("Connection State".to_string()),
                                "Connection State {get_sort_indicator(sort_column, sort_direction, \"Connection State\".to_string())}"
                            }
                            th { class: "tx-bytes-column",
                                onclick: move |_| sort_peers_signal("Tx bytes".to_string()),
                                "Tx bytes {get_sort_indicator(sort_column, sort_direction, \"Tx bytes\".to_string())}"
                            }
                            th { class: "rx-bytes-column",
                                onclick: move |_| sort_peers_signal("Rx bytes".to_string()),
                                "Rx bytes {get_sort_indicator(sort_column, sort_direction, \"Rx bytes\".to_string())}"
                            }
                        }
                    }
                    tbody {
                        for peer in current_peers.into_iter() {
                            tr {
                                onclick: move |_| toggle_row_expansion(peer.endpoint.to_string()),
                                td { class: "protocol-column", "{peer.endpoint.proto()}" }
                                td { class: "address-column", "{peer.endpoint.address().ip()}" }
                                td { class: "port-column", "{peer.endpoint.address().port()}" }
                                td { class: "type-column", "{peer.pt}" }
                                td { class: "connection-state-column", "{peer.connection_state}" }
                                td { class: "tx-bytes-column", "{human_bytes(peer.tx_bytes as f64)}" }
                                td { class: "rx-bytes-column", "{human_bytes(peer.rx_bytes as f64)}" }
                            }
                            {
                                let peer_expanded = expanded_rows.read().0.contains(&peer.endpoint.to_string());
                                if peer_expanded {
                                    rsx! {
                                        ExpandedPeerRow {
                                            peer_endpoint: peer.endpoint,
                                            peer_data: peer_data,
                                            on_close: move |_| toggle_row_expansion(peer.endpoint.to_string()),
                                        }
                                    }
                                } else {
                                    rsx! {}
                                }
                            }
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
                    disabled: (current_page + 1) * items_per_page >= peers_len as i32,
                    onclick: move |_| change_page(1),
                    "Next"
                }
            }
        }
    }
}

#[derive(Clone)]
struct BandwidthData {
    tx_bytes: u64,
    rx_bytes: u64,
    timestamp: tokio::time::Duration,
}

#[component]
fn ExpandedPeerRow(
    peer_endpoint: Endpoint,
    peer_data: Signal<HashMap<Endpoint, PeerStats>>,
    on_close: EventHandler<()>,
) -> Element {
    let bandwidth_data = use_signal(VecDeque::<BandwidthData>::new);
    let start_time = use_signal(tokio::time::Instant::now);

    use_future(move || {
        to_owned![bandwidth_data, start_time, peer_data, peer_endpoint];
        async move {
            let mut last_tx = 0;
            let mut last_rx = 0;
            if let Some(peer_stats) = peer_data.read().get(&peer_endpoint) {
                last_tx = peer_stats.tx_bytes;
                last_rx = peer_stats.rx_bytes;
            }

            loop {
                let current_time = tokio::time::Instant::now();
                let elapsed_time = current_time.duration_since(*start_time.read());

                if let Some(peer_stats) = peer_data.read().get(&peer_endpoint) {
                    let tx_rate =
                        (peer_stats.tx_bytes - last_tx) as f64 / (REFRESH_RATE_MS as f64 / 1000.0);
                    let rx_rate =
                        (peer_stats.rx_bytes - last_rx) as f64 / (REFRESH_RATE_MS as f64 / 1000.0);

                    bandwidth_data.with_mut(|data| {
                        let new_data = BandwidthData {
                            tx_bytes: tx_rate as u64,
                            rx_bytes: rx_rate as u64,
                            timestamp: elapsed_time,
                        };
                        data.push_back(new_data);

                        if data.len() > MAX_DATA_POINTS {
                            data.pop_front();
                        }
                    });

                    last_tx = peer_stats.tx_bytes;
                    last_rx = peer_stats.rx_bytes;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(REFRESH_RATE_MS)).await;
            }
        }
    });

    let tx_data = use_memo(move || {
        bandwidth_data
            .read()
            .iter()
            .map(|d| d.tx_bytes as f32)
            .collect::<Vec<f32>>()
    });

    let rx_data = use_memo(move || {
        bandwidth_data
            .read()
            .iter()
            .map(|d| d.rx_bytes as f32)
            .collect::<Vec<f32>>()
    });
    let labels = bandwidth_data
        .read()
        .iter()
        .map(|d| format!("{:.1}", d.timestamp.as_secs_f64()))
        .collect::<Vec<String>>();

    let remove_peer = move |_| {
        spawn(async move {
            println!("Removing peer: {}", peer_endpoint);
            let server_addr = use_context::<Signal<ServerAddress>>().read().0;
            match api::remove_peer(server_addr, peer_endpoint).await {
                Ok(_) => on_close.call(()),
                Err(e) => eprintln!("Error removing peer: {e}"),
            }
        });
    };

    rsx! {
        tr { class: "expanded-row",
            td { colspan: "7",
                div { class: "expanded-content",
                    div { class: "graph-container",
                        // Tx chart
                        div { class: "graph-title", "Tx Bytes/s" }
                        LineChart {
                            show_grid: false,
                            show_dots: false,
                            padding_top: 80,
                            padding_left: 100,
                            padding_right: 80,
                            padding_bottom: 80,
                            label_interpolation: (|v| human_bytes(v as f64).to_string()) as fn(f32)-> String,
                            series: vec![tx_data.read().to_vec()],
                            labels: labels.clone(),
                            series_labels: vec!["Tx Bytes/s".into()],
                        }
                    }
                    div { class: "graph-container",
                        // Rx chart
                        div { class: "graph-title", "Rx Bytes/s" }
                        LineChart {
                            show_grid: false,
                            show_dots: false,
                            padding_top: 80,
                            padding_left: 100,
                            padding_right: 80,
                            padding_bottom: 80,
                            label_interpolation: (|v| human_bytes(v as f64).to_string()) as fn(f32)-> String,
                            series: vec![rx_data.read().clone()],
                            labels: labels.clone(),
                            series_labels: vec!["Rx Bytes/s".into()],
                        }
                    }
                    div { class: "button-container",
                        button { class: "close-button",
                            onclick: move |_| on_close.call(()),
                            "Close"
                        }
                        button { class: "remove-button",
                            onclick: remove_peer,
                            "Remove peer"
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone, PartialEq)]
struct ExpandedRows(HashSet<String>);

fn sort_peers(
    peers: &mut [mycelium::peer_manager::PeerStats],
    column: &str,
    direction: &SortDirection,
) {
    peers.sort_by(|a, b| {
        let cmp = match column {
            "Protocol" => a.endpoint.proto().cmp(&b.endpoint.proto()),
            "Address" => a.endpoint.address().ip().cmp(&b.endpoint.address().ip()),
            "Port" => a
                .endpoint
                .address()
                .port()
                .cmp(&b.endpoint.address().port()),
            "Type" => PeerTypeWrapper(a.pt.clone()).cmp(&PeerTypeWrapper(b.pt.clone())),
            "Connection State" => a.connection_state.cmp(&b.connection_state),
            "Tx bytes" => a.tx_bytes.cmp(&b.tx_bytes),
            "Rx bytes" => a.rx_bytes.cmp(&b.rx_bytes),
            _ => Ordering::Equal,
        };
        match direction {
            SortDirection::Ascending => cmp,
            SortDirection::Descending => cmp.reverse(),
        }
    });
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
