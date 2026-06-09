use crate::jj_binary::JjBinary;
use crate::markdown::{PlanDocument, StrippedSection};
use crate::plan_dir::PlanDir;
use crate::stack_render::StackFormat;
use crate::types::PlanRegistry;
use crate::workspace::Workspace;
use crate::wrap::SyncChangeView;

/// Verbosity mode for the "stripped scratch sections" report printed by
/// `jj plan done` and `jj plan done --dry-run`.
///
/// The user-facing CLI value is spelled `none`; `parse_show_stripped` maps it
/// to `ShowStripped::Off` to avoid `Option::None` collisions at use sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowStripped {
    /// Print the top-level scratch heading, its descendant headings, and the
    /// verbatim body of each stripped section.
    Full,
    /// Print the top-level scratch heading with descendant headings indented
    /// beneath it (a "table of contents" of the strip). Default.
    Toc,
    /// Print only the top-level scratch headings; descendants are omitted.
    Headings,
    /// Print nothing about the strip.
    Off,
}

impl ShowStripped {
    pub const DEFAULT: Self = Self::Toc;
}

/// Parse the `--show-stripped=<mode>` flag value.
///
/// Recognizes `full`, `toc`, `headings`, `none`. Maps `none` to `Off` at the
/// boundary so internal sites use the unambiguous variant name.
pub fn parse_show_stripped(s: &str) -> Result<ShowStripped, String> {
    match s {
        "full" => Ok(ShowStripped::Full),
        "toc" => Ok(ShowStripped::Toc),
        "headings" => Ok(ShowStripped::Headings),
        "none" => Ok(ShowStripped::Off),
        other => Err(format!(
            "invalid --show-stripped value '{}': expected one of full, toc, headings, none",
            other
        )),
    }
}

/// Run `jj plan done` — mark one or all plans as done.
///
/// Strips `[scratch]` sections from descriptions (unless `--keep-scratch`)
/// and sets front matter `status: ✅` in the description.
///
/// ## Flags
///
/// - `--stack`: mark all changes in the stack as done
/// - `--keep-scratch`: don't strip `[scratch]` sections
/// - `--dry-run`: show what would be changed without modifying anything
/// - `--show-stripped=<mode>`: control the stripped-section report
///   (`full | toc | headings | none`, default `toc`)
/// - Positional arg: a specific CHANGE_ID to mark done (defaults to `@`)
pub fn run_done(jj: &JjBinary, plan_dir: &PlanDir, args: &[String], workspace: &mut Workspace, registry: &PlanRegistry, format: StackFormat) -> crate::error::Result<i32> {
    // ------------------------------------------------------------------
    // 1. Parse args
    // ------------------------------------------------------------------
    let mut do_stack = false;
    let mut keep_scratch = false;
    let mut dry_run = false;
    let mut show_stripped = ShowStripped::DEFAULT;
    let mut target_id: Option<String> = None;

    for arg in args {
        match arg.as_str() {
            "--stack" => do_stack = true,
            "--keep-scratch" => keep_scratch = true,
            "--dry-run" => dry_run = true,
            s if s.starts_with("--show-stripped=") => {
                let value = &s["--show-stripped=".len()..];
                match parse_show_stripped(value) {
                    Ok(mode) => show_stripped = mode,
                    Err(msg) => {
                        eprintln!("jj plan done: {}", msg);
                        return Ok(2);
                    }
                }
            }
            _ => target_id = Some(arg.clone()),
        }
    }

    // ------------------------------------------------------------------
    // 2. Flush local plan edits to jj descriptions, then anchor the baseline
    //    to the converged state. Without the anchor, the `done` stamp below
    //    would be judged against a stale baseline and reverted (the file would
    //    appear "changed" and win); anchored, it lands as a desc-side change
    //    (DescToFile) and sync writes it to the file.
    // ------------------------------------------------------------------
    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();
    let prev_baselines = crate::sync_state::load_sync_state(&repo_root)
        .map(|s| s.baselines)
        .unwrap_or_default();
    crate::flush::flush_and_anchor(&plan_dir.path, jj, workspace, registry, &prev_baselines);

    // ------------------------------------------------------------------
    // 3. Resolve stack (flush_and_anchor already reloaded the workspace)
    // ------------------------------------------------------------------
    let changes = build_sync_views_for_done(workspace, registry);

    // ------------------------------------------------------------------
    // 4. Dispatch: --stack or single plan
    // ------------------------------------------------------------------
    if do_stack {
        run_done_stack(jj, plan_dir, changes.as_deref(), keep_scratch, dry_run, show_stripped, workspace, registry, format)
    } else {
        run_done_single(jj, plan_dir, changes.as_deref(), target_id, keep_scratch, dry_run, show_stripped, workspace, registry, format)
    }
}

