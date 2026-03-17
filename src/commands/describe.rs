use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::plan_file;
use crate::repo::LoadedRepo;

/// Intercept `jj describe -m "..."` to write the message to the plan file first.
///
/// When a user runs `jj describe -m "content"` in a plan-activated repo, we
/// write the message to the plan file so it becomes the source of truth. Then
/// we delegate to `wrap::wrap()` which runs the standard lifecycle:
/// flush (picks up our file write) → command → sync → show.
///
/// For editor-mode describe (no `-m`/`--message`), we pass through directly
/// to `wrap::wrap()` — the editor flow doesn't conflict because sync will
/// pick up whatever the user wrote via the editor.
///
/// `args` is the full original argument list starting with "describe",
/// e.g. `["describe", "-m", "new content"]` or
/// `["describe", "-r", "abc", "-m", "content"]`.
pub fn handle_describe(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    args: &[String],
    loaded_repo: Option<&LoadedRepo>,
) -> crate::error::Result<i32> {
    // 1. Parse describe args to find -m/--message values and -r/--revision target
    let parsed = parse_describe_args(args);

    // If no -m/--message found, this is editor-mode → pass through to wrap
    if parsed.messages.is_empty() {
        return crate::wrap::wrap(plan_dir, jj, args, loaded_repo);
    }

    // 2. Build the concatenated message (jj concatenates multiple -m with newlines)
    let message = parsed.messages.join("\n");

    // 3. Determine target change: -r/--revision value, or "@" if unspecified
    let target = parsed.revision.as_deref().unwrap_or("@");

    // 4. Resolve the target change ID
    let target_change_id = resolve_target_change_id(jj, target);

    // 5. Find the matching plan file and write the message to it
    if let Some(ref change_id) = target_change_id {
        let plan_files = plan_file::collect_plan_files(&plan_dir.path);

        // Find the entry where the change_id matches (prefix match: the
        // filename contains the shortest unique prefix, so either the
        // resolved ID starts with the file's ID or vice versa).
        let entry = plan_files.iter().find(|e| {
            change_id.starts_with(&e.change_id) || e.change_id.starts_with(change_id.as_str())
        });

        if let Some(entry) = entry {
            // Write the message to the plan file — flush will pick this up
            plan_file::write_or_warn(&entry.path, &message);
        }
        // If no plan file found, the change isn't in the current stack.
        // Fall through to wrap which will run the describe normally.
    }
    // If we couldn't resolve the change ID (e.g. invalid revset), let
    // wrap handle it — jj describe will produce its own error.

    // 6. Pass through to wrap: flush → command → sync → show
    // After flush, jj description matches the file. Then `jj describe -m "..."`
    // sets the same content again (idempotent). Then sync reads jj and writes
    // back to files. All consistent.
    crate::wrap::wrap(plan_dir, jj, args, loaded_repo)
}

// ---------------------------------------------------------------------------
// Arg parsing
// ---------------------------------------------------------------------------

/// Parsed result from scanning describe arguments.
struct ParsedDescribeArgs {
    /// All -m/--message values, in order.
    messages: Vec<String>,
    /// The -r/--revision value, if any.
    revision: Option<String>,
}

/// Parse `jj describe` arguments to extract -m/--message and -r/--revision.
///
/// Handles all common forms:
/// - `-m VALUE` (separate args)
/// - `-mVALUE` (joined short form)
/// - `--message VALUE` (separate args)
/// - `--message=VALUE` (equals form)
/// - `-r VALUE` (separate args)
/// - `-rVALUE` (joined short form)
/// - `--revision VALUE` (separate args)
/// - `--revision=VALUE` (equals form)
fn parse_describe_args(args: &[String]) -> ParsedDescribeArgs {
    let mut messages = Vec::new();
    let mut revision = None;

    // Skip index 0 which is "describe"
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];

        // -- stops option parsing
        if arg == "--" {
            break;
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

        // Skip other flags/positional args (e.g. --no-edit, --stdin, etc.)
        i += 1;
    }

    ParsedDescribeArgs { messages, revision }
}

// ---------------------------------------------------------------------------
// Change ID resolution
// ---------------------------------------------------------------------------

/// Resolve a revision specifier to a change ID using jj.
///
/// Runs `jj log -r <target> -T change_id.shortest(8) --no-graph` silently
/// and returns the trimmed output, or None on failure.
fn resolve_target_change_id(jj: &JjBinary, target: &str) -> Option<String> {
    let result = jj.run_silent(&[
        "log",
        "-r",
        target,
        "-T",
        "change_id.shortest(8)",
        "--no-graph",
    ]);

    match result {
        Ok((status, stdout, _)) if status.success() => {
            let id = stdout.trim().to_string();
            if id.is_empty() {
                None
            } else {
                Some(id)
            }
        }
        _ => None,
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
}