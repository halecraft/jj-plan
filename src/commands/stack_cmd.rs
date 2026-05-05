use crate::error::{JjPlanError, Result};
use crate::jj_binary::JjBinary;
use crate::merge::{create_merge_plan, poll_readiness, MergeCandidate, PollConfig, ReadinessOutcome};
use crate::plan_dir::{self, PlanDir};
use crate::plan_registry;
use crate::pr_cache::save_pr_cache;
use crate::stack_builder::{build_multi_stack, build_stack, collect_submission_chain, find_submit_target, narrow_segments};
use crate::stack_context::StackContext;
use crate::stack_render::StackFormat;
use crate::submit::{
    analyze_submission, comments, create_submission_plan, execute_submission,
    ExecutionStep, NoopProgress, SubmissionPlan,
    Phase, ProgressCallback, PushStatus,
};
use crate::types::{MergeMethod, PlanRegistry, StackResult};
use crate::workspace::Workspace;

use async_trait::async_trait;

// ---------------------------------------------------------------------------
// Dispatch-level argument parsing
// ---------------------------------------------------------------------------

/// Parsed dispatch-level arguments for `jj stack`.
///
/// Separates flags (`--help`, `--all`, `--format`) from the positional
/// subcommand so that flag order doesn't matter and combined flags
/// (e.g. `jj stack --all --format=regular`) work correctly.
struct StackDispatchArgs<'a> {
    /// The first positional argument (subcommand name), if any.
    subcommand: Option<&'a str>,
    /// Whether `--help` or `-h` was present.
    show_help: bool,
    /// Whether `--all` was present.
    show_all: bool,
    /// The value of `--format=X` or `--format X`, if present.
    format_override: Option<&'a str>,
}

/// Parse dispatch-level arguments from the full `jj stack ...` arg list.
///
/// Scans `args[1..]` (skipping `"stack"` at index 0), classifying each
/// token as a known dispatch flag, a known dispatch option, or a positional.
/// The first positional becomes `subcommand`. Unknown flags (e.g. `--dry-run`)
/// are ignored here — they are passed through to sub-command handlers.
fn parse_stack_dispatch_args(args: &[String]) -> StackDispatchArgs<'_> {
    let mut subcommand: Option<&str> = None;
    let mut show_help = false;
    let mut show_all = false;
    let mut format_override: Option<&str> = None;

    let mut i = 1; // skip args[0] which is "stack"
    while i < args.len() {
        let arg = args[i].as_str();

        match arg {
            "--help" | "-h" => show_help = true,
            "--all" => show_all = true,
            "--format" => {
                // --format VALUE (separate args)
                if i + 1 < args.len() {
                    i += 1;
                    format_override = Some(args[i].as_str());
                }
            }
            _ if arg.starts_with("--format=") => {
                // --format=VALUE (equals form)
                format_override = Some(&arg["--format=".len()..]);
            }
            _ if arg.starts_with('-') => {
                // Unknown flag — skip (will be passed to sub-command handler)
            }
            _ => {
                // First positional = subcommand
                if subcommand.is_none() {
                    subcommand = Some(arg);
                }
                // Subsequent positionals are ignored at dispatch level
            }
        }

        i += 1;
    }

    StackDispatchArgs {
        subcommand,
        show_help,
        show_all,
        format_override,
    }
}

