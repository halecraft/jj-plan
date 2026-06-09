//! Plan-file ↔ description reconcile: the pure 3-way decision, the per-bookmark
//! baseline, and the read-path drift signal.
//!
//! jj-plan keeps each plan in two places — the jj change **description** and an
//! editable `.jj-plan/L-NN-bookmark.md` **file**. `flush` moves file→description and
//! `sync` moves description→file. These are the two directions of one 3-way reconcile.
//!
//! [`reconcile`] is the single pure decision shared by both directions (mental model =
//! git merge-base: `file = ours`, `desc = theirs`, `base = merge base`). Each shell
//! executes only its lane and refuses to act destructively in the other's, so neither
//! side can clobber the other.
//!
//! The **baseline** is the content last confirmed equal on both sides, stored per
//! bookmark in `.jj/repo/jj-plan/sync-state.toml` (mirrors `pr_cache`). It advances only
//! via [`anchor`], which records `hash(content)` for a bookmark **only when its two sides
//! are observed equal** — so a failed flush cannot poison it.
//!
//! The same per-bookmark hashes drive the read-path drift gate: a `log`/`show`/`evolog`
//! command flushes only when [`current_file_hashes`] differs from the stored baselines
//! ([`is_drifted`]), avoiding a jj-lib open in the common (no-edit) case.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{JjPlanError, Result};
use crate::plan_file::PlanFileEntry;
use crate::plan_registry::resolve_repo_path;
use crate::types::PlanRegistry;

/// Current version of the sync-state file format.
///
/// v1 stored a single aggregate `digest`; v2 stores a per-bookmark baseline map. A v1
/// (or unknown-version) file loads as `None` — forcing one self-healing flush.
pub const SYNC_STATE_VERSION: u32 = 2;

/// Filename for the sync-state sidecar.
const SYNC_STATE_FILE: &str = "sync-state.toml";

/// Directory name for jj-plan metadata within `.jj/repo/`.
const JJ_PLAN_DIR: &str = "jj-plan";

// ---------------------------------------------------------------------------
// Pure: normalization + content hashing
// ---------------------------------------------------------------------------

/// Normalize content for comparison/hashing by stripping trailing newlines.
///
/// The read path (`read_description_at`/`gather_descriptions`) strips the trailing `\n`,
/// while editors routinely add one. Without this, a newline-only difference would read as
/// a divergence and produce a false `Conflict`. Comparing content modulo trailing newlines
/// makes such a difference `InSync`.
fn normalize(s: &str) -> &str {
    s.trim_end_matches('\n')
}

/// sha256-hex of a content string (normalized first). Pure.
///
/// Per-bookmark hashing keys on the bookmark name (the map key), so — unlike the old
/// aggregate digest — no length-prefixed framing is needed to avoid cross-file collisions.
pub fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalize(content).as_bytes());
    format!("{:x}", hasher.finalize())
}

// ---------------------------------------------------------------------------
// Pure: the 3-way reconcile decision
// ---------------------------------------------------------------------------

/// The single pure decision shared by flush (file→desc) and sync (desc→file).
///
/// Each shell executes only its lane:
/// - flush pushes on `FileToDesc`; leaves the description alone on everything else.
/// - sync writes on `DescToFile`; preserves the file on everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconcile {
    /// `file == desc` — already equal; nobody acts, baseline records agreement.
    InSync,
    /// Only the file changed — flush pushes file→desc; sync preserves the file.
    FileToDesc,
    /// Only the description changed (or the file is absent) — sync writes desc→file;
    /// flush leaves the description.
    DescToFile,
    /// Both sides diverged from a known base — preserve the file, surface the incoming
    /// description; neither shell overwrites.
    Conflict,
}

