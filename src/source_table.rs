use std::{collections::HashMap, net::IpAddr};

use x25519_dalek::PublicKey;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub struct SourceKey {
    pub prefix: IpAddr,
    pub plen: u8,
    pub router_id: PublicKey,
}

#[derive(Debug, Clone, Copy)]
pub struct FeasibilityDistance {
    pub metric: u16,
    pub seqno: u16,
}

impl FeasibilityDistance {
    pub fn new(metric: u16, seqno: u16) -> Self {
        FeasibilityDistance { metric, seqno }
    }
}

// Store (prefix, plen, router_id) -> feasibility distance mapping
#[derive(Debug)]
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
