use std::path::Path;

use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::workspace::Workspace;

/// Run `jj plan config` — print resolved configuration and stack info.
///
/// This is a read-only introspection command with no flush, no sync, and
/// no side effects.
pub fn run_config(_jj: &JjBinary, plan_dir: &PlanDir, repo_root: &Path, workspace: &Workspace) {
    let self_exe = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(unknown)".to_string());

    println!("jj-plan configuration:");
    println!();
    println!("  shim path:        {}", self_exe);
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

    // Stack info using the new stack builder
    let stack_result = crate::stack_builder::build_stack(workspace);
    match stack_result {
        crate::types::StackResult::Ok(stack) => {
            println!("  stack model:      trunk()..(@  | descendants(@))");
            println!("  stack segments:   {}", stack.segments.len());
            if !stack.gaps.is_empty() {
                println!("  stack gaps:       {} (unbookmarked commits between bookmarks)", stack.gaps.len());
            }
        }
        crate::types::StackResult::MergeCommits => {
            println!("  stack:            (merge commits detected — not supported)");
        }
        crate::types::StackResult::Empty => {
            println!("  stack:            (empty — @ is at trunk)");
        }
    }
}