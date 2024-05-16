use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    num::NonZeroU8,
};

use bytes::{Buf, BufMut};
use tracing::{debug, trace};

use crate::{router_id::RouterId, sequence_number::SeqNo, subnet::Subnet};

use super::{AE_IPV4, AE_IPV6, AE_IPV6_LL, AE_WILDCARD};

/// The default HOP COUNT value used in new SeqNo requests, as per https://datatracker.ietf.org/doc/html/rfc8966#section-3.8.2.1
// SAFETY: value is not zero.
const DEFAULT_HOP_COUNT: NonZeroU8 = unsafe { NonZeroU8::new_unchecked(64) };

/// Base wire size of a [`SeqNoRequest`] without variable length address encoding.
const SEQNO_REQUEST_BASE_WIRE_SIZE: u8 = 6 + RouterId::BYTE_SIZE as u8;

/// Seqno request TLV body as defined in https://datatracker.ietf.org/doc/html/rfc8966#name-seqno-request
#[derive(Debug, Clone, PartialEq)]
pub struct SeqNoRequest {
    /// The sequence number that is being requested.
    seqno: SeqNo,
    /// The maximum number of times this TLV may be forwarded, plus 1.
    hop_count: NonZeroU8,
    /// The router id that is being requested.
    router_id: RouterId,
    /// The prefix being requested
    prefix: Subnet,
}

impl SeqNoRequest {
    /// Create a new `SeqNoRequest` for the given [prefix](Subnet) advertised by the [`RouterId`],
    /// with the required new [`SeqNo`].
    pub fn new(seqno: SeqNo, router_id: RouterId, prefix: Subnet) -> SeqNoRequest {
        Self {
            seqno,
            hop_count: DEFAULT_HOP_COUNT,
            router_id,
            prefix,
        }
    }

    /// Return the [`prefix`](Subnet) associated with this `SeqNoRequest`.
    pub fn prefix(&self) -> Subnet {
        self.prefix
    }

    /// Return the [`RouterId`] associated with this `SeqNoRequest`.
    pub fn router_id(&self) -> RouterId {
        self.router_id
    }

    /// Return the requested [`SeqNo`] associated with this `SeqNoRequest`.
    pub fn seqno(&self) -> SeqNo {
        self.seqno
    }

    /// Get the hop count for this `SeqNoRequest`.
    pub fn hop_count(&self) -> u8 {
        self.hop_count.into()
    }

    /// Decrement the hop count for this `SeqNoRequest`.
    ///
    /// # Panics
    ///
    /// This function will panic if the hop count before calling this function is 1, as that will
    /// result in a hop count of 0, which is illegal for a `SeqNoRequest`. It is up to the caller
    /// to ensure this condition holds.
    pub fn decrement_hop_count(&mut self) {
        // SAFETY: The panic from this expect is documented in the function signature.
        self.hop_count = NonZeroU8::new(self.hop_count.get() - 1)
            .expect("Decrementing a hop count of 1 is not allowed");
    }

    /// Calculates the size on the wire of this `Update`.
    pub fn wire_size(&self) -> u8 {
        SEQNO_REQUEST_BASE_WIRE_SIZE + (self.prefix.prefix_len() + 7) / 8
        // TODO: Wildcard should be encoded differently
    }

