//! Shared CLI invocation classification for jj-plan.
//!
//! This module owns the shell-boundary parse that identifies:
//! - the real jj subcommand when leading global options are present
//! - built-in jj aliases normalized to canonical command names
//! - leading `--color` override (for help rendering)
//! - leading `-R` / `--repository` override (for repo-context resolution)
//!
//! The parser is intentionally conservative:
//! - known leading globals are skipped
//! - help/version terminators return `command: None`
//! - unknown leading flags also return `command: None`, causing the caller
//!   to passthrough unchanged to the real `jj` binary
//!
//! This keeps jj-plan faithful to jj even when new global flags are added.

/// Standalone jj global flags that consume no following value.
pub const GLOBAL_FLAGS: &[&str] = &[
    "--ignore-working-copy",
    "--ignore-immutable",
    "--debug",
    "--quiet",
    "--no-pager",
];

/// jj global options that consume a following value when written in the
/// separated form, and one token when written as `--opt=value`.
pub const GLOBAL_OPTIONS_WITH_VALUE: &[&str] = &[
    "-R",
    "--repository",
    "--at-operation",
    "--at-op",
    "--color",
    "--config",
    "--config-file",
];

/// jj global flags that terminate normal command dispatch by printing output
/// and exiting immediately.
pub const GLOBAL_TERMINATORS: &[&str] = &["-h", "--help", "-V", "--version"];

/// Built-in jj command aliases normalized by jj-plan.
///
/// User-defined aliases are intentionally not resolved here. Unknown commands
/// are left unchanged and can still be passed through to the real jj binary.
pub const BUILTIN_ALIASES: &[(&str, &str)] = &[
    ("st", "status"),
    ("b", "bookmark"),
    ("ci", "commit"),
    ("desc", "describe"),
    ("op", "operation"),
    ("evolution-log", "evolog"),
];

/// Parsed result of classifying the raw CLI arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedInvocation {
    /// Index of the subcommand in the original args, or `args.len()` if none found.
    pub command_index: usize,
    /// Canonical subcommand name after alias resolution, or `None`.
    pub command: Option<String>,
    /// Raw leading `--color` value, if present before the command.
    pub leading_color: Option<String>,
    /// Raw leading `-R` / `--repository` value, if present before the command.
    pub repository_override: Option<String>,
}

/// Classify a raw jj invocation.
///
/// This function scans only the leading global options. Once the first real
/// subcommand token is found, parsing stops and the command is normalized
/// through the built-in alias table.
///
/// Conservative behavior:
/// - `-h`, `--help`, `-V`, `--version` return `command: None`
/// - unknown leading flags return `command: None`
/// - empty args return `command: None`
pub fn classify_args(args: &[String]) -> ParsedInvocation {
    let mut idx = 0;
    let mut leading_color: Option<String> = None;
    let mut repository_override: Option<String> = None;

    while idx < args.len() {
        let arg = args[idx].as_str();

        if GLOBAL_TERMINATORS.contains(&arg) {
            return ParsedInvocation {
                command_index: args.len(),
                command: None,
                leading_color,
                repository_override,
            };
        }

        if GLOBAL_FLAGS.contains(&arg) {
            idx += 1;
            continue;
        }

        if arg == "--color" {
            let Some(value) = args.get(idx + 1) else {
                return none_found(args.len(), leading_color, repository_override);
            };
            leading_color = Some(value.clone());
            idx += 2;
            continue;
        }

        if let Some(value) = arg.strip_prefix("--color=") {
            leading_color = Some(value.to_string());
            idx += 1;
            continue;
        }

        if arg == "-R" || arg == "--repository" {
            let Some(value) = args.get(idx + 1) else {
                return none_found(args.len(), leading_color, repository_override);
            };
            repository_override = Some(value.clone());
            idx += 2;
            continue;
        }

        if let Some(value) = arg.strip_prefix("--repository=") {
            repository_override = Some(value.to_string());
            idx += 1;
            continue;
        }

        if arg.starts_with("-R") && !arg.starts_with("--") && arg.len() > 2 {
            repository_override = Some(arg[2..].to_string());
            idx += 1;
            continue;
        }

        if consumes_value_option(arg) {
            let Some(_value) = args.get(idx + 1) else {
                return none_found(args.len(), leading_color, repository_override);
            };
            idx += 2;
            continue;
        }

        if is_equals_form_value_option(arg) {
            idx += 1;
            continue;
        }

        if arg.starts_with('-') {
            return none_found(args.len(), leading_color, repository_override);
        }

        return ParsedInvocation {
            command_index: idx,
            command: Some(resolve_builtin_alias(arg).to_string()),
            leading_color,
            repository_override,
        };
    }

    none_found(args.len(), leading_color, repository_override)
}

