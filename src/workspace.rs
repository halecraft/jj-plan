//! Unified workspace layer for jj-plan.
//!
//! Provides all repository operations needed by the plan lifecycle
//! (flush, sync, navigation, done, describe interception) and git write
//! operations (fetch, push, rebase, delete-bookmark) via jj-lib's
//! in-process API.
//!
//! ## Architecture: "Path A" — jj-lib for reads, CLI for writes
//!
//! Mutations (`jj describe`, `jj new`, `jj edit`, `jj abandon`, `jj bookmark set`)
//! remain as subprocess calls because the CLI handles working copy snapshotting,
//! auto-rebase, conflict resolution, and user-facing output formatting.
//!
//! ## Design decisions
//!
//! - **Cached repo snapshot.** Unlike ryu's `JjWorkspace` which re-loads the
//!   repo from scratch on every method call, this struct caches the repo and
//!   refreshes only via explicit `reload()` calls after CLI mutations.
//! - **Broader `trunk()` default.** Checks `main`, `master`, AND `trunk` branch
//!   names against both `origin` and `upstream` remotes, matching jj's actual
//!   CLI behavior more closely than the old jj-plan default.
//! - **Full hex change IDs in `LogEntry`.** Short reverse-hex prefixes for CLI
//!   use are computed on demand via `short_change_id()`.
//!
//! ## Git write operations
//!
//! `git_fetch`, `git_push`, `delete_bookmark`, `rebase_bookmark_onto_trunk`,
//! `git_remotes`, and `default_branch` are implemented via jj-lib's in-process
//! API (arrived in jj:zypnnqyt).

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::Arc;

use jj_lib::commit::Commit;
use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::git::{
    self, GitFetch, GitFetchRefExpression, GitImportOptions, GitProgress, GitRefUpdate,
    GitSettings, GitSidebandLineTerminator, GitSubprocessCallback, expand_fetch_refspecs,
    export_refs,
};
use jj_lib::hex_util::encode_reverse_hex;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_heads_store;
use jj_lib::op_store::{RefTarget, RemoteRef, RemoteRefState};
use jj_lib::ref_name::{RefName, RemoteName, RemoteNameBuf};
use jj_lib::repo::{ReadonlyRepo, Repo as _, StoreFactories};
use jj_lib::revset::{
    self, RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions, RevsetIteratorExt as _,
    RevsetParseContext, RevsetWorkspaceContext, SymbolResolver, SymbolResolverExtension,
};
use jj_lib::rewrite::{MoveCommitsLocation, MoveCommitsTarget, RebaseOptions, move_commits};
use jj_lib::settings::UserSettings;
use jj_lib::str_util::{StringExpression, StringMatcher};
use jj_lib::time_util::DatePatternContext;
use jj_lib::workspace::default_working_copy_factories;

use crate::error::JjPlanError;
use crate::types::{Bookmark, GitRemote, LogEntry};

/// A loaded jj repository ready for in-process reads.
///
/// Created once per command invocation via `Workspace::open()`. Holds the
/// jj workspace (for working copy info) and a cached read-only repo snapshot.
///
/// After CLI mutations, call `reload()` to refresh the snapshot.
pub struct Workspace {
    workspace: jj_lib::workspace::Workspace,
    repo: Arc<ReadonlyRepo>,
}

/// No-op callback for git subprocess operations.
///
/// Used by git_fetch and git_push. Can be upgraded later to wire into
/// indicatif progress reporting.
struct NoopGitCallback;

impl GitSubprocessCallback for NoopGitCallback {
    fn needs_progress(&self) -> bool {
        false
    }

    fn progress(&mut self, _progress: &GitProgress) -> io::Result<()> {
        Ok(())
    }

    fn local_sideband(
        &mut self,
        _message: &[u8],
        _term: Option<GitSidebandLineTerminator>,
    ) -> io::Result<()> {
        Ok(())
    }

    fn remote_sideband(
        &mut self,
        _message: &[u8],
        _term: Option<GitSidebandLineTerminator>,
    ) -> io::Result<()> {
        Ok(())
    }
}

