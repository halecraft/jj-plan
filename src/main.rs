mod commands;
mod error;
mod flush;
mod markdown;
mod jj_binary;
mod plan_dir;
mod plan_file;
mod stack;
mod sync;
mod wrap;

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
        jj.exec_strings(args)?;
        unreachable!("exec replaces the process");
    }

    let subcommand = &args[0];

    // Resolve repo root — if not in a repo, passthrough
    let repo_root = match jj.repo_root() {
        Some(root) => root,
        None => {
            // Not in a jj repo — passthrough (jj will produce its own error)
            jj.exec_strings(args)?;
            unreachable!();
        }
    };

    // Resolve plan directory — if not activated, passthrough
    let plan_dir = match resolve_plan_dir(Some(&repo_root)) {
        Some(pd) => pd,
        None => {
            // No plan directory — not activated, full passthrough
            jj.exec_strings(args)?;
            unreachable!();
        }
    };

    // Special handling for "plan" subcommand
    if subcommand == "plan" {
        return commands::dispatch_plan(jj, &plan_dir, &repo_root, args);
    }

    // Special handling for "abandon" — recover stack bookmark if lost
    if subcommand == "abandon" {
        return commands::abandon::run_abandon(jj, &plan_dir, args);
    }

    // Special handling for "describe" — intercept -m to write to plan file first
    if subcommand == "describe" {
        return commands::describe::handle_describe(jj, &plan_dir, args);
    }

    // All other commands: wrap handler (flush → command → sync → show)
    //
    // Commands like status/st, new, edit, and the general catch-all
    // all go through the full lifecycle:
    //   1. flush_all()  — write local plan file edits to jj descriptions
    //   2. run jj       — execute the actual jj command
    //   3. sync()       — mirror jj state back to plan files
    //   4. show_stack() — display the plan stack summary
    wrap::wrap(&plan_dir, jj, args)
}