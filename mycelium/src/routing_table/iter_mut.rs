use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace};

use crate::{peer::Peer, subnet::Subnet};

use super::{
    EntryIdentifier, Occupied, PossiblyEmpty, RouteEntry, RouteKey, RouteList, RoutingTableInner,
    RoutingTableOplogEntry,
};
use std::{
    cell::RefCell,
    marker::PhantomData,
    net::Ipv6Addr,
    ops::{Deref, DerefMut},
    rc::Rc,
    sync::{Arc, MutexGuard},
};

/// An iterator over a [`routing table`](super::RoutingTable), yielding mutable access to the
/// entries in the table.
pub struct RoutingTableIterMut<'a, 'b> {
    write_guard: Rc<
        RefCell<MutexGuard<'a, left_right::WriteHandle<RoutingTableInner, RoutingTableOplogEntry>>>,
    >,
    iter: ip_network_table_deps_treebitmap::Iter<'b, Ipv6Addr, Arc<RouteList>>,

    expired_route_entry_sink: mpsc::Sender<RouteKey>,
    cancel_token: CancellationToken,
}

impl<'a, 'b> RoutingTableIterMut<'a, 'b> {
    pub(super) fn new(
        write_guard: Rc<
            RefCell<
                MutexGuard<'a, left_right::WriteHandle<RoutingTableInner, RoutingTableOplogEntry>>,
            >,
        >,
        iter: ip_network_table_deps_treebitmap::Iter<'b, Ipv6Addr, Arc<RouteList>>,

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
}

impl<'a, 'b> Iterator for RoutingTableIterMut<'a, 'b> {
    type Item = (Subnet, RoutingTableIterMutEntry<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|(ip, prefix_size, rl)| {
            let subnet = Subnet::new(ip.into(), prefix_size as u8)
                .expect("Routing table contains valid subnets");
            (
                subnet,
                RoutingTableIterMutEntry {
                    writer: Rc::clone(&self.write_guard),
                    value: Arc::clone(rl),
                    owned: false,
                    subnet,
                    expired_route_entry_sink: self.expired_route_entry_sink.clone(),
                    cancellation_token: self.cancel_token.clone(),
                },
            )
        })
    }
}

/// A smart pointer giving mutable access to a [`RouteList`].
pub struct RoutingTableIterMutEntry<'a> {
    writer: Rc<
        RefCell<MutexGuard<'a, left_right::WriteHandle<RoutingTableInner, RoutingTableOplogEntry>>>,
    >,
    /// Owned copy of the RouteList, this is populated once mutable access the the RouteList has
    /// been requested.
    value: Arc<RouteList>,
    owned: bool,
    /// The subnet we are writing to.
    subnet: Subnet,
    expired_route_entry_sink: mpsc::Sender<RouteKey>,
    cancellation_token: CancellationToken,
}

impl<'a, 'b> RoutingTableIterMutEntry<'a> {
    pub fn entry_mut(&'b mut self, neighbour: &Peer) -> IterRouteEntryGuard<'a, 'b, PossiblyEmpty> {
        let entry_identifier = if let Some(pos) = self
            .list
            .iter_mut()
            .map(|(_, e)| e)
            .position(|entry| entry.neighbour() == neighbour)
        {
            EntryIdentifier::Pos(pos)
        } else {
            EntryIdentifier::None
        };

        IterRouteEntryGuard {
            write_guard: self,
            entry_identifier,
            delete: false,
            _marker: PhantomData,
        }
    }
}

impl<'a> Deref for RoutingTableIterMutEntry<'a> {
    type Target = RouteList;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl DerefMut for RoutingTableIterMutEntry<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.owned = true;
        Arc::make_mut(&mut self.value)
    }
}

impl Drop for RoutingTableIterMutEntry<'_> {
    fn drop(&mut self) {
        // If owned is false, we never got a mutable reference, and the existing value is a
        // reference to the already existing value in the route table. So we don't have to do
        // anything here.
        if !self.owned {
            return;
        }

        let mut writer = self.writer.borrow_mut();

        // FIXME: try to get rid of clones on the Arc here
        // There was an existing route list which is now empty, so the entry for this subnet
        // needs to be deleted in the routing table.
        if self.value.is_empty() {
            trace!(subnet = ?self.subnet, "Removing route list for subnet");
            writer.append(RoutingTableOplogEntry::Delete(self.subnet));
        }
        // The value already existed, and was mutably accessed, so it was dissociated. Update
        // the routing table to point to the new value.
        else {
            trace!(subnet = ?self.subnet, "Updating route list for subnet");
            writer.append(RoutingTableOplogEntry::Upsert(
                self.subnet,
                Arc::clone(&self.value),
            ));
        }
    }
}

