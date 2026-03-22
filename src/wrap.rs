use crate::jj_binary::JjBinary;
use crate::plan_dir::{self, PlanDir};
use crate::plan_registry;
use crate::pr_cache::load_pr_cache;
use crate::stack_builder::build_multi_stack;
use crate::stack_render::{self, StackColumn};
use crate::types::{self, PlanRegistry};
use crate::workspace::Workspace;
use crate::sync;

/// Pre-gathered display data returned by `resolve_and_sync` for reuse by
/// `show_plan_stack`. Avoids a second GATHER traversal of the repository.
pub struct StackDisplayData {
    pub columns: Vec<StackColumn>,
    pub num_stacks: usize,
}

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
    registry: &PlanRegistry,
) -> crate::error::Result<i32> {
    // 1. Flush all local plan file edits to jj descriptions
    crate::flush::flush_all(&plan_dir.path, jj, workspace, registry);

    // 2. Run the actual jj command with inherited stdio
    let status = jj.run_inherit_strings(args)?;
    let exit_code = status.code().unwrap_or(1);

    // 3-5. Reload repo, re-resolve stack, sync plan files
    workspace.reload();
    let gathered = resolve_and_sync(plan_dir, workspace, registry);

    // 6. Display the plan stack
    show_plan_stack(plan_dir, gathered.as_ref());

    // 7. Auto-cleanup merged stacks (registry + base bookmarks behind trunk)
    auto_cleanup_merged_stacks(workspace, plan_dir);

    Ok(exit_code)
}

/// Canonical post-mutation sync: build stack → sync plan files.
///
/// This is the single entry point for "re-read jj state and update plan files
/// after a mutation". Does NOT display anything — callers that want to show
/// the stack should follow up with `show_plan_stack()`.
///
/// Callers must call `workspace.reload()` after CLI mutations before
/// calling this.

/// Auto-cleanup stacks whose base change is behind trunk (fully merged).
///
/// Scans the registry for plans with an explicit `stack` value, checks
/// whether that change ID is an ancestor of `trunk()`, and if so untracks
/// all plans in that stack and deletes the base bookmark.
///
/// This runs after every mutating command (via `wrap()`), so merged stacks
/// are cleaned up automatically without user intervention.
pub fn auto_cleanup_merged_stacks(workspace: &mut Workspace, plan_dir: &PlanDir) {
    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();
    let registry = plan_registry::load_registry(&repo_root);

    // Collect unique explicit stack IDs (standard hex change IDs of stack bases)
    let mut stack_ids: Vec<String> = Vec::new();
    for bm in &registry.bookmarks {
        if let Some(ref sid) = bm.stack {
            if !stack_ids.contains(sid) {
                stack_ids.push(sid.clone());
            }
        }
    }

    if stack_ids.is_empty() {
        return;
    }

    let stack_prefix = plan_dir::stack_prefix();
    let mut cleaned_any = false;

    for stack_id in &stack_ids {
        // Convert standard hex change ID to reverse-hex for use in revsets.
        // jj revsets only accept reverse-hex change IDs; standard hex is
        // interpreted as a commit ID and will silently fail to resolve.
        let revset_id = match workspace.short_change_id_from_hex(stack_id) {
            Some(id) => id,
            None => continue, // Invalid hex or unknown change — skip
        };

        // Check if the stack base change is an ancestor of trunk()
        let revset = format!("{} & ::trunk()", revset_id);
        let is_merged = workspace
            .evaluate_revset(&revset)
            .map(|commits| !commits.is_empty())
            .unwrap_or(false);

        if !is_merged {
            continue;
        }

        // This stack has been fully merged — clean it up
        let plans = registry.plans_in_stack(Some(stack_id));
        if plans.is_empty() {
            continue;
        }

        let plan_names: Vec<String> = plans.iter().map(|p| p.name.clone()).collect();

        // Derive a human-readable stack name from the base bookmark.
        // Scope to the bookmark whose change_id matches this stack_id
        // (standard hex comparison — both sides use commit.change_id().hex()).
        let stack_name = workspace
            .local_bookmarks()
            .iter()
            .find(|b| b.name.starts_with(&stack_prefix) && b.change_id == *stack_id)
            .map(|b| b.name.strip_prefix(&stack_prefix).unwrap_or(&b.name).to_string())
            .unwrap_or_else(|| plan_names.first().cloned().unwrap_or_default());

        // Untrack all plans in this stack
        let mut registry_mut = plan_registry::load_registry(&repo_root);
        for name in &plan_names {
            registry_mut.untrack(name);
        }
        plan_registry::save_registry(&repo_root, &registry_mut);

        // Delete the stack base bookmark if one exists.
        // Scope to the bookmark whose change_id matches this stack_id.
        let base_bm: Option<String> = workspace
            .local_bookmarks()
            .iter()
            .find(|b| b.name.starts_with(&stack_prefix) && b.change_id == *stack_id)
            .map(|b| b.name.clone());

        if let Some(ref bb) = base_bm {
            let _ = workspace.delete_bookmark(bb);
        }

        eprintln!("jj-plan: auto-cleaned stack '{}' (merged to trunk)", stack_name);
        cleaned_any = true;
    }

    if cleaned_any {
        // Re-sync after cleanup so plan files reflect the updated registry.
        // The return value (display data) is intentionally discarded — this
        // is a background cleanup, not a user-facing display path.
        workspace.reload();
        let post_registry = plan_registry::load_registry(&repo_root);
        let _ = resolve_and_sync(plan_dir, workspace, &post_registry);
    }
}

