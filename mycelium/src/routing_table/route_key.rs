use crate::{peer::Peer, subnet::Subnet};

/// RouteKey uniquely defines a route via a peer.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteKey {
    subnet: Subnet,
    neighbour: Peer,
}

impl RouteKey {
    /// Creates a new `RouteKey` for the given [`Subnet`] and [`neighbour`](Peer).
    #[inline]
    pub fn new(subnet: Subnet, neighbour: Peer) -> Self {
        Self { subnet, neighbour }
    }

    /// Get's the [`Subnet`] identified by this `RouteKey`.
    #[inline]
    pub fn subnet(&self) -> Subnet {
        self.subnet
    }

    /// Gets the [`neighbour`](Peer) identified by this `RouteKey`.
    #[inline]
    pub fn neighbour(&self) -> &Peer {
        &self.neighbour
    }
}

impl std::fmt::Display for RouteKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{} via {}",
            self.subnet,
            self.neighbour.connection_identifier()
        ))
    }
}
