//! The babel [Hello TLV](https://datatracker.ietf.org/doc/html/rfc8966#section-4.6.5).

use std::io;

use bytes::{Buf, BufMut};

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

    /// Returns the [`SeqNo`] in this `Hello`.
    pub fn seqno(&self) -> SeqNo {
        self.seqno
    }

    /// Returns the interval in this `Hello`. This value is expressed as centiseconds. If this is
    /// non-zero, a new scheduled `Hello` will be sent after this inverval, with the same flag
    /// settings.
    pub fn interval(&self) -> u16 {
        self.interval
    }

    /// Indicates if this is a unicast `Hello`.
    pub fn is_unicast(&self) -> bool {
        self.flags & HELLO_FLAG_UNICAST != 0
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
