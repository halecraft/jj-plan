use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::jj_binary::JjBinary;
use crate::plan_file;
use crate::sync_state::{self, Reconcile};
use crate::types::PlanRegistry;
use crate::workspace::Workspace;

/// Flush local plan-file edits to jj descriptions — the file→description direction of the
/// shared 3-way reconcile.
///
/// For each plan file, [`sync_state::reconcile`] decides the direction. flush executes only
/// its lane: it pushes (`jj describe`) on `FileToDesc`, and **leaves the description alone**
/// on `InSync`/`DescToFile`/`Conflict`. The `DescToFile`/`Conflict` no-push is the
/// mirror-bug guard — a stale file can never overwrite a changed description.
///
/// `flush_all` is deliberately **desc-only**: it never writes plan files and never persists
/// the baseline (it cannot observe `file == desc` until the post-command reload — that is
/// sync's / the gate's job, via `anchor`). Callers that flush *and then do more work against
/// the result* — `run_done`, `wrap`, the read gate — must use [`flush_and_anchor`], which
/// composes `flush_all` + reload + `observe` + `anchor` + save so the baseline tracks the
/// converged state; otherwise a following description rewrite is mis-attributed by
/// `reconcile` against a stale baseline.
///
/// Internally GATHER → PLAN → EXECUTE (FC/IS):
/// - Gather: read plan files, resolve bookmark→change_id, batch-read descriptions, load
///   baselines (I/O).
/// - Plan: `reconcile` each file → `Vec<FlushAction>` (pure).
/// - Execute: shell out `jj describe` for each action (I/O, best-effort).
pub fn flush_all(plan_dir: &Path, jj: &JjBinary, workspace: &Workspace, registry: &PlanRegistry) {
    // Don't flush in the error state.
    if plan_file::is_error_state(plan_dir) {
        return;
    }

    let gathered = gather_flush_state(plan_dir, workspace, registry);
    let actions = plan_flush(&gathered);
    execute_flush(jj, &actions);
}

// ---------------------------------------------------------------------------
// GATHER — read filesystem and jj state
// ---------------------------------------------------------------------------

/// All the data needed to compute flush actions, collected in one pass.
struct FlushGatherState {
    /// Map of bookmark_name → file content (read from disk; includes empty files —
    /// `reconcile` handles emptiness, replacing flush's old `!is_empty()` skip).
    file_contents: HashMap<String, String>,
    /// Map of bookmark_name → change_id (resolved from workspace bookmarks).
    bookmark_to_change_id: HashMap<String, String>,
    /// Map of change_id → jj description (batch-read).
    jj_descriptions: HashMap<String, String>,
    /// Map of bookmark_name → confirmed-equal content hash from the last sync.
    baselines: BTreeMap<String, String>,
}

/// Collect plan file contents, the corresponding jj descriptions, and the baselines.
///
/// Resolution chain: plan filename → (registry) → bookmark name → change ID → description.
fn gather_flush_state(plan_dir: &Path, workspace: &Workspace, registry: &PlanRegistry) -> FlushGatherState {
    // Read the plan-file set once (shared primitive; includes empty files).
    let file_contents: HashMap<String, String> = sync_state::read_plan_contents(plan_dir, registry)
        .into_iter()
        .map(|(entry, content)| (entry.bookmark_name, content))
        .collect();

    if file_contents.is_empty() {
        return FlushGatherState {
            file_contents,
            bookmark_to_change_id: HashMap::new(),
            jj_descriptions: HashMap::new(),
            baselines: BTreeMap::new(),
        };
    }

    // Build bookmark_name → change_id from workspace bookmarks (only those with plan files).
    let all_bookmarks = workspace.local_bookmarks();
    let mut bookmark_to_change_id = HashMap::new();
    for bookmark_name in file_contents.keys() {
        if let Some(bm) = all_bookmarks.iter().find(|b| &b.name == bookmark_name) {
            // Short reverse-hex change ID — what `jj describe -r` expects and what
            // gather_descriptions keys on.
            let short_id = workspace
                .short_change_id_from_hex(&bm.change_id)
                .unwrap_or_else(|| bm.change_id[..8.min(bm.change_id.len())].to_string());
            bookmark_to_change_id.insert(bookmark_name.clone(), short_id);
        }
    }

    // Batch-read jj descriptions for all resolved change IDs.
    let change_ids: Vec<&str> = bookmark_to_change_id.values().map(|s| s.as_str()).collect();
    let jj_descriptions = if change_ids.is_empty() {
        HashMap::new()
    } else {
        workspace.gather_descriptions(&change_ids)
    };

    // Load the per-bookmark baselines (internal — keeps flush_all's 13 callers unchanged).
    let repo_root = workspace.jj_workspace().workspace_root();
    let baselines = sync_state::load_sync_state(repo_root)
        .map(|s| s.baselines)
        .unwrap_or_default();

    FlushGatherState {
        file_contents,
        bookmark_to_change_id,
        jj_descriptions,
        baselines,
    }
}

