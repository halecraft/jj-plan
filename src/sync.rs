use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::plan_dir::PlanDir;
use crate::plan_file::{
    self, PlanFileEntry, remove_or_warn, rename_or_warn, write_or_warn,
};
use crate::types::PlanRegistry;
use crate::wrap::SyncChangeView;

// ---------------------------------------------------------------------------
// Public API — same external signatures as before, internal FC/IS split
// ---------------------------------------------------------------------------

/// Sync jj stack state to plan files, symlink, and `stack.md` summary.
///
/// After this runs, `.jj-plan/` exactly reflects the current jj stack.
/// Assumes `flush_all()` has already been called (jj descriptions are
/// authoritative at this point).
///
/// Also handles bookmark-loss detection: if `stack_changes` is `None` but
/// plan files exist, a stack was lost — emit a warning.
/// Returns the terminal view string if a stack was synced, or `None` if
/// there was no stack (bookmark loss, error, etc.).
pub fn sync(
    plan_dir: &PlanDir,
    stack_changes: Option<&[SyncChangeView]>,
    max_stack_size: usize,
    registry: &PlanRegistry,
    stack_md_content: Option<&str>,
) {
    let dir = &plan_dir.path;

    // GATHER — read directory once
    let current_state = gather_current_state(dir, registry);

    // PLAN — pure decision logic, no I/O
    let plan = plan_sync(&current_state, stack_changes, max_stack_size, registry, stack_md_content);

    // EXECUTE — thin imperative shell
    execute_sync(dir, &plan);
}

/// Set error state: write `error.md`, update `current.md` symlink, emit warning.
pub fn set_error(plan_dir: &Path, message: &str) {
    write_or_warn(
        &plan_dir.join("error.md"),
        &format!("{}\n", message),
    );

    // Update current.md symlink to point to error.md
    let current = plan_dir.join("current.md");
    remove_or_warn(&current);
    #[cfg(unix)]
    plan_file::symlink_or_warn("error.md", &current);

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
// GATHER — read filesystem state once
// ---------------------------------------------------------------------------

/// Snapshot of the plan directory's current on-disk state.
///
/// Collected once per sync cycle to avoid repeated `read_dir` calls.
#[derive(Debug)]
struct CurrentPlanState {
    /// All existing plan file entries (from a single `read_dir`).
    entries: Vec<PlanFileEntry>,
    /// Map of bookmark_name → filename for quick lookup during planning.
    bookmark_to_filename: HashMap<String, String>,
}

/// Read the plan directory once and build the current state snapshot.
fn gather_current_state(plan_dir: &Path, registry: &PlanRegistry) -> CurrentPlanState {
    let entries = plan_file::collect_plan_files(plan_dir, registry);
    let bookmark_to_filename = entries
        .iter()
        .map(|e| (e.bookmark_name.clone(), e.filename.clone()))
        .collect();
    CurrentPlanState {
        entries,
        bookmark_to_filename,
    }
}

// ---------------------------------------------------------------------------
// PLAN — pure decision logic, no I/O
// ---------------------------------------------------------------------------

/// Describes a file write operation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileWrite {
    /// Filename relative to the plan directory (e.g. `01-kpqxywon.md`).
    filename: String,
    /// Content to write.
    content: String,
}

/// Describes a file rename operation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileRename {
    /// Old filename (relative to plan dir).
    from: String,
    /// New filename (relative to plan dir).
    to: String,
}

/// Warning to emit to stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SyncWarning {
    /// Stack bookmark was lost — plan files exist but no base resolves.
    BookmarkLost,
}

