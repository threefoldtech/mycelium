use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use bytes::{Buf, BufMut};
use tracing::trace;

use crate::subnet::Subnet;

use super::{AE_IPV4, AE_IPV6, AE_IPV6_LL, AE_WILDCARD};

/// Base wire size of a [`RouteRequest`] without variable length address encoding.
const ROUTE_REQUEST_BASE_WIRE_SIZE: u8 = 2;

/// Seqno request TLV body as defined in https://datatracker.ietf.org/doc/html/rfc8966#name-route-request
#[derive(Debug, Clone, PartialEq)]
pub struct RouteRequest {
    /// The prefix being requested
    prefix: Option<Subnet>,
}

impl RouteRequest {
    /// Creates a new `RouteRequest` for the given [`prefix`]. If no [`prefix`] is given, a full
    /// route table dumb in requested.
    ///
    /// [`prefix`]: Subnet
    pub fn new(prefix: Option<Subnet>) -> Self {
        Self { prefix }
    }

    /// Return the [`prefix`](Subnet) associated with this `RouteRequest`.
    pub fn prefix(&self) -> Option<Subnet> {
        self.prefix
    }

    /// Calculates the size on the wire of this `RouteRequest`.
    pub fn wire_size(&self) -> u8 {
        ROUTE_REQUEST_BASE_WIRE_SIZE
            + (if let Some(prefix) = self.prefix {
                (prefix.prefix_len() + 7) / 8
            } else {
                0
            })
    }

    /// Construct a `RouteRequest` from wire bytes.
    ///
    /// # Panics
    ///
    /// This function will panic if there are insufficient bytes present in the provided buffer to
    /// decode a complete `RouteRequest`.
    pub fn from_bytes(src: &mut bytes::BytesMut, len: u8) -> Option<Self> {
        let ae = src.get_u8();
        let plen = src.get_u8();

        let prefix_size = ((plen + 7) / 8) as usize;

        let prefix_ip = match ae {
            AE_WILDCARD => None,
            AE_IPV4 => {
                if plen > 32 {
                    return None;
                }
                let mut raw_ip = [0; 4];
                raw_ip[..prefix_size].copy_from_slice(&src[..prefix_size]);
                src.advance(prefix_size);
                Some(Ipv4Addr::from(raw_ip).into())
            }
            AE_IPV6 => {
                if plen > 128 {
                    return None;
                }
                let mut raw_ip = [0; 16];
                raw_ip[..prefix_size].copy_from_slice(&src[..prefix_size]);
                src.advance(prefix_size);
                Some(Ipv6Addr::from(raw_ip).into())
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
                Some(Ipv6Addr::from(raw_ip).into())
            }
            _ => {
                // Invalid AE type, skip reamining data and ignore
                trace!("Invalid AE type in route_request packet, drop packet");
                src.advance(len as usize - 2);
                return None;
            }
        };

        let prefix = prefix_ip.and_then(|prefix| Subnet::new(prefix, plen).ok());

        trace!("Read route_request tlv body");

        Some(RouteRequest { prefix })
    }

