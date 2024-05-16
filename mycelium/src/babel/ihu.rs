//! The babel [IHU TLV](https://datatracker.ietf.org/doc/html/rfc8966#name-ihu).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use bytes::{Buf, BufMut};
use tracing::trace;

use crate::metric::Metric;

use super::{AE_IPV4, AE_IPV6, AE_IPV6_LL, AE_WILDCARD};

/// Base wire size of an [`Ihu`] without variable length address encoding.
const IHU_BASE_WIRE_SIZE: u8 = 6;

/// IHU TLV body as defined in https://datatracker.ietf.org/doc/html/rfc8966#name-ihu.
#[derive(Debug, Clone, PartialEq)]
pub struct Ihu {
    rx_cost: Metric,
    interval: u16,
    address: Option<IpAddr>,
}

impl Ihu {
    /// Create a new `Ihu` to be transmitted.
    pub fn new(rx_cost: Metric, interval: u16, address: Option<IpAddr>) -> Self {
        // An interval of 0 is illegal according to the RFC, as this value is used by the receiver
        // to calculate the hold time.
        if interval == 0 {
            panic!("Ihu interval MUST NOT be 0");
        }
        Self {
            rx_cost,
            interval,
            address,
        }
    }

    /// Calculates the size on the wire of this `Ihu`.
    pub fn wire_size(&self) -> u8 {
        IHU_BASE_WIRE_SIZE
            + match self.address {
                None => 0,
                Some(IpAddr::V4(_)) => 4,
                // TODO: link local should be encoded differently
                Some(IpAddr::V6(_)) => 16,
            }
    }

    /// Construct a `Ihu` from wire bytes.
    ///
    /// # Panics
    ///
    /// This function will panic if there are insufficient bytes present in the provided buffer to
    /// decode a complete `Ihu`.
    pub fn from_bytes(src: &mut bytes::BytesMut, len: u8) -> Option<Self> {
        let ae = src.get_u8();
        // read and ignore reserved byte
        let _ = src.get_u8();
        let rx_cost = src.get_u16().into();
        let interval = src.get_u16();
        let address = match ae {
            AE_WILDCARD => None,
            AE_IPV4 => {
                let mut raw_ip = [0; 4];
                raw_ip.copy_from_slice(&src[..4]);
                src.advance(4);
                Some(Ipv4Addr::from(raw_ip).into())
            }
            AE_IPV6 => {
                let mut raw_ip = [0; 16];
                raw_ip.copy_from_slice(&src[..16]);
                src.advance(16);
                Some(Ipv6Addr::from(raw_ip).into())
            }
            AE_IPV6_LL => {
                let mut raw_ip = [0; 16];
                raw_ip[0] = 0xfe;
                raw_ip[1] = 0x80;
                raw_ip[8..].copy_from_slice(&src[..8]);
                src.advance(8);
                Some(Ipv6Addr::from(raw_ip).into())
            }
            _ => {
                // Invalid AE type, skip reamining data and ignore
                trace!("Invalid AE type in IHU TLV, drop TLV");
                src.advance(len as usize - 6);
                return None;
            }
        };

        trace!("Read ihu tlv body");

        Some(Self {
            rx_cost,
            interval,
            address,
        })
    }

