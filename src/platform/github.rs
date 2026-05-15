//! GitHub platform service implementation.
//!
//! Wraps the octocrab client to provide PR operations against GitHub's API.

use async_trait::async_trait;
use octocrab::models::pulls::ReviewState;
use octocrab::params::repos::Commitish;
use tokio::process::Command;

use crate::error::{ForgeCliFailure, JjPlanError, Result, flatten_error_chain};
use crate::platform::error::{
    Operation, PlatformApiError, build_platform_api_error,
};
use crate::types::{
    MergeMethod, MergeReadiness, MergeResult, Platform, PlatformConfig, PrComment, PrState,
    PullRequest, PullRequestDetails,
};

use super::PlatformService;

/// GitHub platform service backed by octocrab.
pub struct GitHubService {
    client: octocrab::Octocrab,
    config: PlatformConfig,
}

// ─── Error adapter (pure builder + thin extractor) ────────────────────────

/// Extract HTTP status, server message, and optional detail from an
/// `octocrab::Error`. Thin extractor — easy to compose with
/// `build_platform_api_error` at call sites.
///
/// For `octocrab::Error::GitHub`, pulls the `GitHubError`'s status/message/
/// field-errors. For all other variants (Hyper, Service, Json, ...), walks
/// the source chain via `flatten_error_chain` and returns `status: None`.
fn extract_github_error_fields(err: &octocrab::Error) -> (Option<u16>, String, Option<String>) {
    if let octocrab::Error::GitHub { source, .. } = err {
        let status = Some(source.status_code.as_u16());
        let message = source.message.clone();
        let detail = format_github_field_errors(source);
        (status, message, detail)
    } else {
        (None, flatten_error_chain(err), None)
    }
}

