//! Gitea integration tests for jj-plan platform service.
//!
//! Validates the full PR lifecycle (submit, merge, comments, drafts,
//! description updates) against a real Gitea instance.
//!
//! **Gated**: Skipped unless `GITEA_INTEGRATION=1` is set.
//!
//! Run:
//! ```sh
//! GITEA_INTEGRATION=1 GITEA_HOST=code.halecraft.org GITEA_TOKEN=xxx \
//!   cargo test --test gitea_integration -- --test-threads=1
//! ```
//!
//! Each test creates a throwaway private repo and deletes it on completion.

use std::env;

use reqwest::Client;
use serde::Deserialize;

use jj_plan::merge::{create_merge_plan, execute_merge, MergeCandidate, MergeStep};
use jj_plan::platform::GiteaService;
use jj_plan::platform::PlatformService;
use jj_plan::types::{MergeMethod, Platform, PlatformConfig, PrState};

// ── Gate ─────────────────────────────────────────────────────────────────────

fn should_run() -> bool {
    env::var("GITEA_INTEGRATION").as_deref() == Ok("1")
}

fn require_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} must be set"))
}

// ── Test harness ─────────────────────────────────────────────────────────────

struct GiteaTestRepo {
    client: Client,
    host: String,
    token: String,
    owner: String,
    repo: String,
}

#[derive(Deserialize)]
struct CreatedRepo {
    name: String,
}

#[derive(Deserialize)]
struct User {
    login: String,
}

impl GiteaTestRepo {
    async fn create(suffix: &str) -> Self {
        let host = require_env("GITEA_HOST");
        let token = require_env("GITEA_TOKEN");

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");

        let user: User = client
            .get(format!("https://{host}/api/v1/user"))
            .header("Authorization", format!("token {token}"))
            .send()
            .await
            .expect("failed to reach Gitea")
            .error_for_status()
            .expect("auth failed")
            .json()
            .await
            .expect("bad user response");

        let repo_name = format!("jj-plan-test-{suffix}");

        // Delete any leftover repo from a previous failed run (idempotent).
        let _ = client
            .delete(format!("https://{host}/api/v1/repos/{}/{repo_name}", user.login))
            .header("Authorization", format!("token {token}"))
            .send()
            .await;

        let created: CreatedRepo = client
            .post(format!("https://{host}/api/v1/user/repos"))
            .header("Authorization", format!("token {token}"))
            .json(&serde_json::json!({
                "name": &repo_name,
                "auto_init": true,
                "default_branch": "main",
                "private": true,
            }))
            .send()
            .await
            .expect("failed to create repo")
            .error_for_status()
            .expect("repo creation failed")
            .json()
            .await
            .expect("bad repo response");

        // Give Gitea a moment to initialise the repo
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        Self {
            client,
            host,
            token,
            owner: user.login,
            repo: created.name,
        }
    }

    fn api(&self, path: &str) -> String {
        format!(
            "https://{}/api/v1/repos/{}/{}{}",
            self.host, self.owner, self.repo, path
        )
    }

    fn auth_value(&self) -> String {
        format!("token {}", self.token)
    }

    async fn create_branch(&self, new_branch: &str, from_branch: &str) {
        self.client
            .post(self.api("/branches"))
            .header("Authorization", self.auth_value())
            .json(&serde_json::json!({
                "new_branch_name": new_branch,
                "old_branch_name": from_branch,
            }))
            .send()
            .await
            .expect("failed to create branch")
            .error_for_status()
            .expect("branch creation failed");
    }

    async fn create_file(&self, path: &str, branch: &str, content: &str) {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        self.client
            .post(self.api(&format!("/contents/{path}")))
            .header("Authorization", self.auth_value())
            .json(&serde_json::json!({
                "message": format!("add {path}"),
                "content": encoded,
                "branch": branch,
            }))
            .send()
            .await
            .expect("failed to create file")
            .error_for_status()
            .expect("file creation failed");
    }

    async fn setup_stacked_branches(&self) {
        self.create_branch("branch-a", "main").await;
        self.create_file("file-a.txt", "branch-a", "content-a").await;
        self.create_branch("branch-b", "branch-a").await;
        self.create_file("file-b.txt", "branch-b", "content-b").await;
    }

    /// Set up a 3-deep stack: branch-a → main, branch-b → branch-a, branch-c → branch-b.
    async fn setup_3_stack(&self) {
        self.setup_stacked_branches().await;
        self.create_branch("branch-c", "branch-b").await;
        self.create_file("file-c.txt", "branch-c", "content-c").await;
    }

