use arc_swap::ArcSwap;

use super::RouteList;

/// An entry for a [Subnet](crate::subnet::Subnet) in the routing table.
#[allow(dead_code)]
pub enum SubnetEntry {
    /// Routes for the given subnet exist
    Exists { list: ArcSwap<RouteList> },
    /// Routes are being queried from peers for the given subnet, but we haven't gotten a response
    /// yet
    Queried { query_start: tokio::time::Instant },
    /// We queried our peers for the subnet, but we didn't get a valid response in time, so there
    /// is for sure no route to the subnet.
    NoRoute { expiry: tokio::time::Instant },
}
