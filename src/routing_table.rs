use ip_network_table_deps_treebitmap::IpLookupTable;
use log::{error, warn};
use tokio::{sync::mpsc, task::JoinHandle};

use crate::{
    metric::Metric, peer::Peer, router_id::RouterId, sequence_number::SeqNo,
    source_table::SourceKey, subnet::Subnet,
};
use core::fmt;
use std::{
    net::{IpAddr, Ipv6Addr},
    time::Duration,
};

/// Default time before a route expires.
const DEFAULT_ROUTE_EXPIRATION: Duration = Duration::from_secs(60);

/// Information about a routes expiration.
pub enum RouteExpirationType {
    /// Route should be retracted.
    Retract,
    /// Route should be flushed from the table.
    Remove,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteKey {
    subnet: Subnet,
    neighbor: Peer,
}

impl Eq for RouteKey {}

impl fmt::Display for RouteKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "{} via {}",
            self.subnet,
            self.neighbor.underlay_ip()
        ))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteEntry {
    source: SourceKey,
    neighbor: Peer,
    metric: Metric,
    seqno: SeqNo,
    selected: bool,
}

impl RouteKey {
    /// Create a new `RouteKey` with the given values.
    #[inline]
    pub const fn new(subnet: Subnet, neighbor: Peer) -> Self {
        Self { subnet, neighbor }
    }

    /// Returns the [`Subnet`] associated with this `RouteKey`.
    #[inline]
    pub const fn subnet(&self) -> Subnet {
        self.subnet
    }

    /// Returns the [`neighbour`](Peer) associated with this `RouteKey`.
    #[inline]
    pub fn neighbour(&self) -> &Peer {
        &self.neighbor
    }
}

impl RouteEntry {
    /// Create a new `RouteEntry`.
    pub const fn new(
        source: SourceKey,
        neighbor: Peer,
        metric: Metric,
        seqno: SeqNo,
        selected: bool,
    ) -> Self {
        Self {
            source,
            neighbor,
            metric,
            seqno,
            selected,
        }
    }

    /// Returns the [`SourceKey`] associated with this `RouteEntry`.
    pub const fn source(&self) -> SourceKey {
        self.source
    }

    /// Returns the [`SeqNo`] associated with this `RouteEntry`.
    pub const fn seqno(&self) -> SeqNo {
        self.seqno
    }

    /// Returns the metric associated with this `RouteEntry`.
    pub const fn metric(&self) -> Metric {
        self.metric
    }

    /// Return the (neighbour)[`Peer`] associated with this `RouteEntry`.
    pub fn neighbour(&self) -> &Peer {
        &self.neighbor
    }

    /// Indicates this `RouteEntry` is the selected route for the destination.
    pub const fn selected(&self) -> bool {
        self.selected
    }

    /// Updates the metric of this `RouteEntry` to the given value.
    pub fn update_metric(&mut self, metric: Metric) {
        self.metric = metric;
    }

    /// Updates the seqno of this `RouteEntry` to the given value.
    pub fn update_seqno(&mut self, seqno: SeqNo) {
        self.seqno = seqno;
    }

    /// Updates the source [`RouterId`] of this `RouteEntry` to the given value.
    pub fn update_router_id(&mut self, router_id: RouterId) {
        self.source.set_router_id(router_id);
    }

    /// Sets whether or not this `RouteEntry` is the selected route for the associated [`Peer`].
    pub fn set_selected(&mut self, selected: bool) {
        self.selected = selected
    }
}

/// The `RoutingTable` contains all known subnets, and the associated [`RouteEntry`]'s.
pub struct RoutingTable {
    table: IpLookupTable<Ipv6Addr, Vec<(RouteEntry, JoinHandle<()>)>>,
}

impl RoutingTable {
    /// Create a new, empty `RoutingTable`.
    pub fn new() -> Self {
        Self {
            table: IpLookupTable::new(),
        }
    }

    /// Get a  reference to the [`RouteEntry`] associated with the [`RouteKey`] if one is
    /// present in the table.
    pub fn get(&self, key: &RouteKey) -> Option<&RouteEntry> {
        let addr = match key.subnet.address() {
            IpAddr::V6(addr) => addr,
            _ => return None,
        };

        self.table
            .exact_match(addr, key.subnet.prefix_len() as u32)?
            .iter()
            .map(|(entry, _)| entry)
            .find(|entry| entry.neighbor == key.neighbor)
    }

    /// Get a mutable reference to the [`RouteEntry`] associated with the [`RouteKey`] if one is
    /// present in the table.
    pub fn get_mut(&mut self, key: &RouteKey) -> Option<&mut RouteEntry> {
        let addr = match key.subnet.address() {
            IpAddr::V6(addr) => addr,
            _ => return None,
        };

        self.table
            .exact_match_mut(addr, key.subnet.prefix_len() as u32)?
            .iter_mut()
            .map(|(entry, _)| entry)
            .find(|entry| entry.neighbor == key.neighbor)
    }

