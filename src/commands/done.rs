use crate::jj_binary::JjBinary;
use crate::markdown::strip_scratch_sections;
use crate::plan_dir::PlanDir;
use crate::types::PlanRegistry;
use crate::workspace::Workspace;
use crate::wrap::SyncChangeView;

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
pub fn run_done(jj: &JjBinary, plan_dir: &PlanDir, args: &[String], workspace: &mut Workspace, registry: &PlanRegistry) -> crate::error::Result<i32> {
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
    crate::flush::flush_all(&plan_dir.path, jj, workspace, registry);

    // ------------------------------------------------------------------
    // 3. Resolve stack (jj-lib — reload after flush in case flush mutated)
    // ------------------------------------------------------------------
    workspace.reload();
    let changes = build_sync_views_for_done(workspace, registry);

    // ------------------------------------------------------------------
    // 4. Dispatch: --stack or single plan
    // ------------------------------------------------------------------
    if do_stack {
        run_done_stack(jj, plan_dir, changes.as_deref(), keep_scratch, dry_run, workspace, registry)
    } else {
        run_done_single(jj, plan_dir, changes.as_deref(), target_id, keep_scratch, dry_run, workspace, registry)
    }
}

// ---------------------------------------------------------------------------
// --stack flow
// ---------------------------------------------------------------------------

