use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::plan_dir::PlanDir;
use crate::plan_file::{
    self, PlanFileEntry, remove_or_warn, rename_or_warn, write_atomic_or_warn, write_or_warn,
};
use crate::sync_state::{self, Reconcile};
use crate::types::PlanRegistry;
use crate::wrap::SyncChangeView;

// ---------------------------------------------------------------------------
// Public API — desc→file direction of the shared 3-way reconcile
// ---------------------------------------------------------------------------

/// Sync jj stack state to plan files, surfacing conflicts non-destructively.
///
/// For each stack change, [`sync_state::reconcile`] decides the direction; sync executes
/// only its lane: it writes the description to the file on `DescToFile`, and **preserves
/// the file** on `InSync`/`FileToDesc`/`Conflict`. A non-empty file is never overwritten
/// unless it is confirmed clean (`== base`), so unflushed edits are never lost.
///
/// Returns the advanced per-bookmark baselines (via [`sync_state::anchor`], pruned to the
/// current plan set). The caller persists them. `baselines` is the previous baseline map.
pub fn sync(
    plan_dir: &PlanDir,
    stack_changes: Option<&[SyncChangeView]>,
    max_stack_size: usize,
    registry: &PlanRegistry,
    stack_md_content: Option<&str>,
    baselines: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let dir = &plan_dir.path;

    // GATHER — read directory + content once
    let current_state = gather_current_state(dir, registry);

    // PLAN — pure decision logic, no I/O
    let plan = plan_sync(&current_state, stack_changes, max_stack_size, registry, stack_md_content, baselines);

    // EXECUTE — thin imperative shell
    execute_sync(dir, &plan);

    // ADVANCE baselines from the executed outcomes, pruned to bookmarks still present.
    let present: HashSet<&str> = plan.observations.iter().map(|(bm, _, _)| bm.as_str()).collect();
    let mut next = sync_state::anchor(baselines, &plan.observations);
    next.retain(|bm, _| present.contains(bm.as_str()));
    next
}

/// Set error state: write `error.md` and emit warning.
pub fn set_error(plan_dir: &Path, message: &str) {
    write_or_warn(&plan_dir.join("error.md"), &format!("{}\n", message));
    eprintln!("jj-plan: ERROR: {}", message);
}

/// Clear error state: remove `error.md` if it exists.
pub fn clear_error(plan_dir: &Path) {
    let error_path = plan_dir.join("error.md");
    if error_path.exists() {
        remove_or_warn(&error_path);
    }
}

// ---------------------------------------------------------------------------
// GATHER — read filesystem state once (entries + content)
// ---------------------------------------------------------------------------

/// Snapshot of the plan directory's current on-disk state.
#[derive(Debug)]
struct CurrentPlanState {
    /// All existing plan file entries (from a single `read_dir`).
    entries: Vec<PlanFileEntry>,
    /// Map of bookmark_name → filename for quick lookup during planning.
    bookmark_to_filename: HashMap<String, String>,
    /// Map of bookmark_name → current file content (for the 3-way reconcile). Pure data
    /// passed to `plan_sync` — no lookup closures.
    bookmark_to_content: HashMap<String, String>,
}

/// Read the plan directory once and build the current state snapshot.
fn gather_current_state(plan_dir: &Path, registry: &PlanRegistry) -> CurrentPlanState {
    let contents = sync_state::read_plan_contents(plan_dir, registry);
    let mut entries = Vec::with_capacity(contents.len());
    let mut bookmark_to_filename = HashMap::new();
    let mut bookmark_to_content = HashMap::new();
    for (entry, content) in contents {
        bookmark_to_filename.insert(entry.bookmark_name.clone(), entry.filename.clone());
        bookmark_to_content.insert(entry.bookmark_name.clone(), content);
        entries.push(entry);
    }
    CurrentPlanState {
        entries,
        bookmark_to_filename,
        bookmark_to_content,
    }
}

// ---------------------------------------------------------------------------
// PLAN — pure decision logic, no I/O
// ---------------------------------------------------------------------------

/// A file write (`DescToFile`): adopt the description into the plan file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileWrite {
    filename: String,
    content: String,
    /// Prior on-disk content, for the pre-overwrite `.history` snapshot (`None` = new file).
    prev: Option<String>,
}