// ---------------------------------------------------------------------------
// --stack flow
// ---------------------------------------------------------------------------

/// Mark every change in the stack as done.
#[allow(clippy::too_many_arguments)]
fn run_done_stack(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    changes: Option<&[SyncChangeView]>,
    keep_scratch: bool,
    dry_run: bool,
    show_stripped: ShowStripped,
    workspace: &mut Workspace,
    registry: &PlanRegistry,
    format: StackFormat,
) -> crate::error::Result<i32> {
    let changes = match changes {
        Some(c) => c,
        None => {
            eprintln!("jj plan done --stack: could not resolve stack changes");
            return Ok(1);
        }
    };

    // Accumulate reports and print them together after all describes complete,
    // so multi-change output isn't interleaved with the per-describe round-trips.
    let mut reports: Vec<(String, String, String)> = Vec::new(); // (bookmark, change_id, report_text)

    for change in changes {
        let desc = &change.description;
        let doc = PlanDocument::parse(desc);

        if dry_run {
            print_dry_run_diff(&doc, keep_scratch, show_stripped, &change.change_id);
            continue;
        }

        let (final_desc, sections) = doc.as_done_with_report(keep_scratch);
        let _ = jj.run_silent(&["describe", "-r", &change.change_id, "-m", &final_desc]);

        if !keep_scratch
            && let Some(report) = format_strip_report(
                &sections,
                show_stripped,
                doc.body(),
                &change.change_id,
            )
        {
            reports.push((change.bookmark_name.clone(), change.change_id.clone(), report));
        }
    }

    if dry_run {
        return Ok(0);
    }

    for (bookmark, change_id, report) in &reports {
        eprintln!("{}", stack_change_separator(bookmark, change_id));
        eprint!("{}", report);
    }

    // Sync plan files immediately after describes so the plan files reflect
    // the new front matter. Without this, any subsequent flush cycle would
    // read stale plan files and overwrite the jj descriptions.
    workspace.reload();
    crate::wrap::full_sync_and_show(plan_dir, workspace, registry, format);

    // --stack marks everything done, suggest starting a new stack
    eprintln!();
    eprintln!("All plans in stack are done 🎉");
    eprintln!("Start a new plan: jj plan new <bookmark-name>");

    Ok(0)
}

// ---------------------------------------------------------------------------
// Single plan flow (default)
// ---------------------------------------------------------------------------

/// Mark a single plan as done.
#[allow(clippy::too_many_arguments)]
fn run_done_single(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    changes: Option<&[SyncChangeView]>,
    target_id: Option<String>,
    keep_scratch: bool,
    dry_run: bool,
    show_stripped: ShowStripped,
    workspace: &mut Workspace,
    registry: &PlanRegistry,
    format: StackFormat,
) -> crate::error::Result<i32> {
    let target = target_id.clone().unwrap_or_else(|| "@".to_string());

    // Try to find the change in the resolved stack
    let found = changes.and_then(|cs| find_change_in_stack(cs, &target));

    // Read description: from stack if found, otherwise from jj directly
    let (change_id_for_describe, desc) = match found {
        Some(change) => (
            change.change_id.clone(),
            change.description.clone(),
        ),
        None => {
            // Not found in stack — read description from jj directly
            let desc = match read_description(workspace, &target) {
                Some(d) => d,
                None => {
                    eprintln!("jj plan done: could not read description for '{}'", target);
                    return Ok(1);
                }
            };
            (target.clone(), desc)
        }
    };

    let doc = PlanDocument::parse(&desc);

    // Dry run: show what would be stripped and exit
    if dry_run {
        print_dry_run_diff(&doc, keep_scratch, show_stripped, &change_id_for_describe);
        return Ok(0);
    }

    let (final_desc, sections) = doc.as_done_with_report(keep_scratch);
    let _ = jj.run_silent(&[
        "describe",
        "-r",
        &change_id_for_describe,
        "-m",
        &final_desc,
    ]);

    if !keep_scratch
        && let Some(report) = format_strip_report(
            &sections,
            show_stripped,
            doc.body(),
            &change_id_for_describe,
        )
    {
        eprint!("{}", report);
    }

    // Sync plan files immediately after describe so the plan file reflects
    // the new front matter. Without this, a subsequent `jj edit` (via the
    // shell shim's wrap → flush_all) would read the stale plan file and
    // overwrite the jj description, losing the front matter we just set.
    workspace.reload();
    crate::wrap::cleanup_stale_and_migrate(plan_dir, workspace, registry);
    let gathered = crate::wrap::sync_to_disk(plan_dir, workspace, registry);

    // Show the updated stack.
    crate::wrap::show_plan_stack(plan_dir, gathered.as_ref(), format);
    Ok(0)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find a change in the stack, either by working copy marker (for "@") or by
/// change ID prefix match.
fn find_change_in_stack<'a>(changes: &'a [SyncChangeView], target: &str) -> Option<&'a SyncChangeView> {
    if target == "@" {
        changes.iter().find(|c| c.is_working_copy)
    } else {
        // Prefix match: the target might be a prefix of the change_id or vice versa
        changes
            .iter()
            .find(|c| c.change_id.starts_with(target) || target.starts_with(&c.change_id))
    }
}

