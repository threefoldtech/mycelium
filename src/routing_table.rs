use ip_network_table_deps_treebitmap::IpLookupTable;

use crate::{
    metric::Metric, peer::Peer, router_id::RouterId, sequence_number::SeqNo,
    source_table::SourceKey, subnet::Subnet,
};
use core::fmt;
use std::{
    borrow::Borrow,
    cmp::Ordering,
    collections::HashSet,
    hash::Hash,
    mem,
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
        Some(self.cmp(other))
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
    metric: Metric, // If metric is 0xFFFF, the route has recently been retracted
    seqno: SeqNo,
    selected: bool,
}

/// The routing table uses only the IP as key, but we also want to include the (neighbour)[`Peer`]
/// in the key (see [`RouteKey`]). We use a [`HashSet`] for this, which means [`RouteEntry`] would
/// need to implement both [`Eq`] and [`Hash`]. While doable, using the [`HashSet::get`] method
/// with a [`Peer`] requires [`RouteEntry`] (the value in the [`HashSet`]) to implement
/// [`Borrow`]. And, problematically, the [`Borrow`] target must have the
/// same result for [`Hash`] and [`Eq`]. This is again doable, but would pollute the base type
/// which is public. As such, we create a private newtype here, to implement these methods, so we
/// don't leak the public value.
#[repr(transparent)]
struct RouteEntryPeerIndexed(RouteEntry);

impl PartialEq for RouteEntryPeerIndexed {
    fn eq(&self, other: &Self) -> bool {
        self.0
            .neighbor
            .overlay_ip()
            .eq(&other.0.neighbor.overlay_ip())
    }
}

impl Eq for RouteEntryPeerIndexed {}

impl Hash for RouteEntryPeerIndexed {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.neighbor.overlay_ip().hash(state)
    }
}

impl Borrow<PeerIpEq> for RouteEntryPeerIndexed {
    fn borrow(&self) -> &PeerIpEq {
        // SAFETY: we are transmuting to a struct annotated with repr(transparent).
        unsafe { mem::transmute(&self.0.neighbor) }
    }
}

struct PeerIpEq(Peer);

impl PartialEq for PeerIpEq {
    fn eq(&self, other: &Self) -> bool {
        self.0.overlay_ip().eq(&other.0.overlay_ip())
    }
}

impl Eq for PeerIpEq {}

impl Hash for PeerIpEq {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.overlay_ip().hash(state)
    }
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

/// The `RoutingTable` contains all known subnets, and the associated [`RouteEntry`]'s.
pub struct RoutingTable {
    table: IpLookupTable<Ipv6Addr, HashSet<RouteEntryPeerIndexed>>,
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

        let set = self
            .table
            .exact_match(addr, key.subnet.prefix_len() as u32)?;
        set.get(&PeerIpEq(key.neighbor.clone())).map(|p| &p.0)
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
        match self
            .table
            .exact_match_mut(addr, key.subnet.prefix_len() as u32)
        {
            Some(set) => {
                set.insert(RouteEntryPeerIndexed(entry));
            }
            None => {
                let mut set = HashSet::new();
                set.insert(RouteEntryPeerIndexed(entry));
                self.table.insert(addr, key.subnet.prefix_len() as u32, set);
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
            Some(set) => {
                let val = set.take(&PeerIpEq(key.neighbor.clone()));
                // Remove the table entry entirely if the set is empty
                // NOTE: we don't care if val is some, we only care that no empty set is left
                // behind.
                if set.is_empty() {
                    self.table.remove(addr, key.subnet.prefix_len() as u32);
                }
                val.map(|e| e.0)
            }
            None => None,
        }
    }

    /// Create an iterator over all key value pairs in the table.
    // TODO: remove this?
    pub fn iter(&self) -> impl Iterator<Item = (RouteKey, &'_ RouteEntry)> {
        self.table.iter().flat_map(|(addr, prefix, set)| {
            set.iter().map(move |value| {
                (
                    RouteKey::new(
                        Subnet::new(addr.into(), prefix as u8)
                            .expect("Only proper subnets are inserted in the table; qed"),
                        value.0.neighbor.clone(),
                    ),
                    &value.0,
                )
            })
        })
    }

    /// Remove all [`RouteKey`] and [`RouteEntry`] pairs where the [`RouteEntry`]'s neighbour value
    /// is the given [`Peer`].
    // TODO: performance
    pub fn remove_peer(&mut self, peer: Peer) {
        let q = PeerIpEq(peer);
        for (_, _, set) in self.table.iter_mut() {
            set.remove(&q);
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
        self.table
            .longest_match(addr)
            .and_then(|(_, _, set)| {
                let mut lowest_metric = None;
                for entry in set {
                    match lowest_metric {
                        None => {
                            lowest_metric = Some(&entry.0);
                        }
                        Some(e) => {
                            if entry.0.metric < e.metric {
                                lowest_metric = Some(&entry.0);
                            }
                        }
                    }
                }
                // TODO: Verify how we can get an empty map here.
                lowest_metric
            })
            .cloned()
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
