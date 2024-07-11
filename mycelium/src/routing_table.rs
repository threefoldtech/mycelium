use std::{
    cell::RefCell,
    net::{IpAddr, Ipv6Addr},
    ops::{Deref, DerefMut, Index},
    rc::Rc,
    sync::{Arc, Mutex, MutexGuard},
};

use ip_network_table_deps_treebitmap::IpLookupTable;
use tokio::{sync::mpsc, task::AbortHandle};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace};

use crate::{crypto::SharedSecret, peer::Peer, subnet::Subnet};

pub use iter::RoutingTableIter;
pub use iter_mut::RoutingTableIterMut;
pub use route_entry::RouteEntry;
pub use route_key::RouteKey;

mod iter;
mod iter_mut;
mod route_entry;
mod route_key;

/// The routing table holds a list of route entries for every known subnet.
#[derive(Clone)]
pub struct RoutingTable {
    writer: Arc<Mutex<left_right::WriteHandle<RoutingTableInner, RoutingTableOplogEntry>>>,
    reader: left_right::ReadHandle<RoutingTableInner>,

    expired_route_entry_sink: mpsc::Sender<RouteKey>,
    cancel_token: CancellationToken,
}

#[derive(Default)]
struct RoutingTableInner {
    table: IpLookupTable<Ipv6Addr, Arc<RouteList>>,
}

/// The RouteList holds all routes for a specific subnet.
// By convention, if a route is selected, it will always be at index 0 in the list.
#[derive(Clone)]
pub struct RouteList {
    list: Vec<(Arc<AbortHandle>, RouteEntry)>,
    shared_secret: SharedSecret,
}

impl RouteList {
    /// Create a new empty RouteList
    fn new(shared_secret: SharedSecret) -> Self {
        Self {
            list: Vec::new(),
            shared_secret,
        }
    }

    /// Returns the [`SharedSecret`] used for encryption of packets to and from the associated
    /// [`Subnet`].
    #[inline]
    pub fn shared_secret(&self) -> &SharedSecret {
        &self.shared_secret
    }

    /// Checks if there are any actual routes in the list.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Returns the selected route for the [`Subnet`] this is the `RouteList` for, if one exists.
    pub fn selected(&self) -> Option<&RouteEntry> {
        self.list
            .first()
            .map(|(_, re)| re)
            .and_then(|re| if re.selected() { Some(re) } else { None })
    }

    /// Returns an iterator over the `RouteList`.
    ///
    /// The iterator yields all [`route entries`](RouteEntry) in the list.
    pub fn iter(&self) -> RouteListIter {
        RouteListIter::new(self)
    }

    /// Selects the [`RouteEntry`] which matches the [`RouteKey`] for the associated subnet.
    ///
    /// If a selected route already exists, it is unselected automatically.
    pub fn set_selected(&mut self, neighbour: &Peer) {
        let Some(pos) = self
            .list
            .iter()
            .position(|re| re.1.neighbour() == neighbour)
        else {
            error!(
                neighbour = neighbour.connection_identifier(),
                "Failed to select route entry with given route key, no such entry"
            );
            return;
        };

        // We don't need a check for an empty list here, since we found a selected route there
        // _MUST_ be at least 1 entry.
        // Set the first element to unselected, then select the proper element so this also works
        // in case the existing route is "reselected".
        self.list[0].1.set_selected(false);
        self.list[pos].1.set_selected(true);
        self.list.swap(0, pos);
    }
}

impl Index<usize> for RouteList {
    type Output = RouteEntry;

    fn index(&self, index: usize) -> &Self::Output {
        &self.list[index].1
    }
}

pub struct RouteListIter<'a> {
    route_list: &'a RouteList,
    idx: usize,
}

impl<'a> RouteListIter<'a> {
    /// Create a new `RouteListIter` which will iterate over the given [`RouteList`].
    fn new(route_list: &'a RouteList) -> Self {
        Self { route_list, idx: 0 }
    }
}

impl<'a> Iterator for RouteListIter<'a> {
    type Item = &'a RouteEntry;

    fn next(&mut self) -> Option<Self::Item> {
        self.idx += 1;
        self.route_list.list.get(self.idx - 1).map(|(_, re)| re)
    }
}

