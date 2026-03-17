use std::path::Path;

use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;

/// Run `jj plan config` — print resolved configuration and stack info.
///
/// This is a read-only introspection command with no flush, no sync, and
/// no side effects. It prints all resolved configuration as key: value pairs,
/// matching the zsh shim's output format.
pub fn run_config(jj: &JjBinary, plan_dir: &PlanDir, repo_root: &Path) {
    let self_exe = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(unknown)".to_string());

    println!("jj-plan configuration:");
    println!();
    println!("  shim path:        {}", self_exe);
    println!("  real jj binary:   {}", jj.path().display());
    println!("  repo root:        {}", repo_root.display());
    println!();

    // JJ_PLAN_DIR env display
    match std::env::var("JJ_PLAN_DIR") {
        Ok(val) if !val.is_empty() => {
            println!("  JJ_PLAN_DIR env:  {}", val);
        }
        _ => {
            println!("  JJ_PLAN_DIR env:  (not set)");
        }
    }

    let max = crate::plan_dir::plan_max();
    println!("  JJ_PLAN_MAX env:  {}", max);
    println!();
    println!("  resolved dir:     {}", plan_dir.path.display());
    println!("  resolution source: {}", plan_dir.source);
    println!();

    // Stack info — read-only, no flush/sync
    // Use jj subprocess to resolve stack base (same approach as zsh shim)
    let stack_base = resolve_stack_base_via_jj(jj);
    match stack_base {
        Some((base, mode)) => {
            println!("  stack base:       {} ({})", base, mode);

            let revset = match mode.as_str() {
                "inclusive" => format!("({}::@) | descendants(@)", base),
                _ => format!("({}..@) | descendants(@)", base),
            };
            let size = count_revset_via_jj(jj, &revset);
            println!("  stack size:       {}", size);
        }
        None => {
            println!("  stack base:       (none)");
            println!("  stack size:       0");
        }
    }
}

/// Resolve the stack base by shelling out to jj, returning (base_id, range_mode).
///
/// This mirrors `__jj_plan_resolve_stack_base` from the zsh shim.
/// Returns `Some(("change_id", "inclusive"|"exclusive"))` or `None`.
fn resolve_stack_base_via_jj(jj: &JjBinary) -> Option<(String, String)> {
    // 1. stack / stack/* bookmarks — nearest ancestor of @ (inclusive)
    let revset =
        r#"heads((bookmarks(exact:"stack") | bookmarks(glob:"stack/*")) & ::@)"#.to_string();
    let args = vec![
        "log".to_string(),
        "-r".to_string(),
        revset,
        "-T".to_string(),
        r#"change_id.shortest(8) ++ "\n""#.to_string(),
        "--no-graph".to_string(),
    ];

    if let Ok((status, stdout, _)) = jj.run_silent(&args) {
        if status.success() {
            let heads: Vec<&str> = stdout.trim().lines().filter(|l| !l.is_empty()).collect();
            if heads.len() == 1 {
                return Some((heads[0].to_string(), "inclusive".to_string()));
            }
            // Multiple heads = ambiguous, but for config display we just report none
            if heads.len() > 1 {
                return None;
            }
        }
    }

    // 2. trunk() — if it resolves to something other than root() (exclusive)
    let args = vec![
        "log".to_string(),
        "-r".to_string(),
        "trunk() & ~root()".to_string(),
        "-T".to_string(),
        "change_id".to_string(),
        "--no-graph".to_string(),
    ];

    if let Ok((status, stdout, _)) = jj.run_silent(&args) {
        if status.success() && !stdout.trim().is_empty() {
            return Some(("trunk()".to_string(), "exclusive".to_string()));
        }
    }

    // 3. No usable base
    None
}

/// Count the number of changes matching a revset by shelling out to jj.
fn count_revset_via_jj(jj: &JjBinary, revset: &str) -> usize {
    let args = vec![
        "log".to_string(),
        "-r".to_string(),
        revset.to_string(),
        "-T".to_string(),
        r#""x""#.to_string(),
        "--no-graph".to_string(),
    ];

    match jj.run_silent(&args) {
        Ok((status, stdout, _)) if status.success() => {
            // Each change produces one "x", so count characters
            stdout.trim().len()
        }
        _ => 0,
    }
}