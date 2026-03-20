use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::jj_binary::JjBinary;
use crate::plan_file;
use crate::workspace::Workspace;

/// Flush ALL local plan file edits to jj descriptions.
///
/// Reads all `NN-BOOKMARKNAME.md` files from the plan directory, resolves
/// each bookmark name to a change ID via the workspace, compares file
/// content against jj descriptions, and calls `jj describe -r CHANGEID -m
/// CONTENT` for each file that differs.
///
/// Internally structured as GATHER → PLAN → EXECUTE (FC/IS):
/// - Gather: collect plan files, resolve bookmark→change_id, batch-read descriptions (I/O)
/// - Plan: diff file contents against descriptions → `Vec<FlushAction>` (pure)
/// - Execute: shell out `jj describe` for each FlushAction (I/O)
pub fn flush_all(plan_dir: &Path, jj: &JjBinary, workspace: &Workspace) {
    // Don't flush if current.md points to error.md (error state)
    if plan_file::is_error_state(plan_dir) {
        return;
    }

    // GATHER — collect plan files and their contents + jj descriptions
    let gathered = gather_flush_state(plan_dir, workspace);

    // PLAN — pure diff logic, no I/O
    let actions = plan_flush(&gathered);

    // EXECUTE — shell out to jj describe for each changed file
    execute_flush(jj, &actions);
}

// ---------------------------------------------------------------------------
// GATHER — read filesystem and jj state
// ---------------------------------------------------------------------------

/// All the data needed to compute flush actions, collected in one pass.
struct FlushGatherState {
    /// Map of bookmark_name → file content (read from disk).
    file_contents: HashMap<String, String>,
    /// Map of bookmark_name → change_id (resolved from workspace bookmarks).
    bookmark_to_change_id: HashMap<String, String>,
    /// Map of change_id → jj description (read from jj via batch-read).
    jj_descriptions: HashMap<String, String>,
}