/// A guard which allows write access to a single [`RouteEntry`].
#[repr(C)] // This is needed since we transmute between instances with different markers.
pub struct RouteEntryGuard<'a, 'b, O> {
    write_guard: &'b mut WriteGuard<'a>,
    /// A way to identify the actual [`RouteEntry`].
    entry_identifier: EntryIdentifier,
    /// Keep track whether we need to delete this entry or not.
    delete: bool,
    _marker: std::marker::PhantomData<O>,
}

/// Marker to allow [`RouteEntryGuard::or_insert`] to be called once.
pub struct PossiblyEmpty;
/// Maker to clearify [`RouteEntryGuard::or_insert`] has been called, and we can always deref the
/// pointer.
pub struct Occupied;

/// Specified how the entry we want from the list can be found.
enum EntryIdentifier {
    /// The given entry exists in the RouteList at the specified position.
    Pos(usize),
    /// A new Route Entry has been inserted which is not part of the RouteList yet.
    New(RouteEntry),
    /// No entry is found in the list, and we haven't been given one either.
    None,
}

impl<'a, 'b, O> RouteEntryGuard<'a, 'b, O> {
    /// Mark the [`RouteEntry`] contained in this `RouteEntryGuard` to be deleted.
    pub fn delete(mut self) {
        self.delete = true
    }
}

impl<'a, 'b> RouteEntryGuard<'a, 'b, PossiblyEmpty> {
    /// Inserts the given [`RouteEntry`] if it does not exist yet. If an entry already exists, this
    /// does nothing.
    pub fn or_insert(mut self, re: RouteEntry) -> RouteEntryGuard<'a, 'b, Occupied> {
        if matches!(self.entry_identifier, EntryIdentifier::None) {
            self.entry_identifier = EntryIdentifier::New(re)
        };

        // SAFETY: Transmuting to the same struct with a different marker, on a repr(C) struct.
        unsafe { std::mem::transmute::<Self, RouteEntryGuard<'a, 'b, Occupied>>(self) }
    }

    /// Returns `true` if the `RouteEntryGuard` does not point to an existing value, or is not a
    /// new value.
    pub fn is_none(&self) -> bool {
        matches!(self.entry_identifier, EntryIdentifier::None)
    }

    /// Returns `true` if the `RouteEntryGuard` points to either an existing value, or contains a
    /// new value to be inserted.
    pub fn is_some(&self) -> bool {
        matches!(
            self.entry_identifier,
            EntryIdentifier::Pos(_) | EntryIdentifier::New(_)
        )
    }

    /// Convert this `RouteEntryGuard` to an `Occupied` variant, panicking if the contained entry
    /// is none.
    pub fn unwrap(self) -> RouteEntryGuard<'a, 'b, Occupied> {
        assert!(
            !matches!(self.entry_identifier, EntryIdentifier::None),
            "Called RouteEntryGuard::unwrap on an emtpy entry guard"
        );

        // SAFETY: Transmuting to the same struct with a different marker, on a repr(C) struct.
        unsafe { std::mem::transmute::<Self, RouteEntryGuard<'a, 'b, Occupied>>(self) }
    }
}

impl Deref for RouteEntryGuard<'_, '_, Occupied> {
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

impl DerefMut for RouteEntryGuard<'_, '_, Occupied> {
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

impl<O> Drop for RouteEntryGuard<'_, '_, O> {
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
                            debug!(route_key = %rk, "Expired route entry for route key");
                            if let Err(e) =  expired_route_entry_sink.send(rk).await {
                                error!(route_key = %e.0, "Failed to send expired route key on cleanup channel");
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

/// Hold an exclusive write lock over the routing table. While this item is in scope, no other
/// calls can get a mutable refernce to the content of a routing table. Once this guard goes out of
/// scope, changes to the contained RouteList will be applied.
pub struct WriteGuard<'a> {
    writer: MutexGuard<'a, left_right::WriteHandle<RoutingTableInner, RoutingTableOplogEntry>>,
    /// Owned copy of the RouteList, this is populated once mutable access the the RouteList has
    /// been requested.
    value: Arc<RouteList>,
    owned: bool,
    /// Did the RouteList exist initially?
    exists: bool,
    /// The subnet we are writing to.
    subnet: Subnet,
    expired_route_entry_sink: mpsc::Sender<RouteKey>,
    cancellation_token: CancellationToken,
}

impl RoutingTable {
    /// Create a new empty RoutingTable. The passed channel is used to notify an external observer
    /// of route entry expiration events. It is the callers responsibility to ensure these events
    /// are properly handled.
    ///
    /// # Panics
    ///
    /// This will panic if not executed in the context of a tokio runtime.
    pub fn new(expired_route_entry_sink: mpsc::Sender<RouteKey>) -> Self {
        let (writer, reader) = left_right::new();
        let writer = Arc::new(Mutex::new(writer));

        let cancel_token = CancellationToken::new();

        RoutingTable {
            writer,
            reader,
            expired_route_entry_sink,
            cancel_token,
        }
    }

