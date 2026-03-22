//! Pure merge planning.
//!
//! Produces the *intended* merge sequence for a stack of PRs. The planner
//! does not assess readiness — that is the executor's responsibility
//! (just-in-time, with polling for transient forge states).
//!
//! The planner's job is purely structural:
//! 1. For each (bookmark, pr_number) pair, emit a `Merge` step.
//! 2. After each `Merge` step (except the last), emit a `RetargetBase`
//!    step for the next PR so its base points at trunk.

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
    /// Retarget a PR's base branch after a preceding merge.
    RetargetBase {
        bookmark: String,
        pr_number: u64,
        new_base: String,
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
/// Produces a Merge step for each candidate (bottom of stack first),
/// with RetargetBase steps interleaved between consecutive merges.
/// The executor is responsible for readiness assessment and stopping
/// on hard blocks.
pub fn create_merge_plan(
    candidates: &[MergeCandidate],
    trunk_branch: &str,
    method: MergeMethod,
) -> MergePlan {
    let mut steps = Vec::new();
    let bookmarks_in_order: Vec<String> = candidates.iter().map(|c| c.bookmark.clone()).collect();

    for (i, candidate) in candidates.iter().enumerate() {
        steps.push(MergeStep::Merge {
            bookmark: candidate.bookmark.clone(),
            pr_number: candidate.pr_number,
            method,
        });

        // After merging this PR, the next PR's base branch no longer exists
        // on the forge (it was the merged branch). Retarget the next PR to
        // point at trunk so it becomes independently mergeable.
        if let Some(next) = candidates.get(i + 1) {
            steps.push(MergeStep::RetargetBase {
                bookmark: next.bookmark.clone(),
                pr_number: next.pr_number,
                new_base: trunk_branch.to_string(),
            });
        }
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
    fn test_two_candidates_merge_retarget_merge() {
        let candidates = vec![
            MergeCandidate { bookmark: "feat-a".to_string(), pr_number: 1 },
            MergeCandidate { bookmark: "feat-b".to_string(), pr_number: 2 },
        ];
        let plan = create_merge_plan(&candidates, "main", MergeMethod::Squash);

        // Merge A, Retarget B → main, Merge B
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(&plan.steps[0], MergeStep::Merge { pr_number: 1, .. }));
        assert!(matches!(
            &plan.steps[1],
            MergeStep::RetargetBase { pr_number: 2, new_base, .. } if new_base == "main"
        ));
        assert!(matches!(&plan.steps[2], MergeStep::Merge { pr_number: 2, .. }));
    }

    #[test]
    fn test_three_candidates_interleaved_retargets() {
        let candidates = vec![
            MergeCandidate { bookmark: "a".to_string(), pr_number: 10 },
            MergeCandidate { bookmark: "b".to_string(), pr_number: 20 },
            MergeCandidate { bookmark: "c".to_string(), pr_number: 30 },
        ];
        let plan = create_merge_plan(&candidates, "main", MergeMethod::Squash);

        // Merge A, Retarget B, Merge B, Retarget C, Merge C
        let kinds: Vec<&str> = plan.steps.iter().map(|s| match s {
            MergeStep::Merge { .. } => "merge",
            MergeStep::RetargetBase { .. } => "retarget",
        }).collect();
        assert_eq!(kinds, vec!["merge", "retarget", "merge", "retarget", "merge"]);
        assert_eq!(plan.bookmarks_in_order, vec!["a", "b", "c"]);
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

    #[test]
    fn test_trunk_branch_in_retarget() {
        let candidates = vec![
            MergeCandidate { bookmark: "a".to_string(), pr_number: 1 },
            MergeCandidate { bookmark: "b".to_string(), pr_number: 2 },
        ];
        let plan = create_merge_plan(&candidates, "develop", MergeMethod::Squash);

        assert_eq!(plan.trunk_branch, "develop");
        assert!(matches!(
            &plan.steps[1],
            MergeStep::RetargetBase { new_base, .. } if new_base == "develop"
        ));
    }
}