/// Post-flush observations for baseline anchoring: `(bookmark, Some(file), description)`
/// for every plan file whose bookmark resolves to an existing change.
///
/// Used by the read-path and summary gates, which flush without a following sync pass: they
/// call this after `flush_all` + `workspace.reload()`, then `sync_state::anchor` advances
/// only the bookmarks now observed equal — so a failed flush cannot poison the baseline.
/// Bookmarks that don't resolve are omitted (anchor keeps their prior baseline).
pub fn observe(
    plan_dir: &Path,
    workspace: &Workspace,
    registry: &PlanRegistry,
) -> Vec<(String, Option<String>, String)> {
    let g = gather_flush_state(plan_dir, workspace, registry);
    g.file_contents
        .iter()
        .filter_map(|(bookmark, content)| {
            let change_id = g.bookmark_to_change_id.get(bookmark)?;
            let desc = g.jj_descriptions.get(change_id.as_str())?;
            Some((bookmark.clone(), Some(content.clone()), desc.clone()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// PLAN — pure decision logic, no I/O
// ---------------------------------------------------------------------------

/// A single flush action: write this content to a jj change's description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushAction {
    /// The change ID to describe (short reverse-hex for `jj describe -r`).
    pub change_id: String,
    /// The new description content (from the plan file).
    pub content: String,
}

/// Pure planning: project [`sync_state::reconcile`] onto flush's lane.
///
/// A `FlushAction` is produced only when `reconcile` returns `FileToDesc` AND the bookmark
/// resolves to a change that still exists in jj. Every other outcome (`InSync`,
/// `DescToFile`, `Conflict`) is a no-op for flush — in particular `DescToFile`/`Conflict`
/// never push, so a stale file cannot clobber a changed description.
fn plan_flush(state: &FlushGatherState) -> Vec<FlushAction> {
    let mut actions = Vec::new();

    for (bookmark_name, file_content) in &state.file_contents {
        // Resolve bookmark → change_id (skip deleted bookmarks).
        let change_id = match state.bookmark_to_change_id.get(bookmark_name) {
            Some(id) => id,
            None => continue,
        };
        // Skip if the change no longer exists in jj (abandoned externally).
        let jj_desc = match state.jj_descriptions.get(change_id.as_str()) {
            Some(desc) => desc,
            None => continue,
        };
        let base = state.baselines.get(bookmark_name).map(|s| s.as_str());

        if sync_state::reconcile(Some(file_content), jj_desc, base) == Reconcile::FileToDesc {
            actions.push(FlushAction {
                change_id: change_id.clone(),
                content: file_content.clone(),
            });
        }
    }

    actions
}

// ---------------------------------------------------------------------------
// EXECUTE — apply flush actions via jj subprocess
// ---------------------------------------------------------------------------

/// Shell out to `jj describe` for each flush action.
///
/// Errors are ignored (best-effort, matching the historical behavior). This is now safe:
/// a failed push leaves `file != desc`, so `anchor` simply does not advance that bookmark's
/// baseline — correctness no longer depends on the describe succeeding.
fn execute_flush(jj: &JjBinary, actions: &[FlushAction]) {
    for action in actions {
        let _ = jj.run_silent(&["describe", "-r", &action.change_id, "-m", &action.content]);
    }
}

// ---------------------------------------------------------------------------
// Shared pre-command sequence: flush, then anchor the baseline
// ---------------------------------------------------------------------------

/// Flush plan-file edits to descriptions, then anchor the baseline to the
/// post-flush state — the full sequence every pre-command/read-gate site needs.
///
/// A bare [`flush_all`] converges file→desc but leaves the baseline stale
/// (flush is deliberately desc-only). So a command that *rewrites the
/// description right after the flush* — `jj plan done`'s stamp, an editor
/// `jj describe` — would be mis-attributed by [`crate::sync_state::reconcile`]
/// against that stale baseline and reverted (or spuriously conflicted).
/// Anchoring here records the converged content, so the subsequent rewrite
/// lands cleanly as `DescToFile` and sync writes the file.
///
/// `anchor` advances only bookmarks observed equal (`file == desc`), so a failed
/// flush never poisons the baseline. `prev_baselines` is the map loaded before
/// the flush; the advanced map is persisted to the sync-state sidecar.
pub fn flush_and_anchor(
    plan_dir: &Path,
    jj: &JjBinary,
    workspace: &mut Workspace,
    registry: &PlanRegistry,
    prev_baselines: &BTreeMap<String, String>,
) {
    flush_all(plan_dir, jj, workspace, registry);
    // Re-read post-flush descriptions and advance baselines only for bookmarks
    // now confirmed equal — a failed flush leaves file != desc and is NOT
    // recorded (no poisoning).
    workspace.reload();
    let observed = observe(plan_dir, workspace, registry);
    let next = sync_state::anchor(prev_baselines, &observed);
    let repo_root = workspace.jj_workspace().workspace_root();
    let _ = sync_state::save_sync_state(repo_root, &sync_state::SyncState::new(next));
}

// ---------------------------------------------------------------------------
// Tests — plan_flush is pure and testable without I/O
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a FlushGatherState with bookmark-keyed data.
    ///
    /// `files`: (bookmark_name, file_content) pairs
    /// `bm_to_id`: (bookmark_name, change_id) pairs
    /// `descs`: (change_id, jj_description) pairs
    /// `baselines`: (bookmark_name, content-hash) pairs
    fn state(
        files: &[(&str, &str)],
        bm_to_id: &[(&str, &str)],
        descs: &[(&str, &str)],
        baselines: &[(&str, &str)],
    ) -> FlushGatherState {
        FlushGatherState {
            file_contents: files.iter().map(|(n, c)| (n.to_string(), c.to_string())).collect(),
            bookmark_to_change_id: bm_to_id.iter().map(|(n, i)| (n.to_string(), i.to_string())).collect(),
            jj_descriptions: descs.iter().map(|(i, d)| (i.to_string(), d.to_string())).collect(),
            baselines: baselines.iter().map(|(n, h)| (n.to_string(), h.to_string())).collect(),
        }
    }

    #[test]
    fn test_plan_flush_no_changes_when_matching() {
        let s = state(&[("feat-auth", "hello world")], &[("feat-auth", "abc")], &[("abc", "hello world")], &[]);
        assert!(plan_flush(&s).is_empty(), "file == desc → InSync, no push");
    }

    #[test]
    fn test_plan_flush_produces_action_when_different() {
        // No baseline → recoverable bias → FileToDesc → push.
        let s = state(&[("feat-auth", "new content")], &[("feat-auth", "abc")], &[("abc", "old content")], &[]);
        let actions = plan_flush(&s);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].change_id, "abc");
        assert_eq!(actions[0].content, "new content");
    }

    #[test]
    fn test_plan_flush_mirror_no_push_when_only_desc_changed() {
        // file unchanged (== base), desc changed → DescToFile → flush must NOT push
        // (the mirror-bug guard: a stale file can't clobber a changed description).
        let base = sync_state::hash_content("stale");
        let s = state(
            &[("feat-auth", "stale")],
            &[("feat-auth", "abc")],
            &[("abc", "freshly rebased description")],
            &[("feat-auth", &base)],
        );
        assert!(plan_flush(&s).is_empty(), "only desc changed → no push");
    }

    #[test]
    fn test_plan_flush_empty_file_does_not_push() {
        // An emptied file must never clear a non-empty description.
        let s = state(&[("feat-auth", "")], &[("feat-auth", "abc")], &[("abc", "real plan")], &[]);
        assert!(plan_flush(&s).is_empty(), "empty file → DescToFile, no push");
    }

    #[test]
    fn test_plan_flush_skips_abandoned_change() {
        let s = state(&[("feat-auth", "content")], &[("feat-auth", "abc")], &[], &[]);
        assert!(plan_flush(&s).is_empty());
    }

    #[test]
    fn test_plan_flush_skips_deleted_bookmark() {
        let s = state(&[("feat-auth", "content")], &[], &[], &[]);
        assert!(plan_flush(&s).is_empty());
    }

    #[test]
    fn test_plan_flush_multiple_files_mixed() {
        let s = state(
            &[("feat-auth", "same"), ("feat-session", "changed"), ("feat-api", "also changed")],
            &[("feat-auth", "aaa"), ("feat-session", "bbb"), ("feat-api", "ccc")],
            &[("aaa", "same"), ("bbb", "original"), ("ccc", "was this")],
            &[],
        );
        let actions = plan_flush(&s);
        assert_eq!(actions.len(), 2);
        let ids: Vec<&str> = actions.iter().map(|a| a.change_id.as_str()).collect();
        assert!(ids.contains(&"bbb"));
        assert!(ids.contains(&"ccc"));
        assert!(!ids.contains(&"aaa"));
    }

    #[test]
    fn test_plan_flush_bookmark_to_change_id_resolution() {
        let s = state(&[("fix/login", "updated fix")], &[("fix/login", "xyz123")], &[("xyz123", "old fix")], &[]);
        let actions = plan_flush(&s);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].change_id, "xyz123");
        assert_eq!(actions[0].content, "updated fix");
    }
}
