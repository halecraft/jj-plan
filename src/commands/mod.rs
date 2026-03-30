pub mod abandon;
pub mod config;
pub mod describe;
pub mod done;
pub mod help;
pub mod nav;
pub mod new;
pub mod stack_cmd;
pub mod summary;
pub mod track;
pub mod untrack;

use crate::error::{JjPlanError, Result};
use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::stack_render::StackFormat;
use crate::types::PlanRegistry;
use crate::wrap::SyncChangeView;
use crate::workspace::Workspace;

/// Returns true if any element in `args` is `"--help"` or `"-h"`.
///
/// Used to intercept help requests in subcommand args before they reach
/// handlers that would interpret them as passthrough flags to jj.
fn sub_args_request_help(args: &[String]) -> bool {
    args.iter().any(|a| a == "--help" || a == "-h")
}

/// Resolve the tracked plan bookmark name at a given revision target.
///
/// Returns `Some(bookmark_name)` if the target resolves to a change ID
/// that has a tracked bookmark in the registry. Returns `None` otherwise.
///
/// Used by `dispatch_plan` (orientation check) and `summary::run_summary`
/// (bookmark resolution) to avoid duplicating the registry scan pattern.
pub fn resolve_plan_bookmark_at(
    workspace: &Workspace,
    registry: &PlanRegistry,
    target: &str,
) -> Option<String> {
    let change_id = workspace.resolve_change_id(target)?;
    registry
        .bookmarks
        .iter()
        .find(|b| {
            workspace
                .short_change_id_from_hex(&b.change_id)
                .as_deref()
                == Some(change_id.as_str())
        })
        .map(|b| b.name.clone())
}

/// Dispatch `jj plan <subcommand>` to the appropriate handler.
///
/// `args` is the full argument list starting with "plan".
/// For example: `["plan", "config"]` or `["plan", "--help"]`.
pub fn dispatch_plan(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    repo_root: &std::path::Path,
    args: &[String],
    workspace: &mut Workspace,
    registry: &PlanRegistry,
    format: StackFormat,
) -> Result<i32> {
    // args[0] is "plan", args[1] is the subcommand (if present)
    let subcommand = args.get(1).map(|s| s.as_str());

    // Intercept `--help` / `-h` as the subcommand itself (e.g. `jj plan --help`)
    if matches!(subcommand, Some("--help" | "-h")) {
        help::print_help();
        return Ok(0);
    }

    // Central help guard: if any sub_args after the subcommand contain
    // --help / -h, show the top-level plan help and exit with no side effects.
    // This uniformly covers stack, new, done, go, next, prev, and config
    // without requiring each handler to check for --help itself.
    let sub_args = if args.len() > 2 { &args[2..] } else { &[] as &[String] };
    if sub_args_request_help(sub_args) {
        help::print_help();
        return Ok(0);
    }

    match subcommand {
        Some("config") => {
            config::run_config(jj, plan_dir, repo_root, workspace);
            Ok(0)
        }

        Some("new") => {
            new::run_new(jj, plan_dir, sub_args, workspace, registry, format)
        }
        Some("track") => {
            track::run_track(jj, plan_dir, sub_args, workspace, registry, format)
        }
        Some("untrack") => {
            untrack::run_untrack(jj, plan_dir, sub_args, workspace, registry, format)
        }
        Some("done") => {
            done::run_done(jj, plan_dir, sub_args, workspace, registry, format)
        }
        Some("summary") => {
            summary::run_summary(jj, plan_dir, sub_args, workspace, registry, format)
        }

        Some("next") => nav::plan_next(jj, plan_dir, workspace, registry, format),
        Some("prev") => nav::plan_prev(jj, plan_dir, workspace, registry, format),
        Some("go") => {
            let target = args.get(2).map(|s| s.as_str());
            match target {
                Some(t) => nav::plan_go(jj, plan_dir, t, workspace, registry, format),
                None => {
                    eprintln!("jj plan go: missing target (index, bookmark name, or change ID)");
                    Ok(1)
                }
            }
        }

        // No subcommand — show summary if @ is a plan, orientation otherwise
        None => {
            if resolve_plan_bookmark_at(workspace, registry, "@").is_some() {
                return summary::run_summary(jj, plan_dir, &[], workspace, registry, format);
            }

            // GATHER: @ is not a tracked plan — build orientation
            let change_id = workspace
                .resolve_change_id("@")
                .unwrap_or_else(|| "?".to_string());
            let stack_plans = crate::wrap::build_sync_views(workspace, registry);

            // PLAN + EXECUTE
            let msg = build_orientation_message(&change_id, stack_plans.as_deref());
            eprint!("{}", msg);
            Ok(0)
        }

        // Unknown subcommand
        Some(unknown) => Err(JjPlanError::PlanUnknownSubcommand(unknown.to_string())),
    }
}

