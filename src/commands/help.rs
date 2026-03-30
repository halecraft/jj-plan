use std::io::{self, IsTerminal};

use crate::jj_binary::JjBinary;

/// When to colorize help output.
///
/// Mirrors jj's `--color <WHEN>` values closely enough for jj-plan help:
/// - `always`: always emit ANSI
/// - `never`: never emit ANSI
/// - `auto`: emit ANSI only when stdout is a terminal
/// - `debug`: emit ANSI in debug builds
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorWhen {
    Always,
    Never,
    Auto,
    Debug,
}

impl ColorWhen {
    /// Parse a jj-style color mode value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            "auto" => Some(Self::Auto),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }

    /// Whether ANSI styling should be emitted for the current stdout target.
    pub fn should_color(self) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => io::stdout().is_terminal(),
            Self::Debug => cfg!(debug_assertions),
        }
    }

    /// Whether ANSI styling should be emitted for the current stderr target.
    ///
    /// Plan stack output goes to stderr, so color decisions for it should
    /// check stderr's terminal status, not stdout's.
    pub fn should_color_stderr(self) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => io::stderr().is_terminal(),
            Self::Debug => cfg!(debug_assertions),
        }
    }
}

/// Parsed info for a `jj plan --help`-style invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanHelpInvocation {
    pub color_override: Option<ColorWhen>,
}

/// Invocation classification for the help path.
///
/// This deliberately recognizes only the top-level `plan --help` shape, not
/// arbitrary subcommand help such as `plan stack --help`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvocationKind {
    PlanHelp(PlanHelpInvocation),
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanHelp {
    pub title: &'static str,
    pub mental_model: &'static str,
    pub usage: Vec<&'static str>,
    pub workflow: Vec<(&'static str, &'static str)>,
    pub commands: Vec<HelpEntry>,
    pub options: Vec<HelpEntry>,
    pub notes: Vec<&'static str>,
    pub docs: Vec<HelpEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpEntry {
    pub label: &'static str,
    pub description: &'static str,
}

/// Current default entry point used by existing callers.
///
/// Resolves the default color mode from jj's `ui.color` setting, then lets any
/// explicit `--color` flag in the current process args override it.
pub fn print_help() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_default = configured_default_color_mode();
    print!("{}", help_text_for_args(&args, config_default));
}



/// Build help text for an argument vector with a configurable default color mode.
pub fn help_text_for_args(args: &[String], config_default: ColorWhen) -> String {
    let color = resolve_help_color_mode(args, config_default);
    render_plan_help(&build_plan_help(), color)
}

/// Resolve the configured default color mode from `jj config get ui.color`.
///
/// Falls back to `auto` if the real jj binary cannot be resolved, the config
/// lookup fails, or the returned value is unknown.
fn configured_default_color_mode() -> ColorWhen {
    let Ok(jj) = JjBinary::resolve() else {
        return ColorWhen::Auto;
    };

    let Ok((status, stdout, _stderr)) = jj.run_silent(&["config", "get", "ui.color"]) else {
        return ColorWhen::Auto;
    };

    if !status.success() {
        return ColorWhen::Auto;
    }

    ColorWhen::parse(stdout.trim()).unwrap_or(ColorWhen::Auto)
}

/// Classify whether an invocation is the top-level `jj plan --help` help path.
///
/// Recognized forms include:
/// - `plan --help`
/// - `plan -h`
/// - `--color always plan --help`
/// - `plan --help --color always`
/// - `--color=never plan -h`
pub fn classify_invocation(args: &[String]) -> InvocationKind {
    let (leading_color, command_index) = match scan_leading_globals(args) {
        Some(result) => result,
        None => return InvocationKind::Other,
    };

    let Some(command) = args.get(command_index) else {
        return InvocationKind::Other;
    };

    if command != "plan" {
        return InvocationKind::Other;
    }

    let mut color_override = leading_color;
    let mut saw_help = false;
    let mut idx = command_index + 1;

    while idx < args.len() {
        let arg = args[idx].as_str();

        match arg {
            "--help" | "-h" => {
                saw_help = true;
                idx += 1;
            }
            "--color" => {
                let Some(value) = args.get(idx + 1) else {
                    return InvocationKind::Other;
                };
                let Some(parsed) = ColorWhen::parse(value) else {
                    return InvocationKind::Other;
                };
                color_override = Some(parsed);
                idx += 2;
            }
            _ if arg.starts_with("--color=") => {
                let Some((parsed, consumed)) = parse_color_flag(args, idx) else {
                    return InvocationKind::Other;
                };
                color_override = Some(parsed);
                idx += consumed;
            }
            _ => {
                // Any non-help, non-color token after `plan` means this is not
                // the top-level `plan --help` surface.
                return InvocationKind::Other;
            }
        }
    }

    if saw_help {
        InvocationKind::PlanHelp(PlanHelpInvocation { color_override })
    } else {
        InvocationKind::Other
    }
}

