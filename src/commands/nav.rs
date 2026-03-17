use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::stack::StackChange;

/// Run `jj plan next` — advance `@` to the next change in the stack.
///
/// Lifecycle: flush → resolve stack → find current position → navigate → sync → show.
///
/// If `@` is already the last plan, prints a message and stays put.
pub fn plan_next(jj: &JjBinary, plan_dir: &PlanDir) -> crate::error::Result<i32> {
    // 1. Flush pending edits
    crate::flush::flush_all(&plan_dir.path, jj);

    // 2. Resolve stack
    let (changes, current_idx) = match resolve_stack_and_position(jj) {
        Some(result) => result,
        None => {
            eprintln!("jj plan next: could not resolve stack or find current position");
            return Ok(1);
        }
    };

    // 3. Check if already at the last plan
    if current_idx >= changes.len() - 1 {
        eprintln!("Already at the last plan in the stack");
        sync_and_show(jj, plan_dir);
        return Ok(0);
    }

    // 4. Navigate to the next change
    let next_id = &changes[current_idx + 1].change_id;
    let status = jj.run_inherit(&["edit", "-r", next_id])?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }

    // 5. Sync + show stack
    sync_and_show(jj, plan_dir);
    Ok(0)
}

/// Run `jj plan prev` — move `@` to the previous change in the stack.
///
/// Lifecycle: flush → resolve stack → find current position → navigate → sync → show.
///
/// If `@` is already the first plan, prints a message and stays put.
pub fn plan_prev(jj: &JjBinary, plan_dir: &PlanDir) -> crate::error::Result<i32> {
    // 1. Flush pending edits
    crate::flush::flush_all(&plan_dir.path, jj);

    // 2. Resolve stack
    let (changes, current_idx) = match resolve_stack_and_position(jj) {
        Some(result) => result,
        None => {
            eprintln!("jj plan prev: could not resolve stack or find current position");
            return Ok(1);
        }
    };

    // 3. Check if already at the first plan
    if current_idx == 0 {
        eprintln!("Already at the first plan in the stack");
        sync_and_show(jj, plan_dir);
        return Ok(0);
    }

    // 4. Navigate to the previous change
    let prev_id = &changes[current_idx - 1].change_id;
    let status = jj.run_inherit(&["edit", "-r", prev_id])?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }

    // 5. Sync + show stack
    sync_and_show(jj, plan_dir);
    Ok(0)
}

/// Run `jj plan go TARGET` — move `@` to a specific change by index or change ID.
///
/// `target` is either:
/// - A 1-based index (matching the `NN-CHANGEID.md` file numbering)
/// - A change ID (passed through to `jj edit -r`)
///
/// Lifecycle: flush → resolve stack → parse target → navigate → sync → show.
pub fn plan_go(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    target: &str,
) -> crate::error::Result<i32> {
    // 1. Flush pending edits
    crate::flush::flush_all(&plan_dir.path, jj);

    // 2. Resolve stack
    let base = crate::stack::resolve_stack_base(jj);
    let changes = base
        .as_ref()
        .and_then(|b| crate::stack::resolve_stack_changes(jj, b));

    let changes = match changes {
        Some(c) => c,
        None => {
            eprintln!("jj plan go: could not resolve stack");
            return Ok(1);
        }
    };

    // 3. Parse target: number (1-based index) or change ID
    let resolved_id = if let Ok(index) = target.parse::<usize>() {
        // It's a number — validate range (1-based)
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
        // Treat as a change ID — pass through directly
        target.to_string()
    };

    // 4. Navigate to the resolved change
    let status = jj.run_inherit(&["edit", "-r", &resolved_id])?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }

    // 5. Sync + show stack
    sync_and_show(jj, plan_dir);
    Ok(0)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the stack and find the current working copy position.
///
/// Returns `Some((changes, current_index))` or `None` if the stack can't
/// be resolved or `@` is not found in the stack.
fn resolve_stack_and_position(jj: &JjBinary) -> Option<(Vec<StackChange>, usize)> {
    let base = crate::stack::resolve_stack_base(jj)?;
    let changes = crate::stack::resolve_stack_changes(jj, &base)?;
    let current_idx = changes.iter().position(|c| c.is_working_copy)?;
    Some((changes, current_idx))
}

/// Sync the plan directory and show the stack summary.
fn sync_and_show(jj: &JjBinary, plan_dir: &PlanDir) {
    let max = crate::plan_dir::plan_max();
    let base = crate::stack::resolve_stack_base(jj);
    let changes = base
        .as_ref()
        .and_then(|b| crate::stack::resolve_stack_changes(jj, b));
    crate::sync::sync(plan_dir, changes.as_deref(), max);
    crate::sync::show_stack(plan_dir);
}