/// The complete plan for a sync operation. Computed by `plan_sync()` with
/// no I/O. Applied by `execute_sync()`.
#[derive(Debug)]
struct SyncPlan {
    /// Filenames to remove (stale plan files for changes no longer in stack).
    files_to_remove: Vec<String>,
    /// Files to rename (reordered changes — same change ID, different index).
    files_to_rename: Vec<FileRename>,
    /// Files to write (plan files with updated content from jj descriptions).
    files_to_write: Vec<FileWrite>,
    /// Target filename for the `current.md` symlink, or None to remove it.
    symlink_target: Option<String>,
    /// File summary for `stack.md`, or None to skip.
    file_summary: Option<String>,
    /// Whether to remove `stack.md` (stale state cleanup in error/None paths).
    remove_stack_md: bool,
    /// Whether to clear a previous error state.
    clear_error: bool,
    /// Whether to set error state (with message). Mutually exclusive with
    /// the rest of the plan — when set, files_to_remove/rename/write are empty.
    error: Option<String>,
    /// Warnings to emit.
    warnings: Vec<SyncWarning>,
}

/// Pure planning function: given the current directory state and the stack
/// changes from jj, compute the complete set of filesystem operations needed.
///
/// This function performs NO I/O — it only examines its inputs and returns
/// a `SyncPlan` describing what `execute_sync()` should do.
fn plan_sync(
    current_state: &CurrentPlanState,
    stack_changes: Option<&[SyncChangeView]>,
    max_stack_size: usize,
    registry: &PlanRegistry,
    stack_md_content: Option<&str>,
) -> SyncPlan {
    let mut plan = SyncPlan {
        files_to_remove: Vec::new(),
        files_to_rename: Vec::new(),
        files_to_write: Vec::new(),
        symlink_target: None,
        file_summary: None,
        remove_stack_md: false,
        clear_error: false,
        error: None,
        warnings: Vec::new(),
    };

    match stack_changes {
        None => {
            // Plan-loss detection: if plan files exist but no registered
            // bookmarks produced segments, the plans may have been untracked.
            // Only warn if the registry still has entries (unexpected loss).
            // If the registry is empty, this is an intentional untrack — no warning.
            if !current_state.entries.is_empty() && !registry.bookmarks.is_empty() {
                plan.warnings.push(SyncWarning::BookmarkLost);
            }
            // Clean up all stale plan files when stack is gone
            for entry in &current_state.entries {
                plan.files_to_remove.push(entry.filename.clone());
            }
            // Clean up stale stack.md when stack is gone
            plan.remove_stack_md = true;
        }
        Some(changes) => {
            // Check stack size against max
            if changes.len() > max_stack_size {
                plan.error = Some(format!(
                    "Stack has {} changes (max {}). Refusing to sync. \
                     Is @ in the right place? Create a plan: jj plan new <bookmark-name>  or track one: jj plan track <bookmark>",
                    changes.len(),
                    max_stack_size
                ));
                // Clean up stale stack.md when entering error state
                plan.remove_stack_md = true;
                return plan;
            }

            // Stack is within bounds — clear any previous error
            plan.clear_error = true;

            // Build lookup set of current stack bookmark names
            let current_bookmarks: HashSet<&str> =
                changes.iter().map(|c| c.bookmark_name.as_str()).collect();

            // 1. Identify stale files to remove (bookmarks no longer in stack)
            for entry in &current_state.entries {
                if !current_bookmarks.contains(entry.bookmark_name.as_str()) {
                    plan.files_to_remove.push(entry.filename.clone());
                }
            }

            // 2. For each stack change, plan renames and writes
            let mut current_file: Option<String> = None;

            for (idx, change) in changes.iter().enumerate() {
                let padded_idx = format!("{:02}", idx + 1);
                let encoded_name = plan_file::encode_bookmark_for_filename(&change.bookmark_name);
                let target_filename =
                    format!("{}-{}.md", padded_idx, encoded_name);

                // Check if a file exists with the right bookmark name but wrong
                // index — if so, rename rather than delete+recreate
                if let Some(existing_name) =
                    current_state.bookmark_to_filename.get(&change.bookmark_name)
                    && *existing_name != target_filename {
                        plan.files_to_rename.push(FileRename {
                            from: existing_name.clone(),
                            to: target_filename.clone(),
                        });
                    }

                // Always write the description (jj is authoritative after flush)
                plan.files_to_write.push(FileWrite {
                    filename: target_filename.clone(),
                    content: change.description.clone(),
                });

                if change.is_working_copy {
                    current_file = Some(target_filename.clone());
                }
            }

            // 3. Set symlink target
            // If @ is not on any segment tip (e.g. user did `jj new` to start
            // coding on an unbookmarked WIP commit), fall back to the last
            // (tip-most) plan file. This keeps `current.md` pointing at the
            // most relevant plan rather than disappearing.
            if current_file.is_none() && !changes.is_empty() {
                let last = changes.last().unwrap();
                let last_idx = changes.len();
                let encoded = plan_file::encode_bookmark_for_filename(&last.bookmark_name);
                let last_filename = format!("{:02}-{}.md", last_idx, encoded);
                current_file = Some(last_filename);
            }
            plan.symlink_target = current_file.clone();

            // 4. Set file summary for stack.md (content generated by the rendering pipeline)
            plan.file_summary = stack_md_content.map(|s| s.to_string());
        }
    }

    plan
}

