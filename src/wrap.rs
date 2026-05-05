use std::collections::HashSet;

use crate::jj_binary::JjBinary;
use crate::plan_dir::{self, PlanDir};
use crate::plan_registry;
use crate::pr_cache::{load_pr_cache, save_pr_cache};
use crate::stack_render::{self, RenderOptions, StackColumn, StackFormat};
use crate::types::{self, PlanRegistry, StackResult};
use crate::workspace::Workspace;
use crate::sync;

/// Pre-gathered display data returned by `sync_to_disk` for reuse by
/// `show_plan_stack`. Avoids a second GATHER traversal of the repository.
pub struct StackDisplayData {
    pub columns: Vec<StackColumn>,
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
    format: StackFormat,
) -> crate::error::Result<i32> {
    // 1. Flush all local plan file edits to jj descriptions
    crate::flush::flush_all(&plan_dir.path, jj, workspace, registry);

    // 2. Run the actual jj command with inherited stdio
    let status = jj.run_inherit_strings(args)?;
    let exit_code = status.code().unwrap_or(1);

    // 3-5. Reload repo, untrack stale bookmarks, migrate legacy filenames,
    //      sync plan files, show stack
    workspace.reload();
    full_sync_and_show(plan_dir, workspace, registry, format);

    // 6. Auto-cleanup merged stacks (registry + base bookmarks behind trunk)
    auto_cleanup_merged_stacks(workspace, plan_dir);

    Ok(exit_code)
}

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
    let mut registry = plan_registry::load_registry(&repo_root);

    // Collect unique explicit stack IDs (standard hex change IDs of stack bases)
    let mut stack_ids: Vec<String> = Vec::new();
    for bm in &registry.bookmarks {
        if let Some(ref sid) = bm.stack
            && !stack_ids.contains(sid) {
                stack_ids.push(sid.clone());
            }
    }

    if stack_ids.is_empty() {
        return;
    }

    let stack_prefix = plan_dir::stack_prefix();

    // Accumulate all mutations across merged stacks, then apply once.
    let mut bookmarks_to_untrack: Vec<String> = Vec::new();
    let mut base_bookmarks_to_delete: Vec<String> = Vec::new();
    let mut cleaned_stack_names: Vec<String> = Vec::new();

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

        // This stack has been fully merged — collect its plans for cleanup
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

        bookmarks_to_untrack.extend(plan_names);

        // Collect the stack base bookmark for deletion (if one exists).
        if let Some(base_bm) = workspace
            .local_bookmarks()
            .iter()
            .find(|b| b.name.starts_with(&stack_prefix) && b.change_id == *stack_id)
            .map(|b| b.name.clone())
        {
            base_bookmarks_to_delete.push(base_bm);
        }

        cleaned_stack_names.push(stack_name);
    }

    if bookmarks_to_untrack.is_empty() {
        return;
    }

    // Apply all mutations once: untrack plans, save registry, delete base bookmarks.
    for name in &bookmarks_to_untrack {
        registry.untrack(name);
    }
    plan_registry::save_registry(&repo_root, &registry);

    for bb in &base_bookmarks_to_delete {
        let _ = workspace.delete_bookmark(bb);
    }

    for stack_name in &cleaned_stack_names {
        eprintln!("jj-plan: auto-cleaned stack '{}' (merged to trunk)", stack_name);
    }

    // Re-sync after cleanup so plan files reflect the updated registry.
    // Uses sync_to_disk (not full_sync_and_show) because stale bookmarks
    // were just cleaned — no stale check needed.
    workspace.reload();
    let _ = sync_to_disk(plan_dir, workspace, &registry);
}

// ---------------------------------------------------------------------------
// Pure function: identify stale registry entries
// ---------------------------------------------------------------------------

