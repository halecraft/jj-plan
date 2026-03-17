use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::repo::LoadedRepo;

/// Pre-abandon snapshot of the stack bookmark state.
///
/// Captures enough information to recover the bookmark if the abandon
/// removes the change that held it.
struct BookmarkSnapshot {
    /// Shortest unique change ID of the change holding the bookmark.
    /// (Used during snapshot construction for the child query, not read after.)
    _change_id: String,
    /// The `stack` or `stack/*` bookmark name.
    bookmark_name: String,
    /// True if the bookmarked change was the working copy (`@`).
    was_working_copy: bool,
    /// First child of the bookmarked change (if any), for recovery.
    first_child: Option<String>,
}

/// Snapshot the stack bookmark state before an abandon.
///
/// Queries jj for the nearest ancestor of `@` that carries a `stack` or
/// `stack/*` bookmark, records whether it is the working copy, and finds
/// its first child (potential recovery target).
///
/// Returns `None` if there is no stack bookmark in the ancestry of `@`.
fn snapshot_stack_bookmark(jj: &JjBinary) -> Option<BookmarkSnapshot> {
    // Combined query: change_id, bookmarks, working-copy flag
    let revset = r#"heads((bookmarks(exact:"stack") | bookmarks(glob:"stack/*")) & ::@)"#;
    let template = concat!(
        r#"change_id.shortest(8) ++ " ""#,
        r#" ++ bookmarks.join(",") ++ " ""#,
        r#" ++ if(self.contained_in("@"), "C", "-") ++ "\n""#,
    );

    let (status, stdout, _) = jj
        .run_silent(&["log", "-r", revset, "-T", template, "--no-graph"])
        .ok()?;
    if !status.success() {
        return None;
    }

    // Take the first non-empty line (there should be at most one head).
    let line = stdout.lines().find(|l| !l.is_empty())?;

    // Parse: "CHANGE_ID BOOKMARKS WC_FLAG"
    let parts: Vec<&str> = line.splitn(3, ' ').collect();
    if parts.len() < 3 {
        return None;
    }

    let change_id = parts[0].to_string();
    let bookmarks_raw = parts[1];
    let wc_flag = parts[2];

    // Find the specific stack bookmark among possibly many bookmarks.
    let bookmark_name = bookmarks_raw
        .split(',')
        .find(|b| *b == "stack" || b.starts_with("stack/"))?
        .to_string();

    let was_working_copy = wc_flag == "C";

    // Query first child of the bookmarked change (potential recovery target).
    // children() is revset-only, so this is an irreducible separate call.
    let first_child = {
        let child_revset = format!("children({}) ~ {}", change_id, change_id);
        let child_template = r#"change_id.shortest(8) ++ "\n""#;
        match jj.run_silent(&[
            "log",
            "-r",
            &child_revset,
            "-T",
            child_template,
            "--no-graph",
            "--reversed",
        ]) {
            Ok((st, out, _)) if st.success() => {
                
                out.lines().find(|l| !l.is_empty()).map(|s| s.to_string())
            }
            _ => None,
        }
    };

    Some(BookmarkSnapshot {
        _change_id: change_id,
        bookmark_name,
        was_working_copy,
        first_child,
    })
}