impl Workspace {
    /// Open a jj workspace at the given repo root path.
    ///
    /// Returns `None` on any error (graceful degradation to subprocess fallback).
    /// This mirrors the loading pattern from jj's CLI, but simplified: we don't
    /// snapshot the working copy, resolve operation conflicts, or import git
    /// refs — we just need a read-only view.
    pub fn open(repo_root: &Path) -> Option<Self> {
        let config = build_minimal_config(repo_root)?;
        let settings = UserSettings::from_config(config).ok()?;

        let store_factories = StoreFactories::default();
        let working_copy_factories = default_working_copy_factories();

        let workspace =
            jj_lib::workspace::Workspace::load(
                &settings,
                repo_root,
                &store_factories,
                &working_copy_factories,
            )
            .ok()?;

        // Load at the latest operation (resolve concurrent op heads by picking
        // the first one — we only need a read-only snapshot, not perfect merging).
        let loader = workspace.repo_loader();
        let op = op_heads_store::resolve_op_heads(
            loader.op_heads_store().as_ref(),
            loader.op_store(),
            |mut op_heads: Vec<jj_lib::operation::Operation>| -> Result<_, OpResolveError> {
                Ok(op_heads.pop().unwrap())
            },
        )
        .ok()?;

        let repo = loader.load_at(&op).ok()?;

        Some(Self { workspace, repo })
    }

    /// Reload the repository after a CLI mutation.
    ///
    /// Calls `ReadonlyRepo::reload_at_head()` to get a fresh snapshot that
    /// reflects changes made by subprocess mutations. Reuses the existing
    /// loader (stores, factories), so this is cheaper than a full `open()`.
    ///
    /// Returns `true` on success, `false` on failure. On failure, the repo
    /// remains at its previous (stale) snapshot.
    pub fn reload(&mut self) -> bool {
        match self.repo.reload_at_head() {
            Ok(new_repo) => {
                self.repo = new_repo;
                true
            }
            Err(_) => false,
        }
    }

    // -----------------------------------------------------------------------
    // Revset evaluation
    // -----------------------------------------------------------------------

    /// Evaluate a revset string and return matching commits.
    ///
    /// Returns commits in jj's default topological order (children before
    /// parents). Returns `None` on parse/evaluation failure.
    pub fn evaluate_revset(&self, expr: &str) -> Option<Vec<Commit>> {
        evaluate_revset(&self.repo, &self.workspace, expr)
    }

    /// Evaluate a revset and return commits in reversed order (parents before
    /// children — stack order with 01 closest to trunk).
    pub fn evaluate_revset_reversed(&self, expr: &str) -> Option<Vec<Commit>> {
        let mut commits = self.evaluate_revset(expr)?;
        commits.reverse();
        Some(commits)
    }

    // -----------------------------------------------------------------------
    // Commit → type conversions
    // -----------------------------------------------------------------------

    /// Convert a jj `Commit` to a `LogEntry`.
    ///
    /// `change_id` stores full standard hex (64 chars). Use `short_change_id()`
    /// for CLI-facing short reverse-hex.
    pub fn commit_to_log_entry(&self, commit: &Commit) -> LogEntry {
        let view = self.repo.view();

        let local_bookmarks: Vec<String> = view
            .local_bookmarks_for_commit(commit.id())
            .map(|(name, _)| name.as_str().to_string())
            .collect();

        let remote_bookmarks: Vec<String> = view
            .all_remote_bookmarks()
            .filter(|(_, remote_ref)| {
                remote_ref
                    .target
                    .as_normal()
                    .is_some_and(|id| id == commit.id())
            })
            .map(|(symbol, _)| format!("{}@{}", symbol.name.as_str(), symbol.remote.as_str()))
            .collect();

        let parents: Vec<String> = commit.parent_ids().iter().map(|id| id.hex()).collect();

        let description = commit.description().to_owned();
        // Strip trailing newline (jj appends one to descriptions)
        let description = description
            .strip_suffix('\n')
            .unwrap_or(&description)
            .to_string();
        let description_first_line = description.lines().next().unwrap_or("").to_string();

        let author = commit.author();
        let committer = commit.committer();

        let authored_at = timestamp_to_datetime(&author.timestamp);
        let committed_at = timestamp_to_datetime(&committer.timestamp);

        let is_working_copy = self
            .repo
            .view()
            .get_wc_commit_id(self.workspace.workspace_name())
            == Some(commit.id());

        let is_empty = commit_is_empty(&self.repo, commit);

        LogEntry {
            commit_id: commit.id().hex(),
            change_id: commit.change_id().hex(),
            author_name: author.name.clone(),
            author_email: author.email.clone(),
            description_first_line,
            description,
            parents,
            local_bookmarks,
            remote_bookmarks,
            is_working_copy,
            is_empty,
            authored_at,
            committed_at,
        }
    }

