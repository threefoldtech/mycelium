use crate::{api, Route, ServerAddress};
use dioxus::prelude::*;
use dioxus_free_icons::{icons::fa_solid_icons::FaChevronLeft, Icon};

#[component]
pub fn Layout() -> Element {
    let sidebar_collapsed = use_signal(|| false);

    rsx! {
        div { class: "app-container",
            Header {}
            div { class: "content-container",
                Sidebar { collapsed: sidebar_collapsed }
                main { class: if *sidebar_collapsed.read() { "main-content expanded" } else { "main-content" },
                    Outlet::<Route> {}
                }
            }
        }
    }
}

#[component]
pub fn Header() -> Element {
    let server_addr = use_context::<Signal<ServerAddress>>();
    let fetched_node_info = use_resource(move || api::get_node_info(server_addr.read().0));

    rsx! {
        header {
            h1 { "Mycelium Network Dashboard" }
            div { class: "node-info",
                { match &*fetched_node_info.read_unchecked() {
                    Some(Ok(info)) => rsx! {
                        span { "Subnet: {info.node_subnet}" }
                        span { class: "separator", "|" }
                        span { "Public Key: {info.node_pubkey}" }
                    },
                    Some(Err(_)) => rsx! { span { "Error loading node info" } },
                    None => rsx! { span { "Loading node info..." } },
                }}
            }
        }
    }
}

#[component]
pub fn Sidebar(collapsed: Signal<bool>) -> Element {
    rsx! {
        nav { class: if *collapsed.read() { "sidebar collapsed" } else { "sidebar" },
            ul {
                li { Link { to: Route::Home {}, "Home" } }
                li { Link { to: Route::Peers {}, "Peers" } }
                li { Link { to: Route::Routes {}, "Routes" } }
            }
        }
        button { class: if *collapsed.read() { "toggle-sidebar collapsed" } else { "toggle-sidebar" },
            onclick: {
                let c = *collapsed.read();
                move |_| collapsed.set(!c)
            },
            Icon {
                icon: FaChevronLeft,
            }
        }
    }
}
