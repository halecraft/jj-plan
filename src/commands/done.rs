use crate::jj_binary::JjBinary;
use crate::markdown::strip_scratch_sections;
use crate::plan_dir::PlanDir;
use crate::repo::LoadedRepo;
use crate::stack::StackChange;

/// Run `jj plan done` — mark one or all plans as done.
///
/// Strips `[scratch]` sections from descriptions (unless `--keep-scratch`)
/// and appends `plan-status: ✅` to the description.
///
/// ## Flags
///
/// - `--stack`: mark all changes in the stack as done
/// - `--keep-scratch`: don't strip `[scratch]` sections
/// - `--dry-run`: show what would be changed without modifying anything
/// - Positional arg: a specific CHANGE_ID to mark done (defaults to `@`)
///
/// When marking a single plan done (the default), if the target is the
/// working copy (`@`), automatically advances to the next undone plan.
pub fn run_done(jj: &JjBinary, plan_dir: &PlanDir, args: &[String], loaded_repo: &mut LoadedRepo) -> crate::error::Result<i32> {
    // ------------------------------------------------------------------
    // 1. Parse args
    // ------------------------------------------------------------------
    let mut do_stack = false;
    let mut keep_scratch = false;
    let mut dry_run = false;
    let mut target_id: Option<String> = None;

    for arg in args {
        match arg.as_str() {
            "--stack" => do_stack = true,
            "--keep-scratch" => keep_scratch = true,
            "--dry-run" => dry_run = true,
            _ => target_id = Some(arg.clone()),
        }
    }

    // ------------------------------------------------------------------
    // 2. Flush local plan edits to jj descriptions
    // ------------------------------------------------------------------
    crate::flush::flush_all(&plan_dir.path, jj, &*loaded_repo);

    // ------------------------------------------------------------------
    // 3. Resolve stack (jj-lib — reload after flush in case flush mutated)
    // ------------------------------------------------------------------
    loaded_repo.reload();
    let base = crate::repo::resolve_stack_base_lib(&*loaded_repo);
    let changes = base
        .as_ref()
        .and_then(|b| crate::repo::resolve_stack_changes_lib(&*loaded_repo, b));

    // ------------------------------------------------------------------
    // 4. Dispatch: --stack or single plan
    // ------------------------------------------------------------------
    if do_stack {
        run_done_stack(jj, plan_dir, changes.as_deref(), keep_scratch, dry_run, loaded_repo)
    } else {
        run_done_single(jj, plan_dir, changes.as_deref(), target_id, keep_scratch, dry_run, loaded_repo)
    }
}

// ---------------------------------------------------------------------------
// --stack flow
// ---------------------------------------------------------------------------

/// Mark every change in the stack as done.
fn run_done_stack(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    changes: Option<&[StackChange]>,
    keep_scratch: bool,
    dry_run: bool,
    loaded_repo: &mut LoadedRepo,
) -> crate::error::Result<i32> {
    let changes = match changes {
        Some(c) => c,
        None => {
            eprintln!("jj plan done --stack: could not resolve stack changes");
            return Ok(1);
        }
    };

    for change in changes {
        let desc = &change.description;

        if dry_run {
            print_dry_run_diff(&change.change_id, desc, keep_scratch);
            continue;
        }

        let cleaned = if keep_scratch {
            desc.clone()
        } else {
            strip_scratch_sections(desc)
        };

        let final_desc = append_done_marker(&cleaned, change.is_done());
        let _ = jj.run_silent(&["describe", "-r", &change.change_id, "-m", &final_desc]);
    }

    if dry_run {
        return Ok(0);
    }

    // Reload after describes, then sync and show stack
    loaded_repo.reload();
    crate::wrap::resolve_and_sync(plan_dir, jj, &loaded_repo);

    // --stack marks everything done, suggest starting a new stack
    eprintln!();
    eprintln!("All plans in stack are done 🎉");
    eprintln!("Start a new stack: jj plan stack [-r REV] [name]");

    Ok(0)
}

// ---------------------------------------------------------------------------
// Single plan flow (default)
// ---------------------------------------------------------------------------

