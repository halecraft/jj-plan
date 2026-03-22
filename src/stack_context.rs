//! Shared command context for `jj stack` commands.
//!
//! Extracts common setup code shared by submit, sync, and merge commands.

use crate::error::{JjPlanError, Result};

use crate::platform::{
    create_platform_service, parse_repo_info, parse_repo_info_as_gitea, PlatformService,
};
use crate::pr_cache::{load_pr_cache, PrCache};
use crate::types::PlanRegistry;
use crate::workspace::{select_remote, Workspace};
use std::path::Path;

/// Shared context for CLI commands that interact with the platform.
///
/// This struct encapsulates the common setup needed by submit, sync, and merge:
/// - Using the jj workspace (for git operations and stack building)
/// - Loading plan registry and PR cache
/// - Selecting and validating the remote
/// - Detecting the platform and creating the service
///
/// Note: Does NOT own the workspace — it borrows mutably from the caller.
/// The workspace reference is NOT stored in this struct because Rust lifetimes
/// make it difficult to borrow &mut Workspace while also passing &StackContext
/// to functions. Instead, callers pass both the context and workspace separately.
pub struct StackContext {
    /// PR cache for bookmark → PR mappings.
    pub pr_cache: PrCache,
    /// Platform service (GitHub/GitLab/Gitea).
    pub platform: Box<dyn PlatformService>,
    /// Selected remote name.
    pub remote_name: String,
    /// Default branch name (e.g., "main").
    pub default_branch: String,
}

impl StackContext {
    /// Create a new stack context.
    ///
    /// This performs the common setup shared by submit/sync/merge:
    /// - Load plan registry
    /// - Load PR cache
    /// - Select and validate remote
    /// - Detect platform and create service
    /// - Get default branch
    ///
    /// When the remote URL doesn't match any known platform (GitHub, GitLab,
    /// Gitea via `GITEA_HOST` or codeberg.org), an async probe of the host's
    /// `/api/v1/version` endpoint is attempted. If it returns a JSON object
    /// with a `"version"` field, the host is assumed to be a Gitea instance.
    pub async fn new(
        workspace: &Workspace,
        workspace_root: &Path,
        remote: Option<&str>,
        _registry: &PlanRegistry,
    ) -> Result<Self> {
        // Load PR cache
        let pr_cache = load_pr_cache(workspace_root)?;

        // Get remotes and select one
        let remotes = workspace.git_remotes()?;
        let remote_name = select_remote(&remotes, remote)?;

        // Detect platform from remote URL
        let remote_info = remotes
            .iter()
            .find(|r| r.name == remote_name)
            .ok_or_else(|| JjPlanError::RemoteNotFound(remote_name.clone()))?;

        let platform_config = match parse_repo_info(&remote_info.url) {
            Ok(config) => config,
            Err(JjPlanError::NoSupportedRemotes) => {
                // Unknown host — try async Gitea probe before giving up.
                probe_gitea_fallback(&remote_info.url).await?
            }
            Err(e) => return Err(e),
        };

        // Create platform service (resolves auth token)
        let platform = create_platform_service(&platform_config).await?;

        // Get default branch
        let default_branch = workspace.default_branch();

        Ok(Self {
            pr_cache,
            platform,
            remote_name,
            default_branch,
        })
    }
}

/// Probe a remote host's `/api/v1/version` endpoint to detect Gitea.
///
/// Gitea (and its forks like Forgejo) respond to this endpoint with a JSON
/// object containing a `"version"` field. If the probe succeeds, the URL is
/// parsed as a Gitea remote. If it fails, the original `NoSupportedRemotes`
/// error is returned.
async fn probe_gitea_fallback(
    url: &str,
) -> Result<crate::types::PlatformConfig> {
    // Extract hostname from the remote URL to build the probe URL.
    let hostname = extract_probe_hostname(url)
        .ok_or(JjPlanError::NoSupportedRemotes)?;

    let probe_url = format!("https://{hostname}/api/v1/version");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|_| JjPlanError::NoSupportedRemotes)?;

    let response = client
        .get(&probe_url)
        .send()
        .await
        .map_err(|_| JjPlanError::NoSupportedRemotes)?;

    if !response.status().is_success() {
        return Err(JjPlanError::NoSupportedRemotes);
    }

    // Check that the response is JSON with a "version" field.
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| JjPlanError::NoSupportedRemotes)?;

    if body.get("version").and_then(|v| v.as_str()).is_none() {
        return Err(JjPlanError::NoSupportedRemotes);
    }

    // Probe succeeded — parse the URL as a Gitea remote.
    parse_repo_info_as_gitea(url)
}

/// Extract hostname from a git remote URL for probing.
fn extract_probe_hostname(url: &str) -> Option<String> {
    if url.starts_with("git@") {
        return url
            .strip_prefix("git@")
            .and_then(|s| s.split(':').next())
            .map(ToString::to_string);
    }
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(ToString::to_string))
}