use std::path::{Path, PathBuf};

use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::plan_file;
use crate::stack_render::StackFormat;
use crate::types::PlanRegistry;
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Intercept `jj describe` (and `jj desc`) with plan-aware guard logic.
///
/// When `-m`/`--message` or `--stdin` targets a tracked plan, the command
/// is **blocked** with an educational error — unless `--override-plan-protocol`
/// is present. This prevents LLMs (and humans) from accidentally replacing
/// a rich plan document with a one-liner.
///
/// Editor-mode describe (no `-m`/`--stdin`) is always passed through to
/// `wrap::wrap()` unguarded — the user can see the full content and make
/// informed edits.
///
/// `args` is the full original argument list starting with "describe" or "desc",
/// e.g. `["describe", "-m", "new content"]` or
/// `["desc", "-r", "abc", "-m", "content"]`.
pub fn handle_describe(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    args: &[String],
    workspace: &mut Workspace,
    registry: &PlanRegistry,
    format: StackFormat,
) -> crate::error::Result<i32> {
    // ── GATHER ──────────────────────────────────────────────────────────
    let parsed = parse_describe_args(args);

    // Resolve target revision → plan bookmark → plan file entry.
    let target = parsed.revision.as_deref().unwrap_or("@");
    let bookmark_name = super::resolve_plan_bookmark_at(workspace, registry, target);

    let plan_file_entry = bookmark_name.as_ref().and_then(|bm_name| {
        let plan_files = plan_file::collect_plan_files(&plan_dir.path, registry);
        plan_files.into_iter().find(|e| &e.bookmark_name == bm_name)
    });

    let plan_file_path = plan_file_entry.as_ref().map(|e| e.path.as_path());

    // ── PLAN ────────────────────────────────────────────────────────────
    let action = plan_describe_action(&parsed, plan_file_path);

    // ── EXECUTE ─────────────────────────────────────────────────────────
    match action {
        DescribeAction::EditorPassthrough => {
            crate::wrap::wrap(plan_dir, jj, args, workspace, registry, format)
        }

        DescribeAction::Allow => {
            // Non-plan target with -m. If there's a plan file for the target,
            // write the message to it (this path is for non-plan changes that
            // still happen to have a plan file, which shouldn't happen — but
            // Allow means plan_file_path was None, so this is just wrap).
            crate::wrap::wrap(plan_dir, jj, args, workspace, registry, format)
        }

        DescribeAction::AllowOverride { plan_file_path: pf_path } => {
            // User explicitly wants to replace the plan. Write message to
            // the plan file so flush picks it up, strip the override flag,
            // then delegate to wrap.
            let message = parsed.messages.join("\n");
            plan_file::write_or_warn(&pf_path, &message);

            let stripped: Vec<String> = strip_override_flag(args);
            crate::wrap::wrap(plan_dir, jj, &stripped, workspace, registry, format)
        }

        DescribeAction::Block { plan_file_path: display_path } => {
            let verb = if parsed.has_stdin { "--stdin" } else { "-m" };
            eprintln!(
                "jj-plan: blocked `jj describe {}` — this would replace the entire plan document.",
                verb,
            );
            eprintln!();
            eprintln!("The plan for this change is at: {}", display_path);
            eprintln!("Edit it directly instead of using `jj describe {}`.", verb);
            eprintln!();
            eprintln!("To replace the full description anyway, add --override-plan-protocol.");
            Ok(1)
        }
    }
}

// ---------------------------------------------------------------------------
// Decision types
// ---------------------------------------------------------------------------

/// The action `handle_describe` should take after gathering data.
#[derive(Debug, PartialEq)]
enum DescribeAction {
    /// Editor mode (no -m/--stdin). Pass through to wrap unchanged.
    EditorPassthrough,
    /// -m/--stdin targets a non-plan change. Delegate to wrap.
    Allow,
    /// -m/--stdin targets a tracked plan WITH `--override-plan-protocol`.
    /// Write message to plan file, strip flag, then wrap.
    AllowOverride { plan_file_path: PathBuf },
    /// -m/--stdin targets a tracked plan WITHOUT override. Block with error.
    Block { plan_file_path: String },
}

// ---------------------------------------------------------------------------
// Pure decision function
// ---------------------------------------------------------------------------

