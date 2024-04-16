//! This module is used for collection of runtime metrics of a `mycelium` system. The  main item of
//! interest is the [`Metrics`] trait. Users can provide their own implementation of this, or use
//! the default provided implementation to disable gathering metrics.

/// The collection of all metrics exported by a [`mycelium node`](crate::Node). It is up to the
/// user to provide an implementation which implements the methods for metrics they are interested
/// in. All methods have a default implementation, so if the user is not interested in any metrics,
/// a NOOP handler can be implemented as follows:
///
/// ```rust
/// use mycelium::metrics::Metrics;
///
/// #[derive(Clone)]
/// struct NoMetrics;
/// impl Metrics for NoMetrics {}
/// ```
pub trait Metrics {}