    /// Encode this `RouteRequest` tlv as part of a packet.
    pub fn write_bytes(&self, dst: &mut bytes::BytesMut) {
        if let Some(prefix) = self.prefix {
            dst.put_u8(match prefix.address() {
                IpAddr::V4(_) => AE_IPV4,
                IpAddr::V6(_) => AE_IPV6,
            });
            dst.put_u8(prefix.prefix_len());
            let prefix_len = ((prefix.prefix_len() + 7) / 8) as usize;
            match prefix.address() {
                IpAddr::V4(ip) => dst.put_slice(&ip.octets()[..prefix_len]),
                IpAddr::V6(ip) => dst.put_slice(&ip.octets()[..prefix_len]),
            }
        } else {
            dst.put_u8(AE_WILDCARD);
            // Prefix len MUST be 0 for wildcard requests
            dst.put_u8(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use bytes::Buf;

    use crate::subnet::Subnet;

    #[test]
    fn encoding() {
        let mut buf = bytes::BytesMut::new();

        let rr = super::RouteRequest {
            prefix: Some(
                Subnet::new(Ipv6Addr::new(512, 25, 26, 27, 28, 0, 0, 29).into(), 64)
                    .expect("64 is a valid IPv6 prefix size; qed"),
            ),
        };

        rr.write_bytes(&mut buf);

        assert_eq!(buf.len(), 10);
        assert_eq!(buf[..10], [2, 64, 2, 0, 0, 25, 0, 26, 0, 27]);

        let mut buf = bytes::BytesMut::new();

        let rr = super::RouteRequest {
            prefix: Some(
                Subnet::new(Ipv4Addr::new(10, 101, 4, 1).into(), 32)
                    .expect("32 is a valid IPv4 prefix size; qed"),
            ),
        };

        rr.write_bytes(&mut buf);

        assert_eq!(buf.len(), 6);
        assert_eq!(buf[..6], [1, 32, 10, 101, 4, 1]);

        let mut buf = bytes::BytesMut::new();

        let rr = super::RouteRequest { prefix: None };

        rr.write_bytes(&mut buf);

        assert_eq!(buf.len(), 2);
        assert_eq!(buf[..2], [0, 0]);
    }

    #[test]
    fn decoding() {
        let mut buf = bytes::BytesMut::from(&[0, 0][..]);

        let rr = super::RouteRequest { prefix: None };

        let buf_len = buf.len();
        assert_eq!(
            super::RouteRequest::from_bytes(&mut buf, buf_len as u8),
            Some(rr)
        );
        assert_eq!(buf.remaining(), 0);

        let mut buf = bytes::BytesMut::from(&[1, 24, 10, 15, 19][..]);

        let rr = super::RouteRequest {
            prefix: Some(
                Subnet::new(Ipv4Addr::new(10, 15, 19, 0).into(), 24)
                    .expect("24 is a valid IPv4 prefix size; qed"),
            ),
        };

        let buf_len = buf.len();
        assert_eq!(
            super::RouteRequest::from_bytes(&mut buf, buf_len as u8),
            Some(rr)
        );
        assert_eq!(buf.remaining(), 0);

        let mut buf = bytes::BytesMut::from(&[2, 64, 0, 10, 0, 20, 0, 30, 0, 40][..]);

        let rr = super::RouteRequest {
            prefix: Some(
                Subnet::new(Ipv6Addr::new(10, 20, 30, 40, 0, 0, 0, 0).into(), 64)
                    .expect("64 is a valid IPv6 prefix size; qed"),
            ),
        };

        let buf_len = buf.len();
        assert_eq!(
            super::RouteRequest::from_bytes(&mut buf, buf_len as u8),
            Some(rr)
        );
        assert_eq!(buf.remaining(), 0);

        let mut buf = bytes::BytesMut::from(&[3, 64, 0, 10, 0, 20, 0, 30, 0, 40][..]);

        let rr = super::RouteRequest {
            prefix: Some(
                Subnet::new(Ipv6Addr::new(0xfe80, 0, 0, 0, 10, 20, 30, 40).into(), 64)
                    .expect("64 is a valid IPv6 prefix size; qed"),
            ),
        };

        let buf_len = buf.len();
        assert_eq!(
            super::RouteRequest::from_bytes(&mut buf, buf_len as u8),
            Some(rr)
        );
        assert_eq!(buf.remaining(), 0);
    }

    #[test]
    fn decode_ignores_invalid_ae_encoding() {
        // AE 4 as it is the first one which should be used in protocol extension, causing this
        // test to fail if we forget to update something
        let mut buf = bytes::BytesMut::from(
            &[
                4, 64, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
            ][..],
        );

        let buf_len = buf.len();

        assert_eq!(
            super::RouteRequest::from_bytes(&mut buf, buf_len as u8),
            None
        );
        // Decode function should still consume the required amount of bytes to leave parser in a
        // good state (assuming the length in the tlv preamble is good).
        assert_eq!(buf.remaining(), 0);
    }

    #[test]
    fn roundtrip() {
        let mut buf = bytes::BytesMut::new();

        let seqno_src = super::RouteRequest::new(Some(
            Subnet::new(
                Ipv6Addr::new(0x21f, 0x4025, 0xabcd, 0xdead, 0, 0, 0, 0).into(),
                64,
            )
            .expect("64 is a valid IPv6 prefix size; qed"),
        ));
        seqno_src.write_bytes(&mut buf);
        let buf_len = buf.len();
        let decoded = super::RouteRequest::from_bytes(&mut buf, buf_len as u8);

        assert_eq!(Some(seqno_src), decoded);
        assert_eq!(buf.remaining(), 0);
    }
}
