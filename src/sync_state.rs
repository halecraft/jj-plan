//! Read-path drift signal for plan files.
//!
//! jj-plan propagates direct `.jj-plan/*.md` edits into jj descriptions only on
//! the next command that flushes. Read-only commands that surface descriptions
//! (`log`, `show`, `evolog`) historically `exec`'d real `jj` directly, so they
//! could render a stale description until some later command flushed.
//!
//! Opening jj-lib to diff plan files against descriptions is the expensive part
//! of a flush (milliseconds), while reading the small plan files is cheap
//! (microseconds). This module provides the cheap signal that lets the read path
//! decide whether a flush — and therefore a jj-lib open — is even needed:
//!
//! - After every sync, `wrap::sync_to_disk` records a content **digest** of the
//!   plan-file set in `.jj/repo/jj-plan/sync-state.toml` (mirrors `pr_cache`).
//! - On a gated read command, we recompute the digest from disk and compare. If
//!   it matches the recorded one, nothing drifted → pure `exec`. If it differs
//!   (or no record exists), there are unflushed edits → flush, then `exec`.
//!
//! The digest is over **file content**, so sync's unconditional `stack.md`
//! rewrite and identical-content plan-file rewrites do not perturb it — only a
//! real plan-file edit does. The sidecar is safe to delete: a missing record
//! counts as drift, costing at most one extra (correct) flush.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{JjPlanError, Result};
use crate::plan_registry::resolve_repo_path;
use crate::types::PlanRegistry;

/// Current version of the sync-state file format.
pub const SYNC_STATE_VERSION: u32 = 1;

/// Filename for the sync-state sidecar.
const SYNC_STATE_FILE: &str = "sync-state.toml";

/// Directory name for jj-plan metadata within `.jj/repo/`.
const JJ_PLAN_DIR: &str = "jj-plan";

/// Persisted content digest of the plan-file set as of the last sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    /// File format version.
    pub version: u32,
    /// sha256 (hex) over the sorted plan-file set.
    pub digest: String,
}

