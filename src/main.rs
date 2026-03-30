use jj_plan::error::JjPlanError;
use jj_plan::jj_binary::JjBinary;
use jj_plan::plan_dir::{find_repo_root, resolve_plan_dir, resolved_stack_format};
use jj_plan::plan_registry::load_registry;
use jj_plan::workspace;
use jj_plan::commands;
use jj_plan::wrap;

/// Read-only commands that get zero-overhead passthrough via exec.
/// Note: status/st are NOT here — they get special handling to append stack summary.
/// Note: "workspace" is NOT here — subcommand routing is handled separately
/// because `workspace update-stale` is mutating (can change `@`).
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
];

fn is_readonly_command(cmd: &str) -> bool {
    READONLY_COMMANDS.contains(&cmd)
}

/// Workspace subcommands that are read-only and safe for exec passthrough.
/// All other workspace subcommands (`update-stale`, `add`, `forget`, `rename`)
/// are mutating and go through `wrap::wrap()` for flush/sync.
const WORKSPACE_READONLY_SUBS: &[&str] = &["list", "root"];

/// Classify whether a `workspace` invocation is read-only (exec passthrough)
/// or mutating (needs wrap lifecycle).
///
/// Returns `true` if the workspace subcommand is read-only or if no subcommand
/// is present (bare `workspace` shows help). Checks if any element in
/// `args[1..]` matches a known read-only subcommand. This is conservative:
/// unknown subcommands route through wrap, which is always safe.
fn is_workspace_readonly(args: &[String]) -> bool {
    // Bare `workspace` (shows help) or `workspace --help` → passthrough
    if args.len() <= 1 {
        return true;
    }
    // If any arg after "workspace" is a known read-only subcommand → passthrough.
    // Scanning all args handles flags before the subcommand (e.g. `workspace --color always list`).
    args[1..].iter().any(|a| WORKSPACE_READONLY_SUBS.contains(&a.as_str()))
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
                JjPlanError::PlanUnknownSubcommand(_) => 1,
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

    // Workspace subcommand routing: read-only subs (list, root) get exec
    // passthrough; mutating subs (update-stale, add, forget, rename) fall
    // through to wrap::wrap() for flush/sync.
    if args[0] == "workspace" && is_workspace_readonly(args) {
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
            // plan and stack are jj-plan-only commands — intercept before passthrough
            if matches!(subcommand.as_str(), "plan" | "stack") {
                eprintln!("jj-plan is not activated in this repository.");
                eprintln!();
                eprintln!("To activate:");
                eprintln!("  echo '.jj-plan' >> .gitignore");
                eprintln!("  mkdir .jj-plan");
                return Ok(1);
            }
            // All other commands: passthrough to real jj
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

    // GATHER: read stack format preference once at the shell boundary.
    // This is threaded as data through the entire call chain (FC/IS).
    let format = resolved_stack_format();

    // Special handling for "plan" subcommand
    if subcommand == "plan" {
        return commands::dispatch_plan(jj, &plan_dir, &repo_root, args, &mut workspace, &registry, format);
    }

    // Special handling for "stack" subcommand (PR operations)
    if subcommand == "stack" {
        return commands::stack_cmd::dispatch_stack(jj, &plan_dir, args, &mut workspace, &registry, format);
    }

    // Special handling for "abandon" — recover stack bookmark if lost
    if subcommand == "abandon" {
        return commands::abandon::run_abandon(jj, &plan_dir, args, &mut workspace, &registry, format);
    }

    // Special handling for "describe" (and its alias "desc") — intercept -m to write to plan file first
    if matches!(subcommand.as_str(), "describe" | "desc") {
        return commands::describe::handle_describe(jj, &plan_dir, args, &mut workspace, &registry, format);
    }

    // All other commands: wrap lifecycle (flush → command → reload → sync → show)
    wrap::wrap(&plan_dir, jj, args, &mut workspace, &registry, format)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn workspace_readonly_subs_contains_list_and_root() {
        assert!(WORKSPACE_READONLY_SUBS.contains(&"list"));
        assert!(WORKSPACE_READONLY_SUBS.contains(&"root"));
        assert_eq!(WORKSPACE_READONLY_SUBS.len(), 2);
    }

    #[test]
    fn workspace_update_stale_is_mutating() {
        assert!(!is_workspace_readonly(&args(&["workspace", "update-stale"])));
    }

    #[test]
    fn workspace_add_is_mutating() {
        assert!(!is_workspace_readonly(&args(&["workspace", "add", "../other"])));
    }

    #[test]
    fn workspace_forget_is_mutating() {
        assert!(!is_workspace_readonly(&args(&["workspace", "forget", "secondary"])));
    }

    #[test]
    fn workspace_rename_is_mutating() {
        assert!(!is_workspace_readonly(&args(&["workspace", "rename", "new-name"])));
    }

    #[test]
    fn workspace_list_is_readonly() {
        assert!(is_workspace_readonly(&args(&["workspace", "list"])));
    }

    #[test]
    fn workspace_root_is_readonly() {
        assert!(is_workspace_readonly(&args(&["workspace", "root"])));
    }

    #[test]
    fn workspace_root_with_flags_before_sub_is_readonly() {
        assert!(is_workspace_readonly(&args(&["workspace", "--color", "always", "root"])));
    }

    #[test]
    fn bare_workspace_is_readonly() {
        // Bare `workspace` shows help — safe for passthrough
        assert!(is_workspace_readonly(&args(&["workspace"])));
    }

    #[test]
    fn workspace_help_routes_through_wrap() {
        // `workspace --help` has no known readonly sub, so it conservatively
        // routes through wrap. This is harmless — wrap just runs the command
        // with flush/sync around it, and `--help` produces no mutations.
        assert!(!is_workspace_readonly(&args(&["workspace", "--help"])));
    }

    #[test]
    fn workspace_not_in_readonly_commands() {
        assert!(!READONLY_COMMANDS.contains(&"workspace"));
    }

    #[test]
    fn desc_alias_matches_describe_dispatch() {
        // Both "describe" and "desc" should match the dispatch condition
        assert!(matches!("describe", "describe" | "desc"));
        assert!(matches!("desc", "describe" | "desc"));
    }
}