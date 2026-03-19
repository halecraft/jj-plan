use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::repo::LoadedRepo;
use crate::template;

/// Run `jj plan new` — create a new plan change in the stack.
///
/// Supports positional flags:
/// - `--first`: insert before the first change in the stack
/// - `--last`: insert after the last change in the stack
/// - (default): insert after the current working copy (`@`)
///
/// Any other arguments are forwarded to `jj new`. If `-r`, `-A`,
/// `--insert-after`, `-B`, or `--insert-before` are present, the default
/// `--insert-after @` is suppressed so the user's explicit position wins.
///
/// After creating the change, a placeholder description is set and the
/// plan directory is synced to reflect the new stack state.
pub fn run_new(jj: &JjBinary, plan_dir: &PlanDir, args: &[String], loaded_repo: &mut LoadedRepo) -> crate::error::Result<i32> {
    // ------------------------------------------------------------------
    // 1. Parse args
    // ------------------------------------------------------------------
    let mut plan_first = false;
    let mut plan_last = false;
    let mut has_explicit_position = false;
    let mut jj_args: Vec<&str> = Vec::new();

    for arg in args {
        match arg.as_str() {
            "--first" => plan_first = true,
            "--last" => plan_last = true,
            "-r" | "-A" | "--insert-after" | "-B" | "--insert-before" => {
                has_explicit_position = true;
                jj_args.push(arg.as_str());
            }
            _ => {
                jj_args.push(arg.as_str());
            }
        }
    }

    if plan_first && plan_last {
        eprintln!("jj plan new: cannot specify both --first and --last");
        return Ok(1);
    }

    // ------------------------------------------------------------------
    // 2. Flush local plan edits to jj descriptions
    // ------------------------------------------------------------------
    crate::flush::flush_all(&plan_dir.path, jj, &*loaded_repo);

    // ------------------------------------------------------------------
    // 3. Resolve stack if --first or --last
    // ------------------------------------------------------------------
    if plan_first || plan_last {
        loaded_repo.reload();
        let base = match crate::repo::resolve_stack_base_lib(&*loaded_repo) {
            Some(b) => b,
            None => {
                eprintln!("jj plan new: could not resolve stack base");
                return Ok(1);
            }
        };
        let changes = match crate::repo::resolve_stack_changes_lib(&*loaded_repo, &base) {
            Some(c) => c,
            None => {
                eprintln!("jj plan new: could not resolve stack changes");
                return Ok(1);
            }
        };

        if plan_first {
            // -------------------------------------------------------
            // 4. --first: insert before the first change
            // -------------------------------------------------------
            let first_id = &changes[0].change_id;

            let mut cmd_args: Vec<&str> = vec!["new", "--insert-before", first_id.as_str()];
            cmd_args.extend_from_slice(&jj_args);

            let new_id = match create_change_and_describe(jj, plan_dir, &cmd_args, loaded_repo)? {
                Some(id) => id,
                None => return Ok(1),
            };

            // Move stack bookmark to the new first change
            if let Some(bm_name) = find_stack_bookmark(&changes[0].bookmarks) {
                let _ = jj.run_silent(&["bookmark", "set", &bm_name, "-r", "@", "-B"]);
            }

            return finish(jj, plan_dir, &new_id, loaded_repo);
        } else {
            // -------------------------------------------------------
            // 5. --last: insert after the last change
            // -------------------------------------------------------
            let last_id = &changes[changes.len() - 1].change_id;

            let mut cmd_args: Vec<&str> = vec!["new", "--insert-after", last_id.as_str()];
            cmd_args.extend_from_slice(&jj_args);

            let new_id = match create_change_and_describe(jj, plan_dir, &cmd_args, loaded_repo)? {
                Some(id) => id,
                None => return Ok(1),
            };

            return finish(jj, plan_dir, &new_id, loaded_repo);
        }
    }

    // ------------------------------------------------------------------
    // 6. Default path (no --first / --last)
    // ------------------------------------------------------------------
    let mut cmd_args: Vec<&str> = vec!["new"];
    if !has_explicit_position {
        cmd_args.push("--insert-after");
        cmd_args.push("@");
    }
    cmd_args.extend_from_slice(&jj_args);

    let new_id = match create_change_and_describe(jj, plan_dir, &cmd_args, loaded_repo)? {
        Some(id) => id,
        None => return Ok(1),
    };

    finish(jj, plan_dir, &new_id, loaded_repo)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a jj change via `jj new` and set a templated description on it.
///
/// This is the shared core for all three `run_new` paths (`--first`, `--last`,
/// default). It includes a before/after guard: the WC change ID is captured
/// before and after the `jj new` call. If the ID is unchanged (meaning `jj new`
/// exited 0 without actually creating a change — e.g. because `--help` was
/// passed through), the function aborts instead of destructively describing the
/// existing working copy.
///
/// Returns `Ok(Some(new_change_id))` on success, `Ok(None)` if `jj new` failed
/// or the WC didn't change.
fn create_change_and_describe(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    cmd_args: &[&str],
    loaded_repo: &mut LoadedRepo,
) -> crate::error::Result<Option<String>> {
    // 1. Capture WC change ID before
    loaded_repo.reload();
    let wc_before = read_current_change_id(&*loaded_repo);

    // 2. Run `jj new ...`
    let status = jj.run_inherit(cmd_args)?;
    if !status.success() {
        return Ok(None);
    }

    // 3. Reload and read WC change ID after
    loaded_repo.reload();
    let new_id = match read_current_change_id(&*loaded_repo) {
        Some(id) => id,
        None => {
            eprintln!("jj plan new: could not read new change ID");
            return Ok(None);
        }
    };

    // 4. Before/after guard: bail if WC didn't actually change
    if wc_before.as_deref() == Some(new_id.as_str()) {
        eprintln!("jj plan new: jj new exited 0 but working copy did not change — aborting");
        return Ok(None);
    }

    // 5. Set templated description
    let description = template::render_template(&plan_dir.path, &new_id);
    let _ = jj.run_silent(&["describe", "-m", &description]);

    Ok(Some(new_id))
}

/// Read the current working-copy change ID (shortest 8-char prefix).
fn read_current_change_id(loaded_repo: &LoadedRepo) -> Option<String> {
    crate::repo::read_change_id_at_wc(loaded_repo)
}

/// Find the first bookmark that is exactly `"stack"` or starts with `"stack/"`.
fn find_stack_bookmark(bookmarks: &[String]) -> Option<String> {
    bookmarks
        .iter()
        .find(|b| *b == "stack" || b.starts_with("stack/"))
        .cloned()
}

/// Sync plan files, print the creation message, and show the stack.
///
/// Shared epilogue for all three paths (--first, --last, default).
fn finish(jj: &JjBinary, plan_dir: &PlanDir, new_id: &str, loaded_repo: &mut LoadedRepo) -> crate::error::Result<i32> {
    eprintln!("Created plan change: jj:{}", new_id);
    loaded_repo.reload();
    crate::wrap::resolve_and_sync(plan_dir, jj, &loaded_repo);
    Ok(0)
}