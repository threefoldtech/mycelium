use mycelium::metrics::Metrics;

#[derive(Clone)]
pub struct NoMetrics;
impl Metrics for NoMetrics {}
