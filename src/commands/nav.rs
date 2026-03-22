use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::types::{PlanRegistry, Stack, StackResult};
use crate::workspace::Workspace;

/// A navigation target: a segment's tip commit info.
struct NavTarget {
    /// Short change ID (reverse-hex) for `jj edit`.
    change_id: String,
    /// Whether this segment contains the working copy.
    is_working_copy: bool,
    /// Bookmark names on the segment tip.
    bookmarks: Vec<String>,
}

/// Run `jj plan next` — advance `@` to the next plan in the stack.
///
/// Navigates between plan-registered segments. Each segment's tip commit is
/// the navigation target. If `@` is on an unbookmarked WIP commit, the
/// nearest segment is used as the current position anchor.
pub fn plan_next(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    workspace: &mut Workspace,
    registry: &PlanRegistry,
) -> crate::error::Result<i32> {
    // 1. Flush pending edits
    crate::flush::flush_all(&plan_dir.path, jj, workspace, registry);

    // 2. Build stack
    workspace.reload();

    let (targets, current_idx) = match resolve_targets_and_position(workspace, registry) {
        Some(result) => result,
        None => {
            eprintln!("No plans in stack");
            return Ok(1);
        }
    };

    // 3. Check if already at the last plan
    if current_idx >= targets.len() - 1 {
        eprintln!("Already at the last plan");
        workspace.reload();
        crate::wrap::resolve_and_sync(plan_dir, workspace, registry);
        crate::wrap::show_plan_stack(plan_dir, workspace, registry);
        return Ok(0);
    }

    // 4. Navigate to the next segment's tip
    let next_id = &targets[current_idx + 1].change_id;
    let status = jj.run_inherit(&["edit", "-r", next_id])?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }

    // 5. Reload + Sync + show stack
    workspace.reload();
    crate::wrap::resolve_and_sync(plan_dir, workspace, registry);
    crate::wrap::show_plan_stack(plan_dir, workspace, registry);
    Ok(0)
}

/// Run `jj plan prev` — move `@` to the previous plan in the stack.
pub fn plan_prev(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    workspace: &mut Workspace,
    registry: &PlanRegistry,
) -> crate::error::Result<i32> {
    // 1. Flush pending edits
    crate::flush::flush_all(&plan_dir.path, jj, workspace, registry);

    // 2. Build stack
    workspace.reload();

    let (targets, current_idx) = match resolve_targets_and_position(workspace, registry) {
        Some(result) => result,
        None => {
            eprintln!("No plans in stack");
            return Ok(1);
        }
    };

    // 3. Check if already at the first plan
    if current_idx == 0 {
        eprintln!("Already at the first plan");
        workspace.reload();
        crate::wrap::resolve_and_sync(plan_dir, workspace, registry);
        crate::wrap::show_plan_stack(plan_dir, workspace, registry);
        return Ok(0);
    }

    // 4. Navigate to the previous segment's tip
    let prev_id = &targets[current_idx - 1].change_id;
    let status = jj.run_inherit(&["edit", "-r", prev_id])?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }

    // 5. Reload + Sync + show stack
    workspace.reload();
    crate::wrap::resolve_and_sync(plan_dir, workspace, registry);
    crate::wrap::show_plan_stack(plan_dir, workspace, registry);
    Ok(0)
}