    /// Get the shortest unique change ID prefix for a commit (8+ chars).
    ///
    /// Returns the reverse-hex encoding (k-z alphabet) that jj uses for
    /// display and revset resolution.
    pub fn short_change_id(&self, commit: &Commit) -> String {
        short_change_id(&self.repo, commit)
    }

    // -----------------------------------------------------------------------
    // Bookmark queries (from ryu, adapted)
    // -----------------------------------------------------------------------

    /// Get all local bookmarks with sync status.
    ///
    /// Returns `Bookmark` structs with `has_remote` and `is_synced` computed
    /// from the remote tracking state. Uses the cached repo rather than
    /// ryu's reload-every-call pattern.
    pub fn local_bookmarks(&self) -> Vec<Bookmark> {
        let view = self.repo.view();
        let mut bookmarks = Vec::new();

        for (name, target) in view.local_bookmarks() {
            if let Some(commit_id) = target.as_normal() {
                let commit = match self.repo.store().get_commit(commit_id) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                // Check if bookmark has remote tracking (excluding @git pseudo-remote)
                let name_matcher =
                    jj_lib::str_util::StringPattern::exact(name.as_str()).to_matcher();
                let remote_matcher = jj_lib::str_util::StringMatcher::All;
                let has_remote = view
                    .remote_bookmarks_matching(&name_matcher, &remote_matcher)
                    .any(|(symbol, _)| symbol.remote.as_str() != "git");

                // Check if synced with remote (excluding @git pseudo-remote)
                let is_synced = view
                    .remote_bookmarks_matching(&name_matcher, &remote_matcher)
                    .filter(|(symbol, _)| symbol.remote.as_str() != "git")
                    .any(|(_, remote_ref)| {
                        remote_ref
                            .target
                            .as_normal()
                            .is_some_and(|id| id == commit_id)
                    });

                bookmarks.push(Bookmark {
                    name: name.as_str().to_string(),
                    commit_id: commit_id.hex(),
                    change_id: commit.change_id().hex(),
                    has_remote,
                    is_synced,
                });
            }
        }

        bookmarks
    }

    // -----------------------------------------------------------------------
    // Single-value read helpers (replace isolated subprocess reads in commands)
    // -----------------------------------------------------------------------

    /// Read the working copy's shortest unique change ID (reverse hex).
    ///
    /// Callers pass this to `jj` subprocesses.
    pub fn read_change_id_at_wc(&self) -> Option<String> {
        let wc_commit_id = self
            .repo
            .view()
            .get_wc_commit_id(self.workspace.workspace_name())?;
        let commit = self.repo.store().get_commit(wc_commit_id).ok()?;
        Some(short_change_id(&self.repo, &commit))
    }

    /// Read a change's description by evaluating a revset target.
    ///
    /// Returns the description with trailing newline stripped.
    pub fn read_description_at(&self, target: &str) -> Option<String> {
        let commits = self.evaluate_revset(target)?;
        let commit = commits.first()?;
        let desc = commit.description().to_owned();
        Some(desc.strip_suffix('\n').unwrap_or(&desc).to_string())
    }

    /// Resolve a revset target to a shortest unique change ID (reverse hex).
    ///
    /// Callers pass this to `jj` subprocesses.
    pub fn resolve_change_id(&self, target: &str) -> Option<String> {
        let commits = self.evaluate_revset(target)?;
        let commit = commits.first()?;
        Some(short_change_id(&self.repo, &commit))
    }

    /// Check whether a commit identified by a revset target exists.
    pub fn commit_exists(&self, target: &str) -> bool {
        self.evaluate_revset(target)
            .map(|commits| !commits.is_empty())
            .unwrap_or(false)
    }

