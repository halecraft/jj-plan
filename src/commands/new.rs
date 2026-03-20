use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;
use crate::plan_registry;
use crate::template;
use crate::types::PlannedBookmark;
use crate::workspace::Workspace;

/// Run `jj plan new <bookmark-name>` — create a new plan change with a bookmark.
///
/// This is the primary entry point for creating plans. It:
/// 1. Validates the bookmark name is provided and doesn't already exist
/// 2. Flushes pending plan edits
/// 3. Creates a new jj change (`jj new`)
/// 4. Creates a bookmark on the new change
/// 5. Registers the bookmark in the PlanRegistry
/// 6. Sets a templated description with {{CHANGE_ID}} and {{BOOKMARK}}
/// 7. Syncs plan directory and prints summary
///
/// ## Args
///
/// `args` contains everything after `plan new`, e.g. for
/// `jj plan new feat-auth -r main`, args is `["feat-auth", "-r", "main"]`.
pub fn run_new(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    args: &[String],
    workspace: &mut Workspace,
) -> crate::error::Result<i32> {
    // ------------------------------------------------------------------
    // 1. Parse args: bookmark name (required positional) + jj new flags
    // ------------------------------------------------------------------
    let mut bookmark_name: Option<String> = None;
    let mut has_explicit_position = false;
    let mut jj_passthrough: Vec<String> = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "-r" | "-A" | "--insert-after" | "-B" | "--insert-before" => {
                has_explicit_position = true;
                jj_passthrough.push(arg.clone());
                // These flags take a value argument
                if i + 1 < args.len() {
                    i += 1;
                    jj_passthrough.push(args[i].clone());
                }
            }
            _ => {
                if bookmark_name.is_none() && !arg.starts_with('-') {
                    bookmark_name = Some(arg.clone());
                } else {
                    jj_passthrough.push(arg.clone());
                }
            }
        }
        i += 1;
    }

    let bookmark_name = match bookmark_name {
        Some(name) => name,
        None => {
            eprintln!("jj plan new: missing required <bookmark-name> argument");
            eprintln!();
            eprintln!("Usage: jj plan new <bookmark-name> [-r REV] [-A REV] [-B REV]");
            eprintln!();
            eprintln!("Creates a new plan: jj change + bookmark + plan file + registry entry.");
            eprintln!("The bookmark name becomes the plan name (e.g. feat-auth, fix-login).");
            return Ok(1);
        }
    };

    // ------------------------------------------------------------------
    // 2. Validate bookmark doesn't already exist
    // ------------------------------------------------------------------
    let existing_bookmarks = workspace.local_bookmarks();
    if existing_bookmarks.iter().any(|b| b.name == bookmark_name) {
        eprintln!(
            "jj plan new: bookmark '{}' already exists. Use `jj plan track {}` to adopt it as a plan.",
            bookmark_name, bookmark_name
        );
        return Ok(1);
    }

    // Check registry too
    let repo_root = workspace.jj_workspace().workspace_root().to_path_buf();
    let registry = plan_registry::load_registry(&repo_root);
    if registry.is_tracked(&bookmark_name) {
        eprintln!(
            "jj plan new: '{}' is already registered as a plan",
            bookmark_name
        );
        return Ok(1);
    }

    // ------------------------------------------------------------------
    // 3. Flush pending plan edits
    // ------------------------------------------------------------------
    crate::flush::flush_all(&plan_dir.path, jj, workspace);

    // ------------------------------------------------------------------
    // 4. Create new jj change
    // ------------------------------------------------------------------
    // Capture WC change ID before
    workspace.reload();
    let wc_before = workspace.read_change_id_at_wc();

    let mut new_args: Vec<&str> = vec!["new"];
    if !has_explicit_position {
        new_args.push("--insert-after");
        new_args.push("@");
    }
    for arg in &jj_passthrough {
        new_args.push(arg.as_str());
    }

    let status = jj.run_inherit(&new_args)?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }

    // Reload and verify WC actually changed
    workspace.reload();
    let new_change_id = match workspace.read_change_id_at_wc() {
        Some(id) => id,
        None => {
            eprintln!("jj plan new: could not read new change ID");
            return Ok(1);
        }
    };

    if wc_before.as_deref() == Some(new_change_id.as_str()) {
        eprintln!("jj plan new: jj new exited 0 but working copy did not change — aborting");
        return Ok(1);
    }

    // ------------------------------------------------------------------
    // 5. Create bookmark on the new change
    // ------------------------------------------------------------------
    let (bm_status, _bm_stdout, bm_stderr) =
        jj.run_silent(&["bookmark", "create", &bookmark_name, "-r", "@"])?;

    if !bm_status.success() {
        eprintln!(
            "jj plan new: failed to create bookmark '{}': {}",
            bookmark_name,
            bm_stderr.trim()
        );
        // Roll back the `jj new`
        let _ = jj.run_silent(&["undo"]);
        return Ok(bm_status.code().unwrap_or(1));
    }

    // ------------------------------------------------------------------
    // 6. Register in PlanRegistry
    // ------------------------------------------------------------------
    // Get the full standard hex change ID for the registry entry.
    // PlannedBookmark.change_id must be full hex (matching LogEntry.change_id
    // convention from jj:pozrnomw) — NOT short reverse-hex. Look up the
    // bookmark we just created to get the correct encoding.
    workspace.reload();
    let full_change_id = workspace
        .local_bookmarks()
        .iter()
        .find(|b| b.name == bookmark_name)
        .map(|b| b.change_id.clone())
        .unwrap_or_else(|| {
            // Fallback: should not happen since we just created the bookmark,
            // but degrade gracefully with the short ID rather than crashing.
            eprintln!("jj plan new: warning: could not resolve full change ID for bookmark '{}'", bookmark_name);
            new_change_id.clone()
        });

    let mut registry = plan_registry::load_registry(&repo_root);
    registry.track(PlannedBookmark::new(
        bookmark_name.clone(),
        full_change_id,
    ));
    plan_registry::save_registry(&repo_root, &registry);

    // ------------------------------------------------------------------
    // 7. Set templated description
    // ------------------------------------------------------------------
    let description =
        template::render_template_with_bookmark(&plan_dir.path, &new_change_id, &bookmark_name);
    let _ = jj.run_silent(&["describe", "-m", &description]);

    // ------------------------------------------------------------------
    // 8. Reload, sync, and show
    // ------------------------------------------------------------------
    eprintln!("Created plan: {} (jj:{})", bookmark_name, new_change_id);
    workspace.reload();
    crate::wrap::resolve_and_sync(plan_dir, workspace);

    Ok(0)
}