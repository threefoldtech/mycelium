use std::{collections::HashMap, net::IpAddr};

use crate::{
    packet::{BabelTLV, ControlStruct},
    peer::Peer,
    router::Router,
    source_table::SourceKey,
    timers::Timer,
};

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
    //pub route_expiry_timer: Timer,
}

impl RouteEntry {
    pub fn new(
        source: SourceKey,
        neighbor: Peer,
        metric: u16,
        seqno: u16,
        next_hop: IpAddr,
        selected: bool,
        router: Router,
    ) -> Self {
        Self {
            source,
            neighbor,
            metric,
            seqno,
            next_hop,
            selected,
        }
    }

    pub fn update(&mut self, update: ControlStruct, should_be_selected: bool) {
        // the update is assumed to be feasible here
        match update.control_packet.body.tlv {
            BabelTLV::Update { seqno, metric, .. } => {
                self.metric = metric;
                self.seqno = seqno;
                self.selected = should_be_selected;
            }
            _ => {
                panic!("Received update with invalid TLV");
            }
        }
    }

    pub fn retracted(&mut self) {
        self.metric = 0xFFFF;
    }

    pub fn is_retracted(&self) -> bool {
        self.metric == 0xFFFF
    }
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
        //println!("Added route to routing table: {:?}", self.table);
    }

    pub fn remove(&mut self, key: &RouteKey) -> Option<RouteEntry> {
        self.table.remove(key)
    }

    pub fn get(&self, key: &RouteKey) -> Option<&RouteEntry> {
        self.table.get(key)
    }
}
