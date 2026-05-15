//! Phase 3: Submission execution.
//!
//! Pushes bookmarks, creates/updates PRs, updates descriptions,
//! publishes drafts, and adds stack comments.
//!
//! By default, execution aborts on the first error (`abort_on_error = true`).
//! In a stacked PR model each bookmark depends on the one below it, so a
//! mid-stack failure makes downstream steps nonsensical. Pass
//! `abort_on_error = false` (via `--continue-on-error`) to collect all
//! errors without stopping — useful for independent steps like comments.

use std::borrow::Cow;

use crate::error::{JjPlanError, Result};
use crate::platform::PlatformService;
use crate::pr_cache::PrCache;
use crate::submit::plan::{ExecutionStep, SubmissionPlan};
use crate::submit::progress::{Phase, ProgressCallback, PushStatus};
use crate::types::PullRequest;
use crate::workspace::{PushOutcome, Workspace};

/// Pure stage 1: derive the diagnostic and hint for a failed forge-API step.
///
/// For `JjPlanError::PlatformApi`, the diagnostic comes from the error's own
/// `Display` (which renders `{Platform} {Operation} failed (status): message`).
/// For other variants the caller-supplied `fallback_summary` is used. The hint
/// is sourced from `err.hint()` regardless of variant — the single entry
/// point for all hint extraction.
fn step_failure_parts<'a>(
    fallback_summary: &'a str,
    err: &'a JjPlanError,
) -> (String, Option<Cow<'static, str>>) {
    let diagnostic = match err {
        JjPlanError::PlatformApi(_) => err.to_string(),
        _ => format!("{fallback_summary}: {err}"),
    };
    (diagnostic, err.hint())
}

/// Pure stage 2: format diagnostic + optional hint into a single rendered
/// string. The two-tier format is `diagnostic` on the first line and
/// `Hint: ...` on a following line when present.
fn format_step_failure(diagnostic: &str, hint: Option<&str>) -> String {
    match hint {
        Some(h) => format!("{diagnostic}\n  Hint: {h}"),
        None => diagnostic.to_string(),
    }
}

/// Imperative shell: emit the progress event and record the message in
/// `result.errors`. Composes the two-tier output via `step_failure_parts` +
/// `format_step_failure`.
async fn report_step_failure(
    progress: &dyn ProgressCallback,
    result: &mut SubmissionResult,
    fallback_summary: &str,
    err: &JjPlanError,
) -> Result<()> {
    let (diagnostic, hint) = step_failure_parts(fallback_summary, err);
    let msg = format_step_failure(&diagnostic, hint.as_deref());
    progress.on_error(&msg).await?;
    result.errors.push(msg);
    Ok(())
}

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
///
/// When `abort_on_error` is `true` (the default for stacked submit),
/// execution stops at the first failure and reports how many steps were
/// skipped. When `false`, all steps are attempted regardless of earlier
/// failures (useful for independent operations like stack comments).
pub async fn execute_submission(
    plan: &SubmissionPlan,
    workspace: &mut Workspace,
    platform: &dyn PlatformService,
    pr_cache: &mut PrCache,
    progress: &dyn ProgressCallback,
    dry_run: bool,
    abort_on_error: bool,
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
    let total_steps = plan.steps.len();

    for (step_index, step) in plan.steps.iter().enumerate() {
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
                        let fallback = format!("Failed to create PR for {bookmark}");
                        report_step_failure(progress, &mut result, &fallback, &e).await?;
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
                        let fallback = format!("Failed to retarget #{pr_number} ({bookmark})");
                        report_step_failure(progress, &mut result, &fallback, &e).await?;
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
                        let fallback = format!(
                            "Failed to update description for #{pr_number} ({bookmark})"
                        );
                        report_step_failure(progress, &mut result, &fallback, &e).await?;
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
                        let fallback =
                            format!("Failed to publish #{pr_number} ({bookmark})");
                        report_step_failure(progress, &mut result, &fallback, &e).await?;
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
                        let fallback = format!(
                            "Failed to add stack comment on #{pr_number} ({bookmark})"
                        );
                        report_step_failure(progress, &mut result, &fallback, &e).await?;
                    }
                }
            }
        }

        // Abort early if a step failed and abort_on_error is active.
        if abort_on_error && !result.errors.is_empty() {
            let remaining = total_steps - step_index - 1;
            if remaining > 0 {
                progress
                    .on_error(&format!(
                        "Aborting — {remaining} remaining step(s) skipped"
                    ))
                    .await?;
            }
            break;
        }
    }

    progress.on_phase(Phase::Complete).await?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::error::{Operation, build_platform_api_error};
    use crate::types::Platform;

    #[test]
    fn step_failure_parts_platform_api_with_hint_derives_diagnostic_from_error() {
        let pae = build_platform_api_error(
            Platform::GitHub,
            Operation::CreatePr,
            Some("feat/x".to_string()),
            Some(422),
            "Validation Failed".to_string(),
            None,
        );
        let err = JjPlanError::PlatformApi(pae);
        let (diag, hint) = step_failure_parts("UNUSED FALLBACK", &err);
        assert!(diag.contains("GitHub CreatePr failed (422): Validation Failed"));
        assert!(!diag.contains("UNUSED FALLBACK"));
        let h = hint.expect("hint should be present for 422 + CreatePr with target");
        assert!(h.contains("feat/x"));
    }

    #[test]
    fn step_failure_parts_platform_api_without_hint() {
        let pae = build_platform_api_error(
            Platform::GitHub,
            Operation::MergePr,
            None,
            Some(409),
            "Conflict".to_string(),
            None,
        );
        let err = JjPlanError::PlatformApi(pae);
        let (diag, hint) = step_failure_parts("fallback", &err);
        assert!(diag.contains("GitHub MergePr failed (409): Conflict"));
        assert!(hint.is_none());
    }

    #[test]
    fn step_failure_parts_non_platform_api_uses_fallback_summary() {
        let err = JjPlanError::Git("ref export failed".to_string());
        let (diag, hint) = step_failure_parts("Failed to push feat/x", &err);
        assert!(diag.contains("Failed to push feat/x"));
        assert!(diag.contains("ref export failed"));
        assert!(hint.is_none());
    }

    #[test]
    fn format_step_failure_with_hint() {
        let s = format_step_failure("diag line", Some("do the thing"));
        assert!(s.starts_with("diag line"));
        assert!(s.contains("Hint: do the thing"));
    }

    #[test]
    fn format_step_failure_without_hint() {
        assert_eq!(format_step_failure("diag line", None), "diag line");
    }
}