/// Read a change's description via jj-lib.
fn read_description(workspace: &Workspace, target: &str) -> Option<String> {
    workspace.read_description_at(target)
}

/// Build SyncChangeView list from the stack for done's consumption.
///
/// Delegates to the shared `wrap::build_sync_views()` so there is a single
/// place to maintain the `StackResult` → `Vec<SyncChangeView>` conversion.
fn build_sync_views_for_done(workspace: &Workspace, registry: &PlanRegistry) -> Option<Vec<SyncChangeView>> {
    crate::wrap::build_sync_views(workspace, registry)
}

/// Render a stripped-section report for stderr.
///
/// Returns `None` when the mode is `Off` or there is nothing to report (no
/// scratch sections were stripped). Otherwise returns the full multi-line
/// string ready to `eprintln!`.
///
/// Pure function over its inputs: `input` is the original (pre-strip) document
/// text that the renderer slices from in `Full` mode.
pub fn format_strip_report(
    sections: &[StrippedSection],
    mode: ShowStripped,
    input: &str,
    change_id: &str,
) -> Option<String> {
    if matches!(mode, ShowStripped::Off) || sections.is_empty() {
        return None;
    }

    let mut out = String::new();
    out.push_str("Stripped scratch sections:\n");

    for (i, section) in sections.iter().enumerate() {
        // Heading line, e.g. "  ## Notes [scratch]"
        out.push_str("  ");
        for _ in 0..section.heading.level {
            out.push('#');
        }
        out.push(' ');
        out.push_str(&section.heading.text);
        out.push('\n');

        if matches!(mode, ShowStripped::Toc | ShowStripped::Full) {
            // Descendant headings, indented one level beyond their nesting depth
            for desc in &section.descendant_headings {
                let indent = (desc.level as usize).saturating_sub(section.heading.level as usize);
                for _ in 0..(indent + 1) {
                    out.push_str("  ");
                }
                for _ in 0..desc.level {
                    out.push('#');
                }
                out.push(' ');
                out.push_str(&desc.text);
                out.push('\n');
            }
        }

        if matches!(mode, ShowStripped::Full) {
            // The byte range starts at the scratch heading itself, but we
            // already printed an indented version of that heading above —
            // skip past the first line so it isn't shown twice.
            let slice = &input[section.range.clone()];
            let body_start = slice.find('\n').map(|n| n + 1).unwrap_or(slice.len());
            let body = &slice[body_start..];
            if !body.is_empty() {
                out.push('\n');
                out.push_str(body);
                if !body.ends_with('\n') {
                    out.push('\n');
                }
            }
            if i + 1 < sections.len() {
                out.push_str("─────\n");
            }
        }
    }

    out.push_str(&format!("Recover with: jj evolog -r {}\n", change_id));
    Some(out)
}

/// Per-change separator for `--stack` mode, e.g. `--- feat-auth (kpqxywon) ---`.
///
/// Matches the existing `--- change ---` style from `print_dry_run_diff` but
/// interpolates the bookmark and change ID so multi-change output stays readable.
fn stack_change_separator(bookmark: &str, change_id: &str) -> String {
    if bookmark.is_empty() {
        format!("--- {} ---", change_id)
    } else {
        format!("--- {} ({}) ---", bookmark, change_id)
    }
}