    /// Return the first child's change ID for a given change ID.
    ///
    /// Evaluates `children(change_id) ~ change_id` and returns the first
    /// result's shortest change ID, or `None` if no children exist.
    pub fn first_child_change_id(&self, change_id: &str) -> Option<String> {
        let revset_str = format!("children({}) ~ {}", change_id, change_id);
        let commits = self.evaluate_revset(&revset_str)?;
        let commit = commits.last()?; // reversed = parents first, so last is earliest child
        Some(short_change_id(&self.repo, &commit))
    }

    // -----------------------------------------------------------------------
    // Flush support: batch description reads
    // -----------------------------------------------------------------------

    /// Gather jj descriptions for a set of change IDs, for use by flush.
    ///
    /// Accepts short reverse-hex IDs (from plan filenames), joins them into
    /// a revset, and keys results by the short reverse-hex IDs from
    /// `short_change_id()` so the caller can match against input.
    ///
    /// Returns a HashMap of short_change_id → description.
    pub fn gather_descriptions(&self, change_ids: &[&str]) -> HashMap<String, String> {
        if change_ids.is_empty() {
            return HashMap::new();
        }
        let revset_str = change_ids.join(" | ");
        let commits = match self.evaluate_revset_reversed(&revset_str) {
            Some(c) => c,
            None => return HashMap::new(),
        };
        if commits.is_empty() {
            return HashMap::new();
        }

        let mut result = HashMap::new();
        for commit in &commits {
            let id = short_change_id(&self.repo, commit);
            let description = commit.description().to_owned();
            let description = description
                .strip_suffix('\n')
                .unwrap_or(&description)
                .to_string();
            result.insert(id, description);
        }
        result
    }

    // -----------------------------------------------------------------------
    // Accessors (for stack builder and other modules)
    // -----------------------------------------------------------------------

    /// Access the underlying jj-lib repo (for stack builder).
    pub fn repo(&self) -> &Arc<ReadonlyRepo> {
        &self.repo
    }

    /// Access the underlying jj-lib workspace (for stack builder).
    pub fn jj_workspace(&self) -> &jj_lib::workspace::Workspace {
        &self.workspace
    }

    // -----------------------------------------------------------------------
    // Git write operations (deferred from jj:pozrnomw, arriving in jj:zypnnqyt)
    // -----------------------------------------------------------------------

    /// Get all git remotes with their URLs.
    pub fn git_remotes(&self) -> std::result::Result<Vec<GitRemote>, JjPlanError> {
        let remote_names = git::get_all_remote_names(self.repo.store())
            .map_err(|_| JjPlanError::Git("Not a git-backed repo".to_string()))?;

        let git_repo = git::get_git_repo(self.repo.store())
            .map_err(|_| JjPlanError::Git("Not a git-backed repo".to_string()))?;

        let mut remotes = Vec::new();
        for name in remote_names {
            let url = git_repo
                .try_find_remote(name.as_str())
                .and_then(std::result::Result::ok)
                .and_then(|remote| {
                    remote
                        .url(gix::remote::Direction::Push)
                        .map(|u| u.to_bstring().to_string())
                })
                .unwrap_or_default();

            remotes.push(GitRemote {
                name: name.as_str().to_string(),
                url,
            });
        }

        Ok(remotes)
    }

    /// Get the default branch name by checking remote HEAD first, then common names.
    pub fn default_branch(&self) -> String {
        // Try to detect from git remote HEAD
        if let Ok(git_repo) = git::get_git_repo(self.repo.store()) {
            if let Some((branch, _)) = detect_default_branch_from_remote(&git_repo) {
                return branch;
            }
        }

        // Fall back to checking local bookmarks for common names
        let view = self.repo.view();
        for name in &["main", "master", "trunk"] {
            let target = view.get_local_bookmark(RefName::new(name));
            if target.is_present() {
                return (*name).to_string();
            }
        }

        // Final fallback
        "main".to_string()
    }

