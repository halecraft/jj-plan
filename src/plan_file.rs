use std::fs;
use std::path::{Path, PathBuf};

use crate::types::PlanRegistry;

/// Encode a bookmark name for use in a plan filename.
///
/// Forward slashes (`/`) in bookmark names (e.g. `feat/auth`) are encoded
/// as `--` since `/` is a path separator and cannot appear in filenames.
///
/// Note: this encoding is not collision-free — a bookmark literally named
/// `feat--auth` would collide with the encoded form of `feat/auth`. This
/// edge case is documented; callers that create bookmarks (`jj plan new`)
/// should reject `--` in bookmark names if collision-free round-tripping
/// is required.
pub fn encode_bookmark_for_filename(name: &str) -> String {
    name.replace('/', "--")
}

/// Compute a single-letter filesystem sort key from a position-from-tip index.
///
/// `position_from_tip` is 0 for the tip (working copy), 1 for the next
/// toward trunk, etc. Returns `'a'` for 0, `'b'` for 1, ..., `'z'` for 25.
/// Positions ≥ 25 are clamped to `'z'`.
///
/// The letter ensures `ls .jj-plan/` shows tip first (matching `jj stack`
/// display order), while the numeric dependency index in the filename
/// retains its dependency-chain meaning (`01` = trunk-nearest).
pub fn sort_letter(position_from_tip: usize) -> char {
    let offset = position_from_tip.min(25) as u8;
    (b'a' + offset) as char
}

/// Build a plan filename in the `L-NN-ENCODED.md` format.
///
/// - `dependency_index`: 0-based, trunk-nearest = 0.
/// - `num_total`: total number of plans in the stack.
/// - `encoded_bookmark`: bookmark name already encoded via `encode_bookmark_for_filename`.
///
/// The letter prefix controls filesystem sort order (tip = `'a'`, sorts first).
/// The numeric index carries semantic meaning (`01` = trunk-nearest = first to merge,
/// matches `jj plan go 1`).
///
/// Example for a 3-plan stack:
/// - `dependency_index=0` (trunk-nearest) → `c-01-feat-auth.md`
/// - `dependency_index=1` (middle)        → `b-02-feat-session.md`
/// - `dependency_index=2` (tip)           → `a-03-feat-api.md`
pub fn format_plan_filename(dependency_index: usize, num_total: usize, encoded_bookmark: &str) -> String {
    let position_from_tip = num_total.saturating_sub(1).saturating_sub(dependency_index);
    let letter = sort_letter(position_from_tip);
    let padded = format!("{:02}", dependency_index + 1);
    format!("{letter}-{padded}-{encoded_bookmark}.md")
}

/// Parse a plan filename and return the encoded bookmark portion.
///
/// Returns `None` if the filename doesn't match a recognized pattern.
///
/// Accepts two formats:
/// - **New**: `L-NN-ENCODED.md` — single lowercase letter, dash, two digits,
///   dash, encoded bookmark, `.md`. Example: `a-03-feat-api.md`.
/// - **Legacy**: `NN-ENCODED.md` — two digits, dash, encoded bookmark, `.md`.
///   Example: `01-feat-auth.md`. Recognized for backward compatibility during
///   migration; the sync pipeline renames these to the new format.
///
/// The returned value is the raw encoded portion — callers use
/// `PlanRegistry::resolve_encoded()` to map it back to the canonical
/// bookmark name.
pub fn parse_plan_filename(name: &str) -> Option<&str> {
    if !name.ends_with(".md") || name == "error.md" {
        return None;
    }

    let bytes = name.as_bytes();

    // New format: L-NN-ENCODED.md (minimum length: 1 + 1 + 2 + 1 + 1 + 3 = 9)
    if bytes.len() > 8
        && bytes[0].is_ascii_lowercase()
        && bytes[1] == b'-'
        && bytes[2].is_ascii_digit()
        && bytes[3].is_ascii_digit()
        && bytes[4] == b'-'
    {
        let encoded = &name[5..name.len() - 3];
        if !encoded.is_empty() {
            return Some(encoded);
        }
    }

    // Legacy format: NN-ENCODED.md (minimum length: 2 + 1 + 1 + 3 = 7)
    if bytes.len() > 6
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b'-'
    {
        let encoded = &name[3..name.len() - 3];
        if !encoded.is_empty() {
            return Some(encoded);
        }
    }

    None
}

