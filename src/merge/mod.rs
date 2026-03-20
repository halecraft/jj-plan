//! Merge engine: plan → execute.
//!
//! Ported from jj-ryu. Plans which PRs to merge (pure function),
//! then executes the merges via the platform API.

mod execute;
mod plan;

pub use execute::{execute_merge, MergeExecutionResult};
pub use plan::{create_merge_plan, MergeConfidence, MergePlan, MergeStep, PrInfo};