use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

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

/// Decode a bookmark name from its filename-encoded form.
///
/// Reverses the encoding applied by `encode_bookmark_for_filename`:
/// `--` is replaced with `/`.
pub fn decode_bookmark_from_filename(encoded: &str) -> String {
    encoded.replace("--", "/")
}

/// Parse a plan filename like `01-feat-auth.md` and return the bookmark name.
/// Returns `None` if the filename doesn't match the expected pattern.
///
/// Pattern: `NN-BOOKMARKNAME.md` where NN is two digits and BOOKMARKNAME
/// is a non-empty string. The bookmark name is decoded from the filename
/// encoding (e.g. `feat--auth` → `feat/auth`).
///
/// Excluded filenames: `error.md`, `current.md`, `.stack`, `template.md`,
/// `problem.md`, and any file that doesn't start with two digits + dash
/// or doesn't end with `.md`.
pub fn parse_plan_filename(name: &str) -> Option<&str> {
    if name.len() > 6
        && name.as_bytes()[0].is_ascii_digit()
        && name.as_bytes()[1].is_ascii_digit()
        && name.as_bytes()[2] == b'-'
        && name.ends_with(".md")
        && name != "error.md"
    {
        // Extract bookmark name (encoded): everything between the dash and .md
        Some(&name[3..name.len() - 3])
    } else {
        None
    }
}

/// A plan file entry from a directory listing.
#[derive(Debug, Clone)]
pub struct PlanFileEntry {
    /// The full filename, e.g. `01-feat-auth.md`.
    pub filename: String,
    /// The bookmark name extracted from the filename (with slashes restored).
    pub bookmark_name: String,
    /// The full path to the file.
    pub path: PathBuf,
}

/// Read the plan directory once and return all plan file entries.
///
/// This is the single point of directory scanning — used by both flush
/// and sync gather phases. Reads the directory exactly once.
///
/// The returned entries have `bookmark_name` decoded from the filename
/// encoding (e.g. `feat--auth` in the filename → `feat/auth` in the field).
pub fn collect_plan_files(plan_dir: &Path) -> Vec<PlanFileEntry> {
    let mut result = Vec::new();

    let entries = match fs::read_dir(plan_dir) {
        Ok(entries) => entries,
        Err(_) => return result,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy().into_owned();

        if let Some(encoded_name) = parse_plan_filename(&name_str) {
            let bookmark_name = decode_bookmark_from_filename(encoded_name);
            result.push(PlanFileEntry {
                filename: name_str,
                bookmark_name,
                path: entry.path(),
            });
        }
    }

    result
}

/// Build a map of bookmark_name → file_path from collected plan files.
///
/// Convenience wrapper for flush, which needs to look up paths by bookmark name.
pub fn plan_files_by_bookmark(plan_dir: &Path) -> HashMap<String, PathBuf> {
    collect_plan_files(plan_dir)
        .into_iter()
        .map(|e| (e.bookmark_name, e.path))
        .collect()
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
    extracted.len() >= 8 && extracted.bytes().all(|b| b >= b'k' && b <= b'z')
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

/// Check if any plan files (NN-*.md) exist in the directory.
///
/// Used for plan-loss detection: if plan files exist but no registered
/// plan bookmarks can be resolved, the plans may have been untracked.
pub fn plan_files_exist(plan_dir: &Path) -> bool {
    if let Ok(entries) = fs::read_dir(plan_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if parse_plan_filename(&name).is_some() {
                return true;
            }
        }
    }
    false
}

