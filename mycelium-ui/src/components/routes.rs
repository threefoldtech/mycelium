use std::cmp::Ordering;

use crate::api;
use crate::{get_sort_indicator, SearchState, ServerAddress, SortDirection};
use dioxus::prelude::*;

#[component]
pub fn Routes() -> Element {
    rsx! {
        SelectedRoutesTable {}
        FallbackRoutesTable {}
    }
}

#[component]
pub fn SelectedRoutesTable() -> Element {
    let server_addr = use_context::<Signal<ServerAddress>>();
    let fetched_selected_routes =
        use_resource(move || api::get_selected_routes(server_addr.read().0));

    match &*fetched_selected_routes.read_unchecked() {
        Some(Ok(routes)) => {
            rsx! { RoutesTable { routes: routes.clone(), table_name: "Selected"} }
        }
        Some(Err(e)) => rsx! { div { "An error has occurred while fetching selected routes: {e}" }},
        None => rsx! { div { "Loading selected routes..." }},
    }
}

#[component]
pub fn FallbackRoutesTable() -> Element {
    let server_addr = use_context::<Signal<ServerAddress>>();
    let fetched_fallback_routes =
        use_resource(move || api::get_fallback_routes(server_addr.read().0));

    match &*fetched_fallback_routes.read_unchecked() {
        Some(Ok(routes)) => {
            rsx! { RoutesTable { routes: routes.clone(), table_name: "Fallback"} }
        }
        Some(Err(e)) => rsx! { div { "An error has occurred while fetching fallback routes: {e}" }},
        None => rsx! { div { "Loading fallback routes..." }},
    }
}

#[component]
fn RoutesTable(routes: Vec<mycelium_api::Route>, table_name: String) -> Element {
    let mut current_page = use_signal(|| 0);
    let items_per_page = 10;
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
        current_page.set(0);
    };

    let sorted_routes = use_memo(move || {
        let mut sorted = routes.clone();
        sort_routes(&mut sorted, &sort_column.read(), &sort_direction.read());
        sorted
    });

    let mut search_state = use_signal(|| SearchState {
        query: String::new(),
        column: "Subnet".to_string(),
    });

    let filtered_routes = use_memo(move || {
        let query = search_state.read().query.to_lowercase();
        let column = &search_state.read().column;
        sorted_routes
            .read()
            .iter()
            .filter(|route| match column.as_str() {
                "Subnet" => route.subnet.to_string().to_lowercase().contains(&query),
                "Next-hop" => route.next_hop.to_string().to_lowercase().contains(&query),
                "Metric" => route.metric.to_string().to_lowercase().contains(&query),
                "Seqno" => route.seqno.to_string().to_lowercase().contains(&query),
                _ => false,
            })
            .cloned()
            .collect::<Vec<_>>()
    });

    let routes_len = filtered_routes.len();

    let start = current_page * items_per_page;
    let end = (start + items_per_page).min(routes_len);
    let current_routes = &filtered_routes.read()[start..end];

    rsx! {
        div { class: "{table_name.to_lowercase()}-routes",
            h2 { "{table_name} Routes" }
            div { class: "search-container",
                input {
                    placeholder: "Search...",
                    value: "{search_state.read().query}",
                    oninput: move |evt| search_state.write().query.clone_from(&evt.value()),
                }
                select {
                    value: "{search_state.read().column}",
                    onchange: move |evt| search_state.write().column.clone_from(&evt.value()),
                    option { value: "Subnet", "Subnet" }
                    option { value: "Next-hop", "Next-hop" }
                    option { value: "Metric", "Metric" }
                    option { value: "Seqno", "Seqno" }
                }
            }
            div { class: "table-container",
                table {
                    thead {
                        tr {
                            th { class: "subnet-column",
                                onclick: move |_| sort_routes_signal("Subnet".to_string()),
                                "Subnet {get_sort_indicator(sort_column, sort_direction, \"Subnet\".to_string())}"
                            }
                            th { class: "next-hop-column",
                                onclick: move |_| sort_routes_signal("Next-hop".to_string()),
                                "Next-hop {get_sort_indicator(sort_column, sort_direction, \"Next-hop\".to_string())}"
                            }
                            th { class: "metric-column",
                                onclick: move |_| sort_routes_signal("Metric".to_string()),
                                "Metric {get_sort_indicator(sort_column, sort_direction, \"Metric\".to_string())}"
                            }
                            th { class: "seqno_column",
                                onclick: move |_| sort_routes_signal("Seqno".to_string()),
                                "Seqno {get_sort_indicator(sort_column, sort_direction, \"Seqno\".to_string())}"
                            }
                        }
                    }
                    tbody {
                        for route in current_routes {
                            tr {
                                td { class: "subnet-column", "{route.subnet}" }
                                td { class: "next-hop-column", "{route.next_hop}" }
                                td { class: "metric-column", "{route.metric}" }
                                td { class: "seqno-column", "{route.seqno}" }
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
                    disabled: (current_page + 1) * items_per_page >= routes_len,
                    onclick: move |_| change_page(1),
                    "Next"
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
