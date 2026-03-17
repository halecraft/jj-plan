use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::repo::{BookmarkSnapshot, LoadedRepo};

/// Attempt to recover a lost stack bookmark after an abandon.
///
/// Recovery strategy (mirrors the zsh shim):
/// 1. If the bookmarked change had a first child that still exists, move
///    the bookmark there.
/// 2. If the abandoned change was the working copy (`@`), jj created a
///    new `@` — move the bookmark to it.
/// 3. Otherwise, warn the user that the bookmark was lost and they need
///    to manually re-set it.
fn attempt_bookmark_recovery(jj: &JjBinary, loaded_repo: &LoadedRepo, snapshot: &BookmarkSnapshot) {
    let mut recovery_target: Option<String> = None;

    // 1. Try first child (may have survived the abandon / rebase).
    if let Some(ref child_id) = snapshot.first_child {
        if crate::repo::commit_exists(loaded_repo, child_id) {
            recovery_target = Some(child_id.clone());
        }
    }

    // 2. If no surviving child but the abandoned change was @, use new @.
    if recovery_target.is_none() && snapshot.was_working_copy {
        if let Some(id) = crate::repo::read_change_id_at_wc(loaded_repo) {
            if !id.is_empty() {
                recovery_target = Some(id);
            }
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
    loaded_repo: &mut LoadedRepo,
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
        crate::repo::snapshot_bookmark_state(&*loaded_repo)
    };

    // ------------------------------------------------------------------
    // 3. Flush local plan edits to jj descriptions
    // ------------------------------------------------------------------
    crate::flush::flush_all(&plan_dir.path, jj, &*loaded_repo);

    // ------------------------------------------------------------------
    // 4. Run the abandon command with all original args
    // ------------------------------------------------------------------
    let status = jj.run_inherit_strings(args)?;
    let exit_code = status.code().unwrap_or(1);

    // ------------------------------------------------------------------
    // 5. If abandon succeeded and we had bookmark info, check survival
    // ------------------------------------------------------------------
    if exit_code == 0 {
        if let Some(ref snap) = snapshot {
            loaded_repo.reload();
            if !crate::repo::stack_bookmark_survives(&*loaded_repo) {
                attempt_bookmark_recovery(jj, &*loaded_repo, snap);
            }
        }
    }

    // ------------------------------------------------------------------
    // 6. Sync plan files + show stack
    // ------------------------------------------------------------------
    loaded_repo.reload();
    crate::wrap::resolve_and_sync(plan_dir, jj, &loaded_repo);

    Ok(exit_code)
}