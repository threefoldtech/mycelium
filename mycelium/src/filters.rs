use crate::{babel, subnet::Subnet};

/// This trait is used to filter incoming updates from peers. Only updates which pass all
/// configured filters on the local [`Router`](crate::router::Router) will actually be forwarded
/// to the [`Router`](crate::router::Router) for processing.
pub trait RouteUpdateFilter {
    /// Judge an incoming update.
    fn allow(&self, update: &babel::Update) -> bool;
}

/// Limit the subnet size of subnets announced in updates to be at most `N` bits. Note that "at
/// most" here means that the actual prefix length needs to be **AT LEAST** this value.
pub struct MaxSubnetSize<const N: u8>;

impl<const N: u8> RouteUpdateFilter for MaxSubnetSize<N> {
    fn allow(&self, update: &babel::Update) -> bool {
        update.subnet().prefix_len() >= N
    }
}

/// Limit the subnet announced to be included in the given subnet.
pub struct AllowedSubnet {
    subnet: Subnet,
}

impl AllowedSubnet {
    /// Create a new `AllowedSubnet` filter, which only allows updates who's `Subnet` is contained
    /// in the given `Subnet`.
    pub fn new(subnet: Subnet) -> Self {
        Self { subnet }
    }
}

impl RouteUpdateFilter for AllowedSubnet {
    fn allow(&self, update: &babel::Update) -> bool {
        self.subnet.contains_subnet(&update.subnet())
    }
}

/// Limit the announced subnets to those which contain the derived IP from the `RouterId`.
///
/// Since retractions can be sent by any node to indicate they don't have a route for the subnet,
/// these are also allowed.
pub struct RouterIdOwnsSubnet;

impl RouteUpdateFilter for RouterIdOwnsSubnet {
    fn allow(&self, update: &babel::Update) -> bool {
        update.metric().is_infinite()
            || update
                .subnet()
                .contains_ip(update.router_id().to_pubkey().address().into())
    }
}
