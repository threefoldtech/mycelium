//! Conversions between mycelium core types and AIDL-generated parcelables.

use crate::peer_manager::PeerStats;
use crate::routing_table::RouteEntry;

use super::tech::threefold::mycelium::PacketStatEntry::PacketStatEntry;
use super::tech::threefold::mycelium::PeerInfo::PeerInfo;
use super::tech::threefold::mycelium::Route::Route;

impl From<PeerStats> for PeerInfo {
    fn from(p: PeerStats) -> Self {
        PeerInfo {
            protocol: p.endpoint.proto().to_string(),
            address: p.endpoint.address().to_string(),
            peerType: p.pt.to_string(),
            connectionState: p.connection_state.to_string(),
            rxBytes: p.rx_bytes as i64,
            txBytes: p.tx_bytes as i64,
            discoveredSeconds: p.discovered as i64,
            lastConnectedSeconds: p.last_connected.map(|v| v as i64).unwrap_or(-1),
        }
    }
}

impl From<RouteEntry> for Route {
    fn from(r: RouteEntry) -> Self {
        Route {
            subnet: r.source().subnet().to_string(),
            nextHop: r.neighbour().connection_identifier().clone(),
            metric: if r.metric().is_infinite() {
                -1
            } else {
                u16::from(r.metric()) as i64
            },
            seqno: u16::from(r.seqno()) as i32,
        }
    }
}

impl From<crate::router::PacketStatEntry> for PacketStatEntry {
    fn from(e: crate::router::PacketStatEntry) -> Self {
        PacketStatEntry {
            ip: e.ip.to_string(),
            packetCount: e.packet_count as i64,
            byteCount: e.byte_count as i64,
        }
    }
}