/// Resolve the effective help color mode.
///
/// Explicit `--color` flags win. Otherwise the caller-provided configured or
/// default mode is used.
pub fn resolve_help_color_mode(args: &[String], config_default: ColorWhen) -> ColorWhen {
    match classify_invocation(args) {
        InvocationKind::PlanHelp(invocation) => invocation.color_override.unwrap_or(config_default),
        InvocationKind::Other => config_default,
    }
}

/// Build the structured help model.
///
/// This is the pure "PLAN" step; formatting decisions happen in
/// `render_plan_help()`.
pub fn build_plan_help() -> PlanHelp {
    PlanHelp {
        title: "jj plan — plan-oriented programming commands",
        mental_model:
            "One bookmark = one plan = one PR. Plans are jj change descriptions synced to `.jj-plan/` markdown files.",
        usage: vec![
            "jj plan [SUBCOMMAND]",
            "jj plan --help [--color <WHEN>]",
        ],
        workflow: vec![
            ("jj plan new <bookmark>", "Create a plan (change + bookmark + template)"),
            ("$EDITOR .jj-plan/NN-bookmark.md", "Edit the plan file (path shown in stack output)"),
            ("jj plan new <next-bookmark>", "Add another plan to the stack"),
            ("jj plan done", "Mark the current plan done"),
            ("jj plan", "Show plan summary (same as `jj plan summary`)"),
        ],
        commands: vec![
            HelpEntry {
                label: "new <bookmark> [-r REV]",
                description: "Create a plan (change + bookmark + plan file + registry entry)",
            },
            HelpEntry {
                label: "track <bookmark>",
                description: "Adopt an existing bookmark as a plan",
            },
            HelpEntry {
                label: "untrack <bookmark>",
                description: "Remove a bookmark from plan tracking",
            },
            HelpEntry {
                label: "done [flags] [CHANGE_ID]",
                description: "Mark a plan as done (defaults to @)",
            },
            HelpEntry {
                label: "  --stack",
                description: "Mark all plans in the stack as done",
            },
            HelpEntry {
                label: "  --keep-scratch",
                description: "Keep [scratch] sections instead of stripping them",
            },
            HelpEntry {
                label: "  --dry-run",
                description: "Show what would change without modifying anything",
            },
            HelpEntry {
                label: "next",
                description: "Advance @ to the next plan in the stack",
            },
            HelpEntry {
                label: "prev",
                description: "Move @ to the previous plan in the stack",
            },
            HelpEntry {
                label: "go <N | bookmark | ID>",
                description: "Jump to a plan by index (1-based), bookmark name, or change ID",
            },
            HelpEntry {
                label: "summary [target] [flags]",
                description: "Show structured plan summary (LLM-friendly)",
            },
            HelpEntry {
                label: "  --json",
                description: "Output as JSON instead of text",
            },
            HelpEntry {
                label: "  --no-diff-stat",
                description: "Suppress diff stat section",
            },
            HelpEntry {
                label: "  --stack=full|minimal|quiet",
                description: "Control stack verbosity (default: full)",
            },
            HelpEntry {
                label: "config",
                description: "Show resolved configuration and stack info",
            },
        ],
        options: vec![
            HelpEntry {
                label: "--help, -h",
                description: "Show this help message",
            },
            HelpEntry {
                label: "--color <WHEN>",
                description: "When to colorize output [always, never, debug, auto]",
            },
        ],
        notes: vec![
            "`jj plan` shows the plan summary if @ is a plan, or orientation with next steps.",
            "`jj plan summary` always shows raw summary data (even if @ is not a plan).",
            "`jj status` shows the current plan stack with file paths.",
            "Plan files are `.jj-plan/NN-bookmark.md` — paths shown as `→` in stack output.",
            "`jj describe -m` on a tracked plan is blocked; edit the plan file directly.",
            "`jj stack submit/sync/merge` — stacked PR operations.",
        ],
        docs: vec![
            HelpEntry {
                label: "README.md",
                description: "Overview, philosophy, and quick start",
            },
            HelpEntry {
                label: "MANUAL.md",
                description: "Exhaustive command reference and recipes",
            },
        ],
    }
}

