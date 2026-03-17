use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Parse a plan filename like `01-kpqxywon.md` and return the change ID.
/// Returns `None` if the filename doesn't match the expected pattern.
///
/// Pattern: `NN-CHANGEID.md` where NN is two digits.
pub fn parse_plan_filename(name: &str) -> Option<&str> {
    if name.len() > 6
        && name.as_bytes()[0].is_ascii_digit()
        && name.as_bytes()[1].is_ascii_digit()
        && name.as_bytes()[2] == b'-'
        && name.ends_with(".md")
        && name != "error.md"
    {
        // Extract change ID: everything between the dash and .md
        Some(&name[3..name.len() - 3])
    } else {
        None
    }
}

/// A plan file entry from a directory listing.
#[derive(Debug, Clone)]
pub struct PlanFileEntry {
    /// The full filename, e.g. `01-kpqxywon.md`.
    pub filename: String,
    /// The change ID extracted from the filename.
    pub change_id: String,
    /// The full path to the file.
    pub path: PathBuf,
}

/// Read the plan directory once and return all plan file entries.
///
/// This is the single point of directory scanning — used by both flush
/// and sync gather phases. Reads the directory exactly once.
pub fn collect_plan_files(plan_dir: &Path) -> Vec<PlanFileEntry> {
    let mut result = Vec::new();

    let entries = match fs::read_dir(plan_dir) {
        Ok(entries) => entries,
        Err(_) => return result,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy().into_owned();

        let change_id = parse_plan_filename(&name_str).map(str::to_string);
        if let Some(change_id) = change_id {
            result.push(PlanFileEntry {
                filename: name_str,
                change_id,
                path: entry.path(),
            });
        }
    }

    result
}

/// Build a map of change_id → file_path from collected plan files.
///
/// Convenience wrapper for flush, which needs to look up paths by change ID.
pub fn plan_files_by_id(plan_dir: &Path) -> HashMap<String, PathBuf> {
    collect_plan_files(plan_dir)
        .into_iter()
        .map(|e| (e.change_id, e.path))
        .collect()
}

/// Check if any plan files (NN-*.md) exist in the directory.
///
/// Used for bookmark-loss detection: if plan files exist but no stack base
/// can be resolved, the stack bookmark was lost.
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

    #[test]
    fn test_parse_plan_filename_valid() {
        assert_eq!(parse_plan_filename("01-kpqxywon.md"), Some("kpqxywon"));
        assert_eq!(parse_plan_filename("02-mtzrlpvq.md"), Some("mtzrlpvq"));
        assert_eq!(parse_plan_filename("10-abcdefgh.md"), Some("abcdefgh"));
        assert_eq!(
            parse_plan_filename("99-longchangeid.md"),
            Some("longchangeid")
        );
    }

    #[test]
    fn test_parse_plan_filename_invalid() {
        assert_eq!(parse_plan_filename("current.md"), None);
        assert_eq!(parse_plan_filename("error.md"), None);
        assert_eq!(parse_plan_filename(".stack"), None);
        assert_eq!(parse_plan_filename("ab-test.md"), None); // non-digit prefix
        assert_eq!(parse_plan_filename("1-short.md"), None); // single digit
        assert_eq!(parse_plan_filename("01-.md"), None); // empty change ID (len <= 6)
    }

    #[test]
    fn test_plan_files_exist_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!plan_files_exist(tmp.path()));
    }

    #[test]
    fn test_plan_files_exist_with_plan_file() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-abcdefgh.md"), "content").unwrap();
        assert!(plan_files_exist(tmp.path()));
    }

    #[test]
    fn test_plan_files_exist_with_non_plan_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("current.md"), "content").unwrap();
        fs::write(tmp.path().join(".stack"), "content").unwrap();
        assert!(!plan_files_exist(tmp.path()));
    }

    #[test]
    fn test_collect_plan_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-aaa.md"), "a").unwrap();
        fs::write(tmp.path().join("02-bbb.md"), "b").unwrap();
        fs::write(tmp.path().join("current.md"), "link").unwrap();
        fs::write(tmp.path().join(".stack"), "stack").unwrap();

        let files = collect_plan_files(tmp.path());
        assert_eq!(files.len(), 2);

        let ids: Vec<&str> = files.iter().map(|f| f.change_id.as_str()).collect();
        assert!(ids.contains(&"aaa"));
        assert!(ids.contains(&"bbb"));
    }

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
        fs::write(tmp.path().join("01-abc.md"), "plan").unwrap();
        std::os::unix::fs::symlink("01-abc.md", tmp.path().join("current.md")).unwrap();
        assert!(!is_error_state(tmp.path()));
    }

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

    #[test]
    fn test_plan_files_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01-xxx.md"), "x").unwrap();
        fs::write(tmp.path().join("02-yyy.md"), "y").unwrap();

        let map = plan_files_by_id(tmp.path());
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("xxx"));
        assert!(map.contains_key("yyy"));
    }
}