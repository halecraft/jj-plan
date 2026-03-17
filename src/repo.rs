//! In-process repository access via jj-lib.
//!
//! This module provides direct access to the jj repository without spawning
//! subprocess calls. It loads the workspace once (~5-10ms) and then all
//! subsequent reads (revset evaluation, commit data, bookmark enumeration)
//! are sub-millisecond.
//!
//! Architecture: "Path A" — jj-lib for reads, CLI for writes.
//! Mutations (`jj describe`, `jj new`, `jj edit`, `jj abandon`, `jj bookmark set`)
//! remain as subprocess calls because the CLI handles working copy snapshotting,
//! auto-rebase, conflict resolution, and user-facing output formatting.

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
use jj_lib::workspace::{default_working_copy_factories, Workspace};

use crate::stack::{StackBase, StackChange};

/// A loaded jj repository ready for in-process reads.
///
/// Created once per command invocation via `load_repo()`. Holds the workspace
/// (for working copy info) and a read-only repo snapshot.
pub struct LoadedRepo {
    pub workspace: Workspace,
    pub repo: Arc<ReadonlyRepo>,
}

impl LoadedRepo {
    /// Reload the repository after a CLI mutation.
    ///
    /// Calls `ReadonlyRepo::reload_at_head()` to get a fresh snapshot that
    /// reflects changes made by subprocess mutations (`jj describe`,
    /// `jj new`, `jj edit`, etc.). Reuses the existing loader (stores,
    /// factories), so this is cheaper than a full `load_repo()`.
    ///
    /// Returns `true` on success, `false` on failure. On failure, the
    /// repo remains at its previous (stale) snapshot — callers should
    /// fall back to subprocess reads.
    pub fn reload(&mut self) -> bool {
        match self.repo.reload_at_head() {
            Ok(new_repo) => {
                self.repo = new_repo;
                true
            }
            Err(_) => false,
        }
    }
}

