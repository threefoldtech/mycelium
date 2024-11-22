use axum::{routing::get, Router};
use mycelium::metrics::Metrics;
use prometheus::{
    opts, register_int_counter, register_int_counter_vec, register_int_gauge, Encoder, IntCounter,
    IntCounterVec, IntGauge, TextEncoder,
};
use tracing::{error, info};

use std::net::SocketAddr;

/// A [`Metrics`] implementation which uses prometheus to expose the metrics to the outside world.
#[derive(Clone)]
pub struct PrometheusExporter {
    router_processed_tlvs: IntCounterVec,
    router_peer_added: IntCounter,
    router_peer_removed: IntCounter,
    router_peer_died: IntCounter,
    router_route_selection_ran: IntCounter,
    router_source_key_expired: IntCounter,
    router_expired_routes: IntCounterVec,
    router_selected_route_expired: IntCounter,
    router_triggered_update: IntCounter,
    router_route_packet: IntCounterVec,
    router_seqno_action: IntCounterVec,
    router_tlv_handling_time_spent: IntCounterVec,
    router_update_dead_peer: IntCounter,
    router_received_tlvs: IntCounter,
    router_tlv_source_died: IntCounter,
    router_tlv_discarded: IntCounter,
    router_propage_selected_peers_time_spent: IntCounter,
    router_update_skipped_route_selection: IntCounter,
    router_update_denied_by_filter: IntCounter,
    router_update_not_interested: IntCounter,
    peer_manager_peer_added: IntCounterVec,
    peer_manager_known_peers: IntGauge,
    peer_manager_connection_attemps: IntCounterVec,
}

impl PrometheusExporter {
    /// Create a new [`PrometheusExporter`].
    pub fn new() -> Self {
        Self {
            router_processed_tlvs: register_int_counter_vec!(
                opts!(
                    "mycelium_router_processed_tlvs",
                    "Amount of processed TLV's from peers, by type of TLV"
                ), &["tlv_type"]
            ).expect("Can register int counter vec in default registry"),
            router_peer_added: register_int_counter!(
                "mycelium_router_peer_added",
                "Amount of times a peer was added to the router"
            ).expect("Can register int counter in default registry"),
            router_peer_removed: register_int_counter!(
                "mycelium_router_peer_removed",
                "Amount of times a peer was removed from the router"
            ).expect("Can register int counter in default registry"),
            router_peer_died: register_int_counter!(
                "mycelium_router_peer_died",
                "Amount of times the router noticed a peer was dead, or the peer noticed itself and informed the router",
            ).expect("Can register int counter in default registry"),
            router_route_selection_ran: register_int_counter!(
                "mycelium_router_route_selections",
                "Amount of times a route selection procedure was ran as result of routes expiring or peers being disconnected. Does not include route selection after an update",
            ).expect("Can register int counte rin default registry"),
            router_source_key_expired: register_int_counter!(
                "mycelium_router_source_key_expired",
                "Amount of source keys expired"
            )
            .expect("Can register int counter in default registry"),
            router_expired_routes: register_int_counter_vec!(
                opts!(
                    "mycelium_router_expired_routes",
                    "Route expiration events and the action taken on the route",
                ),
                &["action"]
            )
            .expect("Can register int counter vec in default registry"),
            router_selected_route_expired: register_int_counter!(
                "mycelium_router_selected_route_expired",
                "Amount of times a selected route in the routing table expired"
            )
            .expect("Can register int counter in default registry"),
            router_triggered_update: register_int_counter!(
                "mycelium_router_triggered_updates",
                "Amount of triggered updates sent"
            )
            .expect("Can register int counter in default registry"),
            router_route_packet: register_int_counter_vec!(
                opts!(
                    "mycelium_router_packets_routed",
                    "What happened to a routed data packet"
                ),
                &["verdict"],
            )
            .expect("Can register int counter vec in default registry"),
            router_seqno_action: register_int_counter_vec!(
                opts!(
                    "mycelium_router_seqno_handling",
                    "What happened to a received seqno request",
                ),
                &["action"],
            )
            .expect("Can register int counter vec in default registry"),
            router_tlv_handling_time_spent: register_int_counter_vec!(
                opts!(
                    "mycelium_router_tlv_handling_time",
                    "Amount of time spent handling incoming TLV packets, in nanoseconds",
                ),
                &["tlv_type"],
            )
            .expect("Can register an int counter vec in default registry"),
            router_update_dead_peer: register_int_counter!(
                "mycelium_router_update_dead_peer",
                "Amount of updates we tried to send to a peer, where we found the peer to be dead before actually sending"
            )
            .expect("Can register an int counter in default registry"),
            router_received_tlvs: register_int_counter!(
                "mycelium_router_received_tlvs",
                "Amount of tlv's received by peers",
            )
            .expect("Can register an int counter in the default registry"),
            router_tlv_source_died: register_int_counter!(
                "mycelium_router_tlv_source_died",
                "Dropped TLV's which have been received, but where the peer has died before they could be processed",
            )
            .expect("Can register an int counter in default registry"),
            router_tlv_discarded: register_int_counter!(
                "mycelium_router_tlv_discarded",
                "Dropped TLV's which have been received, but where not processed because the router couldn't keep up",
            )
            .expect("Can register an int counter in default registry"),
            router_propage_selected_peers_time_spent: register_int_counter!(
                "mycelium_router_propagate_selected_route_time",
                "Time spent in the propagate_selected_route task, which periodically announces selected routes to peers. Measurement is in nanoseconds",
            )
            .expect("Can register an int counter in default registry"),
            router_update_skipped_route_selection: register_int_counter!(
                "mycelium_router_update_skipped_route_selection",
                "Updates which were processed but did not run the route selection step, because the updated route could not be selected anyway",
            )
            .expect("Can register an int counter in default registry"),
            router_update_denied_by_filter: register_int_counter!(
                "mycelium_router_update_denied",
                "Updates which were received and immediately denied by a configured filter",
            )
            .expect("Can register an int counter in default registry"),
            router_update_not_interested: register_int_counter!(
                "mycelium_router_update_not_interested",
                "Updates which were allowed by the configured filters, but not of interest as they were either not feasible, or retractions, for an unknown subnet",
            )
            .expect("Can register an int counter in default registry"),
            peer_manager_peer_added: register_int_counter_vec!(
                opts!(
                    "mycelium_peer_manager_peers_added",
                    "Peers added to the peer manager at runtime, by peer type"
                ),
                &["peer_type"],
            )
            .expect("Can register int counter vec in default registry"),
            peer_manager_known_peers: register_int_gauge!(
                "mycelium_peer_manager_known_peers",
                "Amount of known peers in the peer manager"
            )
            .expect("Can register int gauge in default registry"),
            peer_manager_connection_attemps: register_int_counter_vec!(
                opts!(
                    "mycelium_peer_manager_connection_attempts",
                    "Count how many connections the peer manager started to remotes, and finished"
                ),
                &["connection_state"]
            )
            .expect("Can register int counter vec in the default registry"),
        }
    }

