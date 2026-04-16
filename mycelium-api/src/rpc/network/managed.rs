use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use super::models::ListenerPolicy;

#[derive(Debug)]
pub struct ManagedState {
    pub bridges: BTreeSet<String>,
    pub addresses: BTreeSet<(String, IpAddr, u8)>,
    pub policy: ListenerPolicy,
    pub explicit_addresses: Vec<String>,
}

impl Default for ManagedState {
    fn default() -> Self {
        Self {
            bridges: BTreeSet::new(),
            addresses: BTreeSet::new(),
            policy: ListenerPolicy::default(),
            explicit_addresses: Vec::new(),
        }
    }
}

impl ManagedState {
    pub fn new_shared() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::default()))
    }
}