    /// Fetch from a git remote.
    pub fn git_fetch(&mut self, remote: &str) -> std::result::Result<(), JjPlanError> {
        // Reload to get fresh state before write
        self.reload();

        let settings = build_minimal_settings()?;
        let git_settings = GitSettings::from_settings(&settings)
            .map_err(|e| JjPlanError::Config(format!("Invalid git settings: {e}")))?;

        let mut tx = self.repo.start_transaction();

        let import_options = GitImportOptions {
            auto_local_bookmark: git_settings.auto_local_bookmark,
            abandon_unreachable_commits: git_settings.abandon_unreachable_commits,
            remote_auto_track_bookmarks: std::iter::once((
                RemoteNameBuf::from(remote),
                StringMatcher::all(),
            ))
            .collect(),
        };

        let mut fetch = GitFetch::new(
            tx.repo_mut(),
            git_settings.to_subprocess_options(),
            &import_options,
        )
        .map_err(|e| JjPlanError::Git(format!("Failed to create fetch: {e}")))?;

        let remote_name = RemoteName::new(remote);
        let refspecs = expand_fetch_refspecs(
            remote_name,
            GitFetchRefExpression {
                bookmark: StringExpression::all(),
                tag: StringExpression::none(),
            },
        )
        .map_err(|e| JjPlanError::Git(format!("Failed to expand refspecs: {e}")))?;

        let mut callback = NoopGitCallback;
        fetch
            .fetch(remote_name, refspecs, &mut callback, None, None)
            .map_err(|e| JjPlanError::Git(format!("Failed to fetch: {e}")))?;

        fetch
            .import_refs()
            .map_err(|e| JjPlanError::Git(format!("Failed to import refs: {e}")))?;

        // Rebase descendants if there were any rewrites from the import
        if tx.repo().has_rewrites() {
            tx.repo_mut()
                .rebase_descendants()
                .map_err(|e| JjPlanError::Git(format!("Failed to rebase descendants: {e}")))?;
        }

        let new_repo = tx
            .commit(format!("fetch from {remote}"))
            .map_err(|e| JjPlanError::Git(format!("Failed to commit fetch: {e}")))?;
        self.repo = new_repo;

        Ok(())
    }

    /// Push a bookmark to a remote.
    pub fn git_push(
        &mut self,
        bookmark: &str,
        remote: &str,
    ) -> std::result::Result<(), JjPlanError> {
        // Reload to get fresh state before write
        self.reload();

        let settings = build_minimal_settings()?;
        let git_settings = GitSettings::from_settings(&settings)
            .map_err(|e| JjPlanError::Config(format!("Invalid git settings: {e}")))?;

        let view = self.repo.view();
        let ref_name = RefName::new(bookmark);
        let target = view.get_local_bookmark(ref_name);

        if !target.is_present() {
            return Err(JjPlanError::BookmarkNotFound(bookmark.to_string()));
        }

        let new_target = target.as_normal().cloned();

        let remote_name = RemoteName::new(remote);
        let remote_symbol = ref_name.to_remote_symbol(remote_name);
        let remote_ref = view.get_remote_bookmark(remote_symbol);
        let expected_current_target = remote_ref.target.as_normal().cloned();

        let mut tx = self.repo.start_transaction();

        // Export refs to underlying git repo before pushing
        let export_stats = export_refs(tx.repo_mut())
            .map_err(|e| JjPlanError::Git(format!("Failed to export refs: {e}")))?;

        if export_stats
            .failed_bookmarks
            .iter()
            .any(|(symbol, _)| symbol.name.as_str() == bookmark)
        {
            return Err(JjPlanError::Git(format!(
                "Failed to export bookmark '{bookmark}' to git"
            )));
        }

        let update = GitRefUpdate {
            qualified_name: format!("refs/heads/{bookmark}").into(),
            expected_current_target,
            new_target,
        };

        let mut callback = NoopGitCallback;
        git::push_updates(
            tx.repo().base_repo().as_ref(),
            git_settings.to_subprocess_options(),
            remote_name,
            &[update],
            &mut callback,
        )
        .map_err(|e| JjPlanError::Git(format!("Failed to push: {e}")))?;

        // Update the remote tracking ref
        let new_remote_ref = RemoteRef {
            target: target.clone(),
            state: RemoteRefState::Tracked,
        };
        tx.repo_mut()
            .set_remote_bookmark(remote_symbol, new_remote_ref);

        let new_repo = tx
            .commit(format!("push {bookmark} to {remote}"))
            .map_err(|e| JjPlanError::Git(format!("Failed to commit push: {e}")))?;
        self.repo = new_repo;

        Ok(())
    }

