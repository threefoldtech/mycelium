use std::{net::Ipv6Addr, sync::Arc};

use crate::{routing_table::RouteList, subnet::Subnet};

/// An iterator over a [`routing table`](super::RoutingTable) giving read only access to
/// [`RouteList`]'s.
pub struct RoutingTableIter<'a>(
    ip_network_table_deps_treebitmap::Iter<'a, Ipv6Addr, Arc<RouteList>>,
);

impl<'a> RoutingTableIter<'a> {
    /// Create a new `RoutingTableIter` which will iterate over all entries in a [`RoutingTable`].
    pub(super) fn new(
        inner: ip_network_table_deps_treebitmap::Iter<'a, Ipv6Addr, Arc<RouteList>>,
    ) -> Self {
        Self(inner)
    }
}

impl<'a> Iterator for RoutingTableIter<'a> {
    type Item = (Subnet, &'a Arc<RouteList>);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(ip, prefix_size, rl)| {
            (
                Subnet::new(ip.into(), prefix_size as u8)
                    .expect("Routing table contains valid subnets"),
                rl,
            )
        })
    }
}
