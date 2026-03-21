//! Shared command context for `jj stack` commands.
//!
//! Extracts common setup code shared by submit, sync, and merge commands.

use crate::error::{JjPlanError, Result};

use crate::platform::{create_platform_service, parse_repo_info, PlatformService};
use crate::pr_cache::{load_pr_cache, PrCache};
use crate::types::PlanRegistry;
use crate::workspace::{select_remote, Workspace};
use std::path::{Path, PathBuf};

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
    /// Root path of the workspace.
    pub workspace_root: PathBuf,
    /// Plan registry (which bookmarks are designated for submission).
    pub plan_registry: PlanRegistry,
    /// PR cache for bookmark → PR mappings.
    pub pr_cache: PrCache,
    /// Platform service (GitHub/GitLab).
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
    pub async fn new(
        workspace: &Workspace,
        workspace_root: &Path,
        remote: Option<&str>,
        registry: &PlanRegistry,
    ) -> Result<Self> {
        // Use caller-provided registry; load PR cache
        let plan_registry = registry.clone();
        let pr_cache = load_pr_cache(workspace_root)?;

        // Get remotes and select one
        let remotes = workspace.git_remotes()?;
        let remote_name = select_remote(&remotes, remote)?;

        // Detect platform from remote URL
        let remote_info = remotes
            .iter()
            .find(|r| r.name == remote_name)
            .ok_or_else(|| JjPlanError::RemoteNotFound(remote_name.clone()))?;

        let platform_config = parse_repo_info(&remote_info.url)?;

        // Create platform service (resolves auth token)
        let platform = create_platform_service(&platform_config).await?;

        // Get default branch
        let default_branch = workspace.default_branch();

        Ok(Self {
            workspace_root: workspace_root.to_path_buf(),
            plan_registry,
            pr_cache,
            platform,
            remote_name,
            default_branch,
        })
    }

    /// Check if any bookmarks are tracked.
    pub fn has_tracked_bookmarks(&self) -> bool {
        !self.plan_registry.tracked_names().is_empty()
    }

    /// Get tracked bookmark names.
    pub fn tracked_names(&self) -> Vec<&str> {
        self.plan_registry.tracked_names()
    }
}