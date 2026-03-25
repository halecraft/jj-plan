//! Gitea platform service implementation.
//!
//! Provides PR operations via the Gitea v1 REST API.

use std::env;

use crate::error::{JjPlanError, Result};
use crate::platform::PlatformService;
use crate::types::{
    MergeMethod, MergeReadiness, MergeResult, Platform, PlatformConfig, PrComment, PrState,
    PullRequest, PullRequestDetails,
};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

// ─── Internal deserialization types ──────────────────────────────────────────

#[derive(Deserialize)]
struct GiteaBranchRef {
    label: String,
}

#[derive(Deserialize)]
struct GiteaPullRequest {
    number: u64,
    html_url: String,
    title: String,
    #[serde(default)]
    body: Option<String>,
    state: String, // "open", "closed"
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    mergeable: bool,
    #[serde(default)]
    draft: bool,
    merge_commit_sha: Option<String>,
    base: GiteaBranchRef,
    head: GiteaBranchRef,
}

#[derive(Deserialize)]
struct GiteaComment {
    id: u64,
    body: String,
}

#[derive(Deserialize)]
struct GiteaReview {
    state: String, // "APPROVED", "REQUEST_CHANGES", "COMMENT", "PENDING", etc.
}

// ─── Conversions ─────────────────────────────────────────────────────────────

impl From<&GiteaPullRequest> for PullRequest {
    fn from(pr: &GiteaPullRequest) -> Self {
        Self {
            number: pr.number,
            html_url: pr.html_url.clone(),
            base_ref: pr.base.label.clone(),
            head_ref: pr.head.label.clone(),
            title: pr.title.clone(),
            node_id: None, // Gitea doesn't use GraphQL node IDs
            is_draft: pr.draft,
        }
    }
}

impl From<GiteaPullRequest> for PullRequest {
    fn from(pr: GiteaPullRequest) -> Self {
        PullRequest::from(&pr)
    }
}

// ─── Service ─────────────────────────────────────────────────────────────────

/// Default request timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Default draft title prefix.
const DEFAULT_DRAFT_PREFIX: &str = "WIP: ";

/// Gitea service using reqwest.
pub struct GiteaService {
    client: Client,
    token: String,
    host: String,
    #[allow(dead_code)] // Read through the `config()` trait method, which is itself #[allow(dead_code)].
    config: PlatformConfig,
    /// Title prefix used to mark PRs as drafts. Gitea recognises `WIP:` and
    /// several other prefixes as draft markers. Configurable via the
    /// `GITEA_DRAFT_PREFIX` environment variable; defaults to `"Draft: "`.
    draft_prefix: String,
}

impl GiteaService {
    /// Create a new Gitea service.
    pub fn new(token: String, owner: String, repo: String, host: Option<String>) -> Result<Self> {
        let host = host.unwrap_or_else(|| "codeberg.org".to_string());

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .map_err(|e| JjPlanError::GiteaApi(format!("failed to create HTTP client: {e}")))?;

        let config_host = if host == "codeberg.org" {
            None
        } else {
            Some(host.clone())
        };

        let draft_prefix = env::var("GITEA_DRAFT_PREFIX")
            .unwrap_or_else(|_| DEFAULT_DRAFT_PREFIX.to_string());

        Ok(Self {
            client,
            token,
            host,
            config: PlatformConfig {
                platform: Platform::Gitea,
                owner,
                repo,
                host: config_host,
            },
            draft_prefix,
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!("https://{}/api/v1{}", self.host, path)
    }

    fn repo_path(&self) -> String {
        format!(
            "/repos/{}/{}",
            self.config.owner, self.config.repo
        )
    }

    /// Fetch a single PR by index.
    async fn get_pr(&self, pr_number: u64) -> Result<GiteaPullRequest> {
        let url = self.api_url(&format!("{}/pulls/{}", self.repo_path(), pr_number));

        let pr: GiteaPullRequest = self
            .client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GiteaApi(e.to_string()))?
            .json()
            .await?;

        Ok(pr)
    }
}

#[async_trait]
impl PlatformService for GiteaService {
    async fn find_existing_pr(&self, head_branch: &str) -> Result<Option<PullRequest>> {
        let url = self.api_url(&format!("{}/pulls", self.repo_path()));

        // Gitea's `head` query parameter is a loose filter — it matches PRs
        // where the head OR base branch contains the value. We must fetch
        // candidates and then client-side filter to the exact head branch.
        let prs: Vec<GiteaPullRequest> = self
            .client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .query(&[("state", "open"), ("head", head_branch)])
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GiteaApi(e.to_string()))?
            .json()
            .await?;