/// Render `GitHubError.errors` as a human-readable string. Appends the
/// `documentation_url` (when present) on its own line. Returns `None` if
/// neither piece is present.
fn format_github_field_errors(err: &octocrab::GitHubError) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(errors) = err.errors.as_ref().filter(|e| !e.is_empty()) {
        for e in errors {
            if let Some(obj) = e.as_object() {
                let resource = obj.get("resource").and_then(|v| v.as_str());
                let field = obj.get("field").and_then(|v| v.as_str());
                let code = obj.get("code").and_then(|v| v.as_str());
                let message = obj.get("message").and_then(|v| v.as_str());

                let key = match (resource, field) {
                    (Some(r), Some(f)) => format!("{r}.{f}"),
                    (Some(r), None) => r.to_string(),
                    (None, Some(f)) => f.to_string(),
                    (None, None) => "error".to_string(),
                };
                let body = message.or(code).unwrap_or("invalid");
                parts.push(format!("{key}: {body}"));
            } else {
                parts.push(e.to_string());
            }
        }
    }
    if let Some(url) = &err.documentation_url {
        parts.push(format!("see: {url}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Convert an `octocrab::Error` into a `PlatformApiError` tagged with the
/// given `Operation` and `target`. The full pipeline:
/// `extract_github_error_fields` → `build_platform_api_error`.
fn octocrab_err(
    operation: Operation,
    target: Option<String>,
    err: octocrab::Error,
) -> PlatformApiError {
    let (status, message, detail) = extract_github_error_fields(&err);
    build_platform_api_error(
        Platform::GitHub,
        operation,
        target,
        status,
        message,
        detail,
    )
}

// ─── Service ──────────────────────────────────────────────────────────────

impl GitHubService {
    /// Create a new GitHub service.
    ///
    /// If `host` is provided, the client targets a GitHub Enterprise instance;
    /// otherwise it targets `github.com`.
    pub fn new(
        token: &str,
        owner: String,
        repo: String,
        host: Option<String>,
    ) -> Result<Self> {
        let mut builder = octocrab::Octocrab::builder().personal_token(token.to_string());

        if let Some(ref h) = host {
            let base_url = format!("https://{h}/api/v3/");
            builder = builder.base_uri(base_url).map_err(|e| {
                JjPlanError::Config(format!("invalid GitHub Enterprise host '{h}': {e}"))
            })?;
        }

        let client = builder
            .build()
            .map_err(|e| JjPlanError::Config(format!("failed to build GitHub client: {e}")))?;

        Ok(Self {
            client,
            config: PlatformConfig {
                platform: Platform::GitHub,
                owner,
                repo,
                host,
            },
        })
    }

    fn pulls(&self) -> octocrab::pulls::PullRequestHandler<'_> {
        self.client.pulls(&self.config.owner, &self.config.repo)
    }

    /// Convert an octocrab `PullRequest` into our domain type.
    fn convert_pr(pr: &octocrab::models::pulls::PullRequest) -> PullRequest {
        PullRequest {
            number: pr.number,
            html_url: pr
                .html_url
                .as_ref()
                .map(|u| u.to_string())
                .unwrap_or_default(),
            base_ref: pr.base.ref_field.clone(),
            head_ref: pr.head.ref_field.clone(),
            title: pr.title.clone().unwrap_or_default(),
            node_id: pr.node_id.clone(),
            is_draft: pr.draft.unwrap_or(false),
        }
    }
}

impl GitHubService {
    /// Check review status for a PR.
    ///
    /// Returns `true` if the PR is approved: at least one APPROVED review
    /// and no CHANGES_REQUESTED reviews. Returns `true` if there are no
    /// reviews at all (no required reviewers).
    async fn check_reviews(&self, pr_number: u64) -> Result<bool> {
        let target = format!("#{pr_number}");
        let reviews: Vec<octocrab::models::pulls::Review> = self
            .client
            .get(
                format!(
                    "/repos/{}/{}/pulls/{}/reviews",
                    self.config.owner, self.config.repo, pr_number
                ),
                None::<&()>,
            )
            .await
            .map_err(|e| octocrab_err(Operation::CheckMergeReadiness, Some(target.clone()), e))?;

        if reviews.is_empty() {
            return Ok(true); // No reviews → no required reviewers → approved
        }

        let any_approved = reviews
            .iter()
            .any(|r| r.state == Some(ReviewState::Approved));
        let any_changes_requested = reviews
            .iter()
            .any(|r| r.state == Some(ReviewState::ChangesRequested));

        Ok(any_approved && !any_changes_requested)
    }

    /// Check CI status for a commit SHA.
    ///
    /// Returns `true` if CI passes:
    /// - No check runs exist (no CI configured), OR
    /// - All completed check runs have `conclusion == "success"`
    ///
    /// In-progress runs are not treated as failures — they produce an
    /// uncertainty note in the caller.
    async fn check_ci_status(&self, head_sha: &str) -> Result<bool> {
        let target = head_sha.to_string();
        let check_runs = self
            .client
            .checks(&self.config.owner, &self.config.repo)
            .list_check_runs_for_git_ref(Commitish(head_sha.to_string()))
            .send()
            .await
            .map_err(|e| octocrab_err(Operation::CheckMergeReadiness, Some(target), e))?;

        if check_runs.total_count == 0 {
            return Ok(true); // No CI configured
        }

        let all_completed_ok = check_runs.check_runs.iter().all(|cr| {
            cr.conclusion
                .as_deref()
                .is_none_or(|c| c == "success" || c == "skipped" || c == "neutral")
        });

        Ok(all_completed_ok)
    }
}

#[async_trait]
impl PlatformService for GitHubService {
    async fn find_existing_pr(&self, head_branch: &str) -> Result<Option<PullRequest>> {
        let owner = &self.config.owner;
        let head_filter = format!("{owner}:{head_branch}");
        let target = head_branch.to_string();

        let page = self
            .pulls()
            .list()
            .head(head_filter)
            .state(octocrab::params::State::Open)
            .send()
            .await
            .map_err(|e| octocrab_err(Operation::FindExistingPr, Some(target), e))?;

        Ok(page.items.first().map(Self::convert_pr))
    }

    async fn create_pr_with_options(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: Option<&str>,
        draft: bool,
    ) -> Result<PullRequest> {
        let target = head.to_string();
        let pulls = self.pulls();
        let mut builder = pulls.create(title, head, base);

        if let Some(b) = body {
            builder = builder.body(b);
        }
        if draft {
            builder = builder.draft(true);
        }

        let pr = builder
            .send()
            .await
            .map_err(|e| octocrab_err(Operation::CreatePr, Some(target), e))?;
        Ok(Self::convert_pr(&pr))
    }

    async fn update_pr_description(
        &self,
        pr_number: u64,
        title: &str,
        body: &str,
    ) -> Result<PullRequest> {
        let target = format!("#{pr_number}");
        let pr = self
            .pulls()
            .update(pr_number)
            .title(title)
            .body(body)
            .send()
            .await
            .map_err(|e| octocrab_err(Operation::UpdateDescription, Some(target), e))?;

        Ok(Self::convert_pr(&pr))
    }

    async fn update_pr_base(&self, pr_number: u64, new_base: &str) -> Result<PullRequest> {
        let target = format!("#{pr_number}");
        let pr = self
            .pulls()
            .update(pr_number)
            .base(new_base)
            .send()
            .await
            .map_err(|e| octocrab_err(Operation::UpdateBase, Some(target), e))?;

        Ok(Self::convert_pr(&pr))
    }

    async fn publish_pr(&self, pr_number: u64) -> Result<PullRequest> {
        // REST v3 PATCH cannot clear draft status. Use GraphQL mutation
        // `markPullRequestReadyForReview`, falling back to `gh pr ready`.

        let target = format!("#{pr_number}");

        // Fetch the PR to get its node_id for GraphQL.
        let pr = self
            .pulls()
            .get(pr_number)
            .await
            .map_err(|e| octocrab_err(Operation::PublishPr, Some(target.clone()), e))?;
        let node_id = pr.node_id.clone().unwrap_or_default();

        if !node_id.is_empty() {
            // Attempt GraphQL mutation.
            let mutation = serde_json::json!({
                "query": format!(
                    r#"mutation {{
                        markPullRequestReadyForReview(input: {{ pullRequestId: "{node_id}" }}) {{
                            pullRequest {{ isDraft }}
                        }}
                    }}"#
                )
            });

            match self.client.graphql::<serde_json::Value>(&mutation).await {
                Ok(response) => {
                    // Check the response for errors or successful un-draft.
                    let is_draft = response
                        .pointer("/data/markPullRequestReadyForReview/pullRequest/isDraft")
                        .and_then(|v| v.as_bool());

                    if is_draft == Some(false) {
                        // Success — refetch via REST to get the full PR object.
                        let refreshed = self
                            .pulls()
                            .get(pr_number)
                            .await
                            .map_err(|e| {
                                octocrab_err(Operation::PublishPr, Some(target.clone()), e)
                            })?;
                        return Ok(Self::convert_pr(&refreshed));
                    }

                    // GraphQL returned but mutation didn't clear draft — check for errors.
                    if let Some(errors) = response.get("errors") {
                        eprintln!(
                            "GraphQL markPullRequestReadyForReview returned errors: {errors}"
                        );
                    }
                    // Fall through to gh CLI fallback.
                }
                Err(e) => {
                    eprintln!("GraphQL publish_pr failed ({e}), falling back to `gh pr ready`");
                    // Fall through to gh CLI fallback.
                }
            }
        }

        // Fallback: shell out to `gh pr ready`.
        let repo_slug = format!("{}/{}", self.config.owner, self.config.repo);
        let command = format!("pr ready {pr_number} --repo {repo_slug}");
        let output = Command::new("gh")
            .args(["pr", "ready", &pr_number.to_string(), "--repo", &repo_slug])
            .output()
            .await
            .map_err(|io_err| JjPlanError::ForgeCli {
                tool: "gh",
                command: command.clone(),
                kind: ForgeCliFailure::NotInstalled(io_err),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(JjPlanError::ForgeCli {
                tool: "gh",
                command,
                kind: ForgeCliFailure::Failed {
                    exit_code: output.status.code(),
                    stderr,
                },
            });
        }

        // Refetch the PR to return updated state.
        let refreshed = self
            .pulls()
            .get(pr_number)
            .await
            .map_err(|e| octocrab_err(Operation::PublishPr, Some(target), e))?;
        Ok(Self::convert_pr(&refreshed))
    }

    async fn list_pr_comments(&self, pr_number: u64) -> Result<Vec<PrComment>> {
        let target = format!("#{pr_number}");
        let comments = self
            .client
            .issues(&self.config.owner, &self.config.repo)
            .list_comments(pr_number)
            .send()
            .await
            .map_err(|e| octocrab_err(Operation::ListComments, Some(target), e))?;

        Ok(comments
            .items
            .into_iter()
            .map(|c| PrComment {
                id: c.id.into_inner(),
                body: c.body.unwrap_or_default(),
            })
            .collect())
    }

    async fn create_pr_comment(&self, pr_number: u64, body: &str) -> Result<()> {
        let target = format!("#{pr_number}");
        self.client
            .issues(&self.config.owner, &self.config.repo)
            .create_comment(pr_number, body)
            .await
            .map_err(|e| octocrab_err(Operation::CreateComment, Some(target), e))?;
        Ok(())
    }

    async fn update_pr_comment(
        &self,
        pr_number: u64,
        comment_id: u64,
        body: &str,
    ) -> Result<()> {
        let target = format!("#{pr_number}");
        // Issue comments are repo-scoped, so pr_number is unused in the URL —
        // we still tag it as the target for hint context.
        let comment_id = octocrab::models::CommentId(comment_id);
        self.client
            .issues(&self.config.owner, &self.config.repo)
            .update_comment(comment_id, body)
            .await
            .map_err(|e| octocrab_err(Operation::UpdateComment, Some(target), e))?;
        Ok(())
    }

    fn config(&self) -> &PlatformConfig {
        &self.config
    }

    async fn get_pr_details(&self, pr_number: u64) -> Result<PullRequestDetails> {
        let target = format!("#{pr_number}");
        let pr = self
            .pulls()
            .get(pr_number)
            .await
            .map_err(|e| octocrab_err(Operation::GetPrDetails, Some(target), e))?;

        let state = match &pr.state {
            Some(octocrab::models::IssueState::Open) => PrState::Open,
            Some(octocrab::models::IssueState::Closed) => {
                if pr.merged_at.is_some() {
                    PrState::Merged
                } else {
                    PrState::Closed
                }
            }
            _ => PrState::Open,
        };

        Ok(PullRequestDetails {
            number: pr.number,
            title: pr.title.clone().unwrap_or_default(),
            body: pr.body.clone(),
            state,
            is_draft: pr.draft.unwrap_or(false),
            mergeable: pr.mergeable,
            head_ref: pr.head.ref_field.clone(),
            head_sha: Some(pr.head.sha.clone()),
            base_ref: pr.base.ref_field.clone(),
            html_url: pr
                .html_url
                .as_ref()
                .map(|u| u.to_string())
                .unwrap_or_default(),
        })
    }

    async fn check_merge_readiness(&self, pr_number: u64) -> Result<MergeReadiness> {
        let target = format!("#{pr_number}");
        let pr = self
            .pulls()
            .get(pr_number)
            .await
            .map_err(|e| octocrab_err(Operation::CheckMergeReadiness, Some(target), e))?;

        let details = PullRequestDetails {
            number: pr.number,
            title: pr.title.clone().unwrap_or_default(),
            body: pr.body.clone(),
            state: PrState::Open,
            is_draft: pr.draft.unwrap_or(false),
            mergeable: pr.mergeable,
            head_ref: pr.head.ref_field.clone(),
            head_sha: Some(pr.head.sha.clone()),
            base_ref: pr.base.ref_field.clone(),
            html_url: pr
                .html_url
                .as_ref()
                .map(|u| u.to_string())
                .unwrap_or_default(),
        };

        let head_sha = pr.head.sha.clone();
        let mut blocking_reasons = Vec::new();
        let mut uncertainties = Vec::new();

        if details.is_draft {
            blocking_reasons.push("PR is still in draft".to_string());
        }

        if details.mergeable == Some(false) {
            blocking_reasons.push("PR is not mergeable (conflicts or policy)".to_string());
        } else if details.mergeable.is_none() {
            uncertainties.push("mergeable status is unknown".to_string());
        }

        // ── Check reviews ────────────────────────────────────────────────
        // List reviews via the REST API. A PR is approved if any review has
        // state APPROVED and no review has state CHANGES_REQUESTED.
        let is_approved = match self.check_reviews(pr_number).await {
            Ok(approved) => approved,
            Err(e) => {
                uncertainties.push(format!("could not check reviews: {e}"));
                true // Permissive fallback — don't block on review API failure
            }
        };

        // ── Check CI status ──────────────────────────────────────────────
        // Query check runs for the head SHA. CI passes if there are no
        // check runs (no CI configured) or all completed runs succeeded.
        let ci_passed = match self.check_ci_status(&head_sha).await {
            Ok(passed) => passed,
            Err(e) => {
                uncertainties.push(format!("could not check CI status: {e}"));
                true // Permissive fallback
            }
        };

        if !is_approved {
            blocking_reasons.push("Not approved (or changes requested)".to_string());
        }
        if !ci_passed {
            blocking_reasons.push("CI checks not passing".to_string());
        }

        Ok(MergeReadiness {
            is_approved,
            ci_passed,
            is_mergeable: details.mergeable,
            is_draft: details.is_draft,
            blocking_reasons,
            uncertainties,
        })
    }

    async fn merge_pr(&self, pr_number: u64, method: MergeMethod) -> Result<MergeResult> {
        let target = format!("#{pr_number}");
        let merge_method = match method {
            MergeMethod::Squash => octocrab::params::pulls::MergeMethod::Squash,
            MergeMethod::Merge => octocrab::params::pulls::MergeMethod::Merge,
            MergeMethod::Rebase => octocrab::params::pulls::MergeMethod::Rebase,
        };

        let result = self
            .pulls()
            .merge(pr_number)
            .method(merge_method)
            .send()
            .await
            .map_err(|e| octocrab_err(Operation::MergePr, Some(target), e))?;

        Ok(MergeResult {
            merged: result.merged,
            sha: result.sha,
            message: result.message,
        })
    }
}

