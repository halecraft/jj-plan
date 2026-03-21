use crate::error::{JjPlanError, Result};
use crate::jj_binary::JjBinary;
use crate::merge::{create_merge_plan, execute_merge, MergeStep, PrInfo};
use crate::plan_dir::PlanDir;
use crate::pr_cache::{load_pr_cache, save_pr_cache};
use crate::stack_builder::{build_stack, collect_submission_chain, find_submit_target, narrow_segments};
use crate::stack_context::StackContext;
use crate::submit::{
    analyze_submission, create_submission_plan, execute_submission,
    Phase, ProgressCallback, PushStatus,
};
use crate::types::{MergeMethod, PlanRegistry, StackResult};
use crate::workspace::Workspace;

use async_trait::async_trait;

/// Dispatch `jj stack <subcommand>` to the appropriate handler.
///
/// `args` is the full argument list starting with "stack".
/// For example: `["stack", "submit"]` or `["stack", "--help"]`.
pub fn dispatch_stack(
    _jj: &JjBinary,
    _plan_dir: &PlanDir,
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
            // Bare `jj stack` — show stack visualization
            show_stack_visualization(workspace, registry);
            Ok(0)
        }
        Some("submit") => run_submit(workspace, &args[1..], registry),
        Some("sync") => run_sync(workspace, &args[1..], registry),
        Some("merge") => run_merge(workspace, &args[1..], registry),
        Some("auth") => run_auth(&args[1..]),
        Some(unknown) => {
            eprintln!("jj stack: unknown subcommand '{}'", unknown);
            eprintln!();
            eprintln!("Available subcommands: submit, sync, merge, auth");
            eprintln!("Run 'jj stack --help' for more information.");
            Ok(1)
        }
    }
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
        StackResult::MergeCommits => {
            eprintln!("Stack contains merge commits — rebase to create a linear history first.");
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
        StackResult::MergeCommits => {
            eprintln!("Stack contains merge commits after fetch — rebase first.");
            return Ok(1);
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
        StackResult::MergeCommits => {
            eprintln!("Stack contains merge commits — rebase first.");
            return Ok(1);
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

/// Show stack visualization with bookmark structure and PR status.
fn show_stack_visualization(workspace: &Workspace, registry: &PlanRegistry) {
    let stack_result = build_stack(workspace, Some(registry));

    match stack_result {
        StackResult::Empty => {
            eprintln!("No plans between trunk and working copy.");
            eprintln!("Create one with: jj plan new <bookmark-name>");
        }
        StackResult::MergeCommits => {
            eprintln!("Stack contains merge commits — cannot display as a linear stack.");
            eprintln!("Rebase to create a linear history first.");
        }
        StackResult::Ok(stack) => {
            // Load PR cache for status display
            let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();
            let pr_cache = load_pr_cache(&repo_root).ok();

            let narrowed = narrow_segments(&stack, registry);

            // Display leaf-first (reverse of trunk-to-tip order)
            if narrowed.is_empty() && stack.segments.is_empty() {
                eprintln!("No plans between trunk and working copy.");
                eprintln!("Create one with: jj plan new <bookmark-name>");
                return;
            }

            eprintln!();

            // Show segments from tip to trunk
            for (i, seg) in narrowed.iter().enumerate().rev() {
                let bookmark_name = &seg.bookmark.name;
                let tip = seg.changes.first();
                let is_wc = tip.is_some_and(|c| c.is_working_copy);
                let is_synced = seg.bookmark.is_synced;
                let is_done = tip.is_some_and(|c| c.is_done());

                // Resolve short change ID for display.
                // LogEntry.change_id stores standard hex; we need the short
                // reverse-hex form that jj uses for display and revsets.
                let short_change_id = tip
                    .and_then(|c| workspace.short_change_id_from_hex(&c.change_id))
                    .unwrap_or_default();

                // Build status indicators
                let mut indicators = Vec::new();

                // Working copy marker
                if is_wc {
                    indicators.push("@".to_string());
                }

                // Done marker
                if is_done {
                    indicators.push("✓".to_string());
                }

                // Sync status
                if is_synced {
                    indicators.push("synced".to_string());
                }

                // PR status from cache
                if let Some(ref cache) = pr_cache {
                    if let Some(cached_pr) = cache.get(bookmark_name) {
                        indicators.push(format!("PR #{}", cached_pr.number));
                    }
                }

                let indicator_str = if indicators.is_empty() {
                    String::new()
                } else {
                    format!("({})", indicators.join(", "))
                };

                eprintln!(
                    "  {} {} {} {}",
                    if is_wc { "◉" } else { "○" },
                    bookmark_name,
                    short_change_id,
                    indicator_str,
                );

                // Show first line of description
                if let Some(change) = tip {
                    eprintln!("  │ {}", change.first_line());
                }

                if i > 0 {
                    eprintln!("  │");
                }
            }

            // Show gaps if any
            if !stack.gaps.is_empty() {
                eprintln!();
                eprintln!(
                    "  ⚠ {} unbookmarked change(s) between plans",
                    stack
                        .gaps
                        .iter()
                        .map(|g| g.unbookmarked.len())
                        .sum::<usize>()
                );
                for gap in &stack.gaps {
                    for change in &gap.unbookmarked {
                        eprintln!("    {} {}", change.short_id, change.description_first_line);
                    }
                }
            }

            // Show trunk at bottom
            eprintln!("  │");
            eprintln!("  ◆ trunk()");
            eprintln!();
        }
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