        Ok(prs
            .into_iter()
            .find(|pr| pr.head.label == head_branch)
            .map(Into::into))
    }

    async fn create_pr_with_options(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: Option<&str>,
        draft: bool,
    ) -> Result<PullRequest> {
        let url = self.api_url(&format!("{}/pulls", self.repo_path()));

        // Gitea versions before ~1.22 silently ignore `draft: true` on both
        // creation and PATCH. The reliable cross-version workaround is a
        // recognised title prefix (e.g. "Draft: ", "WIP: ") — Gitea sees it
        // and sets `draft = true` in the response.
        let effective_title = if draft {
            format!("{}{title}", self.draft_prefix)
        } else {
            title.to_string()
        };

        let mut payload = serde_json::json!({
            "head": head,
            "base": base,
            "title": effective_title,
        });

        if let Some(body_text) = body {
            payload["body"] = serde_json::Value::String(body_text.to_string());
        }

        let pr: GiteaPullRequest = self
            .client
            .post(&url)
            .header("Authorization", format!("token {}", self.token))
            .json(&payload)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GiteaApi(e.to_string()))?
            .json()
            .await?;

        Ok(pr.into())
    }

    async fn update_pr_base(&self, pr_number: u64, new_base: &str) -> Result<PullRequest> {
        let url = self.api_url(&format!("{}/pulls/{}", self.repo_path(), pr_number));

        let pr: GiteaPullRequest = self
            .client
            .patch(&url)
            .header("Authorization", format!("token {}", self.token))
            .json(&serde_json::json!({ "base": new_base }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GiteaApi(e.to_string()))?
            .json()
            .await?;

        Ok(pr.into())
    }

    async fn update_pr_description(
        &self,
        pr_number: u64,
        title: &str,
        body: &str,
    ) -> Result<PullRequest> {
        let url = self.api_url(&format!("{}/pulls/{}", self.repo_path(), pr_number));

        let pr: GiteaPullRequest = self
            .client
            .patch(&url)
            .header("Authorization", format!("token {}", self.token))
            .json(&serde_json::json!({ "title": title, "body": body }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GiteaApi(e.to_string()))?
            .json()
            .await?;

        Ok(pr.into())
    }

    async fn publish_pr(&self, pr_number: u64) -> Result<PullRequest> {
        let url = self.api_url(&format!("{}/pulls/{}", self.repo_path(), pr_number));

        // Fetch current PR to get its title.
        let current = self.get_pr(pr_number).await?;

        // Strip the draft prefix that we applied on creation, and also
        // try `draft: false` in the PATCH body for newer Gitea versions.
        let prefix = &self.draft_prefix;
        let new_title = current
            .title
            .strip_prefix(prefix)
            .or_else(|| current.title.strip_prefix(prefix.trim_end()))
            .unwrap_or(&current.title)
            .to_string();

        let pr: GiteaPullRequest = self
            .client
            .patch(&url)
            .header("Authorization", format!("token {}", self.token))
            .json(&serde_json::json!({ "title": new_title, "draft": false }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GiteaApi(e.to_string()))?
            .json()
            .await?;

        Ok(pr.into())
    }

    async fn list_pr_comments(&self, pr_number: u64) -> Result<Vec<PrComment>> {
        // Gitea uses the issues endpoint for comments on PRs.
        let url = self.api_url(&format!(
            "{}/issues/{}/comments",
            self.repo_path(),
            pr_number
        ));

        let comments: Vec<GiteaComment> = self
            .client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GiteaApi(e.to_string()))?
            .json()
            .await?;

        Ok(comments
            .into_iter()
            .map(|c| PrComment {
                id: c.id,
                body: c.body,
            })
            .collect())
    }

    async fn create_pr_comment(&self, pr_number: u64, body: &str) -> Result<()> {
        let url = self.api_url(&format!(
            "{}/issues/{}/comments",
            self.repo_path(),
            pr_number
        ));

        self.client
            .post(&url)
            .header("Authorization", format!("token {}", self.token))
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GiteaApi(e.to_string()))?;

        Ok(())
    }

    async fn update_pr_comment(&self, _pr_number: u64, comment_id: u64, body: &str) -> Result<()> {
        // Gitea uses /repos/{owner}/{repo}/issues/comments/{id} for updating —
        // the comment ID is globally unique, so pr_number is not needed in the URL.
        let url = self.api_url(&format!(
            "{}/issues/comments/{}",
            self.repo_path(),
            comment_id
        ));

        self.client
            .patch(&url)
            .header("Authorization", format!("token {}", self.token))
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GiteaApi(e.to_string()))?;

        Ok(())
    }

    fn config(&self) -> &PlatformConfig {
        &self.config
    }

    // =========================================================================
    // Merge-related methods
    // =========================================================================

    async fn get_pr_details(&self, pr_number: u64) -> Result<PullRequestDetails> {
        let pr = self.get_pr(pr_number).await?;

        let state = if pr.merged {
            PrState::Merged
        } else {
            match pr.state.as_str() {
                "open" => PrState::Open,
                _ => PrState::Closed,
            }
        };

        Ok(PullRequestDetails {
            number: pr.number,
            title: pr.title,
            body: pr.body,
            state,
            is_draft: pr.draft,
            mergeable: Some(pr.mergeable),
            head_ref: pr.head.label,
            head_sha: None, // Gitea uses review API for readiness, not head SHA
            base_ref: pr.base.label,
            html_url: pr.html_url,
        })
    }

    async fn check_merge_readiness(&self, pr_number: u64) -> Result<MergeReadiness> {
        // Single-shot observation — no polling. The merge executor handles
        // transient states (forge still recomputing mergeable status after
        // graph-changing events) generically via `poll_until_ready`.
        let details = self.get_pr_details(pr_number).await?;

        // Fetch reviews to check approval status.
        let reviews_url = self.api_url(&format!(
            "{}/pulls/{}/reviews",
            self.repo_path(),
            pr_number
        ));

        let (is_approved, has_changes_requested) = match self
            .client
            .get(&reviews_url)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
        {
            Ok(response) => {
                if response.status().is_success() {
                    let reviews: Vec<GiteaReview> =
                        response.json().await.unwrap_or_default();

                    if reviews.is_empty() {
                        // No reviews at all — self-hosted Gitea typically has no
                        // required reviews, so treat as approved.
                        (true, false)
                    } else {
                        let any_approved = reviews.iter().any(|r| r.state == "APPROVED");
                        let any_changes_requested =
                            reviews.iter().any(|r| r.state == "REQUEST_CHANGES");
                        let approved = any_approved && !any_changes_requested;
                        (approved, any_changes_requested)
                    }
                } else {
                    // Cannot fetch reviews — default to approved (permissive).
                    (true, false)
                }
            }
            Err(_) => (true, false),
        };

        // Gitea Actions status is not easily available per-PR, so we
        // optimistically assume CI passed.
        let ci_passed = true;

        // Build blocking reasons.
        let mut blocking_reasons = Vec::new();
        if details.is_draft {
            blocking_reasons.push("PR is a draft".to_string());
        }
        if !is_approved {
            if has_changes_requested {
                blocking_reasons.push("Changes requested".to_string());
            } else {
                blocking_reasons.push("Not approved".to_string());
            }
        }
        if details.mergeable == Some(false) {
            blocking_reasons.push("Has merge conflicts".to_string());
        }

        // Gitea doesn't expose CI status per-PR easily — note the uncertainty.
        let uncertainties = vec!["CI status not checked (Gitea Actions status not available per-PR)".to_string()];

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
        let url = self.api_url(&format!(
            "{}/pulls/{}/merge",
            self.repo_path(),
            pr_number
        ));

        let do_method = match method {
            MergeMethod::Squash => "squash",
            MergeMethod::Merge => "merge",
            MergeMethod::Rebase => "rebase",
        };

        // Gitea merge endpoint uses uppercase "Do" key.
        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("token {}", self.token))
            .json(&serde_json::json!({ "Do": do_method }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GiteaApi(format!("Merge failed: {e}")))?;

        // Gitea returns an empty body on successful merge (HTTP 200).
        // We must GET the PR afterwards to confirm merged status and get the SHA.
        drop(response);

        let pr = self.get_pr(pr_number).await?;

        Ok(MergeResult {
            merged: pr.merged,
            sha: pr.merge_commit_sha,
            message: None,
        })
    }
}