    /// Rebase a bookmark and its descendants onto trunk.
    pub fn rebase_bookmark_onto_trunk(
        &mut self,
        bookmark: &str,
    ) -> std::result::Result<(), JjPlanError> {
        // Reload to get fresh state before write
        self.reload();

        // Resolve trunk
        let trunk_commits = self.evaluate_revset("trunk()");
        let trunk_commit = trunk_commits
            .as_ref()
            .and_then(|v| v.first())
            .ok_or_else(|| {
                JjPlanError::RebaseFailed("trunk() resolved to empty set".to_string())
            })?;
        let trunk_commit_id = trunk_commit.id().clone();

        // Resolve the bookmark
        let bookmark_commits = self.evaluate_revset(bookmark);
        let bookmark_commit = bookmark_commits
            .as_ref()
            .and_then(|v| v.first())
            .ok_or_else(|| {
                JjPlanError::RebaseFailed(format!(
                    "bookmark '{bookmark}' resolved to empty set"
                ))
            })?;
        let bookmark_commit_id = bookmark_commit.id().clone();

        let mut tx = self.repo.start_transaction();

        let location = MoveCommitsLocation {
            new_parent_ids: vec![trunk_commit_id],
            new_child_ids: vec![],
            target: MoveCommitsTarget::Roots(vec![bookmark_commit_id]),
        };

        let options = RebaseOptions::default();

        move_commits(tx.repo_mut(), &location, &options)
            .map_err(|e| JjPlanError::RebaseFailed(format!("Failed to rebase: {e}")))?;

        let new_repo = tx
            .commit(format!("rebase {bookmark} onto trunk"))
            .map_err(|e| JjPlanError::RebaseFailed(format!("Failed to commit rebase: {e}")))?;
        self.repo = new_repo;

        Ok(())
    }

    /// Delete a local bookmark.
    pub fn delete_bookmark(&mut self, bookmark: &str) -> std::result::Result<(), JjPlanError> {
        // Reload to get fresh state before write
        self.reload();

        let mut tx = self.repo.start_transaction();

        let ref_name = RefName::new(bookmark);
        tx.repo_mut()
            .set_local_bookmark_target(ref_name, RefTarget::absent());

        let new_repo = tx
            .commit(format!("delete bookmark {bookmark}"))
            .map_err(|e| JjPlanError::Git(format!("Failed to commit bookmark deletion: {e}")))?;
        self.repo = new_repo;

        Ok(())
    }
}

// ===========================================================================
// Private helpers
// ===========================================================================

/// Detect default branch from git remote HEAD (e.g., refs/remotes/origin/HEAD).
///
/// Returns `(branch_name, remote_name)` if found.
fn detect_default_branch_from_remote(
    git_repo: &gix::Repository,
) -> Option<(String, &'static str)> {
    const REMOTE_PREFERENCE: &[&str] = &["origin", "upstream"];

    for &remote in REMOTE_PREFERENCE {
        let ref_name = format!("refs/remotes/{remote}/HEAD");
        if let Some(reference) = git_repo.try_find_reference(&ref_name).ok().flatten()
            && let Some(target_name) = reference.target().try_name()
        {
            let target_str = target_name.to_string();
            let prefix = format!("refs/remotes/{remote}/");
            if let Some(branch) = target_str.strip_prefix(&prefix) {
                return Some((branch.to_string(), remote));
            }
        }
    }
    None
}

/// Select a remote from a list of available remotes.
///
/// - If `specified` is provided and exists, use it
/// - If only one remote exists, use it
/// - If multiple remotes exist, prefer "origin", else use first
pub fn select_remote(
    remotes: &[GitRemote],
    specified: Option<&str>,
) -> std::result::Result<String, JjPlanError> {
    if remotes.is_empty() {
        return Err(JjPlanError::NoSupportedRemotes);
    }

    if let Some(name) = specified {
        if !remotes.iter().any(|r| r.name == name) {
            return Err(JjPlanError::RemoteNotFound(name.to_string()));
        }
        return Ok(name.to_string());
    }

    if remotes.len() == 1 {
        return Ok(remotes[0].name.clone());
    }

    Ok(remotes
        .iter()
        .find(|r| r.name == "origin")
        .map_or_else(|| remotes[0].name.clone(), |r| r.name.clone()))
}

