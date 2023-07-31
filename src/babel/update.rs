//! The babel [Update TLV](https://datatracker.ietf.org/doc/html/rfc8966#name-update).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use bytes::{Buf, BufMut};
use log::trace;
use x25519_dalek::PublicKey;

use crate::{metric::Metric, sequence_number::SeqNo};

use super::{AE_IPV4, AE_IPV6, AE_IPV6_LL, AE_WILDCARD};

/// Flag bit indicating an [`Update`] TLV establishes a new default prefix.
const UPDATE_FLAG_PREFIX: u8 = 0x80;
/// Flag bit indicating an [`Update`] TLV establishes a new default router-id.
const UPDATE_FLAG_ROUTER_ID: u8 = 0x40;
/// Mask to apply to [`Update`] flags, leaving only valid flags.
const FLAG_MASK: u8 = 0b1100_0000;

/// Base wire size of an [`Update`] without variable lenght address encoding.
const UPDATE_BASE_WIRE_SIZE: u8 = 10 + 32;

/// Update TLV body as defined in https://datatracker.ietf.org/doc/html/rfc8966#name-update.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    /// Flags set in the TLV.
    flags: u8,
    /// Prefix length in bits of the advertised prefix.
    plen: u8,
    /// The number of octets that have been omitted and that should be taken from a preceding
    /// update TLV in the same body.
    // TODO: remove this field
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

    /// Calculates the size on the wire of this `Update`.
    pub fn wire_size(&self) -> u8 {
        UPDATE_BASE_WIRE_SIZE
            + match self.prefix {
                // TODO: link local and wildcard should be encoded differently
                IpAddr::V4(_) => 4,
                IpAddr::V6(_) => 16,
            }
    }

    /// Construct an `Update` from wire bytes.
    ///
    /// # Panics
    ///
    /// This function will panic if there are insufficient bytes present in the provided buffer to
    /// decode a complete `Update`.
    pub fn from_bytes(src: &mut bytes::BytesMut, len: u8) -> Option<Self> {
        let ae = src.get_u8();
        let flags = src.get_u8() & FLAG_MASK;
        let plen = src.get_u8();
        let omitted = src.get_u8();
        let interval = src.get_u16();
        let seqno = src.get_u16().into();
        let metric = src.get_u16().into();
        // based on the remaining bytes (ip + router_id) we can check if it's IPv4 or v6
        let prefix = match ae {
            AE_WILDCARD => {
                // TODO: this is a temporary placeholder until we figure out how to handle this
                Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0).into()
            }
            AE_IPV4 => {
                let mut raw_ip = [0; 4];
                raw_ip.copy_from_slice(&src[..4]);
                Ipv4Addr::from(raw_ip).into()
            }
            AE_IPV6 => {
                let mut raw_ip = [0; 16];
                raw_ip.copy_from_slice(&src[..16]);
                Ipv6Addr::from(raw_ip).into()
            }
            AE_IPV6_LL => {
                let mut raw_ip = [0; 16];
                raw_ip[0] = 0xfe;
                raw_ip[1] = 0x80;
                raw_ip[8..].copy_from_slice(&src[..8]);
                Ipv6Addr::from(raw_ip).into()
            }
            _ => {
                // Invalid AE type, skip reamining data and ignore
                trace!("Invalid AE type in update packet, drop packet");
                src.advance(len as usize - 10);
                return None;
            }
        };

        let mut router_id_bytes = [0u8; 32];
        router_id_bytes.copy_from_slice(&src[..32]);
        src.advance(32);

        let router_id = PublicKey::from(router_id_bytes);

        Some(Update {
            flags,
            plen,
            omitted,
            interval,
            seqno,
            metric,
            prefix,
            router_id,
        })
    }

    /// Encode this `Update` tlv as part of a packet.
    pub fn write_bytes(&self, dst: &mut bytes::BytesMut) {
        dst.put_u8(match self.prefix {
            IpAddr::V4(_) => AE_IPV4,
            IpAddr::V6(_) => AE_IPV6,
        });
        dst.put_u8(self.flags);
        dst.put_u8(self.plen);
        dst.put_u8(self.omitted);
        dst.put_u16(self.interval);
        dst.put_u16(self.seqno.into());
        dst.put_u16(self.metric.into());
        match self.prefix {
            IpAddr::V4(ip) => dst.put_slice(&ip.octets()),
            IpAddr::V6(ip) => dst.put_slice(&ip.octets()),
        }
        dst.put_slice(&self.router_id.as_bytes()[..])
    }
}
