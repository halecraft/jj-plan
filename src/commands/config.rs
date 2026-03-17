use std::path::Path;

use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::stack::{self, StackBase};

/// Run `jj plan config` — print resolved configuration and stack info.
///
/// This is a read-only introspection command with no flush, no sync, and
/// no side effects. It prints all resolved configuration as key: value pairs,
/// matching the zsh shim's output format.
///
/// Stack resolution is delegated to `stack::resolve_stack_base()` and
/// `stack::resolve_stack_changes()` — the same code path used by wrap/sync.
/// No duplicate resolution logic.
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

    // Stack info — uses the shared stack resolution from stack.rs
    let base = stack::resolve_stack_base(jj);
    match &base {
        Some(StackBase::Ambiguous(_)) | None => {
            println!("  stack base:       (none)");
            println!("  stack size:       0");
        }
        Some(stack_base) => {
            if let Some((base_str, mode_str)) = stack_base.display_pair() {
                println!("  stack base:       {} ({})", base_str, mode_str);

                let size = stack::resolve_stack_changes(jj, stack_base)
                    .map(|c| c.len())
                    .unwrap_or(0);
                println!("  stack size:       {}", size);
            } else {
                println!("  stack base:       (none)");
                println!("  stack size:       0");
            }
        }
    }
}