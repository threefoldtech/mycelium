//! The babel [Hello TLV](https://datatracker.ietf.org/doc/html/rfc8966#section-4.6.5).

use bytes::{Buf, BufMut};
use tracing::trace;

use crate::sequence_number::SeqNo;

/// Flag bit indicating a [`Hello`] is sent as unicast hello.
const HELLO_FLAG_UNICAST: u16 = 0x8000;

/// Mask to apply to [`Hello`] flags, leaving only valid flags.
const FLAG_MASK: u16 = 0b10000000_00000000;

/// Wire size of a [`Hello`] TLV without TLV header.
const HELLO_WIRE_SIZE: u8 = 6;

/// Hello TLV body as defined in https://datatracker.ietf.org/doc/html/rfc8966#section-4.6.5.
#[derive(Debug, Clone, PartialEq)]
pub struct Hello {
    flags: u16,
    seqno: SeqNo,
    interval: u16,
}

impl Hello {
    /// Create a new unicast hello packet.
    pub fn new_unicast(seqno: SeqNo, interval: u16) -> Self {
        Self {
            flags: HELLO_FLAG_UNICAST,
            seqno,
            interval,
        }
    }

    /// Calculates the size on the wire of this `Hello`.
    pub fn wire_size(&self) -> u8 {
        HELLO_WIRE_SIZE
    }

    /// Construct a `Hello` from wire bytes.
    ///
    /// # Panics
    ///
    /// This function will panic if there are insufficient bytes present in the provided buffer to
    /// decode a complete `Hello`.
    pub fn from_bytes(src: &mut bytes::BytesMut) -> Self {
        let flags = src.get_u16() & FLAG_MASK;
        let seqno = src.get_u16().into();
        let interval = src.get_u16();

        trace!("Read hello tlv body");

        Self {
            flags,
            seqno,
            interval,
        }
    }

    /// Encode this `Hello` tlv as part of a packet.
    pub fn write_bytes(&self, dst: &mut bytes::BytesMut) {
        dst.put_u16(self.flags);
        dst.put_u16(self.seqno.into());
        dst.put_u16(self.interval);
    }
}

#[cfg(test)]
mod tests {
    use bytes::Buf;

    #[test]
    fn encoding() {
        let mut buf = bytes::BytesMut::new();

        let hello = super::Hello {
            flags: 0,
            seqno: 25.into(),
            interval: 400,
        };

        hello.write_bytes(&mut buf);

        assert_eq!(buf.len(), 6);
        assert_eq!(buf[..6], [0, 0, 0, 25, 1, 144]);

        let mut buf = bytes::BytesMut::new();

        let hello = super::Hello {
            flags: super::HELLO_FLAG_UNICAST,
            seqno: 16.into(),
            interval: 4000,
        };

        hello.write_bytes(&mut buf);

        assert_eq!(buf.len(), 6);
        assert_eq!(buf[..6], [128, 0, 0, 16, 15, 160]);
    }

    #[test]
    fn decoding() {
        let mut buf = bytes::BytesMut::from(&[0b10000000u8, 0b00000000, 0, 19, 2, 1][..]);

        let hello = super::Hello {
            flags: super::HELLO_FLAG_UNICAST,
            seqno: 19.into(),
            interval: 513,
        };

        assert_eq!(super::Hello::from_bytes(&mut buf), hello);
        assert_eq!(buf.remaining(), 0);

        let mut buf = bytes::BytesMut::from(&[0b00000000u8, 0b00000000, 1, 19, 200, 100][..]);

        let hello = super::Hello {
            flags: 0,
            seqno: 275.into(),
            interval: 51300,
        };

        assert_eq!(super::Hello::from_bytes(&mut buf), hello);
        assert_eq!(buf.remaining(), 0);
    }

    #[test]
    fn decode_ignores_invalid_flag_bits() {
        let mut buf = bytes::BytesMut::from(&[0b10001001u8, 0b00000000, 0, 100, 1, 144][..]);

        let hello = super::Hello {
            flags: super::HELLO_FLAG_UNICAST,
            seqno: 100.into(),
            interval: 400,
        };

        assert_eq!(super::Hello::from_bytes(&mut buf), hello);
        assert_eq!(buf.remaining(), 0);

        let mut buf = bytes::BytesMut::from(&[0b00001001u8, 0b00000000, 0, 100, 1, 144][..]);

        let hello = super::Hello {
            flags: 0,
            seqno: 100.into(),
            interval: 400,
        };

        assert_eq!(super::Hello::from_bytes(&mut buf), hello);
        assert_eq!(buf.remaining(), 0);
    }

    #[test]
    fn roundtrip() {
        let mut buf = bytes::BytesMut::new();

        let hello_src = super::Hello::new_unicast(16.into(), 400);
        hello_src.write_bytes(&mut buf);
        let decoded = super::Hello::from_bytes(&mut buf);

        assert_eq!(hello_src, decoded);
        assert_eq!(buf.remaining(), 0);
    }
}
