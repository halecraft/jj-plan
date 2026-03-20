use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::plan_registry;
use crate::workspace::Workspace;

/// Run `jj plan untrack <bookmark-name>` — remove a bookmark from plan tracking.
///
/// This removes a bookmark from the PlanRegistry. The bookmark itself is
/// NOT deleted — it remains in the repository. The plan file for this
/// bookmark will be removed by the next sync since it's no longer a plan.
///
/// ## Args
///
/// `args` contains everything after `plan untrack`, e.g. for
/// `jj plan untrack feat-auth`, args is `["feat-auth"]`.
///
/// ## Steps
///
/// 1. Parse args: required positional `<bookmark-name>`
/// 2. Validate bookmark is currently registered
/// 3. Remove `PlannedBookmark` entry from `PlanRegistry`
/// 4. Reload + sync + show stack
/// 5. Print summary
pub fn run_untrack(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    args: &[String],
    workspace: &mut Workspace,
) -> crate::error::Result<i32> {
    // ------------------------------------------------------------------
    // 1. Parse args: bookmark name (required positional)
    // ------------------------------------------------------------------
    let bookmark_name = match args.first() {
        Some(name) if !name.starts_with('-') => name.clone(),
        _ => {
            eprintln!("jj plan untrack: missing required <bookmark-name> argument");
            eprintln!();
            eprintln!("Usage: jj plan untrack <bookmark-name>");
            eprintln!();
            eprintln!("Removes a bookmark from plan tracking. The bookmark itself");
            eprintln!("is not deleted — only the plan registration is removed.");
            return Ok(1);
        }
    };

    // ------------------------------------------------------------------
    // 2. Validate bookmark is currently registered
    // ------------------------------------------------------------------
    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();
    let mut registry = plan_registry::load_registry(&repo_root);

    if !registry.is_tracked(&bookmark_name) {
        eprintln!(
            "jj plan untrack: '{}' is not registered as a plan",
            bookmark_name
        );
        eprintln!();
        let tracked = registry.tracked_names();
        if tracked.is_empty() {
            eprintln!("No plans are currently registered.");
        } else {
            eprintln!("Currently registered plans:");
            for name in &tracked {
                eprintln!("  {}", name);
            }
        }
        return Ok(1);
    }

    // ------------------------------------------------------------------
    // 3. Flush pending plan edits before mutation
    // ------------------------------------------------------------------
    crate::flush::flush_all(&plan_dir.path, jj, workspace);

    // ------------------------------------------------------------------
    // 4. Remove from PlanRegistry
    // ------------------------------------------------------------------
    registry.untrack(&bookmark_name);
    plan_registry::save_registry(&repo_root, &registry);

    // ------------------------------------------------------------------
    // 5. Reload, sync, and show
    // ------------------------------------------------------------------
    eprintln!("Untracked plan: {}", bookmark_name);
    workspace.reload();
    crate::wrap::resolve_and_sync(plan_dir, workspace);

    Ok(0)
}