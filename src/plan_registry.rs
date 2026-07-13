//! Persistence for PlanRegistry in `.jj/repo/jj-plan/`.
//!
//! Path resolution (including jj workspace indirection) lives in [`crate::plan_dir`];
//! this module is persistence only.

use crate::error::{JjPlanError, Result};
use crate::plan_dir::meta_path;
use crate::types::{PlanRegistry, PLAN_REGISTRY_VERSION};
use std::fs;
use std::path::{Path, PathBuf};

/// Filename for the plan registry.
const REGISTRY_FILE: &str = "plans.toml";

/// Get path to the plan registry file.
pub fn registry_path(workspace_root: &Path) -> PathBuf {
    meta_path(workspace_root, REGISTRY_FILE)
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

/// Save plan registry to disk, creating `.jj/repo/jj-plan/` if needed.
///
/// Stamps the current format version onto `registry` **in place**, so the caller's
/// in-memory value is exactly what landed on disk and can be reused directly as the
/// post-mutation registry — no re-read required.
///
/// Returns `Err` on any failure. This used to warn and return `()`, which is what let a
/// failed write masquerade as success: `jj plan new` printed `Created plan:` and then
/// rendered an empty stack, because the registry it re-read from disk had never been
/// written. A silent save is the difference between a bug and a *confusing* bug.
pub fn save_registry(workspace_root: &Path, registry: &mut PlanRegistry) -> Result<()> {
    let path = registry_path(workspace_root);
    let dir = path.parent().expect("registry path has parent");

    if !dir.exists() {
        fs::create_dir_all(dir).map_err(|e| {
            JjPlanError::Io(std::io::Error::other(format!(
                "failed to create {}: {e}",
                dir.display()
            )))
        })?;
    }

    registry.version = PLAN_REGISTRY_VERSION;

    let content = toml::to_string_pretty(&registry)
        .map_err(|e| JjPlanError::Config(format!("failed to serialize plan registry: {e}")))?;

    let content_with_header = format!(
        "# jj-plan registry\n# Auto-generated — manual edits may be overwritten\n\n{content}"
    );

    crate::plan_file::write_atomic(&path, &content_with_header).map_err(|e| {
        JjPlanError::Io(std::io::Error::other(format!(
            "failed to write {}: {e}",
            path.display()
        )))
    })
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

        let mut registry = PlanRegistry::new();
        save_registry(temp.path(), &mut registry).unwrap();

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

        save_registry(temp.path(), &mut registry).unwrap();

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
        let mut registry = PlanRegistry::new();
        save_registry(temp.path(), &mut registry).unwrap();

        let content = fs::read_to_string(registry_path(temp.path())).unwrap();
        assert!(content.starts_with("# jj-plan registry"));
        assert!(content.contains("Auto-generated"));
    }

    /// The in-memory registry must match what landed on disk, so callers can reuse it as
    /// the post-mutation registry instead of re-reading. Context: jj:mqmkxzlv
    #[test]
    fn save_stamps_version_in_place() {
        let temp = setup_fake_jj_workspace();
        let mut registry = PlanRegistry::new();
        registry.version = 1;

        save_registry(temp.path(), &mut registry).unwrap();

        assert_eq!(registry.version, PLAN_REGISTRY_VERSION);
        assert_eq!(load_registry(temp.path()).version, registry.version);
    }

    /// A write failure must be visible to the caller, not swallowed into a warning — that
    /// is what let `jj plan new` print "Created plan:" over an unwritten registry.
    /// Reproduces the original workspace ENOTDIR: a pointer file whose target is a *file*.
    #[test]
    fn save_reports_failure_instead_of_warning() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("ws");
        let jj_dir = workspace_root.join(".jj");
        fs::create_dir_all(&jj_dir).unwrap();

        // .jj/repo points at a plain file, so `.../jj-plan` can never be created.
        // (`.jj/..` is the workspace root, so escaping to `temp/` takes two levels.)
        fs::write(temp.path().join("not-a-dir"), "").unwrap();
        fs::write(jj_dir.join("repo"), "../../not-a-dir").unwrap();

        let mut registry = PlanRegistry::new();
        assert!(save_registry(&workspace_root, &mut registry).is_err());
    }
}