/// A conflict (`Conflict`): preserve the file, surface the incoming description.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileConflict {
    bookmark: String,
    filename: String,
    incoming: String,
}

/// A stale file to remove (bookmark no longer in the stack).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileRemove {
    bookmark: String,
    filename: String,
    /// Prior content for the `.history` snapshot (`None`/empty = nothing to preserve).
    prev: Option<String>,
    /// Whether the content diverged from its baseline (unflushed) — preserve as `.orphan`.
    diverged: bool,
}

/// A file rename (reordered change — same bookmark, different index).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileRename {
    from: String,
    to: String,
}

/// Warnings to emit to stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SyncWarning {
    /// Stack bookmark was lost — plan files exist but no base resolves.
    BookmarkLost,
}

/// The complete plan for a sync operation. Computed by `plan_sync()` with no I/O.
#[derive(Debug)]
struct SyncPlan {
    files_to_remove: Vec<FileRemove>,
    files_to_rename: Vec<FileRename>,
    files_to_write: Vec<FileWrite>,
    files_in_conflict: Vec<FileConflict>,
    /// Bookmarks whose flush failed (file has unpushed edits) — warn, but preserve.
    unflushed: Vec<String>,
    file_summary: Option<String>,
    remove_stack_md: bool,
    clear_error: bool,
    error: Option<String>,
    warnings: Vec<SyncWarning>,
    /// Post-execution `(bookmark, file_after, desc)` per change — drives `anchor`.
    observations: Vec<(String, Option<String>, String)>,
}

impl SyncPlan {
    fn empty() -> Self {
        SyncPlan {
            files_to_remove: Vec::new(),
            files_to_rename: Vec::new(),
            files_to_write: Vec::new(),
            files_in_conflict: Vec::new(),
            unflushed: Vec::new(),
            file_summary: None,
            remove_stack_md: false,
            clear_error: false,
            error: None,
            warnings: Vec::new(),
            observations: Vec::new(),
        }
    }
}

/// Build a `FileRemove` for a stale entry, computing whether its content diverged from
/// its baseline (unflushed → preserve as `.orphan`).
fn plan_removal(
    entry: &PlanFileEntry,
    current_state: &CurrentPlanState,
    baselines: &BTreeMap<String, String>,
) -> FileRemove {
    let prev = current_state.bookmark_to_content.get(&entry.bookmark_name).cloned();
    let diverged = match &prev {
        Some(content) if !content.is_empty() => {
            baselines.get(&entry.bookmark_name).map(String::as_str)
                != Some(sync_state::hash_content(content).as_str())
        }
        _ => false,
    };
    FileRemove {
        bookmark: entry.bookmark_name.clone(),
        filename: entry.filename.clone(),
        prev,
        diverged,
    }
}

