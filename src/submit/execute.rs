//! Phase 3: Submission execution.
//!
//! Pushes bookmarks, creates/updates PRs, updates descriptions,
//! publishes drafts, and adds stack comments.

use crate::error::Result;
use crate::platform::PlatformService;
use crate::pr_cache::PrCache;
use crate::submit::plan::{ExecutionStep, SubmissionPlan};
use crate::submit::progress::{Phase, ProgressCallback, PushStatus};
use crate::types::PullRequest;
use crate::workspace::{PushOutcome, Workspace};

/// Result of submission execution.
#[derive(Debug, Default)]
pub struct SubmissionResult {
    pub pushed: Vec<String>,
    pub created: Vec<(String, PullRequest)>,
    pub updated: Vec<String>,
    pub description_updated: Vec<String>,
    pub published: Vec<String>,
    pub comments: Vec<String>,
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
                ExecutionStep::UpdateDescription {
                    bookmark,
                    pr_number,
                    title,
                    ..
                } => {
                    progress
                        .on_message(&format!(
                            "Would update description: #{pr_number} ({bookmark}) \"{title}\""
                        ))
                        .await?;
                }
                ExecutionStep::PublishPr {
                    bookmark,
                    pr_number,
                } => {
                    progress
                        .on_message(&format!(
                            "Would publish: #{pr_number} ({bookmark}) draft → ready"
                        ))
                        .await?;
                }
                ExecutionStep::AddStackComment {
                    bookmark,
                    pr_number,
                    existing_comment_id,
                    ..
                } => {
                    let action = if existing_comment_id.is_some() {
                        "update"
                    } else {
                        "add"
                    };
                    progress
                        .on_message(&format!(
                            "Would {action} stack comment: #{pr_number} ({bookmark})"
                        ))
                        .await?;
                }
            }
        }
        progress.on_phase(Phase::Complete).await?;
        return Ok(result);
    }

    progress.on_phase(Phase::Executing).await?;

    // Track whether we've emitted the AddingComments phase header yet.
    let mut in_comment_phase = false;

    for step in &plan.steps {
        match step {
            ExecutionStep::Push { bookmark } => {
                progress
                    .on_bookmark_push(bookmark, PushStatus::Started)
                    .await?;

                match workspace.git_push(bookmark, &plan.remote) {
                    Ok(PushOutcome::Success) => {
                        progress
                            .on_bookmark_push(bookmark, PushStatus::Success)
                            .await?;
                        result.pushed.push(bookmark.clone());
                    }
                    Ok(PushOutcome::Rejected { reason }) => {
                        let msg = format!(
                            "Push rejected for {bookmark}: {reason} \
                             (try `jj git fetch` to refresh tracking state)"
                        );
                        progress
                            .on_bookmark_push(bookmark, PushStatus::Failed(msg.clone()))
                            .await?;
                        result.errors.push(msg);
                    }
                    Ok(PushOutcome::RemoteRejected { reason }) => {
                        let msg = format!(
                            "Push rejected by remote for {bookmark}: {reason}"
                        );
                        progress
                            .on_bookmark_push(bookmark, PushStatus::Failed(msg.clone()))
                            .await?;
                        result.errors.push(msg);
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
            ExecutionStep::UpdateDescription {
                bookmark,
                pr_number,
                title,
                body,
            } => {
                match platform
                    .update_pr_description(*pr_number, title, body)
                    .await
                {
                    Ok(_) => {
                        progress
                            .on_message(&format!(
                                "  Updated description for #{pr_number} ({bookmark})"
                            ))
                            .await?;
                        result.description_updated.push(bookmark.clone());
                    }
                    Err(e) => {
                        let msg = format!(
                            "Failed to update description for #{pr_number} ({bookmark}): {e}"
                        );
                        progress.on_error(&msg).await?;
                        result.errors.push(msg);
                    }
                }
            }
            ExecutionStep::PublishPr {
                bookmark,
                pr_number,
            } => {
                match platform.publish_pr(*pr_number).await {
                    Ok(_) => {
                        progress
                            .on_message(&format!(
                                "  Published #{pr_number} ({bookmark}): draft → ready"
                            ))
                            .await?;
                        result.published.push(bookmark.clone());
                    }
                    Err(e) => {
                        let msg =
                            format!("Failed to publish #{pr_number} ({bookmark}): {e}");
                        progress.on_error(&msg).await?;
                        result.errors.push(msg);
                    }
                }
            }
            ExecutionStep::AddStackComment {
                bookmark,
                pr_number,
                comment_body,
                existing_comment_id,
            } => {
                // Emit the AddingComments phase header once, on first comment step.
                if !in_comment_phase {
                    progress.on_phase(Phase::AddingComments).await?;
                    in_comment_phase = true;
                }

                let comment_result = if let Some(comment_id) = existing_comment_id {
                    platform
                        .update_pr_comment(*pr_number, *comment_id, comment_body)
                        .await
                } else {
                    platform.create_pr_comment(*pr_number, comment_body).await
                };

                match comment_result {
                    Ok(()) => {
                        let action = if existing_comment_id.is_some() {
                            "Updated"
                        } else {
                            "Added"
                        };
                        progress
                            .on_message(&format!(
                                "  {action} stack comment on #{pr_number} ({bookmark})"
                            ))
                            .await?;
                        result.comments.push(bookmark.clone());
                    }
                    Err(e) => {
                        let msg = format!(
                            "Failed to add stack comment on #{pr_number} ({bookmark}): {e}"
                        );
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