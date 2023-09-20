//! A module to abstract the way known public keys for destinations are stored.

use std::net::{IpAddr, Ipv6Addr};

use ip_network_table_deps_treebitmap::IpLookupTable;

use crate::{
    crypto::{PublicKey, SharedSecret},
    subnet::Subnet,
};

pub struct IpPubkeyMap {
    inner: IpLookupTable<Ipv6Addr, (PublicKey, SharedSecret)>,
}

impl IpPubkeyMap {
    /// Create a new empty `IpPubkeyMap`.
    pub fn new() -> Self {
        Self {
            inner: IpLookupTable::new(),
        }
    }

    /// Insert a new [`PublicKey`] and [`SharedSecret`] for a [`Subnet`] in the map.
    pub fn insert(&mut self, subnet: Subnet, pk: PublicKey, ss: SharedSecret) {
        let addr = match subnet.network() {
            IpAddr::V6(addr) => addr,
            _ => return,
        };
        self.inner
            .insert(addr, subnet.prefix_len() as u32, (pk, ss));
    }

    /// Look up the [`PublicKey`] and [`SharedSecret`] associated with an [`IpAddr`], if any is
    /// known in the map.
    pub fn lookup(&self, addr: Ipv6Addr) -> Option<&(PublicKey, SharedSecret)> {
        self.inner.longest_match(addr).map(|(_, _, v)| v)
    }
}

impl Clone for IpPubkeyMap {
    fn clone(&self) -> Self {
        let mut new = IpPubkeyMap::new();
        for (addr, plen, value) in self.inner.iter() {
            new.inner.insert(addr, plen, value.clone());
        }
        new
    }
}
