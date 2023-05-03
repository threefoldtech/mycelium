use std::{net::IpAddr, collections::HashMap};

use crate::{source_table::SourceKey, peer::Peer};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    // route_timer
}

#[derive(Debug, Clone)]
pub struct RoutingTable {
    pub table: HashMap<RouteKey, RouteEntry>,
}

impl RoutingTable {
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
        }
    }

    pub fn insert(&mut self, key: RouteKey, entry: RouteEntry) {
        self.table.insert(key, entry);
        println!("Added route to routing table: {:?}", self.table);
    }

    pub fn remove(&mut self, key: &RouteKey) {
        self.table.remove(key);
    }

    pub fn get(&self, key: &RouteKey) -> Option<&RouteEntry> {
        self.table.get(key)
    }
}

// TODO: add support of suprious starvation detection
pub fn select_best_route(routes: &[RouteEntry]) -> Option<&RouteEntry> {
    //routes.iter().min_by_key(|route| (route.metric, route.router_id))
    todo!("select_best_route")
}