pub fn resolve_and_sync(plan_dir: &PlanDir, workspace: &Workspace, registry: &PlanRegistry) -> Option<StackDisplayData> {
    debug_log!("resolve_and_sync(plan_dir={:?})", plan_dir.path);

    let max_stack_size = crate::plan_dir::plan_max();

    // Auto-untrack registry entries whose bookmarks no longer exist in jj.
    // When `jj abandon` deletes a bookmark (the default behavior without
    // --retain-bookmarks), the registry entry becomes stale. Clean it up
    // so the plan file disappears on the next sync cycle.
    let all_bookmarks = workspace.local_bookmarks();
    let live_bookmark_names: std::collections::HashSet<&str> = all_bookmarks
        .iter()
        .map(|b| b.name.as_str())
        .collect();

    let stale: Vec<String> = registry
        .bookmarks
        .iter()
        .filter(|b| !live_bookmark_names.contains(b.name.as_str()))
        .map(|b| b.name.clone())
        .collect();

    if !stale.is_empty() {
        let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();
        let mut registry_mut = plan_registry::load_registry(&repo_root);
        for name in &stale {
            registry_mut.untrack(name);
        }
        plan_registry::save_registry(&repo_root, &registry_mut);
        eprintln!(
            "jj-plan: auto-untracked {} abandoned bookmark(s): {}",
            stale.len(),
            stale.join(", ")
        );
    }

    // Migrate any legacy change-ID-based filenames to bookmark-named files.
    // This must happen before gather_current_state() in sync so the rest of
    // the pipeline sees only bookmark-named files.
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
    let sync_changes = build_sync_views(workspace, registry);
    match &sync_changes {
        Some(views) => {
            debug_log!("  sync views: {} bookmark(s)", views.len());
        }
        None => {
            let tracked: Vec<&str> = registry.tracked_names();
            debug_log!("  sync views: None (empty/failed stack)");
            debug_log!("  registry: {} tracked bookmark(s): {:?}", tracked.len(), tracked);
            debug_log!("  plan_dir: {:?}", plan_dir.path);
        }
    }
    // Build multi-stack for the rendering pipeline (markdown content for stack.md)
    let multi = build_multi_stack(workspace, registry);
    let (stack_md_content, display_data) = if multi.stacks.is_empty() {
        (None, None)
    } else {
        let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();
        let pr_cache = load_pr_cache(&repo_root).ok();
        let columns = stack_render::build_columns(&multi, registry, workspace, pr_cache.as_ref());
        let rendered = stack_render::render_stack(&columns);
        let md_content = stack_render::format_markdown_with_header(&rendered);
        let num_stacks = multi.stacks.len();
        (Some(md_content), Some(StackDisplayData { columns, num_stacks }))
    };

    sync::sync(plan_dir, sync_changes.as_deref(), max_stack_size, registry, stack_md_content.as_deref());

    display_data
}