/// Render structured help as plain text or ANSI-styled text.
///
/// This is the pure "EXECUTE formatting" step. It returns a string instead of
/// writing to stdout so it is easy to unit test.
pub fn render_plan_help(help: &PlanHelp, color: ColorWhen) -> String {
    let ansi = color.should_color();
    let mut out = String::new();

    out.push_str(help.title);
    out.push_str("\n\n");
    out.push_str(help.mental_model);
    out.push_str("\n\n");

    push_heading(&mut out, "Usage:", ansi);
    for line in &help.usage {
        push_code_line(&mut out, line, ansi);
    }
    out.push('\n');

    push_heading(&mut out, "Workflow:", ansi);
    for (label, description) in &help.workflow {
        push_entry(&mut out, label, description, ansi);
    }
    out.push('\n');

    push_heading(&mut out, "Commands:", ansi);
    for entry in &help.commands {
        push_entry(&mut out, entry.label, entry.description, ansi);
    }
    out.push('\n');

    push_heading(&mut out, "Options:", ansi);
    for entry in &help.options {
        push_entry(&mut out, entry.label, entry.description, ansi);
    }
    out.push('\n');

    push_heading(&mut out, "Notes:", ansi);
    for note in &help.notes {
        out.push_str("  ");
        out.push_str(note);
        out.push('\n');
    }
    out.push('\n');

    push_heading(&mut out, "Docs:", ansi);
    for entry in &help.docs {
        push_entry(&mut out, entry.label, entry.description, ansi);
    }

    out
}

// ---------------------------------------------------------------------------
// Pure parsing helpers
// ---------------------------------------------------------------------------

fn scan_leading_globals(args: &[String]) -> Option<(Option<ColorWhen>, usize)> {
    let mut idx = 0;
    let mut color_override = None;

    while idx < args.len() {
        let arg = args[idx].as_str();

        match arg {
            "--color" => {
                let value = args.get(idx + 1)?;
                let parsed = ColorWhen::parse(value)?;
                color_override = Some(parsed);
                idx += 2;
            }
            "-R" | "--repository" | "--at-operation" | "--at-op" => {
                if idx + 1 >= args.len() {
                    return None;
                }
                idx += 2;
            }
            "--ignore-working-copy"
            | "--ignore-immutable"
            | "--debug"
            | "--quiet"
            | "--no-pager" => {
                idx += 1;
            }
            _ if arg.starts_with("--color=") => {
                let (parsed, consumed) = parse_color_flag(args, idx)?;
                color_override = Some(parsed);
                idx += consumed;
            }
            _ if arg.starts_with("--repository=")
                || arg.starts_with("--at-operation=")
                || arg.starts_with("--at-op=") =>
            {
                idx += 1;
            }
            _ if arg.starts_with('-') => {
                // Unknown leading option — don't try to reclassify.
                return None;
            }
            _ => {
                return Some((color_override, idx));
            }
        }
    }

    Some((color_override, idx))
}

