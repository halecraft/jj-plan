//! Merge execution.
//!
//! The executor is the imperative shell of the merge engine. It owns timing,
//! retries, and real-world failure modes. Before each `Merge` step it
//! assesses readiness **just-in-time** via `poll_until_ready`, which
//! distinguishes transient "forge still computing" states from real blockers.
//!
//! This design ensures that readiness is always evaluated against the
//! *current* forge state — not a stale snapshot taken before execution
//! began. This is critical for stacked merges where each merge + retarget
//! triggers async recomputation on the forge.

use crate::error::Result;
use crate::platform::PlatformService;
use crate::types::MergeReadiness;

use super::plan::{MergePlan, MergeStep};

/// Result of merge execution.
#[derive(Debug, Default)]
pub struct MergeExecutionResult {
    /// Bookmarks that were successfully merged.
    pub merged_bookmarks: Vec<String>,
    /// Bookmark that failed to merge, if any.
    pub failed_bookmark: Option<String>,
    /// Error message for failure.
    pub error_message: Option<String>,
}

/// Outcome of a readiness assessment.
///
/// Used by the executor to decide whether to proceed, wait, or stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadinessOutcome {
    /// All checks passed — safe to merge now.
    Ready,
    /// The only issue is `mergeable == false` (or `None`) with no other
    /// blockers. This is likely transient — the forge is still recomputing
    /// after a graph-changing event (creation, retarget, preceding merge).
    Transient,
    /// Real blockers exist (draft, changes requested, CI failure, etc.).
    /// Contains the blocking reasons for display.
    Blocked(Vec<String>),
}

/// Classify a `MergeReadiness` snapshot into an actionable outcome.
///
/// This is a pure function — no I/O, no retries. The executor calls it
/// after each single-shot `check_merge_readiness` to decide what to do.
///
/// Classification logic:
/// - If `is_blocked()` returns `false` → `Ready`.
/// - If the *only* blocker is `is_mergeable == Some(false)` or `None`
///   (no draft, approved, CI passing) → `Transient`.
/// - Otherwise → `Blocked` with the blocking reasons.
pub fn classify_readiness(readiness: &MergeReadiness) -> ReadinessOutcome {
    if !readiness.is_blocked() {
        return ReadinessOutcome::Ready;
    }

    // Check for hard blockers (anything besides mergeable status).
    let has_hard_blockers = readiness.is_draft
        || !readiness.is_approved
        || !readiness.ci_passed;

    if has_hard_blockers {
        return ReadinessOutcome::Blocked(readiness.blocking_reasons.clone());
    }

    // The only blocker is mergeable status — likely transient.
    // (is_blocked() returned true, but no draft/approval/CI issues,
    // so it must be `is_mergeable == Some(false)`.)
    ReadinessOutcome::Transient
}

/// How long to wait between readiness polls (milliseconds).
const POLL_INTERVAL_MS: u64 = 1000;

/// Maximum number of polls before giving up.
const MAX_POLL_ATTEMPTS: u32 = 15;