    fn service(&self) -> GiteaService {
        GiteaService::new(
            self.token.clone(),
            self.owner.clone(),
            self.repo.clone(),
            Some(self.host.clone()),
        )
        .expect("failed to create GiteaService")
    }

    async fn teardown(&self) {
        let _ = self
            .client
            .delete(format!(
                "https://{}/api/v1/repos/{}/{}",
                self.host, self.owner, self.repo
            ))
            .header("Authorization", self.auth_value())
            .send()
            .await;
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

/// Task 3.2: Submit integration test — create PRs, find, retarget, comments.
#[tokio::test]
async fn test_submit_lifecycle() {
    if !should_run() {
        eprintln!("Skipped (set GITEA_INTEGRATION=1 to run)");
        return;
    }

    let repo = GiteaTestRepo::create("submit").await;
    let svc = repo.service();
    repo.setup_stacked_branches().await;

    // ── Create PR #1: branch-a → main ────────────────────────────────────
    let pr1 = svc
        .create_pr_with_options("branch-a", "main", "PR 1: branch-a", Some("First PR in stack"), false)
        .await
        .expect("create PR 1");

    assert_eq!(pr1.base_ref, "main");
    assert_eq!(pr1.head_ref, "branch-a");
    assert!(!pr1.is_draft);

    // ── Create PR #2: branch-b → branch-a ────────────────────────────────
    let pr2 = svc
        .create_pr_with_options("branch-b", "branch-a", "PR 2: branch-b", Some("Second PR in stack"), false)
        .await
        .expect("create PR 2");

    assert_eq!(pr2.base_ref, "branch-a");
    assert_eq!(pr2.head_ref, "branch-b");

    // ── find_existing_pr ─────────────────────────────────────────────────
    let found = svc.find_existing_pr("branch-a").await.expect("find PR 1");
    assert!(found.is_some());
    assert_eq!(found.unwrap().number, pr1.number);

    let found2 = svc.find_existing_pr("branch-b").await.expect("find PR 2");
    assert!(found2.is_some());
    assert_eq!(found2.unwrap().number, pr2.number);

    let not_found = svc.find_existing_pr("nonexistent-branch").await.expect("find nonexistent");
    assert!(not_found.is_none());

    // ── update_pr_base (retarget) ────────────────────────────────────────
    let retargeted = svc.update_pr_base(pr2.number, "main").await.expect("retarget PR 2");
    assert_eq!(retargeted.base_ref, "main");

    // Retarget back for consistency
    let _ = svc.update_pr_base(pr2.number, "branch-a").await.expect("retarget PR 2 back");

    // ── Comments ─────────────────────────────────────────────────────────
    let marker = "<!-- jj-plan stack -->";
    let comment_body = format!("{marker}\n### Stack\ntest comment");

    svc.create_pr_comment(pr1.number, &comment_body).await.expect("create comment");

    let comments = svc.list_pr_comments(pr1.number).await.expect("list comments");
    assert!(!comments.is_empty());

    let jj_comment = comments.iter().find(|c| c.body.contains(marker));
    assert!(jj_comment.is_some(), "should find jj-plan stack comment");
    let comment_id = jj_comment.unwrap().id;

    // Update comment
    let updated_body = format!("{marker}\n### Stack\nupdated comment");
    svc.update_pr_comment(pr1.number, comment_id, &updated_body)
        .await
        .expect("update comment");

    let comments_after = svc.list_pr_comments(pr1.number).await.expect("list comments after update");
    let updated = comments_after.iter().find(|c| c.id == comment_id);
    assert!(updated.is_some());
    assert!(updated.unwrap().body.contains("updated comment"));

    repo.teardown().await;
}

/// Task 3.3: Merge integration test — full stack merge with retarget.
#[tokio::test]
async fn test_merge_lifecycle() {
    if !should_run() {
        eprintln!("Skipped (set GITEA_INTEGRATION=1 to run)");
        return;
    }

    let repo = GiteaTestRepo::create("merge").await;
    let svc = repo.service();
    repo.setup_stacked_branches().await;

    // Create stacked PRs
    let pr1 = svc
        .create_pr_with_options("branch-a", "main", "PR 1: branch-a", Some("First"), false)
        .await
        .expect("create PR 1");

    let pr2 = svc
        .create_pr_with_options("branch-b", "branch-a", "PR 2: branch-b", Some("Second"), false)
        .await
        .expect("create PR 2");

    // ── Check merge readiness on PR #1 ───────────────────────────────────
    // Single-shot observation — no polling here. The merge executor's
    // poll_until_ready handles transient states generically.
    let readiness = svc.check_merge_readiness(pr1.number).await.expect("check readiness PR 1");
    // Note: readiness may show mergeable=false transiently right after
    // PR creation. We check the non-timing fields only.
    assert!(readiness.is_approved, "PR 1 should be approved (no required reviews)");
    assert!(readiness.ci_passed, "PR 1 CI should pass (Gitea optimistic)");
    assert!(!readiness.is_draft, "PR 1 should not be a draft");

    // ── Merge PR #1 (squash) ─────────────────────────────────────────────
    let merge1 = svc.merge_pr(pr1.number, MergeMethod::Squash).await.expect("merge PR 1");
    assert!(merge1.merged, "PR 1 should be merged");
    assert!(merge1.sha.is_some(), "should have merge commit SHA");

    // Verify state via details
    let details1 = svc.get_pr_details(pr1.number).await.expect("get PR 1 details");
    assert_eq!(details1.state, PrState::Merged);

    // ── Retarget PR #2 base → main ──────────────────────────────────────
    let retargeted = svc.update_pr_base(pr2.number, "main").await.expect("retarget PR 2");
    assert_eq!(retargeted.base_ref, "main");

    // No explicit pause — the merge executor's poll_until_ready handles
    // waiting for Gitea to recompute mergeable status after retargeting.

    // ── Merge PR #2 (squash) ─────────────────────────────────────────────
    let merge2 = svc.merge_pr(pr2.number, MergeMethod::Squash).await.expect("merge PR 2");
    assert!(merge2.merged, "PR 2 should be merged");

    let details2 = svc.get_pr_details(pr2.number).await.expect("get PR 2 details");
    assert_eq!(details2.state, PrState::Merged);

    repo.teardown().await;
}

/// Task 3.4: Draft lifecycle — create as draft, verify, publish, verify.
#[tokio::test]
async fn test_draft_lifecycle() {
    if !should_run() {
        eprintln!("Skipped (set GITEA_INTEGRATION=1 to run)");
        return;
    }

    let repo = GiteaTestRepo::create("draft").await;
    let svc = repo.service();
    repo.create_branch("feature", "main").await;
    repo.create_file("feature.txt", "feature", "draft content").await;

    // Create as draft — GiteaService uses `WIP: ` title prefix as the
    // cross-version workaround since Gitea's API ignores `draft: true`.
    let pr = svc
        .create_pr_with_options("feature", "main", "Draft PR", Some("This is a draft"), true)
        .await
        .expect("create draft PR");

    // The "WIP: " prefix makes Gitea set draft=true in the response.
    assert!(pr.is_draft, "PR should be a draft (via WIP: prefix)");
    assert!(
        pr.title.starts_with("WIP: "),
        "draft PR title should have WIP: prefix, got: {}",
        pr.title
    );

    // Verify via details
    let details = svc.get_pr_details(pr.number).await.expect("get details");
    assert!(details.is_draft);

    // Publish (un-draft) — strips WIP: prefix and sets draft:false
    let published = svc.publish_pr(pr.number).await.expect("publish PR");
    assert!(!published.is_draft, "PR should no longer be a draft");
    assert_eq!(published.title, "Draft PR", "WIP: prefix should be stripped after publish");

    // Verify via details
    let details_after = svc.get_pr_details(pr.number).await.expect("get details after publish");
    assert!(!details_after.is_draft);
    assert_eq!(details_after.title, "Draft PR");

    repo.teardown().await;
}

/// Task 3.5: Description update — create, update title and body, verify.
#[tokio::test]
async fn test_description_update() {
    if !should_run() {
        eprintln!("Skipped (set GITEA_INTEGRATION=1 to run)");
        return;
    }

    let repo = GiteaTestRepo::create("desc").await;
    let svc = repo.service();
    repo.create_branch("desc-branch", "main").await;
    repo.create_file("desc.txt", "desc-branch", "desc content").await;

    let pr = svc
        .create_pr_with_options("desc-branch", "main", "Original Title", Some("Original body"), false)
        .await
        .expect("create PR");

    assert_eq!(pr.title, "Original Title");

    // Update description
    let updated = svc
        .update_pr_description(pr.number, "Updated Title", "Updated body text")
        .await
        .expect("update description");

    assert_eq!(updated.title, "Updated Title");

    // Verify via details
    let details = svc.get_pr_details(pr.number).await.expect("get details");
    assert_eq!(details.title, "Updated Title");
    assert_eq!(details.body.as_deref(), Some("Updated body text"));

    repo.teardown().await;
}

/// Verify the GiteaService config() returns correct platform info.
#[tokio::test]
async fn test_service_config() {
    if !should_run() {
        eprintln!("Skipped (set GITEA_INTEGRATION=1 to run)");
        return;
    }

    let host = require_env("GITEA_HOST");
    let token = require_env("GITEA_TOKEN");

    let svc = GiteaService::new(
        token,
        "testowner".to_string(),
        "testrepo".to_string(),
        Some(host.clone()),
    )
    .expect("create service");

    let config: &PlatformConfig = svc.config();
    assert_eq!(config.platform, Platform::Gitea);
    assert_eq!(config.owner, "testowner");
    assert_eq!(config.repo, "testrepo");
    assert_eq!(config.host.as_deref(), Some(host.as_str()));
}

/// 3-PR stack merge using the merge engine (create_merge_plan + execute_merge).
///
/// This exercises the same code path as `jj stack merge`:
/// - Builds MergeCandidates (bookmark + PR number pairs)
/// - Calls `create_merge_plan` to produce a plan with Merge + RetargetBase steps
/// - Calls `execute_merge` to run the plan against the live Gitea API
///   (the executor handles readiness polling just-in-time)
/// - Verifies all 3 PRs end up merged into main
#[tokio::test]
async fn test_merge_engine_3_stack() {
    if !should_run() {
        eprintln!("Skipped (set GITEA_INTEGRATION=1 to run)");
        return;
    }

    let repo = GiteaTestRepo::create("engine3").await;
    let svc = repo.service();
    repo.setup_3_stack().await;

    // Create 3 stacked PRs: branch-a → main, branch-b → branch-a, branch-c → branch-b
    let pr1 = svc
        .create_pr_with_options("branch-a", "main", "PR 1: branch-a", Some("First"), false)
        .await
        .expect("create PR 1");
    let pr2 = svc
        .create_pr_with_options("branch-b", "branch-a", "PR 2: branch-b", Some("Second"), false)
        .await
        .expect("create PR 2");
    let pr3 = svc
        .create_pr_with_options("branch-c", "branch-b", "PR 3: branch-c", Some("Third"), false)
        .await
        .expect("create PR 3");

    // Build MergeCandidates — just (bookmark, pr_number) pairs.
    // No readiness assessment — the executor handles that just-in-time.
    let candidates = vec![
        MergeCandidate { bookmark: "branch-a".to_string(), pr_number: pr1.number },
        MergeCandidate { bookmark: "branch-b".to_string(), pr_number: pr2.number },
        MergeCandidate { bookmark: "branch-c".to_string(), pr_number: pr3.number },
    ];

    // ── Create merge plan ────────────────────────────────────────────────
    let plan = create_merge_plan(&candidates, "main", MergeMethod::Squash);

    assert!(!plan.steps.is_empty(), "plan should have steps");

    // Verify the plan shape: Merge, Retarget, Merge, Retarget, Merge
    let step_kinds: Vec<&str> = plan
        .steps
        .iter()
        .map(|s| match s {
            MergeStep::Merge { .. } => "merge",
            MergeStep::RetargetBase { .. } => "retarget",
        })
        .collect();
    assert_eq!(
        step_kinds,
        vec!["merge", "retarget", "merge", "retarget", "merge"],
        "expected Merge/Retarget/Merge/Retarget/Merge, got: {step_kinds:?}"
    );

    // ── Execute merge plan ───────────────────────────────────────────────
    // The executor polls readiness just-in-time before each merge step,
    // handling async forge recomputation after retargets automatically.
    let result = execute_merge(&plan, &svc).await.expect("execute merge");

    assert_eq!(
        result.merged_bookmarks,
        vec!["branch-a", "branch-b", "branch-c"],
        "all 3 bookmarks should be merged"
    );
    assert!(
        result.failed_bookmark.is_none(),
        "no bookmark should have failed: {:?}",
        result.error_message
    );

    // ── Verify all 3 PRs are merged ─────────────────────────────────────
    for (label, pr_number) in [("PR 1", pr1.number), ("PR 2", pr2.number), ("PR 3", pr3.number)] {
        let details = svc.get_pr_details(pr_number).await.expect("get final details");
        assert_eq!(details.state, PrState::Merged, "{label} should be merged");
    }

    repo.teardown().await;
}