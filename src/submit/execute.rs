//! Phase 3: Submission execution.
//!
//! Pushes bookmarks, creates/updates PRs, and adds stack comments.

use crate::error::Result;
use crate::platform::PlatformService;
use crate::pr_cache::PrCache;
use crate::submit::plan::{ExecutionStep, SubmissionPlan};
use crate::submit::progress::{Phase, ProgressCallback, PushStatus};
use crate::types::PullRequest;
use crate::workspace::Workspace;

/// Result of submission execution.
#[derive(Debug, Default)]
pub struct SubmissionResult {
    pub pushed: Vec<String>,
    pub created: Vec<(String, PullRequest)>,
    pub updated: Vec<String>,
    pub errors: Vec<String>,
}

/// Execute the submission plan.
pub async fn execute_submission(
    plan: &SubmissionPlan,
    workspace: &mut Workspace,
    platform: &dyn PlatformService,
    pr_cache: &mut PrCache,
    progress: &dyn ProgressCallback,
    dry_run: bool,
) -> Result<SubmissionResult> {
    let mut result = SubmissionResult::default();

    if dry_run {
        progress.on_phase(Phase::Planning).await?;
        for step in &plan.steps {
            match step {
                ExecutionStep::Push { bookmark } => {
                    progress
                        .on_message(&format!("Would push: {bookmark}"))
                        .await?;
                }
                ExecutionStep::CreatePr {
                    bookmark,
                    base,
                    title,
                    draft,
                    ..
                } => {
                    let draft_str = if *draft { " (draft)" } else { "" };
                    progress
                        .on_message(&format!(
                            "Would create PR: {bookmark} → {base} \"{title}\"{draft_str}"
                        ))
                        .await?;
                }
                ExecutionStep::UpdateBase {
                    bookmark,
                    pr_number,
                    new_base,
                } => {
                    progress
                        .on_message(&format!(
                            "Would retarget: #{pr_number} ({bookmark}) → {new_base}"
                        ))
                        .await?;
                }
            }
        }
        progress.on_phase(Phase::Complete).await?;
        return Ok(result);
    }

    progress.on_phase(Phase::Executing).await?;

    for step in &plan.steps {
        match step {
            ExecutionStep::Push { bookmark } => {
                progress
                    .on_bookmark_push(bookmark, PushStatus::Started)
                    .await?;

                match workspace.git_push(bookmark, &plan.remote) {
                    Ok(()) => {
                        progress
                            .on_bookmark_push(bookmark, PushStatus::Success)
                            .await?;
                        result.pushed.push(bookmark.clone());
                    }
                    Err(e) => {
                        let msg = format!("Failed to push {bookmark}: {e}");
                        progress
                            .on_bookmark_push(bookmark, PushStatus::Failed(msg.clone()))
                            .await?;
                        result.errors.push(msg);
                    }
                }
            }
            ExecutionStep::CreatePr {
                bookmark,
                base,
                title,
                body,
                draft,
            } => {
                match platform
                    .create_pr_with_options(bookmark, base, title, Some(body.as_str()), *draft)
                    .await
                {
                    Ok(pr) => {
                        progress
                            .on_pr_created(bookmark, pr.number, &pr.html_url)
                            .await?;
                        pr_cache.upsert(bookmark, &pr, &plan.remote);
                        result.created.push((bookmark.clone(), pr));
                    }
                    Err(e) => {
                        let msg = format!("Failed to create PR for {bookmark}: {e}");
                        progress.on_error(&msg).await?;
                        result.errors.push(msg);
                    }
                }
            }
            ExecutionStep::UpdateBase {
                bookmark,
                pr_number,
                new_base,
            } => {
                match platform.update_pr_base(*pr_number, new_base).await {
                    Ok(_) => {
                        progress.on_pr_updated(bookmark, *pr_number).await?;
                        result.updated.push(bookmark.clone());
                    }
                    Err(e) => {
                        let msg = format!("Failed to retarget #{pr_number} ({bookmark}): {e}");
                        progress.on_error(&msg).await?;
                        result.errors.push(msg);
                    }
                }
            }
        }
    }

    progress.on_phase(Phase::Complete).await?;
    Ok(result)
}