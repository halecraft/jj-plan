//! Pure merge planning.
//!
//! Determines which PRs should be merged and in what order.

use crate::types::{MergeMethod, MergeReadiness, NarrowedBookmarkSegment, PullRequestDetails};

/// Information about a PR for merge planning.
#[derive(Debug, Clone)]
pub struct PrInfo {
    pub bookmark: String,
    pub details: PullRequestDetails,
    pub readiness: MergeReadiness,
}

/// Confidence in a merge step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeConfidence {
    /// All checks passed — safe to merge.
    Certain,
    /// Some checks are uncertain (e.g., merge status still computing).
    Uncertain(String),
}

/// A step in the merge plan.
#[derive(Debug, Clone)]
pub enum MergeStep {
    /// Merge this PR.
    Merge {
        bookmark: String,
        pr_number: u64,
        method: MergeMethod,
        confidence: MergeConfidence,
    },
    /// Retarget a PR's base branch after a preceding merge.
    RetargetBase {
        bookmark: String,
        pr_number: u64,
        new_base: String,
    },
    /// Skip this PR (not ready).
    Skip {
        bookmark: String,
        pr_number: u64,
        reason: String,
    },
}

/// A merge plan.
#[derive(Debug)]
pub struct MergePlan {
    pub steps: Vec<MergeStep>,
    /// Bookmarks to clean up after merge.
    #[allow(dead_code)] // Forward-looking: needed for post-merge bookmark cleanup flow.
    pub bookmarks_to_clear: Vec<String>,
    /// The trunk branch name.
    #[allow(dead_code)] // Forward-looking: needed for post-merge rebase-onto-trunk flow.
    pub trunk_branch: String,
    /// Whether there's anything actionable.
    pub has_actionable: bool,
}

/// Create a merge plan from PR info.
///
/// Two-pass algorithm:
/// 1. Collect merge/skip steps from bottom of stack upward
/// 2. Interleave retarget steps for PRs after merged ones
pub fn create_merge_plan(
    segments: &[NarrowedBookmarkSegment],
    pr_info: &[PrInfo],
    trunk_branch: &str,
    method: MergeMethod,
) -> MergePlan {
    let mut steps = Vec::new();
    let mut bookmarks_to_clear = Vec::new();
    let mut has_actionable = false;

    // Pass 1: determine merge/skip for each segment
    let mut can_merge = true; // Can only merge from bottom of stack upward

    for segment in segments {
        let bookmark = &segment.bookmark.name;

        let info = pr_info.iter().find(|i| i.bookmark == *bookmark);

        let Some(info) = info else {
            // No PR for this bookmark — skip
            steps.push(MergeStep::Skip {
                bookmark: bookmark.clone(),
                pr_number: 0,
                reason: "No PR found".to_string(),
            });
            can_merge = false;
            continue;
        };

        if !can_merge {
            steps.push(MergeStep::Skip {
                bookmark: bookmark.clone(),
                pr_number: info.details.number,
                reason: "Earlier PR not mergeable — cannot merge out of order".to_string(),
            });
            continue;
        }

        if info.readiness.is_blocked() {
            let reasons = info.readiness.blocking_reasons.join(", ");
            steps.push(MergeStep::Skip {
                bookmark: bookmark.clone(),
                pr_number: info.details.number,
                reason: format!("Blocked: {reasons}"),
            });
            can_merge = false;
            continue;
        }

        // Can merge
        let confidence = if let Some(u) = info.readiness.uncertainty() {
            MergeConfidence::Uncertain(u.to_string())
        } else {
            MergeConfidence::Certain
        };

        steps.push(MergeStep::Merge {
            bookmark: bookmark.clone(),
            pr_number: info.details.number,
            method,
            confidence: confidence.clone(),
        });

        bookmarks_to_clear.push(bookmark.clone());
        has_actionable = true;
    }

    // Pass 2: interleave retarget steps
    // After merging bookmark N, bookmark N+1's base should change to trunk
    let mut final_steps = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        final_steps.push(step.clone());

        if let MergeStep::Merge { .. } = step {
            // If next step is a non-merge (skip or another merge), we may need retargeting
            if let Some(next) = steps.get(i + 1) {
                match next {
                    MergeStep::Skip {
                        bookmark,
                        pr_number,
                        ..
                    }
                    | MergeStep::Merge {
                        bookmark,
                        pr_number,
                        ..
                    } => {
                        if *pr_number > 0 {
                            final_steps.push(MergeStep::RetargetBase {
                                bookmark: bookmark.clone(),
                                pr_number: *pr_number,
                                new_base: trunk_branch.to_string(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    MergePlan {
        steps: final_steps,
        bookmarks_to_clear,
        trunk_branch: trunk_branch.to_string(),
        has_actionable,
    }
}