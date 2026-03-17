use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::jj_binary::JjBinary;
use crate::plan_file;
use crate::stack::{batch_read_by_ids, StackChange};

/// Flush ALL local plan file edits to jj descriptions.
///
/// Reads all `NN-CHANGEID.md` files from the plan directory, compares their
/// content against jj descriptions (obtained via a single batch read), and
/// calls `jj describe -r CHANGEID -m CONTENT` for each file that differs.
///
/// This is the Rust equivalent of `__jj_plan_flush_all` from the zsh shim.
///
/// Internally structured as GATHER → PLAN → EXECUTE (FC/IS):
/// - Gather: collect plan files + batch-read jj descriptions (I/O)
/// - Plan: diff file contents against descriptions → `Vec<FlushAction>` (pure)
/// - Execute: shell out `jj describe` for each FlushAction (I/O)
pub fn flush_all(plan_dir: &Path, jj: &JjBinary) {
    // Don't flush if current.md points to error.md (error state)
    if plan_file::is_error_state(plan_dir) {
        return;
    }

    // GATHER — collect plan files and their contents + jj descriptions
    let gathered = gather_flush_state(plan_dir, jj);

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
    /// Map of change_id → file content (read from disk).
    file_contents: HashMap<String, String>,
    /// Map of change_id → jj description (read from jj via batch-read).
    jj_descriptions: HashMap<String, String>,
}

/// Collect plan file contents and corresponding jj descriptions.
fn gather_flush_state(plan_dir: &Path, jj: &JjBinary) -> FlushGatherState {
    let plan_files = plan_file::plan_files_by_id(plan_dir);

    if plan_files.is_empty() {
        return FlushGatherState {
            file_contents: HashMap::new(),
            jj_descriptions: HashMap::new(),
        };
    }

    // Read file contents from disk
    let mut file_contents = HashMap::new();
    for (change_id, path) in &plan_files {
        if let Ok(content) = fs::read_to_string(path) {
            if !content.is_empty() {
                file_contents.insert(change_id.clone(), content);
            }
        }
    }

    // Batch-read jj descriptions for all change IDs
    let change_ids: Vec<&str> = plan_files.keys().map(|s| s.as_str()).collect();
    let jj_descriptions = match batch_read_by_ids(jj, &change_ids) {
        Some(changes) => build_description_map(changes),
        None => HashMap::new(),
    };

    FlushGatherState {
        file_contents,
        jj_descriptions,
    }
}

// ---------------------------------------------------------------------------
// PLAN — pure decision logic, no I/O
// ---------------------------------------------------------------------------

/// A single flush action: write this content to a jj change's description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushAction {
    /// The change ID to describe.
    pub change_id: String,
    /// The new description content (from the plan file).
    pub content: String,
}

/// Pure planning function: given file contents and jj descriptions, compute
/// which changes need their descriptions updated.
///
/// A flush action is produced when:
/// - The file content is non-empty (empty files are skipped)
/// - The change exists in jj (abandoned changes are skipped)
/// - The file content differs from the jj description
fn plan_flush(state: &FlushGatherState) -> Vec<FlushAction> {
    let mut actions = Vec::new();

    for (change_id, file_content) in &state.file_contents {
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
// Helpers
// ---------------------------------------------------------------------------

/// Build a HashMap of change_id → description from a Vec<StackChange>.
fn build_description_map(changes: Vec<StackChange>) -> HashMap<String, String> {
    changes
        .into_iter()
        .map(|c| (c.change_id, c.description))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests — plan_flush is pure and testable without I/O
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn state(
        files: &[(&str, &str)],
        descs: &[(&str, &str)],
    ) -> FlushGatherState {
        FlushGatherState {
            file_contents: files
                .iter()
                .map(|(id, content)| (id.to_string(), content.to_string()))
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
            &[("abc", "hello world")],
            &[("abc", "hello world")],
        );
        let actions = plan_flush(&s);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_plan_flush_produces_action_when_different() {
        let s = state(
            &[("abc", "new content")],
            &[("abc", "old content")],
        );
        let actions = plan_flush(&s);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].change_id, "abc");
        assert_eq!(actions[0].content, "new content");
    }

    #[test]
    fn test_plan_flush_skips_abandoned_change() {
        // File exists for "abc" but jj doesn't know about it (abandoned)
        let s = state(
            &[("abc", "content")],
            &[], // empty — no jj descriptions
        );
        let actions = plan_flush(&s);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_plan_flush_multiple_files_mixed() {
        let s = state(
            &[
                ("aaa", "same"),
                ("bbb", "changed"),
                ("ccc", "also changed"),
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
    fn test_build_description_map() {
        let changes = vec![
            StackChange {
                change_id: "abc".to_string(),
                description: "desc A".to_string(),
                is_empty: true,
                is_working_copy: false,
                bookmarks: vec![],
            },
            StackChange {
                change_id: "def".to_string(),
                description: "desc B".to_string(),
                is_empty: false,
                is_working_copy: true,
                bookmarks: vec![],
            },
        ];

        let map = build_description_map(changes);
        assert_eq!(map.get("abc").unwrap(), "desc A");
        assert_eq!(map.get("def").unwrap(), "desc B");
        assert_eq!(map.len(), 2);
    }
}