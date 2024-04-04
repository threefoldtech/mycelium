use core::fmt;

use crate::crypto::PublicKey;

/// A `RouterId` uniquely identifies a router in the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RouterId {
    pk: PublicKey,
    zone: [u8; 2],
    rnd: [u8; 6],
}

impl RouterId {
    /// Size in bytes of a `RouterId`
    pub const BYTE_SIZE: usize = 40;

    /// Create a new `RouterId` from a [`PublicKey`].
    pub fn new(pk: PublicKey) -> Self {
        Self {
            pk,
            zone: [0; 2],
            rnd: rand::random(),
        }
    }

    /// View this `RouterId` as a byte array.
    pub fn as_bytes(&self) -> [u8; Self::BYTE_SIZE] {
        let mut out = [0; Self::BYTE_SIZE];
        out[..32].copy_from_slice(self.pk.as_bytes());
        out[32..34].copy_from_slice(&self.zone);
        out[34..].copy_from_slice(&self.rnd);
        out
    }

    /// Converts this `RouterId` to a [`PublicKey`].
    pub fn to_pubkey(self) -> PublicKey {
        self.pk
    }
}

impl From<[u8; Self::BYTE_SIZE]> for RouterId {
    fn from(bytes: [u8; Self::BYTE_SIZE]) -> RouterId {
        RouterId {
            pk: PublicKey::from(<&[u8] as TryInto<[u8; 32]>>::try_into(&bytes[..32]).unwrap()),
            zone: bytes[32..34].try_into().unwrap(),
            rnd: bytes[34..Self::BYTE_SIZE].try_into().unwrap(),
        }
    }
}

impl fmt::Display for RouterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let RouterId { pk, zone, rnd } = self;
        f.write_fmt(format_args!(
            "{pk}-{}-{}",
            faster_hex::hex_string(zone),
            faster_hex::hex_string(rnd)
        ))
    }
}