/// Pure planning: given the current directory state and the stack changes, compute the
/// complete set of filesystem operations. Performs NO I/O.
fn plan_sync(
    current_state: &CurrentPlanState,
    stack_changes: Option<&[SyncChangeView]>,
    max_stack_size: usize,
    registry: &PlanRegistry,
    stack_md_content: Option<&str>,
    baselines: &BTreeMap<String, String>,
) -> SyncPlan {
    let mut plan = SyncPlan::empty();

    match stack_changes {
        None => {
            // Plan-loss detection: warn only if the registry still has entries.
            if !current_state.entries.is_empty() && !registry.bookmarks.is_empty() {
                plan.warnings.push(SyncWarning::BookmarkLost);
            }
            // Clean up all stale plan files (snapshot/orphan-guarded in execute).
            for entry in &current_state.entries {
                plan.files_to_remove.push(plan_removal(entry, current_state, baselines));
            }
            plan.remove_stack_md = true;
        }
        Some(changes) => {
            if changes.len() > max_stack_size {
                plan.error = Some(format!(
                    "Stack has {} changes (max {}). Refusing to sync. \
                     Is @ in the right place? Create a plan: jj plan new <bookmark-name>  or track one: jj plan track <bookmark>",
                    changes.len(),
                    max_stack_size
                ));
                plan.remove_stack_md = true;
                return plan;
            }

            plan.clear_error = true;

            let current_bookmarks: HashSet<&str> =
                changes.iter().map(|c| c.bookmark_name.as_str()).collect();

            // 1. Stale files to remove (bookmarks no longer in stack).
            for entry in &current_state.entries {
                if !current_bookmarks.contains(entry.bookmark_name.as_str()) {
                    plan.files_to_remove.push(plan_removal(entry, current_state, baselines));
                }
            }

            // 2. For each stack change: rename to correct position, then reconcile.
            let num_changes = changes.len();
            for (idx, change) in changes.iter().enumerate() {
                let encoded_name = plan_file::encode_bookmark_for_filename(&change.bookmark_name);
                let target_filename = plan_file::format_plan_filename(idx, num_changes, &encoded_name);

                // Reposition an existing file for this bookmark (independent of reconcile).
                if let Some(existing_name) = current_state.bookmark_to_filename.get(&change.bookmark_name)
                    && *existing_name != target_filename
                {
                    plan.files_to_rename.push(FileRename {
                        from: existing_name.clone(),
                        to: target_filename.clone(),
                    });
                }

                let file = current_state.bookmark_to_content.get(&change.bookmark_name).map(String::as_str);
                let base = baselines.get(&change.bookmark_name).map(String::as_str);
                let desc = &change.description;

                let (file_after, _) = match sync_state::reconcile(file, desc, base) {
                    Reconcile::DescToFile => {
                        plan.files_to_write.push(FileWrite {
                            filename: target_filename.clone(),
                            content: desc.clone(),
                            prev: file.map(str::to_string),
                        });
                        (Some(desc.clone()), ())
                    }
                    Reconcile::InSync => (file.map(str::to_string), ()),
                    Reconcile::FileToDesc => {
                        // Flush failed to push this file's edits — preserve, warn.
                        plan.unflushed.push(change.bookmark_name.clone());
                        (file.map(str::to_string), ())
                    }
                    Reconcile::Conflict => {
                        plan.files_in_conflict.push(FileConflict {
                            bookmark: change.bookmark_name.clone(),
                            filename: target_filename.clone(),
                            incoming: desc.clone(),
                        });
                        (file.map(str::to_string), ())
                    }
                };

                plan.observations.push((change.bookmark_name.clone(), file_after, desc.clone()));
            }

            plan.file_summary = stack_md_content.map(str::to_string);
        }
    }

    plan
}

// ---------------------------------------------------------------------------
// EXECUTE — thin imperative shell, applies the SyncPlan
// ---------------------------------------------------------------------------

/// Snapshot prior plan-file content to `.jj-plan/.history/<sha8>-<bookmark>.md` so any
/// overwrite or removal is recoverable. Best-effort.
fn snapshot_to_history(plan_dir: &Path, bookmark: &str, content: &str) {
    if content.is_empty() {
        return;
    }
    let history = plan_dir.join(".history");
    if !history.exists() && std::fs::create_dir_all(&history).is_err() {
        return;
    }
    let sha8: String = sync_state::hash_content(content).chars().take(8).collect();
    let encoded = plan_file::encode_bookmark_for_filename(bookmark);
    let _ = std::fs::write(history.join(format!("{sha8}-{encoded}.md")), content);
}