    /// Encode this `Ihu` tlv as part of a packet.
    pub fn write_bytes(&self, dst: &mut bytes::BytesMut) {
        dst.put_u8(match self.address {
            None => AE_WILDCARD,
            Some(IpAddr::V4(_)) => AE_IPV4,
            Some(IpAddr::V6(_)) => AE_IPV6,
        });
        // reserved byte, must be all 0
        dst.put_u8(0);
        dst.put_u16(self.rx_cost.into());
        dst.put_u16(self.interval);
        match self.address {
            None => {}
            Some(IpAddr::V4(ip)) => dst.put_slice(&ip.octets()),
            Some(IpAddr::V6(ip)) => dst.put_slice(&ip.octets()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use bytes::Buf;

    #[test]
    fn encoding() {
        let mut buf = bytes::BytesMut::new();

        let ihu = super::Ihu {
            rx_cost: 25.into(),
            interval: 400,
            address: Some(Ipv4Addr::new(1, 1, 1, 1).into()),
        };

        ihu.write_bytes(&mut buf);

        assert_eq!(buf.len(), 10);
        assert_eq!(buf[..10], [1, 0, 0, 25, 1, 144, 1, 1, 1, 1]);

        let mut buf = bytes::BytesMut::new();

        let ihu = super::Ihu {
            rx_cost: 100.into(),
            interval: 4000,
            address: Some(Ipv6Addr::new(2, 0, 1234, 2345, 3456, 4567, 5678, 1).into()),
        };

        ihu.write_bytes(&mut buf);

        assert_eq!(buf.len(), 22);
        assert_eq!(
            buf[..22],
            [2, 0, 0, 100, 15, 160, 0, 2, 0, 0, 4, 210, 9, 41, 13, 128, 17, 215, 22, 46, 0, 1]
        );
    }

    #[test]
    fn decoding() {
        let mut buf = bytes::BytesMut::from(&[0, 0, 0, 1, 1, 44][..]);

        let ihu = super::Ihu {
            rx_cost: 1.into(),
            interval: 300,
            address: None,
        };

        let buf_len = buf.len();
        assert_eq!(super::Ihu::from_bytes(&mut buf, buf_len as u8), Some(ihu));
        assert_eq!(buf.remaining(), 0);

        let mut buf = bytes::BytesMut::from(&[1, 0, 0, 2, 0, 44, 3, 4, 5, 6][..]);

        let ihu = super::Ihu {
            rx_cost: 2.into(),
            interval: 44,
            address: Some(Ipv4Addr::new(3, 4, 5, 6).into()),
        };

        let buf_len = buf.len();
        assert_eq!(super::Ihu::from_bytes(&mut buf, buf_len as u8), Some(ihu));
        assert_eq!(buf.remaining(), 0);

        let mut buf = bytes::BytesMut::from(
            &[
                2, 0, 0, 2, 0, 44, 4, 0, 0, 0, 0, 5, 0, 6, 7, 8, 9, 10, 11, 12, 13, 14,
            ][..],
        );

        let ihu = super::Ihu {
            rx_cost: 2.into(),
            interval: 44,
            address: Some(Ipv6Addr::new(0x400, 0, 5, 6, 0x708, 0x90a, 0xb0c, 0xd0e).into()),
        };

        let buf_len = buf.len();
        assert_eq!(super::Ihu::from_bytes(&mut buf, buf_len as u8), Some(ihu));
        assert_eq!(buf.remaining(), 0);

        let mut buf = bytes::BytesMut::from(&[3, 0, 1, 2, 0, 42, 7, 8, 9, 10, 11, 12, 13, 14][..]);

        let ihu = super::Ihu {
            rx_cost: 258.into(),
            interval: 42,
            address: Some(Ipv6Addr::new(0xfe80, 0, 0, 0, 0x708, 0x90a, 0xb0c, 0xd0e).into()),
        };

        let buf_len = buf.len();
        assert_eq!(super::Ihu::from_bytes(&mut buf, buf_len as u8), Some(ihu));
        assert_eq!(buf.remaining(), 0);
    }

    #[test]
    fn decode_ignores_invalid_ae_encoding() {
        // AE 4 as it is the first one which should be used in protocol extension, causing this
        // test to fail if we forget to update something
        let mut buf = bytes::BytesMut::from(
            &[
                4, 0, 0, 2, 0, 44, 2, 0, 0, 0, 0, 5, 0, 6, 7, 8, 9, 10, 11, 12, 13, 14,
            ][..],
        );

        let buf_len = buf.len();

        assert_eq!(super::Ihu::from_bytes(&mut buf, buf_len as u8), None);
        // Decode function should still consume the required amount of bytes to leave parser in a
        // good state (assuming the length in the tlv preamble is good).
        assert_eq!(buf.remaining(), 0);
    }

    #[test]
    fn roundtrip() {
        let mut buf = bytes::BytesMut::new();

        let hello_src = super::Ihu::new(
            16.into(),
            400,
            Some(Ipv6Addr::new(156, 5646, 4164, 1236, 872, 960, 10, 844).into()),
        );
        hello_src.write_bytes(&mut buf);
        let buf_len = buf.len();
        let decoded = super::Ihu::from_bytes(&mut buf, buf_len as u8);

        assert_eq!(Some(hello_src), decoded);
        assert_eq!(buf.remaining(), 0);
    }
}
