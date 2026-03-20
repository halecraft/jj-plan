use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::workspace::Workspace;
use crate::sync;

/// Unified handler for mutating commands: flush → command → reload → sync → show.
///
/// All mutating jj commands go through this lifecycle:
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
    workspace: &mut Workspace,
) -> crate::error::Result<i32> {
    // 1. Flush all local plan file edits to jj descriptions
    crate::flush::flush_all(&plan_dir.path, jj, workspace);

    // 2. Run the actual jj command with inherited stdio
    let status = jj.run_inherit_strings(args)?;
    let exit_code = status.code().unwrap_or(1);

    // 3-6. Reload repo, re-resolve stack, sync plan files, show stack
    workspace.reload();
    resolve_and_sync(plan_dir, workspace);

    Ok(exit_code)
}

/// Canonical post-mutation sync: build stack → sync plan files → show stack.
///
/// This is the single entry point for "re-read jj state and update plan files
/// after a mutation". All command modules should call this instead of
/// maintaining their own sync helpers.
///
/// Callers must call `workspace.reload()` after CLI mutations before
/// calling this.
pub fn resolve_and_sync(plan_dir: &PlanDir, workspace: &Workspace) {
    let max_stack_size = crate::plan_dir::plan_max();

    // Migrate any legacy change-ID-based filenames to bookmark-named files.
    // This must happen before gather_current_state() in sync so the rest of
    // the pipeline sees only bookmark-named files.
    let all_bookmarks = workspace.local_bookmarks();
    crate::plan_file::migrate_legacy_filenames(&plan_dir.path, |legacy_change_id| {
        // Resolve the short reverse-hex change ID to a bookmark name.
        // We compare the short ID from the filename against each bookmark's
        // change_id resolved to short form via the workspace.
        for bm in &all_bookmarks {
            if let Some(bm_short) = workspace.resolve_change_id(&bm.change_id) {
                if bm_short == legacy_change_id
                    || legacy_change_id.starts_with(&bm_short)
                    || bm_short.starts_with(legacy_change_id)
                {
                    return Some(bm.name.clone());
                }
            }
        }
        None
    });

    // Build sync views from the registry-filtered stack
    let sync_changes = build_sync_views(workspace);
    sync::sync(plan_dir, sync_changes.as_deref(), max_stack_size);
    sync::show_stack(plan_dir);
}

/// Build `SyncChangeView`s from the registry-filtered stack.
///
/// This is the single shared function for converting the repository's
/// stack state into the flat list that sync, done, and other modules
/// consume. It loads the `PlanRegistry`, builds the stack with registry
/// filtering, and converts each segment's tip commit into a
/// `SyncChangeView`.
///
/// Returns `None` when the stack is empty, contains merge commits, or
/// has no registry-matching segments.
pub fn build_sync_views(workspace: &Workspace) -> Option<Vec<SyncChangeView>> {
    // Load plan registry for filtered stack building
    let repo_root = workspace.jj_workspace().workspace_root();
    let registry = crate::plan_registry::load_registry(repo_root);

    // Build the stack using the new builder with registry filtering
    let stack_result = crate::stack_builder::build_stack(workspace, Some(&registry));

    // Convert to the adapter type
    stack_to_sync_changes(&stack_result, workspace, &registry)
}

/// A lightweight view of a stack change for sync/flush compatibility.
///
/// Bridges the new `Stack`/`BookmarkSegment`/`LogEntry` types with sync.rs's
/// existing `StackChange`-shaped interface. This adapter is temporary — it
/// will be removed when sync.rs is updated to accept `Stack` directly in a
/// later plan.
///
/// Each `SyncChangeView` represents one entry in the `.stack` summary file
/// and one plan file `NN-{bookmark_name}.md`.
pub struct SyncChangeView {
    /// Short reverse-hex change ID (for `jj describe -r` and display).
    pub change_id: String,
    /// The registered plan bookmark name for this segment.
    /// Used for plan filenames (`NN-{bookmark_name}.md`).
    pub bookmark_name: String,
    /// Full description text.
    pub description: String,
    /// Whether this change is empty.
    pub is_empty: bool,
    /// Whether this is the working copy.
    pub is_working_copy: bool,
    /// All bookmark names on this change (may include non-plan bookmarks).
    pub bookmarks: Vec<String>,
}

impl SyncChangeView {
    /// First line of the description, for display in `.stack` summary.
    pub fn first_line(&self) -> &str {
        self.description.lines().next().unwrap_or("")
    }

    /// Whether the description contains `plan-status: ✅`.
    pub fn is_done(&self) -> bool {
        self.description.starts_with("plan-status: ✅")
            || self.description.contains("\nplan-status: ✅")
    }
}

/// Convert a `StackResult` to a flat list of `SyncChangeView`s for sync.
///
/// In the new model, only bookmarked commits get plan files. Each segment's
/// tip commit (the bookmarked commit) produces one `SyncChangeView`.
///
/// Returns `None` for `StackResult::Empty` or `StackResult::MergeCommits`.
fn stack_to_sync_changes(
    result: &crate::types::StackResult,
    workspace: &Workspace,
    registry: &crate::types::PlanRegistry,
) -> Option<Vec<SyncChangeView>> {
    use crate::types::StackResult;

    match result {
        StackResult::Empty | StackResult::MergeCommits => None,
        StackResult::Ok(stack) => {
            if stack.segments.is_empty() {
                return None;
            }

            let mut views = Vec::new();
            for segment in &stack.segments {
                // The tip commit is changes[0] (newest first)
                if let Some(tip) = segment.changes.first() {
                    // We need the short change ID for `jj describe -r`.
                    let short_id = workspace
                        .resolve_change_id(&tip.change_id)
                        .unwrap_or_else(|| tip.change_id[..8].to_string());

                    // Find the registered plan bookmark for this segment.
                    // Since segments are built with registry filtering, at
                    // least one bookmark should be registered. Use the first
                    // registered bookmark, falling back to the first bookmark.
                    let plan_bookmark_name = segment
                        .bookmarks
                        .iter()
                        .find(|b| registry.is_tracked(&b.name))
                        .or_else(|| segment.bookmarks.first())
                        .map(|b| b.name.clone())
                        .unwrap_or_default();

                    views.push(SyncChangeView {
                        change_id: short_id,
                        bookmark_name: plan_bookmark_name,
                        description: tip.description.clone(),
                        is_empty: tip.is_empty,
                        is_working_copy: tip.is_working_copy,
                        bookmarks: tip.local_bookmarks.clone(),
                    });
                }
            }

            if views.is_empty() {
                None
            } else {
                Some(views)
            }
        }
    }
}