/// Collapse the entire describe guard decision tree into one testable unit.
///
/// - `parsed`: parsed argument state (messages, stdin, override flag).
/// - `plan_file_path`: `Some(path)` if the target resolves to a tracked plan
///   file; `None` if the target is not a tracked plan.
fn plan_describe_action(
    parsed: &ParsedDescribeArgs,
    plan_file_path: Option<&Path>,
) -> DescribeAction {
    let has_replacement = !parsed.messages.is_empty() || parsed.has_stdin;

    if !has_replacement {
        return DescribeAction::EditorPassthrough;
    }

    match plan_file_path {
        None => DescribeAction::Allow,
        Some(path) => {
            if parsed.has_override {
                DescribeAction::AllowOverride {
                    plan_file_path: path.to_path_buf(),
                }
            } else {
                // Build a relative display path: .jj-plan/NN-bookmark.md
                let display = path
                    .file_name()
                    .map(|f| format!(".jj-plan/{}", f.to_string_lossy()))
                    .unwrap_or_else(|| path.display().to_string());
                DescribeAction::Block {
                    plan_file_path: display,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Arg stripping
// ---------------------------------------------------------------------------

/// Remove `--override-plan-protocol` from the argument list so jj doesn't
/// choke on the unknown flag.
fn strip_override_flag(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|a| a.as_str() != "--override-plan-protocol")
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Arg parsing
// ---------------------------------------------------------------------------

/// Parsed result from scanning describe arguments.
struct ParsedDescribeArgs {
    /// All -m/--message values, in order.
    messages: Vec<String>,
    /// The -r/--revision value, if any (explicit flag or positional revset).
    revision: Option<String>,
    /// Whether `--stdin` was present.
    has_stdin: bool,
    /// Whether `--override-plan-protocol` was present.
    has_override: bool,
}

/// jj global options (long-form) that consume a following value argument.
/// Used to avoid misidentifying their values as positional revsets.
const GLOBAL_OPTIONS_WITH_VALUE: &[&str] = &[
    "--repository",
    "--at-operation",
    "--at-op",
    "--color",
    "--config",
    "--config-file",
    "-R",
];

/// Parse `jj describe` arguments to extract relevant flags and positional
/// revsets.
///
/// Handles all common forms of `-m`, `--message`, `-r`, `--revision`,
/// `--stdin`, `--override-plan-protocol`, and jj global options. Also
/// collects positional (non-flag) arguments, using the first one as the
/// revision target when no explicit `-r`/`--revision` is given.
fn parse_describe_args(args: &[String]) -> ParsedDescribeArgs {
    let mut messages = Vec::new();
    let mut revision = None;
    let mut has_stdin = false;
    let mut has_override = false;
    let mut positional_args: Vec<String> = Vec::new();

    // Skip index 0 which is "describe" or "desc"
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];

        // -- stops option parsing; remaining args are positional revsets
        if arg == "--" {
            // Collect everything after -- as positional args
            for j in (i + 1)..args.len() {
                positional_args.push(args[j].clone());
            }
            break;
        }

        // --override-plan-protocol
        if arg == "--override-plan-protocol" {
            has_override = true;
            i += 1;
            continue;
        }

        // --stdin
        if arg == "--stdin" {
            has_stdin = true;
            i += 1;
            continue;
        }

        // --message=VALUE
        if let Some(val) = arg.strip_prefix("--message=") {
            messages.push(val.to_string());
            i += 1;
            continue;
        }

        // --message VALUE
        if arg == "--message" {
            if i + 1 < args.len() {
                i += 1;
                messages.push(args[i].clone());
            }
            i += 1;
            continue;
        }

        // -m VALUE or -mVALUE
        if arg == "-m" {
            if i + 1 < args.len() {
                i += 1;
                messages.push(args[i].clone());
            }
            i += 1;
            continue;
        }
        if arg.starts_with("-m") && !arg.starts_with("--") {
            // -mVALUE: everything after "-m" is the value
            messages.push(arg[2..].to_string());
            i += 1;
            continue;
        }

        // --revision=VALUE
        if let Some(val) = arg.strip_prefix("--revision=") {
            revision = Some(val.to_string());
            i += 1;
            continue;
        }

        // --revision VALUE
        if arg == "--revision" {
            if i + 1 < args.len() {
                i += 1;
                revision = Some(args[i].clone());
            }
            i += 1;
            continue;
        }

        // -r VALUE or -rVALUE
        if arg == "-r" {
            if i + 1 < args.len() {
                i += 1;
                revision = Some(args[i].clone());
            }
            i += 1;
            continue;
        }
        if arg.starts_with("-r") && !arg.starts_with("--") {
            // -rVALUE: everything after "-r" is the value
            revision = Some(arg[2..].to_string());
            i += 1;
            continue;
        }

        // jj global options that take a value: skip the option AND its value
        // to avoid misidentifying the value as a positional revset.
        // Handle =VALUE joined forms first (single token, no skip needed).
        if arg.starts_with("--") {
            let is_global_with_eq = GLOBAL_OPTIONS_WITH_VALUE.iter().any(|opt| {
                opt.starts_with("--") && arg.starts_with(&format!("{}=", opt))
            });
            if is_global_with_eq {
                i += 1;
                continue;
            }

            let is_global_separate = GLOBAL_OPTIONS_WITH_VALUE.contains(&arg.as_str());
            if is_global_separate {
                // Skip the option and its value
                i += 2;
                continue;
            }
        }

        // -R VALUE (short form of --repository)
        if arg == "-R" {
            i += 2;
            continue;
        }
        if arg.starts_with("-R") && !arg.starts_with("--") {
            // -RVALUE: joined form, single token
            i += 1;
            continue;
        }

        // Any other flag (starts with -) — skip it (e.g. --no-edit, --editor,
        // --ignore-working-copy, --debug, --quiet, --no-pager, etc.)
        if arg.starts_with('-') {
            i += 1;
            continue;
        }

        // Non-flag argument: this is a positional revset
        positional_args.push(arg.clone());
        i += 1;
    }

    // Positional fallback: if no explicit -r/--revision, use the first
    // positional arg as the revision target.
    if revision.is_none() {
        revision = positional_args.into_iter().next();
    }

    ParsedDescribeArgs {
        messages,
        revision,
        has_stdin,
        has_override,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a Vec<String> from string slices.
    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    // -----------------------------------------------------------------------
    // parse_describe_args tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_no_message_flags() {
        let a = args(&["describe"]);
        let parsed = parse_describe_args(&a);
        assert!(parsed.messages.is_empty());
        assert!(parsed.revision.is_none());
        assert!(!parsed.has_stdin);
        assert!(!parsed.has_override);
    }

    #[test]
    fn test_parse_single_m_separate() {
        let a = args(&["describe", "-m", "hello world"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.messages, vec!["hello world"]);
        assert!(parsed.revision.is_none());
    }

    #[test]
    fn test_parse_single_m_joined() {
        let a = args(&["describe", "-mhello"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.messages, vec!["hello"]);
    }

    #[test]
    fn test_parse_message_long_separate() {
        let a = args(&["describe", "--message", "long form"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.messages, vec!["long form"]);
    }

    #[test]
    fn test_parse_message_long_equals() {
        let a = args(&["describe", "--message=equals form"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.messages, vec!["equals form"]);
    }

    #[test]
    fn test_parse_multiple_messages() {
        let a = args(&["describe", "-m", "first", "-m", "second", "--message=third"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.messages, vec!["first", "second", "third"]);
    }

    #[test]
    fn test_parse_revision_short_separate() {
        let a = args(&["describe", "-r", "abc", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("abc"));
        assert_eq!(parsed.messages, vec!["msg"]);
    }

    #[test]
    fn test_parse_revision_short_joined() {
        let a = args(&["describe", "-rabc", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("abc"));
        assert_eq!(parsed.messages, vec!["msg"]);
    }

    #[test]
    fn test_parse_revision_long_separate() {
        let a = args(&["describe", "--revision", "xyz", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("xyz"));
    }

    #[test]
    fn test_parse_revision_long_equals() {
        let a = args(&["describe", "--revision=xyz", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("xyz"));
    }

    #[test]
    fn test_parse_double_dash_stops_parsing() {
        let a = args(&["describe", "--", "-m", "not a flag"]);
        let parsed = parse_describe_args(&a);
        assert!(parsed.messages.is_empty());
    }

    #[test]
    fn test_parse_mixed_order() {
        let a = args(&["describe", "-r", "rev1", "--no-edit", "-m", "content", "--message=more"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("rev1"));
        assert_eq!(parsed.messages, vec!["content", "more"]);
    }

    #[test]
    fn test_parse_m_at_end_without_value() {
        // -m with no following value — should not panic
        let a = args(&["describe", "-m"]);
        let parsed = parse_describe_args(&a);
        assert!(parsed.messages.is_empty());
    }

    #[test]
    fn test_parse_r_at_end_without_value() {
        // -r with no following value — should not panic
        let a = args(&["describe", "-r"]);
        let parsed = parse_describe_args(&a);
        assert!(parsed.revision.is_none());
    }

    #[test]
    fn test_message_concatenation() {
        let a = args(&["describe", "-m", "line one", "-m", "line two"]);
        let parsed = parse_describe_args(&a);
        let message = parsed.messages.join("\n");
        assert_eq!(message, "line one\nline two");
    }

    #[test]
    fn test_default_target_is_at() {
        let a = args(&["describe", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        let target = parsed.revision.as_deref().unwrap_or("@");
        assert_eq!(target, "@");
    }

    #[test]
    fn test_explicit_target_overrides_default() {
        let a = args(&["describe", "-r", "mychange", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        let target = parsed.revision.as_deref().unwrap_or("@");
        assert_eq!(target, "mychange");
    }

    // -----------------------------------------------------------------------
    // New: --stdin and --override-plan-protocol detection
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_detects_stdin_flag() {
        let a = args(&["describe", "--stdin"]);
        let parsed = parse_describe_args(&a);
        assert!(parsed.has_stdin);
        assert!(parsed.messages.is_empty());
    }

    #[test]
    fn test_parse_detects_override_flag() {
        let a = args(&["describe", "-m", "msg", "--override-plan-protocol"]);
        let parsed = parse_describe_args(&a);
        assert!(parsed.has_override);
        assert_eq!(parsed.messages, vec!["msg"]);
    }

    #[test]
    fn test_parse_stdin_and_override_together() {
        let a = args(&["describe", "--stdin", "--override-plan-protocol"]);
        let parsed = parse_describe_args(&a);
        assert!(parsed.has_stdin);
        assert!(parsed.has_override);
    }

    // -----------------------------------------------------------------------
    // New: positional REVSETS
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_positional_revset() {
        let a = args(&["describe", "mychange", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("mychange"));
        assert_eq!(parsed.messages, vec!["msg"]);
    }

    #[test]
    fn test_parse_positional_revset_does_not_override_explicit_r() {
        let a = args(&["describe", "-r", "explicit", "positional", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("explicit"));
    }

    #[test]
    fn test_parse_positional_skips_global_option_values() {
        let a = args(&["describe", "--color", "always", "mychange", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("mychange"));
    }

    #[test]
    fn test_parse_positional_skips_global_equals_form() {
        let a = args(&["describe", "--color=always", "mychange", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("mychange"));
    }

    #[test]
    fn test_parse_positional_after_double_dash() {
        // Positional after -- should still be collected
        let a = args(&["describe", "--", "mychange"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("mychange"));
    }

    #[test]
    fn test_parse_positional_with_repository_global() {
        let a = args(&["describe", "--repository", "/some/path", "mychange", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("mychange"));
    }

    #[test]
    fn test_parse_positional_with_short_r_separate() {
        let a = args(&["describe", "-R", "/some/path", "mychange", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("mychange"));
    }

    #[test]
    fn test_parse_positional_with_short_r_joined() {
        let a = args(&["describe", "-R/some/path", "mychange", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.revision.as_deref(), Some("mychange"));
    }

    #[test]
    fn test_parse_desc_alias_at_index_zero() {
        // "desc" at args[0] should be skipped just like "describe"
        let a = args(&["desc", "-m", "msg"]);
        let parsed = parse_describe_args(&a);
        assert_eq!(parsed.messages, vec!["msg"]);
        assert!(parsed.revision.is_none());
    }

    // -----------------------------------------------------------------------
    // strip_override_flag tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_strip_override_flag() {
        let a = args(&["describe", "-m", "msg", "--override-plan-protocol", "-r", "abc"]);
        let stripped = strip_override_flag(&a);
        assert_eq!(
            stripped,
            args(&["describe", "-m", "msg", "-r", "abc"])
        );
    }

    #[test]
    fn test_strip_override_flag_not_present() {
        let a = args(&["describe", "-m", "msg"]);
        let stripped = strip_override_flag(&a);
        assert_eq!(stripped, a);
    }

    // -----------------------------------------------------------------------
    // plan_describe_action tests
    // -----------------------------------------------------------------------

    fn parsed_with_message() -> ParsedDescribeArgs {
        ParsedDescribeArgs {
            messages: vec!["some message".to_string()],
            revision: None,
            has_stdin: false,
            has_override: false,
        }
    }

    fn parsed_with_message_and_override() -> ParsedDescribeArgs {
        ParsedDescribeArgs {
            messages: vec!["some message".to_string()],
            revision: None,
            has_stdin: false,
            has_override: true,
        }
    }

    fn parsed_no_message() -> ParsedDescribeArgs {
        ParsedDescribeArgs {
            messages: vec![],
            revision: None,
            has_stdin: false,
            has_override: false,
        }
    }

    fn parsed_with_stdin() -> ParsedDescribeArgs {
        ParsedDescribeArgs {
            messages: vec![],
            revision: None,
            has_stdin: true,
            has_override: false,
        }
    }

    #[test]
    fn test_action_blocks_message_on_tracked_plan() {
        let path = Path::new("/repo/.jj-plan/02-fix-workspace-passthrough.md");
        let action = plan_describe_action(&parsed_with_message(), Some(path));
        match action {
            DescribeAction::Block { plan_file_path } => {
                assert!(plan_file_path.contains("02-fix-workspace-passthrough.md"));
            }
            other => panic!("expected Block, got {:?}", other),
        }
    }

    #[test]
    fn test_action_allows_message_with_override() {
        let path = Path::new("/repo/.jj-plan/02-fix-workspace-passthrough.md");
        let action = plan_describe_action(&parsed_with_message_and_override(), Some(path));
        match action {
            DescribeAction::AllowOverride { plan_file_path } => {
                assert_eq!(plan_file_path, path);
            }
            other => panic!("expected AllowOverride, got {:?}", other),
        }
    }

    #[test]
    fn test_action_allows_message_on_untracked() {
        let action = plan_describe_action(&parsed_with_message(), None);
        assert_eq!(action, DescribeAction::Allow);
    }

    #[test]
    fn test_action_editor_passthrough() {
        let path = Path::new("/repo/.jj-plan/01-my-plan.md");
        let action = plan_describe_action(&parsed_no_message(), Some(path));
        assert_eq!(action, DescribeAction::EditorPassthrough);
    }

    #[test]
    fn test_action_editor_passthrough_no_plan() {
        let action = plan_describe_action(&parsed_no_message(), None);
        assert_eq!(action, DescribeAction::EditorPassthrough);
    }

    #[test]
    fn test_action_stdin_blocked_on_tracked_plan() {
        let path = Path::new("/repo/.jj-plan/01-my-plan.md");
        let action = plan_describe_action(&parsed_with_stdin(), Some(path));
        match action {
            DescribeAction::Block { plan_file_path } => {
                assert!(plan_file_path.contains("01-my-plan.md"));
            }
            other => panic!("expected Block, got {:?}", other),
        }
    }

    #[test]
    fn test_action_stdin_allowed_on_untracked() {
        let action = plan_describe_action(&parsed_with_stdin(), None);
        assert_eq!(action, DescribeAction::Allow);
    }

    #[test]
    fn test_action_stdin_with_override_allows() {
        let path = Path::new("/repo/.jj-plan/01-my-plan.md");
        let parsed = ParsedDescribeArgs {
            messages: vec![],
            revision: None,
            has_stdin: true,
            has_override: true,
        };
        let action = plan_describe_action(&parsed, Some(path));
        match action {
            DescribeAction::AllowOverride { plan_file_path } => {
                assert_eq!(plan_file_path, path);
            }
            other => panic!("expected AllowOverride, got {:?}", other),
        }
    }

    #[test]
    fn test_block_display_path_is_relative() {
        // The display path in Block should be .jj-plan/filename, not the absolute path
        let path = Path::new("/some/deep/repo/.jj-plan/03-guard-describe-m.md");
        let action = plan_describe_action(&parsed_with_message(), Some(path));
        match action {
            DescribeAction::Block { plan_file_path } => {
                assert_eq!(plan_file_path, ".jj-plan/03-guard-describe-m.md");
            }
            other => panic!("expected Block, got {:?}", other),
        }
    }
}