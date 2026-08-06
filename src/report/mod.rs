//! HTML report generation and text summary.

pub mod charts;
pub mod html;
pub mod summary;

pub use html::{render_html, render_html_with_compare};
pub use summary::render_summary;
