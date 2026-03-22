use crate::error::{JjPlanError, Result};
use crate::jj_binary::JjBinary;
use crate::merge::{create_merge_plan, execute_merge, MergeStep, PrInfo};
use crate::plan_dir::{self, PlanDir};
use crate::plan_registry;
use crate::pr_cache::{load_pr_cache, save_pr_cache};
use crate::stack_builder::{build_multi_stack, build_stack, collect_submission_chain, find_submit_target, narrow_segments};
use crate::stack_context::StackContext;
use crate::submit::{
    analyze_submission, create_submission_plan, execute_submission,
    Phase, ProgressCallback, PushStatus,
};
use crate::types::{Gap, MergeMethod, NarrowedBookmarkSegment, PlanRegistry, StackResult};
use crate::workspace::Workspace;

use async_trait::async_trait;

/// Dispatch `jj stack <subcommand>` to the appropriate handler.
///
/// `args` is the full argument list starting with "stack".
/// For example: `["stack", "submit"]` or `["stack", "--help"]`.
pub fn dispatch_stack(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    args: &[String],
    workspace: &mut Workspace,
    registry: &PlanRegistry,
) -> Result<i32> {
    let subcommand = args.get(1).map(|s| s.as_str());

    // Help handling — only on explicit --help/-h
    if matches!(subcommand, Some("--help" | "-h")) {
        print_stack_help();
        return Ok(0);
    }

    match subcommand {
        None => {
            // Bare `jj stack` — flush pending edits, sync plan files, then show visualization.
            crate::flush::flush_all(&plan_dir.path, jj, workspace, registry);
            workspace.reload();
            crate::wrap::resolve_and_sync(plan_dir, workspace, registry);
            crate::wrap::show_plan_stack(plan_dir, workspace, registry);
            Ok(0)
        }
        Some("submit") => run_submit(workspace, &args[1..], registry),
        Some("sync") => run_sync(workspace, &args[1..], registry),
        Some("merge") => run_merge(workspace, &args[1..], registry),
        Some("auth") => run_auth(&args[1..]),
        Some("untrack") => run_stack_untrack(jj, plan_dir, &args[1..], workspace, registry),
        Some(unknown) => {
            eprintln!("jj stack: unknown subcommand '{}'", unknown);
            eprintln!();
            eprintln!("Available subcommands: submit, sync, merge, auth, untrack");
            eprintln!("Run 'jj stack --help' for more information.");
            Ok(1)
        }
    }
}

/// Run `jj stack untrack` — untrack all plans in the current stack.
///
/// This is a pure registry operation: it removes all plans belonging to the
/// current stack from `plans.toml` and deletes the stack base bookmark (if
/// any). It never mutates commit descriptions.
///
/// Determines the current stack by looking up `@`'s plan in the registry
/// and reading its `stack` value. All plans with the same `stack` value
/// are untracked. If `@` has no plan or `stack = None`, all implicit
/// trunk-stack plans (those with `stack = None`) are untracked.
fn run_stack_untrack(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    args: &[String],
    workspace: &mut Workspace,
    registry: &PlanRegistry,
) -> Result<i32> {
    let dry_run = has_flag(args, "--dry-run");

    // 1. Flush pending plan edits before mutation
    crate::flush::flush_all(&plan_dir.path, jj, workspace, registry);

    // 2. Determine the current stack by looking up @'s plan
    workspace.reload();
    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();

    let current_stack_id = find_current_stack_id(workspace, registry);

    // 3. Find all plans in this stack
    let plans_to_untrack = registry.plans_in_stack(current_stack_id.as_deref());

    if plans_to_untrack.is_empty() {
        eprintln!("jj stack untrack: no plans found in the current stack");
        return Ok(1);
    }

    let plan_names: Vec<String> = plans_to_untrack.iter().map(|p| p.name.clone()).collect();

    // Find the stack base bookmark (if any).
    // Scope to the bookmark whose change_id matches the current stack_id
    // (standard hex comparison — both sides use commit.change_id().hex()).
    let stack_prefix = plan_dir::stack_prefix();
    let base_bookmark = if let Some(ref sid) = current_stack_id {
        workspace
            .local_bookmarks()
            .iter()
            .find(|b| b.name.starts_with(&stack_prefix) && b.change_id == *sid)
            .map(|b| b.name.clone())
    } else {
        None
    };

    // 4. Dry-run: show what would happen
    if dry_run {
        eprintln!("jj stack untrack --dry-run:");
        eprintln!("  Would untrack {} plan(s):", plan_names.len());
        for name in &plan_names {
            eprintln!("    {}", name);
        }
        if let Some(ref bb) = base_bookmark {
            eprintln!("  Would delete stack base bookmark: {}", bb);
        }
        return Ok(0);
    }

    // 5. Untrack each plan from the registry
    let mut registry_mut = plan_registry::load_registry(&repo_root);
    for name in &plan_names {
        registry_mut.untrack(name);
    }
    plan_registry::save_registry(&repo_root, &registry_mut);

    // 6. Delete the stack base bookmark if one exists
    if let Some(ref bb) = base_bookmark {
        if let Err(e) = workspace.delete_bookmark(bb) {
            eprintln!("jj stack untrack: warning: failed to delete bookmark '{}': {}", bb, e);
        }
    }

    // 7. Show summary
    eprintln!("Untracked {} plan(s):", plan_names.len());
    for name in &plan_names {
        eprintln!("  {}", name);
    }
    if let Some(ref bb) = base_bookmark {
        eprintln!("Deleted stack base bookmark: {}", bb);
    }

    // 8. Sync and show updated state
    workspace.reload();
    let post_registry = plan_registry::load_registry(&repo_root);
    crate::wrap::resolve_and_sync(plan_dir, workspace, &post_registry);
    crate::wrap::show_plan_stack(plan_dir, workspace, &post_registry);

    Ok(0)
}

