use std::{net::IpAddr, collections::HashMap};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceKey {
    pub prefix: IpAddr,
    pub plen: u8,
    pub router_id: u64, // We temporarily use 100 for all router IDs
}

#[derive(Debug, Clone)]

pub struct FeasibilityDistance(pub u16, pub u16); // (metric, seqno)

// Store (prefix, plen, router_id) -> feasibility distance mapping
pub struct SourceTable {
    pub table: HashMap<SourceKey, FeasibilityDistance>,
}

impl SourceTable {
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
        }
    }

    pub fn insert(&mut self, key: SourceKey, feas_dist: FeasibilityDistance) {
        self.table.insert(key, feas_dist);
    }

    pub fn remove(&mut self, key: &SourceKey) {
        self.table.remove(key);
    }

    pub fn get(&self, key: &SourceKey) -> Option<&FeasibilityDistance> {
        self.table.get(key)
    }
}