#![allow(dead_code)]

use std::{
    net::{IpAddr, Ipv6Addr},
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, MutexGuard},
};

use ip_network_table_deps_treebitmap::IpLookupTable;
use tokio::{sync::mpsc, task::AbortHandle, time::Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace};

use crate::{
    metric::Metric, peer::Peer, sequence_number::SeqNo, source_table::SourceKey, subnet::Subnet,
};

/// RouteKey uniquely defines a route via a peer.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteKey {
    subnet: Subnet,
    neighbour: Peer,
}

impl RouteKey {
    /// Creates a new `RouteKey` for the given [`Subnet`] and [`neighbour`](Peer).
    pub fn new(subnet: Subnet, neighbour: Peer) -> Self {
        Self { subnet, neighbour }
    }

    /// Get's the [`Subnet`] identified by this `RouteKey`.
    pub fn subnet(&self) -> Subnet {
        self.subnet
    }

    /// Gets the [`neighbour`](Peer) identified by this `RouteKey`.
    pub fn neighbour(&self) -> &Peer {
        &self.neighbour
    }
}

/// RouteEntry holds all relevant information about a specific route. Since this includes the next
/// hop, a single subnet can have multiple route entries.
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
    /// Create a new `RouteEntry` with the provided values.
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

    /// Return the [`SourceKey`] for this `RouteEntry`.
    pub fn source(&self) -> SourceKey {
        self.source
    }

    /// Return the [`neighbour`](Peer) used as next hop for this `RouteEntry`.
    pub fn neighbour(&self) -> &Peer {
        &self.neighbour
    }

    /// Return the [`Metric`] of this `RouteEntry`.
    pub fn metric(&self) -> Metric {
        self.metric
    }

    /// Return the [`sequence number`](SeqNo) for the `RouteEntry`.
    pub fn seqno(&self) -> SeqNo {
        self.seqno
    }

    /// Return if this [`RouteEntry`] is selected.
    pub fn selected(&self) -> bool {
        self.selected
    }

    /// Return the [`Instant`] when this `RouteEntry` expires if it doesn't get updated before
    /// then.
    pub fn expires(&self) -> Instant {
        self.expires
    }

    /// Set the [`SourceKey`] for this `RouteEntry`.
    pub fn set_source(&mut self, source: SourceKey) {
        self.source = source;
    }

    /// Sets the [`neighbour`](Peer) for this `RouteEntry`.
    pub fn set_neighbour(&mut self, neighbour: Peer) {
        self.neighbour = neighbour;
    }

    /// Sets the [`Metric`] for this `RouteEntry`.
    pub fn set_metric(&mut self, metric: Metric) {
        self.metric = metric;
    }

    /// Sets the [`sequence number`](SeqNo) for this `RouteEntry`.
    pub fn set_seqno(&mut self, seqno: SeqNo) {
        self.seqno = seqno;
    }

    /// Sets if this `RouteEntry` is the selected route for the associated [`Subnet`].
    pub fn set_selected(&mut self, selected: bool) {
        self.selected = selected;
    }

    /// Sets the expiration time for this [`RouteEntry`].
    pub fn set_expires(&mut self, expires: Instant) {
        self.expires = expires;
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
            .find(|entry| entry.neighbour == route_key.neighbour)
    }
}

