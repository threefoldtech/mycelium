use crate::crypto::PublicKey;

/// A `RouterId` uniquely identifies a router in the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RouterId(PublicKey);

impl RouterId {
    /// Create a new `RouterId` from a [`PublicKey`].
    pub fn new(pk: PublicKey) -> Self {
        Self(pk)
    }

    /// View this `RouterId` as a byte array.
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Converts this `RouterId` to a [`PublicKey`].
    pub fn to_pubkey(self) -> PublicKey {
        self.0
    }
}

impl From<[u8; 32]> for RouterId {
    fn from(bytes: [u8; 32]) -> RouterId {
        RouterId(PublicKey::from(bytes))
    }
}
