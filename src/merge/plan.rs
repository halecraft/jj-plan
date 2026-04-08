//! Pure merge planning.
//!
//! Produces the *intended* merge sequence for a stack of PRs. The planner
//! does not assess readiness — that is the executor's responsibility
//! (just-in-time, with polling for transient forge states).
//!
//! The planner's job is purely structural:
//! 1. For each (bookmark, pr_number) pair, emit a `Merge` step.
//!
//! The between-merge lifecycle (fetch → rebase → push → retarget) is an
//! imperative concern owned by `run_merge_async`, not the planner.

use crate::types::MergeMethod;

/// A (bookmark, PR number) pair for merge planning.
///
/// This is the minimal input the planner needs — no readiness data,
/// no PR details. The executor fetches those just-in-time.
#[derive(Debug, Clone)]
pub struct MergeCandidate {
    pub bookmark: String,
    pub pr_number: u64,
}

/// A step in the merge plan.
#[derive(Debug, Clone)]
pub enum MergeStep {
    /// Merge this PR.
    Merge {
        bookmark: String,
        pr_number: u64,
        method: MergeMethod,
    },
}

/// A merge plan — the intended sequence of operations.
///
/// The plan always has steps for every candidate. The executor decides
/// at runtime whether each step can proceed (ready), needs to wait
/// (transient), or must stop (blocked).
#[derive(Debug)]
pub struct MergePlan {
    pub steps: Vec<MergeStep>,
    /// Bookmarks in merge order (for post-merge cleanup).
    pub bookmarks_in_order: Vec<String>,
    /// The trunk branch name.
    pub trunk_branch: String,
}

/// Create a merge plan from (bookmark, PR number) pairs.
///
/// Produces a Merge step for each candidate (bottom of stack first).
/// The executor is responsible for readiness assessment, retargeting,
/// and stopping on hard blocks.
pub fn create_merge_plan(
    candidates: &[MergeCandidate],
    trunk_branch: &str,
    method: MergeMethod,
) -> MergePlan {
    let mut steps = Vec::new();
    let bookmarks_in_order: Vec<String> = candidates.iter().map(|c| c.bookmark.clone()).collect();

    for candidate in candidates.iter() {
        steps.push(MergeStep::Merge {
            bookmark: candidate.bookmark.clone(),
            pr_number: candidate.pr_number,
            method,
        });
    }

    MergePlan {
        steps,
        bookmarks_in_order,
        trunk_branch: trunk_branch.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_candidates() {
        let plan = create_merge_plan(&[], "main", MergeMethod::Squash);
        assert!(plan.steps.is_empty());
        assert!(plan.bookmarks_in_order.is_empty());
    }

    #[test]
    fn test_single_candidate() {
        let candidates = vec![MergeCandidate {
            bookmark: "feat-a".to_string(),
            pr_number: 1,
        }];
        let plan = create_merge_plan(&candidates, "main", MergeMethod::Squash);

        assert_eq!(plan.steps.len(), 1);
        assert!(matches!(
            &plan.steps[0],
            MergeStep::Merge { bookmark, pr_number: 1, .. } if bookmark == "feat-a"
        ));
        assert_eq!(plan.bookmarks_in_order, vec!["feat-a"]);
    }

    #[test]
    fn test_merge_method_propagated() {
        let candidates = vec![
            MergeCandidate { bookmark: "a".to_string(), pr_number: 1 },
        ];
        let plan = create_merge_plan(&candidates, "main", MergeMethod::Rebase);

        assert!(matches!(
            &plan.steps[0],
            MergeStep::Merge { method: MergeMethod::Rebase, .. }
        ));
    }
}