/// Pure 3-way decision. `base_hash` = `hash_content` of the content last confirmed equal on
/// both sides (`None` = no baseline yet); the baseline is stored as a hash, not content, so
/// "did this side change?" is `hash_content(side) != base_hash`. Two principles govern the
/// edges:
///
/// - **An empty side never overwrites a non-empty side** (emptiness = "no authoritative
///   content").
/// - **Recoverability bias for ambiguity.** A description overwrite is recoverable via the
///   jj oplog; a plan-file overwrite is not. So when attribution is impossible
///   (`base = None`), bias to `FileToDesc`, never `DescToFile`. `Conflict` therefore arises
///   only from a *known* base with genuine divergence — never from `base = None`.
///
/// The safety invariant: no result writes the description over a non-empty file unless the
/// file is confirmed clean (`hash == base_hash`), so unflushed file edits are never lost.
pub fn reconcile(file: Option<&str>, desc: &str, base_hash: Option<&str>) -> Reconcile {
    // Rule 1: no file yet → materialize from the description.
    let file = match file {
        None => return Reconcile::DescToFile,
        Some(f) => normalize(f),
    };
    let desc = normalize(desc);

    // Rule 2: already equal (covers equal, convergent-both-changed, both-empty).
    if file == desc {
        return Reconcile::InSync;
    }

    // Rule 3: exactly one side empty → act toward the non-empty side.
    let file_empty = file.is_empty();
    let desc_empty = desc.is_empty();
    if file_empty != desc_empty {
        return if file_empty {
            Reconcile::DescToFile // empty file → restore from description
        } else {
            Reconcile::FileToDesc // empty description → push the file (never lose it)
        };
    }

    // Both non-empty and differing.
    match base_hash {
        Some(base_hash) => {
            let file_changed = hash_content(file) != base_hash;
            let desc_changed = hash_content(desc) != base_hash;
            match (file_changed, desc_changed) {
                (true, false) => Reconcile::FileToDesc, // only file changed
                (false, true) => Reconcile::DescToFile, // only description changed
                (true, true) => Reconcile::Conflict,    // both diverged (file != desc)
                // (false, false) ⇒ file == base == desc ⇒ file == desc, handled by Rule 2.
                (false, false) => Reconcile::InSync,
            }
        }
        // Rule 5: no baseline → bias to the recoverable direction.
        None => Reconcile::FileToDesc,
    }
}

// ---------------------------------------------------------------------------
// Persisted per-bookmark baseline
// ---------------------------------------------------------------------------

/// Per-bookmark content baselines as of the last confirmed sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    /// File format version.
    pub version: u32,
    /// bookmark name → sha256-hex of the content confirmed equal on BOTH sides.
    #[serde(default)]
    pub baselines: BTreeMap<String, String>,
}

impl SyncState {
    /// Construct a `SyncState` wrapping a baseline map at the current version.
    pub fn new(baselines: BTreeMap<String, String>) -> Self {
        Self {
            version: SYNC_STATE_VERSION,
            baselines,
        }
    }
}

/// Advance baselines: for each observation, set `bm → hash(file)` **only when the two
/// sides are equal** (normalized); otherwise keep the previous baseline. Pure.
///
/// This is the single place baseline poisoning is prevented — it observes equality rather
/// than trusting any "flush succeeded" return. `observed` is sourced from executed
/// outcomes (a `Write` yields `(desc, desc)`; a gate pairs the re-read description with the
/// file it already gathered), never from a fresh disk scan.
pub fn anchor(
    prev: &BTreeMap<String, String>,
    observed: &[(String, Option<String>, String)],
) -> BTreeMap<String, String> {
    let mut next = prev.clone();
    for (bookmark, file, desc) in observed {
        if let Some(file) = file
            && normalize(file) == normalize(desc)
        {
            next.insert(bookmark.clone(), hash_content(file));
        }
    }
    next
}

/// Decide whether plan files have drifted from the last confirmed baseline.
///
/// Pure. A missing/unparseable/old-version baseline (`None`) counts as drift —
/// conservatively forcing one flush. Otherwise compares the current per-bookmark file
/// hashes against the stored baselines.
pub fn is_drifted(current_file_hashes: &BTreeMap<String, String>, stored: Option<&SyncState>) -> bool {
    match stored {
        Some(state) => &state.baselines != current_file_hashes,
        None => true,
    }
}

// ---------------------------------------------------------------------------
// Imperative shell: gather plan-file contents
// ---------------------------------------------------------------------------