/// Poll a PR's merge readiness until it's `Ready` or we determine it's
/// `Blocked`.
///
/// 1. Calls `check_merge_readiness` (single-shot observation).
/// 2. Classifies the result via `classify_readiness`.
/// 3. If `Ready` → returns immediately.
/// 4. If `Blocked` → returns immediately with blocking reasons.
/// 5. If `Transient` → polls at `POLL_INTERVAL_MS` intervals up to
///    `MAX_POLL_ATTEMPTS`, then returns the last outcome.
async fn poll_until_ready(
    platform: &dyn PlatformService,
    pr_number: u64,
    bookmark: &str,
) -> Result<ReadinessOutcome> {
    // First check without delay — often the status is already settled.
    let readiness = platform.check_merge_readiness(pr_number).await?;
    let outcome = classify_readiness(&readiness);

    match &outcome {
        ReadinessOutcome::Ready | ReadinessOutcome::Blocked(_) => return Ok(outcome),
        ReadinessOutcome::Transient => {
            // Fall through to polling loop.
        }
    }

    for attempt in 1..=MAX_POLL_ATTEMPTS {
        tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)).await;

        match platform.check_merge_readiness(pr_number).await {
            Ok(readiness) => {
                let outcome = classify_readiness(&readiness);
                match &outcome {
                    ReadinessOutcome::Ready => return Ok(outcome),
                    ReadinessOutcome::Blocked(_) => return Ok(outcome),
                    ReadinessOutcome::Transient => {
                        if attempt == MAX_POLL_ATTEMPTS {
                            eprintln!(
                                "Warning: #{} ({}) mergeable status did not settle after {}s, proceeding anyway",
                                pr_number,
                                bookmark,
                                (POLL_INTERVAL_MS * u64::from(MAX_POLL_ATTEMPTS)) / 1000
                            );
                            // Return Ready so the executor attempts the merge —
                            // the forge's merge endpoint is the final arbiter.
                            return Ok(ReadinessOutcome::Ready);
                        }
                    }
                }
            }
            Err(e) => {
                if attempt == MAX_POLL_ATTEMPTS {
                    return Err(e);
                }
                eprintln!(
                    "Warning: readiness poll {}/{} for #{} ({}) failed: {}",
                    attempt, MAX_POLL_ATTEMPTS, pr_number, bookmark, e
                );
            }
        }
    }

    // Shouldn't reach here (loop returns on MAX_POLL_ATTEMPTS), but be safe.
    Ok(ReadinessOutcome::Ready)
}