    /// Insert a new [`RouteEntry`] in the table. If there is already an entry for the
    /// [`RouteKey`], the existing entry is removed.
    ///
    /// Currenly only IPv6 is supported, attempting to insert an IPv4 network does nothing.
    pub fn insert(
        &mut self,
        key: RouteKey,
        entry: RouteEntry,
        expired_route_entry_sink: mpsc::Sender<(RouteKey, RouteExpirationType)>,
    ) {
        // We make sure that the selected route has index 0 in the entry list (if there is one).
        // This effectively makes the lookup of a selected route O(1), whereas a lookup of a non
        // selected route is O(n), n being the amount of Peers (as this is the differentiating part
        // in a key for a subnet).
        let addr = match key.subnet.network() {
            IpAddr::V6(addr) => addr,
            _ => return,
        };
        let selected = entry.selected;
        let expiration = tokio::spawn({
            let key = key.clone();
            let t = if entry.metric().is_infinite() {
                RouteExpirationType::Remove
            } else {
                RouteExpirationType::Retract
            };
            async move {
                tokio::time::sleep(DEFAULT_ROUTE_EXPIRATION).await;

                if let Err(e) = expired_route_entry_sink.send((key, t)).await {
                    error!("Failed to notify router of expired key {e}");
                }
            }
        });
        match self
            .table
            .exact_match_mut(addr, key.subnet.prefix_len() as u32)
        {
            Some(entries) => {
                let new_elem_idx = if let Some(idx) = entries
                    .iter()
                    .map(|(entry, _)| entry)
                    .position(|entry| entry.neighbor == key.neighbor)
                {
                    // Overwrite entry if one exists for the key but cancel the timer.
                    entries[idx].1.abort();
                    entries[idx] = (entry, expiration);
                    idx
                } else {
                    entries.push((entry, expiration));
                    entries.len() - 1
                };
                // In debug mode, verify that we only have 1 selected route at most. We do this by
                // checking if entry 0 is not overwritten (the possibly selected route) and if
                // that is selected.
                debug_assert!(if new_elem_idx != 0 && selected {
                    !entries[0].0.selected
                } else {
                    true
                });
                // If the inserted entry is selected, swap it to index 0 so it is at the start of
                // the list.
                if selected {
                    entries.swap(0, new_elem_idx);
                }
            }
            None => {
                self.table.insert(
                    addr,
                    key.subnet.prefix_len() as u32,
                    vec![(entry, expiration)],
                );
            }
        };
    }

    /// Make sure there is no [`RouteEntry`] in the table for a given [`RouteKey`]. If an entry
    /// existed prior to calling this, it is returned.
    ///
    /// Currenly only IPv6 is supported, attempting to remove an IPv4 network does nothing.
    pub fn remove(&mut self, key: &RouteKey) -> Option<RouteEntry> {
        let addr = match key.subnet.network() {
            IpAddr::V6(addr) => addr,
            _ => return None,
        };
        match self
            .table
            .exact_match_mut(addr, key.subnet.prefix_len() as u32)
        {
            Some(entries) => {
                let elem = entries
                    .iter()
                    .position(|(entry, _)| entry.neighbor == key.neighbor)
                    .map(|idx| entries.swap_remove(idx));
                // Remove the table entry entirely if the vec is empty
                // NOTE: we don't care if val is some, we only care that no empty set is left
                // behind.
                if entries.is_empty() {
                    self.table.remove(addr, key.subnet.prefix_len() as u32);
                }
                elem.map(|(entry, expiration)| {
                    expiration.abort();
                    entry
                })
            }
            None => None,
        }
    }

    /// Create an iterator over all key value pairs in the table.
    // TODO: remove this?
    pub fn iter(&self) -> impl Iterator<Item = (RouteKey, &'_ RouteEntry)> {
        self.table.iter().flat_map(|(addr, prefix, entries)| {
            entries.iter().map(move |(value, _)| {
                (
                    RouteKey::new(
                        Subnet::new(addr.into(), prefix as u8)
                            .expect("Only proper subnets are inserted in the table; qed"),
                        value.neighbor.clone(),
                    ),
                    value,
                )
            })
        })
    }

    /// Look up a selected route for an [`IpAddr`] in the `RoutingTable`.
    ///
    /// Currently only IPv6 is supported, looking up an IPv4 address always returns [`Option::None`].
    /// In the event where 2 distinc routes are inserted with selected set to true, the entry with
    /// the minimum `Metric` is selected. Note that it is an error to have 2 such entries, and this
    /// might result in a panic later.
    pub fn lookup_selected(&self, ip: IpAddr) -> Option<RouteEntry> {
        let addr = match ip {
            IpAddr::V6(addr) => addr,
            _ => return None,
        };
        let entries = self.table.longest_match(addr)?.2;
        if entries.is_empty() {
            // This is a logic error in our code, but don't crash as it is recoverable.
            warn!("Empty route entry list for {ip}, this is a bug");
            return None;
        }
        if entries[0].0.selected {
            Some(entries[0].0.clone())
        } else {
            None
        }
    }

