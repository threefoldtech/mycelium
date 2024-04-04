//! Dedicated logic for
//! [intervals](https://datatracker.ietf.org/doc/html/rfc8966#name-solving-starvation-sequenci).

use std::time::Duration;

/// An interval in the babel protocol.
///
/// Intervals represent a duration, and are expressed in centiseconds (0.01 second / 10
/// milliseconds). `Interval` implements [`From`] [`u16`] to create a new interval from a raw
/// value, and [`From`] [`Duration`] to create a new `Interval` from an existing [`Duration`].
/// There are also implementation to convert back to the aforementioned types. Note that in case of
/// duration, millisecond precision is lost.
#[derive(Debug, Clone)]
pub struct Interval(u16);

impl From<Duration> for Interval {
    fn from(value: Duration) -> Self {
        Interval((value.as_millis() / 10) as u16)
    }
}

impl From<Interval> for Duration {
    fn from(value: Interval) -> Self {
        Duration::from_millis(value.0 as u64 * 10)
    }
}

impl From<u16> for Interval {
    fn from(value: u16) -> Self {
        Interval(value)
    }
}

impl From<Interval> for u16 {
    fn from(value: Interval) -> Self {
        value.0
    }
}
