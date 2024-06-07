#![allow(dead_code)]

use std::{
    net::{IpAddr, Ipv6Addr},
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, MutexGuard},
};

use ip_network_table_deps_treebitmap::IpLookupTable;
use tokio::{
    select,
    sync::mpsc,
    task::AbortHandle,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    metric::Metric, peer::Peer, sequence_number::SeqNo, source_table::SourceKey, subnet::Subnet,
};

/// RouteKey uniquely defines a route via a peer.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteKey {
    subnet: Subnet,
    neighbor: Peer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteEntry {
    source: SourceKey,
    neighbour: Peer,
    metric: Metric,
    seqno: SeqNo,
    selected: bool,
    expires: Instant,
}

impl RouteEntry {
    pub fn new(
        source: SourceKey,
        neighbour: Peer,
        metric: Metric,
        seqno: SeqNo,
        selected: bool,
        expires: Instant,
    ) -> Self {
        Self {
            source,
            neighbour,
            metric,
            seqno,
            selected,
            expires,
        }
    }
}

/// The routing table holds a list of route entries for every known subnet.
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
#[derive(Default, Clone)]
pub struct RouteList {
    list: Vec<(Arc<AbortHandle>, RouteEntry)>,
}

impl RouteList {
    /// Create a new empty RouteList
    fn new() -> Self {
        Self::default()
    }

    /// Checks if there are any actual routes in the list.
    #[inline]
    fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Get a reference to the [`RouteEntry`] specified by the [`RouteKey`] from the list.
    pub fn entry(&self, route_key: &RouteKey) -> Option<&RouteEntry> {
        self.list
            .iter()
            .map(|(_, e)| e)
            .find(|entry| entry.neighbour == route_key.neighbor)
    }

    /// Get [`RouteEntryGuard`] containing a [`RouteEntry`] for the given [`RouteKey`].
    pub fn entry_mut(&mut self, route_key: &RouteKey) -> RouteEntryGuard<'_, PossiblyEmpty> {
        let entry_identifier = if let Some(pos) = self
            .list
            .iter_mut()
            .map(|(_, e)| e)
            .position(|entry| entry.neighbour == route_key.neighbor)
        {
            EntryIdentifier::Pos(pos)
        } else {
            EntryIdentifier::None
        };

        RouteEntryGuard {
            route_list: self,
            entry_identifier,
            timer_duration: None,
            _marker: std::marker::PhantomData,
        }
    }
}

pub struct RouteEntryGuard<'a, O> {
    route_list: &'a mut RouteList,
    entry_identifier: EntryIdentifier,
    timer_duration: Option<Duration>,
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

impl<'a> RouteEntryGuard<'a, PossiblyEmpty> {
    /// Inserts the given [`RouteEntry`] if it does not exist yet. If an entry already exists, this
    /// does nothing.
    pub fn or_insert(mut self, re: RouteEntry) -> RouteEntryGuard<'a, Occupied> {
        if matches!(self.entry_identifier, EntryIdentifier::None) {
            self.entry_identifier = EntryIdentifier::New(re)
        };

        // SAFETY: Transmuting to the same struct with a different marker.
        unsafe { std::mem::transmute(self) }
    }
}

impl Deref for RouteEntryGuard<'_, Occupied> {
    type Target = RouteEntry;

    fn deref(&self) -> &Self::Target {
        match self.entry_identifier {
            EntryIdentifier::Pos(pos) => &self.route_list.list[pos].1,
            EntryIdentifier::New(ref re) => re,
            EntryIdentifier::None => {
                unreachable!()
            }
        }
    }
}

impl DerefMut for RouteEntryGuard<'_, Occupied> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self.entry_identifier {
            EntryIdentifier::Pos(pos) => &mut self.route_list.list[pos].1,
            EntryIdentifier::New(ref mut re) => re,
            EntryIdentifier::None => {
                unreachable!()
            }
        }
    }
}

impl<O> Drop for RouteEntryGuard<'_, O> {
    fn drop(&mut self) {
        todo!()
    }
}

enum RoutingTableOplogEntry {}

/// Hold an exclusive write lock over the routing table. While this item is in scope, no other
/// calls can get a mutable refernce to the content of a routing table. Once this guard goes out of
/// scope, changes to the contained RouteList will be applied.
pub struct WriteGuard<'a> {
    writer: MutexGuard<'a, left_right::WriteHandle<RoutingTableInner, RoutingTableOplogEntry>>,
    /// Owned copy of the RouteList, this is populated once mutable access the the RouteList has
    /// been requested.
    //owned: Option<Arc<RouteList>>,
    value: Arc<RouteList>,
    owned: bool,
    /// Did the RouteList exist initially?
    exists: bool,
    /// The subnet we are writing to.
    subnet: Subnet,
}

impl RoutingTable {
    /// Create a new empty RoutingTable.
    ///
    /// # Panics
    ///
    /// This will panic if not executed in the context of a tokio runtime.
    pub fn new() -> Self {
        let (writer, reader) = left_right::new();
        let writer = Arc::new(Mutex::new(writer));

        let (sink, mut stream) = mpsc::channel(1);

        let cancel_token = CancellationToken::new();

        {
            let cancel_token = cancel_token.clone();
            tokio::spawn(async move {
                loop {
                    select! {
                        _ = cancel_token.cancelled() => {
                            debug!("Cancel token awaited, aborting route expiration cleanup task");
                            return;
                        }
                        erk = stream.recv() => {
                            if let Some(expired_route_key) = erk {
                                debug!(route_key = ?expired_route_key, "Route expired");
                                todo!();

                            } else {
                                debug!("All expired route key sinks closed");
                                break

                            }
                        }

                    }
                }

                warn!("Expired route cleanup task exits");
            });
        }

        RoutingTable {
            writer,
            reader,
            expired_route_entry_sink: sink,
            cancel_token,
        }
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

    pub fn routes_mut(&self, subnet: Subnet) -> WriteGuard {
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
            .exact_match(subnet_address, subnet.prefix_len().into())
            .cloned();
        let exists = value.is_some();

        WriteGuard {
            writer,
            // If we didn't find a route list in the route table we create a new empty list,
            // therefore we immediately own it.
            owned: value.is_none(),
            value: if let Some(value) = value {
                value
            } else {
                Arc::new(RouteList::new())
            },
            exists,
            subnet,
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

        match self.exists {
            // The route list did not exist, and now it is not empty, so an entry was added. We
            // need to add the route list to the routing table.
            false if !self.value.is_empty() => {
                todo!();
            }
            // There was an existing route list which is now empty, to the entry for this subnet
            // needs to be deleted in the routing table.
            true if self.value.is_empty() => {
                todo!();
            }
            // The value already existed, and was mutably accessed, so it was dissociated. Update
            // the routing table to point to the new value.
            true => {
                todo!()
            }
            // The value did not exist and is still empty, so nothing was added. Nothing to do
            // here.
            false => {}
        }
    }
}

impl left_right::Absorb<RoutingTableOplogEntry> for RoutingTableInner {
    fn absorb_first(&mut self, _operation: &mut RoutingTableOplogEntry, _other: &Self) {
        todo!()
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
