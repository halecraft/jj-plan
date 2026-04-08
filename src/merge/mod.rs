//! Merge engine: plan → execute.
//!
//! - **Pure**: `classify_readiness` classifies a readiness snapshot.
//! - **Pure**: `create_merge_plan` produces the intended merge sequence.
//! - **Async helper**: `poll_readiness` polls readiness with `PollConfig`.
//! - **Imperative shell**: `run_merge_async` in `stack_cmd.rs` drives the
//!   merge loop, calling these helpers alongside workspace operations.

mod execute;
mod plan;

pub use execute::{classify_readiness, poll_readiness, PollConfig, ReadinessOutcome};
pub use plan::{create_merge_plan, MergeCandidate, MergePlan, MergeStep};