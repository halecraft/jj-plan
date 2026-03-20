//! Three-phase submit engine: analysis → plan → execute.
//!
//! Ported from jj-ryu, adapted for jj-plan's bookmark-based model
//! and plan-file-derived PR descriptions.

mod analysis;
mod execute;
mod plan;
mod progress;

pub use analysis::{SubmissionAnalysis, analyze_submission, get_base_branch, plan_file_to_pr_content};
pub use execute::{execute_submission, SubmissionResult};
pub use plan::{create_submission_plan, ExecutionStep, SubmissionPlan};
pub use progress::{NoopProgress, Phase, ProgressCallback, PushStatus};