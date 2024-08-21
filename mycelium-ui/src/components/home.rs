use crate::api;
use crate::{ServerAddress, ServerConnected};
use dioxus::prelude::*;
use std::net::SocketAddr;
use std::str::FromStr;

#[component]
pub fn Home() -> Element {
    let mut server_addr = use_context::<Signal<ServerAddress>>();
    let mut new_address = use_signal(|| server_addr.read().0.to_string());
    let mut node_info = use_resource(fetch_node_info);

    let try_connect = move |_| {
        if let Ok(addr) = SocketAddr::from_str(&new_address.read()) {
            server_addr.write().0 = addr;
            node_info.restart();
        }
    };

    rsx! {
        div { class: "home-container",
            h2 { "Node information" }
            div { class: "server-input",
                input {
                    placeholder: "Server address (e.g. 127.0.0.1:8989)",
                    value: "{new_address}",
                    oninput: move |evt| new_address.set(evt.value().clone()),
                }
                button { onclick: try_connect, "Connect" }
            }
            {match node_info.read().as_ref() {
                Some(Ok(info)) => rsx! {
                    p {
                        "Node subnet: ",
                        span { class: "bold", "{info.node_subnet}" }
                    }
                    p {
                        "Node public key: ",
                        span { class: "bold", "{info.node_pubkey}" }
                    }
                },
                Some(Err(e)) => rsx! {
                    p { class: "error", "Error: {e}" }
                },
                None => rsx! {
                    p { "Enter a server address and click 'Connect' to fetch node information." }
                }
            }}
        }
    }
}

async fn fetch_node_info() -> Result<mycelium_api::Info, reqwest::Error> {
    let server_addr = use_context::<Signal<ServerAddress>>();
    let mut server_connected = use_context::<Signal<ServerConnected>>();
    let address = server_addr.read().0;

    match api::get_node_info(address).await {
        Ok(info) => {
            server_connected.write().0 = true;
            Ok(info)
        }
        Err(e) => {
            server_connected.write().0 = false;
            Err(e)
        }
    }
}