fn none_found(
    command_index: usize,
    leading_color: Option<String>,
    repository_override: Option<String>,
) -> ParsedInvocation {
    ParsedInvocation {
        command_index,
        command: None,
        leading_color,
        repository_override,
    }
}

fn resolve_builtin_alias(command: &str) -> &str {
    BUILTIN_ALIASES
        .iter()
        .find_map(|(alias, canonical)| (*alias == command).then_some(*canonical))
        .unwrap_or(command)
}

fn consumes_value_option(arg: &str) -> bool {
    matches!(
        arg,
        "--at-operation" | "--at-op" | "--config" | "--config-file"
    )
}

fn is_equals_form_value_option(arg: &str) -> bool {
    arg.starts_with("--at-operation=")
        || arg.starts_with("--at-op=")
        || arg.starts_with("--config=")
        || arg.starts_with("--config-file=")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_args_returns_none() {
        assert_eq!(
            classify_args(&[]),
            ParsedInvocation {
                command_index: 0,
                command: None,
                leading_color: None,
                repository_override: None,
            }
        );
    }

    #[test]
    fn plain_command_at_index_zero() {
        let parsed = classify_args(&args(&["log"]));
        assert_eq!(parsed.command_index, 0);
        assert_eq!(parsed.command.as_deref(), Some("log"));
        assert_eq!(parsed.leading_color, None);
        assert_eq!(parsed.repository_override, None);
    }

    #[test]
    fn skips_no_pager_before_command() {
        let parsed = classify_args(&args(&["--no-pager", "log"]));
        assert_eq!(parsed.command_index, 1);
        assert_eq!(parsed.command.as_deref(), Some("log"));
    }

    #[test]
    fn captures_separate_color_before_command() {
        let parsed = classify_args(&args(&["--color", "never", "status"]));
        assert_eq!(parsed.command_index, 2);
        assert_eq!(parsed.command.as_deref(), Some("status"));
        assert_eq!(parsed.leading_color.as_deref(), Some("never"));
    }

    #[test]
    fn captures_equals_color_before_command() {
        let parsed = classify_args(&args(&["--color=never", "log"]));
        assert_eq!(parsed.command_index, 1);
        assert_eq!(parsed.command.as_deref(), Some("log"));
        assert_eq!(parsed.leading_color.as_deref(), Some("never"));
    }

    #[test]
    fn captures_last_color_when_repeated() {
        let parsed = classify_args(&args(&["--color=always", "--color", "never", "log"]));
        assert_eq!(parsed.command_index, 3);
        assert_eq!(parsed.command.as_deref(), Some("log"));
        assert_eq!(parsed.leading_color.as_deref(), Some("never"));
    }

    #[test]
    fn skips_repository_separate_form() {
        let parsed = classify_args(&args(&["-R", "/path", "show"]));
        assert_eq!(parsed.command_index, 2);
        assert_eq!(parsed.command.as_deref(), Some("show"));
        assert_eq!(parsed.repository_override.as_deref(), Some("/path"));
    }

    #[test]
    fn captures_repository_short_joined_form() {
        let parsed = classify_args(&args(&["-R/path", "show"]));
        assert_eq!(parsed.command_index, 1);
        assert_eq!(parsed.command.as_deref(), Some("show"));
        assert_eq!(parsed.repository_override.as_deref(), Some("/path"));
    }

    #[test]
    fn captures_repository_long_separate_form() {
        let parsed = classify_args(&args(&["--repository", "../other-repo", "log"]));
        assert_eq!(parsed.command_index, 2);
        assert_eq!(parsed.command.as_deref(), Some("log"));
        assert_eq!(
            parsed.repository_override.as_deref(),
            Some("../other-repo")
        );
    }

    #[test]
    fn captures_repository_long_equals_form() {
        let parsed = classify_args(&args(&["--repository=../other-repo", "log"]));
        assert_eq!(parsed.command_index, 1);
        assert_eq!(parsed.command.as_deref(), Some("log"));
        assert_eq!(
            parsed.repository_override.as_deref(),
            Some("../other-repo")
        );
    }

    #[test]
    fn skips_at_op_and_config_like_globals() {
        let parsed = classify_args(&args(&[
            "--at-op",
            "@-",
            "--config",
            "ui.color=never",
            "--config-file=/tmp/c.toml",
            "diff",
        ]));
        assert_eq!(parsed.command_index, 5);
        assert_eq!(parsed.command.as_deref(), Some("diff"));
    }

    #[test]
    fn skips_at_operation_equals_form() {
        let parsed = classify_args(&args(&["--at-operation=@-", "diff"]));
        assert_eq!(parsed.command_index, 1);
        assert_eq!(parsed.command.as_deref(), Some("diff"));
    }

    #[test]
    fn multiple_leading_globals_capture_color_and_repo() {
        let parsed = classify_args(&args(&[
            "--no-pager",
            "--color",
            "never",
            "-R",
            ".",
            "describe",
            "-m",
            "x",
        ]));
        assert_eq!(parsed.command_index, 5);
        assert_eq!(parsed.command.as_deref(), Some("describe"));
        assert_eq!(parsed.leading_color.as_deref(), Some("never"));
        assert_eq!(parsed.repository_override.as_deref(), Some("."));
    }

    #[test]
    fn standalone_flags_can_be_stacked() {
        let parsed = classify_args(&args(&["--ignore-working-copy", "--quiet", "log"]));
        assert_eq!(parsed.command_index, 2);
        assert_eq!(parsed.command.as_deref(), Some("log"));
    }

    #[test]
    fn built_in_alias_st_normalizes_to_status() {
        let parsed = classify_args(&args(&["st"]));
        assert_eq!(parsed.command.as_deref(), Some("status"));
    }

    #[test]
    fn built_in_alias_b_normalizes_to_bookmark() {
        let parsed = classify_args(&args(&["b"]));
        assert_eq!(parsed.command.as_deref(), Some("bookmark"));
    }

    #[test]
    fn built_in_alias_desc_normalizes_to_describe() {
        let parsed = classify_args(&args(&["desc"]));
        assert_eq!(parsed.command.as_deref(), Some("describe"));
    }

    #[test]
    fn built_in_alias_ci_normalizes_to_commit() {
        let parsed = classify_args(&args(&["ci"]));
        assert_eq!(parsed.command.as_deref(), Some("commit"));
    }

    #[test]
    fn built_in_alias_op_normalizes_to_operation() {
        let parsed = classify_args(&args(&["op"]));
        assert_eq!(parsed.command.as_deref(), Some("operation"));
    }

    #[test]
    fn built_in_alias_evolution_log_normalizes_to_evolog() {
        let parsed = classify_args(&args(&["evolution-log"]));
        assert_eq!(parsed.command.as_deref(), Some("evolog"));
    }

    #[test]
    fn aliases_also_work_after_globals() {
        let parsed = classify_args(&args(&["--no-pager", "st"]));
        assert_eq!(parsed.command_index, 1);
        assert_eq!(parsed.command.as_deref(), Some("status"));
    }

    #[test]
    fn help_terminator_returns_none() {
        let parsed = classify_args(&args(&["--help"]));
        assert_eq!(parsed.command, None);
        assert_eq!(parsed.command_index, 1);
    }

    #[test]
    fn short_help_terminator_returns_none() {
        let parsed = classify_args(&args(&["-h"]));
        assert_eq!(parsed.command, None);
        assert_eq!(parsed.command_index, 1);
    }

    #[test]
    fn version_terminators_return_none() {
        let parsed_short = classify_args(&args(&["-V"]));
        assert_eq!(parsed_short.command, None);
        assert_eq!(parsed_short.command_index, 1);

        let parsed_long = classify_args(&args(&["--version"]));
        assert_eq!(parsed_long.command, None);
        assert_eq!(parsed_long.command_index, 1);
    }

    #[test]
    fn help_after_globals_returns_none_but_preserves_context() {
        let parsed = classify_args(&args(&["--no-pager", "--color", "never", "--help"]));
        assert_eq!(parsed.command, None);
        assert_eq!(parsed.command_index, 4);
        assert_eq!(parsed.leading_color.as_deref(), Some("never"));
    }

    #[test]
    fn unknown_leading_flag_returns_none_for_conservative_passthrough() {
        let parsed = classify_args(&args(&["--unknown-flag", "log"]));
        assert_eq!(parsed.command, None);
        assert_eq!(parsed.command_index, 2);
    }

    #[test]
    fn malformed_value_option_without_value_returns_none() {
        let parsed = classify_args(&args(&["--repository"]));
        assert_eq!(parsed.command, None);
        assert_eq!(parsed.command_index, 1);
    }

    #[test]
    fn jj_plan_commands_are_left_unchanged() {
        let plan = classify_args(&args(&["plan"]));
        assert_eq!(plan.command.as_deref(), Some("plan"));

        let stack = classify_args(&args(&["stack"]));
        assert_eq!(stack.command.as_deref(), Some("stack"));
    }

    #[test]
    fn jj_plan_command_after_globals_uses_correct_index() {
        let parsed = classify_args(&args(&["--no-pager", "plan", "summary"]));
        assert_eq!(parsed.command_index, 1);
        assert_eq!(parsed.command.as_deref(), Some("plan"));
    }
}