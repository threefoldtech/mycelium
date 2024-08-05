#![allow(non_snake_case)]

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

const _: &str = manganis::mg!(file("assets/styles.css"));

const DEFAULT_SERVER_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8989);

fn main() {
    // Init logger
    dioxus_logger::init(tracing::Level::INFO).expect("failed to init logger");
    dioxus::launch(App);
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

    rsx! {
        Router::<Route> {}
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

enum AppError {
    NetworkError(reqwest::Error), // reqwest errors
    AddressError(String),         // address parsing errors
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::NetworkError(e) => write!(f, "Network error: {}", e),
            AppError::AddressError(e) => write!(f, "Address error: {}", e),
        }
    }
}

impl From<reqwest::Error> for AppError {
    fn from(value: reqwest::Error) -> Self {
        AppError::NetworkError(value)
    }
}