/// Find the stack ID for the current working copy's plan.
///
/// Looks up `@`'s bookmarks in the registry and returns the `stack` value
/// of the first match. Returns `None` if `@` has no tracked plan or the
/// plan has `stack = None` (implicit trunk stack).
fn find_current_stack_id(workspace: &Workspace, registry: &PlanRegistry) -> Option<String> {
    let commits = workspace.evaluate_revset("@")?;
    let wc = commits.first()?;
    let entry = workspace.commit_to_log_entry(wc);

    for bm_name in &entry.local_bookmarks {
        if let Some(planned) = registry.get(bm_name) {
            return planned.stack.clone();
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Argument parsing helpers
// ---------------------------------------------------------------------------

/// Check if a flag is present in args.
fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// Get the value of a `--key value` or `--key=value` option.
fn get_option<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == key {
            return iter.next().map(|s| s.as_str());
        }
        if let Some(val) = arg.strip_prefix(&format!("{key}=")) {
            return Some(val);
        }
    }
    None
}

/// Get the first positional argument (non-flag, non-option-value).
fn first_positional(args: &[String]) -> Option<&str> {
    let known_flags = [
        "--dry-run",
        "--confirm",
        "--draft",
        "--publish",
        "--allow-gaps",
        "--all",
        "--help",
        "-h",
    ];
    let known_options = ["--remote"];

    let mut skip_next = false;
    for arg in args.iter().skip(1) {
        // skip "submit"/"sync"/"merge" at index 0
        if skip_next {
            skip_next = false;
            continue;
        }
        if known_options.iter().any(|&o| arg == o) {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            // Known flag or unknown flag — skip
            continue;
        }
        return Some(arg.as_str());
    }
    None
}

// ---------------------------------------------------------------------------
// CLI progress callback
// ---------------------------------------------------------------------------

/// Simple CLI progress reporter that prints to stderr.
struct CliProgress;

#[async_trait]
impl ProgressCallback for CliProgress {
    async fn on_phase(&self, phase: Phase) -> Result<()> {
        match phase {
            Phase::Analyzing => eprintln!("Analyzing stack..."),
            Phase::Planning => eprintln!("Planning submission..."),
            Phase::Executing => eprintln!("Executing..."),
            Phase::AddingComments => eprintln!("Adding stack comments..."),
            Phase::Complete => eprintln!("Done."),
        }
        Ok(())
    }

    async fn on_bookmark_push(&self, bookmark: &str, status: PushStatus) -> Result<()> {
        match status {
            PushStatus::Started => eprint!("  Pushing {bookmark}..."),
            PushStatus::Success => eprintln!(" ✓"),
            PushStatus::AlreadySynced => eprintln!(" (already synced)"),
            PushStatus::Failed(ref msg) => eprintln!(" ✗ {msg}"),
        }
        Ok(())
    }

    async fn on_pr_created(&self, bookmark: &str, pr_number: u64, url: &str) -> Result<()> {
        eprintln!("  Created PR #{pr_number} for {bookmark}: {url}");
        Ok(())
    }

    async fn on_pr_updated(&self, bookmark: &str, pr_number: u64) -> Result<()> {
        eprintln!("  Updated PR #{pr_number} for {bookmark}");
        Ok(())
    }

    async fn on_error(&self, message: &str) -> Result<()> {
        eprintln!("  Error: {message}");
        Ok(())
    }

    async fn on_message(&self, message: &str) -> Result<()> {
        eprintln!("  {message}");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract PR title and body from pre-collected plan file entries.
///
/// This avoids rescanning the plan directory per-segment — the caller
/// collects plan files once and passes them in.
fn plan_file_to_pr_content_from_entries(
    plan_files: &[crate::plan_file::PlanFileEntry],
    plan_dir: &std::path::Path,
    bookmark_name: &str,
) -> Option<(String, String)> {
    let entry = plan_files
        .iter()
        .find(|f| f.bookmark_name == bookmark_name)?;

    let content = std::fs::read_to_string(plan_dir.join(&entry.filename)).ok()?;

    if content.trim().is_empty() {
        return None;
    }

    // First line = PR title
    let mut lines = content.lines();
    let title = lines.next()?.to_string();

    if title.trim().is_empty() {
        return None;
    }

    // Remainder = PR body
    let body_raw: String = lines.collect::<Vec<_>>().join("\n");

    // Strip [scratch] sections
    let body_stripped = crate::markdown::strip_scratch_sections(&body_raw);

    // Strip plan-status: ✅ lines
    let body = body_stripped
        .lines()
        .filter(|line| !line.starts_with("plan-status: ✅"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    Some((title, body))
}

// ---------------------------------------------------------------------------
// jj stack submit
// ---------------------------------------------------------------------------

fn run_submit(workspace: &mut Workspace, args: &[String], registry: &PlanRegistry) -> Result<i32> {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        print_submit_help();
        return Ok(0);
    }

    let dry_run = has_flag(args, "--dry-run");
    let draft = has_flag(args, "--draft");
    let allow_gaps = has_flag(args, "--allow-gaps");
    let remote_override = get_option(args, "--remote");
    let target_bookmark = first_positional(args);

    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();

    // Build stack
    let stack_result = build_stack(workspace, Some(registry));
    let stack = match stack_result {
        StackResult::Ok(stack) => stack,
        StackResult::Empty => {
            eprintln!("No plans between trunk and working copy.");
            eprintln!("Create one with: jj plan new <bookmark-name>");
            return Ok(1);
        }

    };

    // Determine target
    let target = if let Some(name) = target_bookmark {
        name.to_string()
    } else if let Some(seg) = find_submit_target(&stack) {
        seg.bookmarks
            .first()
            .map(|b| b.name.clone())
            .unwrap_or_default()
    } else {
        eprintln!("No bookmarked segment found near working copy.");
        eprintln!("Specify a bookmark: jj stack submit <bookmark>");
        return Ok(1);
    };

    // Gap check
    if !allow_gaps {
        let chain = collect_submission_chain(&stack, &target).map_err(|e| {
            JjPlanError::NoStack(e)
        })?;

        if !chain.gaps.is_empty() {
            eprintln!("Error: unbookmarked changes detected between bookmarks.");
            eprintln!();
            eprintln!(
                "The following changes have no bookmark and will be included in the"
            );
            eprintln!("adjacent PR's diff (bookmark at the tip owns all preceding changes):");
            eprintln!();
            for gap in &chain.gaps {
                for change in &gap.unbookmarked {
                    let between = if let Some(ref after) = gap.after_bookmark {
                        format!("between {} and {}", after, gap.before_bookmark)
                    } else {
                        format!("before {}", gap.before_bookmark)
                    };
                    eprintln!("  change {} ({between})", change.short_id);
                    eprintln!("    \"{}\"", change.description_first_line);
                }
            }
            eprintln!();
            eprintln!("Options:");
            eprintln!(
                "  - Squash into adjacent bookmark: jj squash --from <change> --into <bookmark>"
            );
            eprintln!("  - Give it its own bookmark:      jj bookmark create <name> -r <change>");
            eprintln!(
                "  - Allow gaps explicitly:          jj stack submit --allow-gaps"
            );
            return Ok(1);
        }
    }

    // Build tokio runtime and run async submit
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            JjPlanError::Platform(format!("Failed to create async runtime: {e}"))
        })?;

    rt.block_on(async {
        run_submit_async(workspace, &repo_root, &registry, &target, remote_override, dry_run, draft).await
    })
}

async fn run_submit_async(
    workspace: &mut Workspace,
    repo_root: &std::path::Path,
    registry: &PlanRegistry,
    target: &str,
    remote_override: Option<&str>,
    dry_run: bool,
    draft: bool,
) -> Result<i32> {
    let ctx = StackContext::new(workspace, repo_root, remote_override, registry).await?;

    let stack_result = build_stack(workspace, Some(registry));
    let stack = match stack_result {
        StackResult::Ok(stack) => stack,
        _ => {
            eprintln!("Failed to rebuild stack for analysis.");
            return Ok(1);
        }
    };

    // Analyze
    let analysis = analyze_submission(&stack, registry, Some(target), &ctx.default_branch)?;

    // Gather PR content from plan files
    let plan_dir = repo_root.join(".jj-plan");
    let plan_files = crate::plan_file::collect_plan_files(&plan_dir, registry);
    let pr_content: Vec<(String, String, String)> = analysis
        .segments
        .iter()
        .map(|seg| {
            let bookmark = &seg.bookmark.name;
            let (title, body) = plan_file_to_pr_content_from_entries(&plan_files, &plan_dir, bookmark)
                .unwrap_or_else(|| {
                    let desc = seg
                        .changes
                        .first()
                        .map(|c| c.description.clone())
                        .unwrap_or_default();
                    let title = desc.lines().next().unwrap_or("").to_string();
                    (title, desc)
                });
            (bookmark.clone(), title, body)
        })
        .collect();

    // Plan
    let mut pr_cache = ctx.pr_cache;
    let plan = create_submission_plan(
        &analysis,
        ctx.platform.as_ref(),
        &pr_cache,
        &ctx.remote_name,
        draft,
        &pr_content,
    )
    .await?;

    if plan.is_empty() {
        eprintln!("Nothing to submit — stack is already up to date.");
        return Ok(0);
    }

    // Print plan summary
    eprintln!(
        "Submit plan: {} push(es), {} create(s), {} update(s)",
        plan.count_pushes(),
        plan.count_creates(),
        plan.count_updates()
    );

    if dry_run {
        eprintln!();
        eprintln!("Dry run — no changes will be made:");
    }

    // Execute
    let progress: &dyn ProgressCallback = if dry_run {
        &CliProgress
    } else {
        &CliProgress
    };

    let result = execute_submission(&plan, workspace, ctx.platform.as_ref(), &mut pr_cache, progress, dry_run).await?;

    // Save PR cache if we made changes
    if !dry_run && (!result.created.is_empty() || !result.updated.is_empty()) {
        if let Err(e) = save_pr_cache(repo_root, &pr_cache) {
            eprintln!("Warning: failed to save PR cache: {e}");
        }
    }

    // Print summary
    eprintln!();
    if !result.pushed.is_empty() {
        eprintln!("Pushed {} bookmark(s)", result.pushed.len());
    }
    if !result.created.is_empty() {
        eprintln!("Created {} PR(s):", result.created.len());
        for (bookmark, pr) in &result.created {
            eprintln!("  {} → #{} {}", bookmark, pr.number, pr.html_url);
        }
    }
    if !result.updated.is_empty() {
        eprintln!("Updated {} PR(s)", result.updated.len());
    }
    if !result.errors.is_empty() {
        eprintln!("Errors:");
        for err in &result.errors {
            eprintln!("  {err}");
        }
        return Ok(1);
    }

    Ok(0)
}

// ---------------------------------------------------------------------------
// jj stack sync
// ---------------------------------------------------------------------------

fn run_sync(workspace: &mut Workspace, args: &[String], registry: &PlanRegistry) -> Result<i32> {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        print_sync_help();
        return Ok(0);
    }

    let dry_run = has_flag(args, "--dry-run");
    let remote_override = get_option(args, "--remote");

    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();

    // Build tokio runtime and run async sync
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            JjPlanError::Platform(format!("Failed to create async runtime: {e}"))
        })?;

    rt.block_on(async {
        run_sync_async(workspace, &repo_root, remote_override, dry_run, registry).await
    })
}

