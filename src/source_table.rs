use std::{collections::HashMap, net::IpAddr};

use log::error;
use x25519_dalek::PublicKey;

use crate::{
    metric::Metric,
    packet::{BabelTLV, ControlStruct},
    sequence_number::SeqNo,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub struct SourceKey {
    prefix: IpAddr,
    plen: u8,
    router_id: PublicKey,
}

#[derive(Debug, Clone, Copy)]
pub struct FeasibilityDistance {
    metric: Metric,
    seqno: SeqNo,
}

// Store (prefix, plen, router_id) -> feasibility distance mapping
#[derive(Debug)]
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

    /// Indicates if an update is feasible in the context of the current `SoureTable`.
    pub fn is_update_feasible(&self, update: &ControlStruct) -> bool {
        // Before an update is accepted it should be checked against the feasbility condition
        // If an entry in the source table with the same source key exists, we perform the feasbility check
        // If no entry exists yet, the update is accepted as there is no better alternative available (yet)
        match update.control_packet.body.tlv {
            BabelTLV::Update {
                plen,
                interval: _,
                seqno,
                metric,
                prefix,
                router_id,
            } => {
                let source_key = SourceKey::new(prefix, plen, router_id);
                match self.get(&source_key) {
                    Some(&entry) => {
                        return (!seqno.lt(&entry.seqno()))
                            || (seqno == entry.seqno() && metric < entry.metric())
                            || metric.is_infinite();
                    }
                    None => return true,
                }
            }
            _ => {
                error!("Error accepting update, control struct did not match update packet");
                return false;
            }
        }
    }
}
