//! Persistence for PlanRegistry in `.jj/repo/jj-plan/`.
//!
//! Handles loading and saving the plan registry, including
//! jj workspace indirection (child workspaces where `.jj/repo`
//! is a file pointing to the parent workspace's repo directory).

use crate::types::{PlanRegistry, PLAN_REGISTRY_VERSION};
use std::fs;
use std::path::{Path, PathBuf};

/// Directory name for jj-plan metadata within `.jj/repo/`.
const JJ_PLAN_DIR: &str = "jj-plan";

/// Filename for the plan registry.
const REGISTRY_FILE: &str = "plans.toml";

/// Resolve the `.jj/repo` path, handling jj workspace indirection.
///
/// In jj workspaces (created via `jj workspace add`), the `.jj/repo` path
/// in child workspaces is a plain text file containing the absolute path
/// to the parent workspace's `.jj/repo` directory. We must read this file
/// and use its contents as the actual repo path.
///
/// Falls back to the original path if resolution fails.
pub fn resolve_repo_path(workspace_root: &Path) -> PathBuf {
    let repo_path = workspace_root.join(".jj").join("repo");

    // In jj workspaces, .jj/repo may be a file containing the path to the real repo
    if repo_path.is_file() {
        if let Ok(contents) = fs::read_to_string(&repo_path) {
            let target = PathBuf::from(contents.trim());
            if target.is_dir() {
                return fs::canonicalize(&target).unwrap_or(target);
            }
        }
        return repo_path;
    }

    repo_path
}

/// Get path to the jj-plan metadata directory within .jj/repo/.
fn plan_meta_dir(workspace_root: &Path) -> PathBuf {
    resolve_repo_path(workspace_root).join(JJ_PLAN_DIR)
}

/// Get path to the plan registry file.
pub fn registry_path(workspace_root: &Path) -> PathBuf {
    plan_meta_dir(workspace_root).join(REGISTRY_FILE)
}

/// Load plan registry from disk.
///
/// Returns an empty `PlanRegistry` if the file doesn't exist.
/// Prints a warning and returns empty on parse errors.
pub fn load_registry(workspace_root: &Path) -> PlanRegistry {
    let path = registry_path(workspace_root);

    if !path.exists() {
        return PlanRegistry::new();
    }

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("jj-plan: warning: failed to read {}: {}", path.display(), e);
            return PlanRegistry::new();
        }
    };

    match toml::from_str(&content) {
        Ok(registry) => registry,
        Err(e) => {
            eprintln!(
                "jj-plan: warning: failed to parse {}: {}",
                path.display(),
                e
            );
            PlanRegistry::new()
        }
    }
}

/// Save plan registry to disk.
///
/// Creates the `.jj/repo/jj-plan/` directory if it doesn't exist.
pub fn save_registry(workspace_root: &Path, registry: &PlanRegistry) {
    let dir = plan_meta_dir(workspace_root);
    let path = dir.join(REGISTRY_FILE);

    // Ensure directory exists
    if !dir.exists()
        && let Err(e) = fs::create_dir_all(&dir) {
            eprintln!(
                "jj-plan: warning: failed to create {}: {}",
                dir.display(),
                e
            );
            return;
        }

    // Serialize with version
    let mut registry_to_save = registry.clone();
    registry_to_save.version = PLAN_REGISTRY_VERSION;

    let content = match toml::to_string_pretty(&registry_to_save) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("jj-plan: warning: failed to serialize plan registry: {}", e);
            return;
        }
    };

    // Add header comment
    let content_with_header = format!(
        "# jj-plan registry\n# Auto-generated — manual edits may be overwritten\n\n{content}"
    );

    if let Err(e) = fs::write(&path, content_with_header) {
        eprintln!(
            "jj-plan: warning: failed to write {}: {}",
            path.display(),
            e
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PlannedBookmark;
    use tempfile::TempDir;

    fn setup_fake_jj_workspace() -> TempDir {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".jj").join("repo")).unwrap();
        temp
    }

    #[test]
    fn test_load_missing_returns_empty() {
        let temp = setup_fake_jj_workspace();
        let registry = load_registry(temp.path());
        assert!(registry.bookmarks.is_empty());
        assert_eq!(registry.version, PLAN_REGISTRY_VERSION);
    }

    #[test]
    fn test_save_creates_directory() {
        let temp = setup_fake_jj_workspace();
        let plan_dir = temp.path().join(".jj").join("repo").join("jj-plan");
        assert!(!plan_dir.exists());

        let registry = PlanRegistry::new();
        save_registry(temp.path(), &registry);

        assert!(plan_dir.exists());
        assert!(registry_path(temp.path()).exists());
    }

    #[test]
    fn test_roundtrip_serialization() {
        let temp = setup_fake_jj_workspace();

        let mut registry = PlanRegistry::new();
        registry.track(PlannedBookmark::new(
            "feat-auth".to_string(),
            "abc123".to_string(),
        ));
        let mut feat_db = PlannedBookmark::new(
            "feat-db".to_string(),
            "def456".to_string(),
        );
        feat_db.remote = Some("upstream".to_string());
        registry.track(feat_db);

        save_registry(temp.path(), &registry);

        let loaded = load_registry(temp.path());
        assert_eq!(loaded.bookmarks.len(), 2);
        assert_eq!(loaded.bookmarks[0].name, "feat-auth");
        assert_eq!(loaded.bookmarks[0].change_id, "abc123");
        assert!(loaded.bookmarks[0].remote.is_none());
        assert_eq!(loaded.bookmarks[1].name, "feat-db");
        assert_eq!(loaded.bookmarks[1].remote, Some("upstream".to_string()));
    }

    #[test]
    fn test_file_contains_header_comment() {
        let temp = setup_fake_jj_workspace();
        let registry = PlanRegistry::new();
        save_registry(temp.path(), &registry);

        let content = fs::read_to_string(registry_path(temp.path())).unwrap();
        assert!(content.starts_with("# jj-plan registry"));
        assert!(content.contains("Auto-generated"));
    }

    #[test]
    fn test_resolve_repo_path_regular_directory() {
        let temp = setup_fake_jj_workspace();
        let resolved = resolve_repo_path(temp.path());
        assert!(resolved.ends_with(".jj/repo"));
        assert!(resolved.exists());
    }

    #[test]
    fn test_resolve_repo_path_pointer_file() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("parent");
        let child = temp.path().join("child");

        // Create parent workspace with real .jj/repo
        let parent_repo = parent.join(".jj").join("repo");
        fs::create_dir_all(&parent_repo).unwrap();

        // Create child workspace with pointer file
        let child_jj = child.join(".jj");
        fs::create_dir_all(&child_jj).unwrap();
        fs::write(
            child_jj.join("repo"),
            parent_repo.to_string_lossy().as_ref(),
        )
        .unwrap();

        let resolved = resolve_repo_path(&child);
        let canonical_parent = fs::canonicalize(&parent_repo).unwrap();
        assert_eq!(resolved, canonical_parent);
    }
}