/// Collect plan file contents and corresponding jj descriptions.
///
/// Resolution chain: plan filename → bookmark name → change ID → description.
fn gather_flush_state(plan_dir: &Path, workspace: &Workspace) -> FlushGatherState {
    let plan_files = plan_file::plan_files_by_bookmark(plan_dir);

    if plan_files.is_empty() {
        return FlushGatherState {
            file_contents: HashMap::new(),
            bookmark_to_change_id: HashMap::new(),
            jj_descriptions: HashMap::new(),
        };
    }

    // Read file contents from disk (keyed by bookmark name)
    let mut file_contents = HashMap::new();
    for (bookmark_name, path) in &plan_files {
        if let Ok(content) = fs::read_to_string(path)
            && !content.is_empty() {
                file_contents.insert(bookmark_name.clone(), content);
            }
    }

    // Build bookmark_name → change_id mapping from workspace bookmarks.
    // Only bookmarks that have plan files are included.
    let all_bookmarks = workspace.local_bookmarks();
    let mut bookmark_to_change_id = HashMap::new();
    for bookmark_name in plan_files.keys() {
        if let Some(bm) = all_bookmarks.iter().find(|b| &b.name == bookmark_name) {
            // Use the short change ID (reverse-hex) since that's what
            // `jj describe -r` expects and what gather_descriptions keys on.
            let short_id = workspace
                .resolve_change_id(&bm.change_id)
                .unwrap_or_else(|| bm.change_id[..8.min(bm.change_id.len())].to_string());
            bookmark_to_change_id.insert(bookmark_name.clone(), short_id);
        }
    }

    // Batch-read jj descriptions for all resolved change IDs
    let change_ids: Vec<&str> = bookmark_to_change_id.values().map(|s| s.as_str()).collect();
    let jj_descriptions = if change_ids.is_empty() {
        HashMap::new()
    } else {
        workspace.gather_descriptions(&change_ids)
    };

    FlushGatherState {
        file_contents,
        bookmark_to_change_id,
        jj_descriptions,
    }
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

/// Pure planning function: given file contents and jj descriptions, compute
/// which changes need their descriptions updated.
///
/// A flush action is produced when:
/// - The file content is non-empty (empty files are skipped)
/// - The bookmark resolves to a change ID (deleted bookmarks are skipped)
/// - The change exists in jj (abandoned changes are skipped)
/// - The file content differs from the jj description
fn plan_flush(state: &FlushGatherState) -> Vec<FlushAction> {
    let mut actions = Vec::new();

    for (bookmark_name, file_content) in &state.file_contents {
        // Resolve bookmark → change_id
        let change_id = match state.bookmark_to_change_id.get(bookmark_name) {
            Some(id) => id,
            None => continue, // bookmark was deleted externally
        };

        // Skip if change no longer exists in jj (abandoned externally)
        let jj_desc = match state.jj_descriptions.get(change_id.as_str()) {
            Some(desc) => desc,
            None => continue,
        };

        // Only flush if content differs
        if file_content != jj_desc {
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
fn execute_flush(jj: &JjBinary, actions: &[FlushAction]) {
    for action in actions {
        // Ignore errors — best-effort flush (matches zsh: 2>/dev/null)
        let _ = jj.run_silent(&[
            "describe",
            "-r",
            &action.change_id,
            "-m",
            &action.content,
        ]);
    }
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
    fn state(
        files: &[(&str, &str)],
        bm_to_id: &[(&str, &str)],
        descs: &[(&str, &str)],
    ) -> FlushGatherState {
        FlushGatherState {
            file_contents: files
                .iter()
                .map(|(name, content)| (name.to_string(), content.to_string()))
                .collect(),
            bookmark_to_change_id: bm_to_id
                .iter()
                .map(|(name, id)| (name.to_string(), id.to_string()))
                .collect(),
            jj_descriptions: descs
                .iter()
                .map(|(id, desc)| (id.to_string(), desc.to_string()))
                .collect(),
        }
    }

    #[test]
    fn test_plan_flush_no_changes_when_matching() {
        let s = state(
            &[("feat-auth", "hello world")],
            &[("feat-auth", "abc")],
            &[("abc", "hello world")],
        );
        let actions = plan_flush(&s);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_plan_flush_produces_action_when_different() {
        let s = state(
            &[("feat-auth", "new content")],
            &[("feat-auth", "abc")],
            &[("abc", "old content")],
        );
        let actions = plan_flush(&s);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].change_id, "abc");
        assert_eq!(actions[0].content, "new content");
    }

    #[test]
    fn test_plan_flush_skips_abandoned_change() {
        // File exists for "feat-auth", bookmark resolves to "abc",
        // but jj doesn't know about change "abc" (abandoned)
        let s = state(
            &[("feat-auth", "content")],
            &[("feat-auth", "abc")],
            &[], // empty — no jj descriptions
        );
        let actions = plan_flush(&s);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_plan_flush_skips_deleted_bookmark() {
        // File exists for "feat-auth" but the bookmark no longer exists
        // in the workspace (no entry in bookmark_to_change_id)
        let s = state(
            &[("feat-auth", "content")],
            &[], // no bookmark mapping
            &[],
        );
        let actions = plan_flush(&s);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_plan_flush_multiple_files_mixed() {
        let s = state(
            &[
                ("feat-auth", "same"),
                ("feat-session", "changed"),
                ("feat-api", "also changed"),
            ],
            &[
                ("feat-auth", "aaa"),
                ("feat-session", "bbb"),
                ("feat-api", "ccc"),
            ],
            &[
                ("aaa", "same"),
                ("bbb", "original"),
                ("ccc", "was this"),
            ],
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
        // Verify the full chain: bookmark name → change ID → description match
        let s = state(
            &[("fix/login", "updated fix")],
            &[("fix/login", "xyz123")],
            &[("xyz123", "old fix")],
        );
        let actions = plan_flush(&s);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].change_id, "xyz123");
        assert_eq!(actions[0].content, "updated fix");
    }
}