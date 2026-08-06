//! Public runner API. Combines the load pattern, executor, and metrics.

pub mod executor;
pub mod metrics;
pub mod pattern;
pub mod session;

pub use executor::{run_with_cancel, RunOptions, RunOutput};
pub use metrics::{aggregator_to_json, CompletionContext, HistKind, MetricsAggregator, RequestRecord};