fn execute_sync(plan_dir: &Path, plan: &SyncPlan) {
    for warning in &plan.warnings {
        match warning {
            SyncWarning::BookmarkLost => {
                eprintln!("jj-plan: WARNING: No plans found in stack. Register a bookmark: jj plan track <bookmark>");
            }
        }
    }

    if plan.remove_stack_md {
        let stack_md = plan_dir.join("stack.md");
        if stack_md.exists() {
            remove_or_warn(&stack_md);
        }
    }

    if let Some(msg) = &plan.error {
        set_error(plan_dir, msg);
        return;
    }

    if plan.clear_error {
        clear_error(plan_dir);
    }

    // Remove stale files — snapshot first; preserve unflushed (diverged) content as `.orphan`.
    for remove in &plan.files_to_remove {
        if let Some(prev) = &remove.prev {
            snapshot_to_history(plan_dir, &remove.bookmark, prev);
            if remove.diverged {
                let orphan = plan_dir.join(format!("{}.orphan", remove.filename));
                write_or_warn(&orphan, prev);
                eprintln!(
                    "jj-plan: WARNING: '{}' left the stack with unflushed edits — preserved as {}",
                    remove.bookmark,
                    orphan.display()
                );
            }
        }
        remove_or_warn(&plan_dir.join(&remove.filename));
    }

    // Apply renames (reposition existing content).
    for rename in &plan.files_to_rename {
        rename_or_warn(&plan_dir.join(&rename.from), &plan_dir.join(&rename.to));
    }

    // Write adopted descriptions — atomic, with a pre-overwrite snapshot.
    for write in &plan.files_to_write {
        if let Some(prev) = &write.prev {
            // Snapshot keyed by the filename stem (the bookmark) for recoverability.
            let bookmark = plan_file::parse_plan_filename(&write.filename).unwrap_or(&write.filename);
            snapshot_to_history(plan_dir, bookmark, prev);
        }
        write_atomic_or_warn(&plan_dir.join(&write.filename), &write.content);
    }

    // Surface conflicts non-destructively: the file is preserved; the incoming
    // description is written alongside as `.incoming`.
    for conflict in &plan.files_in_conflict {
        if !conflict.incoming.is_empty() {
            let incoming = plan_dir.join(format!("{}.incoming", conflict.filename));
            write_or_warn(&incoming, &conflict.incoming);
            eprintln!(
                "jj-plan: WARNING: '{}' diverged from its description — file kept; incoming saved to {}",
                conflict.bookmark,
                incoming.display()
            );
        } else {
            eprintln!(
                "jj-plan: WARNING: '{}' diverged from its description — file kept (incoming was empty)",
                conflict.bookmark
            );
        }
    }

    // Warn about files whose edits could not be flushed (preserved, but the description is
    // now stale until the next successful flush).
    for bookmark in &plan.unflushed {
        eprintln!(
            "jj-plan: WARNING: '{}' has unflushed edits (flush did not reach the description) — file kept",
            bookmark
        );
    }

    // One-time cleanup: remove stale current.md from older versions.
    let current = plan_dir.join("current.md");
    if current.exists() || current.symlink_metadata().is_ok() {
        remove_or_warn(&current);
    }

    if let Some(file_summary) = &plan.file_summary {
        write_or_warn(&plan_dir.join("stack.md"), file_summary);
    }
}

