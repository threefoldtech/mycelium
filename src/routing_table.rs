use x25519_dalek::PublicKey;

use crate::{peer::Peer, source_table::SourceKey};
use std::{collections::BTreeMap, net::IpAddr};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RouteKey {
    pub prefix: IpAddr,
    pub plen: u8,
    pub neighbor: IpAddr,
}

#[derive(Debug, Clone)]
pub struct RouteEntry {
    pub source: SourceKey,
    pub neighbor: Peer,
    pub metric: u16, // If metric is 0xFFFF, the route has recently been retracted
    pub seqno: u16,
    pub next_hop: IpAddr, // This is the Peer's address
    pub selected: bool,
}

impl RouteEntry {
    pub fn update_metric(&mut self, metric: u16) {
        self.metric = metric;
    }

    pub fn update_seqno(&mut self, seqno: u16) {
        self.seqno = seqno;
    }

    pub fn update_router_id(&mut self, router_id: PublicKey) {
        self.source.router_id = router_id;
    }
}

#[derive(Debug, Clone)]
pub struct RoutingTable {
    pub table: BTreeMap<RouteKey, RouteEntry>,
}

impl RoutingTable {
    pub fn new() -> Self {
        Self {
            table: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key: RouteKey, entry: RouteEntry) {
        self.table.insert(key, entry);
        //println!("Added route to routing table: {:?}", self.table);
    }

    pub fn remove(&mut self, key: &RouteKey) -> Option<RouteEntry> {
        self.table.remove(key)
    }
}