    /// Get a copy of all fallback route entries for an [`IpAddr`] in the `RoutingTable`.
    ///
    /// Currently only IPv6 is supported, looking up an IPv4 address always returns an empty
    /// [`Vec`].
    pub fn lookup_fallbacks(&self, ip: IpAddr) -> Vec<RouteEntry> {
        let addr = match ip {
            IpAddr::V6(addr) => addr,
            _ => return Vec::new(),
        };
        self.table
            .longest_match(addr)
            .map(|(_, _, entries)| entries.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter_map(|(entry, _)| if !entry.selected { Some(entry) } else { None })
            .cloned()
            .collect()
    }

    /// Unselects a route defined by the [`RouteKey`]. This means there will no longer be a
    /// selected route for the subnet defined by the [`RouteKey`].
    ///
    /// # Panics
    ///
    /// This function panics if the [`RouteKey`] does not exist, or the associated [`RouteEntry`]
    /// is not selected.
    pub fn unselect_route(&mut self, key: &RouteKey) {
        let addr = match key.subnet.network() {
            IpAddr::V6(addr) => addr,
            // Panic is fine here as we documented that the RouteKey must exist and that is
            // obviously not the case.
            _ => panic!("RouteKey must exist, so it can't be IPv4"),
        };
        let entries = self
            .table
            .exact_match_mut(addr, key.subnet.prefix_len() as u32)
            .expect("There is an entry for the provided RouteKey");
        // No need for bounds check, RouteKey must exist so there must be at least 1 element.
        // The message on this assert assumes the invariant that the selected route is the first
        // element holds
        assert!(
            entries[0].0.neighbor == key.neighbor,
            "Attempted to unselect route which has a different RouteKey"
        );
        assert!(
            entries[0].0.selected,
            "Attempted to unselected a route which isn't selected"
        );
        entries[0].0.selected = false;
    }

    /// Selects a route defined by the [`RouteKey`]. This means the route defined by the [`RouteKey`]
    /// will become the selected route for the subnet defined by the [`RouteKey`].
    ///
    /// # Panics
    ///
    /// This function panics if the [`RouteKey`] does not exist.
    pub fn select_route(&mut self, key: &RouteKey) {
        let addr = match key.subnet.network() {
            IpAddr::V6(addr) => addr,
            // Panic is fine here as we documented that the RouteKey must exist and that is
            // obviously not the case.
            _ => panic!("RouteKey must exist, so it can't be IPv4"),
        };
        let entries = self
            .table
            .exact_match_mut(addr, key.subnet.prefix_len() as u32)
            .expect("There is an entry for the provided RouteKey");
        // No need for bounds check, RouteKey must exist so there must be at least 1 element.
        // The message on this assert assumes the invariant that the selected route is the first
        // element holds
        // Unconditionally set the selected flag on the first element to false. If there is no
        // selected route, this is a no-op, otherwise it makes sure there is only 1 selected route
        // when we toggle the potentially new route.
        entries[0].0.selected = false;
        let entry_idx = entries
            .iter()
            .position(|(entry, _)| entry.neighbor == key.neighbor)
            .expect("Route entry must be present in route table to select it");
        entries[entry_idx].0.selected = true;
        // Maintain invariant that selected route comes first.
        entries.swap(0, entry_idx);
    }

    /// Get all entries associated with a [`Subnet`]. If a route is selected, it will be the first
    /// entry in the list.
    pub fn entries(&self, subnet: Subnet) -> Vec<RouteEntry> {
        let addr = match subnet.network() {
            IpAddr::V6(addr) => addr,
            // Panic is fine here as we documented that the RouteKey must exist and that is
            // obviously not the case.
            _ => panic!("RouteKey must exist, so it can't be IPv4"),
        };
        self.table
            .exact_match(addr, subnet.prefix_len() as u32)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|(entry, _)| entry)
            .cloned()
            .collect()
    }

    /// Resets the timer associated with a route.
    pub fn reset_route_timer(
        &mut self,
        key: &RouteKey,
        expired_route_entry_sink: mpsc::Sender<(RouteKey, RouteExpirationType)>,
    ) {
        let addr = match key.subnet.network() {
            IpAddr::V6(addr) => addr,
            _ => return,
        };

        if let Some(entries) = self
            .table
            .exact_match_mut(addr, key.subnet.prefix_len() as u32)
        {
            if let Some(idx) = entries
                .iter()
                .map(|(entry, _)| entry)
                .position(|entry| entry.neighbor == key.neighbor)
            {
                // Cancel old entry timer and keep track of the new one.
                entries[idx].1.abort();
                let t = if entries[idx].0.metric().is_infinite() {
                    RouteExpirationType::Remove
                } else {
                    RouteExpirationType::Retract
                };
                let expiration = tokio::spawn({
                    let key = key.clone();
                    async move {
                        tokio::time::sleep(DEFAULT_ROUTE_EXPIRATION).await;

                        if let Err(e) = expired_route_entry_sink.send((key, t)).await {
                            error!("Failed to notify router of expired key {e}");
                        }
                    }
                });
                entries[idx].1 = expiration;
            };
        };
    }
}
