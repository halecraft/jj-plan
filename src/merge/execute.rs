//! Merge readiness helpers.
//!
//! This module provides reusable async building blocks for merge readiness
//! assessment. The imperative merge loop lives in `run_merge_async()`
//! (`stack_cmd.rs`), which calls these helpers alongside workspace operations.
//!
//! - **Pure**: `classify_readiness()` — classifies a readiness snapshot.
//! - **Async helper**: `poll_readiness()` — parameterized polling with `PollConfig`.

use std::time::Duration;

use crate::error::Result;
use crate::platform::PlatformService;
use crate::types::MergeReadiness;

use super::plan::{MergePlan, MergeStep};

/// Result of merge execution (legacy batch API).
#[allow(dead_code)]
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

// ---------------------------------------------------------------------------
// Pure classification
// ---------------------------------------------------------------------------

/// Classify a `MergeReadiness` snapshot into an actionable outcome.
///
/// This is a pure function — no I/O, no retries. The caller invokes it
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

// ---------------------------------------------------------------------------
// Parameterized polling
// ---------------------------------------------------------------------------

/// Configuration for readiness polling.
///
/// Two presets cover the common cases:
/// - `PollConfig::transient()` — short polls (1 s × 15) for forge
///   recomputation after a retarget or merge.
/// - `PollConfig::ci_wait()` — long polls (30 s, unlimited) for CI
///   pipelines after a rebase + push.
#[derive(Debug, Clone)]
pub struct PollConfig {
    /// How long to wait between polls.
    pub interval: Duration,
    /// Maximum number of polls. `None` = no limit (Ctrl-C to abort).
    pub max_attempts: Option<u32>,
    /// Human-readable label for log messages (e.g. "mergeable", "CI").
    pub label: &'static str,
}

impl PollConfig {
    /// Short-poll preset for transient mergeable status.
    ///
    /// 1-second intervals, 15 attempts max. Used before the first merge
    /// in a stack (or after retarget) when the forge is likely still
    /// recomputing.
    pub fn transient() -> Self {
        Self {
            interval: Duration::from_secs(1),
            max_attempts: Some(15),
            label: "mergeable",
        }
    }

    /// Long-poll preset for CI completion after rebase + push.
    ///
    /// 30-second intervals, no max attempts. The user aborts via Ctrl-C.
    pub fn ci_wait() -> Self {
        Self {
            interval: Duration::from_secs(30),
            max_attempts: None,
            label: "CI",
        }
    }
}

