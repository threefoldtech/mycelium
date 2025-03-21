use std::{net::Ipv6Addr, sync::Arc};

use crate::subnet::Subnet;

use super::{subnet_entry::SubnetEntry, RouteListReadGuard};

/// An iterator over a [`routing table`](super::RoutingTable) giving read only access to
/// [`RouteList`]'s.
pub struct RoutingTableIter<'a>(
    ip_network_table_deps_treebitmap::Iter<'a, Ipv6Addr, Arc<SubnetEntry>>,
);

impl<'a> RoutingTableIter<'a> {
    /// Create a new `RoutingTableIter` which will iterate over all entries in a [`RoutingTable`].
    pub(super) fn new(
        inner: ip_network_table_deps_treebitmap::Iter<'a, Ipv6Addr, Arc<SubnetEntry>>,
    ) -> Self {
        Self(inner)
    }
}

impl Iterator for RoutingTableIter<'_> {
    type Item = (Subnet, RouteListReadGuard);

    fn next(&mut self) -> Option<Self::Item> {
        for (ip, prefix_size, rl) in self.0.by_ref() {
            if let SubnetEntry::Exists { list } = &**rl {
                return Some((
                    Subnet::new(ip.into(), prefix_size as u8)
                        .expect("Routing table contains valid subnets"),
                    RouteListReadGuard { inner: list.load() },
                ));
            }
        }
        None
    }
}
