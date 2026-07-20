use std::io::{self, IsTerminal};

use crate::dispatch::classify_args;
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

// ---------------------------------------------------------------------------
// Help model — one section-list screen models every help surface: the `jj
// plan` and `jj stack` landing pages and every leaf subcommand.
// ---------------------------------------------------------------------------

/// A single help screen: a title, an optional one-line blurb, a usage block,
/// and an ordered list of labeled sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpScreen {
    pub title: &'static str,
    pub blurb: Option<&'static str>,
    pub usage: Vec<&'static str>,
    pub sections: Vec<HelpSection>,
}

/// A labeled section (`Commands:`, `Options:`, `Notes:`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpSection {
    pub heading: &'static str,
    pub body: SectionBody,
}

/// A section body is either label+description rows (commands, flags, args) or
/// plain wrapped lines (notes, examples).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionBody {
    Entries(Vec<HelpEntry>),
    Lines(Vec<&'static str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpEntry {
    pub label: &'static str,
    pub description: &'static str,
}

impl HelpEntry {
    const fn new(label: &'static str, description: &'static str) -> Self {
        HelpEntry { label, description }
    }
}

/// Which help screen an invocation asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpTarget {
    PlanTop,
    PlanSub(String),
    StackTop,
    StackSub(String),
}

/// Canonical `jj plan` subcommand names that have a dedicated help screen.
pub const PLAN_SUBCOMMANDS: &[&str] = &[
    "new", "track", "untrack", "done", "summary", "next", "prev", "go", "config",
];

/// Canonical `jj stack` subcommand names that have a dedicated help screen.
pub const STACK_SUBCOMMANDS: &[&str] = &["submit", "sync", "merge", "untrack", "auth"];

// ---------------------------------------------------------------------------
// Classification (pure) — resolved at the shell boundary before any repo work,
// so all help is available pre-activation and is side-effect-free.
// ---------------------------------------------------------------------------

/// Classify whether an invocation asks for jj-plan help, and for which screen.
///
/// Returns `None` for anything that is not a `jj plan`/`jj stack` `--help`
/// invocation — including bare `jj --help` and every real jj command — so the
/// caller passes those through to the real `jj` binary unchanged.
///
/// Built on top of [`classify_args`], so leading globals (`-R`, `--no-pager`),
/// `--color`, and built-in aliases are already handled. An unknown subcommand
/// falls back to the namespace's top-level screen.
pub fn classify_help(args: &[String]) -> Option<(HelpTarget, Option<ColorWhen>)> {
    let invocation = classify_args(args);
    let command = invocation.command.as_deref()?;

    let is_plan = match command {
        "plan" => true,
        "stack" => false,
        _ => return None,
    };

    // Start from any leading `--color` captured before the command; a trailing
    // `--color` after the command overrides it.
    let mut color_override = invocation
        .leading_color
        .as_deref()
        .and_then(ColorWhen::parse);
    let mut saw_help = false;
    let mut subcommand: Option<&str> = None;

    let mut idx = invocation.command_index + 1;
    while idx < args.len() {
        let arg = args[idx].as_str();
        match arg {
            "--help" | "-h" => {
                saw_help = true;
                idx += 1;
            }
            "--color" => {
                if let Some(value) = args.get(idx + 1) {
                    if let Some(parsed) = ColorWhen::parse(value) {
                        color_override = Some(parsed);
                    }
                    idx += 2;
                } else {
                    idx += 1;
                }
            }
            _ if arg.starts_with("--color=") => {
                if let Some(parsed) = arg.strip_prefix("--color=").and_then(ColorWhen::parse) {
                    color_override = Some(parsed);
                }
                idx += 1;
            }
            _ if arg.starts_with('-') => {
                // Any other flag is irrelevant to help classification.
                idx += 1;
            }
            _ => {
                if subcommand.is_none() {
                    subcommand = Some(arg);
                }
                idx += 1;
            }
        }
    }

    if !saw_help {
        return None;
    }

    let target = match (is_plan, subcommand) {
        (true, Some(s)) if PLAN_SUBCOMMANDS.contains(&s) => HelpTarget::PlanSub(s.to_string()),
        (true, _) => HelpTarget::PlanTop,
        (false, Some(s)) if STACK_SUBCOMMANDS.contains(&s) => HelpTarget::StackSub(s.to_string()),
        (false, _) => HelpTarget::StackTop,
    };

    Some((target, color_override))
}

