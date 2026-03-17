mod commands;
mod error;
mod jj_binary;
mod plan_dir;

use error::JjPlanError;
use jj_binary::JjBinary;
use plan_dir::resolve_plan_dir;

/// Read-only commands that get zero-overhead passthrough via exec.
/// Note: status/st are NOT here — they get special handling to append .stack.
const READONLY_COMMANDS: &[&str] = &[
    "log",
    "diff",
    "show",
    "interdiff",
    "evolog",
    "file",
    "config",
    "help",
    "version",
    "root",
    "tag",
    "op",
    "operation",
    "util",
    "git",
    "gerrit",
    "sign",
    "unsign",
    "workspace",
];

fn is_readonly_command(cmd: &str) -> bool {
    READONLY_COMMANDS.contains(&cmd)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let jj = match JjBinary::resolve() {
        Ok(jj) => jj,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    let exit_code = match run(&jj, &args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{}", e);
            match e {
                JjPlanError::PlanMissingSubcommand | JjPlanError::PlanUnknownSubcommand(_) => 1,
                _ => 1,
            }
        }
    };

    std::process::exit(exit_code);
}

fn run(jj: &JjBinary, args: &[String]) -> error::Result<i32> {
    // No args or read-only command → zero-overhead passthrough via exec
    if args.is_empty() || is_readonly_command(&args[0]) {
        jj.exec(args)?;
        unreachable!("exec replaces the process");
    }

    let subcommand = &args[0];

    // Resolve repo root — if not in a repo, passthrough
    let repo_root = match jj.repo_root() {
        Some(root) => root,
        None => {
            // Not in a jj repo — passthrough (jj will produce its own error)
            jj.exec(args)?;
            unreachable!();
        }
    };

    // Resolve plan directory — if not activated, passthrough
    let plan_dir = match resolve_plan_dir(Some(&repo_root)) {
        Some(pd) => pd,
        None => {
            // No plan directory — not activated, full passthrough
            jj.exec(args)?;
            unreachable!();
        }
    };

    // Special handling for "plan" subcommand
    if subcommand == "plan" {
        return commands::dispatch_plan(jj, &plan_dir, &repo_root, args);
    }

    // Special handling for "abandon" — placeholder for jj:swlkutql
    // For now, route through the general wrap handler
    // (abandon recovery will be implemented in jj:swlkutql)

    // All other commands: wrap handler (flush → command → sync → show)
    //
    // In this scaffold phase, we only have passthrough — the full
    // flush/sync lifecycle is implemented in jj:uyooozox. For now,
    // we run jj with inherited stdio and return its exit code.
    //
    // Commands like status/st, new, edit, describe, abandon, and the
    // general catch-all all go through this path.
    wrap_passthrough(jj, args)
}

/// Temporary wrap handler that just runs jj with inherited stdio.
///
/// This is the scaffold version — it does NOT flush or sync plan files.
/// The full flush→command→sync→show lifecycle is implemented in jj:uyooozox.
///
/// Once jj:uyooozox lands, this function is replaced by the real wrap handler
/// that calls flush_all() before the command and sync()+show_stack() after.
fn wrap_passthrough(jj: &JjBinary, args: &[String]) -> error::Result<i32> {
    let status = jj.run_inherit(args)?;
    Ok(status.code().unwrap_or(1))
}