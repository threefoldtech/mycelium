//! Conversions from internal mycelium types to the `repr(C)` FFI mirrors.
//!
//! Mirrors the behaviour of the previous AIDL `conversions.rs` — same fields,
//! same fallback values — just allocating C strings instead of Rust ones.

use crate::peer_manager::PeerStats;
use crate::routing_table::RouteEntry;

use super::types::{cstring, mycelium_packet_stat_entry_t, mycelium_peer_info_t, mycelium_route_t};

impl From<PeerStats> for mycelium_peer_info_t {
    fn from(p: PeerStats) -> Self {
        mycelium_peer_info_t {
            protocol: cstring(p.endpoint.proto().to_string()),
            address: cstring(p.endpoint.address().to_string()),
            peer_type: cstring(p.pt.to_string()),
            connection_state: cstring(p.connection_state.to_string()),
            rx_bytes: p.rx_bytes as i64,
            tx_bytes: p.tx_bytes as i64,
            discovered_seconds: p.discovered as i64,
            last_connected_seconds: p.last_connected.map(|v| v as i64).unwrap_or(-1),
        }
    }
}

impl From<RouteEntry> for mycelium_route_t {
    fn from(r: RouteEntry) -> Self {
        mycelium_route_t {
            subnet: cstring(r.source().subnet().to_string()),
            next_hop: cstring(r.neighbour().connection_identifier().clone()),
            metric: if r.metric().is_infinite() {
                -1
            } else {
                u16::from(r.metric()) as i64
            },
            seqno: u16::from(r.seqno()) as i32,
        }
    }
}

impl From<crate::router::PacketStatEntry> for mycelium_packet_stat_entry_t {
    fn from(e: crate::router::PacketStatEntry) -> Self {
        mycelium_packet_stat_entry_t {
            ip: cstring(e.ip.to_string()),
            packet_count: e.packet_count as i64,
            byte_count: e.byte_count as i64,
        }
    }
}
