use tokio::time::Instant;

use crate::subnet::Subnet;

/// Information about a [`subnet`](Subnet) which is currently in the queried state
#[derive(Debug, Clone, Copy)]
pub struct QueriedSubnet {
    /// The subnet which was queried.
    subnet: Subnet,
    /// Time at which the query expires. If no feasible updates come in before this, the subnet is
    /// marked as no route temporarily.
    query_expires: Instant,
}

impl QueriedSubnet {
    /// Create a new `QueriedSubnet` for the given [`subnet`](Subnet), expiring at the provided
    /// [`time`](Instant).
    pub fn new(subnet: Subnet, query_expires: Instant) -> Self {
        Self {
            subnet,
            query_expires,
        }
    }

    /// The [`subnet`](Subnet) being queried.
    pub fn subnet(&self) -> Subnet {
        self.subnet
    }

    /// The moment this query expires. If no route is discovered before this, the [`subnet`](Subnet)
    /// is marked as no route temporarily.
    pub fn query_expires(&self) -> Instant {
        self.query_expires
    }
}