    /// Get a list of the routes for the most precises [`Subnet`] known which contains the given
    /// [`IpAddr`].
    pub fn best_routes(&self, ip: IpAddr) -> Option<Arc<RouteList>> {
        let IpAddr::V6(ip) = ip else {
            panic!("Only IPv6 is supported currently");
        };
        self.reader
            .enter()
            .expect("Write handle is saved on the router so it is not dropped yet.")
            .table
            .longest_match(ip)
            .map(|(_, _, rl)| rl)
            .cloned()
    }

    /// Get a list of all routes for the given subnet. Changes to the RoutingTable after this
    /// method returns will not be visible and require this method to be called again to be
    /// observed.
    pub fn routes(&self, subnet: Subnet) -> Option<Arc<RouteList>> {
        let subnet_ip = if let IpAddr::V6(ip) = subnet.address() {
            ip
        } else {
            return None;
        };

        self.reader
            .enter()
            .expect("Write handle is saved on the router so it is not dropped yet.")
            .table
            .exact_match(subnet_ip, subnet.prefix_len().into())
            .cloned()
    }

    /// Gets continued read access to the `RoutingTable`. While the returned
    /// [`guard`](RoutingTableReadGuard) is held, updates to the `RoutingTable` will be blocked.
    pub fn read(&self) -> RoutingTableReadGuard {
        RoutingTableReadGuard {
            guard: self
                .reader
                .enter()
                .expect("Write handle is saved on RoutingTable, so this is always Some; qed"),
        }
    }

    /// Locks the `RoutingTable` for continued write access. While the returned
    /// [`guard`](RoutingTableWriteGuard) is held, methods trying to mutate the `RoutingTable`, or
    /// get mutable access otherwise, will be blocked. When the [`guard`](`RoutingTableWriteGuard`)
    /// is dropped, all queued changes will be applied.
    pub fn write(&self) -> RoutingTableWriteGuard {
        RoutingTableWriteGuard {
            write_guard: Rc::new(RefCell::new(self.writer.lock().unwrap())),
            read_guard: self
                .reader
                .enter()
                .expect("Write handle is saved on RoutingTable, so this is always Some; qed"),
            expired_route_entry_sink: self.expired_route_entry_sink.clone(),
            cancel_token: self.cancel_token.clone(),
        }
    }

    /// Get mutable access to the list of routes for the given [`Subnet`].
    pub fn routes_mut(&self, subnet: Subnet) -> Option<WriteGuard> {
        let subnet_address = if let IpAddr::V6(ip) = subnet.address() {
            ip
        } else {
            panic!("IP v4 addresses are not supported")
        };

        let writer = self.writer.lock().unwrap();
        let value = writer
            .enter()
            .expect("Enter through write handle is always valid")
            .table
            .exact_match(subnet_address, subnet.prefix_len().into())?
            .clone();

        Some(WriteGuard {
            writer,
            // If we didn't find a route list in the route table we create a new empty list,
            // therefore we immediately own it.
            owned: false,
            value,
            exists: true,
            subnet,
            expired_route_entry_sink: self.expired_route_entry_sink.clone(),
            cancellation_token: self.cancel_token.clone(),
        })
    }