/// Error type for op-head resolution that satisfies jj-lib's trait bounds.
#[derive(Debug)]
enum OpResolveError {
    OpHeadResolution(jj_lib::op_heads_store::OpHeadResolutionError),
    OpHeadsStore(jj_lib::op_heads_store::OpHeadsStoreError),
    OpStore(jj_lib::op_store::OpStoreError),
}

impl From<jj_lib::op_heads_store::OpHeadResolutionError> for OpResolveError {
    fn from(e: jj_lib::op_heads_store::OpHeadResolutionError) -> Self {
        Self::OpHeadResolution(e)
    }
}

impl From<jj_lib::op_heads_store::OpHeadsStoreError> for OpResolveError {
    fn from(e: jj_lib::op_heads_store::OpHeadsStoreError) -> Self {
        Self::OpHeadsStore(e)
    }
}

impl From<jj_lib::op_store::OpStoreError> for OpResolveError {
    fn from(e: jj_lib::op_store::OpStoreError) -> Self {
        Self::OpStore(e)
    }
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

/// Build a minimal `UserSettings` for write operations.
///
/// Uses the same config as `build_minimal_config()` but returns Result
/// instead of Option for better error reporting.
fn build_minimal_settings() -> std::result::Result<UserSettings, JjPlanError> {
    // We need a UserSettings for GitSettings::from_settings.
    // Build a minimal config — the actual user config is not critical for
    // git_fetch/git_push since they only need git.auto-local-bookmark etc.
    let config = StackedConfig::with_defaults();
    UserSettings::from_config(config)
        .map_err(|e| JjPlanError::Config(format!("Failed to build settings: {e}")))
}

/// Build a minimal StackedConfig for workspace loading.
///
/// Loads repo-level config (for `revset-aliases.trunk()`) and user config
/// (~/.jjconfig.toml) on top of jj-lib's built-in defaults.
fn build_minimal_config(repo_root: &Path) -> Option<StackedConfig> {
    let mut config = StackedConfig::with_defaults();

    // Load repo-level config if it exists (may contain revset-aliases.trunk())
    let repo_config_path = repo_root.join(".jj").join("repo").join("config.toml");
    if repo_config_path.is_file() {
        if let Ok(content) = std::fs::read_to_string(&repo_config_path) {
            if let Ok(doc) = content.parse::<toml_edit::DocumentMut>() {
                config.add_layer(ConfigLayer::with_data(ConfigSource::Repo, doc));
            }
        }
    }

    // Load user config (~/.jjconfig.toml or XDG equivalent)
    if let Some(user_config) = load_user_config() {
        config.add_layer(user_config);
    }

    Some(config)
}

/// Attempt to load the user's jj config file.
fn load_user_config() -> Option<ConfigLayer> {
    // Check JJ_CONFIG env var first
    if let Ok(path) = std::env::var("JJ_CONFIG") {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(doc) = content.parse::<toml_edit::DocumentMut>() {
                return Some(ConfigLayer::with_data(ConfigSource::User, doc));
            }
        }
    }

    // Standard locations: ~/.jjconfig.toml
    if let Some(home) = home_dir() {
        let path = home.join(".jjconfig.toml");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(doc) = content.parse::<toml_edit::DocumentMut>() {
                return Some(ConfigLayer::with_data(ConfigSource::User, doc));
            }
        }
    }

    // XDG: $XDG_CONFIG_HOME/jj/config.toml
    let xdg_config = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".config")));
    if let Some(xdg) = xdg_config {
        let path = xdg.join("jj").join("config.toml");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(doc) = content.parse::<toml_edit::DocumentMut>() {
                return Some(ConfigLayer::with_data(ConfigSource::User, doc));
            }
        }
    }

    None
}

/// Get the user's home directory.
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
}

// ---------------------------------------------------------------------------
// Revset evaluation
// ---------------------------------------------------------------------------

