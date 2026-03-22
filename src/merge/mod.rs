//! Merge engine: plan → execute.
//!
//! The planner produces the *intended* merge sequence (pure function).
//! The executor owns timing, readiness polling, and real-world failure
//! modes — assessing readiness just-in-time before each merge step.

mod execute;
mod plan;

pub use execute::{execute_merge, MergeExecutionResult, ReadinessOutcome};
pub use plan::{create_merge_plan, MergeCandidate, MergePlan, MergeStep};