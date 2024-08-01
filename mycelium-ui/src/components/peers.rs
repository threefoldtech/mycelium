use crate::get_sort_indicator;
use crate::{api, SearchState, ServerAddress, SortDirection};
use dioxus::prelude::*;
use dioxus_charts::LineChart;
use human_bytes::human_bytes;
use mycelium::peer_manager::{PeerStats, PeerType};
use std::{
    cmp::Ordering,
    collections::{HashSet, VecDeque},
};

const REFRESH_RATE_MS: u64 = 500;

#[component]
pub fn Peers() -> Element {
    let server_addr = use_context::<Signal<ServerAddress>>().read().0;
    let peers = use_signal(Vec::new);
    let mut error = use_signal(|| None::<String>);

    let _: Coroutine<()> = use_coroutine(|_rx: UnboundedReceiver<_>| {
        to_owned![peers, server_addr];
        async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(REFRESH_RATE_MS)).await;
                match api::get_peers(server_addr).await {
                    Ok(fetched_peers) => {
                        println!("fetching them peers bro");
                        peers.set(fetched_peers);
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
            }
        }
    });

    rsx! {
        if let Some(err) = error.read().as_ref() {
            div { class: "error-message", "{err}" }
        } else {
             PeersTable { peers: peers }
        }
    }
}

#[component]
fn PeersTable(peers: Signal<Vec<PeerStats>>) -> Element {
    let mut current_page = use_signal(|| 0);
    let items_per_page = 20;
    let mut sort_column = use_signal(|| "Protocol".to_string());
    let mut sort_direction = use_signal(|| SortDirection::Ascending);
    let peers_len = peers.read().len();

    let mut change_page = move |delta: i32| {
        let cur_page = *current_page.read() as i32;
        current_page.set(
            (cur_page + delta)
                .max(0)
                .min((peers_len - 1) as i32 / items_per_page as i32) as usize,
        );
    };

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
        let mut sorted = peers.read().to_vec();
        sort_peers(&mut sorted, &sort_column.read(), &sort_direction.read());
        sorted
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
    let end = (start + items_per_page).min(peers_len);
    let current_peers = filtered_peers.read()[start..end].to_vec();

    rsx! {
        div { class: "peers-table",
            h2 { "Peers" }
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
                                let is_expanded = expanded_rows.read().0.contains(&peer.endpoint.to_string());
                                let mut peer_signal = use_signal(|| peer.clone());
                                // Update the peer_signal when the peers are fetched
                                peer_signal.set(peer.clone());
                                if is_expanded {
                                    rsx! {
                                        ExpandedPeerRow {
                                            peer: peer_signal,
                                            on_close: move |_| toggle_row_expansion(peer.endpoint.to_string())
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
                    disabled: (current_page + 1) * items_per_page >= peers_len,
                    onclick: move |_| change_page(1),
                    "Next"
                }
            }
        }
    }
}

#[component]
fn ExpandedPeerRow(
    peer: Signal<mycelium::peer_manager::PeerStats>,
    on_close: EventHandler<()>,
) -> Element {
    let tx_data_deque: VecDeque<f32> = VecDeque::with_capacity(20);
    let rx_data_deque: VecDeque<f32> = VecDeque::with_capacity(20);
    let mut tx_chart_data = use_signal(|| tx_data_deque);
    let mut rx_chart_data = use_signal(|| rx_data_deque);

    let mut prev_tx = use_signal(|| 0f64);
    let mut prev_rx = use_signal(|| 0f64);

    use_effect(move || {
        println!("yeah...");
        let current_tx = peer.read().tx_bytes as f64;
        let current_rx = peer.read().rx_bytes as f64;

        // Calculate data flow per second
        let tx_per_second = (current_tx - *prev_tx.read()) as u64 * (1000 / REFRESH_RATE_MS);
        let rx_per_second = (current_rx - *prev_rx.read()) as u64 * (1000 / REFRESH_RATE_MS);

        tx_chart_data.with_mut(|tx_data| {
            if tx_data.len() >= 20 {
                tx_data.pop_front();
            }
            tx_data.push_back(tx_per_second as f32);
        });

        rx_chart_data.with_mut(|rx_data| {
            if rx_data.len() >= 20 {
                rx_data.pop_front();
            }
            rx_data.push_back(rx_per_second as f32);
        });
    });
    // Causes a circular dependency which causes the program to become unresponsive
    // prev_tx.set(peer.read().tx_bytes as f64);
    // prev_rx.set(peer.read().rx_bytes as f64);

    rsx! {
        tr { class: "expanded-row",
            td { colspan: "7",
                div { class: "expanded-content",
                    // Tx chart
                    LineChart {
                        show_grid: false,
                        padding_top: 80,
                        padding_left: 100,
                        padding_right: 80,
                        padding_bottom: 80,
                        label_interpolation: (|v| human_bytes(v as f64).to_string()) as fn(f32)-> String,
                        series: vec![tx_chart_data.read().iter().cloned().collect::<Vec<f32>>()],
                        labels: (0..tx_chart_data.read().len()).map(|i| i.to_string()).collect::<Vec<String>>(),
                        series_labels: vec!["Tx Bytes".into()],
                    }
                    // Rx chart
                    LineChart {
                        show_grid: false,
                        padding_top: 80,
                        padding_left: 100,
                        padding_right: 80,
                        padding_bottom: 80,
                        label_interpolation: (|v| human_bytes(v as f64).to_string()) as fn(f32)-> String,
                        series: vec![rx_chart_data.read().iter().cloned().collect::<Vec<f32>>()],
                        labels: (0..rx_chart_data.read().len()).map(|i| i.to_string()).collect::<Vec<String>>(),
                        series_labels: vec!["Rx Bytes".into()],
                    }
                    // p { "Tx bytes: {human_bytes(peer.read().tx_bytes as f64)}" }
                    // p { "Rx bytes: {human_bytes(peer.read().rx_bytes as f64)}" }
                    button { class: "close-button",
                        onclick: move |_| on_close.call(()),
                        "Close"
                    }
                }
            }
        }
    }
}
#[derive(Clone, PartialEq)]
// Rows that have been expaneded out to show additional information
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