async fn run_sync_async(
    workspace: &mut Workspace,
    repo_root: &std::path::Path,
    remote_override: Option<&str>,
    dry_run: bool,
    registry: &PlanRegistry,
) -> Result<i32> {
    // Determine remote for fetch
    let remotes = workspace.git_remotes()?;
    let remote_name = crate::workspace::select_remote(&remotes, remote_override)?;

    // Fetch
    if !dry_run {
        eprintln!("Fetching from {remote_name}...");
        workspace.git_fetch(&remote_name)?;
        workspace.reload();
        eprintln!("Fetch complete.");
    } else {
        eprintln!("Dry run — would fetch from {remote_name}");
    }

    // Now do a submit (sync = fetch + submit)
    let stack_result = build_stack(workspace, Some(registry));
    let stack = match stack_result {
        StackResult::Ok(stack) => stack,
        StackResult::Empty => {
            eprintln!("No plans between trunk and working copy after fetch.");
            return Ok(0);
        }

    };

    let narrowed = narrow_segments(&stack, registry);
    if narrowed.is_empty() {
        eprintln!("No plan-registered bookmarks in stack after fetch.");
        return Ok(0);
    }

    let target = narrowed.last().unwrap().bookmark.name.clone();

    run_submit_async(workspace, repo_root, registry, &target, remote_override, dry_run, false).await
}