/// Dispatch `jj stack <subcommand>` to the appropriate handler.
///
/// `args` is the full argument list starting with "stack".
/// For example: `["stack", "submit"]` or `["stack", "--help"]`.
///
/// `format` is the env-derived default (`resolved_stack_format()`).
/// A `--format=X` flag in `args` takes precedence.
pub fn dispatch_stack(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    args: &[String],
    workspace: &mut Workspace,
    registry: &PlanRegistry,
    format: StackFormat,
) -> Result<i32> {
    let parsed = parse_stack_dispatch_args(args);

    // Resolve effective format: --format flag overrides env-derived default
    let effective_format = match parsed.format_override {
        Some("regular") => StackFormat::Regular,
        Some("compact") => StackFormat::Compact,
        Some(unknown) => {
            eprintln!("jj stack: unknown format '{}' (expected 'compact' or 'regular')", unknown);
            return Ok(1);
        }
        None => format,
    };

    // Help handling
    if parsed.show_help {
        print_stack_help();
        return Ok(0);
    }

    // --all flag: show all stacks across the repo
    if parsed.show_all {
        crate::flush::flush_all(&plan_dir.path, jj, workspace, registry);
        workspace.reload();
        // Cleanup stale bookmarks + migrate legacy filenames, then sync plan
        // files to disk. Display is handled exclusively by show_all_stacks
        // (multi-stack view), so we use sync_to_disk instead of sync_and_show
        // to avoid an unwanted single-stack display.
        crate::wrap::cleanup_stale_and_migrate(plan_dir, workspace, registry);
        let _ = crate::wrap::sync_to_disk(plan_dir, workspace, registry);
        show_all_stacks(plan_dir, workspace, registry, effective_format);
        return Ok(0);
    }

    match parsed.subcommand {
        None => {
            // Bare `jj stack` — flush pending edits, sync plan files, then show visualization.
            crate::flush::flush_all(&plan_dir.path, jj, workspace, registry);
            workspace.reload();
            crate::wrap::full_sync_and_show(plan_dir, workspace, registry, effective_format);
            Ok(0)
        }
        Some("submit") => run_submit(workspace, &args[1..], registry),
        Some("sync") => run_sync(workspace, &args[1..], registry),
        Some("merge") => run_merge(workspace, &args[1..], registry),
        Some("auth") => run_auth(&args[1..]),
        Some("untrack") => run_stack_untrack(jj, plan_dir, &args[1..], workspace, registry, effective_format),
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
    format: StackFormat,
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
    if let Some(ref bb) = base_bookmark
        && let Err(e) = workspace.delete_bookmark(bb) {
            eprintln!("jj stack untrack: warning: failed to delete bookmark '{}': {}", bb, e);
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
    crate::wrap::sync_and_show(plan_dir, workspace, &post_registry, format);

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
    let _known_flags = [
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
/// Thin imperative shell: reads the file, delegates to `PlanDocument::pr_parts`.
fn plan_file_to_pr_content_from_entries(
    plan_files: &[crate::plan_file::PlanFileEntry],
    plan_dir: &std::path::Path,
    bookmark_name: &str,
) -> Option<(String, String)> {
    let entry = plan_files
        .iter()
        .find(|f| f.bookmark_name == bookmark_name)?;

    let content = std::fs::read_to_string(plan_dir.join(&entry.filename)).ok()?;

    crate::markdown::PlanDocument::parse(&content).pr_parts()
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
    let publish = has_flag(args, "--publish");
    let update_descriptions = has_flag(args, "--update-descriptions");
    let no_comments = has_flag(args, "--no-comments");
    let continue_on_error = has_flag(args, "--continue-on-error");
    let allow_gaps = has_flag(args, "--allow-gaps");
    let remote_override = get_option(args, "--remote");
    let target_bookmark = first_positional(args);

    // Mutual exclusion: --draft and --publish cannot coexist.
    if draft && publish {
        eprintln!("Error: --draft and --publish are mutually exclusive.");
        eprintln!();
        eprintln!("  --draft     creates new PRs as drafts");
        eprintln!("  --publish   converts existing draft PRs to ready-for-review");
        eprintln!();
        eprintln!("These cannot be used together.");
        return Ok(1);
    }

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
        run_submit_async(workspace, &repo_root, registry, &target, remote_override, dry_run, draft, update_descriptions, publish, no_comments, continue_on_error).await
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_submit_async(
    workspace: &mut Workspace,
    repo_root: &std::path::Path,
    registry: &PlanRegistry,
    target: &str,
    remote_override: Option<&str>,
    dry_run: bool,
    draft: bool,
    update_descriptions: bool,
    publish: bool,
    no_comments: bool,
    continue_on_error: bool,
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
        update_descriptions,
        publish,
    )
    .await?;

    if plan.is_empty() {
        eprintln!("Nothing to submit — stack is already up to date.");
        return Ok(0);
    }

    // Print plan summary
    let desc_updates = plan.count_description_updates();
    let publishes = plan.count_publishes();
    let mut parts = vec![
        format!("{} push(es)", plan.count_pushes()),
        format!("{} create(s)", plan.count_creates()),
        format!("{} update(s)", plan.count_updates()),
    ];
    if desc_updates > 0 {
        parts.push(format!("{desc_updates} description update(s)"));
    }
    if publishes > 0 {
        parts.push(format!("{publishes} publish(es)"));
    }
    eprintln!("Submit plan: {}", parts.join(", "));

    if dry_run {
        eprintln!();
        eprintln!("Dry run — no changes will be made:");
    }

    // Pre-push fetch: refresh tracking refs for bookmarks we're about to push.
    // This ensures `expected_current_target` reflects the remote's actual state,
    // preventing stale-lease failures from `--force-with-lease`.
    if !dry_run {
        let push_bookmarks: Vec<&str> = plan
            .steps
            .iter()
            .filter_map(|step| match step {
                ExecutionStep::Push { bookmark } => Some(bookmark.as_str()),
                _ => None,
            })
            .collect();

        if !push_bookmarks.is_empty()
            && let Err(e) = workspace.git_fetch_bookmarks(&ctx.remote_name, &push_bookmarks) {
                // Fetch failure is non-fatal — push may still succeed if
                // tracking state happens to be correct.
                eprintln!("Warning: pre-push fetch failed: {e}");
            }
    }

    // Execute — use NoopProgress for dry-run to avoid duplicating messages
    // (dry-run output comes from execute_submission's own dry-run branch).
    let noop = NoopProgress;
    let progress: &dyn ProgressCallback = if dry_run {
        &noop
    } else {
        &CliProgress
    };

    let abort_on_error = !continue_on_error;
    let result = execute_submission(&plan, workspace, ctx.platform.as_ref(), &mut pr_cache, progress, dry_run, abort_on_error).await?;

    // Save PR cache if we made changes
    if !dry_run && (!result.created.is_empty() || !result.updated.is_empty())
        && let Err(e) = save_pr_cache(repo_root, &pr_cache) {
            eprintln!("Warning: failed to save PR cache: {e}");
        }

    // --- Pass 2: Stack comments ---
    // Comment steps depend on PR numbers from freshly-created PRs,
    // so they run after the main plan executes.
    let comment_result = if !no_comments && result.errors.is_empty() {
        // Build the chain: (bookmark, pr_number, title) for all segments.
        // PR numbers come from cache (updated during execution) + freshly created.
        let mut chain: Vec<(String, u64, String)> = Vec::new();
        for (bookmark, title, _body) in &pr_content {
            let pr_number = result
                .created
                .iter()
                .find(|(b, _)| b == bookmark)
                .map(|(_, pr)| pr.number)
                .or_else(|| pr_cache.get(bookmark).map(|c| c.number));

            if let Some(number) = pr_number {
                chain.push((bookmark.clone(), number, title.clone()));
            }
        }

        if !chain.is_empty() {
            // Add stack comments for any non-empty chain, including single-PR stacks
            // (replaces stale multi-PR comments when a stack shrinks).
            let mut comment_steps = Vec::new();

            for (bookmark, pr_number, _title) in &chain {
                // Look up existing jj-plan comment on this PR.
                let existing_comment_id = if !dry_run {
                    match ctx.platform.list_pr_comments(*pr_number).await {
                        Ok(pr_comments) => comments::find_existing_comment(&pr_comments),
                        Err(e) => {
                            eprintln!("Warning: failed to list comments on #{pr_number}: {e}");
                            None
                        }
                    }
                } else {
                    None
                };

                let comment_body = comments::generate_stack_comment(&chain, bookmark, &ctx.default_branch);

                comment_steps.push(ExecutionStep::AddStackComment {
                    bookmark: bookmark.clone(),
                    pr_number: *pr_number,
                    comment_body,
                    existing_comment_id,
                });
            }

            let comment_plan = SubmissionPlan {
                steps: comment_steps,
                remote: ctx.remote_name.clone(),
            };

            Some(
                execute_submission(
                    &comment_plan,
                    workspace,
                    ctx.platform.as_ref(),
                    &mut pr_cache,
                    progress,
                    dry_run,
                    false, // comments are independent — don't abort on error
                )
                .await?,
            )
        } else {
            None
        }
    } else {
        None
    };

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
    if !result.description_updated.is_empty() {
        eprintln!("Updated {} description(s)", result.description_updated.len());
    }
    if !result.published.is_empty() {
        eprintln!("Published {} PR(s)", result.published.len());
    }
    if let Some(ref cr) = comment_result
        && !cr.comments.is_empty() {
            eprintln!("Updated {} stack comment(s)", cr.comments.len());
        }
    if !result.errors.is_empty() {
        eprintln!("Errors:");
        for err in &result.errors {
            eprintln!("  {err}");
        }
        return Ok(1);
    }
    if let Some(ref cr) = comment_result
        && !cr.errors.is_empty() {
            eprintln!("Comment errors:");
            for err in &cr.errors {
                eprintln!("  {err}");
            }
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

    run_submit_async(workspace, repo_root, registry, &target, remote_override, dry_run, false, false, false, false, false).await
}

// ---------------------------------------------------------------------------
// jj stack merge
// ---------------------------------------------------------------------------

/// Format an error message with PR URL appended if available in cache.
fn format_pr_error(
    pr_cache: &crate::pr_cache::PrCache,
    bookmark: &str,
    pr_number: u64,
    message: &str,
) -> String {
    let url = pr_cache.get(bookmark).map(|c| c.url.as_str());
    match url {
        Some(u) => format!("{message}\n  → {u}"),
        None => format!("{message}\n  → PR #{pr_number}"),
    }
}

fn run_merge(workspace: &mut Workspace, args: &[String], registry: &PlanRegistry) -> Result<i32> {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        print_merge_help();
        return Ok(0);
    }

    let dry_run = has_flag(args, "--dry-run");
    let wait = has_flag(args, "--wait");
    let remote_override = get_option(args, "--remote");

    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            JjPlanError::Platform(format!("Failed to create async runtime: {e}"))
        })?;

    rt.block_on(async {
        run_merge_async(workspace, &repo_root, remote_override, dry_run, wait, registry).await
    })
}

async fn run_merge_async(
    workspace: &mut Workspace,
    repo_root: &std::path::Path,
    remote_override: Option<&str>,
    dry_run: bool,
    wait: bool,
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

    // Look up PR numbers for each segment (from cache or find_existing_pr).
    // No readiness assessment here — readiness is checked just-in-time.
    let mut candidates = Vec::new();
    for seg in &narrowed {
        let bookmark = &seg.bookmark.name;

        let pr_number = if let Some(cached) = ctx.pr_cache.get(bookmark) {
            Some(cached.number)
        } else {
            match ctx.platform.find_existing_pr(bookmark).await {
                Ok(Some(pr)) => Some(pr.number),
                Ok(None) => {
                    eprintln!("  {bookmark}: no PR found — skipping");
                    None
                }
                Err(e) => {
                    eprintln!("Warning: failed to find PR for {bookmark}: {e}");
                    None
                }
            }
        };

        if let Some(number) = pr_number {
            candidates.push(MergeCandidate {
                bookmark: bookmark.clone(),
                pr_number: number,
            });
        }
    }

    if candidates.is_empty() {
        eprintln!("No PRs found for any bookmarks in the stack.");
        return Ok(0);
    }

    // Create merge plan (functional core — only Merge steps, no retarget).
    let merge_plan = create_merge_plan(
        &candidates,
        &ctx.default_branch,
        MergeMethod::Squash,
    );

    // Print the intended plan — show only what will actually happen.
    eprintln!("Merge plan:");
    if wait {
        // --wait: show the full plan since all merges will be attempted.
        for (i, candidate) in candidates.iter().enumerate() {
            if i == 0 {
                eprintln!("  • Merge #{} ({})", candidate.pr_number, candidate.bookmark);
            } else {
                eprintln!(
                    "  • Merge #{} ({}) [after CI passes]",
                    candidate.pr_number, candidate.bookmark
                );
            }
            if i + 1 < candidates.len() {
                eprintln!(
                    "    ↳ fetch trunk, rebase remaining stack, push, retarget #{} base → {}",
                    candidates[i + 1].pr_number, ctx.default_branch
                );
            }
        }
    } else {
        // Default: show only the first merge + rebase step.
        let first = &candidates[0];
        eprintln!("  • Merge #{} ({})", first.pr_number, first.bookmark);
        let remaining = candidates.len() - 1;
        if remaining > 0 {
            eprintln!(
                "    ↳ fetch trunk, rebase {} remaining PR(s), push",
                remaining
            );
            eprintln!(
                "  ({} more PR(s) will be ready after CI passes — use --wait to continue automatically)",
                remaining
            );
        }
    }

    if dry_run {
        eprintln!();
        eprintln!("Dry run — no merges will be performed.");
        eprintln!("(Readiness will be checked just-in-time during execution.)");
        return Ok(0);
    }

    // ── Imperative merge loop ────────────────────────────────────────
    //
    // For each candidate:
    //   1. Poll readiness (short-poll for transient mergeable status)
    //   2. Merge via forge API
    //   3. Cleanup (registry, cache, bookmark, plan files)
    //   4. If more remain: fetch trunk → rebase → push → retarget → stop or wait
    //
    eprintln!();
    eprintln!("Executing merges...");

    let mut pr_cache = ctx.pr_cache;
    let mut registry_mut = crate::plan_registry::load_registry(repo_root);
    let mut merged_count: usize = 0;

    for (i, step) in merge_plan.steps.iter().enumerate() {
        let crate::merge::MergeStep::Merge {
            bookmark,
            pr_number,
            method,
        } = step;

        // ── 1. Poll readiness ────────────────────────────────────
        let transient_config = PollConfig::transient();
        match poll_readiness(ctx.platform.as_ref(), *pr_number, bookmark, &transient_config).await {
            Ok(ReadinessOutcome::Ready) => {
                // Proceed to merge.
            }
            Ok(ReadinessOutcome::Transient) => {
                // Shouldn't happen (poll_readiness returns Ready after exhaustion),
                // but attempt anyway.
                eprintln!(
                    "Warning: #{} ({}) still transient after polling, attempting merge",
                    pr_number, bookmark
                );
            }
            Ok(ReadinessOutcome::Blocked(reasons)) => {
                let reason_str = reasons.join(", ");
                let msg = format!("#{} ({}) blocked: {}", pr_number, bookmark, reason_str);
                eprintln!(
                    "  ✗ {}",
                    format_pr_error(&pr_cache, bookmark, *pr_number, &msg)
                );
                return Ok(1);
            }
            Err(e) => {
                eprintln!(
                    "Warning: readiness check failed for #{} ({}): {}",
                    pr_number, bookmark, e
                );
                // Continue anyway — the merge attempt is the definitive test.
            }
        }

        // ── 2. Merge via forge API ───────────────────────────────
        eprintln!("  Merging #{} ({})...", pr_number, bookmark);
        match ctx.platform.merge_pr(*pr_number, *method).await {
            Ok(merge_result) => {
                if merge_result.merged {
                    eprintln!("  ✓ #{} ({}) merged", pr_number, bookmark);
                    merged_count += 1;
                } else {
                    let reason = merge_result.message.as_deref().unwrap_or("unknown error");
                    let msg = format!("Failed to merge #{} ({}): {}", pr_number, bookmark, reason);
                    eprintln!(
                        "  ✗ {}",
                        format_pr_error(&pr_cache, bookmark, *pr_number, &msg)
                    );
                    if reason.to_lowercase().contains("conflict")
                        || reason.to_lowercase().contains("not mergeable")
                    {
                        eprintln!(
                            "  The PR has merge conflicts. This is unexpected after rebase — check the PR on GitHub."
                        );
                    }
                    return Ok(1);
                }
            }
            Err(e) => {
                let msg = format!("Failed to merge #{} ({}): {}", pr_number, bookmark, e);
                eprintln!(
                    "  ✗ {}",
                    format_pr_error(&pr_cache, bookmark, *pr_number, &msg)
                );
                return Ok(1);
            }
        }

        // ── 3. Per-merge cleanup ─────────────────────────────────
        pr_cache.remove(bookmark);
        registry_mut.untrack(bookmark);
        crate::plan_registry::save_registry(repo_root, &registry_mut);
        if let Err(e) = save_pr_cache(repo_root, &pr_cache) {
            eprintln!("Warning: failed to save PR cache: {e}");
        }
        if let Err(e) = workspace.delete_bookmark(bookmark) {
            eprintln!("Warning: failed to delete bookmark {bookmark}: {e}");
        }

        // ── 4. Between-merge lifecycle ───────────────────────────
        let remaining = &candidates[i + 1..];
        if remaining.is_empty() {
            // Last merge — just sync plan files and we're done.
            let plan_dir = crate::plan_dir::resolve_plan_dir(Some(repo_root));
            if let Some(ref pd) = plan_dir {
                let _ = crate::wrap::sync_to_disk(pd, workspace, &registry_mut);
            }
            break;
        }

        // 4a. Fetch updated trunk (selective — only the default branch).
        eprintln!("  Fetching updated trunk...");
        if let Err(e) =
            workspace.git_fetch_bookmarks(&ctx.remote_name, &[&ctx.default_branch])
        {
            eprintln!("Warning: failed to fetch trunk: {e}");
            // Continue anyway — rebase will use whatever trunk we have.
        }

        // 4b. Rebase remaining stack onto new trunk.
        let next_bookmark = &remaining[0].bookmark;
        eprintln!("  Rebasing remaining stack onto trunk...");
        if let Err(_e) = workspace.rebase_bookmark_onto_trunk(next_bookmark) {
            let msg = format!(
                "Rebase conflict: could not rebase {} onto trunk. Resolve manually with 'jj rebase' and re-push.",
                next_bookmark
            );
            eprintln!(
                "  ✗ {}",
                format_pr_error(&pr_cache, next_bookmark, remaining[0].pr_number, &msg)
            );
            // Sync plan files before returning so the merged PR is cleaned up.
            let plan_dir = crate::plan_dir::resolve_plan_dir(Some(repo_root));
            if let Some(ref pd) = plan_dir {
                let _ = crate::wrap::sync_to_disk(pd, workspace, &registry_mut);
            }
            return Ok(1);
        }

        // 4c. Force-push all remaining bookmarks.
        eprintln!("  Pushing rebased bookmarks...");
        let mut push_failed = false;
        for rem in remaining {
            match workspace.git_push(&rem.bookmark, &ctx.remote_name) {
                Ok(crate::workspace::PushOutcome::Success) => {
                    eprintln!("    ✓ pushed {}", rem.bookmark);
                }
                Ok(crate::workspace::PushOutcome::Rejected { reason }) => {
                    let msg = format!(
                        "Force-push rejected for {}: {}. Try 'jj git fetch' to refresh tracking state.",
                        rem.bookmark, reason
                    );
                    eprintln!(
                        "    ✗ {}",
                        format_pr_error(&pr_cache, &rem.bookmark, rem.pr_number, &msg)
                    );
                    push_failed = true;
                    break;
                }
                Ok(crate::workspace::PushOutcome::RemoteRejected { reason }) => {
                    let msg = format!(
                        "Force-push rejected by remote for {}: {}",
                        rem.bookmark, reason
                    );
                    eprintln!(
                        "    ✗ {}",
                        format_pr_error(&pr_cache, &rem.bookmark, rem.pr_number, &msg)
                    );
                    push_failed = true;
                    break;
                }
                Err(e) => {
                    eprintln!("    ✗ Failed to push {}: {}", rem.bookmark, e);
                    push_failed = true;
                    break;
                }
            }
        }

        if push_failed {
            // Sync plan files before returning.
            let plan_dir = crate::plan_dir::resolve_plan_dir(Some(repo_root));
            if let Some(ref pd) = plan_dir {
                let _ = crate::wrap::sync_to_disk(pd, workspace, &registry_mut);
            }
            return Ok(1);
        }

        // 4d. Retarget next PR's base to trunk.
        let next_pr = remaining[0].pr_number;
        eprintln!(
            "  Retargeting #{} ({}) base → {}",
            next_pr, next_bookmark, ctx.default_branch
        );
        if let Err(e) = ctx.platform.update_pr_base(next_pr, &ctx.default_branch).await {
            eprintln!(
                "  Warning: failed to retarget #{} ({}): {}",
                next_pr, next_bookmark, e
            );
            // Non-fatal — readiness polling will detect if the retarget didn't take.
        }

        // 4e. Sync plan files (clean up merged PR's plan file).
        let plan_dir = crate::plan_dir::resolve_plan_dir(Some(repo_root));
        if let Some(ref pd) = plan_dir {
            let _ = crate::wrap::sync_to_disk(pd, workspace, &registry_mut);
        }

        // ── 5. Stop or wait ──────────────────────────────────────
        if !wait {
            eprintln!();
            eprintln!(
                "Merged {} PR(s). {} remaining PR(s) rebased and pushed.",
                merged_count,
                remaining.len()
            );
            eprintln!("Run 'jj stack merge' again after CI passes.");
            return Ok(0);
        }

        // --wait: poll CI on the next PR before continuing.
        eprintln!();
        eprintln!(
            "Waiting for CI on #{} ({})...",
            next_pr, next_bookmark
        );
        let ci_config = PollConfig::ci_wait();
        match poll_readiness(ctx.platform.as_ref(), next_pr, next_bookmark, &ci_config).await {
            Ok(ReadinessOutcome::Ready) => {
                eprintln!(
                    "  ✓ #{} ({}) ready — continuing",
                    next_pr, next_bookmark
                );
                // Continue to next iteration.
            }
            Ok(ReadinessOutcome::Transient) => {
                // Shouldn't happen with unlimited attempts, but proceed anyway.
                eprintln!(
                    "  #{} ({}) still transient, attempting merge anyway",
                    next_pr, next_bookmark
                );
            }
            Ok(ReadinessOutcome::Blocked(reasons)) => {
                let reason_str = reasons.join(", ");
                let msg = format!(
                    "#{} ({}) blocked after CI wait: {}",
                    next_pr, next_bookmark, reason_str
                );
                eprintln!(
                    "  ✗ {}",
                    format_pr_error(&pr_cache, next_bookmark, next_pr, &msg)
                );
                eprintln!();
                eprintln!(
                    "Merged {} PR(s). Remaining PR(s) blocked.",
                    merged_count
                );
                return Ok(1);
            }
            Err(e) => {
                eprintln!(
                    "Warning: CI poll failed for #{} ({}): {}",
                    next_pr, next_bookmark, e
                );
                // Continue anyway — the merge attempt is the definitive test.
            }
        }
    }

    eprintln!();
    if merged_count > 0 {
        eprintln!("Merged {} PR(s). All done.", merged_count);
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
        (Some("gitea"), Some("test")) => {
            rt.block_on(async {
                eprintln!("Testing Gitea authentication...");
                match crate::auth::get_gitea_auth(None).await {
                    Ok(auth) => {
                        eprintln!("  Token source: {:?}", auth.source);
                        eprintln!("  Host: {}", auth.host);
                        match crate::auth::test_gitea_auth(&auth).await {
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
        (Some("gitea"), Some("setup")) => {
            eprintln!("Gitea Authentication Setup");
            eprintln!();
            eprintln!("Personal Access Token:");
            eprintln!("  Create a token at: https://<your-host>/user/settings/applications");
            eprintln!("  Required permissions: repo (read/write)");
            eprintln!("  Set:  export GITEA_TOKEN=<your-token>");
            eprintln!();
            eprintln!("Host configuration:");
            eprintln!("  export GITEA_HOST=gitea.example.com");
            eprintln!();
            eprintln!("Token resolution: GITEA_TOKEN env var");
            Ok(0)
        }
        (Some(p), _) if p == "github" || p == "gitlab" || p == "gitea" => {
            eprintln!("Usage: jj stack auth <github|gitlab|gitea> <test|setup>");
            Ok(1)
        }
        _ => {
            print_auth_help();
            Ok(1)
        }
    }
}

// ---------------------------------------------------------------------------
// Help text
// ---------------------------------------------------------------------------

/// Display all registered stacks across the repo (multi-stack global view).
///
/// This is the `jj stack --all` entry point. Uses `build_multi_stack` to
/// discover all registered plan bookmarks, groups them, and renders a
/// multi-column visualization.
fn show_all_stacks(plan_dir: &PlanDir, workspace: &Workspace, registry: &PlanRegistry, format: StackFormat) {
    use crate::pr_cache::load_pr_cache;
    use crate::stack_render::{self, RenderOptions};

    let multi = build_multi_stack(workspace, registry);
    if multi.stacks.is_empty() {
        eprintln!("No registered plans.");
        eprintln!("Create one with: jj plan new <bookmark-name>");
        return;
    }

    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();
    let pr_cache = load_pr_cache(&repo_root).ok();
    let columns = stack_render::build_columns(&multi, registry, workspace, pr_cache.as_ref(), plan_dir.dir_name());

    let num_stacks = multi.stacks.len();
    eprintln!();
    eprintln!("Plan stacks ({}/; {} stack{}):",
        plan_dir.dir_name(),
        num_stacks,
        if num_stacks == 1 { "" } else { "s" },
    );

    stack_render::render_to_stderr(&columns, &RenderOptions {
        format,
        show_paths: true,
    });
}

fn print_stack_help() {
    eprintln!("jj stack — stack-oriented PR operations");
    eprintln!();
    eprintln!("Usage: jj stack [SUBCOMMAND] [OPTIONS]");
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
    eprintln!("  --all               Show all stacks across the repo");
    eprintln!("  --format=FORMAT     Output format: 'compact' (default) or 'regular'");
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
    eprintln!("  --dry-run               Preview what would be done without making changes");
    eprintln!("  --draft                 Create new PRs as drafts");
    eprintln!("  --publish               Convert existing draft PRs to ready-for-review");
    eprintln!("  --update-descriptions   Push current plan content to existing PR titles/bodies");
    eprintln!("  --no-comments           Skip adding/updating stack navigation comments");
    eprintln!("  --continue-on-error     Don't abort on first failure (default: abort)");
    eprintln!("  --allow-gaps            Allow unbookmarked changes between bookmarks");
    eprintln!("  --remote <remote>       Specify the remote to push to (default: origin)");
    eprintln!("  --help, -h              Show this help message");
    eprintln!();
    eprintln!("Notes:");
    eprintln!("  --draft and --publish are mutually exclusive.");
    eprintln!("  Stack comments are added by default for multi-PR stacks.");
    eprintln!("  Execution aborts on first failure by default (stacked PRs are dependent).");
}

fn print_sync_help() {
    eprintln!("jj stack sync — fetch from remote and re-submit the stack");
    eprintln!();
    eprintln!("Usage: jj stack sync [options]");
    eprintln!();
    eprintln!("Fetches from the remote, then pushes bookmarks and updates PRs.");
    eprintln!("Equivalent to fetch + submit with default flags.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --dry-run               Preview what would be done without making changes");
    eprintln!("  --remote <remote>       Specify the remote (default: origin)");
    eprintln!("  --help, -h              Show this help message");
}

fn print_merge_help() {
    eprintln!("jj stack merge — merge approved PRs from the bottom of the stack");
    eprintln!();
    eprintln!("Usage: jj stack merge [options]");
    eprintln!();
    eprintln!("Merges the first ready PR, then rebases and pushes the remaining");
    eprintln!("stack onto updated trunk. Re-run after CI passes to merge the next.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --dry-run           Preview the merge plan without merging");
    eprintln!("  --wait              After merge+rebase, poll CI and continue merging");
    eprintln!("  --remote <remote>   Specify the remote (default: origin)");
    eprintln!("  --help, -h          Show this help message");
}

fn print_auth_help() {
    eprintln!("jj stack auth — authentication management");
    eprintln!();
    eprintln!("Usage: jj stack auth <platform> <action>");
    eprintln!();
    eprintln!("Platforms: github, gitlab, gitea");
    eprintln!("Actions:   test, setup");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  jj stack auth github test    Test GitHub authentication");
    eprintln!("  jj stack auth github setup   Show GitHub setup instructions");
    eprintln!("  jj stack auth gitlab test    Test GitLab authentication");
    eprintln!("  jj stack auth gitlab setup   Show GitLab setup instructions");
    eprintln!("  jj stack auth gitea test     Test Gitea authentication");
    eprintln!("  jj stack auth gitea setup    Show Gitea setup instructions");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::PlanDocument;

    /// Helper to create a Vec<String> from string slices.
    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    // -- parse_stack_dispatch_args tests -------------------------------------

    #[test]
    fn parse_bare_stack() {
        let a = args(&["stack"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert!(parsed.subcommand.is_none());
        assert!(!parsed.show_help);
        assert!(!parsed.show_all);
        assert!(parsed.format_override.is_none());
    }

    #[test]
    fn parse_stack_help_long() {
        let a = args(&["stack", "--help"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert!(parsed.show_help);
    }

    #[test]
    fn parse_stack_help_short() {
        let a = args(&["stack", "-h"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert!(parsed.show_help);
    }

    #[test]
    fn parse_stack_all() {
        let a = args(&["stack", "--all"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert!(parsed.show_all);
        assert!(parsed.subcommand.is_none());
    }

    #[test]
    fn parse_stack_format_equals() {
        let a = args(&["stack", "--format=regular"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert_eq!(parsed.format_override, Some("regular"));
        assert!(parsed.subcommand.is_none());
    }

    #[test]
    fn parse_stack_format_separate() {
        let a = args(&["stack", "--format", "regular"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert_eq!(parsed.format_override, Some("regular"));
        assert!(parsed.subcommand.is_none());
    }

    #[test]
    fn parse_stack_all_and_format() {
        let a = args(&["stack", "--all", "--format=compact"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert!(parsed.show_all);
        assert_eq!(parsed.format_override, Some("compact"));
    }

    #[test]
    fn parse_stack_submit_with_flag() {
        let a = args(&["stack", "submit", "--dry-run"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert_eq!(parsed.subcommand, Some("submit"));
        assert!(!parsed.show_help);
        assert!(!parsed.show_all);
    }

    #[test]
    fn parse_stack_format_before_subcommand() {
        let a = args(&["stack", "--format=regular", "submit"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert_eq!(parsed.subcommand, Some("submit"));
        assert_eq!(parsed.format_override, Some("regular"));
    }

    #[test]
    fn parse_stack_format_separate_before_subcommand() {
        let a = args(&["stack", "--format", "regular", "submit"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert_eq!(parsed.subcommand, Some("submit"));
        assert_eq!(parsed.format_override, Some("regular"));
    }

    #[test]
    fn parse_stack_all_format_reversed_order() {
        let a = args(&["stack", "--format=regular", "--all"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert!(parsed.show_all);
        assert_eq!(parsed.format_override, Some("regular"));
    }

    #[test]
    fn parse_stack_unknown_flags_ignored() {
        let a = args(&["stack", "--dry-run", "submit"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert_eq!(parsed.subcommand, Some("submit"));
        assert!(!parsed.show_help);
        assert!(!parsed.show_all);
        assert!(parsed.format_override.is_none());
    }

    #[test]
    fn parse_stack_format_without_value() {
        // --format at end without a value — format_override stays None
        let a = args(&["stack", "--format"]);
        let parsed = parse_stack_dispatch_args(&a);
        assert!(parsed.format_override.is_none());
    }

    // -- merge tests ---------------------------------------------------------

    #[test]
    fn merge_flags_parsed_independently() {
        let with_wait = args(&["stack", "merge", "--wait", "--dry-run"]);
        assert!(has_flag(&with_wait, "--wait"));
        assert!(has_flag(&with_wait, "--dry-run"));

        let without_wait = args(&["stack", "merge", "--dry-run"]);
        assert!(!has_flag(&without_wait, "--wait"));
    }

    #[test]
    fn format_pr_error_includes_url_from_cache() {
        use crate::pr_cache::PrCache;
        use crate::types::PullRequest;

        let mut cache = PrCache::new();
        let pr = PullRequest {
            number: 42,
            title: "feat".to_string(),
            head_ref: "feat-a".to_string(),
            base_ref: "main".to_string(),
            html_url: "https://github.com/org/repo/pull/42".to_string(),
            node_id: None,
            is_draft: false,
        };
        cache.upsert("feat-a", &pr, "origin");

        let msg = format_pr_error(&cache, "feat-a", 42, "merge failed");
        assert!(msg.contains("merge failed"), "original message preserved");
        assert!(
            msg.contains("https://github.com/org/repo/pull/42"),
            "URL appended from cache"
        );
    }

    #[test]
    fn format_pr_error_falls_back_to_pr_number() {
        let cache = crate::pr_cache::PrCache::new();
        let msg = format_pr_error(&cache, "unknown-bookmark", 99, "something broke");
        assert!(msg.contains("something broke"));
        assert!(msg.contains("PR #99"), "falls back to PR number when no cache entry");
    }

    #[test]
    fn test_pr_parts_strips_metadata() {
        let content = "feat: my feature\n\n> [!plan]\n> status: 🔴\n> issue: MERC-123\n\n# Background\n\nSome details.\n";
        let (title, body) = PlanDocument::parse(content).pr_parts().unwrap();
        assert_eq!(title, "feat: my feature");
        assert!(!body.contains("> [!plan]"), "callout opener should not appear in PR body");
        assert!(!body.contains("> status:"), "metadata fields should not appear in PR body");
        assert!(!body.contains("> issue:"), "metadata fields should not appear in PR body");
        assert!(body.contains("# Background"), "body content should be preserved");
    }

    #[test]
    fn test_pr_parts_strips_scratch() {
        let content = "feat: my feature\n\n# Background\n\nSome details.\n\n# Notes [scratch]\n\nPrivate notes.\n\n# Results\n\nFinal results.\n";
        let (title, body) = PlanDocument::parse(content).pr_parts().unwrap();
        assert_eq!(title, "feat: my feature");
        assert!(!body.contains("[scratch]"), "scratch sections should be stripped");
        assert!(!body.contains("Private notes"), "scratch content should be stripped");
        assert!(body.contains("# Results"), "non-scratch content should be preserved");
    }

    #[test]
    fn test_pr_parts_preserves_linear_magic_words() {
        let content = "feat: my feature\n\n> [!plan]\n> status: 🔴\n\nCompletes MERC-123\n\n# Details\n\nSome work.\n";
        let (title, body) = PlanDocument::parse(content).pr_parts().unwrap();
        assert_eq!(title, "feat: my feature");
        assert!(body.contains("Completes MERC-123"), "Linear magic words must survive to PR body");
    }

    #[test]
    fn test_pr_parts_title_is_line_1() {
        let content = "feat: actual title\n\n> [!plan]\n> status: 🔴\n\nBody text.\n";
        let (title, _body) = PlanDocument::parse(content).pr_parts().unwrap();
        assert_eq!(title, "feat: actual title", "title should be line 1");
    }

    #[test]
    fn test_pr_parts_empty_content() {
        assert!(PlanDocument::parse("").pr_parts().is_none());
        assert!(PlanDocument::parse("   \n\nbody").pr_parts().is_none());
    }

    #[test]
    fn test_pr_parts_metadata_and_scratch_combined() {
        let content = "feat: combined test\n\n> [!plan]\n> status: 🔴\n> issue: MERC-456\n\n# Background\n\nVisible content.\n\n# Research [scratch]\n\nHidden research.\n\n# Implementation\n\nVisible impl.\n";
        let (title, body) = PlanDocument::parse(content).pr_parts().unwrap();
        assert_eq!(title, "feat: combined test");
        assert!(!body.contains("> [!plan]"), "no callout opener in body");
        assert!(!body.contains("> status:"), "no front matter in body");
        assert!(!body.contains("> issue:"), "no front matter in body");
        assert!(!body.contains("[scratch]"), "no scratch in body");
        assert!(!body.contains("Hidden research"), "scratch content removed");
        assert!(body.contains("Visible content."), "non-scratch body preserved");
        assert!(body.contains("Visible impl."), "non-scratch body preserved");
    }
}