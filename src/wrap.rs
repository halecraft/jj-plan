use std::path::Path;

use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::repo::LoadedRepo;
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
/// 3. Reload the repo to pick up the mutation's changes
/// 4. Re-resolve the stack from fresh repo state
/// 5. Sync jj state back to plan files
/// 6. Display the plan stack summary
///
/// Returns the jj command's exit code.
pub fn wrap(
    plan_dir: &PlanDir,
    jj: &JjBinary,
    args: &[String],
    loaded_repo: Option<&mut LoadedRepo>,
) -> crate::error::Result<i32> {
    // 1. Flush all local plan file edits to jj descriptions
    // (pre-mutation — repo snapshot is fresh, jj-lib reads work)
    crate::flush::flush_all(&plan_dir.path, jj, loaded_repo.as_deref());

    // 2. Run the actual jj command with inherited stdio
    let status = jj.run_inherit_strings(args)?;
    let exit_code = status.code().unwrap_or(1);

    // 3-6. Reload repo, re-resolve stack, sync plan files, show stack summary
    // After the CLI mutation, the on-disk repo state has changed. Reload
    // to get a fresh ReadonlyRepo snapshot, then use jj-lib for sync reads.
    if let Some(repo) = loaded_repo {
        repo.reload();
        resolve_and_sync(plan_dir, jj, Some(repo));
    } else {
        resolve_and_sync(plan_dir, jj, None);
    }

    Ok(exit_code)
}

/// Canonical post-mutation sync: resolve stack → sync plan files → show stack.
///
/// This is the single entry point for "re-read jj state and update plan files
/// after a mutation". All command modules should call this instead of
/// maintaining their own `sync_and_show()` helpers.
///
/// Handles:
/// - Normal stack resolution (inclusive bookmark or exclusive trunk)
/// - Ambiguous sibling bookmarks → sets error state
/// - No usable base → passes None to sync (bookmark-loss detection)
///
/// When `loaded_repo` is `Some`, uses jj-lib for in-process reads (fast path).
/// When `None`, falls back to subprocess-based resolution (slow path).
/// Callers should call `loaded_repo.reload()` after any CLI mutation before
/// passing the repo here.
pub fn resolve_and_sync(plan_dir: &PlanDir, jj: &JjBinary, loaded_repo: Option<&LoadedRepo>) {
    let max_stack_size = crate::plan_dir::plan_max();
    let (_stack_base, stack_changes) = resolve_fresh_stack(jj, &plan_dir.path, loaded_repo);
    sync::sync(plan_dir, stack_changes.as_deref(), max_stack_size);
    sync::show_stack(plan_dir);
}

/// Re-resolve the stack base and changes.
///
/// Returns (Option<StackBase>, Option<Vec<StackChange>>).
/// The StackBase is used for error reporting; the Vec<StackChange>
/// is passed to sync().
///
/// If `loaded_repo` is provided, uses jj-lib for in-process reads.
/// Otherwise falls back to subprocess-based resolution.
pub fn resolve_fresh_stack(
    jj: &JjBinary,
    plan_dir: &Path,
    loaded_repo: Option<&LoadedRepo>,
) -> (Option<StackBase>, Option<Vec<stack::StackChange>>) {
    // Resolve stack base: prefer jj-lib, fall back to subprocess
    let base = if let Some(loaded) = loaded_repo {
        crate::repo::resolve_stack_base_lib(loaded)
    } else {
        stack::resolve_stack_base(jj)
    };

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
            // Resolve stack changes: prefer jj-lib, fall back to subprocess
            let changes = if let Some(loaded) = loaded_repo {
                crate::repo::resolve_stack_changes_lib(loaded, base.as_ref().unwrap())
            } else {
                stack::resolve_stack_changes(jj, base.as_ref().unwrap())
            };
            (base, changes)
        }
    }
}