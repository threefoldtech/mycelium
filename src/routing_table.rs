use ip_network_table_deps_treebitmap::IpLookupTable;

use crate::{
    metric::Metric, peer::Peer, router_id::RouterId, sequence_number::SeqNo,
    source_table::SourceKey, subnet::Subnet,
};
use std::{
    cmp::Ordering,
    net::{IpAddr, Ipv6Addr},
};

#[derive(Debug, Clone, PartialEq)]
pub struct RouteKey {
    subnet: Subnet,
    neighbor: Peer,
}

impl Eq for RouteKey {}
impl PartialOrd for RouteKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match self.subnet.partial_cmp(&other.subnet) {
            Some(Ordering::Equal) => self
                .neighbor
                .overlay_ip()
                .partial_cmp(&other.neighbor.overlay_ip()),
            ord => ord,
        }
    }
}
impl Ord for RouteKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.subnet.cmp(&other.subnet) {
            Ordering::Equal => self.neighbor.overlay_ip().cmp(&other.neighbor.overlay_ip()),
            ord => ord,
        }
    }
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
    pub const fn new(subnet: Subnet, neighbor: Peer) -> Self {
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

pub struct RoutingTable {
    // TODO: we might need a better structure for this.
    //table: BTreeMap<RouteKey, RouteEntry>,
    table: IpLookupTable<Ipv6Addr, RouteEntry>,
}

impl RoutingTable {
    /// Create a new, empty `RoutingTable`.
    pub fn new() -> Self {
        Self {
            table: IpLookupTable::new(),
        }
    }

    /// Get a reference to the [`RouteEntry`] associated with the [`RouteKey`] if one is present in
    /// the table.
    pub fn get(&self, key: &RouteKey) -> Option<&RouteEntry> {
        let addr = match key.subnet.address() {
            IpAddr::V6(addr) => addr,
            _ => return None,
        };

        let val = self
            .table
            .exact_match(addr, key.subnet.prefix_len() as u32)?;
        if val.neighbor == key.neighbor {
            return Some(val);
        }

        None
    }

    /// Get a mutablereference to the [`RouteEntry`] associated with the [`RouteKey`] if one is
    /// present in the table.
    pub fn get_mut(&mut self, key: &RouteKey) -> Option<&mut RouteEntry> {
        let addr = match key.subnet.address() {
            IpAddr::V6(addr) => addr,
            _ => return None,
        };

        let val = self
            .table
            .exact_match_mut(addr, key.subnet.prefix_len() as u32)?;
        if val.neighbor == key.neighbor {
            return Some(val);
        }

        None
    }

    /// Insert a new [`RouteEntry`] in the table. If there is already an entry for the
    /// [`RouteKey`], the existing entry is removed.
    ///
    /// Currenly only IPv6 is supported, attempting to insert an IPv4 network does nothing.
    pub fn insert(&mut self, key: RouteKey, entry: RouteEntry) {
        let addr = match key.subnet.network() {
            IpAddr::V6(addr) => addr,
            _ => return,
        };
        self.table
            .insert(addr, key.subnet.prefix_len() as u32, entry);
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
        self.table.remove(addr, key.subnet.prefix_len() as u32)
    }

    /// Create an iterator over all key value pairs in the table.
    // TODO: remove this?
    pub fn iter(&self) -> impl Iterator<Item = (RouteKey, &'_ RouteEntry)> {
        self.table.iter().map(|(addr, prefix, value)| {
            (
                RouteKey::new(
                    Subnet::new(addr.into(), prefix as u8)
                        .expect("Only proper subnets are inserted in the table; qed"),
                    value.neighbor.clone(),
                ),
                value,
            )
        })
    }

    /// Checks if there is an entry for the given [`RouteKey`].
    //pub fn contains_key(&self, key: &RouteKey) -> bool {
    //    self.table.exact_match()
    //    self.table.contains_key(key);
    //}

    /// Remove all [`RouteKey`] and [`RouteEntry`] pairs where the [`RouteEntry`]'s neighbour value
    /// is the given [`Peer`].
    // TODO: performance
    pub fn remove_peer(&mut self, peer: &Peer) {
        let to_remove = self
            .table
            .iter()
            .filter_map(|(addr, plen, re)| {
                if &re.neighbor == peer {
                    Some((addr, plen))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        for (addr, plen) in to_remove {
            self.table.remove(addr, plen);
        }
    }

    /// Look up a route for an [`IpAddr`] in the `RoutingTable`.
    ///
    /// Currently only IPv6 is supported, looking up an IPv4 address always returns [`Option::None`].
    pub fn lookup(&self, ip: IpAddr) -> Option<RouteEntry> {
        let addr = match ip {
            IpAddr::V6(addr) => addr,
            _ => return None,
        };
        self.table.longest_match(addr).map(|(_, _, re)| re.clone())
    }
}

impl Clone for RoutingTable {
    fn clone(&self) -> Self {
        let mut new = RoutingTable::new();
        for (rk, rv) in self.iter() {
            new.insert(rk, rv.clone());
        }
        new
    }
}