/// Execute a merge plan.
///
/// Processes steps sequentially. Before each `Merge` step, the executor
/// polls the PR's readiness to handle async forge recomputation. Stops
/// at the first hard block or failure.
///
/// Retarget failures are non-fatal (warning only) — the subsequent
/// `Merge` step's readiness poll will detect if the retarget didn't
/// take effect.
pub async fn execute_merge(
    plan: &MergePlan,
    platform: &dyn PlatformService,
) -> Result<MergeExecutionResult> {
    let mut result = MergeExecutionResult::default();

    for step in &plan.steps {
        match step {
            MergeStep::Merge {
                bookmark,
                pr_number,
                method,
            } => {
                // Assess readiness just-in-time, with polling for transient states.
                match poll_until_ready(platform, *pr_number, bookmark).await {
                    Ok(ReadinessOutcome::Ready) => {
                        // Proceed to merge.
                    }
                    Ok(ReadinessOutcome::Transient) => {
                        // poll_until_ready exhausted retries but returned Transient
                        // (shouldn't happen — it returns Ready after max polls).
                        // Attempt the merge anyway.
                        eprintln!(
                            "Warning: #{} ({}) still transient after polling, attempting merge",
                            pr_number, bookmark
                        );
                    }
                    Ok(ReadinessOutcome::Blocked(reasons)) => {
                        let reason_str = reasons.join(", ");
                        eprintln!("  ✗ #{} ({}) blocked: {}", pr_number, bookmark, reason_str);
                        result.failed_bookmark = Some(bookmark.clone());
                        result.error_message = Some(format!("Blocked: {reason_str}"));
                        break;
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: readiness check failed for #{} ({}): {}",
                            pr_number, bookmark, e
                        );
                        // Continue anyway — the merge attempt itself is the
                        // definitive test.
                    }
                }

                eprintln!("  Merging #{} ({})...", pr_number, bookmark);
                match platform.merge_pr(*pr_number, *method).await {
                    Ok(merge_result) => {
                        if merge_result.merged {
                            eprintln!("  ✓ #{} ({}) merged", pr_number, bookmark);
                            result.merged_bookmarks.push(bookmark.clone());
                        } else {
                            result.failed_bookmark = Some(bookmark.clone());
                            result.error_message = merge_result.message;
                            break;
                        }
                    }
                    Err(e) => {
                        result.failed_bookmark = Some(bookmark.clone());
                        result.error_message = Some(format!("{e}"));
                        break;
                    }
                }
            }
            MergeStep::RetargetBase {
                bookmark,
                pr_number,
                new_base,
            } => {
                eprintln!(
                    "  → Retargeting #{} ({}) base → {}",
                    pr_number, bookmark, new_base
                );
                if let Err(e) = platform.update_pr_base(*pr_number, new_base).await {
                    eprintln!(
                        "  Warning: failed to retarget #{} ({}): {}",
                        pr_number, bookmark, e
                    );
                    // Continue — retarget failure is non-fatal.
                    // The next Merge step's readiness poll will detect
                    // if the PR is still not mergeable.
                }
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MergeReadiness;

    fn readiness(
        is_approved: bool,
        ci_passed: bool,
        is_mergeable: Option<bool>,
        is_draft: bool,
    ) -> MergeReadiness {
        let mut blocking_reasons = Vec::new();
        if is_draft {
            blocking_reasons.push("PR is a draft".to_string());
        }
        if !is_approved {
            blocking_reasons.push("Not approved".to_string());
        }
        if !ci_passed {
            blocking_reasons.push("CI not passing".to_string());
        }
        if is_mergeable == Some(false) {
            blocking_reasons.push("Has merge conflicts".to_string());
        }
        MergeReadiness {
            is_approved,
            ci_passed,
            is_mergeable,
            is_draft,
            blocking_reasons,
            uncertainties: vec![],
        }
    }

    #[test]
    fn test_classify_ready() {
        let r = readiness(true, true, Some(true), false);
        assert_eq!(classify_readiness(&r), ReadinessOutcome::Ready);
    }

    #[test]
    fn test_classify_ready_mergeable_none() {
        // mergeable == None but is_blocked() checks `matches!(is_mergeable, Some(false))`,
        // so None does NOT trigger is_blocked(). This should be Ready.
        let r = readiness(true, true, None, false);
        assert_eq!(classify_readiness(&r), ReadinessOutcome::Ready);
    }

    #[test]
    fn test_classify_transient_mergeable_false_only() {
        // Only mergeable is false, everything else is fine → Transient.
        let r = readiness(true, true, Some(false), false);
        assert_eq!(classify_readiness(&r), ReadinessOutcome::Transient);
    }

    #[test]
    fn test_classify_blocked_draft() {
        let r = readiness(true, true, Some(true), true);
        assert!(matches!(classify_readiness(&r), ReadinessOutcome::Blocked(_)));
    }

    #[test]
    fn test_classify_blocked_not_approved() {
        let r = readiness(false, true, Some(true), false);
        assert!(matches!(classify_readiness(&r), ReadinessOutcome::Blocked(_)));
    }

    #[test]
    fn test_classify_blocked_ci_failed() {
        let r = readiness(true, false, Some(true), false);
        assert!(matches!(classify_readiness(&r), ReadinessOutcome::Blocked(_)));
    }

    #[test]
    fn test_classify_blocked_multiple_reasons() {
        let r = readiness(false, false, Some(false), true);
        match classify_readiness(&r) {
            ReadinessOutcome::Blocked(reasons) => {
                // Should have multiple reasons: draft, not approved, CI, conflicts
                assert!(reasons.len() >= 3, "expected multiple blocking reasons, got: {reasons:?}");
            }
            other => panic!("expected Blocked, got: {other:?}"),
        }
    }

    #[test]
    fn test_classify_blocked_draft_plus_mergeable_false() {
        // Draft + mergeable false → Blocked (not Transient), because draft is a hard blocker.
        let r = readiness(true, true, Some(false), true);
        assert!(matches!(classify_readiness(&r), ReadinessOutcome::Blocked(_)));
    }
}