/// Read the plan-file set once into `(entry, content)` pairs — the single content read
/// shared by flush, sync, and the drift gate.
///
/// Imperative. Uses `collect_plan_files`, so non-plan files (`stack.md`, `error.md`,
/// `.history/`, `*.incoming`, `*.orphan`) are excluded. Files that can't be read are
/// skipped (treated as absent).
pub fn read_plan_contents(plan_dir: &Path, registry: &PlanRegistry) -> Vec<(PlanFileEntry, String)> {
    crate::plan_file::collect_plan_files(plan_dir, registry)
        .into_iter()
        .filter_map(|entry| fs::read_to_string(&entry.path).ok().map(|content| (entry, content)))
        .collect()
}

/// Current per-bookmark content hashes (the *drift input* — hashes of files as they are
/// now, keyed by bookmark name). Distinct from the stored `baselines` (the *confirmed*
/// state); `is_drifted` compares the two.
pub fn current_file_hashes(plan_dir: &Path, registry: &PlanRegistry) -> BTreeMap<String, String> {
    read_plan_contents(plan_dir, registry)
        .into_iter()
        .map(|(entry, content)| (entry.bookmark_name, hash_content(&content)))
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

/// Load the recorded sync state, or `None` if missing, unparseable, or an old version.
///
/// A v1 (aggregate-digest) file has no `version = 2`, so it deserializes to a `version`
/// that isn't 2 → treated as `None` (one self-healing flush). Unparseable is also `None`.
pub fn load_sync_state(workspace_root: &Path) -> Option<SyncState> {
    let path = sync_state_path(workspace_root);
    let content = fs::read_to_string(&path).ok()?;
    let state: SyncState = toml::from_str(&content).ok()?;
    if state.version != SYNC_STATE_VERSION {
        return None;
    }
    Some(state)
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
        "# Plan-file reconcile baselines — per-bookmark hash of the content confirmed equal\n\
         # on both sides at last sync. Safe to delete; a missing entry just forces one flush.\n\n{body}"
    );

    fs::write(&path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn obs(items: &[(&str, Option<&str>, &str)]) -> Vec<(String, Option<String>, String)> {
        items
            .iter()
            .map(|(bm, f, d)| (bm.to_string(), f.map(|s| s.to_string()), d.to_string()))
            .collect()
    }

    // -- reconcile (ordered rules) --

    #[test]
    fn reconcile_absent_file_materializes() {
        assert_eq!(reconcile(None, "desc", Some("x")), Reconcile::DescToFile);
        assert_eq!(reconcile(None, "", None), Reconcile::DescToFile);
    }

    #[test]
    fn reconcile_equal_is_insync() {
        assert_eq!(reconcile(Some("same"), "same", Some("old")), Reconcile::InSync);
    }

    #[test]
    fn reconcile_newline_only_difference_is_insync() {
        // The normalization gotcha: trailing-newline-only difference must NOT be a conflict.
        assert_eq!(reconcile(Some("plan\n"), "plan", Some("plan")), Reconcile::InSync);
        assert_eq!(reconcile(Some("plan"), "plan\n\n", Some("base")), Reconcile::InSync);
    }

    #[test]
    fn reconcile_convergent_both_changed_to_same_is_insync() {
        assert_eq!(reconcile(Some("X"), "X", Some("old")), Reconcile::InSync);
    }

    #[test]
    fn reconcile_only_file_changed_pushes() {
        let base = hash_content("old");
        assert_eq!(reconcile(Some("new"), "old", Some(&base)), Reconcile::FileToDesc);
    }

    #[test]
    fn reconcile_only_desc_changed_writes_file() {
        // The mirror direction: a stale file must not overwrite a changed description.
        let base = hash_content("old");
        assert_eq!(reconcile(Some("old"), "new", Some(&base)), Reconcile::DescToFile);
    }

    #[test]
    fn reconcile_both_diverged_known_base_is_conflict() {
        let base = hash_content("base");
        assert_eq!(reconcile(Some("mine"), "theirs", Some(&base)), Reconcile::Conflict);
    }

    #[test]
    fn reconcile_empty_desc_nonempty_file_pushes() {
        // The original bug's fix direction: empty description never overwrites a file.
        assert_eq!(reconcile(Some("RICH"), "", Some("")), Reconcile::FileToDesc);
        assert_eq!(reconcile(Some("RICH"), "", None), Reconcile::FileToDesc);
    }

    #[test]
    fn reconcile_empty_file_nonempty_desc_restores() {
        // An emptied file never overwrites a non-empty description.
        assert_eq!(reconcile(Some(""), "RICH", Some("RICH")), Reconcile::DescToFile);
        assert_eq!(reconcile(Some(""), "RICH", None), Reconcile::DescToFile);
    }

    #[test]
    fn reconcile_base_none_both_nonempty_differ_biases_to_file() {
        // Recoverable bias — never Conflict on a missing baseline.
        assert_eq!(reconcile(Some("file"), "desc", None), Reconcile::FileToDesc);
    }

    // -- anchor (poison-proof) --

    #[test]
    fn anchor_advances_only_when_equal() {
        let prev = map(&[("a", "old-a"), ("b", "old-b")]);
        // a: sides equal → advance to hash; b: sides differ (failed flush) → keep old.
        let observed = obs(&[("a", Some("X"), "X"), ("b", Some("RICH"), "")]);
        let next = anchor(&prev, &observed);
        assert_eq!(next.get("a"), Some(&hash_content("X")));
        assert_eq!(next.get("b"), Some(&"old-b".to_string()), "failed-flush bookmark keeps prior baseline (no poisoning)");
    }

    #[test]
    fn anchor_ignores_newline_only_for_equality() {
        let next = anchor(&BTreeMap::new(), &obs(&[("a", Some("p\n"), "p")]));
        assert_eq!(next.get("a"), Some(&hash_content("p")));
    }

    #[test]
    fn anchor_absent_file_does_not_advance() {
        let prev = map(&[("a", "old")]);
        let next = anchor(&prev, &[("a".to_string(), None, "desc".to_string())]);
        assert_eq!(next.get("a"), Some(&"old".to_string()));
    }

    // -- is_drifted --

    #[test]
    fn drift_when_no_baseline() {
        assert!(is_drifted(&map(&[("a", "h")]), None));
    }

    #[test]
    fn no_drift_when_hashes_match_baselines() {
        let state = SyncState::new(map(&[("a", "h1"), ("b", "h2")]));
        assert!(!is_drifted(&map(&[("a", "h1"), ("b", "h2")]), Some(&state)));
    }

    #[test]
    fn drift_when_a_hash_differs() {
        let state = SyncState::new(map(&[("a", "h1")]));
        assert!(is_drifted(&map(&[("a", "CHANGED")]), Some(&state)));
    }

    #[test]
    fn drift_when_a_bookmark_appears() {
        let state = SyncState::new(map(&[("a", "h1")]));
        assert!(is_drifted(&map(&[("a", "h1"), ("b", "h2")]), Some(&state)));
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
        assert!(sync_state_path(temp.path()).ends_with(".jj/repo/jj-plan/sync-state.toml"));
    }

    #[test]
    fn load_missing_returns_none() {
        let temp = fake_workspace();
        assert!(load_sync_state(temp.path()).is_none());
    }

    #[test]
    fn save_load_roundtrip_v2() {
        let temp = fake_workspace();
        let state = SyncState::new(map(&[("feat-auth", "deadbeef"), ("fix-login", "cafef00d")]));
        save_sync_state(temp.path(), &state).unwrap();

        let loaded = load_sync_state(temp.path()).unwrap();
        assert_eq!(loaded.version, SYNC_STATE_VERSION);
        assert_eq!(loaded.baselines, state.baselines);
    }

    #[test]
    fn load_v1_digest_format_returns_none() {
        // A v1 file (aggregate digest, version = 1) must load as None → one self-healing flush.
        let temp = fake_workspace();
        let path = sync_state_path(temp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "version = 1\ndigest = \"abc123\"\n").unwrap();
        assert!(load_sync_state(temp.path()).is_none());
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
        save_sync_state(temp.path(), &SyncState::new(map(&[("a", "x")]))).unwrap();
        let content = fs::read_to_string(sync_state_path(temp.path())).unwrap();
        assert!(content.contains("Safe to delete"));
    }
}
