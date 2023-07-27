//! The babel [Update TLV](https://datatracker.ietf.org/doc/html/rfc8966#name-update).

use std::net::IpAddr;

use x25519_dalek::PublicKey;

use crate::{metric::Metric, sequence_number::SeqNo};

/// Flag bit indicating an [`Update`] TLV establishes a new default prefix.
const UPDATE_FLAG_PREFIX: u8 = 0x80;
/// Flag bit indicating an [`Update`] TLV establishes a new default router-id.
const UPDATE_FLAG_ROUTER_ID: u8 = 0x40;

/// Update TLV body as defined in https://datatracker.ietf.org/doc/html/rfc8966#name-update.
pub struct Update {
    /// Flags set in the TLV.
    flags: u8,
    /// Prefix length in bits of the advertised prefix.
    plen: u8,
    /// The number of octets that have been omitted and that should be taken from a preceding
    /// update TLV in the same body.
    omitted: u8,
    /// Upper bound in centiseconds after which a new `Update` is sent. Must not be 0.
    interval: u16,
    /// Senders sequence number.
    seqno: SeqNo,
    /// Senders metric for this route.
    metric: Metric,
    /// Prefix being advertised. Size of the field is plen/8 - omitted
    prefix: IpAddr,
    /// Router id of the sender. Importantly this is not part of the update itself, though we do
    /// transmit it for now as such.
    router_id: PublicKey,
}

impl Update {
    /// Create a new `Update`.
    pub fn new(
        plen: u8,
        omitted: u8,
        interval: u16,
        seqno: SeqNo,
        metric: Metric,
        prefix: IpAddr,
        router_id: PublicKey,
    ) -> Self {
        Self {
            // No flags used for now
            flags: 0,
            plen,
            omitted,
            interval,
            seqno,
            metric,
            prefix,
            router_id,
        }
    }

    /// Returns the [`SeqNo`] of the sender of this `Update`.
    pub fn seqno(&self) -> SeqNo {
        self.seqno
    }

    /// Return the [`Metric`] of the sender for this route in the `Update`.
    pub fn metric(&self) -> Metric {
        self.metric
    }
}