    /// Spawns a HTTP server on the provided [`SocketAddr`], to export the gathered metrics. Metrics
    /// are served under the /metrics endpoint.
    pub fn spawn(self, listen_addr: SocketAddr) {
        info!("Enable system metrics on http://{listen_addr}/metrics");
        let app = Router::new().route("/metrics", get(serve_metrics));
        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(listen_addr).await {
                Ok(listener) => listener,
                Err(e) => {
                    error!("Failed to bind listener for Http metrics server: {e}");
                    error!("metrics disabled");
                    return;
                }
            };

            let server = axum::serve(listener, app.into_make_service());
            if let Err(e) = server.await {
                error!("Http API server error: {e}");
            }
        });
    }
}

/// Expose prometheus formatted metrics
async fn serve_metrics() -> String {
    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();

    // Gather the metrics.
    let metric_families = prometheus::gather();
    // Encode them to send.
    encoder
        .encode(&metric_families, &mut buffer)
        .expect("Can encode metrics");
    String::from_utf8(buffer).expect("Metrics are encoded in valid prometheus format")
}

impl Metrics for PrometheusExporter {
    #[inline]
    fn router_process_hello(&self) {
        self.router_processed_tlvs
            .with_label_values(&["hello"])
            .inc()
    }

    #[inline]
    fn router_process_ihu(&self) {
        self.router_processed_tlvs.with_label_values(&["ihu"]).inc()
    }

    #[inline]
    fn router_process_seqno_request(&self) {
        self.router_processed_tlvs
            .with_label_values(&["seqno_request"])
            .inc()
    }

    #[inline]
    fn router_process_route_request(&self, wildcard: bool) {
        let label = if wildcard {
            "wildcard_route_request"
        } else {
            "route_request"
        };
        self.router_processed_tlvs.with_label_values(&[label]).inc()
    }

    #[inline]
    fn router_process_update(&self) {
        self.router_processed_tlvs
            .with_label_values(&["update"])
            .inc()
    }

    #[inline]
    fn router_peer_added(&self) {
        self.router_peer_added.inc()
    }

    #[inline]
    fn router_peer_removed(&self) {
        self.router_peer_removed.inc()
    }

    #[inline]
    fn router_peer_died(&self) {
        self.router_peer_died.inc()
    }

