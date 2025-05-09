//! This module contains a cache implementation for route requests

use std::{
    net::{IpAddr, Ipv6Addr},
    sync::Arc,
};

use dashmap::DashMap;
use tokio::time::{Duration, Instant};
use tracing::trace;

use crate::{babel::RouteRequest, peer::Peer, subnet::Subnet, task::AbortHandle};

/// Clean the route request cache every 5 seconds
const CACHE_CLEANING_INTERVAL: Duration = Duration::from_secs(5);

/// IP used for the [`Subnet`] in the cache in case there is no prefix specified.
const GLOBAL_SUBNET_IP: IpAddr = IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0));

/// Prefix size to use for the [`Subnet`] in case there is no prefix specified.
const GLOBAL_SUBNET_PREFIX_SIZE: u8 = 0;

/// A self cleaning cache for route requests.
pub struct RouteRequestCache {
    /// The actual cache, mapping an instance of a route request to the peers which we've sent this
    /// to.
    cache: Arc<DashMap<Subnet, RouteRequestInfo, ahash::RandomState>>,

    _cleanup_task: AbortHandle,
}

struct RouteRequestInfo {
    /// The lowest generation we've forwarded.
    generation: u8,
    /// Peers which we've sent this route request to already.
    receivers: Vec<Peer>,
    /// The moment we've sent this route request
    sent: Instant,
}

impl RouteRequestCache {
    /// Create a new cache which cleans entries which are older than the given expiration.
    ///
    /// The cache cleaning is done periodically, so entries might live slightly longer than the
    /// allowed expiration.
    pub fn new(expiration: Duration) -> Self {
        let cache = Arc::new(DashMap::with_hasher(ahash::RandomState::new()));

        let _cleanup_task = tokio::spawn({
            let cache = cache.clone();
            async move {
                loop {
                    tokio::time::sleep(CACHE_CLEANING_INTERVAL).await;

                    trace!("Cleaning route request cache");

                    cache.retain(|subnet, info: &mut RouteRequestInfo| {
                        if info.sent.elapsed() < expiration {
                            false
                        } else {
                            trace!(%subnet, "Removing exired route request from cache");
                            true
                        }
                    });
                }
            }
        })
        .abort_handle()
        .into();

        Self {
            cache,
            _cleanup_task,
        }
    }

    /// Record a route request which has been sent to peers.
    pub fn sent_route_request(&self, rr: RouteRequest, receivers: Vec<Peer>) {
        let subnet = rr.prefix().unwrap_or(
            Subnet::new(GLOBAL_SUBNET_IP, GLOBAL_SUBNET_PREFIX_SIZE)
                .expect("Static global IPv6 subnet is valid; qed"),
        );
        let generation = rr.generation();

        let rri = RouteRequestInfo {
            generation,
            receivers,
            sent: Instant::now(),
        };

        self.cache.insert(subnet, rri);
    }

    /// Get cached info about a route request for a subnet, if it exists.
    pub fn info(&self, subnet: Subnet) -> Option<(u8, Vec<Peer>)> {
        self.cache
            .get(&subnet)
            .map(|rri| (rri.generation, rri.receivers.clone()))
    }
}
