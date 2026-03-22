use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::plan_registry;
use crate::types::{PlanRegistry, PlannedBookmark};
use crate::workspace::Workspace;

/// Run `jj plan track <bookmark-name>` — adopt an existing bookmark as a plan.
///
/// This registers an existing bookmark in the PlanRegistry so it appears
/// in the `stack.md` summary, gets plan files, and is a navigation target.
///
/// ## Args
///
/// `args` contains everything after `plan track`, e.g. for
/// `jj plan track feat-auth`, args is `["feat-auth"]`.
///
/// ## Steps
///
/// 1. Parse args: required positional `<bookmark-name>`
/// 2. Validate bookmark exists
/// 3. Validate bookmark is not already registered
/// 4. Resolve bookmark's change ID
/// 5. Write `PlannedBookmark` entry to `PlanRegistry`
/// 6. Reload + sync + show stack
/// 7. Print summary
pub fn run_track(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    args: &[String],
    workspace: &mut Workspace,
    registry: &PlanRegistry,
) -> crate::error::Result<i32> {
    // ------------------------------------------------------------------
    // 1. Parse args: bookmark name (required positional)
    // ------------------------------------------------------------------
    let bookmark_name = match args.first() {
        Some(name) if !name.starts_with('-') => name.clone(),
        _ => {
            eprintln!("jj plan track: missing required <bookmark-name> argument");
            eprintln!();
            eprintln!("Usage: jj plan track <bookmark-name>");
            eprintln!();
            eprintln!("Adopts an existing bookmark as a plan. The bookmark must already");
            eprintln!("exist in the repository (create it with `jj bookmark create`).");
            return Ok(1);
        }
    };

    // ------------------------------------------------------------------
    // 2. Validate bookmark exists
    // ------------------------------------------------------------------
    let existing_bookmarks = workspace.local_bookmarks();
    let bookmark = match existing_bookmarks.iter().find(|b| b.name == bookmark_name) {
        Some(b) => b.clone(),
        None => {
            eprintln!(
                "jj plan track: bookmark '{}' does not exist",
                bookmark_name
            );
            eprintln!();
            eprintln!("Available bookmarks:");
            if existing_bookmarks.is_empty() {
                eprintln!("  (none)");
            } else {
                for b in &existing_bookmarks {
                    eprintln!("  {}", b.name);
                }
            }
            return Ok(1);
        }
    };

    // ------------------------------------------------------------------
    // 3. Validate bookmark is not already registered
    // ------------------------------------------------------------------
    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();

    if registry.is_tracked(&bookmark_name) {
        eprintln!(
            "jj plan track: '{}' is already registered as a plan",
            bookmark_name
        );
        return Ok(0); // Not an error — idempotent
    }

    // Check for encoded-name collision (e.g. feat--auth vs feat/auth)
    if let Some(existing) = registry.would_collide(&bookmark_name) {
        let encoded = crate::plan_file::encode_bookmark_for_filename(&bookmark_name);
        eprintln!(
            "jj plan track: bookmark '{}' would collide with existing plan '{}' (both encode to filename '{}'). Rename one of them.",
            bookmark_name, existing, encoded
        );
        return Ok(1);
    }

    // ------------------------------------------------------------------
    // 4. Flush pending plan edits before mutation
    // ------------------------------------------------------------------
    crate::flush::flush_all(&plan_dir.path, jj, workspace, registry);

    // ------------------------------------------------------------------
    // 5. Register in PlanRegistry
    // ------------------------------------------------------------------
    let mut registry_mut = plan_registry::load_registry(&repo_root);
    registry_mut.track(PlannedBookmark::new(
        bookmark_name.clone(),
        bookmark.change_id.clone(),
    ));
    plan_registry::save_registry(&repo_root, &registry_mut);

    // ------------------------------------------------------------------
    // 6. Reload, sync, and show
    // ------------------------------------------------------------------
    eprintln!("Tracking plan: {} (jj:{})", bookmark_name, bookmark.change_id);
    workspace.reload();
    let post_registry = plan_registry::load_registry(&repo_root);
    crate::wrap::resolve_and_sync(plan_dir, workspace, &post_registry);

    Ok(0)
}