use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::trace;

use crate::subnet::Subnet;

use super::{
    subnet_entry::SubnetEntry, RouteKey, RouteList, RoutingTableInner, RoutingTableOplogEntry,
};
use std::{
    net::Ipv6Addr,
    sync::{Arc, MutexGuard},
};

/// An iterator over a [`routing table`](super::RoutingTable), yielding mutable access to the
/// entries in the table.
pub struct RoutingTableIterMut<'a, 'b> {
    write_guard:
        &'b mut MutexGuard<'a, left_right::WriteHandle<RoutingTableInner, RoutingTableOplogEntry>>,
    iter: ip_network_table_deps_treebitmap::Iter<'b, Ipv6Addr, Arc<SubnetEntry>>,

    expired_route_entry_sink: mpsc::Sender<RouteKey>,
    cancel_token: CancellationToken,
}

impl<'a, 'b> RoutingTableIterMut<'a, 'b> {
    pub(super) fn new(
        write_guard: &'b mut MutexGuard<
            'a,
            left_right::WriteHandle<RoutingTableInner, RoutingTableOplogEntry>,
        >,
        iter: ip_network_table_deps_treebitmap::Iter<'b, Ipv6Addr, Arc<SubnetEntry>>,

        expired_route_entry_sink: mpsc::Sender<RouteKey>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            write_guard,
            iter,
            expired_route_entry_sink,
            cancel_token,
        }
    }

    /// Get the next item in this iterator. This is not implemented as the [`Iterator`] trait,
    /// since we hand out items which are lifetime bound to this struct.
    pub fn next<'c>(&'c mut self) -> Option<(Subnet, RoutingTableIterMutEntry<'a, 'c>)> {
        for (ip, prefix_size, rl) in self.iter.by_ref() {
            if matches!(&**rl, SubnetEntry::Exists { .. }) {
                let subnet = Subnet::new(ip.into(), prefix_size as u8)
                    .expect("Routing table contains valid subnets");
                return Some((
                    subnet,
                    RoutingTableIterMutEntry {
                        writer: self.write_guard,
                        store: Arc::clone(rl),
                        subnet,
                        expired_route_entry_sink: self.expired_route_entry_sink.clone(),
                        cancellation_token: self.cancel_token.clone(),
                    },
                ));
            };
        }

        None
    }
}

/// A smart pointer giving mutable access to a [`RouteList`].
pub struct RoutingTableIterMutEntry<'a, 'b> {
    writer:
        &'b mut MutexGuard<'a, left_right::WriteHandle<RoutingTableInner, RoutingTableOplogEntry>>,
    /// Owned copy of the RouteList, this is populated once mutable access the the RouteList has
    /// been requested.
    store: Arc<SubnetEntry>,
    /// The subnet we are writing to.
    subnet: Subnet,
    expired_route_entry_sink: mpsc::Sender<RouteKey>,
    cancellation_token: CancellationToken,
}

impl RoutingTableIterMutEntry<'_, '_> {
    /// Updates the routes for this entry
    pub fn update_routes<F: FnMut(&mut RouteList, &mpsc::Sender<RouteKey>, &CancellationToken)>(
        &mut self,
        mut op: F,
    ) {
        let mut delete = false;
        if let SubnetEntry::Exists { list } = &*self.store {
            list.rcu(|rl| {
                let mut new_val = rl.clone();
                let v = Arc::make_mut(&mut new_val);

                op(v, &self.expired_route_entry_sink, &self.cancellation_token);
                delete = v.is_empty();

                new_val
            });

            if delete {
                trace!(subnet = %self.subnet, "Queue subnet for deletion since route list is now empty");
                self.writer
                    .append(RoutingTableOplogEntry::Delete(self.subnet));
            }
        }
    }
}
