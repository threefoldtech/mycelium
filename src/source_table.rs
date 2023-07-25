use std::{collections::HashMap, net::IpAddr};

use x25519_dalek::PublicKey;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub struct SourceKey {
    prefix: IpAddr,
    plen: u8,
    router_id: PublicKey,
}

#[derive(Debug, Clone, Copy)]
pub struct FeasibilityDistance {
    metric: u16,
    seqno: u16,
}

// Store (prefix, plen, router_id) -> feasibility distance mapping
#[derive(Debug)]
pub struct SourceTable {
    table: HashMap<SourceKey, FeasibilityDistance>,
}

impl FeasibilityDistance {
    pub fn new(metric: u16, seqno: u16) -> Self {
        FeasibilityDistance { metric, seqno }
    }

    /// Returns the metric for this `FeasibilityDistance`.
    pub const fn metric(&self) -> u16 {
        self.metric
    }

    /// Returns the sequence number for this `FeasibilityDistance`.
    pub const fn seqno(&self) -> u16 {
        self.seqno
    }
}

impl SourceKey {
    /// Create a new `SourceKey`.
    pub const fn new(prefix: IpAddr, plen: u8, router_id: PublicKey) -> Self {
        Self {
            prefix,
            plen,
            router_id,
        }
    }

    /// Returns the prefix for this `SourceKey`.
    pub const fn prefix(&self) -> IpAddr {
        self.prefix
    }

    /// Returns the prefix length for this `SourceKey`.
    pub const fn plen(&self) -> u8 {
        self.plen
    }

    /// Returns the router id for this `SourceKey`.
    pub const fn router_id(&self) -> PublicKey {
        self.router_id
    }

    /// Updates the router id of this `SourceKey`
    pub fn set_router_id(&mut self, router_id: PublicKey) {
        self.router_id = router_id
    }
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
