//! Phase 2: Submission planning.
//!
//! Determines which bookmarks need push, which need PR creation,
//! and which need base-branch retargeting, description updates,
//! draft publishing, and stack comments.

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
    /// Update the title and body of an existing PR.
    UpdateDescription {
        bookmark: String,
        pr_number: u64,
        title: String,
        body: String,
    },
    /// Convert a draft PR to ready-for-review.
    PublishPr {
        bookmark: String,
        pr_number: u64,
    },
    /// Add or update a stack navigation comment on a PR.
    AddStackComment {
        bookmark: String,
        pr_number: u64,
        comment_body: String,
        existing_comment_id: Option<u64>,
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

    pub fn count_description_updates(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s, ExecutionStep::UpdateDescription { .. }))
            .count()
    }

    pub fn count_publishes(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s, ExecutionStep::PublishPr { .. }))
            .count()
    }


}

/// Create a submission plan from the analysis.
///
/// For each segment:
/// 1. Push the bookmark to the remote
/// 2. If no existing PR → create one
/// 3. If existing PR with wrong base → retarget
/// 4. If `update_descriptions` and title/body differ → update description
/// 5. If `publish` and PR is draft → publish
///
/// Stack comment steps are NOT emitted here — they require PR numbers
/// from freshly-created PRs, so they are added in a second pass after
/// execution. See `run_submit_async` in `stack_cmd.rs`.
pub async fn create_submission_plan(
    analysis: &SubmissionAnalysis,
    platform: &dyn PlatformService,
    _pr_cache: &PrCache,
    remote: &str,
    draft: bool,
    pr_content: &[(String, String, String)], // Vec of (bookmark, title, body)
    update_descriptions: bool,
    publish: bool,
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

            // Check if description needs updating (requires fetching full PR details
            // because find_existing_pr returns PullRequest which has no body field).
            if update_descriptions {
                if let Some((_, plan_title, plan_body)) =
                    pr_content.iter().find(|(b, _, _)| b == bookmark)
                {
                    let details = platform.get_pr_details(pr.number).await?;
                    let existing_body = details.body.as_deref().unwrap_or("");

                    if details.title != *plan_title || existing_body != *plan_body {
                        steps.push(ExecutionStep::UpdateDescription {
                            bookmark: bookmark.clone(),
                            pr_number: pr.number,
                            title: plan_title.clone(),
                            body: plan_body.clone(),
                        });
                    }
                }
            }

            // Check if draft PR should be published.
            // Only publish PRs that were already drafts — don't publish PRs
            // being created in this same run (that's handled by the --draft flag).
            if publish && pr.is_draft {
                steps.push(ExecutionStep::PublishPr {
                    bookmark: bookmark.clone(),
                    pr_number: pr.number,
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