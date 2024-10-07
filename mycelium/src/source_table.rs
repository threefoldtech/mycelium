use core::fmt;
use std::{collections::HashMap, time::Duration};

use tokio::{sync::mpsc, task::JoinHandle};
use tracing::error;

use crate::{
    babel, metric::Metric, router_id::RouterId, routing_table::RouteEntry, sequence_number::SeqNo,
    subnet::Subnet,
};

/// Duration after which a source entry is deleted if it is not updated.
const SOURCE_HOLD_DURATION: Duration = Duration::from_secs(60 * 30);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub struct SourceKey {
    subnet: Subnet,
    router_id: RouterId,
}

#[derive(Debug, Clone, Copy)]
pub struct FeasibilityDistance {
    metric: Metric,
    seqno: SeqNo,
}

#[derive(Debug)]
pub struct SourceTable {
    table: HashMap<SourceKey, (JoinHandle<()>, FeasibilityDistance)>,
}

impl FeasibilityDistance {
    pub fn new(metric: Metric, seqno: SeqNo) -> Self {
        FeasibilityDistance { metric, seqno }
    }

    /// Returns the metric for this `FeasibilityDistance`.
    pub const fn metric(&self) -> Metric {
        self.metric
    }

    /// Returns the sequence number for this `FeasibilityDistance`.
    pub const fn seqno(&self) -> SeqNo {
        self.seqno
    }
}

impl SourceKey {
    /// Create a new `SourceKey`.
    pub const fn new(subnet: Subnet, router_id: RouterId) -> Self {
        Self { subnet, router_id }
    }

    /// Returns the [`RouterId`] for this `SourceKey`.
    pub const fn router_id(&self) -> RouterId {
        self.router_id
    }

    /// Returns the [`Subnet`] for this `SourceKey`.
    pub const fn subnet(&self) -> Subnet {
        self.subnet
    }

    /// Updates the [`RouterId`] of this `SourceKey`
    pub fn set_router_id(&mut self, router_id: RouterId) {
        self.router_id = router_id
    }
}

impl SourceTable {
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
        }
    }

    pub fn insert(
        &mut self,
        key: SourceKey,
        feas_dist: FeasibilityDistance,
        sink: mpsc::Sender<SourceKey>,
    ) {
        let expiration_handle = tokio::spawn(async move {
            tokio::time::sleep(SOURCE_HOLD_DURATION).await;

            if let Err(e) = sink.send(key).await {
                error!("Failed to notify router of expired source key {e}");
            }
        });
        // Abort the old task if present.
        if let Some((old_timeout, _)) = self.table.insert(key, (expiration_handle, feas_dist)) {
            old_timeout.abort();
        }
    }

    /// Remove an entry from the source table.
    pub fn remove(&mut self, key: &SourceKey) {
        if let Some((old_timeout, _)) = self.table.remove(key) {
            old_timeout.abort();
        };
    }

    /// Resets the garbage collection timer for a given source key.
    ///
    /// Does nothing if the source key is not present.
    pub fn reset_timer(&mut self, key: SourceKey, sink: mpsc::Sender<SourceKey>) {
        self.table
            .entry(key)
            .and_modify(|(old_expiration_handle, _)| {
                // First cancel the existing task
                old_expiration_handle.abort();
                // Then set the new one
                *old_expiration_handle = tokio::spawn(async move {
                    tokio::time::sleep(SOURCE_HOLD_DURATION).await;

                    if let Err(e) = sink.send(key).await {
                        error!("Failed to notify router of expired source key {e}");
                    }
                });
            });
    }

    /// Get the [`FeasibilityDistance`] currently associated with the [`SourceKey`].
    pub fn get(&self, key: &SourceKey) -> Option<&FeasibilityDistance> {
        self.table.get(key).map(|(_, v)| v)
    }

    /// Indicates if an update is feasible in the context of the current `SoureTable`.
    pub fn is_update_feasible(&self, update: &babel::Update) -> bool {
        // Before an update is accepted it should be checked against the feasbility condition
        // If an entry in the source table with the same source key exists, we perform the feasbility check
        // If no entry exists yet, the update is accepted as there is no better alternative available (yet)
        let source_key = SourceKey::new(update.subnet(), update.router_id());
        match self.get(&source_key) {
            Some(entry) => {
                (update.seqno().gt(&entry.seqno()))
                    || (update.seqno() == entry.seqno() && update.metric() < entry.metric())
                    || update.metric().is_infinite()
            }
            None => true,
        }
    }

    /// Indicates if a [`RouteEntry`] is feasible according to the `SourceTable`.
    pub fn route_feasible(&self, route: &RouteEntry) -> bool {
        match self.get(&route.source()) {
            Some(fd) => {
                (route.seqno().gt(&fd.seqno))
                    || (route.seqno() == fd.seqno && route.metric() < fd.metric)
                    || route.metric().is_infinite()
            }
            None => true,
        }
    }
}

