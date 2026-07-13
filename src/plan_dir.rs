//! Repo root, shared-repo, and plan directory resolution.
//!
//! # Why jj-lib's `WorkspaceLoader` is mirrored here, not called
//!
//! [`resolve_repo_path`] duplicates six lines of `jj_lib::workspace::WorkspaceLoader::new`
//! rather than delegating to it. That is deliberate: this function runs on the *pre-jj-lib*
//! path. The read-path drift gate exists precisely to answer "has any plan file drifted?"
//! **without** paying `Workspace::open` (see TECHNICAL.md, "Read-path drift gate"), and it
//! must locate `sync-state.toml` to do so. Opening the workspace in order to decide whether
//! we need to open the workspace would forfeit the entire optimization on every `jj log`.
//!
//! The upstream semantics have been stable for years, and the mirror is pinned by
//! [`tests::pointer_relative_resolves_against_jj_dir`].

use std::fs;
use std::path::{Path, PathBuf};

use crate::stack_render::StackFormat;

/// Directory name for jj-plan's metadata within the shared `.jj/repo/`.
///
/// Not to be confused with the `JJ_PLAN_DIR` **env var** read by [`resolve_plan_dir`],
/// which points at the `.jj-plan/` *plan-file* directory — an unrelated concept that
/// happens to live in this same module.
const META_DIR: &str = "jj-plan";

/// Where does the shared repo dir live, given the `.jj` dir and the raw contents of a
/// `.jj/repo` pointer file?
///
/// Pure — this one expression *is* the workspace fix. In a non-default jj workspace
/// (`jj workspace add`), `.jj/repo` is a **file** whose contents are a path to the shared
/// repo dir **relative to the `.jj/` directory that contains it** — not to the process
/// CWD, and not to the workspace root. `Path::join` also absorbs the absolute case for
/// free (an absolute right-hand side replaces the left), so one expression covers both.
///
/// Context: jj:mqmkxzlv
fn resolve_pointer(jj_dir: &Path, contents: &str) -> PathBuf {
    jj_dir.join(contents.trim())
}

/// Resolve the shared repo directory (`.jj/repo`) for a workspace root.
///
/// In the default workspace `.jj/repo` is a plain directory and is returned as-is. In a
/// workspace created by `jj workspace add` it is a pointer *file*, which is resolved via
/// [`resolve_pointer`]. Mirrors `jj_lib::workspace::WorkspaceLoader::new`; see the module
/// docs for why it is mirrored rather than called.
///
/// Never returns the pointer file's own path. A dangling or unreadable pointer yields a
/// non-existent *directory* path, which `create_dir_all` can simply create; aliasing the
/// pointer *file* is what made `create_dir_all` try to mkdir inside a regular file →
/// `ENOTDIR`, which is exactly how the original workspace bug presented.
pub fn resolve_repo_path(workspace_root: &Path) -> PathBuf {
    let jj_dir = workspace_root.join(".jj");
    let repo_path = jj_dir.join("repo");

    if !repo_path.is_file() {
        return repo_path;
    }

    let target = fs::read_to_string(&repo_path)
        .map(|contents| resolve_pointer(&jj_dir, &contents))
        // An unreadable pointer still must not resolve to the pointer file itself.
        .unwrap_or_else(|_| resolve_pointer(&jj_dir, ""));

    fs::canonicalize(&target).unwrap_or(target)
}

/// Path to a jj-plan metadata file in the shared repo dir: `.jj/repo/jj-plan/<file>`.
///
/// The single place this path is constructed. `plans.toml`, `pr-cache.toml`, and
/// `sync-state.toml` all route through here, as does anything added later — the metadata
/// path used to be hand-built in four places, and the one that drifted out of sync
/// (`workspace.rs`) is what broke jj workspaces.
pub fn meta_path(workspace_root: &Path, file: &str) -> PathBuf {
    resolve_repo_path(workspace_root).join(META_DIR).join(file)
}

/// Discover the jj repo root by walking up from an arbitrary starting path
/// looking for `.jj/`.
///
/// This replaces the `jj root` subprocess call (~15ms) with a pure
/// filesystem walk (~0ms). Mirrors the logic in jj's own CLI:
/// `cli/src/cli_util.rs::find_workspace_dir()`.
///
/// Returns `Some(path)` where `path` is the directory containing `.jj/`,
/// or `None` if no `.jj/` directory is found in any ancestor.
pub fn find_repo_root_from(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|path| path.join(".jj").is_dir())
        .map(|p| p.to_path_buf())
}