/// A plan file entry from a directory listing.
#[derive(Debug, Clone)]
pub struct PlanFileEntry {
    /// The full filename, e.g. `a-03-feat-api.md` (new format) or
    /// `01-feat-auth.md` (legacy format, recognized for migration).
    pub filename: String,
    /// The canonical bookmark name from the PlanRegistry, or the raw
    /// encoded string for orphan files (no matching registry entry).
    pub bookmark_name: String,
    /// The full path to the file.
    pub path: PathBuf,
}

/// Read the plan directory once and return all plan file entries.
///
/// This is the single point of directory scanning — used by both flush
/// and sync gather phases. Reads the directory exactly once.
///
/// Resolution is registry-authoritative: for each `NN-ENCODED.md` file,
/// the `PlanRegistry` is consulted to find the canonical bookmark name.
/// Files with no matching registry entry get `bookmark_name` set to the
/// raw encoded string (orphan/legacy files).
pub fn collect_plan_files(plan_dir: &Path, registry: &PlanRegistry) -> Vec<PlanFileEntry> {
    let mut result = Vec::new();

    let entries = match fs::read_dir(plan_dir) {
        Ok(entries) => entries,
        Err(_) => return result,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy().into_owned();

        if let Some(encoded_name) = parse_plan_filename(&name_str) {
            // Registry-authoritative: look up the canonical bookmark name
            // from the registry instead of guessing via string replacement.
            let bookmark_name = registry
                .resolve_encoded(encoded_name)
                .map(|s| s.to_string())
                .unwrap_or_else(|| encoded_name.to_string());
            result.push(PlanFileEntry {
                filename: name_str,
                bookmark_name,
                path: entry.path(),
            });
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Legacy migration: change-ID-based → bookmark-named files
// ---------------------------------------------------------------------------

/// Check if a plan filename uses the legacy change-ID-based naming convention.
///
/// Legacy pattern: `NN-CHANGEID.md` where the name portion consists of 8+
/// characters drawn exclusively from the jj reverse-hex alphabet (`k-z`).
/// Bookmark names almost never consist solely of these characters, so this
/// heuristic reliably distinguishes legacy files from bookmark-named files.
///
/// Returns `false` for bookmark-named files (which typically contain hyphens,
/// digits, uppercase letters, or characters outside `k-z`).
pub fn is_legacy_filename(name: &str) -> bool {
    let Some(extracted) = parse_plan_filename(name) else {
        return false;
    };
    // Legacy change IDs are 8+ chars of only [k-z] (reverse-hex alphabet)
    extracted.len() >= 8 && extracted.bytes().all(|b| (b'k'..=b'z').contains(&b))
}

/// Migrate legacy change-ID-based plan filenames to bookmark-named filenames.
///
/// Scans the plan directory for files matching the legacy `NN-CHANGEID.md`
/// pattern (detected via `is_legacy_filename`). For each legacy file, resolves
/// the change ID to a bookmark name using the provided `bookmark_for_change_id`
/// lookup function. If a bookmark is found, the file is renamed in place.
///
/// This is called at the beginning of `sync()`, before `gather_current_state()`,
/// so that the rest of the sync pipeline sees only bookmark-named files.
///
/// The migration is idempotent — running it on already-migrated files is a no-op
/// since they won't match `is_legacy_filename`.
///
/// `bookmark_for_change_id` takes a short reverse-hex change ID and returns
/// the bookmark name that points to that change, if any.
pub fn migrate_legacy_filenames<F>(plan_dir: &Path, bookmark_for_change_id: F)
where
    F: Fn(&str) -> Option<String>,
{
    let entries = match fs::read_dir(plan_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !is_legacy_filename(&name_str) {
            continue;
        }

        // Extract the legacy change ID and the NN prefix
        let Some(legacy_id) = parse_plan_filename(&name_str) else {
            continue;
        };
        let prefix = &name_str[..3]; // "NN-"

        // Resolve change ID → bookmark name
        let Some(bookmark_name) = bookmark_for_change_id(legacy_id) else {
            // No bookmark points to this change — leave the file for sync
            // to remove as stale (or for the user to handle manually).
            continue;
        };

        let encoded = encode_bookmark_for_filename(&bookmark_name);
        let new_filename = format!("{}{}.md", prefix, encoded);

        if new_filename == name_str.as_ref() {
            continue; // already correct (shouldn't happen, but be safe)
        }

        let old_path = plan_dir.join(name_str.as_ref());
        let new_path = plan_dir.join(&new_filename);
        eprintln!("jj-plan: migrated {} → {}", name_str, new_filename);
        rename_or_warn(&old_path, &new_path);
    }
}

/// Check if the plan directory is in error state.
///
/// Error state is indicated by the existence of `error.md` in the plan
/// directory. During error state, flush is skipped to prevent overwriting
/// error info.
pub fn is_error_state(plan_dir: &Path) -> bool {
    plan_dir.join("error.md").exists()
}

// ---------------------------------------------------------------------------
// I/O helpers with error observability
//
// These replace `let _ = fs::write(...)` patterns throughout the codebase.
// On failure, they log a warning to stderr rather than silently discarding
// the error. This makes disk-full, permission, and path errors visible
// without crashing the process.
// ---------------------------------------------------------------------------

/// Write content to a file, warning on failure.
pub fn write_or_warn(path: &Path, content: &str) {
    if let Err(e) = fs::write(path, content) {
        eprintln!(
            "jj-plan: warning: failed to write {}: {}",
            path.display(),
            e
        );
    }
}

/// Remove a file, warning on failure (ignores "not found").
pub fn remove_or_warn(path: &Path) {
    if let Err(e) = fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "jj-plan: warning: failed to remove {}: {}",
                path.display(),
                e
            );
        }
}

/// Rename (move) a file, warning on failure.
pub fn rename_or_warn(from: &Path, to: &Path) {
    if let Err(e) = fs::rename(from, to) {
        eprintln!(
            "jj-plan: warning: failed to rename {} → {}: {}",
            from.display(),
            to.display(),
            e
        );
    }
}



#[cfg(test)]
mod tests {
    use super::*;

    // -- encode bookmark tests --

    #[test]
    fn test_encode_bookmark_no_slashes() {
        assert_eq!(encode_bookmark_for_filename("feat-auth"), "feat-auth");
    }

    #[test]
    fn test_encode_bookmark_with_slash() {
        assert_eq!(encode_bookmark_for_filename("feat/auth"), "feat--auth");
    }

    #[test]
    fn test_encode_bookmark_multiple_slashes() {
        assert_eq!(
            encode_bookmark_for_filename("user/duane/experiment"),
            "user--duane--experiment"
        );
    }

    // -- helper: build a registry with given bookmark names --

    fn make_registry(names: &[&str]) -> PlanRegistry {
        let mut reg = PlanRegistry::new();
        for name in names {
            reg.track(crate::types::PlannedBookmark::new(
                name.to_string(),
                "placeholder".to_string(),
            ));
        }
        reg
    }

    // -- sort_letter tests --

    #[test]
    fn test_sort_letter_tip() {
        assert_eq!(sort_letter(0), 'a');
    }

    #[test]
    fn test_sort_letter_sequence() {
        assert_eq!(sort_letter(1), 'b');
        assert_eq!(sort_letter(2), 'c');
        assert_eq!(sort_letter(24), 'y');
        assert_eq!(sort_letter(25), 'z');
    }

    #[test]
    fn test_sort_letter_clamped() {
        assert_eq!(sort_letter(26), 'z');
        assert_eq!(sort_letter(100), 'z');
    }

    // -- format_plan_filename tests --

    #[test]
    fn test_format_plan_filename_single_item() {
        // 1 plan: dependency_index=0 is both trunk-nearest and tip → position_from_tip=0 → 'a'
        assert_eq!(format_plan_filename(0, 1, "feat-auth"), "a-01-feat-auth.md");
    }

    #[test]
    fn test_format_plan_filename_two_items() {
        // 2 plans: [feat-auth (trunk, idx=0), fix-login (tip, idx=1)]
        assert_eq!(format_plan_filename(0, 2, "feat-auth"), "b-01-feat-auth.md");
        assert_eq!(format_plan_filename(1, 2, "fix-login"), "a-02-fix-login.md");
    }

    #[test]
    fn test_format_plan_filename_three_items() {
        // 3 plans: idx=0 trunk-nearest → 'c', idx=1 middle → 'b', idx=2 tip → 'a'
        assert_eq!(format_plan_filename(0, 3, "feat-auth"), "c-01-feat-auth.md");
        assert_eq!(format_plan_filename(1, 3, "feat-session"), "b-02-feat-session.md");
        assert_eq!(format_plan_filename(2, 3, "feat-api"), "a-03-feat-api.md");
    }

    #[test]
    fn test_format_plan_filename_encodes_slash() {
        assert_eq!(format_plan_filename(0, 1, "feat--auth"), "a-01-feat--auth.md");
    }

    // -- parse_plan_filename tests --

    #[test]
    fn test_parse_plan_filename_new_format() {
        // New L-NN-ENCODED.md format
        assert_eq!(parse_plan_filename("a-03-feat-api.md"), Some("feat-api"));
        assert_eq!(parse_plan_filename("b-02-feat-session.md"), Some("feat-session"));
        assert_eq!(parse_plan_filename("c-01-feat-auth.md"), Some("feat-auth"));
        assert_eq!(parse_plan_filename("z-99-long-name.md"), Some("long-name"));
    }

    #[test]
    fn test_parse_plan_filename_new_format_with_encoded_slash() {
        assert_eq!(parse_plan_filename("a-01-feat--auth.md"), Some("feat--auth"));
        assert_eq!(parse_plan_filename("c-03-user--duane--exp.md"), Some("user--duane--exp"));
    }

    #[test]
    fn test_parse_plan_filename_legacy_format() {
        // Legacy NN-ENCODED.md format still recognized
        assert_eq!(parse_plan_filename("01-feat-auth.md"), Some("feat-auth"));
        assert_eq!(parse_plan_filename("02-fix-login.md"), Some("fix-login"));
        assert_eq!(parse_plan_filename("10-my-feature.md"), Some("my-feature"));
        assert_eq!(parse_plan_filename("99-long-bookmark-name.md"), Some("long-bookmark-name"));
    }

    #[test]
    fn test_parse_plan_filename_legacy_with_encoded_slash() {
        assert_eq!(parse_plan_filename("01-feat--auth.md"), Some("feat--auth"));
        assert_eq!(parse_plan_filename("03-user--duane--exp.md"), Some("user--duane--exp"));
    }

    #[test]
    fn test_parse_plan_filename_legacy_change_id() {
        // Old-style change-ID filenames still parse (same legacy pattern)
        assert_eq!(parse_plan_filename("01-kpqxywon.md"), Some("kpqxywon"));
        assert_eq!(parse_plan_filename("02-mtzrlpvq.md"), Some("mtzrlpvq"));
    }

    #[test]
    fn test_parse_plan_filename_invalid() {
        assert_eq!(parse_plan_filename("current.md"), None);
        assert_eq!(parse_plan_filename("error.md"), None);
        assert_eq!(parse_plan_filename(".stack"), None);
        assert_eq!(parse_plan_filename("1-short.md"), None); // single digit, no letter prefix
        assert_eq!(parse_plan_filename("01-.md"), None); // empty name (legacy)
        assert_eq!(parse_plan_filename("a-01-.md"), None); // empty name (new format)
        assert_eq!(parse_plan_filename("A-01-feat.md"), None); // uppercase letter
    }

    // -- collect_plan_files tests --

    #[test]
    fn test_collect_plan_files_new_format() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a-02-fix-login.md"), "b").unwrap();
        fs::write(tmp.path().join("b-01-feat-auth.md"), "a").unwrap();
        fs::write(tmp.path().join("current.md"), "link").unwrap();
        fs::write(tmp.path().join(".stack"), "stack").unwrap();

        let registry = make_registry(&["feat-auth", "fix-login"]);
        let files = collect_plan_files(tmp.path(), &registry);
        assert_eq!(files.len(), 2);

        let names: Vec<&str> = files.iter().map(|f| f.bookmark_name.as_str()).collect();
        assert!(names.contains(&"feat-auth"));
        assert!(names.contains(&"fix-login"));
    }

    #[test]
    fn test_collect_plan_files_legacy_format() {
        // Legacy NN-ENCODED.md files are still collected
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-feat-auth.md"), "a").unwrap();
        fs::write(tmp.path().join("02-fix-login.md"), "b").unwrap();

        let registry = make_registry(&["feat-auth", "fix-login"]);
        let files = collect_plan_files(tmp.path(), &registry);
        assert_eq!(files.len(), 2);

        let names: Vec<&str> = files.iter().map(|f| f.bookmark_name.as_str()).collect();
        assert!(names.contains(&"feat-auth"));
        assert!(names.contains(&"fix-login"));
    }

    #[test]
    fn test_collect_plan_files_slash_via_registry() {
        // File `a-01-feat--auth.md` with registry entry `feat/auth` → resolves to `feat/auth`
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a-01-feat--auth.md"), "a").unwrap();

        let registry = make_registry(&["feat/auth"]);
        let files = collect_plan_files(tmp.path(), &registry);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].bookmark_name, "feat/auth");
        assert_eq!(files[0].filename, "a-01-feat--auth.md");
    }

    #[test]
    fn test_collect_plan_files_double_dash_literal() {
        // File `a-01-feat--auth.md` with registry entry `feat--auth` → resolves to `feat--auth`
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a-01-feat--auth.md"), "a").unwrap();

        let registry = make_registry(&["feat--auth"]);
        let files = collect_plan_files(tmp.path(), &registry);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].bookmark_name, "feat--auth");
    }

    #[test]
    fn test_collect_plan_files_orphan() {
        // File with no matching registry entry → raw encoded string as bookmark_name
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a-01-orphan-file.md"), "a").unwrap();

        let registry = make_registry(&["something-else"]);
        let files = collect_plan_files(tmp.path(), &registry);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].bookmark_name, "orphan-file");
    }

    // -- error state tests --

    #[test]
    fn test_is_error_state_no_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_error_state(tmp.path()));
    }

    #[test]
    fn test_is_error_state_with_error_md_present() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("error.md"), "error").unwrap();
        assert!(is_error_state(tmp.path()));
    }

    #[test]
    fn test_is_error_state_without_error_md() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-feat-auth.md"), "plan").unwrap();
        assert!(!is_error_state(tmp.path()));
    }

    // -- I/O helper tests --

    #[test]
    fn test_write_or_warn_success() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        write_or_warn(&path, "hello");
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn test_remove_or_warn_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        // Should not panic — just silently ignores NotFound
        remove_or_warn(&tmp.path().join("nonexistent.txt"));
    }

    #[test]
    fn test_rename_or_warn_success() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.txt");
        let dst = tmp.path().join("b.txt");
        fs::write(&src, "content").unwrap();
        rename_or_warn(&src, &dst);
        assert!(!src.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), "content");
    }

    // -- is_legacy_filename tests --

    #[test]
    fn test_is_legacy_filename_change_id() {
        // 8+ chars of only [k-z] → legacy change ID
        assert!(is_legacy_filename("01-kpqxywon.md"));
        assert!(is_legacy_filename("02-mtzrlpvq.md"));
        assert!(is_legacy_filename("05-rssuolvr.md"));
        assert!(is_legacy_filename("01-kkkkkkkkk.md")); // all k's, 9 chars
    }

    #[test]
    fn test_is_legacy_filename_bookmark() {
        // Bookmark names contain hyphens, digits, etc. → not legacy
        assert!(!is_legacy_filename("01-feat-auth.md"));
        assert!(!is_legacy_filename("02-fix-login.md"));
        assert!(!is_legacy_filename("01-feat--auth.md")); // encoded slash
    }

    #[test]
    fn test_is_legacy_filename_bookmark_with_digits() {
        assert!(!is_legacy_filename("01-v2-auth.md"));
        assert!(!is_legacy_filename("01-phase2.md"));
    }

    #[test]
    fn test_is_legacy_filename_short_change_id() {
        // Less than 8 chars of [k-z] → too short to be a change ID
        assert!(!is_legacy_filename("01-kpqx.md")); // only 4 chars
        assert!(!is_legacy_filename("01-kkkkkkk.md")); // only 7 chars
    }

    #[test]
    fn test_is_legacy_filename_not_plan_file() {
        assert!(!is_legacy_filename("current.md"));
        assert!(!is_legacy_filename("error.md"));
        assert!(!is_legacy_filename(".stack"));
    }

    #[test]
    fn test_is_legacy_filename_outside_reverse_hex() {
        // Contains chars outside [k-z] → not legacy
        assert!(!is_legacy_filename("01-abcdefgh.md")); // a-j are not in reverse-hex
        assert!(!is_legacy_filename("01-KPQXYWON.md")); // uppercase
    }

    // -- migrate_legacy_filenames tests --

    #[test]
    fn test_migrate_renames_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-kpqxywon.md"), "plan content").unwrap();
        fs::write(tmp.path().join("02-mtzrlpvq.md"), "other content").unwrap();

        // Simulate bookmark resolution: kpqxywon → feat-auth, mtzrlpvq → fix-login
        migrate_legacy_filenames(tmp.path(), |change_id| {
            match change_id {
                "kpqxywon" => Some("feat-auth".to_string()),
                "mtzrlpvq" => Some("fix-login".to_string()),
                _ => None,
            }
        });

        // Old files should be gone, new files should exist
        assert!(!tmp.path().join("01-kpqxywon.md").exists());
        assert!(!tmp.path().join("02-mtzrlpvq.md").exists());
        assert!(tmp.path().join("01-feat-auth.md").exists());
        assert!(tmp.path().join("02-fix-login.md").exists());

        // Content should be preserved
        assert_eq!(
            fs::read_to_string(tmp.path().join("01-feat-auth.md")).unwrap(),
            "plan content"
        );
    }

    #[test]
    fn test_migrate_leaves_unbookmarked_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-kpqxywon.md"), "orphan").unwrap();

        // No bookmark resolves for this change ID
        migrate_legacy_filenames(tmp.path(), |_| None);

        // File should still exist with the old name
        assert!(tmp.path().join("01-kpqxywon.md").exists());
    }

    #[test]
    fn test_migrate_idempotent_on_new_format_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-feat-auth.md"), "already migrated").unwrap();

        // Should not attempt to rename (not a legacy filename)
        migrate_legacy_filenames(tmp.path(), |_| Some("other-name".to_string()));

        // File should be unchanged
        assert!(tmp.path().join("01-feat-auth.md").exists());
        assert_eq!(
            fs::read_to_string(tmp.path().join("01-feat-auth.md")).unwrap(),
            "already migrated"
        );
    }

    #[test]
    fn test_migrate_handles_slash_bookmarks() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-kpqxywon.md"), "content").unwrap();

        migrate_legacy_filenames(tmp.path(), |_| Some("feat/auth".to_string()));

        assert!(!tmp.path().join("01-kpqxywon.md").exists());
        assert!(tmp.path().join("01-feat--auth.md").exists());
    }


}