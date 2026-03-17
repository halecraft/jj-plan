use crate::jj_binary::JjBinary;

/// How the stack base was resolved.
#[derive(Debug, Clone)]
pub enum StackBase {
    /// The bookmarked change IS the first stack member (inclusive range).
    /// Contains the shortest unique change ID prefix.
    Inclusive(String),
    /// The trunk commit is NOT part of the stack (exclusive range).
    Exclusive,
    /// Ambiguous: multiple equidistant stack bookmarks found.
    Ambiguous(Vec<String>),
}

/// Display helper for `jj plan config` output.
impl StackBase {
    /// Return `(base_string, mode_string)` for display in config output.
    pub fn display_pair(&self) -> Option<(String, &'static str)> {
        match self {
            StackBase::Inclusive(id) => Some((id.clone(), "inclusive")),
            StackBase::Exclusive => Some(("trunk()".to_string(), "exclusive")),
            StackBase::Ambiguous(_) => None,
        }
    }
}

/// Metadata for a single change in the stack.
#[derive(Debug, Clone)]
pub struct StackChange {
    /// Shortest unique change ID prefix (8+ chars).
    pub change_id: String,
    /// Full description text (trailing newline stripped).
    pub description: String,
    /// True if the change has no file changes (description not considered).
    pub is_empty: bool,
    /// True if this change is the current working copy (`@`).
    pub is_working_copy: bool,
    /// Bookmark names on this change, or empty.
    pub bookmarks: Vec<String>,
}

impl StackChange {
    /// First line of the description, for display in `.stack` summary.
    pub fn first_line(&self) -> &str {
        self.description.lines().next().unwrap_or("")
    }

    /// Whether the description contains `plan-status: ✅`.
    pub fn is_done(&self) -> bool {
        // Check: on its own line (after a newline), or at the very start
        self.description.starts_with("plan-status: ✅")
            || self.description.contains("\nplan-status: ✅")
    }
}

/// Resolve the stack base using the standard fallback chain.
///
/// 1. `stack` / `stack/*` bookmarks — nearest ancestor of `@` (inclusive)
/// 2. `trunk()` — if it resolves to something other than `root()` (exclusive)
/// 3. None — no usable base
///
/// Returns `Some(StackBase)` on success, `None` if no base can be found.
pub fn resolve_stack_base(jj: &JjBinary) -> Option<StackBase> {
    // 1. stack / stack/* bookmarks — nearest ancestor of @ (inclusive)
    let revset = r#"heads((bookmarks(exact:"stack") | bookmarks(glob:"stack/*")) & ::@)"#;

    if let Ok((status, stdout, _)) = jj.run_silent(&[
        "log",
        "-r",
        revset,
        "-T",
        r#"change_id.shortest(8) ++ "\n""#,
        "--no-graph",
    ]) {
        if status.success() {
            let heads: Vec<&str> = stdout.trim().lines().filter(|l| !l.is_empty()).collect();
            if heads.len() == 1 {
                return Some(StackBase::Inclusive(heads[0].to_string()));
            }
            if heads.len() > 1 {
                // Multiple heads — ambiguous sibling bookmarks
                return Some(StackBase::Ambiguous(
                    heads.iter().map(|s| s.to_string()).collect(),
                ));
            }
        }
    }

    // 2. trunk() — if it resolves to something other than root() (exclusive)
    if let Ok((status, stdout, _)) = jj.run_silent(&[
        "log",
        "-r",
        "trunk() & ~root()",
        "-T",
        "change_id",
        "--no-graph",
    ]) {
        if status.success() && !stdout.trim().is_empty() {
            return Some(StackBase::Exclusive);
        }
    }

    // 3. No usable base
    None
}

/// Build the revset string for the full stack range given a resolved base.
fn build_stack_revset(base: &StackBase) -> Option<String> {
    match base {
        StackBase::Inclusive(change_id) => {
            Some(format!("({}::@) | descendants(@)", change_id))
        }
        StackBase::Exclusive => Some("(trunk()..@) | descendants(@)".to_string()),
        StackBase::Ambiguous(_) => None, // Can't build a revset for ambiguous state
    }
}

/// Resolve the ordered list of stack changes by querying jj.
///
/// Uses a single `jj log` call with a custom template using RS (\x1e) as
/// field separator and NUL (\0) as record separator — the same approach as
/// the zsh shim's `__jj_plan_batch_read`, but parsed in Rust.
///
/// Returns `None` if the stack can't be resolved or is empty.
pub fn resolve_stack_changes(
    jj: &JjBinary,
    base: &StackBase,
) -> Option<Vec<StackChange>> {
    let revset = build_stack_revset(base)?;
    batch_read_changes(jj, &revset)
}

/// Read changes matching a revset, returning them in stack order (reversed).
///
/// This is the Rust equivalent of `__jj_plan_batch_read`. It uses the same
/// RS/NUL template format for reliable parsing of multi-line descriptions.
pub fn batch_read_changes(jj: &JjBinary, revset: &str) -> Option<Vec<StackChange>> {
    // Template: change_id RS bookmarks RS empty_flag RS wc_flag RS description NUL
    let template = concat!(
        r#"change_id.shortest(8) ++ "\x1e""#,
        r#" ++ if(bookmarks, bookmarks.join(","), "-") ++ "\x1e""#,
        r#" ++ if(empty, "E", "F") ++ "\x1e""#,
        r#" ++ if(self.contained_in("@"), "C", "-") ++ "\x1e""#,
        r#" ++ description ++ "\0""#,
    );

    let (status, stdout, _) = jj
        .run_silent(&["log", "-r", revset, "-T", template, "--reversed", "--no-graph"])
        .ok()?;
    if !status.success() || stdout.is_empty() {
        return None;
    }

    let mut changes = Vec::new();

    // Split by NUL record separator
    for record in stdout.split('\0') {
        if record.is_empty() {
            continue;
        }

        // Split by RS field separator
        let fields: Vec<&str> = record.split('\x1e').collect();
        if fields.len() < 5 {
            continue;
        }

        let change_id = fields[0].to_string();
        if change_id.is_empty() {
            continue;
        }

        let bookmarks = if fields[1] == "-" {
            Vec::new()
        } else {
            fields[1].split(',').map(|s| s.to_string()).collect()
        };

        let is_empty = fields[2] == "E";
        let is_working_copy = fields[3] == "C";

        // Description is everything after the 4th RS separator.
        // Strip trailing newline (jj appends one to descriptions).
        let description = fields[4..].join("\x1e"); // rejoin if description contained RS
        let description = description.strip_suffix('\n').unwrap_or(&description);

        changes.push(StackChange {
            change_id,
            description: description.to_string(),
            is_empty,
            is_working_copy,
            bookmarks,
        });
    }

    if changes.is_empty() {
        None
    } else {
        Some(changes)
    }
}

/// Read changes for specific change IDs (used by flush to compare descriptions).
///
/// Builds a revset like `id1 | id2 | id3` and batch-reads all at once.
pub fn batch_read_by_ids(jj: &JjBinary, change_ids: &[&str]) -> Option<Vec<StackChange>> {
    if change_ids.is_empty() {
        return None;
    }
    let revset = change_ids.join(" | ");
    batch_read_changes(jj, &revset)
}