/// Discover the jj repo root by walking up from the current working
/// directory looking for `.jj/`.
pub fn find_repo_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    find_repo_root_from(&cwd)
}

/// How the plan directory was resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanDirSource {
    /// Resolved from the `JJ_PLAN_DIR` environment variable.
    EnvVar,
    /// Resolved from `.jj-plan/` in the repo root.
    JjPlan,
    /// Resolved from `.jj-plans/` in the repo root (legacy fallback).
    JjPlansLegacy,
}

impl std::fmt::Display for PlanDirSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnvVar => write!(f, "env var"),
            Self::JjPlan => write!(f, ".jj-plan"),
            Self::JjPlansLegacy => write!(f, ".jj-plans (legacy)"),
        }
    }
}

/// Resolved plan directory and its resolution source.
#[derive(Debug, Clone)]
pub struct PlanDir {
    pub path: PathBuf,
    pub source: PlanDirSource,
}

impl PlanDir {
    /// The directory name (last component), used in display strings like
    /// `Plan stack (.jj-plan/; ...)`.
    pub fn dir_name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(".jj-plan")
    }
}

/// Resolve the plan directory using the standard fallback chain:
///
/// 1. `JJ_PLAN_DIR` env var — if set, use as-is (no fallback)
/// 2. `.jj-plan/` in repo root — preferred default
/// 3. `.jj-plans/` in repo root — legacy fallback
/// 4. None — not activated
///
/// `repo_root` may be `None` if we're not in a jj repo. In that case,
/// only the env var path is checked.
pub fn resolve_plan_dir(repo_root: Option<&Path>) -> Option<PlanDir> {
    // 1. JJ_PLAN_DIR env var
    if let Ok(env_dir) = std::env::var("JJ_PLAN_DIR")
        && !env_dir.is_empty() {
            let path = PathBuf::from(&env_dir);
            // The env var is used as-is — no existence check, no fallback.
            // This matches the zsh shim behavior where the env var is
            // trusted unconditionally.
            return Some(PlanDir {
                path,
                source: PlanDirSource::EnvVar,
            });
        }

    let repo_root = repo_root?;

    // 2. .jj-plan/ in repo root
    let jj_plan = repo_root.join(".jj-plan");
    if jj_plan.is_dir() {
        return Some(PlanDir {
            path: jj_plan,
            source: PlanDirSource::JjPlan,
        });
    }

    // 3. .jj-plans/ in repo root (legacy fallback)
    let jj_plans = repo_root.join(".jj-plans");
    if jj_plans.is_dir() {
        return Some(PlanDir {
            path: jj_plans,
            source: PlanDirSource::JjPlansLegacy,
        });
    }

    // 4. Not activated
    None
}

/// Read JJ_PLAN_STACK_PREFIX from the environment, defaulting to `stack/`.
///
/// This prefix is used for stack base bookmarks that mark explicit
/// boundaries between logical stacks within a linear chain.
pub fn stack_prefix() -> String {
    std::env::var("JJ_PLAN_STACK_PREFIX")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "stack/".to_string())
}

/// Read `JJ_PLAN_STACK_FORMAT` from the environment.
///
/// Returns `Compact` when unset or unrecognized (the terminal default).
/// Returns `Regular` when set to `"regular"`.
///
/// This is the GATHER-phase reader — called once at the shell boundary
/// in `main.rs`, then threaded as data through the call chain.
pub fn resolved_stack_format() -> StackFormat {
    match std::env::var("JJ_PLAN_STACK_FORMAT").ok().as_deref() {
        Some("regular") => StackFormat::Regular,
        _ => StackFormat::Compact,
    }
}

