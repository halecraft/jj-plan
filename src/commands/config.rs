use std::path::Path;

use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::plan_registry;
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

    // PlanRegistry info
    let registry = plan_registry::load_registry(repo_root);
    let registry_file = plan_registry::registry_path(repo_root);
    println!("  registry file:    {}", registry_file.display());
    let tracked = registry.tracked_names();
    println!("  registered plans: {}", tracked.len());
    if !tracked.is_empty() {
        for name in &tracked {
            println!("    - {}", name);
        }
    }
    println!();

    // Stack info using the registry-filtered stack builder
    let stack_result = crate::stack_builder::build_stack(workspace, Some(&registry));
    match stack_result {
        crate::types::StackResult::Ok(stack) => {
            println!("  stack model:      trunk()..(@  | descendants(@))");
            println!("  stack segments:   {}", stack.segments.len());
            if !stack.gaps.is_empty() {
                println!("  stack gaps:       {} (unbookmarked commits between plans)", stack.gaps.len());
            }

            // Show plan bookmarks in stack order
            if !stack.segments.is_empty() {
                println!();
                println!("  plans in stack order:");
                for (i, seg) in stack.segments.iter().enumerate() {
                    let names: Vec<&str> = seg.bookmarks.iter()
                        .filter(|b| registry.is_tracked(&b.name))
                        .map(|b| b.name.as_str())
                        .collect();
                    let display = if names.is_empty() {
                        "(no registered bookmark)".to_string()
                    } else {
                        names.join(", ")
                    };
                    let tip_desc = seg.changes.first()
                        .map(|c| c.description_first_line.as_str())
                        .unwrap_or("");
                    println!("    {}. {} — {}", i + 1, display, tip_desc);
                }
            }
        }

        crate::types::StackResult::Empty => {
            println!("  stack:            (empty — @ is at trunk)");
        }
    }
}