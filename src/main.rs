use std::collections::BTreeMap;
use std::path::PathBuf;

use jj_plan::commands;
use jj_plan::dispatch::classify_args;
use jj_plan::error::JjPlanError;
use jj_plan::flush;
use jj_plan::jj_binary::JjBinary;
use jj_plan::plan_dir::{find_repo_root_from, resolve_plan_dir, resolved_stack_format, PlanDir};
use jj_plan::plan_file;
use jj_plan::plan_registry::load_registry;
use jj_plan::sync_state;
use jj_plan::types::PlanRegistry;
use jj_plan::workspace;
use jj_plan::wrap;

/// Read-only commands that get zero-overhead passthrough via exec.
///
/// These are canonical command names only. Built-in jj aliases such as `st`,
/// `b`, `desc`, and `op` are normalized by `classify_args()` before dispatch.
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
    "operation",
    "util",
    "git",
    "gerrit",
    "sign",
    "unsign",
    "bookmark",
];

/// Commands that are read-only in jj but intentionally go through the full
/// wrap lifecycle in jj-plan.
///
/// `status` is the coherence seam: users and LLMs run it frequently, so we
/// intentionally flush direct `.jj-plan/` edits to jj descriptions before
/// showing status, then resync afterward.
const COHERENCE_COMMANDS: &[&str] = &["status"];

/// Read-only commands that can render a change *description* and therefore must
/// reflect pending `.jj-plan/` edits. They get the drift gate (flush-only-on-
/// drift) instead of the pure-exec fast path. `diff`/`interdiff` surface trees,
/// not descriptions, so they stay pure passthrough.
const DRIFT_GATED_READONLY: &[&str] = &["log", "show", "evolog"];

fn is_readonly_command(cmd: &str) -> bool {
    READONLY_COMMANDS.contains(&cmd)
}

fn is_coherence_command(cmd: &str) -> bool {
    COHERENCE_COMMANDS.contains(&cmd)
}

fn is_drift_gated_readonly(cmd: &str) -> bool {
    DRIFT_GATED_READONLY.contains(&cmd)
}

/// Outcome of the read-path drift decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadGate {
    /// No coherence work needed — `exec` the real jj directly (no jj-lib open).
    ExecDirect,
    /// Plan files drifted — flush them to descriptions, then `exec`.
    FlushThenExec,
}

/// Pure decision for the read-path drift gate.
///
/// Flush only when jj-plan is activated, not in the error state, and the plan
/// files have drifted since the last sync. Every other case is a direct exec.
fn decide_read_gate(activated: bool, error_state: bool, drifted: bool) -> ReadGate {
    if activated && !error_state && drifted {
        ReadGate::FlushThenExec
    } else {
        ReadGate::ExecDirect
    }
}

/// Everything the drift gate needs once jj-plan is confirmed activated.
struct DriftGateContext {
    repo_root: PathBuf,
    plan_dir: PlanDir,
    registry: PlanRegistry,
    error_state: bool,
    drifted: bool,
    /// Per-bookmark baselines from the last sync (empty if none/old version). Used to
    /// `anchor` after a flush so a failed flush cannot poison the baseline.
    prev_baselines: BTreeMap<String, String>,
}

/// GATHER: resolve repo + plan dir and compute the cheap drift signal.
///
/// Returns `None` when jj-plan is not activated for the target repo (caller
/// then execs unchanged). Opens **no** jj-lib — only stats and small file reads.
fn gather_drift_gate_context(repository_override: Option<&str>) -> Option<DriftGateContext> {
    let repo_start = resolve_repo_start_path(repository_override)?;
    let repo_root = find_repo_root_from(&repo_start)?;
    let plan_dir = resolve_plan_dir(Some(&repo_root))?;

    let error_state = plan_file::is_error_state(&plan_dir.path);
    let registry = load_registry(&repo_root);

    // In the error state we never flush (matches flush_all), so skip the
    // file reads entirely — `drifted` is irrelevant.
    let (prev_baselines, drifted) = if error_state {
        (BTreeMap::new(), false)
    } else {
        let current = sync_state::current_file_hashes(&plan_dir.path, &registry);
        let stored = sync_state::load_sync_state(&repo_root);
        let drifted = sync_state::is_drifted(&current, stored.as_ref());
        let prev_baselines = stored.map(|s| s.baselines).unwrap_or_default();
        (prev_baselines, drifted)
    };

    Some(DriftGateContext {
        repo_root,
        plan_dir,
        registry,
        error_state,
        drifted,
        prev_baselines,
    })
}