// ---------------------------------------------------------------------------
// EXECUTE — thin imperative shell, applies the SyncPlan
// ---------------------------------------------------------------------------

/// Apply a `SyncPlan` to the filesystem.
///
/// This is the only function in the sync module that performs I/O (aside
/// from `set_error`/`clear_error` which are called from `wrap.rs` for the
/// ambiguous-bookmark case).
fn execute_sync(plan_dir: &Path, plan: &SyncPlan) {
    // Emit warnings
    for warning in &plan.warnings {
        match warning {
            SyncWarning::BookmarkLost => {
                eprintln!("jj-plan: WARNING: No plans found in stack. Register a bookmark: jj plan track <bookmark>");
            }
        }
    }

    // Remove stale stack.md if requested (error or None paths)
    if plan.remove_stack_md {
        let stack_md = plan_dir.join("stack.md");
        if stack_md.exists() {
            remove_or_warn(&stack_md);
        }
    }

    // Handle error state
    if let Some(msg) = &plan.error {
        set_error(plan_dir, msg);
        return;
    }

    // Clear previous error if needed
    if plan.clear_error {
        clear_error(plan_dir);
    }

    // Remove stale files
    for filename in &plan.files_to_remove {
        remove_or_warn(&plan_dir.join(filename));
    }

    // Apply renames
    for rename in &plan.files_to_rename {
        rename_or_warn(&plan_dir.join(&rename.from), &plan_dir.join(&rename.to));
    }

    // Write plan files
    for write in &plan.files_to_write {
        write_or_warn(&plan_dir.join(&write.filename), &write.content);
    }

    // Update current.md symlink
    let current = plan_dir.join("current.md");
    remove_or_warn(&current);
    if let Some(target) = &plan.symlink_target {
        #[cfg(unix)]
        plan_file::symlink_or_warn(target, &current);
        #[cfg(not(unix))]
        {
            let source = plan_dir.join(target);
            plan_file::copy_or_warn(&source, &current);
        }
    }

    // Write stack.md
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
    use crate::types::PlannedBookmark;

    /// Helper to build a minimal SyncChangeView for tests.
    ///
    /// `id` is used as both the change_id and bookmark_name for simplicity
    /// in existing tests. Use `change_with_bookmark` when they need to differ.
    fn change(id: &str, desc: &str, _is_empty: bool, is_wc: bool) -> SyncChangeView {
        SyncChangeView {
            change_id: id.to_string(),
            bookmark_name: id.to_string(),
            description: desc.to_string(),
            is_working_copy: is_wc,
        }
    }

    /// Build a SyncChangeView with distinct change_id and bookmark_name.
    fn change_with_bookmark(change_id: &str, bookmark_name: &str, desc: &str, _is_empty: bool, is_wc: bool) -> SyncChangeView {
        SyncChangeView {
            change_id: change_id.to_string(),
            bookmark_name: bookmark_name.to_string(),
            description: desc.to_string(),
            is_working_copy: is_wc,
        }
    }

    fn empty_state() -> CurrentPlanState {
        CurrentPlanState {
            entries: vec![],
            bookmark_to_filename: HashMap::new(),
        }
    }

    fn state_with(files: &[(&str, &str)]) -> CurrentPlanState {
        let entries: Vec<PlanFileEntry> = files
            .iter()
            .map(|(filename, bookmark_name)| PlanFileEntry {
                filename: filename.to_string(),
                bookmark_name: bookmark_name.to_string(),
                path: std::path::PathBuf::from(filename),
            })
            .collect();
        let bookmark_to_filename = entries
            .iter()
            .map(|e| (e.bookmark_name.clone(), e.filename.clone()))
            .collect();
        CurrentPlanState {
            entries,
            bookmark_to_filename,
        }
    }

    // -- plan_sync tests (new — pure, no filesystem needed) --

    #[test]
    fn test_plan_sync_none_stack_no_files() {
        let state = empty_state();
        // Registry has entries, so the warning fires (unexpected loss, not intentional untrack)
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat-auth", "aabb"));
        let plan = plan_sync(&state, None, 50, &reg, None);

        assert!(plan.files_to_remove.is_empty());
        assert!(plan.files_to_write.is_empty());
        assert!(plan.warnings.is_empty());
        assert!(plan.error.is_none());
    }

    #[test]
    fn test_plan_sync_none_stack_with_files_warns_bookmark_lost() {
        let state = state_with(&[("01-feat-auth.md", "feat-auth")]);
        // Registry has entries → this is an unexpected loss, not an intentional untrack.
        // The warning should fire.
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat-auth", "aabb"));
        let plan = plan_sync(&state, None, 50, &reg, None);

        assert_eq!(plan.warnings.len(), 1);
        assert_eq!(plan.warnings[0], SyncWarning::BookmarkLost);
    }

    #[test]
    fn test_plan_sync_none_stack_with_files_no_warning_after_untrack() {
        let state = state_with(&[("01-feat-auth.md", "feat-auth")]);
        // Empty registry → intentional untrack. No warning, but files should
        // still be scheduled for removal.
        let plan = plan_sync(&state, None, 50, &PlanRegistry::new(), None);

        assert!(plan.warnings.is_empty(), "no warning when registry is empty");
        assert_eq!(plan.files_to_remove, vec!["01-feat-auth.md"]);
        assert!(plan.remove_stack_md);
    }

    #[test]
    fn test_plan_sync_exceeds_max_sets_error() {
        let state = empty_state();
        let changes = vec![
            change("aaa", "a", true, true),
            change("bbb", "b", true, false),
            change("ccc", "c", true, false),
        ];
        let plan = plan_sync(&state, Some(&changes), 2, &PlanRegistry::new(), None);

        assert!(plan.error.is_some());
        assert!(plan.error.as_ref().unwrap().contains("3 changes (max 2)"));
        // When error is set, no file ops
        assert!(plan.files_to_write.is_empty());
    }

    #[test]
    fn test_plan_sync_writes_all_files() {
        let state = empty_state();
        let changes = vec![
            change_with_bookmark("aaa", "feat-auth", "desc A", true, false),
            change_with_bookmark("bbb", "fix-login", "desc B", false, true),
        ];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None);

        assert!(plan.error.is_none());
        assert!(plan.clear_error);
        assert_eq!(plan.files_to_write.len(), 2);
        assert_eq!(plan.files_to_write[0].filename, "01-feat-auth.md");
        assert_eq!(plan.files_to_write[0].content, "desc A");
        assert_eq!(plan.files_to_write[1].filename, "02-fix-login.md");
        assert_eq!(plan.files_to_write[1].content, "desc B");
        assert_eq!(plan.symlink_target, Some("02-fix-login.md".to_string()));
    }

    #[test]
    fn test_plan_sync_removes_stale_files() {
        let state = state_with(&[
            ("01-feat-auth.md", "feat-auth"),
            ("02-feat-session.md", "feat-session"),
            ("03-feat-api.md", "feat-api"),
        ]);
        // Stack now only has feat-auth and feat-api — feat-session is stale
        let changes = vec![
            change_with_bookmark("aaa", "feat-auth", "a", true, true),
            change_with_bookmark("ccc", "feat-api", "c", true, false),
        ];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None);

        assert_eq!(plan.files_to_remove, vec!["02-feat-session.md"]);
    }

    #[test]
    fn test_plan_sync_renames_reordered_files() {
        // File exists as 02-feat-auth.md but should be 01-feat-auth.md after reorder
        let state = state_with(&[("02-feat-auth.md", "feat-auth"), ("01-fix-login.md", "fix-login")]);
        let changes = vec![
            change_with_bookmark("aaa", "feat-auth", "a", true, true),
            change_with_bookmark("bbb", "fix-login", "b", true, false),
        ];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None);

        assert_eq!(plan.files_to_rename.len(), 2);
        // feat-auth: 02 → 01
        assert!(plan.files_to_rename.iter().any(|r| r.from == "02-feat-auth.md"
            && r.to == "01-feat-auth.md"));
        // fix-login: 01 → 02
        assert!(plan.files_to_rename.iter().any(|r| r.from == "01-fix-login.md"
            && r.to == "02-fix-login.md"));
    }

    #[test]
    fn test_plan_sync_no_rename_when_index_matches() {
        let state = state_with(&[("01-feat-auth.md", "feat-auth")]);
        let changes = vec![change_with_bookmark("aaa", "feat-auth", "a", true, true)];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None);

        assert!(plan.files_to_rename.is_empty());
    }

    #[test]
    fn test_plan_sync_stack_summary_generated() {
        let state = empty_state();
        let changes = vec![
            change_with_bookmark("aaa", "feat-auth", "First plan", true, true),
            change_with_bookmark("bbb", "feat-session", "Second plan", false, false),
        ];
        let md_content = "<!-- generated by jj-plan -->\nsome rendered content\n";
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), Some(md_content));

        // file_summary is a pass-through of the provided content
        assert_eq!(plan.file_summary.as_deref(), Some(md_content));
    }

    #[test]
    fn test_plan_sync_current_md_falls_back_to_last_when_wc_not_in_stack() {
        // When @ is on an unbookmarked WIP commit (not in any segment),
        // none of the SyncChangeViews will have is_working_copy=true.
        // The symlink should fall back to the last (tip-most) plan file
        // rather than being set to None (which would delete current.md).
        let state = empty_state();
        let changes = vec![
            change_with_bookmark("aaa", "feat-auth", "First plan", true, false),   // not WC
            change_with_bookmark("bbb", "feat-session", "Second plan", false, false),  // not WC either
        ];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), Some("test summary"));

        // Should fall back to the last plan file
        assert_eq!(
            plan.symlink_target,
            Some("02-feat-session.md".to_string()),
            "When @ is not on any segment tip, current.md should point to the last plan"
        );
    }

    #[test]
    fn test_plan_sync_current_md_prefers_wc_over_fallback() {
        // When @ IS on a segment tip, the symlink should point to that file
        // (not the fallback).
        let state = empty_state();
        let changes = vec![
            change_with_bookmark("aaa", "feat-auth", "First plan", true, true),    // WC is here
            change_with_bookmark("bbb", "feat-session", "Second plan", false, false),
        ];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None);

        assert_eq!(
            plan.symlink_target,
            Some("01-feat-auth.md".to_string()),
            "When @ is on a segment tip, current.md should point to that segment's file"
        );
    }

    #[test]
    fn test_plan_sync_encodes_slash_in_bookmark() {
        let state = empty_state();
        let changes = vec![
            change_with_bookmark("aaa", "feat/auth", "Auth feature", true, true),
        ];
        let plan = plan_sync(&state, Some(&changes), 50, &PlanRegistry::new(), None);

        assert_eq!(plan.files_to_write.len(), 1);
        assert_eq!(plan.files_to_write[0].filename, "01-feat--auth.md");
        assert_eq!(plan.symlink_target, Some("01-feat--auth.md".to_string()));
    }
}