/// Mark a single plan as done, then advance to the next undone plan if the
/// target was the working copy.
fn run_done_single(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    changes: Option<&[StackChange]>,
    target_id: Option<String>,
    keep_scratch: bool,
    dry_run: bool,
    loaded_repo: &mut LoadedRepo,
) -> crate::error::Result<i32> {
    let target = target_id.clone().unwrap_or_else(|| "@".to_string());
    let is_default_target = target_id.is_none(); // targeting working copy

    // Try to find the change in the resolved stack
    let found = changes.and_then(|cs| find_change_in_stack(cs, &target));

    // Read description: from stack if found, otherwise from jj directly
    let (change_id_for_describe, desc, was_done) = match found {
        Some(change) => (
            change.change_id.clone(),
            change.description.clone(),
            change.is_done(),
        ),
        None => {
            // Not found in stack — read description from jj directly
            let desc = match read_description(&*loaded_repo, &target) {
                Some(d) => d,
                None => {
                    eprintln!("jj plan done: could not read description for '{}'", target);
                    return Ok(1);
                }
            };
            let was_done = desc.starts_with("plan-status: ✅")
                || desc.contains("\nplan-status: ✅");
            (target.clone(), desc, was_done)
        }
    };

    // Dry run: show what would be stripped and exit
    if dry_run {
        print_dry_run_diff(&change_id_for_describe, &desc, keep_scratch);
        return Ok(0);
    }

    let cleaned = if keep_scratch {
        desc.clone()
    } else {
        strip_scratch_sections(&desc)
    };

    let final_desc = append_done_marker(&cleaned, was_done);
    let _ = jj.run_silent(&[
        "describe",
        "-r",
        &change_id_for_describe,
        "-m",
        &final_desc,
    ]);

    // If we targeted the working copy (default), advance to the next undone plan
    if is_default_target {
        advance_to_next_undone(jj, plan_dir, loaded_repo);
    }

    // Reload after describe, then sync and show stack
    loaded_repo.reload();
    crate::wrap::resolve_and_sync(plan_dir, jj, &loaded_repo);
    Ok(0)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find a change in the stack, either by working copy marker (for "@") or by
/// change ID prefix match.
fn find_change_in_stack<'a>(changes: &'a [StackChange], target: &str) -> Option<&'a StackChange> {
    if target == "@" {
        changes.iter().find(|c| c.is_working_copy)
    } else {
        // Prefix match: the target might be a prefix of the change_id or vice versa
        changes
            .iter()
            .find(|c| c.change_id.starts_with(target) || target.starts_with(&c.change_id))
    }
}

/// Read a change's description via jj-lib.
fn read_description(loaded_repo: &LoadedRepo, target: &str) -> Option<String> {
    crate::repo::read_description_at(loaded_repo, target)
}

/// Append `plan-status: ✅` to a description if not already present.
fn append_done_marker(desc: &str, already_done: bool) -> String {
    if already_done
        || desc.contains("\nplan-status: ✅")
        || desc.starts_with("plan-status: ✅")
    {
        desc.to_string()
    } else {
        format!("{}\n\nplan-status: ✅", desc.trim_end())
    }
}

/// After marking the current working copy done, re-resolve the stack and
/// advance (`jj edit`) to the next undone change.
///
/// Re-resolves the stack once (after the describe mutation), then searches
/// forward (with wraparound) for the next undone change. After `jj edit`,
/// calls `resolve_and_sync()` exactly once to update plan files.
fn advance_to_next_undone(jj: &JjBinary, plan_dir: &PlanDir, loaded_repo: &mut LoadedRepo) {
    // Re-resolve the stack once after the describe
    loaded_repo.reload();
    let base = crate::repo::resolve_stack_base_lib(&*loaded_repo);
    let changes = base
        .as_ref()
        .and_then(|b| crate::repo::resolve_stack_changes_lib(&*loaded_repo, b));

    let changes = match changes {
        Some(c) => c,
        None => return,
    };

    // Find the current working copy index
    let current_idx = match changes.iter().position(|c| c.is_working_copy) {
        Some(idx) => idx,
        None => return,
    };

    // Search forward then wraparound for the next undone change
    let forward = &changes[current_idx + 1..];
    let wraparound = &changes[..current_idx];
    let next_undone = forward.iter().chain(wraparound.iter()).find(|c| !c.is_done());

    match next_undone {
        Some(change) => {
            let _ = jj.run_inherit(&["edit", "-r", &change.change_id]);
            loaded_repo.reload();
            let max = crate::plan_dir::plan_max();
            let (_base, changes2) = crate::wrap::resolve_fresh_stack(jj, &plan_dir.path, &loaded_repo);
            crate::sync::sync(plan_dir, changes2.as_deref(), max);
        }
        None => {
            eprintln!("All plans in stack are done 🎉");
        }
    }
}

/// Print a dry-run diff for a single change, showing what sections would be
/// stripped and that the done marker would be appended.
fn print_dry_run_diff(change_id: &str, desc: &str, keep_scratch: bool) {
    let cleaned = if keep_scratch {
        desc.to_string()
    } else {
        strip_scratch_sections(desc)
    };

    eprintln!("--- change: {} ---", change_id);

    if !keep_scratch && cleaned != desc {
        // Show the sections that would be stripped
        eprintln!("Would strip scratch sections:");
        // Find lines in `desc` that are NOT in `cleaned`
        let cleaned_lines: std::collections::HashSet<&str> = cleaned.lines().collect();
        for line in desc.lines() {
            if !cleaned_lines.contains(line) {
                eprintln!("  - {}", line);
            }
        }
        eprintln!();
    }

    let already_done =
        desc.starts_with("plan-status: ✅") || desc.contains("\nplan-status: ✅");

    if already_done {
        eprintln!("Already marked done (plan-status: ✅)");
    } else {
        eprintln!("Would append: plan-status: ✅");
    }
    eprintln!();
}