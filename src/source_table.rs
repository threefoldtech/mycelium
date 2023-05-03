use std::{net::IpAddr, collections::HashMap};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceKey {
    pub prefix: IpAddr,
    pub plen: u8,
    pub router_id: u64, // We temporarily use 100 for all router IDs
}

#[derive(Debug, Clone)]
pub struct SourceEntry {
    pub metric: u16,
    pub seqno: u16,
    // source_timer
}

// Used to store the feasibility distances
pub struct SourceTable {
    pub table: HashMap<SourceKey, SourceEntry>,
}

impl SourceTable {
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
        }
    }

    pub fn insert(&mut self, key: SourceKey, entry: SourceEntry) {
        self.table.insert(key, entry);
    }

    pub fn remove(&mut self, key: &SourceKey) {
        self.table.remove(key);
    }

    pub fn get(&self, key: &SourceKey) -> Option<&SourceEntry> {
        self.table.get(key)
    }
}