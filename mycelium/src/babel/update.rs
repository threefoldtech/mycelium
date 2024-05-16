//! The babel [Update TLV](https://datatracker.ietf.org/doc/html/rfc8966#name-update).

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};

use bytes::{Buf, BufMut};
use tracing::trace;

use crate::{metric::Metric, router_id::RouterId, sequence_number::SeqNo, subnet::Subnet};

use super::{AE_IPV4, AE_IPV6, AE_IPV6_LL, AE_WILDCARD};

/// Flag bit indicating an [`Update`] TLV establishes a new default prefix.
#[allow(dead_code)]
const UPDATE_FLAG_PREFIX: u8 = 0x80;
/// Flag bit indicating an [`Update`] TLV establishes a new default router-id.
#[allow(dead_code)]
const UPDATE_FLAG_ROUTER_ID: u8 = 0x40;
/// Mask to apply to [`Update`] flags, leaving only valid flags.
const FLAG_MASK: u8 = 0b1100_0000;

/// Base wire size of an [`Update`] without variable length address encoding.
const UPDATE_BASE_WIRE_SIZE: u8 = 10 + RouterId::BYTE_SIZE as u8;

/// Update TLV body as defined in https://datatracker.ietf.org/doc/html/rfc8966#name-update.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    /// Flags set in the TLV.
    flags: u8,
    /// Upper bound in centiseconds after which a new `Update` is sent. Must not be 0.
    interval: u16,
    /// Senders sequence number.
    seqno: SeqNo,
    /// Senders metric for this route.
    metric: Metric,
    /// The [`Subnet`] contained in this update. An update packet itself can contain any allowed
    /// subnet.
    subnet: Subnet,
    /// Router id of the sender. Importantly this is not part of the update itself, though we do
    /// transmit it for now as such.
    router_id: RouterId,
}

