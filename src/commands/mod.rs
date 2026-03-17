pub mod abandon;
pub mod config;
pub mod describe;
pub mod done;
pub mod help;
pub mod new;
pub mod stack;

use crate::error::{JjPlanError, Result};
use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;

/// Dispatch `jj plan <subcommand>` to the appropriate handler.
///
/// `args` is the full argument list starting with "plan".
/// For example: `["plan", "config"]` or `["plan", "--help"]`.
pub fn dispatch_plan(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    repo_root: &std::path::Path,
    args: &[String],
) -> Result<i32> {
    // args[0] is "plan", args[1] is the subcommand (if present)
    let subcommand = args.get(1).map(|s| s.as_str());

    match subcommand {
        // --help / -h before subcommand dispatch
        Some("--help" | "-h") => {
            help::print_help();
            Ok(0)
        }

        Some("config") => {
            config::run_config(jj, plan_dir, repo_root);
            Ok(0)
        }

        // plan stack, plan new, plan done — placeholders for jj:swlkutql
        Some("stack") => {
            let sub_args = if args.len() > 2 { &args[2..] } else { &[] };
            stack::run_stack(jj, plan_dir, sub_args)
        }
        Some("new") => {
            let sub_args = if args.len() > 2 { &args[2..] } else { &[] };
            new::run_new(jj, plan_dir, sub_args)
        }
        Some("done") => {
            let sub_args = if args.len() > 2 { &args[2..] } else { &[] };
            done::run_done(jj, plan_dir, sub_args)
        }

        // No subcommand
        None => Err(JjPlanError::PlanMissingSubcommand),

        // Unknown subcommand
        Some(unknown) => Err(JjPlanError::PlanUnknownSubcommand(unknown.to_string())),
    }
}