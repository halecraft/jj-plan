//! Unified workspace layer for jj-plan.
//!
//! This module merges jj-plan's `LoadedRepo` and jj-ryu's `JjWorkspace` into
//! a single `Workspace` struct. It provides all read operations needed by the
//! plan lifecycle (flush, sync, navigation, done, describe interception) via
//! jj-lib's in-process API.
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
//! ## Git write operations (deferred)
//!
//! `git_fetch`, `git_push`, `delete_bookmark`, `rebase_bookmark_onto_trunk`,
//! `git_remotes`, and `default_branch` are deferred to jj:zypnnqyt where they
//! can be tested end-to-end with their first callers.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use jj_lib::commit::Commit;
use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::hex_util::encode_reverse_hex;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_heads_store;
use jj_lib::repo::{ReadonlyRepo, Repo as _, StoreFactories};
use jj_lib::revset::{
    self, RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions, RevsetIteratorExt as _,
    RevsetParseContext, RevsetWorkspaceContext, SymbolResolver, SymbolResolverExtension,
};
use jj_lib::settings::UserSettings;
use jj_lib::time_util::DatePatternContext;
use jj_lib::workspace::default_working_copy_factories;

use crate::types::{Bookmark, LogEntry};

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
}

// ===========================================================================
// Private helpers
// ===========================================================================

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