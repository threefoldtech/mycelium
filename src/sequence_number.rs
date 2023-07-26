//! Dedicated logic for
//! [sequence numbers](https://datatracker.ietf.org/doc/html/rfc8966#name-solving-starvation-sequenci).

use core::fmt;
use core::ops::{Add, AddAssign};

/// A sequence number on a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord)]
pub struct SeqNo(u16);

impl SeqNo {
    /// Create a new `SeqNo` with the default value.
    pub fn new() -> Self {
        Self::default()
    }
}

impl fmt::Display for SeqNo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("{}", self.0))
    }
}

impl PartialOrd for SeqNo {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        // TODO: Verify
        self.0.partial_cmp(&other.0)
    }
}

impl Default for SeqNo {
    fn default() -> Self {
        SeqNo(0)
    }
}

impl From<u16> for SeqNo {
    fn from(value: u16) -> Self {
        SeqNo(value)
    }
}

impl From<SeqNo> for u16 {
    fn from(value: SeqNo) -> Self {
        value.0
    }
}

impl Add<u16> for SeqNo {
    type Output = Self;

    fn add(self, rhs: u16) -> Self::Output {
        SeqNo(self.0.wrapping_add(rhs))
    }
}

impl AddAssign<u16> for SeqNo {
    fn add_assign(&mut self, rhs: u16) {
        *self = SeqNo(self.0.wrapping_add(rhs))
    }
}
