//! Dedicated logic for
//! [metrics](https://datatracker.ietf.org/doc/html/rfc8966#metric-computation).

use core::fmt;
use std::ops::{Add, Sub};

/// Value of the infinite metric.
const METRIC_INFINITE: u16 = 0xFFFF;

/// A `Metric` is used to indicate the cost associated with a route. A lower Metric means a route
/// is more favorable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
pub struct Metric(u16);

impl Metric {
    /// Create a new `Metric` with the given value.
    pub const fn new(value: u16) -> Self {
        Metric(value)
    }

    /// Creates a new infinite `Metric`.
    pub const fn infinite() -> Self {
        Metric(METRIC_INFINITE)
    }

    /// Checks if this metric indicates a retracted route.
    pub const fn is_infinite(&self) -> bool {
        self.0 == METRIC_INFINITE
    }

    /// Checks if this metric represents a directly connected route.
    pub const fn is_direct(&self) -> bool {
        self.0 == 0
    }

    /// Computes the absolute value of the difference between this and another `Metric`.
    pub fn delta(&self, rhs: &Self) -> Metric {
        Metric(if self > rhs {
            self.0 - rhs.0
        } else {
            rhs.0 - self.0
        })
    }
}

impl fmt::Display for Metric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_infinite() {
            f.pad("Infinite")
        } else {
            f.write_fmt(format_args!("{}", self.0))
        }
    }
}

impl From<u16> for Metric {
    fn from(value: u16) -> Self {
        Metric(value)
    }
}

impl From<Metric> for u16 {
    fn from(value: Metric) -> Self {
        value.0
    }
}

impl Add for Metric {
    type Output = Self;

    fn add(self, rhs: Metric) -> Self::Output {
        if self.is_infinite() || rhs.is_infinite() {
            return Metric::infinite();
        }
        Metric(
            self.0
                .checked_add(rhs.0)
                .map(|r| if r == u16::MAX { r - 1 } else { r })
                .unwrap_or(u16::MAX - 1),
        )
    }
}

impl Add<&Metric> for &Metric {
    type Output = Metric;

    fn add(self, rhs: &Metric) -> Self::Output {
        if self.is_infinite() || rhs.is_infinite() {
            return Metric::infinite();
        }
        Metric(
            self.0
                .checked_add(rhs.0)
                .map(|r| if r == u16::MAX { r - 1 } else { r })
                .unwrap_or(u16::MAX - 1),
        )
    }
}

impl Add<&Metric> for Metric {
    type Output = Self;

    fn add(self, rhs: &Metric) -> Self::Output {
        if self.is_infinite() || rhs.is_infinite() {
            return Metric::infinite();
        }
        Metric(
            self.0
                .checked_add(rhs.0)
                .map(|r| if r == u16::MAX { r - 1 } else { r })
                .unwrap_or(u16::MAX - 1),
        )
    }
}

impl Add<Metric> for &Metric {
    type Output = Metric;

    fn add(self, rhs: Metric) -> Self::Output {
        if self.is_infinite() || rhs.is_infinite() {
            return Metric::infinite();
        }
        Metric(
            self.0
                .checked_add(rhs.0)
                .map(|r| if r == u16::MAX { r - 1 } else { r })
                .unwrap_or(u16::MAX - 1),
        )
    }
}

impl Sub<Metric> for Metric {
    type Output = Metric;

    fn sub(self, rhs: Metric) -> Self::Output {
        if rhs.is_infinite() {
            panic!("Can't subtract an infinite metric");
        }
        if self.is_infinite() {
            return Metric::infinite();
        }
        Metric(self.0.saturating_sub(rhs.0))
    }
}
