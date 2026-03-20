//! GitLab platform service implementation.
//!
//! Provides MR operations via the GitLab v4 REST API.

use crate::error::{JjPlanError, Result};
use crate::platform::PlatformService;
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
            .map_err(|e| JjPlanError::GitLabApi(format!("failed to create HTTP client: {e}")))?;

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
}

#[async_trait]
impl PlatformService for GitLabService {
    async fn find_existing_pr(&self, head_branch: &str) -> Result<Option<PullRequest>> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests",
            self.encoded_project()
        ));

        let mrs: Vec<MergeRequest> = self
            .client
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .query(&[("source_branch", head_branch), ("state", "opened")])
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GitLabApi(e.to_string()))?
            .json()
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

        let mr: MergeRequest = self
            .client
            .post(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .json(&payload)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GitLabApi(e.to_string()))?
            .json()
            .await?;

        Ok(mr.into())
    }

    async fn update_pr_base(&self, pr_number: u64, new_base: &str) -> Result<PullRequest> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}",
            self.encoded_project(),
            pr_number
        ));

        let mr: MergeRequest = self
            .client
            .put(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .json(&serde_json::json!({ "target_branch": new_base }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GitLabApi(e.to_string()))?
            .json()
            .await?;

        Ok(mr.into())
    }

    async fn publish_pr(&self, pr_number: u64) -> Result<PullRequest> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}",
            self.encoded_project(),
            pr_number
        ));

        // GitLab uses state_event: "ready" to mark as ready for review
        let mr: MergeRequest = self
            .client
            .put(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .json(&serde_json::json!({ "state_event": "ready" }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GitLabApi(e.to_string()))?
            .json()
            .await?;

        Ok(mr.into())
    }

    async fn list_pr_comments(&self, pr_number: u64) -> Result<Vec<PrComment>> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}/notes",
            self.encoded_project(),
            pr_number
        ));

        let notes: Vec<MrNote> = self
            .client
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GitLabApi(e.to_string()))?
            .json()
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

        self.client
            .post(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GitLabApi(e.to_string()))?;

        Ok(())
    }

    async fn update_pr_comment(&self, pr_number: u64, comment_id: u64, body: &str) -> Result<()> {
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}/notes/{}",
            self.encoded_project(),
            pr_number,
            comment_id
        ));

        self.client
            .put(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GitLabApi(e.to_string()))?;

        Ok(())
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

        let mr: MergeRequestDetails = self
            .client
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GitLabApi(e.to_string()))?
            .json()
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
            base_ref: mr.target_branch,
            html_url: mr.web_url,
        })
    }

    async fn check_merge_readiness(&self, pr_number: u64) -> Result<MergeReadiness> {
        // Get MR details first
        let details = self.get_pr_details(pr_number).await?;

        // Check approvals
        let approvals_url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}/approvals",
            self.encoded_project(),
            pr_number
        ));

        let is_approved = match self
            .client
            .get(&approvals_url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await
        {
            Ok(response) => {
                if response.status().is_success() {
                    let approvals: MrApprovals =
                        response.json().await.unwrap_or(MrApprovals { approved: false });
                    approvals.approved
                } else {
                    false
                }
            }
            Err(_) => false,
        };

        // Check pipelines (most recent)
        let pipelines_url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}/pipelines",
            self.encoded_project(),
            pr_number
        ));

        let ci_passed = match self
            .client
            .get(&pipelines_url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await
        {
            Ok(response) => {
                if response.status().is_success() {
                    let pipelines: Vec<Pipeline> = response.json().await.unwrap_or_default();
                    // No pipeline = not blocking, otherwise check most recent
                    pipelines.first().is_none_or(|p| p.status == "success")
                } else {
                    true
                }
            }
            Err(_) => true,
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

        // GitLab always computes merge_status synchronously, so no uncertainties
        Ok(MergeReadiness {
            is_approved,
            ci_passed,
            is_mergeable: details.mergeable,
            is_draft: details.is_draft,
            blocking_reasons,
            uncertainties: vec![],
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

        let response: MergeResponse = self
            .client
            .put(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .json(&body)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| JjPlanError::GitLabApi(format!("Merge failed: {e}")))?
            .json()
            .await?;

        Ok(MergeResult {
            merged: response.state == "merged",
            sha: response.merge_commit_sha,
            message: None,
        })
    }
}