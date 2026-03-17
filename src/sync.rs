use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::plan_dir::PlanDir;
use crate::plan_file::{
    self, PlanFileEntry, remove_or_warn, rename_or_warn, write_or_warn,
};
use crate::stack::StackChange;

// ---------------------------------------------------------------------------
// Public API — same external signatures as before, internal FC/IS split
// ---------------------------------------------------------------------------

/// Sync jj stack state to plan files, symlink, and `.stack` summary.
///
/// After this runs, `.jj-plan/` exactly reflects the current jj stack.
/// Assumes `flush_all()` has already been called (jj descriptions are
/// authoritative at this point).
///
/// Also handles bookmark-loss detection: if `stack_changes` is `None` but
/// plan files exist, a stack was lost — emit a warning.
pub fn sync(
    plan_dir: &PlanDir,
    stack_changes: Option<&[StackChange]>,
    max_stack_size: usize,
) {
    let dir = &plan_dir.path;

    // GATHER — read directory once
    let current_state = gather_current_state(dir);

    // PLAN — pure decision logic, no I/O
    let plan = plan_sync(&current_state, stack_changes, max_stack_size);

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

/// Display the plan stack summary to stdout.
///
/// Pure display function — reads the `.stack` file and prints it.
/// Call after `sync()` has run so `.stack` is up to date.
pub fn show_stack(plan_dir: &PlanDir) {
    let stack_path = plan_dir.path.join(".stack");
    if let Ok(content) = fs::read_to_string(&stack_path)
        && !content.is_empty() {
            println!();
            println!(
                "Plan stack ({}/; *=here ✓=done ~=has changes):",
                plan_dir.dir_name()
            );
            print!("{}", content);
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
    /// Map of change_id → filename for quick lookup during planning.
    id_to_filename: HashMap<String, String>,
}

/// Read the plan directory once and build the current state snapshot.
fn gather_current_state(plan_dir: &Path) -> CurrentPlanState {
    let entries = plan_file::collect_plan_files(plan_dir);
    let id_to_filename = entries
        .iter()
        .map(|e| (e.change_id.clone(), e.filename.clone()))
        .collect();
    CurrentPlanState {
        entries,
        id_to_filename,
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
    /// Stack exceeds max size.
    StackTooLarge { size: usize, max: usize },
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
    /// Content for the `.stack` summary file, or None to skip writing.
    stack_summary: Option<String>,
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
    stack_changes: Option<&[StackChange]>,
    max_stack_size: usize,
) -> SyncPlan {
    let mut plan = SyncPlan {
        files_to_remove: Vec::new(),
        files_to_rename: Vec::new(),
        files_to_write: Vec::new(),
        symlink_target: None,
        stack_summary: None,
        clear_error: false,
        error: None,
        warnings: Vec::new(),
    };

    match stack_changes {
        None => {
            // Bookmark-loss detection: if plan files exist, a stack was lost
            if !current_state.entries.is_empty() {
                plan.warnings.push(SyncWarning::BookmarkLost);
            }
        }
        Some(changes) => {
            // Check stack size against max
            if changes.len() > max_stack_size {
                plan.error = Some(format!(
                    "Stack has {} changes (max {}). Refusing to sync. \
                     Is @ in the right place? Consider: jj bookmark set stack -r <change>",
                    changes.len(),
                    max_stack_size
                ));
                return plan;
            }

            // Stack is within bounds — clear any previous error
            plan.clear_error = true;

            // Build lookup set for current stack change IDs
            let current_ids: HashSet<&str> =
                changes.iter().map(|c| c.change_id.as_str()).collect();

            // 1. Identify stale files to remove (changes no longer in stack)
            for entry in &current_state.entries {
                if !current_ids.contains(entry.change_id.as_str()) {
                    plan.files_to_remove.push(entry.filename.clone());
                }
            }

            // 2. For each stack change, plan renames and writes
            let mut current_file: Option<String> = None;

            for (idx, change) in changes.iter().enumerate() {
                let padded_idx = format!("{:02}", idx + 1);
                let target_filename =
                    format!("{}-{}.md", padded_idx, change.change_id);

                // Check if a file exists with the right change ID but wrong
                // index — if so, rename rather than delete+recreate
                if let Some(existing_name) =
                    current_state.id_to_filename.get(&change.change_id)
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
            plan.symlink_target = current_file.clone();

            // 4. Generate .stack summary (pure)
            plan.stack_summary =
                Some(generate_stack_summary(changes, current_file.as_deref()));
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
                eprintln!("jj-plan: WARNING: stack bookmark was lost. Run: jj bookmark set stack -r <change>");
            }
            SyncWarning::StackTooLarge { size, max } => {
                eprintln!(
                    "jj-plan: WARNING: stack has {} changes (max {})",
                    size, max
                );
            }
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

    // Write .stack summary
    if let Some(summary) = &plan.stack_summary {
        write_or_warn(&plan_dir.join(".stack"), summary);
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Generate the `.stack` summary content.
///
/// Format per line: `{here} {status} {NN}-{id} :: {first_line}`
///
/// - `here`: `*` if working copy, space otherwise
/// - `status`: `✓` if done, `~` if has file changes, space otherwise
/// - Two columns are independent (a change can be both `*` and `✓`)
pub fn generate_stack_summary(
    changes: &[StackChange],
    current_file: Option<&str>,
) -> String {
    let mut lines = Vec::with_capacity(changes.len());

    for (idx, change) in changes.iter().enumerate() {
        let padded = format!("{:02}", idx + 1);
        let filename = format!("{}-{}.md", padded, change.change_id);

        let here = if Some(filename.as_str()) == current_file {
            "*"
        } else {
            " "
        };

        let status = if change.is_done() {
            "✓"
        } else if !change.is_empty {
            // F = has file changes → show ~
            "~"
        } else {
            " "
        };

        let first_line = change.first_line();

        lines.push(format!(
            "{} {} {}-{} :: {}",
            here, status, padded, change.change_id, first_line
        ));
    }

    let mut result = lines.join("\n");
    if !result.is_empty() {
        result.push('\n');
    }
    result
}

// ---------------------------------------------------------------------------
// Tests — pure plan_sync tests that need no filesystem
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to build a minimal StackChange for tests.
    fn change(id: &str, desc: &str, is_empty: bool, is_wc: bool) -> StackChange {
        StackChange {
            change_id: id.to_string(),
            description: desc.to_string(),
            is_empty,
            is_working_copy: is_wc,
            bookmarks: vec![],
        }
    }

    fn empty_state() -> CurrentPlanState {
        CurrentPlanState {
            entries: vec![],
            id_to_filename: HashMap::new(),
        }
    }

    fn state_with(files: &[(&str, &str)]) -> CurrentPlanState {
        let entries: Vec<PlanFileEntry> = files
            .iter()
            .map(|(filename, change_id)| PlanFileEntry {
                filename: filename.to_string(),
                change_id: change_id.to_string(),
                path: std::path::PathBuf::from(filename),
            })
            .collect();
        let id_to_filename = entries
            .iter()
            .map(|e| (e.change_id.clone(), e.filename.clone()))
            .collect();
        CurrentPlanState {
            entries,
            id_to_filename,
        }
    }

    // -- generate_stack_summary tests (preserved from previous version) --

    #[test]
    fn test_generate_stack_summary() {
        let changes = vec![
            change("kpqxywon", "Refactor auth middleware", true, false),
            change("mtzrlpvq", "Extract auth module", false, false),
            change("ykvsnxrl", "Implement JWT strategy", true, true),
        ];

        let summary = generate_stack_summary(&changes, Some("03-ykvsnxrl.md"));

        assert!(summary.contains("    01-kpqxywon :: Refactor auth middleware"));
        assert!(summary.contains("  ~ 02-mtzrlpvq :: Extract auth module"));
        assert!(summary.contains("*   03-ykvsnxrl :: Implement JWT strategy"));
    }

    #[test]
    fn test_generate_stack_summary_done_marker() {
        let changes = vec![StackChange {
            change_id: "abcdefgh".to_string(),
            description: "Done task\n\nplan-status: ✅".to_string(),
            is_empty: true,
            is_working_copy: true,
            bookmarks: vec![],
        }];

        let summary = generate_stack_summary(&changes, Some("01-abcdefgh.md"));
        assert!(summary.contains("* ✓ 01-abcdefgh :: Done task"));
    }

    #[test]
    fn test_generate_stack_summary_empty() {
        let changes: Vec<StackChange> = vec![];
        let summary = generate_stack_summary(&changes, None);
        assert!(summary.is_empty());
    }

    // -- plan_sync tests (new — pure, no filesystem needed) --

    #[test]
    fn test_plan_sync_none_stack_no_files() {
        let state = empty_state();
        let plan = plan_sync(&state, None, 50);

        assert!(plan.files_to_remove.is_empty());
        assert!(plan.files_to_write.is_empty());
        assert!(plan.warnings.is_empty());
        assert!(plan.error.is_none());
    }

    #[test]
    fn test_plan_sync_none_stack_with_files_warns_bookmark_lost() {
        let state = state_with(&[("01-abc.md", "abc")]);
        let plan = plan_sync(&state, None, 50);

        assert_eq!(plan.warnings.len(), 1);
        assert_eq!(plan.warnings[0], SyncWarning::BookmarkLost);
    }

    #[test]
    fn test_plan_sync_exceeds_max_sets_error() {
        let state = empty_state();
        let changes = vec![
            change("aaa", "a", true, true),
            change("bbb", "b", true, false),
            change("ccc", "c", true, false),
        ];
        let plan = plan_sync(&state, Some(&changes), 2);

        assert!(plan.error.is_some());
        assert!(plan.error.as_ref().unwrap().contains("3 changes (max 2)"));
        // When error is set, no file ops
        assert!(plan.files_to_write.is_empty());
    }

    #[test]
    fn test_plan_sync_writes_all_files() {
        let state = empty_state();
        let changes = vec![
            change("aaa", "desc A", true, false),
            change("bbb", "desc B", false, true),
        ];
        let plan = plan_sync(&state, Some(&changes), 50);

        assert!(plan.error.is_none());
        assert!(plan.clear_error);
        assert_eq!(plan.files_to_write.len(), 2);
        assert_eq!(plan.files_to_write[0].filename, "01-aaa.md");
        assert_eq!(plan.files_to_write[0].content, "desc A");
        assert_eq!(plan.files_to_write[1].filename, "02-bbb.md");
        assert_eq!(plan.files_to_write[1].content, "desc B");
        assert_eq!(plan.symlink_target, Some("02-bbb.md".to_string()));
    }

    #[test]
    fn test_plan_sync_removes_stale_files() {
        let state = state_with(&[
            ("01-aaa.md", "aaa"),
            ("02-bbb.md", "bbb"),
            ("03-ccc.md", "ccc"),
        ]);
        // Stack now only has aaa and ccc — bbb is stale
        let changes = vec![
            change("aaa", "a", true, true),
            change("ccc", "c", true, false),
        ];
        let plan = plan_sync(&state, Some(&changes), 50);

        assert_eq!(plan.files_to_remove, vec!["02-bbb.md"]);
    }

    #[test]
    fn test_plan_sync_renames_reordered_files() {
        // File exists as 02-aaa.md but should be 01-aaa.md after reorder
        let state = state_with(&[("02-aaa.md", "aaa"), ("01-bbb.md", "bbb")]);
        let changes = vec![
            change("aaa", "a", true, true),
            change("bbb", "b", true, false),
        ];
        let plan = plan_sync(&state, Some(&changes), 50);

        assert_eq!(plan.files_to_rename.len(), 2);
        // aaa: 02 → 01
        assert!(plan.files_to_rename.iter().any(|r| r.from == "02-aaa.md"
            && r.to == "01-aaa.md"));
        // bbb: 01 → 02
        assert!(plan.files_to_rename.iter().any(|r| r.from == "01-bbb.md"
            && r.to == "02-bbb.md"));
    }

    #[test]
    fn test_plan_sync_no_rename_when_index_matches() {
        let state = state_with(&[("01-aaa.md", "aaa")]);
        let changes = vec![change("aaa", "a", true, true)];
        let plan = plan_sync(&state, Some(&changes), 50);

        assert!(plan.files_to_rename.is_empty());
    }

    #[test]
    fn test_plan_sync_stack_summary_generated() {
        let state = empty_state();
        let changes = vec![
            change("aaa", "First plan", true, true),
            change("bbb", "Second plan", false, false),
        ];
        let plan = plan_sync(&state, Some(&changes), 50);

        let summary = plan.stack_summary.as_ref().unwrap();
        assert!(summary.contains("*   01-aaa :: First plan"));
        assert!(summary.contains("  ~ 02-bbb :: Second plan"));
    }
}