/// Read JJ_PLAN_MAX from the environment, defaulting to 50.
pub fn plan_max() -> usize {
    std::env::var("JJ_PLAN_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50)
}

/// Configurable status indicator strings for phase/task parsing.
///
/// Each field defaults to the standard emoji but can be overridden via
/// `JJ_PLAN_STATUS_*` env vars. Used by `jj plan summary` to identify
/// phase and task statuses in plan headings and list items.
pub struct StatusIndicators {
    pub done: String,
    pub wip: String,
    pub todo: String,
    pub blocked: String,
}

/// Read `JJ_PLAN_STATUS_*` env vars, falling back to default emojis.
///
/// | Env var | Default |
/// |---|---|
/// | `JJ_PLAN_STATUS_DONE` | `✅` |
/// | `JJ_PLAN_STATUS_WIP` | `🟡` |
/// | `JJ_PLAN_STATUS_TODO` | `🔴` |
/// | `JJ_PLAN_STATUS_BLOCKED` | `⛔` |
pub fn resolve_status_indicators() -> StatusIndicators {
    fn env_or(var: &str, default: &str) -> String {
        std::env::var(var)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    }
    StatusIndicators {
        done: env_or("JJ_PLAN_STATUS_DONE", "✅"),
        wip: env_or("JJ_PLAN_STATUS_WIP", "🟡"),
        todo: env_or("JJ_PLAN_STATUS_TODO", "🔴"),
        blocked: env_or("JJ_PLAN_STATUS_BLOCKED", "⛔"),
    }
}

impl StatusIndicators {
    /// Return all indicator strings as a slice for iteration.
    pub fn all(&self) -> [&str; 4] {
        [&self.done, &self.wip, &self.todo, &self.blocked]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn resolve_jj_plan_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".jj-plan")).unwrap();

        let result = resolve_plan_dir(Some(root));
        assert!(result.is_some());
        let plan_dir = result.unwrap();
        assert_eq!(plan_dir.source, PlanDirSource::JjPlan);
        assert_eq!(plan_dir.path, root.join(".jj-plan"));
    }

    #[test]
    fn resolve_legacy_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".jj-plans")).unwrap();

        let result = resolve_plan_dir(Some(root));
        assert!(result.is_some());
        let plan_dir = result.unwrap();
        assert_eq!(plan_dir.source, PlanDirSource::JjPlansLegacy);
        assert_eq!(plan_dir.path, root.join(".jj-plans"));
    }

    #[test]
    fn jj_plan_takes_precedence_over_legacy() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".jj-plan")).unwrap();
        fs::create_dir(root.join(".jj-plans")).unwrap();

        let result = resolve_plan_dir(Some(root));
        assert!(result.is_some());
        let plan_dir = result.unwrap();
        assert_eq!(plan_dir.source, PlanDirSource::JjPlan);
    }

    #[test]
    fn no_plan_dir_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let result = resolve_plan_dir(Some(tmp.path()));
        assert!(result.is_none());
    }

    #[test]
    fn no_repo_root_returns_none() {
        // With no env var set, no repo root means no plan dir
        // (env var tested separately since it requires env manipulation)
        let result = resolve_plan_dir(None);
        assert!(result.is_none());
    }

    #[test]
    fn plan_max_default() {
        // When JJ_PLAN_MAX is not set, default is 50
        // (Can't reliably test env override without env manipulation)
        // Just verify the function doesn't panic
        let _max = plan_max();
    }

    #[test]
    fn test_status_indicators_defaults() {
        let ind = resolve_status_indicators();
        assert_eq!(ind.done, "✅");
        assert_eq!(ind.wip, "🟡");
        assert_eq!(ind.todo, "🔴");
        assert_eq!(ind.blocked, "⛔");
        assert_eq!(ind.all().len(), 4);
    }

    #[test]
    fn ancestor_walk_finds_repo_root_from_nested_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".jj")).unwrap();
        let subdir = root.join("src").join("commands").join("deep");
        fs::create_dir_all(&subdir).unwrap();

        // The ancestor walk (same logic as find_repo_root) must find the
        // nearest ancestor containing .jj/, skipping the subdirectories
        let found = subdir
            .ancestors()
            .find(|p| p.join(".jj").is_dir())
            .map(|p| p.to_path_buf());
        assert_eq!(found, Some(root.to_path_buf()));
    }

    // --- Shared repo path resolution (jj workspace indirection) ---

    /// The bug, in its purest form: jj writes the pointer path **relative to `.jj/`**, and
    /// the old code treated it as absolute (testing `is_dir()` against the process CWD).
    /// No fixture needed — this is the whole defect. Context: jj:mqmkxzlv
    #[test]
    fn pointer_relative_resolves_against_jj_dir() {
        // Exactly what jj 0.42 writes into a workspace's .jj/repo.
        let resolved = resolve_pointer(Path::new("/ws/floor-model/.jj"), "../../../synapse/.jj/repo");
        assert_eq!(
            resolved,
            Path::new("/ws/floor-model/.jj/../../../synapse/.jj/repo")
        );
    }

    #[test]
    fn pointer_absolute_replaces_jj_dir() {
        let resolved = resolve_pointer(Path::new("/ws/b/.jj"), "/repos/main/.jj/repo");
        assert_eq!(resolved, Path::new("/repos/main/.jj/repo"));
    }

    #[test]
    fn pointer_contents_are_trimmed() {
        let resolved = resolve_pointer(Path::new("/ws/b/.jj"), "  ../a/.jj/repo\n");
        assert_eq!(resolved, Path::new("/ws/b/.jj/../a/.jj/repo"));
    }

    #[test]
    fn repo_path_regular_directory_is_returned_as_is() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".jj").join("repo")).unwrap();

        let resolved = resolve_repo_path(temp.path());
        assert!(resolved.ends_with(".jj/repo"));
        assert!(resolved.is_dir());
    }

    /// End-to-end shell over `resolve_pointer`, with the real relative layout jj produces.
    #[test]
    fn repo_path_follows_relative_pointer_file() {
        let temp = tempfile::tempdir().unwrap();
        let main_repo = temp.path().join("main").join(".jj").join("repo");
        fs::create_dir_all(&main_repo).unwrap();

        let ws_jj = temp.path().join("ws").join(".jj");
        fs::create_dir_all(&ws_jj).unwrap();
        // Relative to the .jj dir — as jj writes it.
        fs::write(ws_jj.join("repo"), "../../main/.jj/repo").unwrap();

        let resolved = resolve_repo_path(&temp.path().join("ws"));
        assert_eq!(resolved, fs::canonicalize(&main_repo).unwrap());
    }

    #[test]
    fn repo_path_follows_absolute_pointer_file() {
        let temp = tempfile::tempdir().unwrap();
        let main_repo = temp.path().join("main").join(".jj").join("repo");
        fs::create_dir_all(&main_repo).unwrap();

        let ws_jj = temp.path().join("ws").join(".jj");
        fs::create_dir_all(&ws_jj).unwrap();
        fs::write(ws_jj.join("repo"), main_repo.to_string_lossy().as_ref()).unwrap();

        let resolved = resolve_repo_path(&temp.path().join("ws"));
        assert_eq!(resolved, fs::canonicalize(&main_repo).unwrap());
    }

    /// A dangling pointer must never resolve to the pointer *file* itself. That aliasing is
    /// what made `create_dir_all` try to mkdir *inside a regular file* → ENOTDIR, the
    /// original workspace crash. Resolving to a merely non-existent directory is benign:
    /// `create_dir_all` just creates it.
    #[test]
    fn dangling_pointer_never_resolves_to_the_pointer_file() {
        let temp = tempfile::tempdir().unwrap();
        let ws_jj = temp.path().join("ws").join(".jj");
        fs::create_dir_all(&ws_jj).unwrap();
        fs::write(ws_jj.join("repo"), "../../nowhere/.jj/repo").unwrap();

        let resolved = resolve_repo_path(&temp.path().join("ws"));

        assert_ne!(resolved, ws_jj.join("repo"));
        assert!(!resolved.is_file());
        // Whatever it is, a caller can create under it — no ENOTDIR.
        fs::create_dir_all(resolved.join("jj-plan")).unwrap();
    }

    /// An unreadable pointer must not alias the pointer file either.
    #[test]
    fn empty_pointer_never_resolves_to_the_pointer_file() {
        let temp = tempfile::tempdir().unwrap();
        let ws_jj = temp.path().join("ws").join(".jj");
        fs::create_dir_all(&ws_jj).unwrap();
        fs::write(ws_jj.join("repo"), "").unwrap();

        let resolved = resolve_repo_path(&temp.path().join("ws"));
        assert!(!resolved.is_file());
    }

    #[test]
    fn meta_path_composes_through_the_pointer() {
        let temp = tempfile::tempdir().unwrap();
        let main_repo = temp.path().join("main").join(".jj").join("repo");
        fs::create_dir_all(&main_repo).unwrap();

        let ws_jj = temp.path().join("ws").join(".jj");
        fs::create_dir_all(&ws_jj).unwrap();
        fs::write(ws_jj.join("repo"), "../../main/.jj/repo").unwrap();

        let path = meta_path(&temp.path().join("ws"), "plans.toml");

        // Lands in the *shared* repo dir, not the workspace's own .jj/.
        assert_eq!(
            path,
            fs::canonicalize(&main_repo).unwrap().join("jj-plan").join("plans.toml")
        );
    }

    #[test]
    fn ancestor_walk_returns_none_when_no_jj_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let leaf = tmp.path().join("a").join("b");
        fs::create_dir_all(&leaf).unwrap();

        // No .jj/ anywhere under tmp — walk must not find one within our tree
        let found = leaf
            .ancestors()
            .take_while(|p| p.starts_with(tmp.path()))
            .find(|p| p.join(".jj").is_dir());
        assert!(found.is_none());
    }
}