use tokio::time::Instant;

use crate::{
    metric::Metric, peer::Peer, router_id::RouterId, sequence_number::SeqNo,
    source_table::SourceKey,
};

/// RouteEntry holds all relevant information about a specific route. Since this includes the next
/// hop, a single subnet can have multiple route entries.
#[derive(Clone)]
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

    /// Set the [`RouterId`] for this `RouteEntry`.
    pub fn set_router_id(&mut self, router_id: RouterId) {
        self.source.set_router_id(router_id)
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

    /// Sets if this `RouteEntry` is the selected route for the associated
    /// [`Subnet`](crate::subnet::Subnet).
    pub fn set_selected(&mut self, selected: bool) {
        self.selected = selected;
    }

    /// Sets the expiration time for this [`RouteEntry`].
    pub(super) fn set_expires(&mut self, expires: Instant) {
        self.expires = expires;
    }
}

// Manual Debug implementation since SharedSecret is explicitly not Debug
impl std::fmt::Debug for RouteEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteEntry")
            .field("source", &self.source)
            .field("neighbour", &self.neighbour)
            .field("metric", &self.metric)
            .field("seqno", &self.seqno)
            .field("selected", &self.selected)
            .field("expires", &self.expires)
            .finish()
    }
}
