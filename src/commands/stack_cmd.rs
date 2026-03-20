use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::workspace::Workspace;

/// Dispatch `jj stack <subcommand>` to the appropriate handler.
///
/// `args` is the full argument list starting with "stack".
/// For example: `["stack", "submit"]` or `["stack", "--help"]`.
pub fn dispatch_stack(
    _jj: &JjBinary,
    _plan_dir: &PlanDir,
    args: &[String],
    _workspace: &mut Workspace,
) -> crate::error::Result<i32> {
    let subcommand = args.get(1).map(|s| s.as_str());

    // Help handling
    if matches!(subcommand, Some("--help" | "-h")) || subcommand.is_none() {
        print_stack_help();
        return Ok(0);
    }

    match subcommand {
        Some("submit") => {
            eprintln!("jj stack submit: not yet implemented");
            eprintln!("PR submission will be available in a future release. See jj:zypnnqyt");
            Ok(0)
        }
        Some("sync") => {
            eprintln!("jj stack sync: not yet implemented");
            eprintln!("Stack sync will be available in a future release. See jj:zypnnqyt");
            Ok(0)
        }
        Some("merge") => {
            eprintln!("jj stack merge: not yet implemented");
            eprintln!("PR merging will be available in a future release. See jj:zypnnqyt");
            Ok(0)
        }
        Some("auth") => {
            eprintln!("jj stack auth: not yet implemented");
            eprintln!("Authentication management will be available in a future release. See jj:zypnnqyt");
            Ok(0)
        }
        Some(unknown) => {
            eprintln!("jj stack: unknown subcommand '{}'", unknown);
            eprintln!();
            eprintln!("Available subcommands: submit, sync, merge, auth");
            eprintln!("Run 'jj stack --help' for more information.");
            Ok(1)
        }
        None => unreachable!(), // handled above
    }
}

/// Print help for `jj stack` commands.
fn print_stack_help() {
    eprintln!("jj stack — stack-oriented PR operations");
    eprintln!();
    eprintln!("Usage: jj stack <SUBCOMMAND>");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  submit [bookmark]   Push and create/update PRs (coming soon)");
    eprintln!("  sync                Fetch, push, and update stack (coming soon)");
    eprintln!("  merge               Merge approved PRs (coming soon)");
    eprintln!("  auth                Authentication management (coming soon)");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --help, -h          Show this help message");
    eprintln!();
    eprintln!("Stack visualization coming soon. See jj:zypnnqyt");
}