/// Display the plan stack using pre-gathered display data.
///
/// This is the single rendering entry point for all command paths.
/// Call after `resolve_and_sync()` with the returned `StackDisplayData`.
///
/// Pipeline: PLAN → EXECUTE (GATHER already done by `resolve_and_sync`)
/// - PLAN:   `stack_render::render_stack()` → `Vec<Vec<Span>>`
/// - EXECUTE: `format_ansi()` or `format_plain()` → `eprintln!`
pub fn show_plan_stack(plan_dir: &PlanDir, data: Option<&StackDisplayData>) {
    let data = match data {
        Some(d) => d,
        None => {
            eprintln!("No plans between trunk and working copy.");
            eprintln!("Create one with: jj plan new <bookmark-name>");
            return;
        }
    };

    // PLAN
    let rendered = stack_render::render_stack(&data.columns);

    // EXECUTE
    let color = stack_render::should_color();
    let formatted = if color {
        stack_render::format_ansi(&rendered)
    } else {
        stack_render::format_plain(&rendered)
    };

    let is_multi = data.num_stacks > 1;

    // Header
    eprintln!();
    eprint!("Plan stack ({}/", plan_dir.dir_name());
    if is_multi {
        eprint!(" {} stacks", data.num_stacks);
    }
    eprintln!("):");

    for line in &formatted {
        eprintln!("{}", line);
    }
}

/// Convenience: resolve + sync + show in one call.
///
/// Most command sites just need to do all three steps. Sites that need
/// the intermediate `StackDisplayData` can call `resolve_and_sync` and
/// `show_plan_stack` separately.
pub fn resolve_sync_and_show(plan_dir: &PlanDir, workspace: &Workspace, registry: &PlanRegistry) {
    let gathered = resolve_and_sync(plan_dir, workspace, registry);
    show_plan_stack(plan_dir, gathered.as_ref());
}

/// Build `SyncChangeView`s from the registry-filtered stack.
///
/// This is the single shared function for converting the repository's
/// stack state into the flat list that sync, done, and other modules
/// consume. It accepts a `&PlanRegistry` (loaded once per command),
/// builds the stack with registry filtering, and converts each segment's
/// tip commit into a `SyncChangeView`.
///
/// Returns `None` when the stack is empty, contains merge commits, or
/// has no registry-matching segments.
pub fn build_sync_views(workspace: &Workspace, registry: &PlanRegistry) -> Option<Vec<SyncChangeView>> {
    // Build the stack using the new builder with registry filtering
    let stack_result = crate::stack_builder::build_stack(workspace, Some(registry));

    // Convert to the adapter type
    stack_to_sync_changes(&stack_result, workspace, registry)
}

/// A lightweight view of a stack change for sync and flush.
///
/// Each `SyncChangeView` represents one entry in the `stack.md` summary
/// and one plan file `NN-{bookmark_name}.md`.
pub struct SyncChangeView {
    /// Short reverse-hex change ID (for `jj describe -r` and display).
    pub change_id: String,
    /// The registered plan bookmark name for this segment.
    /// Used for plan filenames (`NN-{bookmark_name}.md`).
    pub bookmark_name: String,
    /// Full description text.
    pub description: String,
    /// Whether this is the working copy.
    pub is_working_copy: bool,
}

impl SyncChangeView {
    /// Whether the description contains `plan-status: ✅`.
    pub fn is_done(&self) -> bool {
        types::description_is_done(&self.description)
    }
}

/// Convert a `StackResult` to a flat list of `SyncChangeView`s for sync.
///
/// In the new model, only bookmarked commits get plan files. Each segment's
/// tip commit (the bookmarked commit) produces one `SyncChangeView`.
///
/// Returns `None` for `StackResult::Empty`.
fn stack_to_sync_changes(
    result: &crate::types::StackResult,
    workspace: &Workspace,
    registry: &crate::types::PlanRegistry,
) -> Option<Vec<SyncChangeView>> {
    use crate::types::StackResult;

    match result {
        StackResult::Empty => None,
        StackResult::Ok(stack) => {
            if stack.segments.is_empty() {
                return None;
            }

            let mut views = Vec::new();
            for segment in &stack.segments {
                // The tip commit is changes[0] (newest first)
                if let Some(tip) = segment.changes.first() {
                    // We need the short change ID for `jj describe -r`.
                    // LogEntry.change_id stores standard hex; convert to the
                    // short reverse-hex form that jj uses for display and revsets.
                    let short_id = workspace
                        .short_change_id_from_hex(&tip.change_id)
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
                        is_working_copy: tip.is_working_copy,
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