/// Mark every change in the stack as done.
fn run_done_stack(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    changes: Option<&[SyncChangeView]>,
    keep_scratch: bool,
    dry_run: bool,
    workspace: &mut Workspace,
    registry: &PlanRegistry,
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
    workspace.reload();
    crate::wrap::resolve_sync_and_show(plan_dir, workspace, registry);

    // --stack marks everything done, suggest starting a new stack
    eprintln!();
    eprintln!("All plans in stack are done 🎉");
    eprintln!("Start a new plan: jj plan new <bookmark-name>");

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
    changes: Option<&[SyncChangeView]>,
    target_id: Option<String>,
    keep_scratch: bool,
    dry_run: bool,
    workspace: &mut Workspace,
    registry: &PlanRegistry,
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
            let desc = match read_description(workspace, &target) {
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

    // If we targeted the working copy (default), advance to the next undone plan.
    // advance_to_next_undone() does its own workspace.reload() + resolve_and_sync(),
    // so skip the outer one to avoid a redundant second stack build + sync cycle.
    if is_default_target {
        advance_to_next_undone(jj, plan_dir, workspace, registry);
    } else {
        // Explicit target or no advance needed — reload + sync once.
        workspace.reload();
        crate::wrap::resolve_sync_and_show(plan_dir, workspace, registry);
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find a change in the stack, either by working copy marker (for "@") or by
/// change ID prefix match.
fn find_change_in_stack<'a>(changes: &'a [SyncChangeView], target: &str) -> Option<&'a SyncChangeView> {
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
fn read_description(workspace: &Workspace, target: &str) -> Option<String> {
    workspace.read_description_at(target)
}

/// Append `plan-status: ✅` to a description if not already present.
///
/// If the description already contains a `plan-status:` line with a different
/// value (e.g. `🔴`), it is replaced in-place rather than appending a duplicate.
fn append_done_marker(desc: &str, already_done: bool) -> String {
    if already_done
        || desc.contains("\nplan-status: ✅")
        || desc.starts_with("plan-status: ✅")
    {
        desc.to_string()
    } else {
        // Replace any existing `plan-status: <value>` line (e.g. 🔴, 🟡)
        // rather than appending a second one.
        let mut found_existing = false;
        let replaced: String = desc
            .lines()
            .map(|line| {
                if line.starts_with("plan-status:") {
                    found_existing = true;
                    "plan-status: ✅"
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        if found_existing {
            replaced
        } else {
            format!("{}\n\nplan-status: ✅", desc.trim_end())
        }
    }
}

/// Build SyncChangeView list from the stack for done's consumption.
///
/// Delegates to the shared `wrap::build_sync_views()` so there is a single
/// place to maintain the `StackResult` → `Vec<SyncChangeView>` conversion.
fn build_sync_views_for_done(workspace: &Workspace, registry: &PlanRegistry) -> Option<Vec<SyncChangeView>> {
    crate::wrap::build_sync_views(workspace, registry)
}

/// After marking the current working copy done, re-resolve the stack and
/// advance (`jj edit`) to the next undone change.
///
/// Re-resolves the stack once (after the describe mutation), then searches
/// forward (with wraparound) for the next undone change. After `jj edit`,
/// calls `resolve_and_sync()` exactly once to update plan files.
fn advance_to_next_undone(jj: &JjBinary, plan_dir: &PlanDir, workspace: &mut Workspace, registry: &PlanRegistry) {
    // Re-resolve the stack once after the describe
    workspace.reload();
    let changes = build_sync_views_for_done(workspace, registry);

    let changes = match changes {
        Some(c) => c,
        None => {
            // No stack resolved — still sync to pick up the done marker.
            crate::wrap::resolve_sync_and_show(plan_dir, workspace, registry);
            return;
        }
    };

    // Find the current working copy index
    let current_idx = match changes.iter().position(|c| c.is_working_copy) {
        Some(idx) => idx,
        None => {
            // Can't determine position — sync what we have.
            crate::wrap::resolve_sync_and_show(plan_dir, workspace, registry);
            return;
        }
    };

    // Search forward then wraparound for the next undone change
    let forward = &changes[current_idx + 1..];
    let wraparound = &changes[..current_idx];
    let next_undone = forward.iter().chain(wraparound.iter()).find(|c| !c.is_done());

    match next_undone {
        Some(change) => {
            let _ = jj.run_inherit(&["edit", "-r", &change.change_id]);
            workspace.reload();
            crate::wrap::resolve_sync_and_show(plan_dir, workspace, registry);
        }
        None => {
            // All done — still sync to pick up the done marker we just wrote.
            crate::wrap::resolve_sync_and_show(plan_dir, workspace, registry);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_append_done_marker_no_existing_status() {
        let desc = "feat: add something\n\n# Background\n\nSome details.";
        let result = append_done_marker(desc, false);
        assert!(result.ends_with("plan-status: ✅"));
        assert_eq!(result.matches("plan-status:").count(), 1);
    }

    #[test]
    fn test_append_done_marker_already_done_flag() {
        let desc = "feat: add something\n\nplan-status: ✅";
        let result = append_done_marker(desc, true);
        assert_eq!(result, desc);
    }

    #[test]
    fn test_append_done_marker_already_has_checkmark() {
        let desc = "feat: add something\n\nplan-status: ✅";
        let result = append_done_marker(desc, false);
        assert_eq!(result, desc);
    }

    #[test]
    fn test_append_done_marker_replaces_red_circle() {
        let desc = "feat: add something\n\n# Details\n\nplan-status: 🔴";
        let result = append_done_marker(desc, false);
        assert!(result.contains("plan-status: ✅"));
        assert!(!result.contains("plan-status: 🔴"));
        assert_eq!(result.matches("plan-status:").count(), 1);
    }

    #[test]
    fn test_append_done_marker_replaces_yellow_circle() {
        let desc = "feat: add something\n\nplan-status: 🟡";
        let result = append_done_marker(desc, false);
        assert!(result.contains("plan-status: ✅"));
        assert!(!result.contains("plan-status: 🟡"));
        assert_eq!(result.matches("plan-status:").count(), 1);
    }

    #[test]
    fn test_append_done_marker_preserves_surrounding_content() {
        let desc = "feat: title\n\n# Phase 1\n\nDone.\n\nplan-status: 🔴";
        let result = append_done_marker(desc, false);
        assert!(result.contains("# Phase 1"));
        assert!(result.contains("Done."));
        assert!(result.contains("feat: title"));
        assert!(result.ends_with("plan-status: ✅"));
    }

    #[test]
    fn test_append_done_marker_no_duplicate_when_replacing() {
        // The exact bug: plan-status: 🔴 exists, append_done_marker should
        // replace it, not add a second plan-status: ✅ line.
        let desc = "feat: something\n\nplan-status: 🔴\n";
        let result = append_done_marker(desc, false);
        assert_eq!(result.matches("plan-status:").count(), 1,
            "should have exactly one plan-status line, got: {:?}", result);
    }
}