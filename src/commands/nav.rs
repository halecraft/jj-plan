use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::workspace::Workspace;
use crate::wrap::SyncChangeView;

/// Run `jj plan next` — advance `@` to the next change in the stack.
///
/// Navigates between bookmarked segments. Each segment's tip commit is
/// the navigation target.
pub fn plan_next(jj: &JjBinary, plan_dir: &PlanDir, workspace: &mut Workspace) -> crate::error::Result<i32> {
    // 1. Flush pending edits
    crate::flush::flush_all(&plan_dir.path, jj, workspace);

    // 2. Resolve stack
    workspace.reload();
    let (changes, current_idx) = match resolve_stack_and_position(workspace) {
        Some(result) => result,
        None => {
            eprintln!("jj plan next: could not resolve stack or find current position");
            return Ok(1);
        }
    };

    // 3. Check if already at the last plan
    if current_idx >= changes.len() - 1 {
        eprintln!("Already at the last plan in the stack");
        workspace.reload();
        crate::wrap::resolve_and_sync(plan_dir, workspace);
        return Ok(0);
    }

    // 4. Navigate to the next change
    let next_id = &changes[current_idx + 1].change_id;
    let status = jj.run_inherit(&["edit", "-r", next_id])?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }

    // 5. Reload + Sync + show stack
    workspace.reload();
    crate::wrap::resolve_and_sync(plan_dir, workspace);
    Ok(0)
}

/// Run `jj plan prev` — move `@` to the previous change in the stack.
pub fn plan_prev(jj: &JjBinary, plan_dir: &PlanDir, workspace: &mut Workspace) -> crate::error::Result<i32> {
    // 1. Flush pending edits
    crate::flush::flush_all(&plan_dir.path, jj, workspace);

    // 2. Resolve stack
    workspace.reload();
    let (changes, current_idx) = match resolve_stack_and_position(workspace) {
        Some(result) => result,
        None => {
            eprintln!("jj plan prev: could not resolve stack or find current position");
            return Ok(1);
        }
    };

    // 3. Check if already at the first plan
    if current_idx == 0 {
        eprintln!("Already at the first plan in the stack");
        workspace.reload();
        crate::wrap::resolve_and_sync(plan_dir, workspace);
        return Ok(0);
    }

    // 4. Navigate to the previous change
    let prev_id = &changes[current_idx - 1].change_id;
    let status = jj.run_inherit(&["edit", "-r", prev_id])?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }

    // 5. Reload + Sync + show stack
    workspace.reload();
    crate::wrap::resolve_and_sync(plan_dir, workspace);
    Ok(0)
}

/// Run `jj plan go TARGET` — move `@` to a specific change by index or change ID.
pub fn plan_go(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    target: &str,
    workspace: &mut Workspace,
) -> crate::error::Result<i32> {
    // 1. Flush pending edits
    crate::flush::flush_all(&plan_dir.path, jj, workspace);

    // 2. Resolve stack
    workspace.reload();
    let changes = match build_sync_views(workspace) {
        Some(c) => c,
        None => {
            eprintln!("jj plan go: could not resolve stack");
            return Ok(1);
        }
    };

    // 3. Parse target: number (1-based index) or change ID
    let resolved_id = if let Ok(index) = target.parse::<usize>() {
        if index == 0 || index > changes.len() {
            eprintln!(
                "jj plan go: index {} is out of range (valid: 1-{})",
                index,
                changes.len()
            );
            return Ok(1);
        }
        changes[index - 1].change_id.clone()
    } else {
        target.to_string()
    };

    // 4. Navigate
    let status = jj.run_inherit(&["edit", "-r", &resolved_id])?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }

    // 5. Reload + Sync + show stack
    workspace.reload();
    crate::wrap::resolve_and_sync(plan_dir, workspace);
    Ok(0)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the stack and find the current working copy position.
///
/// Returns `Some((changes, current_index))` or `None` if the stack can't
/// be resolved or `@` is not found in the stack.
fn resolve_stack_and_position(workspace: &Workspace) -> Option<(Vec<SyncChangeView>, usize)> {
    let changes = build_sync_views(workspace)?;
    let current_idx = changes.iter().position(|c| c.is_working_copy)?;
    Some((changes, current_idx))
}

/// Build SyncChangeView list from the stack for navigation.
fn build_sync_views(workspace: &Workspace) -> Option<Vec<SyncChangeView>> {
    let stack_result = crate::stack_builder::build_stack(workspace);
    match stack_result {
        crate::types::StackResult::Ok(stack) => {
            let mut views = Vec::new();
            for segment in &stack.segments {
                if let Some(tip) = segment.changes.first() {
                    let short_id = workspace
                        .resolve_change_id(&tip.change_id)
                        .unwrap_or_else(|| tip.change_id[..8.min(tip.change_id.len())].to_string());
                    views.push(SyncChangeView {
                        change_id: short_id,
                        description: tip.description.clone(),
                        is_empty: tip.is_empty,
                        is_working_copy: tip.is_working_copy,
                        bookmarks: tip.local_bookmarks.clone(),
                    });
                }
            }
            if views.is_empty() { None } else { Some(views) }
        }
        _ => None,
    }
}