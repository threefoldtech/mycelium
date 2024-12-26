use std::{net::Ipv6Addr, sync::Arc};

use arc_swap::ArcSwap;

use crate::{routing_table::RouteList, subnet::Subnet};

use super::RouteListReadGuard;

/// An iterator over a [`routing table`](super::RoutingTable) giving read only access to
/// [`RouteList`]'s.
pub struct RoutingTableIter<'a>(
    ip_network_table_deps_treebitmap::Iter<'a, Ipv6Addr, Arc<ArcSwap<RouteList>>>,
);

impl<'a> RoutingTableIter<'a> {
    /// Create a new `RoutingTableIter` which will iterate over all entries in a [`RoutingTable`].
    pub(super) fn new(
        inner: ip_network_table_deps_treebitmap::Iter<'a, Ipv6Addr, Arc<ArcSwap<RouteList>>>,
    ) -> Self {
        Self(inner)
    }
}

impl Iterator for RoutingTableIter<'_> {
    type Item = (Subnet, RouteListReadGuard);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(ip, prefix_size, rl)| {
            (
                Subnet::new(ip.into(), prefix_size as u8)
                    .expect("Routing table contains valid subnets"),
                RouteListReadGuard { inner: rl.load() },
            )
        })
    }
}
