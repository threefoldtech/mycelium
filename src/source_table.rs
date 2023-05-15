use std::{collections::HashMap, net::IpAddr};

use crate::packet::{BabelTLV, ControlStruct};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub struct SourceKey {
    pub prefix: IpAddr,
    pub plen: u8,
    pub router_id: u64, // We temporarily use 100 for all router IDs
}

#[derive(Debug, Clone, Copy)]
pub struct FeasibilityDistance(pub u16, pub u16); // (metric, seqno)

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

    pub fn update(&mut self, update: &ControlStruct) {
        match update.control_packet.body.tlv {
            BabelTLV::Update {
                plen,
                interval,
                seqno,
                metric,
                prefix,
                router_id,
            } => {
                // first check if the update is feasible
                if !self.is_feasible(update) {
                    return;
                }

                let key = SourceKey {
                    prefix: prefix,
                    plen: plen,
                    router_id: router_id,
                };

                let new_distance = FeasibilityDistance(metric, seqno);
                let old_distance = self.table.get(&key).cloned();
                match old_distance {
                    Some(old_distance) => {
                        if new_distance.0 < old_distance.0 {
                            self.table
                                .insert(key, FeasibilityDistance(new_distance.0, new_distance.1));
                        }
                    }
                    None => {
                        self.table
                            .insert(key, FeasibilityDistance(new_distance.0, new_distance.1));
                    }
                }
            }
            _ => {
                panic!("not an update");
            }
        }
    }

    pub fn is_feasible(&self, update: &ControlStruct) -> bool {
        match update.control_packet.body.tlv {
            BabelTLV::Update {
                plen,
                interval: _,
                seqno,
                metric,
                prefix,
                router_id,
            } => {
                let key = SourceKey {
                    prefix: prefix,
                    plen: plen,
                    router_id: router_id,
                };

                match self.table.get(&key) {
                    Some(&source_entry) => {
                        let metric_2 = source_entry.0;
                        let seqno_2 = source_entry.1;

                        seqno > seqno_2 || (seqno == seqno_2 && metric < metric_2)
                    }
                    None => true,
                }
            }
            _ => {
                panic!("not an update");
            }
        }
    }
}