/// Load the jj workspace and repository at the given root path.
///
/// Returns `None` on any error (graceful degradation to subprocess fallback).
/// This mirrors the loading pattern from jj's CLI (`cli/src/cli_util.rs`),
/// but simplified: we don't snapshot the working copy, resolve operation
/// conflicts, or import git refs — we just need a read-only view.
pub fn load_repo(repo_root: &Path) -> Option<LoadedRepo> {
    // Build minimal config. We need UserSettings for Workspace::load(),
    // but we don't need the full jj CLI config stack. An empty config
    // with a Default source layer is sufficient for read-only access.
    let config = build_minimal_config(repo_root)?;
    let settings = UserSettings::from_config(config).ok()?;

    let store_factories = StoreFactories::default();
    let working_copy_factories = default_working_copy_factories();

    let workspace =
        Workspace::load(&settings, repo_root, &store_factories, &working_copy_factories).ok()?;

    // Load at the latest operation (resolve concurrent op heads by picking
    // the first one — we only need a read-only snapshot, not perfect merging).
    let loader = workspace.repo_loader();

    // Use a simple error type that satisfies the trait bounds required by
    // resolve_op_heads. We just need to pick one op head.
    let op = op_heads_store::resolve_op_heads(
        loader.op_heads_store().as_ref(),
        loader.op_store(),
        |mut op_heads: Vec<jj_lib::operation::Operation>| -> Result<_, OpResolveError> {
            Ok(op_heads.pop().unwrap())
        },
    )
    .ok()?;

    let repo = loader.load_at(&op).ok()?;

    Some(LoadedRepo { workspace, repo })
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

/// Build a minimal StackedConfig for workspace loading.
///
/// jj-lib requires UserSettings (which requires StackedConfig) to load a
/// workspace. We build the minimum viable config by loading the repo-level
/// config from `.jj/repo/config.toml` (which may contain `revset-aliases`
/// like `trunk()`) and providing defaults for everything else.
///
/// UserSettings::from_config() requires these keys to exist:
/// - `user.name`, `user.email` (identity)
/// - `operation.hostname`, `operation.username` (operation metadata)
/// - `signing.behavior` (commit signing policy)
///
/// The jj CLI normally injects these via its own default config layer.
/// Since we're not using the CLI's config stack, we provide placeholder
/// defaults. These values are only used for read-only operations — we
/// never create commits or operations via jj-lib.
fn build_minimal_config(repo_root: &Path) -> Option<StackedConfig> {
    // Start with jj-lib's built-in defaults (config/misc.toml), which provides
    // all required keys: user.name, user.email, operation.hostname,
    // operation.username, signing.behavior, merge.hunk-level, etc.
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

    // Load user config (~/.jjconfig.toml or XDG equivalent) for user-level
    // revset-aliases and other settings
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
// Revset evaluation helpers
// ---------------------------------------------------------------------------

/// Evaluate a revset string and return matching commits in topological order
/// (children before parents, i.e. reversed for stack display).
fn evaluate_revset(
    repo: &Arc<ReadonlyRepo>,
    workspace: &Workspace,
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

/// Evaluate a revset and return commits in reversed order (parents before
/// children — stack order with 01 closest to trunk).
fn evaluate_revset_reversed(
    repo: &Arc<ReadonlyRepo>,
    workspace: &Workspace,
    revset_str: &str,
) -> Option<Vec<Commit>> {
    let mut commits = evaluate_revset(repo, workspace, revset_str)?;
    commits.reverse();
    Some(commits)
}

/// Load revset aliases from the repo's config.
///
/// This includes the built-in `trunk()` alias that jj defines. Since jj-lib
/// doesn't include CLI-level built-in aliases, we define the standard `trunk()`
/// default here if the user hasn't configured one.
fn load_revset_aliases(repo: &Arc<ReadonlyRepo>) -> RevsetAliasesMap {
    let mut aliases = RevsetAliasesMap::new();

    // Define the standard trunk() alias that jj CLI provides as a built-in.
    // Users can override this in their config, but we need a default.
    // This matches jj's built-in: latest((present(main) | present(master) | root()) & remote_bookmarks())
    let _ = aliases.insert(
        "trunk()",
        "latest((present(main) | present(master) | root()) & remote_bookmarks())",
    );

    // Load user-configured aliases from the repo settings
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
// Stack resolution via jj-lib (replaces subprocess-based stack.rs functions)
// ---------------------------------------------------------------------------

/// Resolve the stack base using jj-lib in-process reads.
///
/// Equivalent to `stack::resolve_stack_base()` but without subprocess calls.
/// Uses the same fallback chain:
/// 1. `stack` / `stack/*` bookmarks — nearest ancestor of `@` (inclusive)
/// 2. `trunk()` — exclusive
/// 3. None
pub fn resolve_stack_base_lib(loaded: &LoadedRepo) -> Option<StackBase> {
    let repo = &loaded.repo;
    let workspace = &loaded.workspace;

    // 1. stack / stack/* bookmarks — nearest ancestor of @ (inclusive)
    let revset_str = r#"heads((bookmarks(exact:"stack") | bookmarks(glob:"stack/*")) & ::@)"#;
    if let Some(commits) = evaluate_revset(repo, workspace, revset_str) {
        if commits.len() == 1 {
            let change_id = short_change_id(repo, &commits[0]);
            return Some(StackBase::Inclusive(change_id));
        }
        if commits.len() > 1 {
            let ids: Vec<String> = commits
                .iter()
                .map(|c| short_change_id(repo, c))
                .collect();
            return Some(StackBase::Ambiguous(ids));
        }
    }

    // 2. trunk() — exclusive (if it resolves to something other than root)
    let revset_str = "trunk() & ~root()";
    if let Some(commits) = evaluate_revset(repo, workspace, revset_str) {
        if !commits.is_empty() {
            return Some(StackBase::Exclusive);
        }
    }

    // 3. No usable base
    None
}

/// Resolve the ordered list of stack changes using jj-lib.
///
/// Equivalent to `stack::resolve_stack_changes()` but without subprocess calls.
pub fn resolve_stack_changes_lib(
    loaded: &LoadedRepo,
    base: &StackBase,
) -> Option<Vec<StackChange>> {
    let revset_str = build_stack_revset(base)?;
    batch_read_changes_lib(loaded, &revset_str)
}

/// Build the revset string for the full stack range given a resolved base.
/// (Same logic as `stack::build_stack_revset`)
fn build_stack_revset(base: &StackBase) -> Option<String> {
    match base {
        StackBase::Inclusive(change_id) => Some(format!("({}::@) | descendants(@)", change_id)),
        StackBase::Exclusive => Some("(trunk()..@) | descendants(@)".to_string()),
        StackBase::Ambiguous(_) => None,
    }
}

/// Read changes matching a revset using jj-lib, returning them in stack order.
///
/// Equivalent to `stack::batch_read_changes()` but in-process.
pub fn batch_read_changes_lib(
    loaded: &LoadedRepo,
    revset_str: &str,
) -> Option<Vec<StackChange>> {
    let repo = &loaded.repo;
    let workspace = &loaded.workspace;

    // Evaluate revset in reversed order (parents first = stack order)
    let commits = evaluate_revset_reversed(repo, workspace, revset_str)?;
    if commits.is_empty() {
        return None;
    }

    // Get the working copy commit ID for comparison
    let wc_commit_id = repo
        .view()
        .get_wc_commit_id(workspace.workspace_name())
        .cloned();

    let mut changes = Vec::new();
    for commit in &commits {
        let change_id = short_change_id(repo, commit);
        let description = commit.description().to_owned();
        // Strip trailing newline (jj appends one to descriptions)
        let description = description
            .strip_suffix('\n')
            .unwrap_or(&description)
            .to_string();

        let is_empty = commit_is_empty(repo, commit);
        let is_working_copy = wc_commit_id.as_ref() == Some(commit.id());

        let bookmarks = commit_bookmarks(repo, commit);

        changes.push(StackChange {
            change_id,
            description,
            is_empty,
            is_working_copy,
            bookmarks,
        });
    }

    if changes.is_empty() {
        None
    } else {
        Some(changes)
    }
}

/// Read changes for specific change IDs using jj-lib.
///
/// Equivalent to `stack::batch_read_by_ids()` but in-process.
pub fn batch_read_by_ids_lib(
    loaded: &LoadedRepo,
    change_ids: &[&str],
) -> Option<Vec<StackChange>> {
    if change_ids.is_empty() {
        return None;
    }
    let revset_str = change_ids.join(" | ");
    batch_read_changes_lib(loaded, &revset_str)
}

// ---------------------------------------------------------------------------
// Commit helpers
// ---------------------------------------------------------------------------

/// Get the shortest unique change ID prefix for a commit (8+ chars).
///
/// Uses the repo's index for shortest-prefix computation.
/// Returns the reverse-hex encoding (k-z alphabet) that jj uses for display
/// and revset resolution. Using standard hex (a-f) would cause revset
/// resolution failures since jj interprets those as commit ID prefixes.
fn short_change_id(repo: &Arc<ReadonlyRepo>, commit: &Commit) -> String {
    let change_id = commit.change_id();
    // Use the index to find the shortest unique prefix
    let prefix_len = repo
        .shortest_unique_change_id_prefix_len(change_id)
        .unwrap_or(8)
        .max(8);
    // encode_reverse_hex returns the k-z alphabet encoding that jj uses
    // for change IDs (as opposed to standard hex a-f used for commit IDs).
    let reverse_hex = encode_reverse_hex(change_id.as_bytes());
    let len = prefix_len.min(reverse_hex.len());
    reverse_hex[..len].to_string()
}

/// Check whether a commit is "empty" (its tree matches its parent's merged tree).
///
/// This replicates jj's `empty` template keyword semantics.
fn commit_is_empty(repo: &Arc<ReadonlyRepo>, commit: &Commit) -> bool {
    // A commit is empty if its tree is the same as the auto-merged parent tree.
    // parent_tree() is sync in jj-lib 0.38 (internally calls .block_on()).
    match commit.parent_tree(repo.as_ref()) {
        Ok(parent_tree) => commit.tree_ids() == parent_tree.tree_ids(),
        Err(_) => false, // On error, assume not empty
    }
}

/// Get the list of local bookmark names pointing to this commit.
fn commit_bookmarks(repo: &Arc<ReadonlyRepo>, commit: &Commit) -> Vec<String> {
    repo.view()
        .local_bookmarks_for_commit(commit.id())
        .map(|(name, _target)| name.as_str().to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// Flush support: read descriptions for plan file comparison
// ---------------------------------------------------------------------------

/// Gather jj descriptions for a set of change IDs, for use by flush.
///
/// Returns a HashMap of change_id → description. This replaces the
/// subprocess-based `batch_read_by_ids` call in `flush::gather_flush_state`.
pub fn gather_descriptions(loaded: &LoadedRepo, change_ids: &[&str]) -> HashMap<String, String> {
    match batch_read_by_ids_lib(loaded, change_ids) {
        Some(changes) => changes
            .into_iter()
            .map(|c| (c.change_id, c.description))
            .collect(),
        None => HashMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Single-value read helpers (replace isolated subprocess reads in commands)
// ---------------------------------------------------------------------------

/// Read the working copy's shortest unique change ID (reverse hex).
///
/// Replaces `new.rs::read_current_change_id()` and the inline read in
/// `stack.rs::run_stack()`.
pub fn read_change_id_at_wc(loaded: &LoadedRepo) -> Option<String> {
    let wc_commit_id = loaded
        .repo
        .view()
        .get_wc_commit_id(loaded.workspace.workspace_name())?;
    let commit = loaded.repo.store().get_commit(wc_commit_id).ok()?;
    Some(short_change_id(&loaded.repo, &commit))
}

/// Read a change's description by evaluating a revset target.
///
/// Returns the description with trailing newline stripped.
/// Replaces `done.rs::read_description()`.
pub fn read_description_at(loaded: &LoadedRepo, target: &str) -> Option<String> {
    let commits = evaluate_revset(&loaded.repo, &loaded.workspace, target)?;
    let commit = commits.first()?;
    let desc = commit.description().to_owned();
    Some(desc.strip_suffix('\n').unwrap_or(&desc).to_string())
}

/// Resolve a revset target to a shortest unique change ID (reverse hex).
///
/// Replaces `describe.rs::resolve_target_change_id()`.
pub fn resolve_change_id(loaded: &LoadedRepo, target: &str) -> Option<String> {
    let commits = evaluate_revset(&loaded.repo, &loaded.workspace, target)?;
    let commit = commits.first()?;
    Some(short_change_id(&loaded.repo, &commit))
}

/// Check whether a commit identified by a revset target exists.
///
/// Returns `true` if the revset resolves to at least one commit.
pub fn commit_exists(loaded: &LoadedRepo, target: &str) -> bool {
    evaluate_revset(&loaded.repo, &loaded.workspace, target)
        .map(|commits| !commits.is_empty())
        .unwrap_or(false)
}

/// Return the first child's change ID for a given change ID.
///
/// Evaluates `children(change_id) ~ change_id` and returns the first
/// result's shortest change ID, or `None` if no children exist.
pub fn first_child_change_id(loaded: &LoadedRepo, change_id: &str) -> Option<String> {
    let revset_str = format!("children({}) ~ {}", change_id, change_id);
    let commits = evaluate_revset(&loaded.repo, &loaded.workspace, &revset_str)?;
    let commit = commits.last()?; // reversed = parents first, so last is earliest child
    Some(short_change_id(&loaded.repo, &commit))
}

/// Snapshot bookmark state for abandon recovery.
///
/// Reads the nearest `stack`/`stack/*` bookmark ancestor of `@`, records
/// whether it is the working copy, and finds its first child.
/// Returns `None` if no stack bookmark exists in the ancestry of `@`.
///
/// Replaces `abandon.rs::snapshot_stack_bookmark()`.
pub fn snapshot_bookmark_state(loaded: &LoadedRepo) -> Option<BookmarkSnapshot> {
    let repo = &loaded.repo;
    let workspace = &loaded.workspace;

    // Find the nearest stack bookmark ancestor of @
    let revset_str = r#"heads((bookmarks(exact:"stack") | bookmarks(glob:"stack/*")) & ::@)"#;
    let commits = evaluate_revset(repo, workspace, revset_str)?;
    let commit = commits.first()?;

    let change_id = short_change_id(repo, &commit);

    // Find the specific stack bookmark name
    let bookmarks = commit_bookmarks(repo, &commit);
    let bookmark_name = bookmarks
        .iter()
        .find(|b| *b == "stack" || b.starts_with("stack/"))?
        .clone();

    // Check if this commit is the working copy
    let wc_commit_id = repo
        .view()
        .get_wc_commit_id(workspace.workspace_name());
    let was_working_copy = wc_commit_id == Some(commit.id());

    // Find first child
    let first_child = first_child_change_id(loaded, &change_id);

    Some(BookmarkSnapshot {
        _change_id: change_id,
        bookmark_name,
        was_working_copy,
        first_child,
    })
}

/// Check whether a `stack`/`stack/*` bookmark still exists in the ancestry of `@`.
///
/// Replaces `abandon.rs::stack_bookmark_survives()`.
pub fn stack_bookmark_survives(loaded: &LoadedRepo) -> bool {
    let revset_str = r#"heads((bookmarks(exact:"stack") | bookmarks(glob:"stack/*")) & ::@)"#;
    evaluate_revset(&loaded.repo, &loaded.workspace, revset_str)
        .map(|commits| !commits.is_empty())
        .unwrap_or(false)
}

/// Pre-abandon snapshot of the stack bookmark state.
///
/// Captures enough information to recover the bookmark if the abandon
/// removes the change that held it.
pub struct BookmarkSnapshot {
    /// Shortest unique change ID of the change holding the bookmark.
    pub _change_id: String,
    /// The `stack` or `stack/*` bookmark name.
    pub bookmark_name: String,
    /// True if the bookmarked change was the working copy (`@`).
    pub was_working_copy: bool,
    /// First child of the bookmarked change (if any), for recovery.
    pub first_child: Option<String>,
}