impl fmt::Display for SourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "{} advertised by {}",
            self.subnet, self.router_id
        ))
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use crate::{
        babel,
        connection::DuplexStream,
        crypto::SecretKey,
        metric::Metric,
        peer::Peer,
        router_id::RouterId,
        routing_table::RouteEntry,
        sequence_number::SeqNo,
        source_table::{FeasibilityDistance, SourceKey, SourceTable},
        subnet::Subnet,
    };
    use std::{net::Ipv6Addr, time::Duration};

    /// A retraction is always considered to be feasible.
    #[tokio::test]
    async fn retraction_update_is_feasible() {
        let (sink, _) = tokio::sync::mpsc::channel(1);
        let sk = SecretKey::new();
        let pk = (&sk).into();
        let sn = Subnet::new(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 1).into(), 64)
            .expect("Valid subnet in test case");
        let rid = RouterId::new(pk);

        let mut st = SourceTable::new();
        st.insert(
            SourceKey::new(sn, rid),
            FeasibilityDistance::new(Metric::new(10), SeqNo::from(1)),
            sink,
        );

        let update = babel::Update::new(
            Duration::from_secs(60),
            SeqNo::from(0),
            Metric::infinite(),
            sn,
            rid,
        );

        assert!(st.is_update_feasible(&update));
    }

    /// An update with a smaller metric but with the same seqno is feasible.
    #[tokio::test]
    async fn smaller_metric_update_is_feasible() {
        let (sink, _) = tokio::sync::mpsc::channel(1);
        let sk = SecretKey::new();
        let pk = (&sk).into();
        let sn = Subnet::new(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 1).into(), 64)
            .expect("Valid subnet in test case");
        let rid = RouterId::new(pk);

        let mut st = SourceTable::new();
        st.insert(
            SourceKey::new(sn, rid),
            FeasibilityDistance::new(Metric::new(10), SeqNo::from(1)),
            sink,
        );

        let update = babel::Update::new(
            Duration::from_secs(60),
            SeqNo::from(1),
            Metric::from(9),
            sn,
            rid,
        );

        assert!(st.is_update_feasible(&update));
    }

    /// An update with the same metric and seqno is not feasible.
    #[tokio::test]
    async fn equal_metric_update_is_unfeasible() {
        let (sink, _) = tokio::sync::mpsc::channel(1);
        let sk = SecretKey::new();
        let pk = (&sk).into();
        let sn = Subnet::new(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 1).into(), 64)
            .expect("Valid subnet in test case");
        let rid = RouterId::new(pk);

        let mut st = SourceTable::new();
        st.insert(
            SourceKey::new(sn, rid),
            FeasibilityDistance::new(Metric::new(10), SeqNo::from(1)),
            sink,
        );

        let update = babel::Update::new(
            Duration::from_secs(60),
            SeqNo::from(1),
            Metric::from(10),
            sn,
            rid,
        );

        assert!(!st.is_update_feasible(&update));
    }

    /// An update with a larger metric and the same seqno is not feasible.
    #[tokio::test]
    async fn larger_metric_update_is_unfeasible() {
        let (sink, _) = tokio::sync::mpsc::channel(1);
        let sk = SecretKey::new();
        let pk = (&sk).into();
        let sn = Subnet::new(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 1).into(), 64)
            .expect("Valid subnet in test case");
        let rid = RouterId::new(pk);

        let mut st = SourceTable::new();
        st.insert(
            SourceKey::new(sn, rid),
            FeasibilityDistance::new(Metric::new(10), SeqNo::from(1)),
            sink,
        );

        let update = babel::Update::new(
            Duration::from_secs(60),
            SeqNo::from(1),
            Metric::from(11),
            sn,
            rid,
        );

        assert!(!st.is_update_feasible(&update));
    }

    /// An update with a lower seqno is not feasible.
    #[tokio::test]
    async fn lower_seqno_update_is_unfeasible() {
        let (sink, _) = tokio::sync::mpsc::channel(1);
        let sk = SecretKey::new();
        let pk = (&sk).into();
        let sn = Subnet::new(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 1).into(), 64)
            .expect("Valid subnet in test case");
        let rid = RouterId::new(pk);

        let mut st = SourceTable::new();
        st.insert(
            SourceKey::new(sn, rid),
            FeasibilityDistance::new(Metric::new(10), SeqNo::from(1)),
            sink,
        );

        let update = babel::Update::new(
            Duration::from_secs(60),
            SeqNo::from(0),
            Metric::from(1),
            sn,
            rid,
        );

        assert!(!st.is_update_feasible(&update));
    }

    /// An update with a higher seqno is feasible.
    #[tokio::test]
    async fn higher_seqno_update_is_feasible() {
        let (sink, _) = tokio::sync::mpsc::channel(1);
        let sk = SecretKey::new();
        let pk = (&sk).into();
        let sn = Subnet::new(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 1).into(), 64)
            .expect("Valid subnet in test case");
        let rid = RouterId::new(pk);

        let mut st = SourceTable::new();
        st.insert(
            SourceKey::new(sn, rid),
            FeasibilityDistance::new(Metric::new(10), SeqNo::from(1)),
            sink,
        );

        let update = babel::Update::new(
            Duration::from_secs(60),
            SeqNo::from(2),
            Metric::from(200),
            sn,
            rid,
        );

        assert!(st.is_update_feasible(&update));
    }

    /// A route with a smaller metric but with the same seqno is feasible.
    #[tokio::test]
    async fn smaller_metric_route_is_feasible() {
        let (sink, _) = tokio::sync::mpsc::channel(1);
        let sk = SecretKey::new();
        let pk = (&sk).into();
        let sn = Subnet::new(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 1).into(), 64)
            .expect("Valid subnet in test case");
        let rid = RouterId::new(pk);

        let source_key = SourceKey::new(sn, rid);

        let mut st = SourceTable::new();
        st.insert(
            source_key,
            FeasibilityDistance::new(Metric::new(10), SeqNo::from(1)),
            sink,
        );

        let (router_data_tx, _router_data_rx) = mpsc::channel(1);
        let (router_control_tx, _router_control_rx) = mpsc::unbounded_channel();
        let (dead_peer_sink, _dead_peer_stream) = mpsc::channel(1);
        let (con1, _con2) = tokio::io::duplex(1500);
        let neighbor = Peer::new(
            router_data_tx,
            router_control_tx,
            DuplexStream::new(con1),
            dead_peer_sink,
        )
        .expect("Can create a dummy peer");

        let re = RouteEntry::new(
            source_key,
            neighbor,
            Metric::new(9),
            SeqNo::from(1),
            true,
            tokio::time::Instant::now() + Duration::from_secs(60),
        );

        assert!(st.route_feasible(&re));
    }

    /// If a route has the same metric as the source table it is not feasible.
    #[tokio::test]
    async fn equal_metric_route_is_unfeasible() {
        let (sink, _) = tokio::sync::mpsc::channel(1);
        let sk = SecretKey::new();
        let pk = (&sk).into();
        let sn = Subnet::new(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 1).into(), 64)
            .expect("Valid subnet in test case");
        let rid = RouterId::new(pk);

        let source_key = SourceKey::new(sn, rid);

        let mut st = SourceTable::new();
        st.insert(
            source_key,
            FeasibilityDistance::new(Metric::new(10), SeqNo::from(1)),
            sink,
        );

        let (router_data_tx, _router_data_rx) = mpsc::channel(1);
        let (router_control_tx, _router_control_rx) = mpsc::unbounded_channel();
        let (dead_peer_sink, _dead_peer_stream) = mpsc::channel(1);
        let (con1, _con2) = tokio::io::duplex(1500);
        let neighbor = Peer::new(
            router_data_tx,
            router_control_tx,
            DuplexStream::new(con1),
            dead_peer_sink,
        )
        .expect("Can create a dummy peer");

        let re = RouteEntry::new(
            source_key,
            neighbor,
            Metric::new(10),
            SeqNo::from(1),
            true,
            tokio::time::Instant::now() + Duration::from_secs(60),
        );

        assert!(!st.route_feasible(&re));
    }

    /// If a route has a higher metric as the source table it is not feasible.
    #[tokio::test]
    async fn higher_metric_route_is_unfeasible() {
        let (sink, _) = tokio::sync::mpsc::channel(1);
        let sk = SecretKey::new();
        let pk = (&sk).into();
        let sn = Subnet::new(Ipv6Addr::new(0x400, 0, 0, 0, 0, 0, 0, 1).into(), 64)
            .expect("Valid subnet in test case");
        let rid = RouterId::new(pk);

        let source_key = SourceKey::new(sn, rid);

        let mut st = SourceTable::new();
        st.insert(
            source_key,
            FeasibilityDistance::new(Metric::new(10), SeqNo::from(1)),
            sink,
        );

        let (router_data_tx, _router_data_rx) = mpsc::channel(1);
        let (router_control_tx, _router_control_rx) = mpsc::unbounded_channel();
        let (dead_peer_sink, _dead_peer_stream) = mpsc::channel(1);
        let (con1, _con2) = tokio::io::duplex(1500);
        let neighbor = Peer::new(
            router_data_tx,
            router_control_tx,
            DuplexStream::new(con1),
            dead_peer_sink,
        )
        .expect("Can create a dummy peer");

        let re = RouteEntry::new(
            source_key,
            neighbor,
            Metric::new(11),
            SeqNo::from(1),
            true,
            tokio::time::Instant::now() + Duration::from_secs(60),
        );

        assert!(!st.route_feasible(&re));
    }
}