    #[inline]
    fn router_route_selection_ran(&self) {
        self.router_route_selection_ran.inc()
    }

    #[inline]
    fn router_source_key_expired(&self) {
        self.router_source_key_expired.inc()
    }

    #[inline]
    fn router_route_key_expired(&self, removed: bool) {
        let label = if removed { "removed" } else { "retracted" };
        self.router_expired_routes.with_label_values(&[label]).inc()
    }

    #[inline]
    fn router_selected_route_expired(&self) {
        self.router_selected_route_expired.inc()
    }

    #[inline]
    fn router_triggered_update(&self) {
        self.router_triggered_update.inc()
    }

    #[inline]
    fn router_route_packet_local(&self) {
        self.router_route_packet.with_label_values(&["local"]).inc()
    }

    #[inline]
    fn router_route_packet_forward(&self) {
        self.router_route_packet
            .with_label_values(&["forward"])
            .inc()
    }

    #[inline]
    fn router_route_packet_ttl_expired(&self) {
        self.router_route_packet
            .with_label_values(&["ttl_expired"])
            .inc()
    }

    #[inline]
    fn router_route_packet_no_route(&self) {
        self.router_route_packet
            .with_label_values(&["no_route"])
            .inc()
    }

    #[inline]
    fn router_seqno_request_reply_local(&self) {
        self.router_seqno_action
            .with_label_values(&["reply_local"])
            .inc()
    }

    #[inline]
    fn router_seqno_request_bump_seqno(&self) {
        self.router_seqno_action
            .with_label_values(&["bump_seqno"])
            .inc()
    }

    #[inline]
    fn router_seqno_request_dropped_ttl(&self) {
        self.router_seqno_action
            .with_label_values(&["ttl_expired"])
            .inc()
    }

    #[inline]
    fn router_seqno_request_forward_feasible(&self) {
        self.router_seqno_action
            .with_label_values(&["forward_feasible"])
            .inc()
    }

    #[inline]
    fn router_seqno_request_forward_unfeasible(&self) {
        self.router_seqno_action
            .with_label_values(&["forward_unfeasible"])
            .inc()
    }

    #[inline]
    fn router_seqno_request_unhandled(&self) {
        self.router_seqno_action
            .with_label_values(&["unhandled"])
            .inc()
    }

    #[inline]
    fn router_time_spent_handling_tlv(&self, duration: std::time::Duration, tlv_type: &str) {
        self.router_tlv_handling_time_spent
            .with_label_values(&[tlv_type])
            .inc_by(duration.as_nanos() as u64)
    }

    #[inline]
    fn router_update_dead_peer(&self) {
        self.router_update_dead_peer.inc()
    }

    #[inline]
    fn router_received_tlv(&self) {
        self.router_received_tlvs.inc()
    }

    #[inline]
    fn router_tlv_source_died(&self) {
        self.router_tlv_source_died.inc()
    }

    #[inline]
    fn router_tlv_discarded(&self) {
        self.router_tlv_discarded.inc()
    }

    #[inline]
    fn router_time_spent_periodic_propagating_selected_routes(
        &self,
        duration: std::time::Duration,
    ) {
        self.router_propage_selected_peers_time_spent
            .inc_by(duration.as_nanos() as u64)
    }

    #[inline]
    fn router_update_skipped_route_selection(&self) {
        self.router_update_skipped_route_selection.inc()
    }

    #[inline]
    fn router_update_denied_by_filter(&self) {
        self.router_update_denied_by_filter.inc()
    }

    #[inline]
    fn router_update_not_interested(&self) {
        self.router_update_not_interested.inc()
    }

    #[inline]
    fn peer_manager_peer_added(&self, pt: mycelium::peer_manager::PeerType) {
        let label = match pt {
            mycelium::peer_manager::PeerType::Static => "static",
            mycelium::peer_manager::PeerType::Inbound => "inbound",
            mycelium::peer_manager::PeerType::LinkLocalDiscovery => "link_local",
        };
        self.peer_manager_peer_added
            .with_label_values(&[label])
            .inc()
    }

    #[inline]
    fn peer_manager_known_peers(&self, amount: usize) {
        self.peer_manager_known_peers.set(amount as i64)
    }

    #[inline]
    fn peer_manager_connection_attempted(&self) {
        self.peer_manager_connection_attemps
            .with_label_values(&["started"])
            .inc()
    }

    #[inline]
    fn peer_manager_connection_finished(&self) {
        self.peer_manager_connection_attemps
            .with_label_values(&["finished"])
            .inc()
    }
}

impl Default for PrometheusExporter {
    fn default() -> Self {
        Self::new()
    }
}
