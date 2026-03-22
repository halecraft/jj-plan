//! Three-phase submit engine: analysis → plan → execute.
//!
//! Ported from jj-ryu, adapted for jj-plan's bookmark-based model
//! and plan-file-derived PR descriptions.

mod analysis;
mod execute;
mod plan;
mod progress;

pub use analysis::analyze_submission;
pub use execute::execute_submission;
pub use plan::create_submission_plan;
pub use progress::{Phase, ProgressCallback, PushStatus};