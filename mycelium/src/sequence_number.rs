//! Dedicated logic for
//! [sequence numbers](https://datatracker.ietf.org/doc/html/rfc8966#name-solving-starvation-sequenci).

use core::fmt;
use core::ops::{Add, AddAssign};

/// This value is compared against when deciding if a `SeqNo` is larger or smaller, [as defined in
/// the babel rfc](https://datatracker.ietf.org/doc/html/rfc8966#section-3.2.1).
const SEQNO_COMPARE_TRESHOLD: u16 = 32_768;

/// A sequence number on a route.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeqNo(u16);

impl SeqNo {
    /// Create a new `SeqNo` with the default value.
    pub fn new() -> Self {
        Self::default()
    }

    /// Custom PartialOrd implementation as defined in [the babel rfc](https://datatracker.ietf.org/doc/html/rfc8966#section-3.2.1).
    /// Note that we don't implement the [`PartialOrd`](std::cmd::PartialOrd) trait, as the contract on
    /// that trait specifically defines that it is transitive, which is clearly not the case here.
    ///
    /// There is a quirk in this equality comparison where values which are exactly 32_768 apart,
    /// will result in false in either way of ordering the arguments, which is counterintuitive to
    /// our understanding that a < b generally implies !(b < a).
    pub fn lt(&self, other: &Self) -> bool {
        if self.0 == other.0 {
            false
        } else {
            other.0.wrapping_sub(self.0) < SEQNO_COMPARE_TRESHOLD
        }
    }

    /// Custom PartialOrd implementation as defined in [the babel rfc](https://datatracker.ietf.org/doc/html/rfc8966#section-3.2.1).
    /// Note that we don't implement the [`PartialOrd`](std::cmd::PartialOrd) trait, as the contract on
    /// that trait specifically defines that it is transitive, which is clearly not the case here.
    ///
    /// There is a quirk in this equality comparison where values which are exactly 32_768 apart,
    /// will result in false in either way of ordering the arguments, which is counterintuitive to
    /// our understanding that a < b generally implies !(b < a).
    pub fn gt(&self, other: &Self) -> bool {
        if self.0 == other.0 {
            false
        } else {
            other.0.wrapping_sub(self.0) > SEQNO_COMPARE_TRESHOLD
        }
    }
}

impl fmt::Display for SeqNo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("{}", self.0))
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

#[cfg(test)]
mod tests {
    use super::SeqNo;

    #[test]
    fn cmp_eq_seqno() {
        let s1 = SeqNo::from(1);
        let s2 = SeqNo::from(1);
        assert_eq!(s1, s2);

        let s1 = SeqNo::from(10_000);
        let s2 = SeqNo::from(10_000);
        assert_eq!(s1, s2);
    }

    #[test]
    fn cmp_small_seqno_increase() {
        let s1 = SeqNo::from(1);
        let s2 = SeqNo::from(2);
        assert!(s1.lt(&s2));
        assert!(!s2.lt(&s1));

        assert!(s2.gt(&s1));
        assert!(!s1.gt(&s2));

        let s1 = SeqNo::from(3);
        let s2 = SeqNo::from(30_000);
        assert!(s1.lt(&s2));
        assert!(!s2.lt(&s1));

        assert!(s2.gt(&s1));
        assert!(!s1.gt(&s2));
    }

    #[test]
    fn cmp_big_seqno_increase() {
        let s1 = SeqNo::from(0);
        let s2 = SeqNo::from(32_767);
        assert!(s1.lt(&s2));
        assert!(!s2.lt(&s1));

        assert!(s2.gt(&s1));
        assert!(!s1.gt(&s2));

        // Test equality quirk at cutoff point.
        let s1 = SeqNo::from(0);
        let s2 = SeqNo::from(32_768);
        assert!(!s1.lt(&s2));
        assert!(!s2.lt(&s1));

        assert!(!s2.gt(&s1));
        assert!(!s1.gt(&s2));

        let s1 = SeqNo::from(0);
        let s2 = SeqNo::from(32_769);
        assert!(!s1.lt(&s2));
        assert!(s2.lt(&s1));

        assert!(!s2.gt(&s1));
        assert!(s1.gt(&s2));

        let s1 = SeqNo::from(6);
        let s2 = SeqNo::from(60_000);
        assert!(!s1.lt(&s2));
        assert!(s2.lt(&s1));

        assert!(!s2.gt(&s1));
        assert!(s1.gt(&s2));
    }
}