    /// Construct a `SeqNoRequest` from wire bytes.
    ///
    /// # Panics
    ///
    /// This function will panic if there are insufficient bytes present in the provided buffer to
    /// decode a complete `SeqNoRequest`.
    pub fn from_bytes(src: &mut bytes::BytesMut, len: u8) -> Option<Self> {
        let ae = src.get_u8();
        let plen = src.get_u8();
        let seqno = src.get_u16().into();
        let hop_count = src.get_u8();
        // Read "reserved" value, we assume this is 0
        let _ = src.get_u8();

        let mut router_id_bytes = [0u8; RouterId::BYTE_SIZE];
        router_id_bytes.copy_from_slice(&src[..RouterId::BYTE_SIZE]);
        src.advance(RouterId::BYTE_SIZE);

        let router_id = RouterId::from(router_id_bytes);

        let prefix_size = ((plen + 7) / 8) as usize;

        let prefix = match ae {
            AE_WILDCARD => {
                if plen != 0 {
                    return None;
                }
                // TODO: this is a temporary placeholder until we figure out how to handle this
                Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0).into()
            }
            AE_IPV4 => {
                if plen > 32 {
                    return None;
                }
                let mut raw_ip = [0; 4];
                raw_ip[..prefix_size].copy_from_slice(&src[..prefix_size]);
                src.advance(prefix_size);
                Ipv4Addr::from(raw_ip).into()
            }
            AE_IPV6 => {
                if plen > 128 {
                    return None;
                }
                let mut raw_ip = [0; 16];
                raw_ip[..prefix_size].copy_from_slice(&src[..prefix_size]);
                src.advance(prefix_size);
                Ipv6Addr::from(raw_ip).into()
            }
            AE_IPV6_LL => {
                if plen != 64 {
                    return None;
                }
                let mut raw_ip = [0; 16];
                raw_ip[0] = 0xfe;
                raw_ip[1] = 0x80;
                raw_ip[8..].copy_from_slice(&src[..8]);
                src.advance(8);
                Ipv6Addr::from(raw_ip).into()
            }
            _ => {
                // Invalid AE type, skip reamining data and ignore
                trace!("Invalid AE type in seqno_request packet, drop packet");
                src.advance(len as usize - 46);
                return None;
            }
        };

        let prefix = Subnet::new(prefix, plen).ok()?;

        trace!("Read seqno_request tlv body");

        // Make sure hop_count is valid
        let hop_count = if let Some(hc) = NonZeroU8::new(hop_count) {
            hc
        } else {
            debug!("Dropping seqno_request as hop_count field is set to 0");
            return None;
        };

