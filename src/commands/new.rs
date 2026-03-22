use crate::jj_binary::JjBinary;
use crate::plan_dir::{self, PlanDir};
use crate::plan_registry;
use crate::template;
use crate::types::{LogEntry, PlanRegistry, PlannedBookmark};
use crate::workspace::Workspace;

/// Run `jj plan new <bookmark-name>` — create a new plan change with a bookmark.
///
/// This is the primary entry point for creating plans. It:
/// 1. Validates the bookmark name is provided and doesn't already exist
/// 2. Flushes pending plan edits
/// 3. Creates a new jj change (`jj new`)
/// 4. Creates a bookmark on the new change
/// 5. Registers the bookmark in the PlanRegistry
/// 6. Sets a templated description with {{CHANGE_ID}} and {{BOOKMARK}}
/// 7. Syncs plan directory and prints summary
///
/// ## Args
///
/// `args` contains everything after `plan new`, e.g. for
/// `jj plan new feat-auth -r main`, args is `["feat-auth", "-r", "main"]`.
pub fn run_new(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    args: &[String],
    workspace: &mut Workspace,
    registry: &PlanRegistry,
) -> crate::error::Result<i32> {
    // ------------------------------------------------------------------
    // 1. Parse args: bookmark name (required positional) + jj new flags
    // ------------------------------------------------------------------
    let mut bookmark_name: Option<String> = None;
    let mut stack_name: Option<String> = None;
    let mut has_explicit_position = false;
    let mut jj_passthrough: Vec<String> = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--stack" => {
                // --stack <name> flag: create an explicit stack boundary
                if i + 1 < args.len() {
                    i += 1;
                    stack_name = Some(args[i].clone());
                } else {
                    eprintln!("jj plan new: --stack requires a <name> argument");
                    return Ok(1);
                }
            }
            "-r" | "-A" | "--insert-after" | "-B" | "--insert-before" => {
                has_explicit_position = true;
                jj_passthrough.push(arg.clone());
                // These flags take a value argument
                if i + 1 < args.len() {
                    i += 1;
                    jj_passthrough.push(args[i].clone());
                }
            }
            _ => {
                if bookmark_name.is_none() && !arg.starts_with('-') {
                    bookmark_name = Some(arg.clone());
                } else {
                    jj_passthrough.push(arg.clone());
                }
            }
        }
        i += 1;
    }

    let bookmark_name = match bookmark_name {
        Some(name) => name,
        None => {
            eprintln!("jj plan new: missing required <bookmark-name> argument");
            eprintln!();
            eprintln!("Usage: jj plan new <bookmark-name> [--stack <name>] [-r REV] [-A REV] [-B REV]");
            eprintln!();
            eprintln!("Creates a new plan: jj change + bookmark + plan file + registry entry.");
            eprintln!("The bookmark name becomes the plan name (e.g. feat-auth, fix-login).");
            return Ok(1);
        }
    };

    // ------------------------------------------------------------------
    // 2. Validate bookmark doesn't already exist
    // ------------------------------------------------------------------
    let existing_bookmarks = workspace.local_bookmarks();
    if existing_bookmarks.iter().any(|b| b.name == bookmark_name) {
        eprintln!(
            "jj plan new: bookmark '{}' already exists. Use `jj plan track {}` to adopt it as a plan.",
            bookmark_name, bookmark_name
        );
        return Ok(1);
    }

    // Check registry too
    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();
    if registry.is_tracked(&bookmark_name) {
        eprintln!(
            "jj plan new: '{}' is already registered as a plan",
            bookmark_name
        );
        return Ok(1);
    }

    // Check for encoded-name collision (e.g. feat--auth vs feat/auth)
    if let Some(existing) = registry.would_collide(&bookmark_name) {
        let encoded = crate::plan_file::encode_bookmark_for_filename(&bookmark_name);
        eprintln!(
            "jj plan new: bookmark '{}' would collide with existing plan '{}' (both encode to filename '{}'). Rename one of them.",
            bookmark_name, existing, encoded
        );
        return Ok(1);
    }

    // ------------------------------------------------------------------
    // 3. Flush pending plan edits
    // ------------------------------------------------------------------
    crate::flush::flush_all(&plan_dir.path, jj, workspace, registry);

    // ------------------------------------------------------------------
    // 4. Create new jj change — or adopt @ if it's a blank slate
    // ------------------------------------------------------------------
    workspace.reload();

    // Check if @ is adoptable: empty, no bookmarks, no description, and
    // no explicit positioning flag. This avoids creating a redundant empty
    // change on top of one that jj already auto-created (e.g. after push
    // made the previous WC immutable).
    let adopted = if !has_explicit_position {
        workspace.evaluate_revset("@")
            .and_then(|commits| commits.into_iter().next())
            .map(|c| workspace.commit_to_log_entry(&c))
            .is_some_and(|entry| should_adopt_working_copy(&entry, has_explicit_position))
    } else {
        false
    };

    let new_change_id;

    if adopted {
        // Adopt @ in-place — no `jj new` needed
        new_change_id = match workspace.read_change_id_at_wc() {
            Some(id) => id,
            None => {
                eprintln!("jj plan new: could not read working copy change ID");
                return Ok(1);
            }
        };
    } else {
        // Normal path: create a new jj change
        let wc_before = workspace.read_change_id_at_wc();

        let mut new_args: Vec<&str> = vec!["new"];
        if !has_explicit_position {
            new_args.push("--insert-after");
            new_args.push("@");
        }
        for arg in &jj_passthrough {
            new_args.push(arg.as_str());
        }

        let status = jj.run_inherit(&new_args)?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1));
        }

        // Reload and verify WC actually changed
        workspace.reload();
        new_change_id = match workspace.read_change_id_at_wc() {
            Some(id) => id,
            None => {
                eprintln!("jj plan new: could not read new change ID");
                return Ok(1);
            }
        };

        if wc_before.as_deref() == Some(new_change_id.as_str()) {
            eprintln!("jj plan new: jj new exited 0 but working copy did not change — aborting");
            return Ok(1);
        }
    }

    // ------------------------------------------------------------------
    // 5. Create bookmark on the new change
    // ------------------------------------------------------------------
    let (bm_status, _bm_stdout, bm_stderr) =
        jj.run_silent(&["bookmark", "create", &bookmark_name, "-r", "@"])?;

    if !bm_status.success() {
        eprintln!(
            "jj plan new: failed to create bookmark '{}': {}",
            bookmark_name,
            bm_stderr.trim()
        );
        // Roll back the `jj new` (only if we actually created one)
        if !adopted {
            let _ = jj.run_silent(&["undo"]);
        }
        return Ok(bm_status.code().unwrap_or(1));
    }

    // ------------------------------------------------------------------
    // 5b. Create stack base bookmark (if --stack was specified)
    // ------------------------------------------------------------------
    if let Some(ref sname) = stack_name {
        let stack_prefix = plan_dir::stack_prefix();
        let stack_bookmark = format!("{}{}", stack_prefix, sname);

        let (sb_status, _sb_stdout, sb_stderr) =
            jj.run_silent(&["bookmark", "create", &stack_bookmark, "-r", "@"])?;

        if !sb_status.success() {
            eprintln!(
                "jj plan new: failed to create stack base bookmark '{}': {}",
                stack_bookmark,
                sb_stderr.trim()
            );
            // Roll back: delete the plan bookmark and undo the jj new (if created)
            let _ = jj.run_silent(&["bookmark", "delete", &bookmark_name]);
            if !adopted {
                let _ = jj.run_silent(&["undo"]);
            }
            return Ok(sb_status.code().unwrap_or(1));
        }
    }

    // ------------------------------------------------------------------
    // 6. Register in PlanRegistry
    // ------------------------------------------------------------------
    // Get the full standard hex change ID for the registry entry.
    // PlannedBookmark.change_id must be full hex (matching LogEntry.change_id
    // convention from jj:pozrnomw) — NOT short reverse-hex. Look up the
    // bookmark we just created to get the correct encoding.
    workspace.reload();
    let full_change_id = workspace
        .local_bookmarks()
        .iter()
        .find(|b| b.name == bookmark_name)
        .map(|b| b.change_id.clone())
        .unwrap_or_else(|| {
            // Fallback: should not happen since we just created the bookmark,
            // but degrade gracefully with the short ID rather than crashing.
            eprintln!("jj plan new: warning: could not resolve full change ID for bookmark '{}'", bookmark_name);
            new_change_id.clone()
        });

    // Determine stack assignment:
    // 1. If --stack was provided, use the new change's full change ID as stack ID.
    // 2. Otherwise, inherit the stack ID from the current plan at @ (before jj new
    //    moved us). Look up @- (the parent, which was @ before we ran jj new).
    // 3. If no parent plan exists, stack is None (implicit trunk stack).
    let stack_id: Option<String> = if stack_name.is_some() {
        // Explicit --stack: this change IS the stack base
        Some(full_change_id.clone())
    } else {
        // Inherit from parent plan: look up @- in the registry
        inherit_stack_from_parent(workspace, registry)
    };

    let mut registry_mut = plan_registry::load_registry(&repo_root);
    let planned = if let Some(ref sid) = stack_id {
        PlannedBookmark::with_stack(bookmark_name.clone(), full_change_id, sid)
    } else {
        PlannedBookmark::new(bookmark_name.clone(), full_change_id)
    };
    registry_mut.track(planned);
    plan_registry::save_registry(&repo_root, &registry_mut);

    // ------------------------------------------------------------------
    // 7. Set templated description
    // ------------------------------------------------------------------
    let description =
        template::render_template_with_bookmark(&plan_dir.path, &new_change_id, &bookmark_name);
    let _ = jj.run_silent(&["describe", "-m", &description]);

    // ------------------------------------------------------------------
    // 8. Reload, sync, and show
    // ------------------------------------------------------------------
    if let Some(ref sname) = stack_name {
        eprintln!("Created plan: {} in stack '{}' (jj:{})", bookmark_name, sname, new_change_id);
    } else {
        eprintln!("Created plan: {} (jj:{})", bookmark_name, new_change_id);
    }
    workspace.reload();
    let post_registry = plan_registry::load_registry(&repo_root);
    crate::wrap::resolve_and_sync(plan_dir, workspace, &post_registry);
    crate::wrap::show_plan_stack(plan_dir, workspace, &post_registry);

    Ok(0)
}

