//! Implementations of the [`Metrics`] trait. There are 2 provided options: a NOOP, which
//! essentially disables metrics collection, and a prometheus exporter.

use mycelium::metrics::Metrics;

#[derive(Clone)]
pub struct NoMetrics;
impl Metrics for NoMetrics {}
