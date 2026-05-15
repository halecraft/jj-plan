//! GitLab platform service implementation.
//!
//! Provides MR operations via the GitLab v4 REST API.

use crate::error::{JjPlanError, Result};
use crate::platform::PlatformService;
use crate::platform::error::{Operation, checked_response, checked_status};
use crate::types::{
    MergeMethod, MergeReadiness, MergeResult, Platform, PlatformConfig, PrComment, PrState,
    PullRequest, PullRequestDetails,
};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// GitLab service using reqwest.
pub struct GitLabService {
    client: Client,
    token: String,
    host: String,
    #[allow(dead_code)] // Read through the `config()` trait method, which is itself #[allow(dead_code)].
    config: PlatformConfig,
    project_path: String,
}

#[derive(Deserialize)]
struct MergeRequest {
    iid: u64,
    web_url: String,
    source_branch: String,
    target_branch: String,
    title: String,
    #[serde(default)]
    draft: bool,
}

#[derive(Deserialize)]
struct MrNote {
    id: u64,
    body: String,
    system: bool,
}

/// Extended MR details for merge operations.
#[derive(Deserialize)]
struct MergeRequestDetails {
    iid: u64,
    title: String,
    description: Option<String>,
    state: String, // "opened", "closed", "merged"
    #[serde(default)]
    draft: bool,
    merge_status: String, // "can_be_merged", "cannot_be_merged", etc.
    web_url: String,
    source_branch: String,
    target_branch: String,
}

/// MR approvals response.
#[derive(Deserialize)]
struct MrApprovals {
    approved: bool,
}

/// Pipeline status.
#[derive(Deserialize)]
struct Pipeline {
    status: String, // "success", "failed", "running", "pending"
}

/// Merge response.
#[derive(Deserialize)]
struct MergeResponse {
    state: String,
    merge_commit_sha: Option<String>,
}

impl From<MergeRequest> for PullRequest {
    fn from(mr: MergeRequest) -> Self {
        Self {
            number: mr.iid,
            html_url: mr.web_url,
            base_ref: mr.target_branch,
            head_ref: mr.source_branch,
            title: mr.title,
            node_id: None, // GitLab doesn't use GraphQL node IDs
            is_draft: mr.draft,
        }
    }
}

#[derive(Serialize)]
struct CreateMrPayload {
    source_branch: String,
    target_branch: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    draft: Option<bool>,
}

/// Default request timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