/// Determine whether `jj plan new` should adopt the current working copy
/// in-place rather than creating a new child change.
///
/// Returns `true` when `@` is a blank slate: empty (no file changes),
/// no local bookmarks, no description, and no explicit positioning flag
/// (`-A`, `-r`, `-B`). This prevents the "pile of empty changes" that
/// accumulates when `jj git push` makes the working copy immutable and
/// jj auto-creates an empty change on top.
fn should_adopt_working_copy(entry: &LogEntry, has_explicit_position: bool) -> bool {
    !has_explicit_position
        && entry.is_empty
        && entry.local_bookmarks.is_empty()
        && entry.description.trim().is_empty()
}

/// Look up the stack ID from the parent change's plan (if any).
///
/// After `jj new`, the working copy is the new change and `@-` is the
/// parent (which was `@` before `jj new`). If `@-` has a bookmark that
/// is tracked in the registry with a `stack` value, inherit that value.
fn inherit_stack_from_parent(workspace: &Workspace, registry: &PlanRegistry) -> Option<String> {
    // Get the parent change's bookmarks by evaluating @-
    let commits = workspace.evaluate_revset("@-")?;
    let parent = commits.first()?;
    let parent_entry = workspace.commit_to_log_entry(parent);

    // Check if any of the parent's bookmarks are tracked with a stack value
    for bm_name in &parent_entry.local_bookmarks {
        if let Some(planned) = registry.get(bm_name) {
            if planned.stack.is_some() {
                return planned.stack.clone();
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_entry(is_empty: bool, bookmarks: &[&str], description: &str) -> LogEntry {
        LogEntry {
            commit_id: "aabb".to_string(),
            change_id: "ccdd".to_string(),
            author_name: String::new(),
            author_email: String::new(),
            description_first_line: description.lines().next().unwrap_or("").to_string(),
            description: description.to_string(),
            parents: vec![],
            local_bookmarks: bookmarks.iter().map(|s| s.to_string()).collect(),
            remote_bookmarks: vec![],
            is_working_copy: true,
            is_empty,
            authored_at: Utc::now(),
            committed_at: Utc::now(),
        }
    }

    #[test]
    fn adopt_empty_unbookmarked_undescribed_change() {
        let entry = make_entry(true, &[], "");
        assert!(should_adopt_working_copy(&entry, false));
    }

    #[test]
    fn do_not_adopt_when_explicit_position_flag() {
        let entry = make_entry(true, &[], "");
        assert!(!should_adopt_working_copy(&entry, true));
    }

    #[test]
    fn do_not_adopt_non_empty_change() {
        let entry = make_entry(false, &[], "");
        assert!(!should_adopt_working_copy(&entry, false));
    }

    #[test]
    fn do_not_adopt_change_with_description() {
        let entry = make_entry(true, &[], "work in progress");
        assert!(!should_adopt_working_copy(&entry, false));
    }

    #[test]
    fn do_not_adopt_change_with_bookmark() {
        let entry = make_entry(true, &["some-bm"], "");
        assert!(!should_adopt_working_copy(&entry, false));
    }
}