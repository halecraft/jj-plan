use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
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
pub fn run_new(jj: &JjBinary, plan_dir: &PlanDir, args: &[String]) -> crate::error::Result<i32> {
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
    crate::flush::flush_all(&plan_dir.path, jj);

    // ------------------------------------------------------------------
    // 3. Resolve stack if --first or --last
    // ------------------------------------------------------------------
    if plan_first || plan_last {
        let base = match crate::stack::resolve_stack_base(jj) {
            Some(b) => b,
            None => {
                eprintln!("jj plan new: could not resolve stack base");
                return Ok(1);
            }
        };
        let changes = match crate::stack::resolve_stack_changes(jj, &base) {
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

            let status = jj.run_inherit(&cmd_args)?;
            if !status.success() {
                return Ok(status.code().unwrap_or(1));
            }

            let new_id = match read_current_change_id(jj) {
                Some(id) => id,
                None => {
                    eprintln!("jj plan new: could not read new change ID");
                    return Ok(1);
                }
            };

            // Set templated description
            let description = template::render_template(&plan_dir.path, &new_id);
            let _ = jj.run_silent(&["describe", "-m", &description]);

            // Move stack bookmark to the new first change
            if let Some(bm_name) = find_stack_bookmark(&changes[0].bookmarks) {
                let _ = jj.run_silent(&["bookmark", "set", &bm_name, "-r", "@", "-B"]);
            }

            return finish(jj, plan_dir, &new_id);
        } else {
            // -------------------------------------------------------
            // 5. --last: insert after the last change
            // -------------------------------------------------------
            let last_id = &changes[changes.len() - 1].change_id;

            let mut cmd_args: Vec<&str> = vec!["new", "--insert-after", last_id.as_str()];
            cmd_args.extend_from_slice(&jj_args);

            let status = jj.run_inherit(&cmd_args)?;
            if !status.success() {
                return Ok(status.code().unwrap_or(1));
            }

            let new_id = match read_current_change_id(jj) {
                Some(id) => id,
                None => {
                    eprintln!("jj plan new: could not read new change ID");
                    return Ok(1);
                }
            };

            let description = template::render_template(&plan_dir.path, &new_id);
            let _ = jj.run_silent(&["describe", "-m", &description]);

            return finish(jj, plan_dir, &new_id);
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

    let status = jj.run_inherit(&cmd_args)?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }

    let new_id = match read_current_change_id(jj) {
        Some(id) => id,
        None => {
            eprintln!("jj plan new: could not read new change ID");
            return Ok(1);
        }
    };

    let description = template::render_template(&plan_dir.path, &new_id);
    let _ = jj.run_silent(&["describe", "-m", &description]);

    finish(jj, plan_dir, &new_id)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read the current working-copy change ID (shortest 8-char prefix).
fn read_current_change_id(jj: &JjBinary) -> Option<String> {
    if let Ok((status, stdout, _)) = jj.run_silent(&[
        "log",
        "-r",
        "@",
        "-T",
        "change_id.shortest(8)",
        "--no-graph",
    ]) {
        if status.success() {
            let id = stdout.trim().to_string();
            if !id.is_empty() {
                return Some(id);
            }
        }
    }
    None
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
fn finish(jj: &JjBinary, plan_dir: &PlanDir, new_id: &str) -> crate::error::Result<i32> {
    let max = crate::plan_dir::plan_max();

    let base = crate::stack::resolve_stack_base(jj);
    let changes = base
        .as_ref()
        .and_then(|b| crate::stack::resolve_stack_changes(jj, b));

    crate::sync::sync(plan_dir, changes.as_deref(), max);

    eprintln!("Created plan change: jj:{}", new_id);
    crate::sync::show_stack(plan_dir);

    Ok(0)
}