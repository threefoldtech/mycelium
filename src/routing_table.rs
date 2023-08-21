use crate::{
    crypto::PublicKey, metric::Metric, peer::Peer, sequence_number::SeqNo, source_table::SourceKey,
};
use std::{collections::BTreeMap, net::IpAddr};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RouteKey {
    prefix: IpAddr,
    plen: u8,
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
    pub const fn new(prefix: IpAddr, plen: u8, neighbor: IpAddr) -> Self {
        Self {
            prefix,
            plen,
            neighbor,
        }
    }

    /// Returns the prefix associated with this `RouteKey`.
    #[inline]
    pub const fn prefix(&self) -> IpAddr {
        self.prefix
    }

    /// Returns the plen associated with this `RouteKey`.
    #[inline]
    pub const fn plen(&self) -> u8 {
        self.plen
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
    pub table: BTreeMap<RouteKey, RouteEntry>,
}

impl RoutingTable {
    pub fn new() -> Self {
        Self {
            table: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key: RouteKey, entry: RouteEntry) {
        self.table.insert(key, entry);
        //println!("Added route to routing table: {:?}", self.table);
    }

    pub fn remove(&mut self, key: &RouteKey) -> Option<RouteEntry> {
        self.table.remove(key)
    }
}