        Some(SeqNoRequest {
            seqno,
            hop_count,
            router_id,
            prefix,
        })
    }

    /// Encode this `SeqNoRequest` tlv as part of a packet.
    pub fn write_bytes(&self, dst: &mut bytes::BytesMut) {
        dst.put_u8(match self.prefix.address() {
            IpAddr::V4(_) => AE_IPV4,
            IpAddr::V6(_) => AE_IPV6,
        });
        dst.put_u8(self.prefix.prefix_len());
        dst.put_u16(self.seqno.into());
        dst.put_u8(self.hop_count.into());
        // Write "reserved" value.
        dst.put_u8(0);
        dst.put_slice(&self.router_id.as_bytes()[..]);
        let prefix_len = ((self.prefix.prefix_len() + 7) / 8) as usize;
        match self.prefix.address() {
            IpAddr::V4(ip) => dst.put_slice(&ip.octets()[..prefix_len]),
            IpAddr::V6(ip) => dst.put_slice(&ip.octets()[..prefix_len]),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, Ipv6Addr},
        num::NonZeroU8,
    };

    use crate::{router_id::RouterId, subnet::Subnet};
    use bytes::Buf;

    #[test]
    fn encoding() {
        let mut buf = bytes::BytesMut::new();

        let snr = super::SeqNoRequest {
            seqno: 17.into(),
            hop_count: NonZeroU8::new(64).unwrap(),
            prefix: Subnet::new(Ipv6Addr::new(512, 25, 26, 27, 28, 0, 0, 29).into(), 64)
                .expect("64 is a valid IPv6 prefix size; qed"),
            router_id: RouterId::from([1u8; RouterId::BYTE_SIZE]),
        };

        snr.write_bytes(&mut buf);

        assert_eq!(buf.len(), 54);
        assert_eq!(
            buf[..54],
            [
                2, 64, 0, 17, 64, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0, 25, 0, 26, 0, 27,
            ]
        );

        let mut buf = bytes::BytesMut::new();

        let snr = super::SeqNoRequest {
            seqno: 170.into(),
            hop_count: NonZeroU8::new(111).unwrap(),
            prefix: Subnet::new(Ipv4Addr::new(10, 101, 4, 1).into(), 32)
                .expect("32 is a valid IPv4 prefix size; qed"),
            router_id: RouterId::from([2u8; RouterId::BYTE_SIZE]),
        };

        snr.write_bytes(&mut buf);

        assert_eq!(buf.len(), 50);
        assert_eq!(
            buf[..50],
            [
                1, 32, 0, 170, 111, 0, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
                2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 10, 101, 4, 1,
            ]
        );
    }

    #[test]
    fn decoding() {
        let mut buf = bytes::BytesMut::from(
            &[
                0, 0, 0, 0, 1, 0, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
                3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
            ][..],
        );

        let snr = super::SeqNoRequest {
            hop_count: NonZeroU8::new(1).unwrap(),
            seqno: 0.into(),
            prefix: Subnet::new(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0).into(), 0)
                .expect("0 is a valid IPv6 prefix size; qed"),
            router_id: RouterId::from([3u8; RouterId::BYTE_SIZE]),
        };

        let buf_len = buf.len();
        assert_eq!(
            super::SeqNoRequest::from_bytes(&mut buf, buf_len as u8),
            Some(snr)
        );
        assert_eq!(buf.remaining(), 0);

        let mut buf = bytes::BytesMut::from(
            &[
                3, 64, 0, 42, 232, 0, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
                4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 0, 10, 0, 20, 0, 30, 0,
                40,
            ][..],
        );

        let snr = super::SeqNoRequest {
            seqno: 42.into(),
            hop_count: NonZeroU8::new(232).unwrap(),
            prefix: Subnet::new(Ipv6Addr::new(0xfe80, 0, 0, 0, 10, 20, 30, 40).into(), 64)
                .expect("92 is a valid IPv6 prefix size; qed"),
            router_id: RouterId::from([4u8; RouterId::BYTE_SIZE]),
        };

        let buf_len = buf.len();
        assert_eq!(
            super::SeqNoRequest::from_bytes(&mut buf, buf_len as u8),
            Some(snr)
        );
        assert_eq!(buf.remaining(), 0);
    }

    #[test]
    fn decode_ignores_invalid_ae_encoding() {
        // AE 4 as it is the first one which should be used in protocol extension, causing this
        // test to fail if we forget to update something
        let mut buf = bytes::BytesMut::from(
            &[
                4, 64, 0, 0, 44, 0, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
                5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 6, 7, 8, 9, 10, 11, 12,
                13, 14, 15, 16, 17, 18, 19, 20, 21,
            ][..],
        );

        let buf_len = buf.len();

        assert_eq!(
            super::SeqNoRequest::from_bytes(&mut buf, buf_len as u8),
            None
        );
        // Decode function should still consume the required amount of bytes to leave parser in a
        // good state (assuming the length in the tlv preamble is good).
        assert_eq!(buf.remaining(), 0);
    }

    #[test]
    fn decode_ignores_invalid_hop_count() {
        // Set all flag bits, only allowed bits should be set on the decoded value
        let mut buf = bytes::BytesMut::from(
            &[
                3, 64, 92, 0, 0, 0, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
                4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 0, 10, 0, 20, 0, 30, 0,
                40,
            ][..],
        );

        let buf_len = buf.len();
        assert_eq!(
            super::SeqNoRequest::from_bytes(&mut buf, buf_len as u8),
            None
        );
        assert_eq!(buf.remaining(), 0);
    }

    #[test]
    fn roundtrip() {
        let mut buf = bytes::BytesMut::new();

        let seqno_src = super::SeqNoRequest::new(
            64.into(),
            RouterId::from([6; RouterId::BYTE_SIZE]),
            Subnet::new(
                Ipv6Addr::new(0x21f, 0x4025, 0xabcd, 0xdead, 0, 0, 0, 0).into(),
                64,
            )
            .expect("64 is a valid IPv6 prefix size; qed"),
        );
        seqno_src.write_bytes(&mut buf);
        let buf_len = buf.len();
        let decoded = super::SeqNoRequest::from_bytes(&mut buf, buf_len as u8);

        assert_eq!(Some(seqno_src), decoded);
        assert_eq!(buf.remaining(), 0);
    }
}