fn parse_color_flag(args: &[String], idx: usize) -> Option<(ColorWhen, usize)> {
    let arg = args.get(idx)?.as_str();

    if arg == "--color" {
        let value = args.get(idx + 1)?;
        let parsed = ColorWhen::parse(value)?;
        Some((parsed, 2))
    } else if let Some(value) = arg.strip_prefix("--color=") {
        let parsed = ColorWhen::parse(value)?;
        Some((parsed, 1))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

fn push_heading(out: &mut String, label: &str, ansi: bool) {
    if ansi {
        out.push_str("\x1b[1m\x1b[33m");
        out.push_str(label);
        out.push_str("\x1b[0m\n");
    } else {
        out.push_str(label);
        out.push('\n');
    }
}

fn push_code_line(out: &mut String, line: &str, ansi: bool) {
    out.push_str("  ");
    if ansi {
        out.push_str("\x1b[1m\x1b[32m");
        out.push_str(line);
        out.push_str("\x1b[0m");
    } else {
        out.push_str(line);
    }
    out.push('\n');
}

fn push_entry(out: &mut String, label: &str, description: &str, ansi: bool) {
    const LABEL_WIDTH: usize = 28;

    let padding = LABEL_WIDTH.saturating_sub(display_width(label));
    out.push_str("  ");

    if ansi {
        out.push_str("\x1b[1m\x1b[32m");
        out.push_str(label);
        out.push_str("\x1b[0m");
    } else {
        out.push_str(label);
    }

    out.push_str(&" ".repeat(padding.max(2)));
    out.push_str(description);
    out.push('\n');
}

fn display_width(s: &str) -> usize {
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // -----------------------------------------------------------------------
    // Invocation classification
    // -----------------------------------------------------------------------

    #[test]
    fn classify_plain_plan_help() {
        assert_eq!(
            classify_invocation(&args(&["plan", "--help"])),
            InvocationKind::PlanHelp(PlanHelpInvocation {
                color_override: None
            })
        );
    }

    #[test]
    fn classify_plan_short_help() {
        assert_eq!(
            classify_invocation(&args(&["plan", "-h"])),
            InvocationKind::PlanHelp(PlanHelpInvocation {
                color_override: None
            })
        );
    }

    #[test]
    fn classify_leading_global_color_then_plan_help() {
        assert_eq!(
            classify_invocation(&args(&["--color", "always", "plan", "--help"])),
            InvocationKind::PlanHelp(PlanHelpInvocation {
                color_override: Some(ColorWhen::Always)
            })
        );
    }

    #[test]
    fn classify_trailing_color_after_plan_help() {
        assert_eq!(
            classify_invocation(&args(&["plan", "--help", "--color", "always"])),
            InvocationKind::PlanHelp(PlanHelpInvocation {
                color_override: Some(ColorWhen::Always)
            })
        );
    }

    #[test]
    fn classify_color_equals_then_short_help() {
        assert_eq!(
            classify_invocation(&args(&["--color=never", "plan", "-h"])),
            InvocationKind::PlanHelp(PlanHelpInvocation {
                color_override: Some(ColorWhen::Never)
            })
        );
    }

    #[test]
    fn classify_non_help_invocation_as_other() {
        assert_eq!(
            classify_invocation(&args(&["plan", "stack"])),
            InvocationKind::Other
        );
    }

    #[test]
    fn classify_subcommand_help_as_other() {
        assert_eq!(
            classify_invocation(&args(&["plan", "stack", "--help"])),
            InvocationKind::Other
        );
    }

    #[test]
    fn classify_unknown_leading_option_as_other() {
        assert_eq!(
            classify_invocation(&args(&["--mystery", "plan", "--help"])),
            InvocationKind::Other
        );
    }

    #[test]
    fn classify_repository_then_plan_help() {
        assert_eq!(
            classify_invocation(&args(&["-R", ".", "plan", "--help"])),
            InvocationKind::PlanHelp(PlanHelpInvocation {
                color_override: None
            })
        );
    }

    // -----------------------------------------------------------------------
    // Color resolution
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_help_color_mode_prefers_explicit_always() {
        let result = resolve_help_color_mode(
            &args(&["--color", "always", "plan", "--help"]),
            ColorWhen::Never,
        );
        assert_eq!(result, ColorWhen::Always);
    }

    #[test]
    fn resolve_help_color_mode_prefers_explicit_never() {
        let result = resolve_help_color_mode(
            &args(&["plan", "--help", "--color=never"]),
            ColorWhen::Always,
        );
        assert_eq!(result, ColorWhen::Never);
    }

    #[test]
    fn resolve_help_color_mode_prefers_explicit_auto() {
        let result = resolve_help_color_mode(
            &args(&["plan", "--help", "--color=auto"]),
            ColorWhen::Always,
        );
        assert_eq!(result, ColorWhen::Auto);
    }

    #[test]
    fn resolve_help_color_mode_falls_back_to_default() {
        let result = resolve_help_color_mode(&args(&["plan", "--help"]), ColorWhen::Debug);
        assert_eq!(result, ColorWhen::Debug);
    }

    // -----------------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------------

    #[test]
    fn render_plain_help_has_no_ansi() {
        let text = render_plan_help(&build_plan_help(), ColorWhen::Never);
        assert!(!text.contains("\x1b["));
    }

    #[test]
    fn render_color_help_has_ansi() {
        let text = render_plan_help(&build_plan_help(), ColorWhen::Always);
        assert!(text.contains("\x1b["));
    }

    #[test]
    fn render_help_contains_mental_model_workflow_and_docs() {
        let text = render_plan_help(&build_plan_help(), ColorWhen::Never);

        assert!(text.contains("One bookmark = one plan = one PR."));
        assert!(text.contains("jj plan new <bookmark>"));
        assert!(text.contains("track <bookmark>"));
        assert!(text.contains("untrack <bookmark>"));
        assert!(text.contains("$EDITOR .jj-plan/NN-bookmark.md"));
        assert!(text.contains("`jj status` shows the current plan stack"));
        assert!(text.contains("`jj stack submit/sync/merge`"));
        assert!(text.contains("README.md"));
        assert!(text.contains("MANUAL.md"));
    }

    #[test]
    fn help_text_for_args_uses_explicit_color_override() {
        let text = help_text_for_args(
            &args(&["plan", "--help", "--color", "always"]),
            ColorWhen::Never,
        );
        assert!(text.contains("\x1b["));
    }

    #[test]
    fn help_text_for_args_uses_default_when_no_override() {
        let text = help_text_for_args(&args(&["plan", "--help"]), ColorWhen::Never);
        assert!(!text.contains("\x1b["));
    }
}