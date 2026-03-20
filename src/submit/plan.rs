//! Phase 2: Submission planning.
//!
//! Determines which bookmarks need push, which need PR creation,
//! and which need base-branch retargeting.

use crate::error::Result;
use crate::platform::PlatformService;
use crate::pr_cache::PrCache;
use crate::submit::analysis::{get_base_branch, SubmissionAnalysis};

/// A step to execute during submission.
#[derive(Debug, Clone)]
pub enum ExecutionStep {
    /// Push a bookmark to the remote.
    Push { bookmark: String },
    /// Create a new PR for a bookmark.
    CreatePr {
        bookmark: String,
        base: String,
        title: String,
        body: String,
        draft: bool,
    },
    /// Update the base branch of an existing PR.
    UpdateBase {
        bookmark: String,
        pr_number: u64,
        new_base: String,
    },
}

/// A submission plan — what needs to happen.
#[derive(Debug)]
pub struct SubmissionPlan {
    pub steps: Vec<ExecutionStep>,
    pub remote: String,
}

impl SubmissionPlan {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn count_pushes(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s, ExecutionStep::Push { .. }))
            .count()
    }

    pub fn count_creates(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s, ExecutionStep::CreatePr { .. }))
            .count()
    }

    pub fn count_updates(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s, ExecutionStep::UpdateBase { .. }))
            .count()
    }
}

/// Create a submission plan from the analysis.
///
/// For each segment:
/// 1. Push the bookmark to the remote
/// 2. If no existing PR → create one
/// 3. If existing PR with wrong base → retarget
pub async fn create_submission_plan(
    analysis: &SubmissionAnalysis,
    platform: &dyn PlatformService,
    _pr_cache: &PrCache,
    remote: &str,
    draft: bool,
    pr_content: &[(String, String, String)], // Vec of (bookmark, title, body)
) -> Result<SubmissionPlan> {
    let mut steps = Vec::new();

    for (i, segment) in analysis.segments.iter().enumerate() {
        let bookmark = &segment.bookmark.name;
        let base = get_base_branch(analysis, i);

        // Always push
        steps.push(ExecutionStep::Push {
            bookmark: bookmark.clone(),
        });

        // Check for existing PR on the platform
        let existing_pr = platform.find_existing_pr(bookmark).await?;

        if let Some(pr) = existing_pr {
            // PR exists — check if base needs updating
            if pr.base_ref != base {
                steps.push(ExecutionStep::UpdateBase {
                    bookmark: bookmark.clone(),
                    pr_number: pr.number,
                    new_base: base,
                });
            }
        } else {
            // No PR — create one
            let (title, body) = pr_content
                .iter()
                .find(|(b, _, _)| b == bookmark)
                .map(|(_, t, b)| (t.clone(), b.clone()))
                .unwrap_or_else(|| {
                    // Fallback to first line of change description
                    let desc = segment
                        .changes
                        .first()
                        .map(|c| c.description.clone())
                        .unwrap_or_default();
                    let title = desc.lines().next().unwrap_or("").to_string();
                    (title, desc)
                });

            steps.push(ExecutionStep::CreatePr {
                bookmark: bookmark.clone(),
                base,
                title,
                body,
                draft,
            });
        }
    }

    Ok(SubmissionPlan {
        steps,
        remote: remote.to_string(),
    })
}