/// Build an orientation message for when `@` is not a tracked plan.
///
/// Pure PLAN function — takes gathered data, returns formatted message.
/// Printed to stderr by the dispatch caller.
fn build_orientation_message(change_id: &str, stack_plans: Option<&[SyncChangeView]>) -> String {
    let mut out = String::new();

    let plans: &[SyncChangeView] = stack_plans.unwrap_or(&[]);

    if plans.is_empty() {
        out.push_str(&format!("No plan at @ ({}).\n", change_id));
        out.push_str("\nCreate one with: jj plan new <bookmark-name>\n");
    } else {
        let n = plans.len();
        let plural = if n == 1 { "" } else { "s" };
        out.push_str(&format!(
            "No plan at @ ({}). {} plan{} in stack:\n",
            change_id, n, plural
        ));
        for view in plans {
            out.push_str(&format!("  ○ {} (jj:{})\n", view.bookmark_name, view.change_id));
        }
        out.push_str("\nNavigate to a plan:\n");
        out.push_str("  jj plan go <bookmark-name>\n");
        out.push_str("  jj plan next / jj plan prev\n");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    // -- build_orientation_message tests ------------------------------------

    fn make_sync_view(bookmark: &str, change_id: &str) -> SyncChangeView {
        SyncChangeView {
            change_id: change_id.to_string(),
            bookmark_name: bookmark.to_string(),
            description: String::new(),
            is_working_copy: false,
        }
    }

    #[test]
    fn orientation_message_no_plans() {
        let msg = build_orientation_message("tsvtxtvr", None);
        assert!(msg.contains("No plan at @ (tsvtxtvr)"), "should show change id: {msg}");
        assert!(msg.contains("jj plan new"), "should hint jj plan new: {msg}");
        assert!(!msg.contains("plan(s) in stack"), "should not mention stack: {msg}");
    }

    #[test]
    fn orientation_message_with_plans() {
        let views = vec![
            make_sync_view("feat-auth", "kpqxywon"),
            make_sync_view("feat-api", "mtzrlpvq"),
        ];
        let msg = build_orientation_message("tsvtxtvr", Some(&views));
        assert!(msg.contains("No plan at @ (tsvtxtvr)"), "should show change id: {msg}");
        assert!(msg.contains("2 plans in stack"), "should show count: {msg}");
        assert!(msg.contains("○ feat-auth (jj:kpqxywon)"), "should list feat-auth: {msg}");
        assert!(msg.contains("○ feat-api (jj:mtzrlpvq)"), "should list feat-api: {msg}");
        assert!(msg.contains("jj plan go"), "should hint navigation: {msg}");
        assert!(msg.contains("jj plan next"), "should hint next/prev: {msg}");
    }

    #[test]
    fn orientation_message_singular_plan() {
        let views = vec![make_sync_view("feat-auth", "kpqxywon")];
        let msg = build_orientation_message("tsvtxtvr", Some(&views));
        assert!(msg.contains("1 plan in stack"), "singular: {msg}");
        assert!(!msg.contains("plans in stack"), "should not be plural: {msg}");
    }

    // -- sub_args_request_help tests ----------------------------------------

    #[test]
    fn sub_args_request_help_with_long_flag() {
        assert!(sub_args_request_help(&args(&["--help"])));
    }

    #[test]
    fn sub_args_request_help_with_short_flag() {
        assert!(sub_args_request_help(&args(&["-h"])));
    }

    #[test]
    fn sub_args_request_help_mixed_with_other_flags() {
        assert!(sub_args_request_help(&args(&["--first", "--help"])));
    }

    #[test]
    fn sub_args_request_help_no_help_flag() {
        assert!(!sub_args_request_help(&args(&["--first"])));
    }

    #[test]
    fn sub_args_request_help_empty() {
        assert!(!sub_args_request_help(&args(&[])));
    }
}