/// A guard which allows write access to a single [`RouteEntry`].
#[repr(C)] // This is needed since we transmute between instances with different markers.
pub struct IterRouteEntryGuard<'a, 'b, O> {
    write_guard: &'b mut RoutingTableIterMutEntry<'a>,
    /// A way to identify the actual [`RouteEntry`].
    entry_identifier: EntryIdentifier,
    /// Keep track whether we need to delete this entry or not.
    delete: bool,
    _marker: std::marker::PhantomData<O>,
}

impl<'a, 'b, O> IterRouteEntryGuard<'a, 'b, O> {
    /// Mark the [`RouteEntry`] contained in this `RouteEntryGuard` to be deleted.
    pub fn delete(mut self) {
        self.delete = true
    }
}

impl<'a, 'b> IterRouteEntryGuard<'a, 'b, PossiblyEmpty> {
    /// Returns `true` if the `RouteEntryGuard` does not point to an existing value, or is not a
    /// new value.
    pub fn is_none(&self) -> bool {
        matches!(self.entry_identifier, EntryIdentifier::None)
    }

    /// Convert this `RouteEntryGuard` to an `Occupied` variant, panicking if the contained entry
    /// is none.
    pub fn unwrap(self) -> IterRouteEntryGuard<'a, 'b, Occupied> {
        assert!(
            !matches!(self.entry_identifier, EntryIdentifier::None),
            "Called RouteEntryGuard::unwrap on an emtpy entry guard"
        );

        // SAFETY: Transmuting to the same struct with a different marker, on a repr(C) struct.
        unsafe { std::mem::transmute::<Self, IterRouteEntryGuard<'a, 'b, Occupied>>(self) }
    }
}

impl Deref for IterRouteEntryGuard<'_, '_, Occupied> {
    type Target = RouteEntry;

    fn deref(&self) -> &Self::Target {
        match self.entry_identifier {
            EntryIdentifier::Pos(pos) => &self.write_guard.list[pos].1,
            EntryIdentifier::New(ref re) => re,
            EntryIdentifier::None => {
                unreachable!()
            }
        }
    }
}

impl DerefMut for IterRouteEntryGuard<'_, '_, Occupied> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self.entry_identifier {
            EntryIdentifier::Pos(pos) => &mut self.write_guard.list[pos].1,
            EntryIdentifier::New(ref mut re) => re,
            EntryIdentifier::None => {
                unreachable!()
            }
        }
    }
}

impl<O> Drop for IterRouteEntryGuard<'_, '_, O> {
    fn drop(&mut self) {
        if self.delete {
            // If we refer to an existing entry, cancel the hold timer
            if let EntryIdentifier::Pos(idx) = self.entry_identifier {
                // Important: swap remove reorders the vector, but this is not an issue as it moves
                // the _last_ element, and we only need the first element to be stable.
                let old = self.write_guard.list.swap_remove(idx);
                old.0.abort();
            }
            return;
        }

        let expired_route_entry_sink = self.write_guard.expired_route_entry_sink.clone();
        let cancellation_token = self.write_guard.cancellation_token.clone();

        let spawn_timer = |re: &RouteEntry| {
            let expiration = re.expires();
            let rk = RouteKey::new(re.source().subnet(), re.neighbour().clone());
            Arc::new(
                tokio::spawn(async move {
                    tokio::select! {
                        _ = cancellation_token.cancelled() => {}
                        _ = tokio::time::sleep_until(expiration) => {
                            debug!(route_key = ?rk, "Expired route entry for route key");
                            if let Err(e) =  expired_route_entry_sink.send(rk).await {
                                error!(route_key = ?e.0, "Failed to send expired route key on cleanup channel");
                            }
                        }
                    }
                })
                .abort_handle(),
            )
        };

        let value = std::mem::replace(&mut self.entry_identifier, EntryIdentifier::None);
        match value {
            EntryIdentifier::Pos(pos) => {
                let v = &mut self.write_guard.list[pos];
                let handle = spawn_timer(&v.1);
                v.0.abort();
                v.0 = handle;
            }
            EntryIdentifier::New(re) => {
                let handle = spawn_timer(&re);
                self.write_guard.list.push((handle, re));
            }
            EntryIdentifier::None => {}
        }

        // TODO: manage route entry based on selected flag
    }
}
