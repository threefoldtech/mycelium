#![allow(non_snake_case)]
// Disable terminal popup on Windows
#![cfg_attr(feature = "bundle", windows_subsystem = "windows")]

mod api;
mod components;

use components::home::Home;
use components::peers::Peers;
use components::routes::Routes;

use dioxus::prelude::*;
use mycelium::{endpoint::Endpoint, peer_manager::PeerStats};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

const _: manganis::Asset = manganis::asset!("assets/styles.css");

const DEFAULT_SERVER_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8989);

fn main() {
    // Init logger
    dioxus_logger::init(tracing::Level::INFO).expect("failed to init logger");

    let config = dioxus::desktop::Config::new()
        .with_custom_head(r#"<link rel="stylesheet" href="styles.css">"#.to_string());
    LaunchBuilder::desktop().with_cfg(config).launch(App);
    // dioxus::launch(App);
}

#[component]
fn App() -> Element {
    use_context_provider(|| Signal::new(ServerAddress(DEFAULT_SERVER_ADDR)));
    use_context_provider(|| Signal::new(ServerConnected(false)));
    use_context_provider(|| {
        Signal::new(PeerSignalMapping(
            HashMap::<Endpoint, Signal<PeerStats>>::new(),
        ))
    });
    use_context_provider(|| Signal::new(StopFetchingPeerSignal(false)));

    rsx! {
        Router::<Route> {
            config: || {
                RouterConfig::default().on_update(|state| {
                    use_context::<Signal<StopFetchingPeerSignal>>().write().0 = state.current() != Route::Peers {};
                    (state.current() == Route::Peers {}).then_some(NavigationTarget::Internal(Route::Peers {}))
                })
            }
        }
    }
}

#[derive(Clone, Routable, Debug, PartialEq)]
#[rustfmt::skip]
pub enum Route {
    #[layout(components::layout::Layout)]
        #[route("/")]
        Home {},
        #[route("/peers")]
        Peers,
        #[route("/routes")]
        Routes,
    #[end_layout]
    #[route("/:..route")]
    PageNotFound { route: Vec<String> },
}
//
#[derive(Clone, PartialEq)]
struct SearchState {
    query: String,
    column: String,
}

// This signal is used to stop the loop that keeps fetching information about the peers when
// looking at the peers table, e.g. when the user goes back to Home or Routes page.
#[derive(Clone, PartialEq)]
struct StopFetchingPeerSignal(bool);

#[derive(Clone, PartialEq)]
struct ServerAddress(SocketAddr);

#[derive(Clone, PartialEq)]
struct ServerConnected(bool);

#[derive(Clone, PartialEq)]
struct PeerSignalMapping(HashMap<Endpoint, Signal<PeerStats>>);

pub fn get_sort_indicator(
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

#[component]
fn PageNotFound(route: Vec<String>) -> Element {
    rsx! {
        p { "Page not found"}
    }
}

#[derive(Clone)]
pub enum SortDirection {
    Ascending,
    Descending,
}