// ---------------------------------------------------------------------------
// Tests — pure plan_sync tests that need no filesystem
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn change_with_bookmark(change_id: &str, bookmark_name: &str, desc: &str, is_wc: bool) -> SyncChangeView {
        SyncChangeView {
            change_id: change_id.to_string(),
            bookmark_name: bookmark_name.to_string(),
            description: desc.to_string(),
            is_working_copy: is_wc,
        }
    }

    fn change(id: &str, desc: &str, is_wc: bool) -> SyncChangeView {
        change_with_bookmark(id, id, desc, is_wc)
    }

    fn empty_state() -> CurrentPlanState {
        CurrentPlanState {
            entries: vec![],
            bookmark_to_filename: HashMap::new(),
            bookmark_to_content: HashMap::new(),
        }
    }

    /// `files`: (filename, bookmark_name, content) — content drives the reconcile.
    fn state_with(files: &[(&str, &str, &str)]) -> CurrentPlanState {
        let mut entries = Vec::new();
        let mut bookmark_to_filename = HashMap::new();
        let mut bookmark_to_content = HashMap::new();
        for (filename, bookmark_name, content) in files {
            entries.push(PlanFileEntry {
                filename: filename.to_string(),
                bookmark_name: bookmark_name.to_string(),
                path: std::path::PathBuf::from(filename),
            });
            bookmark_to_filename.insert(bookmark_name.to_string(), filename.to_string());
            bookmark_to_content.insert(bookmark_name.to_string(), content.to_string());
        }
        CurrentPlanState { entries, bookmark_to_filename, bookmark_to_content }
    }

    fn no_baselines() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn baselines(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(bm, c)| (bm.to_string(), sync_state::hash_content(c))).collect()
    }

    #[test]
    fn test_plan_sync_none_stack_with_files_warns_bookmark_lost() {
        let state = state_with(&[("01-feat-auth.md", "feat-auth", "plan")]);
        let mut reg = PlanRegistry::new();
        reg.track(crate::types::PlannedBookmark::new("feat-auth", "aabb"));
        let plan = plan_sync(&state, None, 50, &reg, None, &no_baselines());
        assert_eq!(plan.warnings, vec![SyncWarning::BookmarkLost]);
        assert_eq!(plan.files_to_remove.len(), 1);
    }

    #[test]
    fn test_plan_sync_none_stack_no_warning_after_untrack() {
        let state = state_with(&[("01-feat-auth.md", "feat-auth", "plan")]);
        let plan = plan_sync(&state, None, 50, &PlanRegistry::new(), None, &no_baselines());
        assert!(plan.warnings.is_empty());
        assert_eq!(plan.files_to_remove[0].filename, "01-feat-auth.md");
        assert!(plan.remove_stack_md);
    }

    #[test]
    fn test_plan_sync_exceeds_max_sets_error() {
        let changes = vec![change("aaa", "a", true), change("bbb", "b", false), change("ccc", "c", false)];
        let plan = plan_sync(&empty_state(), Some(&changes), 2, &PlanRegistry::new(), None, &no_baselines());
        assert!(plan.error.as_ref().unwrap().contains("3 changes (max 2)"));
        assert!(plan.files_to_write.is_empty());
    }

    #[test]
    fn test_plan_sync_writes_all_files_when_absent() {
        // No files on disk → each change materializes (DescToFile).
        let changes = vec![
            change_with_bookmark("aaa", "feat-auth", "desc A", false),
            change_with_bookmark("bbb", "fix-login", "desc B", true),
        ];
        let plan = plan_sync(&empty_state(), Some(&changes), 50, &PlanRegistry::new(), None, &no_baselines());
        assert!(plan.error.is_none());
        assert_eq!(plan.files_to_write.len(), 2);
        assert_eq!(plan.files_to_write[0].filename, "b-01-feat-auth.md");
        assert_eq!(plan.files_to_write[0].content, "desc A");
        assert_eq!(plan.files_to_write[1].filename, "a-02-fix-login.md");
        assert_eq!(plan.files_to_write[1].content, "desc B");
    }

    #[test]
    fn test_plan_sync_clean_file_adopts_changed_desc() {
        // file == base, desc changed → DescToFile (happy update path).
        let state = state_with(&[("a-01-feat-auth.md", "feat-auth", "old plan")]);
        let changes = vec![change_with_bookmark("aaa", "feat-auth", "new plan", true)];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None, &baselines(&[("feat-auth", "old plan")]));
        assert_eq!(plan.files_to_write.len(), 1);
        assert_eq!(plan.files_to_write[0].content, "new plan");
        assert_eq!(plan.files_to_write[0].prev.as_deref(), Some("old plan"));
        assert!(plan.files_in_conflict.is_empty());
    }

    #[test]
    fn test_plan_sync_unflushed_file_is_preserved_not_clobbered() {
        // file changed since base, desc still the old baseline (flush failed) → FileToDesc:
        // sync must NOT write; the authored file is preserved. (The reported bug.)
        let state = state_with(&[("a-01-feat-auth.md", "feat-auth", "RICH authored plan")]);
        let changes = vec![change_with_bookmark("aaa", "feat-auth", "old", true)];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None, &baselines(&[("feat-auth", "old")]));
        assert!(plan.files_to_write.is_empty(), "must not overwrite the authored file");
        assert_eq!(plan.unflushed, vec!["feat-auth"]);
    }

    #[test]
    fn test_plan_sync_empty_desc_never_clobbers_nonempty_file() {
        // The original-bug shape: file authored, description empty, no baseline → FileToDesc.
        let state = state_with(&[("a-01-redesign.md", "redesign", "the real plan")]);
        let changes = vec![change_with_bookmark("aaa", "redesign", "", true)];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None, &no_baselines());
        assert!(plan.files_to_write.is_empty(), "empty description must not zero the file");
    }

    #[test]
    fn test_plan_sync_true_conflict_preserves_and_surfaces() {
        // Both sides diverged from a known base → Conflict: file preserved, incoming surfaced.
        let state = state_with(&[("a-01-feat-auth.md", "feat-auth", "my edits")]);
        let changes = vec![change_with_bookmark("aaa", "feat-auth", "their edits", true)];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None, &baselines(&[("feat-auth", "base")]));
        assert!(plan.files_to_write.is_empty());
        assert_eq!(plan.files_in_conflict.len(), 1);
        assert_eq!(plan.files_in_conflict[0].incoming, "their edits");
    }

    #[test]
    fn test_plan_sync_removes_stale_files() {
        let state = state_with(&[
            ("b-01-feat-auth.md", "feat-auth", "a"),
            ("c-02-feat-session.md", "feat-session", "s"),
            ("a-03-feat-api.md", "feat-api", "c"),
        ]);
        let changes = vec![
            change_with_bookmark("aaa", "feat-auth", "a", true),
            change_with_bookmark("ccc", "feat-api", "c", false),
        ];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None, &no_baselines());
        let removed: Vec<&str> = plan.files_to_remove.iter().map(|r| r.filename.as_str()).collect();
        assert_eq!(removed, vec!["c-02-feat-session.md"]);
    }

    #[test]
    fn test_plan_sync_stale_diverged_file_marked_orphan() {
        // A stale file whose content was never flushed (no baseline) → diverged → orphan.
        let state = state_with(&[("a-01-gone.md", "gone", "unflushed work")]);
        let plan = plan_sync(&state, Some(&[change_with_bookmark("a", "kept", "k", true)]), 50, &PlanRegistry::new(), None, &no_baselines());
        let r = plan.files_to_remove.iter().find(|r| r.bookmark == "gone").unwrap();
        assert!(r.diverged, "unflushed stale content must be preserved as .orphan");
    }

    #[test]
    fn test_plan_sync_renames_reordered_files() {
        let state = state_with(&[("a-02-feat-auth.md", "feat-auth", "x"), ("b-01-fix-login.md", "fix-login", "y")]);
        let changes = vec![
            change_with_bookmark("aaa", "feat-auth", "a", true),
            change_with_bookmark("bbb", "fix-login", "b", false),
        ];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None, &no_baselines());
        assert!(plan.files_to_rename.iter().any(|r| r.from == "a-02-feat-auth.md" && r.to == "b-01-feat-auth.md"));
        assert!(plan.files_to_rename.iter().any(|r| r.from == "b-01-fix-login.md" && r.to == "a-02-fix-login.md"));
    }

    #[test]
    fn test_plan_sync_observations_drive_anchor() {
        // A write yields (file_after == desc) → anchor advances; an unflushed file does not.
        let state = state_with(&[("a-01-keep.md", "keep", "RICH"), ("b-02-clean.md", "clean", "old")]);
        let changes = vec![
            change_with_bookmark("k", "keep", "old", false),   // file RICH != base old → FileToDesc
            change_with_bookmark("c", "clean", "new", true),   // file old == base old → DescToFile
        ];
        let prev = baselines(&[("keep", "old"), ("clean", "old")]);
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None, &prev);
        let next = sync_state::anchor(&prev, &plan.observations);
        // clean advanced to hash("new"); keep stayed (unflushed, not poisoned).
        assert_eq!(next.get("clean"), Some(&sync_state::hash_content("new")));
        assert_eq!(next.get("keep"), Some(&sync_state::hash_content("old")));
    }

    #[test]
    fn test_plan_sync_stack_summary_passed_through() {
        let changes = vec![change_with_bookmark("aaa", "feat-auth", "First", true)];
        let md = "<!-- generated -->\nrendered\n";
        let plan = plan_sync(&empty_state(), Some(&changes), 50, &PlanRegistry::new(), Some(md), &no_baselines());
        assert_eq!(plan.file_summary.as_deref(), Some(md));
    }

    #[test]
    fn test_plan_sync_encodes_slash_in_bookmark() {
        let changes = vec![change_with_bookmark("aaa", "feat/auth", "Auth", true)];
        let plan = plan_sync(&empty_state(), Some(&changes), 50, &PlanRegistry::new(), None, &no_baselines());
        assert_eq!(plan.files_to_write[0].filename, "a-01-feat--auth.md");
    }
}