impl Update {
    /// Create a new `Update`.
    pub fn new(
        interval: Duration,
        seqno: SeqNo,
        metric: Metric,
        subnet: Subnet,
        router_id: RouterId,
    ) -> Self {
        let interval_centiseconds = (interval.as_millis() / 10) as u16;
        Self {
            // No flags used for now
            flags: 0,
            interval: interval_centiseconds,
            seqno,
            metric,
            subnet,
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

    /// Return the [`Subnet`] in this `Update.`
    pub fn subnet(&self) -> Subnet {
        self.subnet
    }

    /// Return the [`router-id`](PublicKey) of the router who advertised this [`Prefix`](IpAddr).
    pub fn router_id(&self) -> RouterId {
        self.router_id
    }

    /// Calculates the size on the wire of this `Update`.
    pub fn wire_size(&self) -> u8 {
        let address_bytes = (self.subnet.prefix_len() + 7) / 8;
        UPDATE_BASE_WIRE_SIZE + address_bytes
    }

    /// Get the time until a new `Update` for the [`Subnet`] is received at the latest.
    pub fn interval(&self) -> Duration {
        // Interval is expressed as centiseconds on the wire.
        Duration::from_millis(self.interval as u64 * 10)
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
        // Read "omitted" value, we assume this is 0
        let _ = src.get_u8();
        let interval = src.get_u16();
        let seqno = src.get_u16().into();
        let metric = src.get_u16().into();
        let prefix_size = ((plen + 7) / 8) as usize;
        let prefix = match ae {
            AE_WILDCARD => {
                if prefix_size != 0 {
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
                trace!("Invalid AE type in update packet, drop packet");
                src.advance(len as usize - 10);
                return None;
            }
        };

        let subnet = Subnet::new(prefix, plen).ok()?;

        let mut router_id_bytes = [0u8; RouterId::BYTE_SIZE];
        router_id_bytes.copy_from_slice(&src[..RouterId::BYTE_SIZE]);
        src.advance(RouterId::BYTE_SIZE);

        let router_id = RouterId::from(router_id_bytes);

        trace!("Read update tlv body");

        Some(Update {
            flags,
            interval,
            seqno,
            metric,
            subnet,
            router_id,
        })
    }

    /// Encode this `Update` tlv as part of a packet.
    pub fn write_bytes(&self, dst: &mut bytes::BytesMut) {
        dst.put_u8(match self.subnet.address() {
            IpAddr::V4(_) => AE_IPV4,
            IpAddr::V6(_) => AE_IPV6,
        });
        dst.put_u8(self.flags);
        dst.put_u8(self.subnet.prefix_len());
        // Write "omitted" value, currently not used in our encoding scheme.
        dst.put_u8(0);
        dst.put_u16(self.interval);
        dst.put_u16(self.seqno.into());
        dst.put_u16(self.metric.into());
        let prefix_len = ((self.subnet.prefix_len() + 7) / 8) as usize;
        match self.subnet.address() {
            IpAddr::V4(ip) => dst.put_slice(&ip.octets()[..prefix_len]),
            IpAddr::V6(ip) => dst.put_slice(&ip.octets()[..prefix_len]),
        }
        dst.put_slice(&self.router_id.as_bytes()[..])
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, Ipv6Addr},
        time::Duration,
    };

    use crate::{router_id::RouterId, subnet::Subnet};
    use bytes::Buf;

    #[test]
    fn encoding() {
        let mut buf = bytes::BytesMut::new();

        let ihu = super::Update {
            flags: 0b1100_0000,
            interval: 400,
            seqno: 17.into(),
            metric: 25.into(),
            subnet: Subnet::new(Ipv6Addr::new(512, 25, 26, 27, 28, 0, 0, 29).into(), 64)
                .expect("64 is a valid IPv6 prefix size; qed"),
            router_id: RouterId::from([1u8; RouterId::BYTE_SIZE]),
        };

        ihu.write_bytes(&mut buf);

        assert_eq!(buf.len(), 58);
        assert_eq!(
            buf[..58],
            [
                2, 192, 64, 0, 1, 144, 0, 17, 0, 25, 2, 0, 0, 25, 0, 26, 0, 27, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1
            ]
        );

        let mut buf = bytes::BytesMut::new();

        let ihu = super::Update {
            flags: 0b0000_0000,
            interval: 600,
            seqno: 170.into(),
            metric: 256.into(),
            subnet: Subnet::new(Ipv4Addr::new(10, 101, 4, 1).into(), 23)
                .expect("23 is a valid IPv4 prefix size; qed"),
            router_id: RouterId::from([2u8; RouterId::BYTE_SIZE]),
        };

        ihu.write_bytes(&mut buf);

        assert_eq!(buf.len(), 53);
        assert_eq!(
            buf[..53],
            [
                1, 0, 23, 0, 2, 88, 0, 170, 1, 0, 10, 101, 4, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
                2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2
            ]
        );
    }

    #[test]
    fn decoding() {
        let mut buf = bytes::BytesMut::from(
            &[
                0, 64, 0, 0, 0, 100, 0, 70, 2, 0, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
                3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
            ][..],
        );

        let ihu = super::Update {
            flags: 0b0100_0000,
            interval: 100,
            seqno: 70.into(),
            metric: 512.into(),
            subnet: Subnet::new(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0).into(), 0)
                .expect("0 is a valid IPv6 prefix size; qed"),
            router_id: RouterId::from([3u8; RouterId::BYTE_SIZE]),
        };

        let buf_len = buf.len();
        assert_eq!(
            super::Update::from_bytes(&mut buf, buf_len as u8),
            Some(ihu)
        );
        assert_eq!(buf.remaining(), 0);

        let mut buf = bytes::BytesMut::from(
            &[
                3, 0, 64, 0, 3, 232, 0, 42, 3, 1, 0, 10, 0, 20, 0, 30, 0, 40, 4, 4, 4, 4, 4, 4, 4,
                4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
                4, 4, 4, 4, 4,
            ][..],
        );

        let ihu = super::Update {
            flags: 0b0000_0000,
            interval: 1000,
            seqno: 42.into(),
            metric: 769.into(),
            subnet: Subnet::new(Ipv6Addr::new(0xfe80, 0, 0, 0, 10, 20, 30, 40).into(), 64)
                .expect("92 is a valid IPv6 prefix size; qed"),
            router_id: RouterId::from([4u8; RouterId::BYTE_SIZE]),
        };

        let buf_len = buf.len();
        assert_eq!(
            super::Update::from_bytes(&mut buf, buf_len as u8),
            Some(ihu)
        );
        assert_eq!(buf.remaining(), 0);
    }

    #[test]
    fn decode_ignores_invalid_ae_encoding() {
        // AE 4 as it is the first one which should be used in protocol extension, causing this
        // test to fail if we forget to update something
        let mut buf = bytes::BytesMut::from(
            &[
                4, 0, 64, 0, 0, 44, 2, 0, 0, 10, 10, 5, 0, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
                17, 18, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
                5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
            ][..],
        );

        let buf_len = buf.len();

        assert_eq!(super::Update::from_bytes(&mut buf, buf_len as u8), None);
        // Decode function should still consume the required amount of bytes to leave parser in a
        // good state (assuming the length in the tlv preamble is good).
        assert_eq!(buf.remaining(), 0);
    }

    #[test]
    fn decode_ignores_invalid_flag_bits() {
        // Set all flag bits, only allowed bits should be set on the decoded value
        let mut buf = bytes::BytesMut::from(
            &[
                3, 255, 64, 0, 3, 232, 0, 42, 3, 1, 0, 10, 0, 20, 0, 30, 0, 40, 4, 4, 4, 4, 4, 4,
                4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
                4, 4, 4, 4, 4, 4,
            ][..],
        );

        let ihu = super::Update {
            flags: super::UPDATE_FLAG_PREFIX | super::UPDATE_FLAG_ROUTER_ID,
            interval: 1000,
            seqno: 42.into(),
            metric: 769.into(),
            subnet: Subnet::new(Ipv6Addr::new(0xfe80, 0, 0, 0, 10, 20, 30, 40).into(), 64)
                .expect("92 is a valid IPv6 prefix size; qed"),
            router_id: RouterId::from([4u8; RouterId::BYTE_SIZE]),
        };

        let buf_len = buf.len();
        assert_eq!(
            super::Update::from_bytes(&mut buf, buf_len as u8),
            Some(ihu)
        );
        assert_eq!(buf.remaining(), 0);
    }

    #[test]
    fn roundtrip() {
        let mut buf = bytes::BytesMut::new();

        let hello_src = super::Update::new(
            Duration::from_secs(64),
            10.into(),
            25.into(),
            Subnet::new(
                Ipv6Addr::new(0x21f, 0x4025, 0xabcd, 0xdead, 0, 0, 0, 0).into(),
                64,
            )
            .expect("64 is a valid IPv6 prefix size; qed"),
            RouterId::from([6; RouterId::BYTE_SIZE]),
        );
        hello_src.write_bytes(&mut buf);
        let buf_len = buf.len();
        let decoded = super::Update::from_bytes(&mut buf, buf_len as u8);

        assert_eq!(Some(hello_src), decoded);
        assert_eq!(buf.remaining(), 0);
    }
}
