use crate::jj_binary::JjBinary;
use crate::plan_dir::PlanDir;

/// Run `jj plan stack` — create a new stack with a single plan (jj change).
///
/// This is the Rust equivalent of the zsh shim's `jj plan stack` handler.
///
/// ## Args
///
/// `args` contains everything after `plan stack`, e.g. for
/// `jj plan stack myname -r main`, args is `["myname", "-r", "main"]`.
///
/// ## Supported flags
///
/// - `-r <rev>`: revision to create the new change after (passed to `jj new -r`)
/// - Positional arg: stack name — bookmark will be `stack/{name}` (or just
///   `stack` if omitted)
///
/// ## Steps
///
/// 1. Parse args (positional stack name, optional `-r` rev)
/// 2. Flush local plan edits to jj descriptions
/// 3. Create a new jj change (`jj new`)
/// 4. Set a `stack` / `stack/{name}` bookmark on the new change
/// 5. Give the change a placeholder description
/// 6. Sync plan directory to reflect the new stack
/// 7. Print summary
pub fn run_stack(
    jj: &JjBinary,
    plan_dir: &PlanDir,
    args: &[String],
) -> crate::error::Result<i32> {
    // -----------------------------------------------------------------------
    // 1. Parse args: `-r <rev>` and positional stack name
    // -----------------------------------------------------------------------
    let mut stack_rev: Option<String> = None;
    let mut stack_name: Option<String> = None;
    let mut i = 0;

    while i < args.len() {
        if args[i] == "-r" {
            if i + 1 >= args.len() {
                eprintln!("jj plan stack: -r requires a revision argument");
                return Ok(1);
            }
            stack_rev = Some(args[i + 1].clone());
            i += 2;
        } else {
            stack_name = Some(args[i].clone());
            i += 1;
        }
    }

    // -----------------------------------------------------------------------
    // 2. Determine bookmark name
    // -----------------------------------------------------------------------
    let bookmark_name = match &stack_name {
        Some(name) => format!("stack/{}", name),
        None => "stack".to_string(),
    };

    // -----------------------------------------------------------------------
    // 3. Flush local plan edits to jj descriptions
    // -----------------------------------------------------------------------
    crate::flush::flush_all(&plan_dir.path, jj);

    // -----------------------------------------------------------------------
    // 4. Create new change
    // -----------------------------------------------------------------------
    let status = if let Some(ref rev) = stack_rev {
        jj.run_inherit(&["new", "-r", rev])?
    } else {
        jj.run_inherit(&["new"])?
    };

    if !status.success() {
        let code = status.code().unwrap_or(1);
        eprintln!("jj plan stack: failed to create new change (exit {})", code);
        return Ok(code);
    }

    // -----------------------------------------------------------------------
    // 5. Set bookmark on the new change
    // -----------------------------------------------------------------------
    let (bm_status, _bm_stdout, bm_stderr) =
        jj.run_silent(&["bookmark", "set", &bookmark_name, "-r", "@", "-B"])?;

    if !bm_status.success() {
        let code = bm_status.code().unwrap_or(1);
        eprintln!(
            "jj plan stack: failed to set bookmark '{}': {}",
            bookmark_name,
            bm_stderr.trim()
        );
        // Roll back the `jj new` we just did
        let _ = jj.run_silent(&["undo"]);
        return Ok(code);
    }

    // -----------------------------------------------------------------------
    // 6. Read back the change ID of the new change
    // -----------------------------------------------------------------------
    let (_log_status, log_stdout, _log_stderr) = jj.run_silent(&[
        "log",
        "-r",
        "@",
        "-T",
        "change_id.shortest(8)",
        "--no-graph",
    ])?;
    let change_id = log_stdout.trim().to_string();

    // -----------------------------------------------------------------------
    // 7. Set placeholder description
    // -----------------------------------------------------------------------
    let placeholder = format!("(placeholder: jj:{})", change_id);
    let _ = jj.run_silent(&["describe", "-m", &placeholder]);

    // -----------------------------------------------------------------------
    // 8. Sync plan directory to reflect the new stack
    // -----------------------------------------------------------------------
    let max = crate::plan_dir::plan_max();
    let base = crate::stack::resolve_stack_base(jj);
    let changes = base
        .as_ref()
        .and_then(|b| crate::stack::resolve_stack_changes(jj, b));
    crate::sync::sync(plan_dir, changes.as_deref(), max);

    // -----------------------------------------------------------------------
    // 9. Print summary (to stderr, matching zsh shim convention)
    // -----------------------------------------------------------------------
    eprintln!("Started new stack: {} ({})", bookmark_name, change_id);

    // -----------------------------------------------------------------------
    // 10. Show the stack
    // -----------------------------------------------------------------------
    crate::sync::show_stack(plan_dir);

    Ok(0)
}