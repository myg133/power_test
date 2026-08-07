//! HTML report generation, text summary, and per-model dashboard.

pub mod charts;
pub mod html;
pub mod i18n;
pub mod model_dashboard;
pub mod summary;

pub use html::{render_html, render_html_with_compare};
pub use model_dashboard::{render_dashboard, run_summary_from_run, RunSummary};
pub use summary::render_summary;
