//! Report-rendering proxy for the frontend.
//!
//! Formerly a Redis-backed cache (it shielded the expensive live report aggregation); now a thin
//! proxy over the rollup-backed [`crate::backend::Backend::task_report`], so the frontend no longer
//! requires Redis to boot. The module name is retained to limit churn; a rename is tracked as a
//! cleanup follow-up in `docs/KNOWN_ISSUES.md`.
pub mod task_report;

pub use task_report::task_report;
