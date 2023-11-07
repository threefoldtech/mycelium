use core::fmt;
use std::collections::HashMap;

use crate::{babel, metric::Metric, router_id::RouterId, sequence_number::SeqNo, subnet::Subnet};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub struct SourceKey {
    subnet: Subnet,
    router_id: RouterId,
}

#[derive(Debug, Clone, Copy)]
pub struct FeasibilityDistance {
    metric: Metric,
    seqno: SeqNo,
}

// Store (prefix, plen, router_id) -> feasibility distance mapping
#[derive(Debug, Clone)]
pub struct SourceTable {
    table: HashMap<SourceKey, FeasibilityDistance>,
}

impl FeasibilityDistance {
    pub fn new(metric: Metric, seqno: SeqNo) -> Self {
        FeasibilityDistance { metric, seqno }
    }

    /// Returns the metric for this `FeasibilityDistance`.
    pub const fn metric(&self) -> Metric {
        self.metric
    }

    /// Returns the sequence number for this `FeasibilityDistance`.
    pub const fn seqno(&self) -> SeqNo {
        self.seqno
    }
}

impl SourceKey {
    /// Create a new `SourceKey`.
    pub const fn new(subnet: Subnet, router_id: RouterId) -> Self {
        Self { subnet, router_id }
    }

    /// Returns the [`RouterId`] for this `SourceKey`.
    pub const fn router_id(&self) -> RouterId {
        self.router_id
    }

    /// Returns the [`Subnet`] for this `SourceKey`.
    pub const fn subnet(&self) -> Subnet {
        self.subnet
    }

    /// Updates the [`RouterId`] of this `SourceKey`
    pub fn set_router_id(&mut self, router_id: RouterId) {
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

    /// Indicates if an update is feasible in the context of the current `SoureTable`.
    pub fn is_update_feasible(&self, update: &babel::Update) -> bool {
        // Before an update is accepted it should be checked against the feasbility condition
        // If an entry in the source table with the same source key exists, we perform the feasbility check
        // If no entry exists yet, the update is accepted as there is no better alternative available (yet)
        let source_key = SourceKey::new(update.subnet(), update.router_id());
        match self.get(&source_key) {
            Some(entry) => {
                (update.seqno().gt(&entry.seqno()))
                    || (update.seqno() == entry.seqno() && update.metric() < entry.metric())
                    || update.metric().is_infinite()
            }
            None => true,
        }
    }

    /// Get an iterator over the `SourceTable`.
    pub fn iter(&self) -> impl Iterator<Item = (&SourceKey, &FeasibilityDistance)> {
        self.table.iter()
    }
}

impl fmt::Display for SourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "{} advertised by {}",
            self.subnet, self.router_id
        ))
    }
}