// ---------------------------------------------------------------------------
// Shell entry points
// ---------------------------------------------------------------------------

/// Resolve the configured default color mode from `jj config get ui.color`.
///
/// Falls back to `auto` if the real jj binary cannot be resolved, the config
/// lookup fails, or the returned value is unknown.
pub fn configured_default_color_mode() -> ColorWhen {
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

/// Print the help screen for a classified target to stdout.
///
/// The imperative shell: resolves color (explicit `--color` wins over the
/// configured default) and renders the pure screen model.
pub fn print_help_screen(target: HelpTarget, color_override: Option<ColorWhen>) {
    let color = color_override.unwrap_or_else(configured_default_color_mode);
    print!("{}", render_help_screen(&screen_for_target(&target), color));
}

fn screen_for_target(target: &HelpTarget) -> HelpScreen {
    match target {
        HelpTarget::PlanTop => build_plan_help(),
        HelpTarget::PlanSub(sub) => plan_subcommand_help(sub).unwrap_or_else(build_plan_help),
        HelpTarget::StackTop => build_stack_help(),
        HelpTarget::StackSub(sub) => stack_subcommand_help(sub).unwrap_or_else(build_stack_help),
    }
}

// ---------------------------------------------------------------------------
// Screen models (pure)
// ---------------------------------------------------------------------------

/// The top-level `jj plan --help` landing page.
///
/// Workflow-first and deliberately compact: one line per command, no inline
/// flag rows (those live in each subcommand's own `--help`).
pub fn build_plan_help() -> HelpScreen {
    HelpScreen {
        title: "jj plan — plan-oriented programming commands",
        blurb: Some(
            "One bookmark = one plan = one PR. Plans are jj change descriptions synced to `.jj-plan/` markdown files.",
        ),
        usage: vec!["jj plan [SUBCOMMAND]", "jj plan <subcommand> --help"],
        sections: vec![
            HelpSection {
                heading: "Workflow:",
                body: SectionBody::Entries(vec![
                    HelpEntry::new(
                        "jj plan new <bookmark>",
                        "Create a plan (change + bookmark + template)",
                    ),
                    HelpEntry::new(
                        "$EDITOR .jj-plan/NN-bookmark.md",
                        "Edit the plan file (path shown in stack output)",
                    ),
                    HelpEntry::new("jj plan done", "Mark the current plan done"),
                    HelpEntry::new("jj plan", "Show the plan summary or orientation"),
                ]),
            },
            HelpSection {
                heading: "Commands:",
                body: SectionBody::Entries(vec![
                    HelpEntry::new(
                        "new <bookmark>",
                        "Create a plan; flags -A/-B/--stack (`jj plan new --help`)",
                    ),
                    HelpEntry::new(
                        "track [bookmark]",
                        "Adopt an existing bookmark as a plan (auto-detects from @)",
                    ),
                    HelpEntry::new("untrack <bookmark>", "Remove a bookmark from plan tracking"),
                    HelpEntry::new(
                        "done [CHANGE_ID]",
                        "Mark a plan (or --stack) done (`jj plan done --help`)",
                    ),
                    HelpEntry::new(
                        "summary [target]",
                        "Structured, LLM-friendly summary (`jj plan summary --help`)",
                    ),
                    HelpEntry::new("next / prev", "Move @ to the next / previous plan"),
                    HelpEntry::new(
                        "go <N | bookmark | ID>",
                        "Jump to a plan by index, bookmark, or change ID",
                    ),
                    HelpEntry::new("config", "Show resolved configuration and stack info"),
                    HelpEntry::new(
                        "stack [SUBCOMMAND]",
                        "Stacked-PR ops: `jj stack submit/sync/merge` — see `jj stack --help`",
                    ),
                ]),
            },
            HelpSection {
                heading: "Options:",
                body: SectionBody::Entries(vec![
                    HelpEntry::new("--help, -h", "Show this help message"),
                    HelpEntry::new(
                        "--color <WHEN>",
                        "When to colorize output [always, never, debug, auto]",
                    ),
                ]),
            },
            HelpSection {
                heading: "Notes:",
                body: SectionBody::Lines(vec![
                    "`jj status` shows the current plan stack with file paths.",
                    "Edit `.jj-plan/NN-bookmark.md` directly — `jj describe -m`/`--stdin` on a tracked plan is blocked.",
                    "Add --override-plan-protocol to replace a plan's full description anyway.",
                ]),
            },
            HelpSection {
                heading: "Docs:",
                body: SectionBody::Entries(vec![
                    HelpEntry::new("README.md", "Overview, philosophy, and quick start"),
                    HelpEntry::new("MANUAL.md", "Exhaustive command reference and recipes"),
                ]),
            },
        ],
    }
}

/// The top-level `jj stack --help` landing page.
pub fn build_stack_help() -> HelpScreen {
    HelpScreen {
        title: "jj stack — stack-oriented PR operations",
        blurb: Some(
            "Stacked-PR operations over a plan stack: push, sync, and merge dependent PRs.",
        ),
        usage: vec!["jj stack [SUBCOMMAND] [OPTIONS]", "jj stack <subcommand> --help"],
        sections: vec![
            HelpSection {
                heading: "Subcommands:",
                body: SectionBody::Entries(vec![
                    HelpEntry::new("submit [bookmark]", "Push bookmarks and create/update PRs"),
                    HelpEntry::new("sync", "Fetch from the remote, then re-submit the stack"),
                    HelpEntry::new("merge", "Merge approved PRs from the bottom of the stack"),
                    HelpEntry::new("untrack", "Stop tracking the current stack"),
                    HelpEntry::new("auth", "Authentication management (github/gitlab/gitea)"),
                ]),
            },
            HelpSection {
                heading: "Options:",
                body: SectionBody::Entries(vec![
                    HelpEntry::new("--all", "Show all stacks across the repo"),
                    HelpEntry::new(
                        "--format=<compact|regular>",
                        "Output format (default: compact)",
                    ),
                    HelpEntry::new("--help, -h", "Show this help message"),
                ]),
            },
            HelpSection {
                heading: "Notes:",
                body: SectionBody::Lines(vec![
                    "Bare `jj stack` shows the current stack: bookmark structure, sync status, and PR status.",
                ]),
            },
        ],
    }
}

/// Per-subcommand help for `jj plan <sub>`.
pub fn plan_subcommand_help(sub: &str) -> Option<HelpScreen> {
    let screen = match sub {
        "new" => HelpScreen {
            title: "jj plan new — create a plan",
            blurb: Some("Creates a jj change + bookmark + plan file + registry entry."),
            usage: vec!["jj plan new <bookmark> [--stack <name>] [-r <rev>] [-A <rev>] [-B <rev>]"],
            sections: vec![
                HelpSection {
                    heading: "Arguments:",
                    body: SectionBody::Entries(vec![HelpEntry::new(
                        "<bookmark>",
                        "Name for the bookmark and plan file (e.g. feat-auth)",
                    )]),
                },
                HelpSection {
                    heading: "Options:",
                    body: SectionBody::Entries(vec![
                        HelpEntry::new(
                            "--stack <name>",
                            "Create a new named stack (a stack/<name> base bookmark)",
                        ),
                        HelpEntry::new("-r <rev>", "Create the plan change at <rev> (passed to jj new)"),
                        HelpEntry::new(
                            "-A, --insert-after <rev>",
                            "Insert the plan after <rev> (passed to jj new)",
                        ),
                        HelpEntry::new(
                            "-B, --insert-before <rev>",
                            "Insert the plan before <rev> (passed to jj new)",
                        ),
                    ]),
                },
                HelpSection {
                    heading: "Notes:",
                    body: SectionBody::Lines(vec![
                        "With no positioning flag, the plan is added after @ (adopting @ if it is an",
                        "empty, unbookmarked, undescribed change).",
                    ]),
                },
            ],
        },
        "track" => HelpScreen {
            title: "jj plan track — adopt an existing bookmark as a plan",
            blurb: None,
            usage: vec!["jj plan track [bookmark]"],
            sections: vec![
                HelpSection {
                    heading: "Arguments:",
                    body: SectionBody::Entries(vec![HelpEntry::new(
                        "[bookmark]",
                        "Bookmark to adopt; if omitted, auto-detects a single untracked bookmark at @",
                    )]),
                },
                HelpSection {
                    heading: "Notes:",
                    body: SectionBody::Lines(vec![
                        "The bookmark must already exist (create it with `jj bookmark create`).",
                    ]),
                },
            ],
        },
        "untrack" => HelpScreen {
            title: "jj plan untrack — stop tracking a bookmark as a plan",
            blurb: None,
            usage: vec!["jj plan untrack <bookmark>"],
            sections: vec![HelpSection {
                heading: "Arguments:",
                body: SectionBody::Entries(vec![HelpEntry::new(
                    "<bookmark>",
                    "Bookmark to untrack; the bookmark and change remain, only the plan record and file are removed",
                )]),
            }],
        },
        "done" => HelpScreen {
            title: "jj plan done — mark a plan done",
            blurb: Some("Strips [scratch] sections and sets plan-status: ✅."),
            usage: vec!["jj plan done [CHANGE_ID] [flags]"],
            sections: vec![
                HelpSection {
                    heading: "Arguments:",
                    body: SectionBody::Entries(vec![HelpEntry::new(
                        "[CHANGE_ID]",
                        "Plan to mark done (defaults to @)",
                    )]),
                },
                HelpSection {
                    heading: "Options:",
                    body: SectionBody::Entries(vec![
                        HelpEntry::new("--stack", "Mark all plans in the stack as done"),
                        HelpEntry::new(
                            "--keep-scratch",
                            "Keep [scratch] sections instead of stripping them",
                        ),
                        HelpEntry::new(
                            "--dry-run",
                            "Show what would change without modifying anything",
                        ),
                        HelpEntry::new(
                            "--show-stripped=<mode>",
                            "Report stripped scratch sections: full | toc | headings | none (default: toc)",
                        ),
                    ]),
                },
            ],
        },
        "summary" => HelpScreen {
            title: "jj plan summary — structured plan summary (LLM-friendly)",
            blurb: None,
            usage: vec!["jj plan summary [target] [flags]"],
            sections: vec![
                HelpSection {
                    heading: "Arguments:",
                    body: SectionBody::Entries(vec![HelpEntry::new(
                        "[target]",
                        "Revset to summarize (defaults to @)",
                    )]),
                },
                HelpSection {
                    heading: "Options:",
                    body: SectionBody::Entries(vec![
                        HelpEntry::new("--json", "Output as JSON instead of text"),
                        HelpEntry::new("--no-diff-stat", "Suppress the diff stat section"),
                        HelpEntry::new(
                            "--stack=<mode>",
                            "Stack verbosity: full | minimal | quiet (default: full)",
                        ),
                    ]),
                },
            ],
        },
        "next" => HelpScreen {
            title: "jj plan next — advance @ to the next plan",
            blurb: None,
            usage: vec!["jj plan next"],
            sections: vec![HelpSection {
                heading: "Notes:",
                body: SectionBody::Lines(vec!["Moves the working copy to the next plan up the stack."]),
            }],
        },
        "prev" => HelpScreen {
            title: "jj plan prev — move @ to the previous plan",
            blurb: None,
            usage: vec!["jj plan prev"],
            sections: vec![HelpSection {
                heading: "Notes:",
                body: SectionBody::Lines(vec!["Moves the working copy to the previous plan down the stack."]),
            }],
        },
        "go" => HelpScreen {
            title: "jj plan go — jump to a plan",
            blurb: None,
            usage: vec!["jj plan go <N | bookmark | ID>"],
            sections: vec![HelpSection {
                heading: "Arguments:",
                body: SectionBody::Entries(vec![HelpEntry::new(
                    "<N | bookmark | ID>",
                    "Target: 1-based stack index, bookmark name, or change ID",
                )]),
            }],
        },
        "config" => HelpScreen {
            title: "jj plan config — show resolved configuration",
            blurb: None,
            usage: vec!["jj plan config"],
            sections: vec![HelpSection {
                heading: "Notes:",
                body: SectionBody::Lines(vec![
                    "Prints the resolved plan directory, stack format, and current stack info.",
                ]),
            }],
        },
        _ => return None,
    };
    Some(screen)
}

/// Per-subcommand help for `jj stack <sub>`.
pub fn stack_subcommand_help(sub: &str) -> Option<HelpScreen> {
    let screen = match sub {
        "submit" => HelpScreen {
            title: "jj stack submit — push bookmarks and create/update PRs",
            blurb: Some(
                "With no bookmark, submits up to the tip-most bookmarked segment near @.",
            ),
            usage: vec!["jj stack submit [bookmark] [options]"],
            sections: vec![
                HelpSection {
                    heading: "Arguments:",
                    body: SectionBody::Entries(vec![HelpEntry::new(
                        "[bookmark]",
                        "Submit up to this bookmark",
                    )]),
                },
                HelpSection {
                    heading: "Options:",
                    body: SectionBody::Entries(vec![
                        HelpEntry::new("--dry-run", "Preview without making changes"),
                        HelpEntry::new("--draft", "Create new PRs as drafts"),
                        HelpEntry::new("--publish", "Convert existing draft PRs to ready-for-review"),
                        HelpEntry::new(
                            "--update-descriptions",
                            "Push current plan content to existing PR titles/bodies",
                        ),
                        HelpEntry::new("--no-comments", "Skip stack navigation comments"),
                        HelpEntry::new(
                            "--continue-on-error",
                            "Don't abort on first failure (default: abort)",
                        ),
                        HelpEntry::new("--allow-gaps", "Allow unbookmarked changes between bookmarks"),
                        HelpEntry::new("--remote <remote>", "Remote to push to (default: origin)"),
                    ]),
                },
                HelpSection {
                    heading: "Notes:",
                    body: SectionBody::Lines(vec![
                        "--draft and --publish are mutually exclusive.",
                        "Execution aborts on first failure by default (stacked PRs are dependent).",
                    ]),
                },
            ],
        },
        "sync" => HelpScreen {
            title: "jj stack sync — fetch from remote and re-submit the stack",
            blurb: Some("Fetches, then pushes bookmarks and updates PRs (fetch + submit)."),
            usage: vec!["jj stack sync [options]"],
            sections: vec![HelpSection {
                heading: "Options:",
                body: SectionBody::Entries(vec![
                    HelpEntry::new("--dry-run", "Preview without making changes"),
                    HelpEntry::new("--remote <remote>", "Remote to use (default: origin)"),
                ]),
            }],
        },
        "merge" => HelpScreen {
            title: "jj stack merge — merge approved PRs from the bottom of the stack",
            blurb: Some(
                "Merges the first ready PR, then rebases and pushes the rest onto updated trunk.",
            ),
            usage: vec!["jj stack merge [options]"],
            sections: vec![HelpSection {
                heading: "Options:",
                body: SectionBody::Entries(vec![
                    HelpEntry::new("--dry-run", "Preview the merge plan without merging"),
                    HelpEntry::new("--wait", "After merge+rebase, poll CI and continue merging"),
                    HelpEntry::new("--remote <remote>", "Remote to use (default: origin)"),
                ]),
            }],
        },
        "untrack" => HelpScreen {
            title: "jj stack untrack — stop tracking the current stack",
            blurb: None,
            usage: vec!["jj stack untrack [--dry-run]"],
            sections: vec![HelpSection {
                heading: "Options:",
                body: SectionBody::Entries(vec![HelpEntry::new(
                    "--dry-run",
                    "Show what would be untracked without modifying anything",
                )]),
            }],
        },
        "auth" => HelpScreen {
            title: "jj stack auth — authentication management",
            blurb: None,
            usage: vec!["jj stack auth <platform> <action>"],
            sections: vec![
                HelpSection {
                    heading: "Arguments:",
                    body: SectionBody::Entries(vec![
                        HelpEntry::new("<platform>", "github | gitlab | gitea"),
                        HelpEntry::new("<action>", "test | setup"),
                    ]),
                },
                HelpSection {
                    heading: "Examples:",
                    body: SectionBody::Lines(vec![
                        "jj stack auth github test    Test GitHub authentication",
                        "jj stack auth github setup   Show GitHub setup instructions",
                        "jj stack auth gitlab test    Test GitLab authentication",
                        "jj stack auth gitea setup    Show Gitea setup instructions",
                    ]),
                },
            ],
        },
        _ => return None,
    };
    Some(screen)
}

// ---------------------------------------------------------------------------
// Rendering (pure)
// ---------------------------------------------------------------------------

/// Render a help screen as plain text or ANSI-styled text.
///
/// Returns a string instead of writing to stdout so it is easy to unit test.
pub fn render_help_screen(screen: &HelpScreen, color: ColorWhen) -> String {
    let ansi = color.should_color();
    let mut out = String::new();

    out.push_str(screen.title);
    out.push('\n');

    if let Some(blurb) = screen.blurb {
        out.push('\n');
        out.push_str(blurb);
        out.push('\n');
    }

    if !screen.usage.is_empty() {
        out.push('\n');
        push_heading(&mut out, "Usage:", ansi);
        for line in &screen.usage {
            push_code_line(&mut out, line, ansi);
        }
    }

    for section in &screen.sections {
        out.push('\n');
        push_heading(&mut out, section.heading, ansi);
        match &section.body {
            SectionBody::Entries(entries) => {
                for entry in entries {
                    push_entry(&mut out, entry.label, entry.description, ansi);
                }
            }
            SectionBody::Lines(lines) => {
                for line in lines {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }

    out
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
    // classify_help
    // -----------------------------------------------------------------------

    #[test]
    fn classify_plan_top() {
        assert_eq!(
            classify_help(&args(&["plan", "--help"])),
            Some((HelpTarget::PlanTop, None))
        );
        assert_eq!(
            classify_help(&args(&["plan", "-h"])),
            Some((HelpTarget::PlanTop, None))
        );
    }

    #[test]
    fn classify_plan_subcommand() {
        assert_eq!(
            classify_help(&args(&["plan", "new", "--help"])),
            Some((HelpTarget::PlanSub("new".to_string()), None))
        );
        assert_eq!(
            classify_help(&args(&["plan", "done", "-h"])),
            Some((HelpTarget::PlanSub("done".to_string()), None))
        );
    }

    #[test]
    fn classify_unknown_plan_subcommand_falls_back_to_top() {
        assert_eq!(
            classify_help(&args(&["plan", "bogus", "--help"])),
            Some((HelpTarget::PlanTop, None))
        );
    }

    #[test]
    fn classify_stack_top_and_subcommand() {
        assert_eq!(
            classify_help(&args(&["stack", "--help"])),
            Some((HelpTarget::StackTop, None))
        );
        assert_eq!(
            classify_help(&args(&["stack", "submit", "--help"])),
            Some((HelpTarget::StackSub("submit".to_string()), None))
        );
        assert_eq!(
            classify_help(&args(&["stack", "bogus", "--help"])),
            Some((HelpTarget::StackTop, None))
        );
    }

    #[test]
    fn classify_honors_leading_and_trailing_color() {
        assert_eq!(
            classify_help(&args(&["--color", "always", "plan", "--help"])),
            Some((HelpTarget::PlanTop, Some(ColorWhen::Always)))
        );
        assert_eq!(
            classify_help(&args(&["plan", "--help", "--color=never"])),
            Some((HelpTarget::PlanTop, Some(ColorWhen::Never)))
        );
        assert_eq!(
            classify_help(&args(&["-R", ".", "stack", "submit", "-h"])),
            Some((HelpTarget::StackSub("submit".to_string()), None))
        );
    }

    #[test]
    fn classify_non_help_returns_none() {
        assert_eq!(classify_help(&args(&["--help"])), None); // bare jj --help → real jj
        assert_eq!(classify_help(&args(&["log"])), None);
        assert_eq!(classify_help(&args(&["plan"])), None); // no --help → dispatch, not help
        assert_eq!(classify_help(&args(&["plan", "new", "feat-x"])), None);
    }

    // -----------------------------------------------------------------------
    // Screen models
    // -----------------------------------------------------------------------

    #[test]
    fn plan_new_help_documents_positioning_flags() {
        let text = render_help_screen(&plan_subcommand_help("new").unwrap(), ColorWhen::Never);
        for needle in ["-A", "--insert-after", "-B", "--insert-before", "--stack"] {
            assert!(text.contains(needle), "new help missing {needle}: {text}");
        }
    }

    #[test]
    fn plan_done_and_summary_help_document_flags() {
        let done = render_help_screen(&plan_subcommand_help("done").unwrap(), ColorWhen::Never);
        assert!(done.contains("--show-stripped"));
        assert!(done.contains("--keep-scratch"));

        let summary = render_help_screen(&plan_subcommand_help("summary").unwrap(), ColorWhen::Never);
        assert!(summary.contains("--json"));
        assert!(summary.contains("--stack"));
    }

    #[test]
    fn stack_submit_and_auth_help_exist_and_document_flags() {
        let submit = render_help_screen(&stack_subcommand_help("submit").unwrap(), ColorWhen::Never);
        for needle in ["--draft", "--publish", "--continue-on-error", "--allow-gaps", "--remote"] {
            assert!(submit.contains(needle), "submit help missing {needle}");
        }
        assert!(stack_subcommand_help("auth").is_some());
    }

    #[test]
    fn unknown_subcommands_have_no_screen() {
        assert!(plan_subcommand_help("bogus").is_none());
        assert!(stack_subcommand_help("bogus").is_none());
    }

    #[test]
    fn every_named_subcommand_has_a_screen() {
        for sub in PLAN_SUBCOMMANDS {
            assert!(plan_subcommand_help(sub).is_some(), "no plan screen for {sub}");
        }
        for sub in STACK_SUBCOMMANDS {
            assert!(stack_subcommand_help(sub).is_some(), "no stack screen for {sub}");
        }
    }

    // -----------------------------------------------------------------------
    // Top-level plan help: slimmed but preserves the anchor strings
    // -----------------------------------------------------------------------

    #[test]
    fn plan_help_preserves_anchor_strings() {
        let text = render_help_screen(&build_plan_help(), ColorWhen::Never);
        for needle in [
            "One bookmark = one plan = one PR.",
            "jj plan new <bookmark>",
            "track [bookmark]",
            "untrack <bookmark>",
            "$EDITOR .jj-plan/NN-bookmark.md",
            "`jj status` shows the current plan stack",
            "`jj stack submit/sync/merge`",
            "README.md",
            "MANUAL.md",
            "Commands:",
        ] {
            assert!(text.contains(needle), "plan help missing {needle}");
        }
    }

    #[test]
    fn plan_help_is_slim_no_inline_flag_rows() {
        // The done/summary flag rows were moved into per-subcommand help.
        let text = render_help_screen(&build_plan_help(), ColorWhen::Never);
        assert!(!text.contains("--keep-scratch"), "flag rows should not be in top-level help");
        assert!(!text.contains("--no-diff-stat"), "flag rows should not be in top-level help");
    }

    // -----------------------------------------------------------------------
    // Rendering / color
    // -----------------------------------------------------------------------

    #[test]
    fn render_plain_has_no_ansi() {
        assert!(!render_help_screen(&build_plan_help(), ColorWhen::Never).contains("\x1b["));
        assert!(!render_help_screen(&build_stack_help(), ColorWhen::Never).contains("\x1b["));
    }

    #[test]
    fn render_color_has_ansi() {
        assert!(render_help_screen(&build_plan_help(), ColorWhen::Always).contains("\x1b["));
        assert!(
            render_help_screen(&stack_subcommand_help("submit").unwrap(), ColorWhen::Always)
                .contains("\x1b[")
        );
    }
}
