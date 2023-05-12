use std::{net::IpAddr, collections::HashMap};

use crate::{source_table::SourceKey, peer::Peer, timers::Timer, router::Router, packet::{ControlStruct, BabelTLV}};

const HELLO_INTERVAL: u16 = 4;
const IHU_INTERVAL: u16 = HELLO_INTERVAL * 3;
const UPDATE_INTERVAL: u16 = HELLO_INTERVAL * 4;

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
    pub fn new(source: SourceKey, neighbor: Peer, metric: u16, seqno: u16, next_hop: IpAddr, selected: bool, router: Router) -> Self {
        Self {
            source,
            neighbor,
            metric,
            seqno,
            next_hop,
            selected,
        }
    }

    // pub fn update(&mut self, metric: u16, seqno: u16, next_hop: IpAddr) {
    //     self.metric = metric;
    //     self.seqno = seqno;
    //     self.next_hop = next_hop;
    // }

    pub fn update(&mut self, update: ControlStruct) {
        // the update is assumed to be feasible here
        match update.control_packet.body.tlv {
            BabelTLV::Update { plen, interval, seqno, metric, prefix, router_id } => {
                self.metric = metric;
                self.seqno = seqno; 
            },
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

    pub fn remove(&mut self, key: &RouteKey) {
        self.table.remove(key);
    }

    pub fn get(&self, key: &RouteKey) -> Option<&RouteEntry> {
        self.table.get(key)
    }
}