/// Pure: identify registry entries whose bookmarks no longer exist in jj.
///
/// Returns the names of bookmarks that are tracked in the registry but
/// absent from the live bookmark set. The caller decides whether to
/// untrack them and persist the change.
pub fn find_stale_bookmarks(
    registry: &PlanRegistry,
    live_bookmark_names: &HashSet<&str>,
) -> Vec<String> {
    registry
        .bookmarks
        .iter()
        .filter(|b| !live_bookmark_names.contains(b.name.as_str()))
        .map(|b| b.name.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Sync tier 1: sync_to_disk — pure resolve + sync, no side effects
// ---------------------------------------------------------------------------

/// Build the stack, sync plan files to disk, return display data.
///
/// This is the lowest-level sync function. It performs no registry mutations
/// and no stderr output beyond debug logging. Side effects are limited to
/// plan file I/O via `sync::sync()`.
///
/// Callers that need stale-bookmark cleanup or legacy filename migration
/// should use `full_sync_and_show` instead.
pub fn sync_to_disk(plan_dir: &PlanDir, workspace: &Workspace, registry: &PlanRegistry) -> Option<StackDisplayData> {
    debug_log!("sync_to_disk(plan_dir={:?})", plan_dir.path);

    let max_stack_size = crate::plan_dir::plan_max();

    // Build the @-relative stack once — single source of truth for both
    // plan file sync and rendering.
    let stack_result = build_current_stack(workspace, registry);

    // Fork 1: sync views for plan files
    let sync_changes = stack_to_sync_changes(&stack_result, workspace, registry);
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

    // Fork 2: rendering for stack.md + terminal
    let (stack_md_content, display_data) = match &stack_result {
        StackResult::Ok(stack) if !stack.segments.is_empty() => {
            let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();
            let pr_cache = load_pr_cache(&repo_root).ok();
            let stack_name = derive_stack_name(stack, registry);
            match stack_render::build_column_from_stack(stack, &stack_name, registry, workspace, pr_cache.as_ref(), plan_dir.dir_name()) {
                Some(column) => {
                    let columns = vec![column];
                    let rendered = stack_render::render_stack(&columns, &RenderOptions {
                        format: StackFormat::Regular,
                        show_paths: false,
                    });
                    let md_content = stack_render::format_markdown_with_header(&rendered);
                    (Some(md_content), Some(StackDisplayData { columns }))
                }
                None => (None, None),
            }
        }
        _ => (None, None),
    };

    sync::sync(plan_dir, sync_changes.as_deref(), max_stack_size, registry, stack_md_content.as_deref());

    display_data
}

// ---------------------------------------------------------------------------
// Sync tier 2: sync_and_show — sync_to_disk + display
// ---------------------------------------------------------------------------

/// Sync plan files to disk and display the stack.
///
/// Lite convenience wrapper for internal re-sync paths (post-untrack,
/// post-cleanup) where stale-bookmark detection is not needed.
///
/// For user-facing command paths that follow mutations, use
/// `full_sync_and_show` instead.
pub fn sync_and_show(plan_dir: &PlanDir, workspace: &Workspace, registry: &PlanRegistry, format: StackFormat) {
    let gathered = sync_to_disk(plan_dir, workspace, registry);
    show_plan_stack(plan_dir, gathered.as_ref(), format);
}

// ---------------------------------------------------------------------------
// Composable prerequisite: cleanup_stale_and_migrate
// ---------------------------------------------------------------------------

/// Cleanup: untrack stale bookmarks, migrate legacy filenames, prune PR cache.
///
/// Performs three imperative side effects that must run before sync:
/// 1. Detect and untrack registry entries whose bookmarks no longer exist in jj.
/// 2. Migrate legacy change-ID-based filenames to bookmark-named files.
/// 3. Prune orphaned PR cache entries whose bookmarks no longer exist in jj
///    or the plan registry.
///
/// Idempotent: safe to call multiple times (second call finds nothing to do).
///
/// Callers compose this with `sync_to_disk` or `sync_and_show` depending on
/// whether they need display. `full_sync_and_show` is the convenience wrapper
/// that calls both `cleanup_stale_and_migrate` + `sync_and_show`.
pub fn cleanup_stale_and_migrate(
    plan_dir: &PlanDir,
    workspace: &Workspace,
    registry: &PlanRegistry,
) {
    // 1. Detect and untrack stale registry entries.
    let all_bookmarks = workspace.local_bookmarks();
    let live_bookmark_names: HashSet<&str> = all_bookmarks
        .iter()
        .map(|b| b.name.as_str())
        .collect();

    let stale = find_stale_bookmarks(registry, &live_bookmark_names);

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

    // 2. Migrate legacy change-ID-based filenames to bookmark-named files.
    // This must happen before gather_current_state() in sync so the rest of
    // the pipeline sees only bookmark-named files.
    crate::plan_file::migrate_legacy_filenames(&plan_dir.path, |legacy_change_id| {
        for bm in &all_bookmarks {
            if let Some(bm_short) = workspace.resolve_change_id(&bm.change_id)
                && (bm_short == legacy_change_id
                    || legacy_change_id.starts_with(&bm_short)
                    || bm_short.starts_with(legacy_change_id))
                {
                    return Some(bm.name.clone());
                }
        }
        None
    });

    // 3. Prune orphaned PR cache entries.
    // A cache entry is orphaned when its bookmark exists in neither the live
    // jj bookmarks nor the plan registry. The registry parameter is the
    // original snapshot from before this function was called — intentionally
    // conservative: a bookmark just untracked in step 1 still appears in this
    // registry, so its cache entry survives one extra cycle.
    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();
    let mut cache_live: HashSet<&str> = live_bookmark_names; // already has local bookmarks
    for bm in &registry.bookmarks {
        cache_live.insert(&bm.name);
    }
    if let Ok(mut pr_cache) = load_pr_cache(&repo_root) {
        let pruned = pr_cache.retain_bookmarks(&cache_live);
        if !pruned.is_empty() {
            if let Err(e) = save_pr_cache(&repo_root, &pr_cache) {
                eprintln!("Warning: failed to save PR cache after pruning: {e}");
            } else {
                eprintln!(
                    "jj-plan: pruned {} stale PR cache entry(ies): {}",
                    pruned.len(),
                    pruned.join(", ")
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Sync tier 3: full_sync_and_show — stale cleanup + migration + sync + show
// ---------------------------------------------------------------------------

/// Full post-mutation sync: untrack stale bookmarks, migrate legacy
/// filenames, sync plan files to disk, show stack.
///
/// This is the batteries-included function for user-facing command paths.
/// Delegates to `cleanup_stale_and_migrate` + `sync_and_show`.
///
/// Callers that need cleanup without display (e.g. `jj stack --all` which
/// uses its own multi-stack display) can call `cleanup_stale_and_migrate` +
/// `sync_to_disk` directly.
pub fn full_sync_and_show(
    plan_dir: &PlanDir,
    workspace: &Workspace,
    registry: &PlanRegistry,
    format: StackFormat,
) {
    cleanup_stale_and_migrate(plan_dir, workspace, registry);
    sync_and_show(plan_dir, workspace, registry, format);
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

/// Display the plan stack using pre-gathered display data.
///
/// This is the single rendering entry point for all command paths.
/// Call after `sync_to_disk()` with the returned `StackDisplayData`.
///
/// Pipeline: PLAN → EXECUTE (GATHER already done by `sync_to_disk`)
/// - PLAN:   `stack_render::render_stack()` → `Vec<Vec<Span>>`
/// - EXECUTE: `format_ansi()` or `format_plain()` → `eprintln!`
pub fn show_plan_stack(plan_dir: &PlanDir, data: Option<&StackDisplayData>, format: StackFormat) {
    let data = match data {
        Some(d) => d,
        None => {
            eprintln!("No plans between trunk and working copy.");
            eprintln!("Create one with: jj plan new <bookmark-name>");
            return;
        }
    };

    // Header
    eprintln!();
    eprintln!("Plan stack ({}/):", plan_dir.dir_name());

    // Render → format → print (delegated to shared helper)
    stack_render::render_to_stderr(&data.columns, &RenderOptions {
        format,
        show_paths: true,
    });
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
    let stack_result = build_current_stack(workspace, registry);
    stack_to_sync_changes(&stack_result, workspace, registry)
}

/// Build the @-relative stack. Single source of truth for "what stack am I on?"
///
/// Thin wrapper around `build_stack()` with registry filtering. Returns the
/// raw `StackResult` so callers can fork it into both sync views and rendering.
fn build_current_stack(workspace: &Workspace, registry: &PlanRegistry) -> StackResult {
    crate::stack_builder::build_stack(workspace, Some(registry))
}

/// Derive a human-readable name for the current stack.
///
/// Uses the tip-most (last segment) registered bookmark name, falling back
/// to `"plan"` if no registered bookmark is found.
fn derive_stack_name(stack: &crate::types::Stack, registry: &PlanRegistry) -> String {
    stack.segments.last()
        .and_then(|seg| {
            seg.bookmarks.iter()
                .find(|b| registry.is_tracked(&b.name))
                .map(|b| b.name.clone())
        })
        .unwrap_or_else(|| "plan".to_string())
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
    /// Whether the description's front matter `status` field is `✅`.
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
    result: &StackResult,
    workspace: &Workspace,
    registry: &PlanRegistry,
) -> Option<Vec<SyncChangeView>> {

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Bookmark, BookmarkSegment, LogEntry, Stack};
    use chrono::Utc;

    fn make_bookmark(name: &str) -> Bookmark {
        Bookmark {
            name: name.to_string(),
            commit_id: "aabb".to_string(),
            change_id: "ccdd".to_string(),
            has_remote: false,
            is_synced: false,
        }
    }

    fn make_segment(bookmark_name: &str) -> BookmarkSegment {
        BookmarkSegment {
            bookmarks: vec![make_bookmark(bookmark_name)],
            changes: vec![LogEntry {
                commit_id: "aabb".to_string(),
                change_id: "ccdd".to_string(),
                author_name: String::new(),
                author_email: String::new(),
                description_first_line: "desc".to_string(),
                description: "desc".to_string(),
                parents: vec![],
                local_bookmarks: vec![bookmark_name.to_string()],
                remote_bookmarks: vec![],
                is_working_copy: false,
                is_empty: true,
                authored_at: Utc::now(),
                committed_at: Utc::now(),
            }],
        }
    }

    fn make_registry(names: &[&str]) -> PlanRegistry {
        let mut reg = PlanRegistry::new();
        for name in names {
            reg.track(crate::types::PlannedBookmark::new(*name, "ccdd"));
        }
        reg
    }

    #[test]
    fn derive_stack_name_uses_tip_bookmark() {
        let stack = Stack {
            segments: vec![make_segment("feat-auth"), make_segment("feat-api")],
            gaps: vec![],
        };
        let registry = make_registry(&["feat-auth", "feat-api"]);
        assert_eq!(derive_stack_name(&stack, &registry), "feat-api");
    }

    #[test]
    fn derive_stack_name_skips_unregistered_bookmarks() {
        let stack = Stack {
            segments: vec![make_segment("feat-auth"), make_segment("unregistered")],
            gaps: vec![],
        };
        // Only feat-auth is registered — unregistered tip is skipped,
        // but derive_stack_name checks tip only, so falls back to "plan".
        let registry = make_registry(&["feat-auth"]);
        assert_eq!(derive_stack_name(&stack, &registry), "plan");
    }

    #[test]
    fn derive_stack_name_empty_stack_falls_back() {
        let stack = Stack {
            segments: vec![],
            gaps: vec![],
        };
        let registry = make_registry(&[]);
        assert_eq!(derive_stack_name(&stack, &registry), "plan");
    }

    #[test]
    fn derive_stack_name_single_segment() {
        let stack = Stack {
            segments: vec![make_segment("hotfix")],
            gaps: vec![],
        };
        let registry = make_registry(&["hotfix"]);
        assert_eq!(derive_stack_name(&stack, &registry), "hotfix");
    }

    // -- find_stale_bookmarks tests --

    #[test]
    fn find_stale_bookmarks_detects_abandoned() {
        let registry = make_registry(&["feat-x"]);
        let live: HashSet<&str> = HashSet::from(["feat-y", "feat-z"]);
        let stale = find_stale_bookmarks(&registry, &live);
        assert_eq!(stale, vec!["feat-x"]);
    }

    #[test]
    fn find_stale_bookmarks_none_stale() {
        let registry = make_registry(&["feat-a", "feat-b"]);
        let live: HashSet<&str> = HashSet::from(["feat-a", "feat-b", "feat-c"]);
        let stale = find_stale_bookmarks(&registry, &live);
        assert!(stale.is_empty());
    }

    #[test]
    fn find_stale_bookmarks_multiple_stale() {
        let registry = make_registry(&["keep", "gone-1", "gone-2"]);
        let live: HashSet<&str> = HashSet::from(["keep", "other"]);
        let mut stale = find_stale_bookmarks(&registry, &live);
        stale.sort();
        assert_eq!(stale, vec!["gone-1", "gone-2"]);
    }
}