/// Evaluate a revset string and return matching commits in topological order
/// (children before parents).
fn evaluate_revset(
    repo: &Arc<ReadonlyRepo>,
    workspace: &jj_lib::workspace::Workspace,
    revset_str: &str,
) -> Option<Vec<Commit>> {
    let aliases_map = load_revset_aliases(repo);
    let extensions = RevsetExtensions::default();
    let path_converter = jj_lib::repo_path::RepoPathUiConverter::Fs {
        cwd: std::env::current_dir().ok()?,
        base: workspace.workspace_root().to_owned(),
    };
    let workspace_ctx = RevsetWorkspaceContext {
        path_converter: &path_converter,
        workspace_name: workspace.workspace_name(),
    };

    let mut diagnostics = RevsetDiagnostics::new();
    let context = RevsetParseContext {
        aliases_map: &aliases_map,
        local_variables: HashMap::new(),
        user_email: "",
        date_pattern_context: DatePatternContext::from(chrono::Local::now()),
        default_ignored_remote: None,
        use_glob_by_default: false,
        extensions: &extensions,
        workspace: Some(workspace_ctx),
    };

    let expression = revset::parse(&mut diagnostics, revset_str, &context).ok()?;
    let no_extensions: Vec<Box<dyn SymbolResolverExtension>> = vec![];
    let symbol_resolver = SymbolResolver::new(repo.as_ref(), &no_extensions);
    let resolved = expression
        .resolve_user_expression(repo.as_ref(), &symbol_resolver)
        .ok()?;
    let revset_result = resolved.evaluate(repo.as_ref()).ok()?;

    let mut commits = Vec::new();
    for commit_or_err in revset_result.iter().commits(repo.store()) {
        let commit = commit_or_err.ok()?;
        commits.push(commit);
    }

    Some(commits)
}

/// Load revset aliases from the repo's config.
///
/// Provides a broader `trunk()` default that checks `main`, `master`, AND
/// `trunk` branch names against both `origin` and `upstream` remotes,
/// matching jj's actual CLI behavior more closely. User config overrides
/// take precedence.
fn load_revset_aliases(repo: &Arc<ReadonlyRepo>) -> RevsetAliasesMap {
    let mut aliases = RevsetAliasesMap::new();

    // Broader trunk() default matching jj's actual CLI behavior and ryu's default.
    // Checks main/master/trunk against origin and upstream, falling back to root().
    // Context: jj:pozrnomw
    let _ = aliases.insert(
        "trunk()",
        r#"latest(
            remote_bookmarks(exact:"main", exact:"origin") |
            remote_bookmarks(exact:"master", exact:"origin") |
            remote_bookmarks(exact:"trunk", exact:"origin") |
            remote_bookmarks(exact:"main", exact:"upstream") |
            remote_bookmarks(exact:"master", exact:"upstream") |
            remote_bookmarks(exact:"trunk", exact:"upstream") |
            root()
        )"#,
    );

    // Load user-configured aliases from the repo settings.
    // User aliases override the defaults above.
    let config = repo.settings().config();
    if let Ok(table) = config.get_table("revset-aliases") {
        for (key, value) in table.iter() {
            if let Some(value_str) = value.as_str() {
                let _ = aliases.insert(key, value_str);
            }
        }
    }

    aliases
}

// ---------------------------------------------------------------------------
// Commit helpers
// ---------------------------------------------------------------------------

/// Get the shortest unique change ID prefix for a commit (8+ chars).
///
/// Returns the reverse-hex encoding (k-z alphabet) that jj uses for display
/// and revset resolution.
fn short_change_id(repo: &Arc<ReadonlyRepo>, commit: &Commit) -> String {
    let change_id = commit.change_id();
    let prefix_len = repo
        .shortest_unique_change_id_prefix_len(change_id)
        .unwrap_or(8)
        .max(8);
    let reverse_hex = encode_reverse_hex(change_id.as_bytes());
    let len = prefix_len.min(reverse_hex.len());
    reverse_hex[..len].to_string()
}

/// Check whether a commit is "empty" (its tree matches its parent's merged tree).
fn commit_is_empty(repo: &Arc<ReadonlyRepo>, commit: &Commit) -> bool {
    match commit.parent_tree(repo.as_ref()) {
        Ok(parent_tree) => commit.tree_ids() == parent_tree.tree_ids(),
        Err(_) => false,
    }
}

/// Convert a jj `Timestamp` to a `chrono::DateTime<Utc>`.
fn timestamp_to_datetime(timestamp: &jj_lib::backend::Timestamp) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone as _;
    chrono::Utc
        .timestamp_millis_opt(timestamp.timestamp.0)
        .single()
        .unwrap_or_else(chrono::Utc::now)
}