//! The seqno request cache keeps track of seqno requests sent by the node. This allows us to drop
//! duplicate requests, and to notify the source of requests (if it wasn't the local node) about
//! relevant updates.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use dashmap::DashMap;
use tokio::time::MissedTickBehavior;
use tracing::{debug, trace};

use crate::{peer::Peer, router_id::RouterId, sequence_number::SeqNo, subnet::Subnet};

/// The amount of time to remember a seqno request (since it was first seen), before we remove it
/// (assuming it was not removed manually before that).
const SEQNO_DEDUP_TTL: Duration = Duration::from_secs(60);

/// A sequence number request, either forwarded or originated by the local node.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeqnoRequestCacheKey {
    pub router_id: RouterId,
    pub subnet: Subnet,
    pub seqno: SeqNo,
}

impl std::fmt::Display for SeqnoRequestCacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "seqno {} for {} from {}",
            self.seqno, self.subnet, self.router_id
        )
    }
}

/// Information retained for sequence number requests we've sent.
struct SeqnoForwardInfo {
    /// Which peers have asked us to forward this seqno request.
    sources: Vec<Peer>,
    /// Which peers have we sent this request to.
    targets: Vec<Peer>,
    /// Time at which we first forwarded the requets.
    first_sent: Instant,
    /// When did we last sent a seqno request.
    last_sent: Instant,
}

/// A cache for outbound seqno requests. Entries in the cache are automatically removed after a
/// certain amount of time. The cache does not account for the source table. That is, if the
/// requested seqno is smaller, it might pass the cache, but should have been blocked earlier by
/// the source table check. As such, this cache should be the last step in deciding if a seqno
/// request is forwarded.
#[derive(Clone)]
pub struct SeqnoCache {
    /// Actual cache wrapped in an Arc to make it sharaeble.
    cache: Arc<DashMap<SeqnoRequestCacheKey, SeqnoForwardInfo, ahash::RandomState>>,
}

impl SeqnoCache {
    /// Create a new [`SeqnoCache`].
    pub fn new() -> Self {
        trace!(capacity = 0, "Creating new seqno cache");

        let cache = Arc::new(DashMap::with_hasher_and_shard_amount(
            ahash::RandomState::new(),
            // This number has been chosen completely at random
            1024,
        ));
        let sc = Self { cache };

        // Spawn background cleanup task.
        tokio::spawn(sc.clone().sweep_entries());

        sc
    }

    /// Record a forwarded seqno request to a given target. Also keep track of the origin of the
    /// request. If the local node generated the request, source must be [`None`]
    pub fn forward(&self, request: SeqnoRequestCacheKey, target: Peer, source: Option<Peer>) {
        let mut info = self.cache.entry(request).or_default();
        info.last_sent = Instant::now();
        if !info.targets.contains(&target) {
            info.targets.push(target);
        } else {
            debug!(
                seqno_request = %request,
                "Already sent seqno request to target {}",
                target.connection_identifier()
            );
        }
        if let Some(source) = source {
            if !info.sources.contains(&source) {
                info.sources.push(source);
            } else {
                debug!(seqno_request = %request, "Peer {} is requesting the same seqno again", source.connection_identifier());
            }
        }
    }

    /// Get a list of all peers which we've already sent the given seqno request to, as well as
    /// when we've last sent a request.
    pub fn info(&self, request: &SeqnoRequestCacheKey) -> Option<(Instant, Vec<Peer>)> {
        self.cache
            .get(request)
            .map(|info| (info.last_sent, info.targets.clone()))
    }

    /// Removes forwarding info from the seqno cache. If forwarding info is available, the source
    /// peers (peers which requested us to forward this request) are returned.
    // TODO: cleanup if needed
    #[allow(dead_code)]
    pub fn remove(&self, request: &SeqnoRequestCacheKey) -> Option<Vec<Peer>> {
        self.cache.remove(request).map(|(_, info)| info.sources)
    }

    /// Get forwarding info from the seqno cache. If forwarding info is available, the source
    /// peers (peers which requested us to forward this request) are returned.
    // TODO: cleanup if needed
    #[allow(dead_code)]
    pub fn get(&self, request: &SeqnoRequestCacheKey) -> Option<Vec<Peer>> {
        self.cache.get(request).map(|info| info.sources.clone())
    }

    /// Periodic task to clear old entries for which no reply came in.
    async fn sweep_entries(self) {
        let mut interval = tokio::time::interval(SEQNO_DEDUP_TTL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            debug!("Cleaning up expired seqno requests from seqno cache");

            let prev_entries = self.cache.len();
            let prev_cap = self.cache.capacity();
            self.cache
                .retain(|_, info| info.first_sent.elapsed() <= SEQNO_DEDUP_TTL);
            self.cache.shrink_to_fit();

            debug!(
                cleaned_entries = prev_entries - self.cache.len(),
                removed_capacity = prev_cap - self.cache.capacity(),
                "Cleaned up stale seqno request cache entries"
            );
        }
    }
}

impl Default for SeqnoCache {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for SeqnoForwardInfo {
    fn default() -> Self {
        Self {
            sources: vec![],
            targets: vec![],
            first_sent: Instant::now(),
            last_sent: Instant::now(),
        }
    }
}

impl std::fmt::Debug for SeqnoRequestCacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeqnoRequestCacheKey")
            .field("router_id", &self.router_id.to_string())
            .field("subnet", &self.subnet.to_string())
            .field("seqno", &self.seqno.to_string())
            .finish()
    }
}