/// A guard which allows write access to a single [`RouteEntry`].
#[repr(C)] // This is needed since we transmute between instances with different markers.
pub struct RouteEntryGuard<'a, 'b, O> {
    write_guard: &'b mut WriteGuard<'a>,
    entry_identifier: EntryIdentifier,
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

impl<'a, 'b> RouteEntryGuard<'a, 'b, PossiblyEmpty> {
    /// Inserts the given [`RouteEntry`] if it does not exist yet. If an entry already exists, this
    /// does nothing.
    pub fn or_insert(mut self, re: RouteEntry) -> RouteEntryGuard<'a, 'b, Occupied> {
        if matches!(self.entry_identifier, EntryIdentifier::None) {
            self.entry_identifier = EntryIdentifier::New(re)
        };

        // SAFETY: Transmuting to the same struct with a different marker, on a repr(C) struct.
        unsafe { std::mem::transmute(self) }
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
        let expired_route_entry_sink = self.write_guard.expired_route_entry_sink.clone();
        let cancellation_token = self.write_guard.cancellation_token.clone();

        let spawn_timer = |re: &RouteEntry| {
            let expiration = re.expires;
            let rk = RouteKey::new(re.source.subnet(), re.neighbour.clone());
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

    /// Get mutable access to the list of routes for the given [`Subnet`].
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
            expired_route_entry_sink: self.expired_route_entry_sink.clone(),
            cancellation_token: self.cancel_token.clone(),
        }
    }
}

impl<'a> WriteGuard<'a> {
    /// Get [`RouteEntryGuard`] containing a [`RouteEntry`] for the given [`RouteKey`].
    pub fn entry_mut<'b>(
        &'b mut self,
        route_key: &RouteKey,
    ) -> RouteEntryGuard<'a, 'b, PossiblyEmpty> {
        let entry_identifier = if let Some(pos) = self
            .list
            .iter_mut()
            .map(|(_, e)| e)
            .position(|entry| entry.neighbour == route_key.neighbour)
        {
            EntryIdentifier::Pos(pos)
        } else {
            EntryIdentifier::None
        };

        RouteEntryGuard {
            write_guard: self,
            entry_identifier,
            _marker: std::marker::PhantomData,
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
                trace!(subnet = ?self.subnet, "Inserting new route list for subnet");
                self.writer.append(RoutingTableOplogEntry::Upsert(
                    self.subnet,
                    Arc::clone(&self.value),
                ));
            }
            // There was an existing route list which is now empty, so the entry for this subnet
            // needs to be deleted in the routing table.
            true if self.value.is_empty() => {
                trace!(subnet = ?self.subnet, "Removing route list for subnet");
                self.writer
                    .append(RoutingTableOplogEntry::Delete(self.subnet));
            }
            // The value already existed, and was mutably accessed, so it was dissociated. Update
            // the routing table to point to the new value.
            true => {
                trace!(subnet = ?self.subnet, "Updating route list for subnet");
                self.writer.append(RoutingTableOplogEntry::Upsert(
                    self.subnet,
                    Arc::clone(&self.value),
                ));
            }
            // The value did not exist and is still empty, so nothing was added. Nothing to do
            // here.
            false => {
                trace!(subnet = ?self.subnet, "Unknown subnet had no routes and still doesn't have any");
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

#[cfg(test)]
mod tests {
    use std::{
        net::Ipv6Addr,
        sync::{atomic::AtomicU64, Arc},
    };

    use tokio::sync::mpsc;

    use crate::{
        crypto::SecretKey, metric::Metric, peer::Peer, router_id::RouterId, sequence_number::SeqNo,
        source_table::SourceKey, subnet::Subnet,
    };

    #[tokio::test]
    async fn routing_table_base_flow() {
        let sk = SecretKey::new();
        let pk = (&sk).into();
        let sn = Subnet::new(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 1).into(), 64)
            .expect("Valid subnet in test case");
        let rid = RouterId::new(pk);

        let (router_data_tx, _router_data_rx) = mpsc::channel(1);
        let (router_control_tx, _router_control_rx) = mpsc::unbounded_channel();
        let (dead_peer_sink, _dead_peer_stream) = mpsc::channel(1);
        let (con1, _con2) = tokio::io::duplex(1500);
        let neighbour = Peer::new(
            router_data_tx,
            router_control_tx,
            con1,
            dead_peer_sink,
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .expect("Can create a dummy peer");

        let source_key = SourceKey::new(sn, rid);

        let (erk_sink, _erk_stream) = mpsc::channel(1);

        let rt = super::RoutingTable::new(erk_sink);

        let rk = super::RouteKey::new(sn, neighbour.clone());

        let mut route_list = rt.routes_mut(sn);

        assert!(route_list.is_empty());

        let entry = route_list.entry_mut(&rk);

        let entry = entry.or_insert(super::RouteEntry::new(
            source_key,
            neighbour,
            Metric::new(0),
            SeqNo::new(),
            false,
            tokio::time::Instant::now() + tokio::time::Duration::from_secs(600),
        ));
        drop(entry);

        assert!(!route_list.is_empty());
    }
}
