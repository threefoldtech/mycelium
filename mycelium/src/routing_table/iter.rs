use std::{net::Ipv6Addr, sync::Arc};

use crate::subnet::Subnet;

use super::{subnet_entry::SubnetEntry, NoRouteSubnet, QueriedSubnet, RouteListReadGuard};

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

/// Iterator over queried routes in the routing table.
pub struct RoutingTableQueryIter<'a>(
    ip_network_table_deps_treebitmap::Iter<'a, Ipv6Addr, Arc<SubnetEntry>>,
);

impl<'a> RoutingTableQueryIter<'a> {
    /// Create a new `RoutingTableQueryIter` which will iterate over all queried entries in a [`RoutingTable`].
    pub(super) fn new(
        inner: ip_network_table_deps_treebitmap::Iter<'a, Ipv6Addr, Arc<SubnetEntry>>,
    ) -> Self {
        Self(inner)
    }
}

impl Iterator for RoutingTableQueryIter<'_> {
    type Item = QueriedSubnet;

    fn next(&mut self) -> Option<Self::Item> {
        for (ip, prefix_size, rl) in self.0.by_ref() {
            if let SubnetEntry::Queried { query_timeout } = &**rl {
                return Some(QueriedSubnet::new(
                    Subnet::new(ip.into(), prefix_size as u8)
                        .expect("Routing table contains valid subnets"),
                    *query_timeout,
                ));
            }
        }
        None
    }
}

/// Iterator for entries which are explicitly marked as "no route"in the routing table.
pub struct RoutingTableNoRouteIter<'a>(
    ip_network_table_deps_treebitmap::Iter<'a, Ipv6Addr, Arc<SubnetEntry>>,
);

impl<'a> RoutingTableNoRouteIter<'a> {
    /// Create a new `RoutingTableNoRouteIter` which will iterate over all entries in a [`RoutingTable`]
    /// which are explicitly marked as `NoRoute`
    pub(super) fn new(
        inner: ip_network_table_deps_treebitmap::Iter<'a, Ipv6Addr, Arc<SubnetEntry>>,
    ) -> Self {
        Self(inner)
    }
}

impl Iterator for RoutingTableNoRouteIter<'_> {
    type Item = NoRouteSubnet;

    fn next(&mut self) -> Option<Self::Item> {
        for (ip, prefix_size, rl) in self.0.by_ref() {
            if let SubnetEntry::NoRoute { expiry } = &**rl {
                return Some(NoRouteSubnet::new(
                    Subnet::new(ip.into(), prefix_size as u8)
                        .expect("Routing table contains valid subnets"),
                    *expiry,
                ));
            }
        }
        None
    }
}
