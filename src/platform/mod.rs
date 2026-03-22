//! Platform services for GitHub and GitLab.
//!
//! Provides a unified interface for PR/MR operations across platforms.

mod detection;
mod factory;
mod github;
mod gitlab;

pub use detection::parse_repo_info;
pub use factory::create_platform_service;
pub use github::GitHubService;
pub use gitlab::GitLabService;

use crate::error::Result;
use crate::types::{
    MergeMethod, MergeReadiness, MergeResult, PlatformConfig, PrComment, PullRequest,
    PullRequestDetails,
};
use async_trait::async_trait;

/// Platform service trait for PR/MR operations.
///
/// This trait abstracts GitHub and GitLab operations, allowing the same
/// submission logic to work with either platform.
#[async_trait]
pub trait PlatformService: Send + Sync {
    async fn find_existing_pr(&self, head_branch: &str) -> Result<Option<PullRequest>>;

    async fn create_pr(&self, head: &str, base: &str, title: &str) -> Result<PullRequest> {
        self.create_pr_with_options(head, base, title, None, false)
            .await
    }

    async fn create_pr_with_options(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: Option<&str>,
        draft: bool,
    ) -> Result<PullRequest>;

    async fn update_pr_base(&self, pr_number: u64, new_base: &str) -> Result<PullRequest>;
    async fn publish_pr(&self, pr_number: u64) -> Result<PullRequest>;
    async fn list_pr_comments(&self, pr_number: u64) -> Result<Vec<PrComment>>;
    async fn create_pr_comment(&self, pr_number: u64, body: &str) -> Result<()>;
    async fn update_pr_comment(&self, pr_number: u64, comment_id: u64, body: &str) -> Result<()>;
    fn config(&self) -> &PlatformConfig;
    async fn get_pr_details(&self, pr_number: u64) -> Result<PullRequestDetails>;
    async fn check_merge_readiness(&self, pr_number: u64) -> Result<MergeReadiness>;
    async fn merge_pr(&self, pr_number: u64, method: MergeMethod) -> Result<MergeResult>;
}