/// Print a dry-run preview for a single change.
///
/// Renders the same structured strip report as the live-run path (via
/// `format_strip_report`) and then prints the status-side message
/// ("Would set metadata: status: ✅" / "Already marked done"). Honors
/// `--show-stripped=none` (silent on the strip side) without clamping — the
/// status side still prints.
fn print_dry_run_diff(
    doc: &PlanDocument,
    keep_scratch: bool,
    show_stripped: ShowStripped,
    change_id: &str,
) {
    let (_proposed, sections) = doc.as_done_with_report(keep_scratch);

    eprintln!("--- change ---");

    if !keep_scratch
        && let Some(report) = format_strip_report(&sections, show_stripped, doc.body(), change_id)
    {
        eprint!("{}", report);
        eprintln!();
    }

    if doc.is_done() {
        eprintln!("Already marked done (status: ✅)");
    } else {
        eprintln!("Would set metadata: status: ✅");
    }
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::PlanDocument;

    // ── ShowStripped parsing ─────────────────────────────────────────

    #[test]
    fn parse_show_stripped_valid() {
        assert_eq!(parse_show_stripped("full").unwrap(), ShowStripped::Full);
        assert_eq!(parse_show_stripped("toc").unwrap(), ShowStripped::Toc);
        assert_eq!(parse_show_stripped("headings").unwrap(), ShowStripped::Headings);
        assert_eq!(parse_show_stripped("none").unwrap(), ShowStripped::Off);
    }

    #[test]
    fn parse_show_stripped_invalid_lists_valid() {
        let err = parse_show_stripped("verbose").unwrap_err();
        for valid in ["full", "toc", "headings", "none"] {
            assert!(err.contains(valid), "error message should list '{}': {}", valid, err);
        }
    }

    // ── format_strip_report ──────────────────────────────────────────

    /// Build a `StrippedSection` by parsing `input` and pulling its first scratch section.
    fn first_scratch_section(input: &str) -> (Vec<StrippedSection>, String) {
        let doc = PlanDocument::parse(input);
        let (_stripped, report) = doc.as_done_with_report(false);
        // Report ranges index into the body (scratch lives there), not raw.
        (report, doc.body().to_string())
    }

    #[test]
    fn format_strip_report_off_returns_none() {
        let (sections, raw) = first_scratch_section("# T\n\n## N [scratch]\n\nbody\n");
        assert!(format_strip_report(&sections, ShowStripped::Off, &raw, "kpqxywon").is_none());
    }

    #[test]
    fn format_strip_report_empty_sections_returns_none() {
        let sections: Vec<StrippedSection> = Vec::new();
        for mode in [ShowStripped::Full, ShowStripped::Toc, ShowStripped::Headings] {
            assert!(format_strip_report(&sections, mode, "", "abc").is_none());
        }
    }

    #[test]
    fn format_strip_report_headings_lists_top_level_only() {
        let (sections, raw) = first_scratch_section(
            "# T\n\n## Notes [scratch]\n\nbody\n\n### Sub\n\nnested\n",
        );
        let out = format_strip_report(&sections, ShowStripped::Headings, &raw, "abc").unwrap();
        assert!(out.contains("## Notes [scratch]"), "top-level scratch heading present");
        assert!(!out.contains("### Sub"), "descendant heading must NOT appear in headings mode");
    }

    #[test]
    fn format_strip_report_toc_indents_descendants() {
        let (sections, raw) = first_scratch_section(
            "# T\n\n## Notes [scratch]\n\nbody\n\n### Sub\n\nnested\n",
        );
        let out = format_strip_report(&sections, ShowStripped::Toc, &raw, "abc").unwrap();
        assert!(out.contains("## Notes [scratch]"), "top-level heading present");
        assert!(out.contains("### Sub"), "descendant heading present in toc mode");
        // The descendant line should be indented further than the parent
        let parent_indent = out.lines().find(|l| l.contains("## Notes")).unwrap().find('#').unwrap();
        let desc_indent = out.lines().find(|l| l.contains("### Sub")).unwrap().find('#').unwrap();
        assert!(desc_indent > parent_indent, "descendant should be indented further than parent");
    }

    #[test]
    fn format_strip_report_full_includes_body() {
        let (sections, raw) = first_scratch_section(
            "# T\n\n## Notes [scratch]\n\nverbatim learnings\n",
        );
        let out = format_strip_report(&sections, ShowStripped::Full, &raw, "abc").unwrap();
        assert!(out.contains("verbatim learnings"),
            "full mode should include the body slice verbatim, got:\n{}", out);
    }

    #[test]
    fn format_strip_report_full_separates_between_not_after_sections() {
        // The `─────` belongs between adjacent stripped sections, never trailing
        // the last one before the recovery hint.
        let input = "# T\n\n## A [scratch]\n\nbody a\n\n## Keep\n\nmid\n\n## B [scratch]\n\nbody b\n";
        let (sections, raw) = first_scratch_section(input);
        assert_eq!(sections.len(), 2, "test setup: expected two scratch sections");
        let out = format_strip_report(&sections, ShowStripped::Full, &raw, "abc").unwrap();
        assert_eq!(out.matches("─────").count(), 1,
            "Full mode should place exactly one ───── between two sections, got:\n{}", out);
        // And it should appear before the recovery hint, not after.
        let sep_pos = out.find("─────").unwrap();
        let hint_pos = out.find("Recover with:").unwrap();
        assert!(sep_pos < hint_pos, "───── must precede recovery hint");
    }

    #[test]
    fn format_strip_report_recovery_hint_includes_change_id() {
        let (sections, raw) = first_scratch_section("# T\n\n## N [scratch]\n\nb\n");
        for mode in [ShowStripped::Full, ShowStripped::Toc, ShowStripped::Headings] {
            let out = format_strip_report(&sections, mode, &raw, "kpqxywon").unwrap();
            assert!(out.contains("jj evolog -r kpqxywon"),
                "{:?} mode should include recovery hint with change id", mode);
        }
    }

    #[test]
    fn stack_change_separator_with_bookmark() {
        assert_eq!(stack_change_separator("feat-auth", "kpqxywon"),
            "--- feat-auth (kpqxywon) ---");
    }

    #[test]
    fn stack_change_separator_without_bookmark() {
        assert_eq!(stack_change_separator("", "kpqxywon"), "--- kpqxywon ---");
    }

    #[test]
    fn test_as_done_sets_status() {
        let desc = "feat: title\n\n> [!plan]\n> status: 🔴\n\nbody text here";
        let doc = PlanDocument::parse(desc);
        let result = doc.as_done(false);
        assert!(result.contains("> status: ✅"), "status should be set to ✅");
        assert!(!result.contains("> status: 🔴"), "old status should be replaced");
        assert!(result.contains("body text here"), "body text preserved");
        assert!(result.starts_with("feat: title\n"), "title preserved as line 1");
    }

    #[test]
    fn test_as_done_already_done() {
        let desc = "feat: add something\n\n> [!plan]\n> status: ✅\n\nbody";
        let doc = PlanDocument::parse(desc);
        let result = doc.as_done(false);
        assert!(result.contains("> status: ✅"));
        assert_eq!(result.matches("status:").count(), 1, "no duplicate status");
    }

    #[test]
    fn test_as_done_body_text_no_false_positive() {
        // Body text contains literal "plan-status: ✅" — must NOT trigger false positive.
        let desc = "feat: title\n\n> [!plan]\n> status: 🔴\n\nThis test has plan-status: ✅ in body text";
        let doc = PlanDocument::parse(desc);
        let result = doc.as_done(false);
        assert!(result.contains("> status: ✅"), "metadata status should be ✅");
        assert!(result.contains("plan-status: ✅ in body text"), "body text preserved");
    }

    #[test]
    fn test_as_done_creates_metadata() {
        // No existing metadata → creates callout block after title
        let desc = "feat: add something\n\n# Background\n\nSome details.";
        let doc = PlanDocument::parse(desc);
        let result = doc.as_done(false);
        assert!(result.starts_with("feat: add something\n"), "title preserved as line 1");
        assert!(result.contains("> status: ✅"), "should set status to ✅");
        assert!(result.contains("> [!plan]"), "should have callout block");
        assert!(result.contains("# Background"), "body preserved");
    }

    #[test]
    fn test_as_done_preserves_other_metadata_fields() {
        let desc = "feat: title\n\n> [!plan]\n> status: 🔴\n> issue: MERC-123\n\n# Phase 1\n\nDone.";
        let doc = PlanDocument::parse(desc);
        let result = doc.as_done(false);
        assert!(result.contains("> status: ✅"), "status should be ✅");
        assert!(result.contains("> issue: MERC-123"), "other fields preserved");
        assert!(result.contains("# Phase 1"), "body content preserved");
    }

    #[test]
    fn test_as_done_no_duplicate_status() {
        let desc = "feat: something\n\n> [!plan]\n> status: 🔴\n\nbody";
        let doc = PlanDocument::parse(desc);
        let result = doc.as_done(false);
        assert_eq!(result.matches("status:").count(), 1,
            "should have exactly one status field, got: {:?}", result);
    }
}