    /// Adds a new [`Subnet`] to the `RoutingTable`. The returned [`WriteGuard`] can be used to
    /// insert entries. If no entry is inserted before the guard is dropped, the [`Subnet`] won't
    /// be added.
    pub fn add_subnet(&self, subnet: Subnet, shared_secret: SharedSecret) -> WriteGuard {
        if !matches!(subnet.address(), IpAddr::V6(_)) {
            panic!("IP v4 addresses are not supported")
        };

        let writer = self.writer.lock().unwrap();
        let value = Arc::new(RouteList::new(shared_secret));

        WriteGuard {
            writer,
            value,
            owned: true,
            exists: false,
            subnet,
            expired_route_entry_sink: self.expired_route_entry_sink.clone(),
            cancellation_token: self.cancel_token.clone(),
        }
    }

    /// Gets the selected route for an IpAddr if one exists.
    ///
    /// # Panics
    ///
    /// This will panic if the IP address is not an IPV6 address.
    pub fn selected_route(&self, address: IpAddr) -> Option<RouteEntry> {
        let IpAddr::V6(ip) = address else {
            panic!("IP v4 addresses are not supported")
        };
        self.reader
            .enter()
            .expect("Write handle is saved on RoutingTable, so this is always Some; qed")
            .table
            .longest_match(ip)
            .and_then(|(_, _, rl)| {
                if !rl[0].selected() {
                    None
                } else {
                    Some(rl[0].clone())
                }
            })
    }
}

/// A write guard over the [`RoutingTable`]. While this guard is held, updates won't be able to
/// complete.
pub struct RoutingTableWriteGuard<'a> {
    // NOTE: Wrapping this in an Rc<RefCell<...>> might seem odd at first. The reason this is done
    // is as follows: this type is an intermediate type to allow a mutable iterator over the
    // routing table entries to exist. However, for such a mutable iterator, we need to share a
    // mutable reference to the MutexGuard. The internal left_right data structure does not allow
    // publishing of changes while we are holding a read handle at the same time (deadlock). But
    // the standard `Iterator` trait does not allow us to hand out items which reference the
    // iterator itself. So we can't lock the guard, and hand a reference to some smart pointer to
    // properly queue changes. We could _not_ lock the guard here, and do so on every call to
    // Iterator::next, locking only for the lifetime of the item. But that also has deadlock
    // problems: if the item returned is moved outside of the iterator loop (e.g. pushed to a vec),
    // the iterator will deadlock (we still need to keep the read handle as well for the iterator).
    // And even if this does not happen, we would need to queue the changes on the writehandle,
    // which means that any concurrent update from a different tread which publishes would lead to
    // a deadlock. We could work around this by using raw pointers, and a pin, and creating a
    // contract that the returned value is not kept around. This does require unsafe code. As an
    // alternative, for now we will keep an Rc, which we can clone so there is no reference as far
    // as the compiler is concerned, and a refcell. We then queue changes done during iteration.
    // Additionally, we will warn a potential user not to keep the values around, and in the drop
    // implementation of this type we will try to make sure there are no other Rc's pointing to the
    // same value.
    // TODO: This might be better by implementing some kind of `LendingIterator`.
    write_guard: Rc<
        RefCell<MutexGuard<'a, left_right::WriteHandle<RoutingTableInner, RoutingTableOplogEntry>>>,
    >,
    read_guard: left_right::ReadGuard<'a, RoutingTableInner>,
    expired_route_entry_sink: mpsc::Sender<RouteKey>,
    cancel_token: CancellationToken,
}

impl<'a, 'b> RoutingTableWriteGuard<'a> {
    pub fn iter_mut(&'b mut self) -> RoutingTableIterMut<'a, 'b> {
        RoutingTableIterMut::new(
            Rc::clone(&self.write_guard),
            self.read_guard.table.iter(),
            self.expired_route_entry_sink.clone(),
            self.cancel_token.clone(),
        )
    }
}

impl Drop for RoutingTableWriteGuard<'_> {
    fn drop(&mut self) {
        // Since we have a mutable reference here instead of ownership of self we can't consume the
        // Rc. So manually check reference count.
        if Rc::strong_count(&self.write_guard) != 1 {
            panic!("Dropping RoutingTableWriteGuard while there are outstanding references to the write guard lock");
        }

        self.write_guard.borrow_mut().publish();
    }
}

