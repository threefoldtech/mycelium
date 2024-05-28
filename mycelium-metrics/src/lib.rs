//! This crate provides implementations of [`the Metrics trait`](mycelium::metrics::Metrics).
//! 2 options are exposed currently: a NOOP implementation which doesn't record anything,
//! and a prometheus exporter which exposes all metrics in a promtheus compatible format.

mod noop;
pub use noop::NoMetrics;

#[cfg(feature = "prometheus")]
mod prometheus;
#[cfg(feature = "prometheus")]
pub use prometheus::PrometheusExporter;
