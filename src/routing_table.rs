use crate::{
    crypto::PublicKey, metric::Metric, peer::Peer, sequence_number::SeqNo, source_table::SourceKey,
    subnet::Subnet,
};
use std::{collections::BTreeMap, net::IpAddr};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RouteKey {
    subnet: Subnet,
    neighbor: IpAddr,
}

#[derive(Debug, Clone)]
pub struct RouteEntry {
    source: SourceKey,
    neighbor: Peer,
    metric: Metric, // If metric is 0xFFFF, the route has recently been retracted
    seqno: SeqNo,
    selected: bool,
}

impl RouteKey {
    /// Create a new `RouteKey` with the given values.
    #[inline]
    pub const fn new(subnet: Subnet, neighbor: IpAddr) -> Self {
        Self { subnet, neighbor }
    }

    /// Returns the [`Subnet`] associated with this `RouteKey`.
    #[inline]
    pub const fn subnet(&self) -> Subnet {
        self.subnet
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

    /// Returns the metric associated with this `RouteEntry`.
    pub const fn metric(&self) -> Metric {
        self.metric
    }

    /// Return the address of the next hop [`Peer`] associated with this `RouteEntry`.
    pub fn next_hop(&self) -> IpAddr {
        self.neighbor.overlay_ip()
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

    /// Updates the source router id of this `RouteEntry` to the given value.
    pub fn update_router_id(&mut self, router_id: PublicKey) {
        self.source.set_router_id(router_id);
    }

    /// Sets whether or not this `RouteEntry` is the selected route for the associated [`Peer`].
    pub fn set_selected(&mut self, selected: bool) {
        self.selected = selected
    }
}

#[derive(Debug, Clone)]
pub struct RoutingTable {
    // TODO: we might need a better structure for this.
    table: BTreeMap<RouteKey, RouteEntry>,
}

impl RoutingTable {
    /// Create a new, empty `RoutingTable`.
    pub fn new() -> Self {
        Self {
            table: BTreeMap::new(),
        }
    }

    /// Get a reference to the [`RouteEntry`] associated with the [`RouteKey`] if one is present in
    /// the table.
    pub fn get(&self, key: &RouteKey) -> Option<&RouteEntry> {
        self.table.get(key)
    }

    /// Get a mutablereference to the [`RouteEntry`] associated with the [`RouteKey`] if one is
    /// present in the table.
    pub fn get_mut(&mut self, key: &RouteKey) -> Option<&mut RouteEntry> {
        self.table.get_mut(key)
    }

    /// Insert a new [`RouteEntry`] in the table. If there is already an entry for the
    /// [`RouteKey`], the existing entry is removed.
    pub fn insert(&mut self, key: RouteKey, entry: RouteEntry) {
        self.table.insert(key, entry);
    }

    /// Make sure there is no [`RouteEntry`] in the table for a given [`RouteKey`]. If an entry
    /// existed prior to calling this, it is returned.
    pub fn remove(&mut self, key: &RouteKey) -> Option<RouteEntry> {
        self.table.remove(key)
    }

    /// Create an iterator over all key value pairs in the table.
    // TODO: remove this?
    pub fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a RouteKey, &'a RouteEntry)> {
        self.table.iter()
    }

    /// Checks if there is an entry for the given [`RouteKey`].
    pub fn contains_key(&self, key: &RouteKey) -> bool {
        self.table.contains_key(key)
    }

    /// Only maintain the [`RouteEntry`]'s indicated by the predicate.
    pub fn retain<F>(&mut self, f: F)
    where
        F: FnMut(&RouteKey, &mut RouteEntry) -> bool,
    {
        self.table.retain(f)
    }
}