/// Run `jj plan go TARGET` — move `@` to a specific plan by index, bookmark
/// name, or change ID.
///
/// Target resolution:
/// 1. If target is a number (1-based index), resolve to that segment's tip.
/// 2. If target matches a bookmark name in the stack, resolve to that segment's tip.
/// 3. Otherwise, pass through to `jj edit` as a change ID / revset.
pub fn plan_go(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    target: &str,
    workspace: &mut Workspace,
    registry: &PlanRegistry,
) -> crate::error::Result<i32> {
    // 1. Flush pending edits
    crate::flush::flush_all(&plan_dir.path, jj, workspace, registry);

    // 2. Build stack
    workspace.reload();

    let targets = match build_nav_targets(workspace, registry) {
        Some(t) if !t.is_empty() => t,
        _ => {
            eprintln!("No plans in stack");
            return Ok(1);
        }
    };

    // 3. Resolve target
    let resolved_id = if let Ok(index) = target.parse::<usize>() {
        // Numeric index (1-based)
        if index == 0 || index > targets.len() {
            eprintln!(
                "jj plan go: index {} is out of range (valid: 1-{})",
                index,
                targets.len()
            );
            return Ok(1);
        }
        targets[index - 1].change_id.clone()
    } else {
        // Try bookmark name match first
        match targets.iter().find(|t| t.bookmarks.iter().any(|b| b == target)) {
            Some(t) => t.change_id.clone(),
            None => {
                // Not a bookmark — try as a change ID or revset.
                // If the target matches a change ID in the stack, use it directly.
                match targets.iter().find(|t| t.change_id == target || t.change_id.starts_with(target)) {
                    Some(t) => t.change_id.clone(),
                    None => {
                        // Fall through to raw `jj edit -r` which handles
                        // arbitrary change IDs and revsets.
                        target.to_string()
                    }
                }
            }
        }
    };

    // 4. Navigate
    let status = jj.run_inherit(&["edit", "-r", &resolved_id])?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }

    // 5. Reload + Sync + show stack
    workspace.reload();
    crate::wrap::resolve_and_sync(plan_dir, workspace, registry);
    crate::wrap::show_plan_stack(plan_dir, workspace, registry);
    Ok(0)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build navigation targets from registry-filtered stack.
///
/// Each target is a segment's tip commit (the bookmarked commit).
/// Only plan-registered segments are included.
fn build_nav_targets(workspace: &Workspace, registry: &PlanRegistry) -> Option<Vec<NavTarget>> {
    let stack_result = crate::stack_builder::build_stack(workspace, Some(registry));
    match stack_result {
        StackResult::Ok(stack) => {
            let targets = stack_to_nav_targets(&stack, workspace);
            if targets.is_empty() {
                None
            } else {
                Some(targets)
            }
        }
        _ => None,
    }
}

/// Convert a `Stack` to a list of `NavTarget`s.
fn stack_to_nav_targets(stack: &Stack, workspace: &Workspace) -> Vec<NavTarget> {
    let mut targets = Vec::new();
    for segment in &stack.segments {
        if let Some(tip) = segment.changes.first() {
            let short_id = workspace
                .short_change_id_from_hex(&tip.change_id)
                .unwrap_or_else(|| {
                    tip.change_id[..8.min(tip.change_id.len())].to_string()
                });
            targets.push(NavTarget {
                change_id: short_id,
                is_working_copy: tip.is_working_copy,
                bookmarks: tip.local_bookmarks.clone(),
            });
        }
    }
    targets
}

/// Resolve the stack and find the current working copy position among
/// plan-registered segments.
///
/// If `@` is directly on a segment's tip, that segment's index is returned.
/// If `@` is on an unbookmarked WIP commit (not in any segment), we find
/// the nearest segment as the anchor:
/// - If `@` is beyond all segments (tip-ward), use the last segment.
/// - Otherwise, use the last segment whose tip is an ancestor of `@`.
///
/// Returns `None` if the stack is empty or has no segments.
fn resolve_targets_and_position(
    workspace: &Workspace,
    registry: &PlanRegistry,
) -> Option<(Vec<NavTarget>, usize)> {
    let stack_result = crate::stack_builder::build_stack(workspace, Some(registry));
    let stack = match stack_result {
        StackResult::Ok(stack) if !stack.segments.is_empty() => stack,
        _ => return None,
    };

    let targets = stack_to_nav_targets(&stack, workspace);
    if targets.is_empty() {
        return None;
    }

    // Try to find @ directly in a segment's tip
    if let Some(idx) = targets.iter().position(|t| t.is_working_copy) {
        return Some((targets, idx));
    }

    // @ is on an unbookmarked WIP commit. Check if any segment's changes
    // (not just the tip) contain the working copy.
    let last_idx = targets.len() - 1;
    for (seg_idx, segment) in stack.segments.iter().enumerate() {
        if segment.changes.iter().any(|c| c.is_working_copy) {
            return Some((targets, seg_idx));
        }
    }

    // @ is not in any segment at all (e.g. unbookmarked WIP at tip after
    // all segments). Default to the last segment as the anchor.
    Some((targets, last_idx))
}