/// A read guard over the [`RoutingTable`]. While this guard is held, updates won't be able to
/// complete.
pub struct RoutingTableReadGuard<'a> {
    guard: left_right::ReadGuard<'a, RoutingTableInner>,
}

impl<'a> RoutingTableReadGuard<'a> {
    pub fn iter(&self) -> RoutingTableIter {
        RoutingTableIter::new(self.guard.table.iter())
    }
}

impl<'a> WriteGuard<'a> {
    /// Get [`RouteEntryGuard`] containing a [`RouteEntry`] for the given [`RouteKey`].
    pub fn entry_mut<'b>(&'b mut self, neighbour: &Peer) -> RouteEntryGuard<'a, 'b, PossiblyEmpty> {
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

        RouteEntryGuard {
            write_guard: self,
            delete: false,
            entry_identifier,
            _marker: std::marker::PhantomData,
        }
    }

    /// Ensures no [`route`] is selected for this [`Subnet`]. If no [`route`] was selected, this
    /// does nothing.
    ///
    /// [`route`]: RouteEntry
    pub fn unselect(&mut self) {
        if let Some((_, entry)) = self.list.get_mut(0) {
            entry.set_selected(false);
        }
    }
}

impl Deref for WriteGuard<'_> {
    type Target = RouteList;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl DerefMut for WriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.owned = true;
        Arc::make_mut(&mut self.value)
    }
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        // If owned is false, we never got a mutable reference, and the existing value is a
        // reference to the already existing value in the route table. So we don't have to do
        // anything here.
        if !self.owned {
            return;
        }

        // FIXME: try to get rid of clones on the Arc here
        match self.exists {
            // The route list did not exist, and now it is not empty, so an entry was added. We
            // need to add the route list to the routing table.
            false if !self.value.is_empty() => {
                trace!(subnet = %self.subnet, "Inserting new route list for subnet");
                self.writer.append(RoutingTableOplogEntry::Upsert(
                    self.subnet,
                    Arc::clone(&self.value),
                ));
            }
            // There was an existing route list which is now empty, so the entry for this subnet
            // needs to be deleted in the routing table.
            true if self.value.is_empty() => {
                trace!(subnet = %self.subnet, "Removing route list for subnet");
                self.writer
                    .append(RoutingTableOplogEntry::Delete(self.subnet));
            }
            // The value already existed, and was mutably accessed, so it was dissociated. Update
            // the routing table to point to the new value.
            true => {
                trace!(subnet = %self.subnet, "Updating route list for subnet");
                self.writer.append(RoutingTableOplogEntry::Upsert(
                    self.subnet,
                    Arc::clone(&self.value),
                ));
            }
            // The value did not exist and is still empty, so nothing was added. Nothing to do
            // here.
            false => {
                trace!(subnet = %self.subnet, "Unknown subnet had no routes and still doesn't have any");
            }
        }

        self.writer.publish();
    }
}

/// Operations allowed on the left_right for the routing table.
enum RoutingTableOplogEntry {
    /// Insert or Update the value for the given subnet.
    Upsert(Subnet, Arc<RouteList>),
    /// Delete the entry for the given subnet.
    Delete(Subnet),
}

/// Convert an [`IpAddr`] into an [`Ipv6Addr`]. Panics if the contained addrss is not an IPv6
/// address.
fn expect_ipv6(ip: IpAddr) -> Ipv6Addr {
    let IpAddr::V6(ip) = ip else {
        panic!("Expected ipv6 address")
    };

    ip
}

impl left_right::Absorb<RoutingTableOplogEntry> for RoutingTableInner {
    fn absorb_first(&mut self, operation: &mut RoutingTableOplogEntry, _other: &Self) {
        match operation {
            RoutingTableOplogEntry::Upsert(subnet, list) => {
                self.table.insert(
                    expect_ipv6(subnet.address()),
                    subnet.prefix_len().into(),
                    Arc::clone(list),
                );
            }
            RoutingTableOplogEntry::Delete(subnet) => {
                self.table
                    .remove(expect_ipv6(subnet.address()), subnet.prefix_len().into());
            }
        }
    }

    fn sync_with(&mut self, first: &Self) {
        for (k, ss, v) in first.table.iter() {
            self.table.insert(k, ss, v.clone());
        }
    }
}

impl Drop for RoutingTable {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}
