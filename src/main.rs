use jj_plan::error::JjPlanError;
use jj_plan::jj_binary::JjBinary;
use jj_plan::plan_dir::{find_repo_root, resolve_plan_dir};
use jj_plan::plan_registry::load_registry;
use jj_plan::workspace;
use jj_plan::commands;
use jj_plan::wrap;

/// Read-only commands that get zero-overhead passthrough via exec.
/// Note: status/st are NOT here — they get special handling to append stack summary.
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

fn run(jj: &JjBinary, args: &[String]) -> jj_plan::error::Result<i32> {
    // Top-level `plan --help` should work even before repo activation checks
    // and should recognize jj-style global options such as `--color`.
    if let commands::help::InvocationKind::PlanHelp(_) =
        commands::help::classify_invocation(args)
    {
        commands::help::print_help();
        return Ok(0);
    }

    // No args or read-only command → zero-overhead passthrough via exec
    if args.is_empty() || is_readonly_command(&args[0]) {
        jj.exec_strings(args)?;
        unreachable!("exec replaces the process");
    }

    let subcommand = &args[0];

    // Resolve repo root — if not in a repo, passthrough
    let repo_root = match find_repo_root() {
        Some(root) => root,
        None => {
            jj.exec_strings(args)?;
            unreachable!();
        }
    };

    // Resolve plan directory — if not activated, passthrough
    let plan_dir = match resolve_plan_dir(Some(&repo_root)) {
        Some(pd) => pd,
        None => {
            jj.exec_strings(args)?;
            unreachable!();
        }
    };

    // Load jj-lib repo for in-process reads.
    // If loading fails, degrade to passthrough — the jj command runs directly
    // without plan sync. This only happens on version mismatch or corrupt repo.
    let mut workspace = match workspace::Workspace::open(&repo_root) {
        Some(w) => w,
        None => {
            eprintln!("jj-plan: warning: could not load repository via jj-lib, running without plan sync");
            jj.exec_strings(args)?;
            unreachable!();
        }
    };

    // Load plan registry once — all command paths receive this reference.
    let registry = load_registry(&repo_root);

    // Special handling for "plan" subcommand
    if subcommand == "plan" {
        return commands::dispatch_plan(jj, &plan_dir, &repo_root, args, &mut workspace, &registry);
    }

    // Special handling for "stack" subcommand (PR operations)
    if subcommand == "stack" {
        return commands::stack_cmd::dispatch_stack(jj, &plan_dir, args, &mut workspace, &registry);
    }

    // Special handling for "abandon" — recover stack bookmark if lost
    if subcommand == "abandon" {
        return commands::abandon::run_abandon(jj, &plan_dir, args, &mut workspace, &registry);
    }

    // Special handling for "describe" — intercept -m to write to plan file first
    if subcommand == "describe" {
        return commands::describe::handle_describe(jj, &plan_dir, args, &mut workspace, &registry);
    }

    // All other commands: wrap lifecycle (flush → command → reload → sync → show)
    wrap::wrap(&plan_dir, jj, args, &mut workspace, &registry)
}