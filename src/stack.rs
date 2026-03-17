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