/// Handle a description-surfacing read-only command (`log`/`show`/`evolog`).
///
/// Fast path (the common case): a few stats + small file reads decide there's no
/// drift, and we `exec` the real jj with zero jj-lib overhead. Only on actual
/// drift do we open jj-lib, flush plan files to descriptions, refresh the
/// sidecar, and then `exec`. The command's own stdout/stderr/exit are always the
/// real jj's — the flush is silent and `exec` replaces this process.
fn run_drift_gated_readonly(
    jj: &JjBinary,
    full_args: &[String],
    repository_override: Option<&str>,
) -> jj_plan::error::Result<i32> {
    let ctx = gather_drift_gate_context(repository_override);

    let gate = match &ctx {
        Some(c) => decide_read_gate(true, c.error_state, c.drifted),
        None => decide_read_gate(false, false, false),
    };

    if let (ReadGate::FlushThenExec, Some(c)) = (gate, &ctx) {
        // Degrade to a plain exec if jj-lib can't open (e.g. version mismatch).
        if let Some(mut workspace) = workspace::Workspace::open(&c.repo_root) {
            flush::flush_all(&c.plan_dir.path, jj, &workspace, &c.registry);
            // Re-read post-flush descriptions and advance baselines only for bookmarks now
            // confirmed equal — a failed flush leaves file != desc and is NOT recorded
            // (no poisoning). This replaces the old "save the file digest unconditionally".
            workspace.reload();
            let observed = flush::observe(&c.plan_dir.path, &workspace, &c.registry);
            let next = sync_state::anchor(&c.prev_baselines, &observed);
            let _ = sync_state::save_sync_state(&c.repo_root, &sync_state::SyncState::new(next));
        }
    }

    jj.exec_strings(full_args)?;
    unreachable!("exec replaces the process");
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

fn resolve_repo_start_path(repository_override: Option<&str>) -> Option<PathBuf> {
    match repository_override {
        Some(path) => {
            let override_path = PathBuf::from(path);
            if override_path.is_absolute() {
                Some(override_path)
            } else {
                std::env::current_dir()
                    .ok()
                    .map(|cwd| cwd.join(override_path))
            }
        }
        None => std::env::current_dir().ok(),
    }
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

    let invocation = classify_args(args);
    let subcommand = match invocation.command.as_deref() {
        Some(command) => command,
        None => {
            jj.exec_strings(args)?;
            unreachable!("exec replaces the process");
        }
    };

    let full_args = args;
    let cmd_args = &args[invocation.command_index..];

    // Description-surfacing read-only commands (log/show/evolog) take the drift
    // gate: a cheap content-hash check that flushes pending plan-file edits (and
    // opens jj-lib) only when they actually changed — otherwise pure exec.
    if is_drift_gated_readonly(subcommand) {
        return run_drift_gated_readonly(jj, full_args, invocation.repository_override.as_deref());
    }

    // Other read-only commands go straight to the real jj binary. Coherence
    // commands are intentionally excluded from this fast path.
    if !is_coherence_command(subcommand) && is_readonly_command(subcommand) {
        jj.exec_strings(full_args)?;
        unreachable!("exec replaces the process");
    }

    // Workspace subcommand routing: read-only subs (list, root) get exec
    // passthrough; mutating subs (update-stale, add, forget, rename) fall
    // through to wrap::wrap() for flush/sync.
    if subcommand == "workspace" && is_workspace_readonly(cmd_args) {
        jj.exec_strings(full_args)?;
        unreachable!("exec replaces the process");
    }

    let repo_start = match resolve_repo_start_path(invocation.repository_override.as_deref()) {
        Some(path) => path,
        None => {
            jj.exec_strings(full_args)?;
            unreachable!("exec replaces the process");
        }
    };

    // Resolve repo root — if not in a repo (or the explicit -R target is not
    // a repo), passthrough and let the real jj binary report the error.
    let repo_root = match find_repo_root_from(&repo_start) {
        Some(root) => root,
        None => {
            jj.exec_strings(full_args)?;
            unreachable!("exec replaces the process");
        }
    };

    // Resolve plan directory — if not activated, passthrough except for
    // jj-plan-only commands (`plan`, `stack`), which get an activation message.
    let plan_dir = match resolve_plan_dir(Some(&repo_root)) {
        Some(pd) => pd,
        None => {
            if matches!(subcommand, "plan" | "stack") {
                eprintln!("jj-plan is not activated in this repository.");
                eprintln!();
                eprintln!("To activate:");
                eprintln!("  echo '.jj-plan' >> .gitignore");
                eprintln!("  mkdir .jj-plan");
                return Ok(1);
            }
            jj.exec_strings(full_args)?;
            unreachable!("exec replaces the process");
        }
    };

    // Load jj-lib repo for in-process reads.
    // If loading fails, degrade to passthrough — the jj command runs directly
    // without plan sync. This only happens on version mismatch or corrupt repo.
    let mut workspace = match workspace::Workspace::open(&repo_root) {
        Some(w) => w,
        None => {
            eprintln!("jj-plan: warning: could not load repository via jj-lib, running without plan sync");
            jj.exec_strings(full_args)?;
            unreachable!("exec replaces the process");
        }
    };

    // Load plan registry once — all command paths receive this reference.
    let registry = load_registry(&repo_root);

    // GATHER: read stack format preference once at the shell boundary.
    // This is threaded as data through the entire call chain (FC/IS).
    let format = resolved_stack_format();

    // Special handling for "plan" subcommand
    if subcommand == "plan" {
        return commands::dispatch_plan(
            jj,
            &plan_dir,
            &repo_root,
            cmd_args,
            &mut workspace,
            &registry,
            format,
        );
    }

    // Special handling for "stack" subcommand (PR operations)
    if subcommand == "stack" {
        return commands::stack_cmd::dispatch_stack(
            jj,
            &plan_dir,
            cmd_args,
            &mut workspace,
            &registry,
            format,
        );
    }

    // Special handling for "describe" — intercept -m to write to plan file first
    if subcommand == "describe" {
        return commands::describe::handle_describe(
            jj,
            &plan_dir,
            full_args,
            cmd_args,
            &mut workspace,
            &registry,
            format,
        );
    }

    // All other commands: wrap lifecycle (flush → command → reload → sync → show)
    wrap::wrap(&plan_dir, jj, full_args, &mut workspace, &registry, format)
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
    fn status_is_a_coherence_command() {
        assert!(is_coherence_command("status"));
        assert!(!is_coherence_command("log"));
    }

    #[test]
    fn bookmark_is_in_readonly_commands() {
        assert!(is_readonly_command("bookmark"));
        assert!(!is_readonly_command("status"));
    }

    #[test]
    fn drift_gated_set_is_description_surfacing_only() {
        assert!(is_drift_gated_readonly("log"));
        assert!(is_drift_gated_readonly("show"));
        assert!(is_drift_gated_readonly("evolog"));
        // diff/interdiff surface trees, not descriptions — stay pure exec.
        assert!(!is_drift_gated_readonly("diff"));
        assert!(!is_drift_gated_readonly("interdiff"));
        // every drift-gated command is also a read-only command.
        for cmd in DRIFT_GATED_READONLY {
            assert!(is_readonly_command(cmd));
        }
    }

    #[test]
    fn decide_read_gate_truth_table() {
        use ReadGate::*;
        // Only flush when activated AND not error-state AND drifted.
        assert_eq!(decide_read_gate(true, false, true), FlushThenExec);
        assert_eq!(decide_read_gate(true, false, false), ExecDirect);
        assert_eq!(decide_read_gate(true, true, true), ExecDirect); // error state never flushes
        assert_eq!(decide_read_gate(false, false, true), ExecDirect); // not activated
        assert_eq!(decide_read_gate(false, true, false), ExecDirect);
    }
}