use tokio::time::Instant;

use crate::subnet::Subnet;

/// Information about a [`subnet`](Subnet) which is currently marked as NoRoute.
#[derive(Debug, Clone, Copy)]
pub struct NoRouteSubnet {
    /// The subnet which has no route.
    subnet: Subnet,
    /// Time at which the entry expires. After this timeout expires, the entry is removed and a new
    /// query can be performed.
    entry_expires: Instant,
}

impl NoRouteSubnet {
    /// Create a new `NoRouteSubnet` for the given [`subnet`](Subnet), expiring at the provided
    /// [`time`](Instant).
    pub fn new(subnet: Subnet, entry_expires: Instant) -> Self {
        Self {
            subnet,
            entry_expires,
        }
    }

    /// The [`subnet`](Subnet) for which there is no route.
    pub fn subnet(&self) -> Subnet {
        self.subnet
    }

    /// The moment this entry expires. Once this timeout expires, a new query can be launched for
    /// route discovery for this [`subnet`](Subnet).
    pub fn entry_expires(&self) -> Instant {
        self.entry_expires
    }
}