/// Poll a PR's merge readiness until it's `Ready` or we determine it's
/// `Blocked`.
///
/// 1. Calls `check_merge_readiness` (single-shot observation).
/// 2. Classifies the result via `classify_readiness`.
/// 3. If `Ready` → returns immediately.
/// 4. If `Blocked` → returns immediately with blocking reasons.
/// 5. If `Transient` → polls at `config.interval` intervals up to
///    `config.max_attempts` (or indefinitely if `None`), then returns
///    `Ready` so the forge's merge endpoint can be the final arbiter.
pub async fn poll_readiness(
    platform: &dyn PlatformService,
    pr_number: u64,
    bookmark: &str,
    config: &PollConfig,
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

    let mut attempt: u32 = 0;
    loop {
        attempt += 1;

        let is_last = config.max_attempts.is_some_and(|max| attempt >= max);

        tokio::time::sleep(config.interval).await;

        // For long polls (CI wait), print elapsed time.
        if config.max_attempts.is_none() {
            let elapsed_secs = u64::from(attempt) * config.interval.as_secs();
            let mins = elapsed_secs / 60;
            let secs = elapsed_secs % 60;
            eprintln!(
                "  Waiting for {} on #{} ({})... [{}m{:02}s]",
                config.label, pr_number, bookmark, mins, secs
            );
        }

        match platform.check_merge_readiness(pr_number).await {
            Ok(readiness) => {
                let outcome = classify_readiness(&readiness);
                match &outcome {
                    ReadinessOutcome::Ready => return Ok(outcome),
                    ReadinessOutcome::Blocked(_) => return Ok(outcome),
                    ReadinessOutcome::Transient => {
                        if is_last {
                            let total_secs =
                                u64::from(attempt) * config.interval.as_secs();
                            eprintln!(
                                "Warning: #{} ({}) {} status did not settle after {}s, proceeding anyway",
                                pr_number,
                                bookmark,
                                config.label,
                                total_secs,
                            );
                            // Return Ready so the caller can attempt the merge —
                            // the forge's merge endpoint is the final arbiter.
                            return Ok(ReadinessOutcome::Ready);
                        }
                    }
                }
            }
            Err(e) => {
                if is_last {
                    return Err(e);
                }
                let max_str = config
                    .max_attempts
                    .map_or("∞".to_string(), |m| m.to_string());
                eprintln!(
                    "Warning: {} poll {}/{} for #{} ({}) failed: {}",
                    config.label, attempt, max_str, pr_number, bookmark, e
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy batch executor (deprecated)
// ---------------------------------------------------------------------------

/// Execute a merge plan in a single batch pass.
///
/// **Deprecated**: Use `poll_readiness` + `platform.merge_pr()` from
/// `run_merge_async` instead. The new merge loop in `stack_cmd.rs`
/// rebases the remaining stack between successive merges, which this
/// batch function cannot do (it has no access to the workspace).
#[deprecated(
    note = "Use poll_readiness + platform.merge_pr() from run_merge_async instead"
)]
#[allow(deprecated, dead_code)]
pub async fn execute_merge(
    plan: &MergePlan,
    platform: &dyn PlatformService,
) -> Result<MergeExecutionResult> {
    let mut result = MergeExecutionResult::default();
    let config = PollConfig::transient();

    for step in &plan.steps {
        match step {
            MergeStep::Merge {
                bookmark,
                pr_number,
                method,
            } => {
                // Assess readiness just-in-time, with polling for transient states.
                match poll_readiness(platform, *pr_number, bookmark, &config).await {
                    Ok(ReadinessOutcome::Ready) => {
                        // Proceed to merge.
                    }
                    Ok(ReadinessOutcome::Transient) => {
                        // poll_readiness exhausted retries but returned Transient
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
        }
    }

    Ok(result)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MergeReadiness;

    // ── Test helper ──────────────────────────────────────────────────

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

    // ── classify_readiness tests (pure) ──────────────────────────────

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
        assert!(matches!(
            classify_readiness(&r),
            ReadinessOutcome::Blocked(_)
        ));
    }

    #[test]
    fn test_classify_blocked_not_approved() {
        let r = readiness(false, true, Some(true), false);
        assert!(matches!(
            classify_readiness(&r),
            ReadinessOutcome::Blocked(_)
        ));
    }

    #[test]
    fn test_classify_blocked_ci_failed() {
        let r = readiness(true, false, Some(true), false);
        assert!(matches!(
            classify_readiness(&r),
            ReadinessOutcome::Blocked(_)
        ));
    }

    #[test]
    fn test_classify_blocked_multiple_reasons() {
        let r = readiness(false, false, Some(false), true);
        match classify_readiness(&r) {
            ReadinessOutcome::Blocked(reasons) => {
                // Should have multiple reasons: draft, not approved, CI, conflicts
                assert!(
                    reasons.len() >= 3,
                    "expected multiple blocking reasons, got: {reasons:?}"
                );
            }
            other => panic!("expected Blocked, got: {other:?}"),
        }
    }

    #[test]
    fn test_classify_blocked_draft_plus_mergeable_false() {
        // Draft + mergeable false → Blocked (not Transient), because draft is a hard blocker.
        let r = readiness(true, true, Some(false), true);
        assert!(matches!(
            classify_readiness(&r),
            ReadinessOutcome::Blocked(_)
        ));
    }

    // ── poll_readiness tests (async, with mock) ──────────────────────

    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use std::collections::VecDeque;
    use crate::types::{
        MergeMethod, MergeResult, PlatformConfig, PullRequest, PullRequestDetails,
        PrComment,
    };

    /// Minimal mock platform for testing `poll_readiness`.
    ///
    /// Scripted responses: each call to `check_merge_readiness` pops the
    /// next `MergeReadiness` from the queue. If the queue is empty, it
    /// returns a "ready" response.
    struct MockPlatform {
        responses: Arc<Mutex<VecDeque<MergeReadiness>>>,
    }

    impl MockPlatform {
        fn new(responses: Vec<MergeReadiness>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            }
        }
    }

    #[async_trait]
    impl PlatformService for MockPlatform {
        async fn find_existing_pr(&self, _head_branch: &str) -> Result<Option<PullRequest>> {
            unimplemented!("not needed for poll_readiness tests")
        }
        async fn create_pr_with_options(
            &self, _head: &str, _base: &str, _title: &str, _body: Option<&str>, _draft: bool,
        ) -> Result<PullRequest> {
            unimplemented!()
        }
        async fn update_pr_base(&self, _pr_number: u64, _new_base: &str) -> Result<PullRequest> {
            unimplemented!()
        }
        async fn update_pr_description(
            &self, _pr_number: u64, _title: &str, _body: &str,
        ) -> Result<PullRequest> {
            unimplemented!()
        }
        async fn publish_pr(&self, _pr_number: u64) -> Result<PullRequest> {
            unimplemented!()
        }
        async fn list_pr_comments(&self, _pr_number: u64) -> Result<Vec<PrComment>> {
            unimplemented!()
        }
        async fn create_pr_comment(&self, _pr_number: u64, _body: &str) -> Result<()> {
            unimplemented!()
        }
        async fn update_pr_comment(
            &self, _pr_number: u64, _comment_id: u64, _body: &str,
        ) -> Result<()> {
            unimplemented!()
        }
        fn config(&self) -> &PlatformConfig {
            unimplemented!()
        }
        async fn get_pr_details(&self, _pr_number: u64) -> Result<PullRequestDetails> {
            unimplemented!()
        }
        async fn check_merge_readiness(&self, _pr_number: u64) -> Result<MergeReadiness> {
            let mut queue = self.responses.lock().unwrap();
            Ok(queue.pop_front().unwrap_or_else(|| {
                // Default: fully ready
                readiness(true, true, Some(true), false)
            }))
        }
        async fn merge_pr(&self, _pr_number: u64, _method: MergeMethod) -> Result<MergeResult> {
            unimplemented!()
        }
    }

    /// Fast poll config for tests — 1 ms intervals.
    fn test_poll_config(max_attempts: Option<u32>) -> PollConfig {
        PollConfig {
            interval: Duration::from_millis(1),
            max_attempts,
            label: "test",
        }
    }

    #[tokio::test]
    async fn test_poll_readiness_ready_immediately() {
        let platform = MockPlatform::new(vec![readiness(true, true, Some(true), false)]);
        let config = test_poll_config(Some(5));

        let outcome = poll_readiness(&platform, 1, "feat-a", &config)
            .await
            .unwrap();
        assert_eq!(outcome, ReadinessOutcome::Ready);

        // Only one response consumed — no polling needed.
        assert_eq!(platform.responses.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_poll_readiness_blocked_immediately() {
        let platform = MockPlatform::new(vec![readiness(false, true, Some(true), false)]);
        let config = test_poll_config(Some(5));

        let outcome = poll_readiness(&platform, 1, "feat-a", &config)
            .await
            .unwrap();
        assert!(matches!(outcome, ReadinessOutcome::Blocked(_)));

        // Only one response consumed.
        assert_eq!(platform.responses.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_poll_readiness_transient_then_ready() {
        let platform = MockPlatform::new(vec![
            // First check: transient (mergeable=false, everything else fine)
            readiness(true, true, Some(false), false),
            // Poll 1: still transient
            readiness(true, true, Some(false), false),
            // Poll 2: ready!
            readiness(true, true, Some(true), false),
        ]);
        let config = test_poll_config(Some(5));

        let outcome = poll_readiness(&platform, 1, "feat-a", &config)
            .await
            .unwrap();
        assert_eq!(outcome, ReadinessOutcome::Ready);

        // All 3 responses consumed.
        assert_eq!(platform.responses.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_poll_readiness_exhausts_max_attempts() {
        // 3 transient responses, max_attempts = 2 (so initial + 2 polls = 3 checks).
        let platform = MockPlatform::new(vec![
            readiness(true, true, Some(false), false),
            readiness(true, true, Some(false), false),
            readiness(true, true, Some(false), false),
        ]);
        let config = test_poll_config(Some(2));

        let outcome = poll_readiness(&platform, 1, "feat-a", &config)
            .await
            .unwrap();
        // After exhausting max_attempts on Transient, returns Ready
        // (proceed anyway — the forge's merge endpoint is the final arbiter).
        assert_eq!(outcome, ReadinessOutcome::Ready);
    }

    #[tokio::test]
    async fn test_poll_readiness_transient_then_blocked() {
        let platform = MockPlatform::new(vec![
            // First: transient
            readiness(true, true, Some(false), false),
            // Poll 1: now blocked (CI failed)
            readiness(true, false, Some(false), false),
        ]);
        let config = test_poll_config(Some(5));

        let outcome = poll_readiness(&platform, 1, "feat-a", &config)
            .await
            .unwrap();
        assert!(matches!(outcome, ReadinessOutcome::Blocked(_)));
    }

    #[tokio::test]
    async fn test_poll_readiness_unlimited_terminates_on_ready() {
        let platform = MockPlatform::new(vec![
            // First check: transient
            readiness(true, true, Some(false), false),
            // Poll 1: still transient
            readiness(true, true, Some(false), false),
            // Poll 2: ready!
            readiness(true, true, Some(true), false),
        ]);
        let config = test_poll_config(None);

        let outcome = poll_readiness(&platform, 1, "feat-a", &config)
            .await
            .unwrap();
        assert_eq!(outcome, ReadinessOutcome::Ready);

        // All 3 responses consumed — unlimited polling terminates on Ready.
        assert_eq!(platform.responses.lock().unwrap().len(), 0);
    }
}