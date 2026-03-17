use std::path::Path;

use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::stack::{self, StackBase};
use crate::sync;

/// Unified handler for mutating commands: flush → command → sync → show.
///
/// This is the Rust equivalent of `__jj_plan_wrap` from the zsh shim.
/// All mutating jj commands (status, st, new, edit, describe, abandon,
/// and the general catch-all) go through this lifecycle:
///
/// 1. Flush all local plan file edits to jj descriptions
/// 2. Run the actual jj command with inherited stdio
/// 3. Re-resolve the stack (repo state changed after the command)
/// 4. Sync jj state back to plan files
/// 5. Display the plan stack summary
///
/// Returns the jj command's exit code.
pub fn wrap(
    plan_dir: &PlanDir,
    jj: &JjBinary,
    args: &[String],
) -> crate::error::Result<i32> {
    let max_stack_size = crate::plan_dir::plan_max();

    // 1. Flush all local plan file edits to jj descriptions
    crate::flush::flush_all(&plan_dir.path, jj);

    // 2. Run the actual jj command with inherited stdio
    let status = jj.run_inherit_strings(args)?;
    let exit_code = status.code().unwrap_or(1);

    // 3. Re-resolve the stack (repo state may have changed)
    //    We must re-read from jj after the mutation.
    let (_stack_base, stack_changes) = resolve_fresh_stack(jj, &plan_dir.path);

    // 4. Sync jj state back to plan files
    sync::sync(plan_dir, stack_changes.as_deref(), max_stack_size);

    // 5. Display the plan stack summary
    sync::show_stack(plan_dir);

    Ok(exit_code)
}

/// Re-resolve the stack base and changes after a mutation.
///
/// Returns (Option<StackBase>, Option<Vec<StackChange>>).
/// The StackBase is used for error reporting; the Vec<StackChange>
/// is passed to sync().
fn resolve_fresh_stack(
    jj: &JjBinary,
    plan_dir: &Path,
) -> (Option<StackBase>, Option<Vec<stack::StackChange>>) {
    let base = stack::resolve_stack_base(jj);

    match &base {
        None => {
            // No usable stack base. Bookmark-loss detection happens in sync().
            (None, None)
        }
        Some(StackBase::Ambiguous(ids)) => {
            // Ambiguous sibling bookmarks — set error in sync
            sync::set_error(
                plan_dir,
                &format!(
                    "Ambiguous stack: multiple stack/* bookmarks are equidistant ancestors of @. Conflicting change IDs: {}. Advance or remove one so a single nearest ancestor remains.",
                    ids.join(" ")
                ),
            );
            (base, None)
        }
        Some(StackBase::Inclusive(_) | StackBase::Exclusive) => {
            let changes = stack::resolve_stack_changes(jj, base.as_ref().unwrap());
            (base, changes)
        }
    }
}