/// Check if `current.md` points to `error.md` (error state).
///
/// During error state, flush is skipped to prevent overwriting error info.
pub fn is_error_state(plan_dir: &Path) -> bool {
    let current = plan_dir.join("current.md");

    #[cfg(unix)]
    {
        if let Ok(target) = fs::read_link(&current) {
            return target.file_name().and_then(|n| n.to_str()) == Some("error.md");
        }
    }

    false
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

/// Write bytes to a file, warning on failure.
pub fn write_bytes_or_warn(path: &Path, content: &[u8]) {
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

/// Create a symlink, warning on failure. Unix only.
#[cfg(unix)]
pub fn symlink_or_warn(target: &str, link: &Path) {
    if let Err(e) = std::os::unix::fs::symlink(target, link) {
        eprintln!(
            "jj-plan: warning: failed to create symlink {} → {}: {}",
            link.display(),
            target,
            e
        );
    }
}

/// Copy a file, warning on failure.
#[cfg(not(unix))]
pub fn copy_or_warn(src: &Path, dst: &Path) {
    if let Err(e) = fs::copy(src, dst) {
        eprintln!(
            "jj-plan: warning: failed to copy {} → {}: {}",
            src.display(),
            dst.display(),
            e
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- encode/decode bookmark tests --

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

    #[test]
    fn test_decode_bookmark_no_double_hyphens() {
        assert_eq!(decode_bookmark_from_filename("feat-auth"), "feat-auth");
    }

    #[test]
    fn test_decode_bookmark_with_double_hyphens() {
        assert_eq!(decode_bookmark_from_filename("feat--auth"), "feat/auth");
    }

    #[test]
    fn test_decode_bookmark_multiple_double_hyphens() {
        assert_eq!(
            decode_bookmark_from_filename("user--duane--experiment"),
            "user/duane/experiment"
        );
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        let names = [
            "feat-auth",
            "feat/auth",
            "fix/typo",
            "user/duane/experiment",
            "simple",
            "with.dots",
            "under_score",
        ];
        for name in &names {
            let encoded = encode_bookmark_for_filename(name);
            let decoded = decode_bookmark_from_filename(&encoded);
            assert_eq!(
                &decoded, name,
                "Round-trip failed for bookmark name '{}'",
                name
            );
        }
    }

    // -- parse_plan_filename tests --

    #[test]
    fn test_parse_plan_filename_valid() {
        // Bookmark-named files
        assert_eq!(parse_plan_filename("01-feat-auth.md"), Some("feat-auth"));
        assert_eq!(parse_plan_filename("02-fix-login.md"), Some("fix-login"));
        assert_eq!(
            parse_plan_filename("10-my-feature.md"),
            Some("my-feature")
        );
        assert_eq!(
            parse_plan_filename("99-long-bookmark-name.md"),
            Some("long-bookmark-name")
        );
    }

    #[test]
    fn test_parse_plan_filename_with_encoded_slash() {
        // Filenames with `--` encoding for `/`
        assert_eq!(
            parse_plan_filename("01-feat--auth.md"),
            Some("feat--auth")
        );
        assert_eq!(
            parse_plan_filename("03-user--duane--exp.md"),
            Some("user--duane--exp")
        );
    }

    #[test]
    fn test_parse_plan_filename_legacy_change_id() {
        // Old-style change-ID filenames still parse (same pattern)
        assert_eq!(parse_plan_filename("01-kpqxywon.md"), Some("kpqxywon"));
        assert_eq!(parse_plan_filename("02-mtzrlpvq.md"), Some("mtzrlpvq"));
    }

    #[test]
    fn test_parse_plan_filename_invalid() {
        assert_eq!(parse_plan_filename("current.md"), None);
        assert_eq!(parse_plan_filename("error.md"), None);
        assert_eq!(parse_plan_filename(".stack"), None);
        assert_eq!(parse_plan_filename("ab-test.md"), None); // non-digit prefix
        assert_eq!(parse_plan_filename("1-short.md"), None); // single digit
        assert_eq!(parse_plan_filename("01-.md"), None); // empty name (len <= 6)
    }

    // -- plan_files_exist tests --

    #[test]
    fn test_plan_files_exist_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!plan_files_exist(tmp.path()));
    }

    #[test]
    fn test_plan_files_exist_with_plan_file() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-feat-auth.md"), "content").unwrap();
        assert!(plan_files_exist(tmp.path()));
    }

    #[test]
    fn test_plan_files_exist_with_non_plan_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("current.md"), "content").unwrap();
        fs::write(tmp.path().join(".stack"), "content").unwrap();
        assert!(!plan_files_exist(tmp.path()));
    }

    // -- collect_plan_files tests --

    #[test]
    fn test_collect_plan_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-feat-auth.md"), "a").unwrap();
        fs::write(tmp.path().join("02-fix-login.md"), "b").unwrap();
        fs::write(tmp.path().join("current.md"), "link").unwrap();
        fs::write(tmp.path().join(".stack"), "stack").unwrap();

        let files = collect_plan_files(tmp.path());
        assert_eq!(files.len(), 2);

        let names: Vec<&str> = files.iter().map(|f| f.bookmark_name.as_str()).collect();
        assert!(names.contains(&"feat-auth"));
        assert!(names.contains(&"fix-login"));
    }

    #[test]
    fn test_collect_plan_files_with_slash_encoding() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-feat--auth.md"), "a").unwrap();

        let files = collect_plan_files(tmp.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].bookmark_name, "feat/auth");
        assert_eq!(files[0].filename, "01-feat--auth.md");
    }

    // -- error state tests --

    #[test]
    fn test_is_error_state_no_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_error_state(tmp.path()));
    }

    #[test]
    #[cfg(unix)]
    fn test_is_error_state_with_error_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("error.md"), "error").unwrap();
        std::os::unix::fs::symlink("error.md", tmp.path().join("current.md")).unwrap();
        assert!(is_error_state(tmp.path()));
    }

    #[test]
    #[cfg(unix)]
    fn test_is_error_state_with_normal_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-feat-auth.md"), "plan").unwrap();
        std::os::unix::fs::symlink("01-feat-auth.md", tmp.path().join("current.md")).unwrap();
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
    fn test_migrate_idempotent_on_bookmark_files() {
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

    // -- plan_files_by_bookmark tests --

    #[test]
    fn test_plan_files_by_bookmark() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-feat-auth.md"), "x").unwrap();
        fs::write(tmp.path().join("02-fix-login.md"), "y").unwrap();

        let map = plan_files_by_bookmark(tmp.path());
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("feat-auth"));
        assert!(map.contains_key("fix-login"));
    }

    #[test]
    fn test_plan_files_by_bookmark_with_slashes() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-feat--auth.md"), "x").unwrap();
        fs::write(tmp.path().join("02-user--duane--exp.md"), "y").unwrap();

        let map = plan_files_by_bookmark(tmp.path());
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("feat/auth"));
        assert!(map.contains_key("user/duane/exp"));
    }
}