impl GitLabService {
    /// Create a new GitLab service.
    pub fn new(token: String, owner: String, repo: String, host: Option<String>) -> Result<Self> {
        let host = host.unwrap_or_else(|| "gitlab.com".to_string());
        let project_path = format!("{owner}/{repo}");

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .map_err(|e| {
                JjPlanError::Config(format!("failed to build GitLab HTTP client: {e}"))
            })?;

        let config_host = if host == "gitlab.com" {
            None
        } else {
            Some(host.clone())
        };

        Ok(Self {
            client,
            token,
            host,
            config: PlatformConfig {
                platform: Platform::GitLab,
                owner,
                repo,
                host: config_host,
            },
            project_path,
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!("https://{}/api/v4{}", self.host, path)
    }

    fn encoded_project(&self) -> String {
        urlencoding::encode(&self.project_path).into_owned()
    }

    // ── authed_* helpers ───────────────────────────────────────────────
    // Apply the GitLab PRIVATE-TOKEN header at a single point so callers
    // can't accidentally make an unauthenticated request.

    fn authed_request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.client.request(method, url).header("PRIVATE-TOKEN", &self.token)
    }
    fn authed_get(&self, url: &str) -> reqwest::RequestBuilder {
        self.authed_request(reqwest::Method::GET, url)
    }
    fn authed_post(&self, url: &str) -> reqwest::RequestBuilder {
        self.authed_request(reqwest::Method::POST, url)
    }
    fn authed_put(&self, url: &str) -> reqwest::RequestBuilder {
        self.authed_request(reqwest::Method::PUT, url)
    }
}

#[async_trait]
impl PlatformService for GitLabService {
    async fn find_existing_pr(&self, head_branch: &str) -> Result<Option<PullRequest>> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests",
            self.encoded_project()
        ));

        let response = self
            .authed_get(&url)
            .query(&[("source_branch", head_branch), ("state", "opened")])
            .send()
            .await?;

        let mrs: Vec<MergeRequest> = checked_response(
            response,
            Platform::GitLab,
            Operation::FindExistingPr,
            Some(head_branch.to_string()),
        )
        .await?;

        Ok(mrs.into_iter().next().map(Into::into))
    }

    async fn create_pr_with_options(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: Option<&str>,
        draft: bool,
    ) -> Result<PullRequest> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests",
            self.encoded_project()
        ));

        let payload = CreateMrPayload {
            source_branch: head.to_string(),
            target_branch: base.to_string(),
            title: title.to_string(),
            description: body.map(ToString::to_string),
            draft: if draft { Some(true) } else { None },
        };

        let response = self.authed_post(&url).json(&payload).send().await?;
        let mr: MergeRequest = checked_response(
            response,
            Platform::GitLab,
            Operation::CreatePr,
            Some(head.to_string()),
        )
        .await?;

        Ok(mr.into())
    }

    async fn update_pr_base(&self, pr_number: u64, new_base: &str) -> Result<PullRequest> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}",
            self.encoded_project(),
            pr_number
        ));
        let target = format!("#{pr_number}");

        let response = self
            .authed_put(&url)
            .json(&serde_json::json!({ "target_branch": new_base }))
            .send()
            .await?;
        let mr: MergeRequest = checked_response(
            response,
            Platform::GitLab,
            Operation::UpdateBase,
            Some(target),
        )
        .await?;

        Ok(mr.into())
    }

    async fn update_pr_description(
        &self,
        pr_number: u64,
        title: &str,
        body: &str,
    ) -> Result<PullRequest> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}",
            self.encoded_project(),
            pr_number
        ));
        let target = format!("#{pr_number}");

        let response = self
            .authed_put(&url)
            .json(&serde_json::json!({ "title": title, "description": body }))
            .send()
            .await?;
        let mr: MergeRequest = checked_response(
            response,
            Platform::GitLab,
            Operation::UpdateDescription,
            Some(target),
        )
        .await?;

        Ok(mr.into())
    }

    async fn publish_pr(&self, pr_number: u64) -> Result<PullRequest> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}",
            self.encoded_project(),
            pr_number
        ));
        let target = format!("#{pr_number}");

        // Remove draft status. GitLab's state_event only accepts "close"/"reopen";
        // the correct way to un-draft is { "draft": false } (since GitLab 15.0).
        let response = self
            .authed_put(&url)
            .json(&serde_json::json!({ "draft": false }))
            .send()
            .await?;
        let mr: MergeRequest = checked_response(
            response,
            Platform::GitLab,
            Operation::PublishPr,
            Some(target),
        )
        .await?;

        Ok(mr.into())
    }

    async fn list_pr_comments(&self, pr_number: u64) -> Result<Vec<PrComment>> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}/notes",
            self.encoded_project(),
            pr_number
        ));
        let target = format!("#{pr_number}");

        let response = self.authed_get(&url).send().await?;
        let notes: Vec<MrNote> = checked_response(
            response,
            Platform::GitLab,
            Operation::ListComments,
            Some(target),
        )
        .await?;

        let comments: Vec<PrComment> = notes
            .into_iter()
            .filter(|n| !n.system)
            .map(|n| PrComment {
                id: n.id,
                body: n.body,
            })
            .collect();

        Ok(comments)
    }

    async fn create_pr_comment(&self, pr_number: u64, body: &str) -> Result<()> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}/notes",
            self.encoded_project(),
            pr_number
        ));
        let target = format!("#{pr_number}");

        let response = self
            .authed_post(&url)
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await?;
        checked_status(
            response,
            Platform::GitLab,
            Operation::CreateComment,
            Some(target),
        )
        .await
    }

    async fn update_pr_comment(&self, pr_number: u64, comment_id: u64, body: &str) -> Result<()> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}/notes/{}",
            self.encoded_project(),
            pr_number,
            comment_id
        ));
        let target = format!("#{pr_number}");

        let response = self
            .authed_put(&url)
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await?;
        checked_status(
            response,
            Platform::GitLab,
            Operation::UpdateComment,
            Some(target),
        )
        .await
    }

    fn config(&self) -> &PlatformConfig {
        &self.config
    }

    // =========================================================================
    // Merge-related methods
    // =========================================================================

    async fn get_pr_details(&self, pr_number: u64) -> Result<PullRequestDetails> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}",
            self.encoded_project(),
            pr_number
        ));
        let target = format!("#{pr_number}");

        let response = self.authed_get(&url).send().await?;
        let mr: MergeRequestDetails = checked_response(
            response,
            Platform::GitLab,
            Operation::GetPrDetails,
            Some(target),
        )
        .await?;

        let state = match mr.state.as_str() {
            "opened" => PrState::Open,
            "merged" => PrState::Merged,
            _ => PrState::Closed,
        };

        Ok(PullRequestDetails {
            number: mr.iid,
            title: mr.title,
            body: mr.description,
            state,
            is_draft: mr.draft,
            mergeable: Some(mr.merge_status == "can_be_merged"),
            head_ref: mr.source_branch,
            head_sha: None, // GitLab uses pipeline API for CI, not head SHA
            base_ref: mr.target_branch,
            html_url: mr.web_url,
        })
    }

    async fn check_merge_readiness(&self, pr_number: u64) -> Result<MergeReadiness> {
        // Get MR details first.
        let details = self.get_pr_details(pr_number).await?;
        let target = format!("#{pr_number}");
        let mut uncertainties: Vec<String> = Vec::new();

        // ── Approvals (secondary; record-and-continue on failure) ─────
        let approvals_url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}/approvals",
            self.encoded_project(),
            pr_number
        ));
        let is_approved =
            match try_checked_response::<MrApprovals>(self.authed_get(&approvals_url), Operation::CheckMergeReadiness, Some(target.clone())).await {
                Ok(a) => a.approved,
                Err(e) => {
                    uncertainties.push(format!("could not fetch approvals: {e}"));
                    false
                }
            };

        // ── Pipelines (secondary; record-and-continue on failure) ─────
        let pipelines_url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}/pipelines",
            self.encoded_project(),
            pr_number
        ));
        let ci_passed =
            match try_checked_response::<Vec<Pipeline>>(self.authed_get(&pipelines_url), Operation::CheckMergeReadiness, Some(target.clone())).await {
                Ok(pipelines) => pipelines.first().is_none_or(|p| p.status == "success"),
                Err(e) => {
                    uncertainties.push(format!("could not fetch pipelines: {e}"));
                    true // Permissive — don't block on a pipelines fetch failure
                }
            };

        // Build blocking reasons
        let mut blocking_reasons = Vec::new();
        if details.is_draft {
            blocking_reasons.push("MR is a draft".to_string());
        }
        if !is_approved {
            blocking_reasons.push("Not approved".to_string());
        }
        if !ci_passed {
            blocking_reasons.push("CI not passing".to_string());
        }
        if details.mergeable == Some(false) {
            blocking_reasons.push("Has merge conflicts".to_string());
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
        // Get MR details for commit message
        let details = self.get_pr_details(pr_number).await?;

        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}/merge",
            self.encoded_project(),
            pr_number
        ));
        let target = format!("#{pr_number}");

        let body = match method {
            MergeMethod::Squash => serde_json::json!({
                "squash": true,
                "squash_commit_message": format!(
                    "{} (!{})\n\n{}",
                    details.title,
                    pr_number,
                    details.body.unwrap_or_default()
                )
            }),
            MergeMethod::Merge => serde_json::json!({}),
            MergeMethod::Rebase => serde_json::json!({
                "merge_method": "rebase"
            }),
        };

        let response = self.authed_put(&url).json(&body).send().await?;
        let merge_response: MergeResponse = checked_response(
            response,
            Platform::GitLab,
            Operation::MergePr,
            Some(target),
        )
        .await?;

        Ok(MergeResult {
            merged: merge_response.state == "merged",
            sha: merge_response.merge_commit_sha,
            message: None,
        })
    }
}

/// Helper: send a request and run `checked_response`, capturing both pre-send
/// `reqwest::Error`s and classified `PlatformApiError`s into a single `Result`.
/// Used by `check_merge_readiness` to gather secondary-endpoint failures into
/// `uncertainties` rather than aborting the whole assessment.
async fn try_checked_response<T: serde::de::DeserializeOwned>(
    rb: reqwest::RequestBuilder,
    operation: Operation,
    target: Option<String>,
) -> Result<T> {
    let response = rb.send().await?;
    checked_response(response, Platform::GitLab, operation, target).await
}