// ---------------------------------------------------------------------------
// jj stack merge
// ---------------------------------------------------------------------------

fn run_merge(workspace: &mut Workspace, args: &[String], registry: &PlanRegistry) -> Result<i32> {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        print_merge_help();
        return Ok(0);
    }

    let dry_run = has_flag(args, "--dry-run");
    let remote_override = get_option(args, "--remote");

    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            JjPlanError::Platform(format!("Failed to create async runtime: {e}"))
        })?;

    rt.block_on(async {
        run_merge_async(workspace, &repo_root, remote_override, dry_run, registry).await
    })
}

async fn run_merge_async(
    workspace: &mut Workspace,
    repo_root: &std::path::Path,
    remote_override: Option<&str>,
    dry_run: bool,
    registry: &PlanRegistry,
) -> Result<i32> {
    let ctx = StackContext::new(workspace, repo_root, remote_override, registry).await?;

    let stack_result = build_stack(workspace, Some(registry));
    let stack = match stack_result {
        StackResult::Ok(stack) => stack,
        StackResult::Empty => {
            eprintln!("No plans between trunk and working copy.");
            return Ok(0);
        }

    };

    let narrowed = narrow_segments(&stack, registry);
    if narrowed.is_empty() {
        eprintln!("No plan-registered bookmarks in stack.");
        return Ok(0);
    }

    // Fetch PR info for all segments
    let mut pr_info = Vec::new();
    for seg in &narrowed {
        let bookmark = &seg.bookmark.name;

        // Look up PR from cache
        if let Some(cached) = ctx.pr_cache.get(bookmark) {
            match ctx.platform.get_pr_details(cached.number).await {
                Ok(details) => {
                    match ctx.platform.check_merge_readiness(cached.number).await {
                        Ok(readiness) => {
                            pr_info.push(PrInfo {
                                bookmark: bookmark.clone(),
                                details,
                                readiness,
                            });
                        }
                        Err(e) => {
                            eprintln!("Warning: failed to check merge readiness for {bookmark}: {e}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Warning: failed to get PR details for {bookmark}: {e}");
                }
            }
        } else {
            // Try to find PR by branch name
            match ctx.platform.find_existing_pr(bookmark).await {
                Ok(Some(pr)) => {
                    match ctx.platform.get_pr_details(pr.number).await {
                        Ok(details) => {
                            match ctx.platform.check_merge_readiness(pr.number).await {
                                Ok(readiness) => {
                                    pr_info.push(PrInfo {
                                        bookmark: bookmark.clone(),
                                        details,
                                        readiness,
                                    });
                                }
                                Err(e) => {
                                    eprintln!("Warning: failed to check merge readiness for {bookmark}: {e}");
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Warning: failed to get PR details for {bookmark}: {e}");
                        }
                    }
                }
                Ok(None) => {
                    eprintln!("  {bookmark}: no PR found");
                }
                Err(e) => {
                    eprintln!("Warning: failed to find PR for {bookmark}: {e}");
                }
            }
        }
    }

    // Create merge plan
    let merge_plan = create_merge_plan(
        &narrowed,
        &pr_info,
        &ctx.default_branch,
        MergeMethod::Squash,
    );

    if !merge_plan.has_actionable {
        eprintln!("No PRs are ready to merge.");
        for step in &merge_plan.steps {
            if let MergeStep::Skip {
                bookmark, reason, ..
            } = step
            {
                eprintln!("  {bookmark}: {reason}");
            }
        }
        return Ok(0);
    }

    // Print plan
    eprintln!("Merge plan:");
    for step in &merge_plan.steps {
        match step {
            MergeStep::Merge {
                bookmark,
                pr_number,
                confidence,
                ..
            } => {
                let conf = if *confidence == crate::merge::MergeConfidence::Certain {
                    ""
                } else {
                    " (uncertain)"
                };
                eprintln!("  ✓ Merge #{pr_number} ({bookmark}){conf}");
            }
            MergeStep::RetargetBase {
                bookmark,
                pr_number,
                new_base,
            } => {
                eprintln!("    → Retarget #{pr_number} ({bookmark}) base → {new_base}");
            }
            MergeStep::Skip {
                bookmark, reason, ..
            } => {
                eprintln!("  ✗ Skip {bookmark}: {reason}");
            }
        }
    }

    if dry_run {
        eprintln!();
        eprintln!("Dry run — no merges will be performed.");
        return Ok(0);
    }

    // Execute merges
    eprintln!();
    eprintln!("Executing merges...");
    let merge_result = execute_merge(&merge_plan, ctx.platform.as_ref()).await?;

    if !merge_result.merged_bookmarks.is_empty() {
        eprintln!(
            "Merged {} PR(s): {}",
            merge_result.merged_bookmarks.len(),
            merge_result.merged_bookmarks.join(", ")
        );

        // Post-merge cleanup
        let mut pr_cache = ctx.pr_cache;
        // Untrack merged bookmarks from the registry (fixes orphan entries in plans.toml)
        let mut registry_mut = crate::plan_registry::load_registry(repo_root);
        for bookmark in &merge_result.merged_bookmarks {
            pr_cache.remove(bookmark);
            registry_mut.untrack(bookmark);

            // Delete local bookmark
            if let Err(e) = workspace.delete_bookmark(bookmark) {
                eprintln!("Warning: failed to delete bookmark {bookmark}: {e}");
            }

            // Remove plan file
            let plan_dir = repo_root.join(".jj-plan");
            let plan_files = crate::plan_file::collect_plan_files(&plan_dir, &registry_mut);
            if let Some(entry) = plan_files.iter().find(|f| f.bookmark_name == *bookmark) {
                let _ = std::fs::remove_file(plan_dir.join(&entry.filename));
            }
        }
        crate::plan_registry::save_registry(repo_root, &registry_mut);

        // Save updated PR cache
        if let Err(e) = save_pr_cache(repo_root, &pr_cache) {
            eprintln!("Warning: failed to save PR cache: {e}");
        }

        // Fetch to get updated trunk
        eprintln!("Fetching updated trunk...");
        if let Err(e) = workspace.git_fetch(&ctx.remote_name) {
            eprintln!("Warning: failed to fetch after merge: {e}");
        } else {
            workspace.reload();
        }
    }

    if let Some(ref failed) = merge_result.failed_bookmark {
        eprintln!(
            "Failed to merge {}: {}",
            failed,
            merge_result.error_message.as_deref().unwrap_or("unknown error")
        );
        return Ok(1);
    }

    Ok(0)
}

// ---------------------------------------------------------------------------
// jj stack auth
// ---------------------------------------------------------------------------

fn run_auth(args: &[String]) -> Result<i32> {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        print_auth_help();
        return Ok(0);
    }

    // Parse: auth <platform> <action>
    // e.g., auth github test, auth gitlab setup
    let platform = args.get(1).map(|s| s.as_str());
    let action = args.get(2).map(|s| s.as_str());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            JjPlanError::Platform(format!("Failed to create async runtime: {e}"))
        })?;

    match (platform, action) {
        (Some("github"), Some("test")) => {
            rt.block_on(async {
                eprintln!("Testing GitHub authentication...");
                match crate::auth::get_github_auth().await {
                    Ok(auth) => {
                        eprintln!("  Token source: {:?}", auth.source);
                        match crate::auth::test_github_auth(&auth).await {
                            Ok(username) => {
                                eprintln!("  ✓ Authenticated as: {username}");
                                Ok(0)
                            }
                            Err(e) => {
                                eprintln!("  ✗ Authentication failed: {e}");
                                Ok(1)
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  ✗ No authentication found: {e}");
                        Ok(1)
                    }
                }
            })
        }
        (Some("github"), Some("setup")) => {
            eprintln!("GitHub Authentication Setup");
            eprintln!();
            eprintln!("Option 1: GitHub CLI (recommended)");
            eprintln!("  Install: https://cli.github.com/");
            eprintln!("  Run:     gh auth login");
            eprintln!();
            eprintln!("Option 2: Personal Access Token");
            eprintln!("  Create a token at: https://github.com/settings/tokens");
            eprintln!("  Required scopes: repo");
            eprintln!("  Set:  export GITHUB_TOKEN=<your-token>");
            eprintln!("  Or:   export GH_TOKEN=<your-token>");
            eprintln!();
            eprintln!("Token resolution order: gh CLI → GITHUB_TOKEN → GH_TOKEN");
            Ok(0)
        }
        (Some("gitlab"), Some("test")) => {
            rt.block_on(async {
                eprintln!("Testing GitLab authentication...");
                match crate::auth::get_gitlab_auth(None).await {
                    Ok(auth) => {
                        eprintln!("  Token source: {:?}", auth.source);
                        eprintln!("  Host: {}", auth.host);
                        match crate::auth::test_gitlab_auth(&auth).await {
                            Ok(username) => {
                                eprintln!("  ✓ Authenticated as: {username}");
                                Ok(0)
                            }
                            Err(e) => {
                                eprintln!("  ✗ Authentication failed: {e}");
                                Ok(1)
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  ✗ No authentication found: {e}");
                        Ok(1)
                    }
                }
            })
        }
        (Some("gitlab"), Some("setup")) => {
            eprintln!("GitLab Authentication Setup");
            eprintln!();
            eprintln!("Option 1: GitLab CLI (recommended)");
            eprintln!("  Install: https://gitlab.com/gitlab-org/cli");
            eprintln!("  Run:     glab auth login");
            eprintln!();
            eprintln!("Option 2: Personal Access Token");
            eprintln!("  Create a token at: https://<your-host>/-/user_settings/personal_access_tokens");
            eprintln!("  Required scopes: api");
            eprintln!("  Set:  export GITLAB_TOKEN=<your-token>");
            eprintln!("  Or:   export GL_TOKEN=<your-token>");
            eprintln!();
            eprintln!("For self-hosted GitLab:");
            eprintln!("  export GITLAB_HOST=gitlab.example.com");
            eprintln!();
            eprintln!("Token resolution order: glab CLI → GITLAB_TOKEN → GL_TOKEN");
            Ok(0)
        }
        (Some(p), _) if p == "github" || p == "gitlab" => {
            eprintln!("Usage: jj stack auth <github|gitlab> <test|setup>");
            Ok(1)
        }
        _ => {
            print_auth_help();
            Ok(1)
        }
    }
}

// ---------------------------------------------------------------------------
// Stack visualization
// ---------------------------------------------------------------------------

/// A prepared display row for one segment in a stack column.
struct DisplayRow {
    /// The bookmark name for this segment.
    bookmark_name: String,
    /// Short change ID (reverse hex) for display.
    short_change_id: String,
    /// Whether this is the working copy commit.
    is_wc: bool,
    /// Parenthesized indicators like (@), (✓), (PR #3).
    indicator_str: String,
    /// First line of the commit description.
    first_line: String,
}

/// A prepared stack column for multi-column rendering.
struct StackColumn {
    /// Human-readable stack name.
    name: String,
    /// Display rows, tip (index 0) to trunk (last index).
    rows: Vec<DisplayRow>,
    /// Whether this column contains the working copy.
    has_wc: bool,
    /// Gap warnings for this stack.
    gaps: Vec<Gap>,
}

/// Render multi-column graph lines from prepared stack columns.
///
/// Pure function: takes prepared display data, returns lines to print.
/// Each line does NOT include a trailing newline.
///
/// For a single stack, renders identically to the pre-multi-column format
/// (no column gutter, just `○`/`◉` + `│` markers).
///
/// For multiple stacks, renders a compact column gutter on the left where
/// each stack gets a 2-char-wide column (`○ `, `│ `, `◉ `), and the
/// segment content (bookmark name, description) appears to the right of
/// the active column's marker.
fn render_multi_column(columns: &[StackColumn]) -> Vec<String> {
    if columns.is_empty() {
        return vec![
            "No plans between trunk and working copy.".to_string(),
            "Create one with: jj plan new <bookmark-name>".to_string(),
        ];
    }

    let num_cols = columns.len();

    if num_cols == 1 {
        return render_single_column(&columns[0]);
    }

    // Multi-stack: interleave columns in a compact gutter layout.
    //
    // Strategy: render each stack sequentially (largest first, matching
    // the existing sort order from build_multi_stack), but prefix every
    // line with a gutter showing which columns are active.
    //
    // Each column in the gutter is 2 chars wide: "│ " when passive,
    // "○ " or "◉ " when a segment node is on this row.

    let mut lines: Vec<String> = Vec::new();
    lines.push(String::new()); // leading blank line

    for (col_idx, column) in columns.iter().enumerate() {
        // Stack header
        let mut header_gutter = build_gutter(num_cols, col_idx, GutterMark::Header);
        header_gutter.push_str(&format!("stack: {}", column.name));
        lines.push(format!("  {}", header_gutter));

        // Segments from tip to trunk
        for (row_idx, row) in column.rows.iter().enumerate() {
            let mark = if row.is_wc { GutterMark::WorkingCopy } else { GutterMark::Node };
            let mut node_gutter = build_gutter(num_cols, col_idx, mark);
            node_gutter.push_str(&format!(
                "{} {}",
                row.bookmark_name,
                row.indicator_str,
            ));
            lines.push(format!("  {}", node_gutter));

            if !row.first_line.is_empty() {
                let mut desc_gutter = build_gutter(num_cols, col_idx, GutterMark::Continuation);
                desc_gutter.push_str(&format!("  {}", row.first_line));
                lines.push(format!("  {}", desc_gutter));
            }

            // Spacer between segments (not after last)
            if row_idx < column.rows.len() - 1 {
                let spacer_gutter = build_gutter(num_cols, col_idx, GutterMark::Continuation);
                lines.push(format!("  {}", spacer_gutter));
            }
        }

        // Gap warnings
        if !column.gaps.is_empty() {
            let total_unbookmarked: usize = column.gaps.iter().map(|g| g.unbookmarked.len()).sum();
            lines.push(String::new());
            let mut warn_gutter = build_gutter(num_cols, col_idx, GutterMark::Continuation);
            warn_gutter.push_str(&format!("⚠ {} unbookmarked change(s) between plans", total_unbookmarked));
            lines.push(format!("  {}", warn_gutter));
            for gap in &column.gaps {
                for change in &gap.unbookmarked {
                    let mut gap_gutter = build_gutter(num_cols, col_idx, GutterMark::Continuation);
                    gap_gutter.push_str(&format!("  {} {}", change.short_id, change.description_first_line));
                    lines.push(format!("  {}", gap_gutter));
                }
            }
        }

        // Spacer between stacks
        if col_idx < num_cols - 1 {
            let spacer = build_gutter(num_cols, col_idx, GutterMark::Continuation);
            lines.push(format!("  {}", spacer));
        }
    }

    // Trunk merge line
    let merge_line = build_trunk_merge(num_cols);
    lines.push(format!("  {}", merge_line));
    lines.push(format!("  {}trunk()", "◆ "));
    lines.push(String::new());

    lines
}

/// Gutter marker types for multi-column rendering.
enum GutterMark {
    /// A regular (non-working-copy) node: ○
    Node,
    /// The working copy node: ◉
    WorkingCopy,
    /// A continuation/pipe line: │
    Continuation,
    /// A header line (stack name): │ for other columns
    Header,
}

/// Build the gutter prefix for a line in the multi-column layout.
///
/// `num_cols` is the total number of stack columns.
/// `active_col` is the column that "owns" this line.
/// `mark` controls what character appears in the active column.
///
/// Returns a string like "│ ○ " or "│ │ " (2 chars per column).
fn build_gutter(num_cols: usize, active_col: usize, mark: GutterMark) -> String {
    let mut gutter = String::with_capacity(num_cols * 2 + 1);
    for col in 0..num_cols {
        if col == active_col {
            match mark {
                GutterMark::Node => gutter.push_str("○ "),
                GutterMark::WorkingCopy => gutter.push_str("◉ "),
                GutterMark::Continuation | GutterMark::Header => gutter.push_str("│ "),
            }
        } else {
            gutter.push_str("│ ");
        }
    }
    gutter
}

/// Build the trunk merge line for multi-column layout.
///
/// For 1 column: "│"
/// For 2 columns: "├─╯"
/// For 3 columns: "├─┴─╯"
/// For N columns: "├─┴─┴─...─╯"
fn build_trunk_merge(num_cols: usize) -> String {
    if num_cols <= 1 {
        return "│".to_string();
    }
    let mut line = String::from("├─");
    for i in 1..num_cols {
        if i < num_cols - 1 {
            line.push_str("┴─");
        } else {
            line.push('╯');
        }
    }
    line
}

/// Render a single-stack column (no gutter, identical to pre-multi-column output).
fn render_single_column(column: &StackColumn) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    lines.push(String::new()); // leading blank line

    for (i, row) in column.rows.iter().enumerate() {
        let marker = if row.is_wc { "◉" } else { "○" };
        lines.push(format!(
            "  {} {} {} {}",
            marker,
            row.bookmark_name,
            row.short_change_id,
            row.indicator_str,
        ));

        if !row.first_line.is_empty() {
            lines.push(format!("  │ {}", row.first_line));
        }

        if i < column.rows.len() - 1 {
            lines.push("  │".to_string());
        }
    }

    // Gap warnings
    if !column.gaps.is_empty() {
        let total: usize = column.gaps.iter().map(|g| g.unbookmarked.len()).sum();
        lines.push(String::new());
        lines.push(format!("  ⚠ {} unbookmarked change(s) between plans", total));
        for gap in &column.gaps {
            for change in &gap.unbookmarked {
                lines.push(format!("    {} {}", change.short_id, change.description_first_line));
            }
        }
    }

    // Trunk
    lines.push("  │".to_string());
    lines.push("  ◆ trunk()".to_string());
    lines.push(String::new());

    lines
}

/// Prepare display rows from narrowed segments.
///
/// Converts `NarrowedBookmarkSegment`s into `DisplayRow`s ready for
/// rendering. Extracts change IDs via the workspace, and collects
/// indicators (working copy, done, synced, PR number).
fn prepare_display_rows(
    narrowed: &[NarrowedBookmarkSegment],
    workspace: &Workspace,
    pr_cache: Option<&crate::pr_cache::PrCache>,
) -> Vec<DisplayRow> {
    // Reverse to get tip-to-trunk order for display
    narrowed.iter().rev().map(|seg| {
        let bookmark_name = &seg.bookmark.name;
        let tip = seg.changes.first();
        let is_wc = tip.is_some_and(|c| c.is_working_copy);
        let is_synced = seg.bookmark.is_synced;
        let is_done = tip.is_some_and(|c| c.is_done());

        let short_change_id = tip
            .and_then(|c| workspace.short_change_id_from_hex(&c.change_id))
            .unwrap_or_default();

        let mut indicators = Vec::new();
        if is_wc { indicators.push("@".to_string()); }
        if is_done { indicators.push("✓".to_string()); }
        if is_synced { indicators.push("synced".to_string()); }
        if let Some(cache) = pr_cache {
            if let Some(cached_pr) = cache.get(bookmark_name) {
                indicators.push(format!("PR #{}", cached_pr.number));
            }
        }

        let indicator_str = if indicators.is_empty() {
            String::new()
        } else {
            format!("({})", indicators.join(", "))
        };

        let first_line = tip
            .map(|c| c.first_line().to_string())
            .unwrap_or_default();

        DisplayRow {
            bookmark_name: bookmark_name.clone(),
            short_change_id,
            is_wc,
            indicator_str,
            first_line,
        }
    }).collect()
}

/// Show stack visualization with bookmark structure and PR status.
fn show_stack_visualization(workspace: &Workspace, registry: &PlanRegistry) {
    let multi = build_multi_stack(workspace, registry);

    if multi.stacks.is_empty() {
        eprintln!("No plans between trunk and working copy.");
        eprintln!("Create one with: jj plan new <bookmark-name>");
        return;
    }

    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();
    let pr_cache = load_pr_cache(&repo_root).ok();

    // Prepare stack columns
    let columns: Vec<StackColumn> = multi.stacks.iter().map(|group| {
        let narrowed = narrow_segments(
            &crate::types::Stack {
                segments: group.segments.clone(),
                gaps: group.gaps.clone(),
            },
            registry,
        );

        let rows = prepare_display_rows(&narrowed, workspace, pr_cache.as_ref());
        let has_wc = rows.iter().any(|r| r.is_wc);

        StackColumn {
            name: group.name.clone(),
            rows,
            has_wc,
            gaps: group.gaps.clone(),
        }
    }).collect();

    // Render and print
    let lines = render_multi_column(&columns);
    for line in &lines {
        eprintln!("{}", line);
    }
}

// ---------------------------------------------------------------------------
// Help text
// ---------------------------------------------------------------------------

fn print_stack_help() {
    eprintln!("jj stack — stack-oriented PR operations");
    eprintln!();
    eprintln!("Usage: jj stack [SUBCOMMAND]");
    eprintln!();
    eprintln!("When run without a subcommand, displays the current stack with");
    eprintln!("bookmark structure, sync status, and PR status.");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  submit [bookmark]   Push and create/update PRs");
    eprintln!("  sync                Fetch, push, and update stack");
    eprintln!("  merge               Merge approved PRs from bottom of stack");
    eprintln!("  untrack             Stop tracking the current stack");
    eprintln!("  auth                Authentication management");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --help, -h          Show this help message");
}

fn print_submit_help() {
    eprintln!("jj stack submit — push bookmarks and create/update PRs");
    eprintln!();
    eprintln!("Usage: jj stack submit [bookmark] [options]");
    eprintln!();
    eprintln!("If no bookmark is specified, submits up to the tip-most bookmarked");
    eprintln!("segment near the working copy.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --dry-run           Preview what would be done without making changes");
    eprintln!("  --draft             Create new PRs as drafts");
    eprintln!("  --allow-gaps        Allow unbookmarked changes between bookmarks");
    eprintln!("  --remote <remote>   Specify the remote to push to (default: origin)");
    eprintln!("  --help, -h          Show this help message");
}

fn print_sync_help() {
    eprintln!("jj stack sync — fetch from remote and re-submit the stack");
    eprintln!();
    eprintln!("Usage: jj stack sync [options]");
    eprintln!();
    eprintln!("Fetches from the remote, then pushes bookmarks and updates PRs.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --dry-run           Preview what would be done without making changes");
    eprintln!("  --remote <remote>   Specify the remote (default: origin)");
    eprintln!("  --help, -h          Show this help message");
}

fn print_merge_help() {
    eprintln!("jj stack merge — merge approved PRs from the bottom of the stack");
    eprintln!();
    eprintln!("Usage: jj stack merge [options]");
    eprintln!();
    eprintln!("Merges PRs that are approved and passing CI, starting from the");
    eprintln!("bottom of the stack. Stops at the first non-mergeable PR.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --dry-run           Preview the merge plan without merging");
    eprintln!("  --remote <remote>   Specify the remote (default: origin)");
    eprintln!("  --help, -h          Show this help message");
}

fn print_auth_help() {
    eprintln!("jj stack auth — authentication management");
    eprintln!();
    eprintln!("Usage: jj stack auth <platform> <action>");
    eprintln!();
    eprintln!("Platforms: github, gitlab");
    eprintln!("Actions:   test, setup");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  jj stack auth github test    Test GitHub authentication");
    eprintln!("  jj stack auth github setup   Show GitHub setup instructions");
    eprintln!("  jj stack auth gitlab test    Test GitLab authentication");
    eprintln!("  jj stack auth gitlab setup   Show GitLab setup instructions");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Gap;

    fn make_row(name: &str, desc: &str, is_wc: bool) -> DisplayRow {
        DisplayRow {
            bookmark_name: name.to_string(),
            short_change_id: "abcd1234".to_string(),
            is_wc,
            indicator_str: if is_wc { "(@)".to_string() } else { String::new() },
            first_line: desc.to_string(),
        }
    }

    fn make_column(name: &str, rows: Vec<DisplayRow>) -> StackColumn {
        let has_wc = rows.iter().any(|r| r.is_wc);
        StackColumn {
            name: name.to_string(),
            rows,
            has_wc,
            gaps: vec![],
        }
    }

    #[test]
    fn trunk_merge_single_column() {
        assert_eq!(build_trunk_merge(1), "│");
    }

    #[test]
    fn trunk_merge_two_columns() {
        assert_eq!(build_trunk_merge(2), "├─╯");
    }

    #[test]
    fn trunk_merge_three_columns() {
        assert_eq!(build_trunk_merge(3), "├─┴─╯");
    }

    #[test]
    fn trunk_merge_four_columns() {
        assert_eq!(build_trunk_merge(4), "├─┴─┴─╯");
    }

    #[test]
    fn single_stack_renders_without_gutter() {
        let col = make_column("auth", vec![
            make_row("auth-tests", "Add tests", false),
            make_row("auth-refactor", "Refactor auth", false),
        ]);
        let lines = render_multi_column(&[col]);
        let output = lines.join("\n");

        // Should have ○ markers (not ◉ since neither is WC)
        assert!(output.contains("○ auth-tests"), "should show auth-tests node");
        assert!(output.contains("○ auth-refactor"), "should show auth-refactor node");
        // Should have trunk
        assert!(output.contains("◆ trunk()"), "should show trunk");
        // Should NOT have "stack:" header (single-stack case)
        assert!(!output.contains("stack:"), "single-stack should not show stack header");
        // Should NOT have multi-column gutter (no "│ ○" pattern)
        assert!(!output.contains("│ ○"), "single-stack should not have column gutter");
    }

    #[test]
    fn single_stack_shows_working_copy_marker() {
        let col = make_column("feat", vec![
            make_row("feat-api", "Feature API", true),
        ]);
        let lines = render_multi_column(&[col]);
        let output = lines.join("\n");

        assert!(output.contains("◉ feat-api"), "working copy should use ◉ marker");
    }

    #[test]
    fn multi_stack_shows_stack_headers() {
        let cols = vec![
            make_column("auth", vec![
                make_row("auth-refactor", "Refactor auth", false),
            ]),
            make_column("dashboard", vec![
                make_row("dash-api", "Dashboard API", true),
            ]),
        ];
        let lines = render_multi_column(&cols);
        let output = lines.join("\n");

        assert!(output.contains("stack: auth"), "should show auth stack header");
        assert!(output.contains("stack: dashboard"), "should show dashboard stack header");
    }

    #[test]
    fn multi_stack_shows_column_gutter() {
        let cols = vec![
            make_column("auth", vec![
                make_row("auth-refactor", "Refactor auth", false),
            ]),
            make_column("dashboard", vec![
                make_row("dash-api", "Dashboard API", true),
            ]),
        ];
        let lines = render_multi_column(&cols);
        let output = lines.join("\n");

        // The auth column (col 0) should show ○ with a │ gutter for col 1
        assert!(output.contains("○ │"), "auth column node should have gutter for dashboard");
        // The dashboard column (col 1) should show ◉ with a │ gutter for col 0
        assert!(output.contains("│ ◉"), "dashboard column node should have gutter for auth");
        // Trunk merge
        assert!(output.contains("├─╯"), "two columns should merge at trunk with ├─╯");
        assert!(output.contains("◆ trunk()"), "should show trunk");
    }

    #[test]
    fn multi_stack_three_columns_merge() {
        let cols = vec![
            make_column("a", vec![make_row("a1", "A", false)]),
            make_column("b", vec![make_row("b1", "B", false)]),
            make_column("c", vec![make_row("c1", "C", true)]),
        ];
        let lines = render_multi_column(&cols);
        let output = lines.join("\n");

        assert!(output.contains("├─┴─╯"), "three columns should merge with ├─┴─╯");
    }

    #[test]
    fn empty_stacks_shows_help() {
        let lines = render_multi_column(&[]);
        assert!(lines[0].contains("No plans"));
    }

    #[test]
    fn column_assignment_matches_input_order() {
        // build_multi_stack sorts by segment count descending.
        // render_multi_column preserves that order: index 0 = leftmost column.
        let cols = vec![
            make_column("largest", vec![
                make_row("l-2", "L2", false),
                make_row("l-1", "L1", false),
            ]),
            make_column("medium", vec![
                make_row("m-1", "M1", true),
            ]),
            make_column("small", vec![
                make_row("s-1", "S1", false),
            ]),
        ];
        let lines = render_multi_column(&cols);

        // Find the line indices of each stack header
        let largest_idx = lines.iter().position(|l| l.contains("stack: largest")).unwrap();
        let medium_idx = lines.iter().position(|l| l.contains("stack: medium")).unwrap();
        let small_idx = lines.iter().position(|l| l.contains("stack: small")).unwrap();

        // Stacks should appear in order: largest first (top), then medium, then small
        assert!(largest_idx < medium_idx, "largest stack should render before medium");
        assert!(medium_idx < small_idx, "medium stack should render before small");
    }
}