impl SyncState {
    /// Construct a `SyncState` wrapping a digest at the current version.
    pub fn new(digest: impl Into<String>) -> Self {
        Self {
            version: SYNC_STATE_VERSION,
            digest: digest.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Pure: digest + drift decision
// ---------------------------------------------------------------------------

/// Compute a content digest over a set of plan files.
///
/// Pure. Sorts the `(filename, content)` pairs by filename so the result is
/// order-independent, then folds each pair into SHA-256 with a length-prefixed,
/// NUL-delimited framing so distinct file sets cannot collide by concatenation.
/// Returns the lowercase hex digest.
pub fn compute_digest(files: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    for (name, content) in sorted {
        hasher.update(name.as_bytes());
        hasher.update([0u8]);
        hasher.update((content.len() as u64).to_le_bytes());
        hasher.update(content.as_bytes());
        hasher.update([0u8]);
    }
    format!("{:x}", hasher.finalize())
}

/// Decide whether plan files have drifted from the last recorded sync.
///
/// Pure. A missing baseline (`None`) counts as drift — conservatively forcing a
/// flush on first run / after the sidecar is deleted. Otherwise compares digests.
pub fn is_drifted(current_digest: &str, stored: Option<&SyncState>) -> bool {
    match stored {
        Some(state) => state.digest != current_digest,
        None => true,
    }
}

// ---------------------------------------------------------------------------
// Imperative shell: gather plan-file contents
// ---------------------------------------------------------------------------

/// Read the plan-file set (the same files `flush` operates on) into
/// `(filename, content)` pairs for digesting.
///
/// Imperative. Uses `collect_plan_files`, so non-plan files (`stack.md`,
/// `current.md`, `error.md`) are excluded — they don't parse as `L-NN-…` plan
/// filenames. Files that can't be read are skipped (treated as absent).
pub fn gather_plan_file_contents(plan_dir: &Path, registry: &PlanRegistry) -> Vec<(String, String)> {
    crate::plan_file::collect_plan_files(plan_dir, registry)
        .into_iter()
        .filter_map(|entry| {
            fs::read_to_string(&entry.path)
                .ok()
                .map(|content| (entry.filename, content))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Imperative shell: persistence (mirrors pr_cache.rs)
// ---------------------------------------------------------------------------

/// Path to the sync-state file (`.jj/repo/jj-plan/sync-state.toml`).
pub fn sync_state_path(workspace_root: &Path) -> PathBuf {
    resolve_repo_path(workspace_root)
        .join(JJ_PLAN_DIR)
        .join(SYNC_STATE_FILE)
}

/// Load the recorded sync state, or `None` if missing or unparseable.
///
/// Unparseable is treated as missing (returns `None`) rather than erroring —
/// the sidecar is a best-effort cache, and a corrupt one should simply force a
/// flush, not break a read command.
pub fn load_sync_state(workspace_root: &Path) -> Option<SyncState> {
    let path = sync_state_path(workspace_root);
    let content = fs::read_to_string(&path).ok()?;
    toml::from_str(&content).ok()
}

/// Persist the sync state, creating `.jj/repo/jj-plan/` if needed.
pub fn save_sync_state(workspace_root: &Path, state: &SyncState) -> Result<()> {
    let path = sync_state_path(workspace_root);
    let dir = path.parent().expect("sync-state path has parent");

    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }

    let mut to_save = state.clone();
    to_save.version = SYNC_STATE_VERSION;

    let body = toml::to_string_pretty(&to_save)
        .map_err(|e| JjPlanError::Config(format!("failed to serialize sync state: {e}")))?;

    let content = format!(
        "# Plan-file drift cache — content digest of the plan-file set at last sync.\n\
         # Safe to delete; a missing/stale entry just forces one extra flush.\n\n{body}"
    );

    fs::write(&path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn files(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, c)| (n.to_string(), c.to_string()))
            .collect()
    }

    // -- compute_digest --

    #[test]
    fn digest_is_deterministic() {
        let f = files(&[("a-01-x.md", "hello"), ("b-02-y.md", "world")]);
        assert_eq!(compute_digest(&f), compute_digest(&f));
    }

    #[test]
    fn digest_is_order_independent() {
        let a = files(&[("a-01-x.md", "hello"), ("b-02-y.md", "world")]);
        let b = files(&[("b-02-y.md", "world"), ("a-01-x.md", "hello")]);
        assert_eq!(compute_digest(&a), compute_digest(&b));
    }

    #[test]
    fn digest_empty_set_is_stable() {
        assert_eq!(compute_digest(&[]), compute_digest(&[]));
    }

    #[test]
    fn digest_flips_on_content_change() {
        let a = files(&[("a-01-x.md", "hello")]);
        let b = files(&[("a-01-x.md", "hello!")]);
        assert_ne!(compute_digest(&a), compute_digest(&b));
    }

    #[test]
    fn digest_flips_on_filename_change() {
        let a = files(&[("a-01-x.md", "hello")]);
        let b = files(&[("a-01-z.md", "hello")]);
        assert_ne!(compute_digest(&a), compute_digest(&b));
    }

    #[test]
    fn digest_no_concatenation_collision() {
        // ("ab","c") vs ("a","bc") must not collide thanks to length-prefix framing.
        let a = files(&[("ab", "c")]);
        let b = files(&[("a", "bc")]);
        assert_ne!(compute_digest(&a), compute_digest(&b));
    }

    // -- is_drifted --

    #[test]
    fn drift_when_no_baseline() {
        assert!(is_drifted("anything", None));
    }

    #[test]
    fn no_drift_when_digests_match() {
        let state = SyncState::new("abc123");
        assert!(!is_drifted("abc123", Some(&state)));
    }

    #[test]
    fn drift_when_digests_differ() {
        let state = SyncState::new("abc123");
        assert!(is_drifted("def456", Some(&state)));
    }

    // -- persistence (mirrors pr_cache tests) --

    fn fake_workspace() -> TempDir {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".jj").join("repo")).unwrap();
        temp
    }

    #[test]
    fn path_is_under_jj_repo() {
        let temp = fake_workspace();
        let path = sync_state_path(temp.path());
        assert!(path.ends_with(".jj/repo/jj-plan/sync-state.toml"));
    }

    #[test]
    fn load_missing_returns_none() {
        let temp = fake_workspace();
        assert!(load_sync_state(temp.path()).is_none());
    }

    #[test]
    fn save_load_roundtrip() {
        let temp = fake_workspace();
        let state = SyncState::new("deadbeef");
        save_sync_state(temp.path(), &state).unwrap();

        let loaded = load_sync_state(temp.path()).unwrap();
        assert_eq!(loaded.digest, "deadbeef");
        assert_eq!(loaded.version, SYNC_STATE_VERSION);
    }

    #[test]
    fn load_corrupt_returns_none() {
        let temp = fake_workspace();
        let path = sync_state_path(temp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not valid toml :::").unwrap();
        assert!(load_sync_state(temp.path()).is_none());
    }

    #[test]
    fn saved_file_has_safe_to_delete_header() {
        let temp = fake_workspace();
        save_sync_state(temp.path(), &SyncState::new("x")).unwrap();
        let content = fs::read_to_string(sync_state_path(temp.path())).unwrap();
        assert!(content.contains("Safe to delete"));
    }
}
