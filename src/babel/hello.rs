//! The babel [Hello TLV](https://datatracker.ietf.org/doc/html/rfc8966#section-4.6.5).

use crate::sequence_number::SeqNo;

/// Flag bit indicating a [`Hello`] is sent as unicast hello.
const HELLO_FLAG_UNICAST: u16 = 0x8000;

/// Hello TLV body as defined in https://datatracker.ietf.org/doc/html/rfc8966#section-4.6.5.
#[derive(Debug, Clone)]
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
}
