pub mod abandon;
pub mod config;
pub mod describe;
pub mod done;
pub mod help;
pub mod nav;
pub mod new;
pub mod stack_cmd;
pub mod track;
pub mod untrack;

use crate::error::{JjPlanError, Result};
use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::stack_render::StackFormat;
use crate::types::PlanRegistry;
use crate::workspace::Workspace;

/// Returns true if any element in `args` is `"--help"` or `"-h"`.
///
/// Used to intercept help requests in subcommand args before they reach
/// handlers that would interpret them as passthrough flags to jj.
fn sub_args_request_help(args: &[String]) -> bool {
    args.iter().any(|a| a == "--help" || a == "-h")
}

/// Dispatch `jj plan <subcommand>` to the appropriate handler.
///
/// `args` is the full argument list starting with "plan".
/// For example: `["plan", "config"]` or `["plan", "--help"]`.
pub fn dispatch_plan(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    repo_root: &std::path::Path,
    args: &[String],
    workspace: &mut Workspace,
    registry: &PlanRegistry,
    format: StackFormat,
) -> Result<i32> {
    // args[0] is "plan", args[1] is the subcommand (if present)
    let subcommand = args.get(1).map(|s| s.as_str());

    // Intercept `--help` / `-h` as the subcommand itself (e.g. `jj plan --help`)
    if matches!(subcommand, Some("--help" | "-h")) {
        help::print_help();
        return Ok(0);
    }

    // Central help guard: if any sub_args after the subcommand contain
    // --help / -h, show the top-level plan help and exit with no side effects.
    // This uniformly covers stack, new, done, go, next, prev, and config
    // without requiring each handler to check for --help itself.
    let sub_args = if args.len() > 2 { &args[2..] } else { &[] as &[String] };
    if sub_args_request_help(sub_args) {
        help::print_help();
        return Ok(0);
    }

    match subcommand {
        Some("config") => {
            config::run_config(jj, plan_dir, repo_root, workspace);
            Ok(0)
        }

        Some("new") => {
            new::run_new(jj, plan_dir, sub_args, workspace, registry, format)
        }
        Some("track") => {
            track::run_track(jj, plan_dir, sub_args, workspace, registry, format)
        }
        Some("untrack") => {
            untrack::run_untrack(jj, plan_dir, sub_args, workspace, registry, format)
        }
        Some("done") => {
            done::run_done(jj, plan_dir, sub_args, workspace, registry, format)
        }

        Some("next") => nav::plan_next(jj, plan_dir, workspace, registry, format),
        Some("prev") => nav::plan_prev(jj, plan_dir, workspace, registry, format),
        Some("go") => {
            let target = args.get(2).map(|s| s.as_str());
            match target {
                Some(t) => nav::plan_go(jj, plan_dir, t, workspace, registry, format),
                None => {
                    eprintln!("jj plan go: missing target (index, bookmark name, or change ID)");
                    Ok(1)
                }
            }
        }

        // No subcommand
        None => Err(JjPlanError::PlanMissingSubcommand),

        // Unknown subcommand
        Some(unknown) => Err(JjPlanError::PlanUnknownSubcommand(unknown.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sub_args_request_help_with_long_flag() {
        assert!(sub_args_request_help(&args(&["--help"])));
    }

    #[test]
    fn sub_args_request_help_with_short_flag() {
        assert!(sub_args_request_help(&args(&["-h"])));
    }

    #[test]
    fn sub_args_request_help_mixed_with_other_flags() {
        assert!(sub_args_request_help(&args(&["--first", "--help"])));
    }

    #[test]
    fn sub_args_request_help_no_help_flag() {
        assert!(!sub_args_request_help(&args(&["--first"])));
    }

    #[test]
    fn sub_args_request_help_empty() {
        assert!(!sub_args_request_help(&args(&[])));
    }
}