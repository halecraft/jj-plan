pub mod config;
pub mod help;

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
            // TODO(jj:swlkutql): implement plan stack
            eprintln!("jj plan stack: not yet implemented in Rust binary");
            Ok(1)
        }
        Some("new") => {
            // TODO(jj:swlkutql): implement plan new
            eprintln!("jj plan new: not yet implemented in Rust binary");
            Ok(1)
        }
        Some("done") => {
            // TODO(jj:swlkutql): implement plan done
            eprintln!("jj plan done: not yet implemented in Rust binary");
            Ok(1)
        }

        // No subcommand
        None => Err(JjPlanError::PlanMissingSubcommand),

        // Unknown subcommand
        Some(unknown) => Err(JjPlanError::PlanUnknownSubcommand(unknown.to_string())),
    }
}