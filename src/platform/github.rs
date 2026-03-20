//! GitHub platform service implementation.
//!
//! Wraps the octocrab client to provide PR operations against GitHub's API.

use async_trait::async_trait;

use crate::error::{JjPlanError, Result};
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
            let base_url = format!("https://{}/api/v3/", h);
            builder = builder.base_uri(base_url).map_err(|e| {
                JjPlanError::GitHubApi(format!("invalid GitHub Enterprise host: {e}"))
            })?;
        }

        let client = builder
            .build()
            .map_err(|e| JjPlanError::GitHubApi(format!("failed to build GitHub client: {e}")))?;

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
        self.client
            .pulls(&self.config.owner, &self.config.repo)
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
            base_ref: pr.base.label.clone().unwrap_or_default(),
            head_ref: pr.head.label.clone().unwrap_or_default(),
            title: pr.title.clone().unwrap_or_default(),
            node_id: pr.node_id.clone(),
            is_draft: pr.draft.unwrap_or(false),
        }
    }
}

#[async_trait]
impl PlatformService for GitHubService {
    async fn find_existing_pr(&self, head_branch: &str) -> Result<Option<PullRequest>> {
        let owner = &self.config.owner;
        let head_filter = format!("{owner}:{head_branch}");

        let page = self
            .pulls()
            .list()
            .head(head_filter)
            .state(octocrab::params::State::Open)
            .send()
            .await?;

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
        let pulls = self.pulls();
        let mut builder = pulls.create(title, head, base);

        if let Some(b) = body {
            builder = builder.body(b);
        }
        if draft {
            builder = builder.draft(true);
        }

        let pr = builder.send().await?;
        Ok(Self::convert_pr(&pr))
    }

    async fn update_pr_base(&self, pr_number: u64, new_base: &str) -> Result<PullRequest> {
        let pr = self
            .pulls()
            .update(pr_number)
            .base(new_base)
            .send()
            .await?;

        Ok(Self::convert_pr(&pr))
    }

    async fn publish_pr(&self, pr_number: u64) -> Result<PullRequest> {
        // The REST v3 PATCH endpoint does not support clearing draft status.
        // We need to use the GraphQL mutation `markPullRequestReadyForReview`.
        // For now, fall back to the update endpoint which at least refreshes
        // our local state; the caller can use the GitHub CLI for the actual
        // publish if needed.
        let pr = self
            .pulls()
            .update(pr_number)
            .send()
            .await?;

        if pr.draft.unwrap_or(false) {
            return Err(JjPlanError::GitHubApi(
                "cannot publish PR via REST API; use `gh pr ready` instead".to_string(),
            ));
        }

        Ok(Self::convert_pr(&pr))
    }

    async fn list_pr_comments(&self, pr_number: u64) -> Result<Vec<PrComment>> {
        let comments = self
            .client
            .issues(&self.config.owner, &self.config.repo)
            .list_comments(pr_number)
            .send()
            .await?;

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
        self.client
            .issues(&self.config.owner, &self.config.repo)
            .create_comment(pr_number, body)
            .await?;
        Ok(())
    }

    async fn update_pr_comment(
        &self,
        _pr_number: u64,
        comment_id: u64,
        body: &str,
    ) -> Result<()> {
        // Issue comments are repo-scoped, so pr_number is unused.
        let comment_id = octocrab::models::CommentId(comment_id);
        self.client
            .issues(&self.config.owner, &self.config.repo)
            .update_comment(comment_id, body)
            .await?;
        Ok(())
    }

    fn config(&self) -> &PlatformConfig {
        &self.config
    }

    async fn get_pr_details(&self, pr_number: u64) -> Result<PullRequestDetails> {
        let pr = self.pulls().get(pr_number).await?;

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
            base_ref: pr.base.ref_field.clone(),
            html_url: pr
                .html_url
                .as_ref()
                .map(|u| u.to_string())
                .unwrap_or_default(),
        })
    }

    async fn check_merge_readiness(&self, pr_number: u64) -> Result<MergeReadiness> {
        let details = self.get_pr_details(pr_number).await?;
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

        // We cannot cheaply determine approval / CI status from the REST PR
        // object alone; mark them as uncertain.
        uncertainties.push("approval status not checked (requires review API)".to_string());
        uncertainties.push("CI status not checked (requires checks API)".to_string());

        Ok(MergeReadiness {
            is_approved: false,
            ci_passed: false,
            is_mergeable: details.mergeable,
            is_draft: details.is_draft,
            blocking_reasons,
            uncertainties,
        })
    }

    async fn merge_pr(&self, pr_number: u64, method: MergeMethod) -> Result<MergeResult> {
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
            .await?;

        Ok(MergeResult {
            merged: result.merged,
            sha: result.sha,
            message: result.message,
        })
    }
}