/// Attempt to recover a lost stack bookmark after an abandon.
///
/// Recovery strategy (mirrors the zsh shim):
/// 1. If the bookmarked change had a first child that still exists, move
///    the bookmark there.
/// 2. If the abandoned change was the working copy (`@`), jj created a
///    new `@` — move the bookmark to it.
/// 3. Otherwise, warn the user that the bookmark was lost and they need
///    to manually re-set it.
fn attempt_bookmark_recovery(jj: &JjBinary, snapshot: &BookmarkSnapshot) {
    let mut recovery_target: Option<String> = None;

    // 1. Try first child (may have survived the abandon / rebase).
    if let Some(ref child_id) = snapshot.first_child
        && let Ok((status, _, _)) =
            jj.run_silent(&["log", "-r", child_id, "-T", "''", "--no-graph"])
            && status.success() {
                recovery_target = Some(child_id.clone());
            }

    // 2. If no surviving child but the abandoned change was @, use new @.
    if recovery_target.is_none() && snapshot.was_working_copy
        && let Ok((status, stdout, _)) = jj.run_silent(&[
            "log",
            "-r",
            "@",
            "-T",
            "change_id.shortest(8)",
            "--no-graph",
        ])
            && status.success() {
                let id = stdout.trim().to_string();
                if !id.is_empty() {
                    recovery_target = Some(id);
                }
            }

    // 3. Apply recovery or warn.
    match recovery_target {
        Some(target) => {
            let _ = jj.run_silent(&[
                "bookmark",
                "set",
                &snapshot.bookmark_name,
                "-r",
                &target,
                "-B",
            ]);
            eprintln!(
                "jj-plan: moved stack bookmark {} to {} (abandoned change held it)",
                snapshot.bookmark_name, target
            );
        }
        None => {
            eprintln!(
                "jj-plan: WARNING: stack bookmark {} was lost \
                 (abandoned change had no descendants). \
                 Run: jj bookmark set {} -r <change>",
                snapshot.bookmark_name, snapshot.bookmark_name
            );
        }
    }
}

/// Check whether the stack bookmark still exists in the ancestry of `@`.
///
/// Returns `true` if at least one `stack` / `stack/*` bookmark is found.
fn stack_bookmark_survives(jj: &JjBinary) -> bool {
    let revset = r#"heads((bookmarks(exact:"stack") | bookmarks(glob:"stack/*")) & ::@)"#;

    match jj.run_silent(&[
        "log",
        "-r",
        revset,
        "-T",
        r#"change_id.shortest(8)"#,
        "--no-graph",
    ]) {
        Ok((status, stdout, _)) => {
            status.success() && stdout.lines().any(|l| !l.is_empty())
        }
        Err(_) => false,
    }
}

/// Run `jj abandon` with bookmark recovery.
///
/// `args` is the FULL original argument list starting with `"abandon"`,
/// e.g. `["abandon", "CHANGE_ID"]`.
///
/// Lifecycle:
/// 1. Snapshot stack bookmark state (unless `--retain-bookmarks` present)
/// 2. Flush local plan edits to jj descriptions
/// 3. Run the abandon command
/// 4. If the abandon removed the bookmark, attempt recovery
/// 5. Sync + show stack
/// 6. Return the abandon command's exit code
pub fn run_abandon(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    args: &[String],
    mut loaded_repo: Option<&mut LoadedRepo>,
) -> crate::error::Result<i32> {
    // ------------------------------------------------------------------
    // 1. Check for --retain-bookmarks
    // ------------------------------------------------------------------
    let has_retain = args.iter().any(|a| a == "--retain-bookmarks");

    // ------------------------------------------------------------------
    // 2. Snapshot bookmark state before abandon (unless --retain-bookmarks)
    // ------------------------------------------------------------------
    let snapshot = if has_retain {
        None
    } else {
        snapshot_stack_bookmark(jj)
    };

    // ------------------------------------------------------------------
    // 3. Flush local plan edits to jj descriptions
    // ------------------------------------------------------------------
    crate::flush::flush_all(&plan_dir.path, jj, loaded_repo.as_deref());

    // ------------------------------------------------------------------
    // 4. Run the abandon command with all original args
    // ------------------------------------------------------------------
    let status = jj.run_inherit_strings(args)?;
    let exit_code = status.code().unwrap_or(1);

    // ------------------------------------------------------------------
    // 5. If abandon succeeded and we had bookmark info, check survival
    // ------------------------------------------------------------------
    if exit_code == 0
        && let Some(ref snap) = snapshot
            && !stack_bookmark_survives(jj) {
                attempt_bookmark_recovery(jj, snap);
            }

    // ------------------------------------------------------------------
    // 6. Sync plan files + show stack
    // ------------------------------------------------------------------
    if let Some(ref mut repo) = loaded_repo {
        repo.reload();
    }
    crate::wrap::resolve_and_sync(plan_dir, jj, loaded_repo.as_deref());

    Ok(exit_code)
}