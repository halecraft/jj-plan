use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::types::PlanRegistry;
use crate::workspace::Workspace;

/// Run `jj abandon` with the standard wrap lifecycle.
///
/// In the new model (no `stack/*` bookmarks), abandon doesn't need special
/// bookmark recovery logic. Regular user bookmarks are not affected by
/// abandoning a change (jj handles bookmark movement automatically).
///
/// `args` is the FULL original argument list starting with `"abandon"`,
/// e.g. `["abandon", "CHANGE_ID"]`.
pub fn run_abandon(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    args: &[String],
    workspace: &mut Workspace,
    registry: &PlanRegistry,
) -> crate::error::Result<i32> {
    // Standard lifecycle: flush → command → reload → sync → show
    crate::wrap::wrap(plan_dir, jj, args, workspace, registry)
}