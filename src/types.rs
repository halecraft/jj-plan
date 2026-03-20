//! Unified domain types for jj-plan.
//!
//! These types form the in-memory domain model shared across all modules.
//! They are adopted from jj-ryu's type system with additions from jj-plan.
//!
//! ## Design decisions
//!
//! - `LogEntry.change_id` stores **full standard hex** (64 chars, 0-9 a-f),
//!   stable across repo mutations. Short reverse-hex prefixes for CLI use
//!   are computed on demand via `Workspace::short_change_id()`.
//! - No `Serialize`/`Deserialize` — these are in-memory types. Serde derives
//!   arrive in jj:zypnnqyt when PR cache serialization is needed.
//! - `BookmarkSegment.changes` uses newest-first ordering (tip toward trunk
//!   within each segment), matching ryu's convention. `Stack.segments` is
//!   ordered trunk (index 0) to tip (last index).

use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// Core domain types (adopted from jj-ryu)
// ---------------------------------------------------------------------------

/// A jj bookmark (branch reference).
///
/// Represents a local bookmark with optional remote tracking status.
/// `commit_id` and `change_id` store full standard hex (64 chars).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    /// Bookmark name.
    pub name: String,
    /// Git commit ID (full hex).
    pub commit_id: String,
    /// jj change ID (full hex).
    pub change_id: String,
    /// Whether this bookmark exists on any remote.
    pub has_remote: bool,
    /// Whether local and remote are in sync.
    pub is_synced: bool,
}

/// A commit/change entry from jj log.
///
/// Rich representation of a single commit, combining data from both
/// jj-plan's `StackChange` and jj-ryu's `LogEntry`.
///
/// `change_id` stores full standard hex (64 chars). Use
/// `Workspace::short_change_id()` for CLI-facing short reverse-hex.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Git commit ID (full hex).
    pub commit_id: String,
    /// jj change ID (full hex).
    pub change_id: String,
    /// Author name.
    pub author_name: String,
    /// Author email.
    pub author_email: String,
    /// First line of commit description.
    pub description_first_line: String,
    /// Full commit description (includes first line).
    pub description: String,
    /// Parent commit IDs (full hex).
    pub parents: Vec<String>,
    /// Local bookmarks pointing to this commit.
    pub local_bookmarks: Vec<String>,
    /// Remote bookmarks pointing to this commit (format: "name@remote").
    pub remote_bookmarks: Vec<String>,
    /// Whether this is the working copy commit.
    pub is_working_copy: bool,
    /// Whether this commit is empty (tree matches parent's merged tree).
    pub is_empty: bool,
    /// When the commit was authored.
    pub authored_at: DateTime<Utc>,
    /// When the commit was committed.
    pub committed_at: DateTime<Utc>,
}

impl LogEntry {
    /// First line of the description, for display in `.stack` summary.
    ///
    /// Migrated from `StackChange::first_line()`.
    pub fn first_line(&self) -> &str {
        self.description.lines().next().unwrap_or("")
    }

    /// Whether the description contains `plan-status: ✅`.
    ///
    /// Migrated from `StackChange::is_done()`.
    pub fn is_done(&self) -> bool {
        self.description.starts_with("plan-status: ✅")
            || self.description.contains("\nplan-status: ✅")
    }
}

/// A segment of changes belonging to one or more bookmarks.
///
/// Represents the bookmark commit plus all unbookmarked commits behind it
/// (back to the previous bookmark or trunk). The bookmark is at the tip.
///
/// `changes` uses newest-first ordering (tip toward trunk within each
/// segment), matching ryu's convention. The first element is the
/// bookmarked tip commit.
#[derive(Debug, Clone)]
pub struct BookmarkSegment {
    /// Bookmarks pointing to the tip of this segment.
    pub bookmarks: Vec<Bookmark>,
    /// Changes in this segment (newest first, i.e. tip toward trunk).
    pub changes: Vec<LogEntry>,
}

// ---------------------------------------------------------------------------
// Stack types (new for jj-plan)
// ---------------------------------------------------------------------------

/// Result of building a stack from the repository state.
///
/// Makes illegal states unrepresentable: a `Stack` is only produced when
/// the revset range is non-empty and contains no merge commits.
#[derive(Debug)]
pub enum StackResult {
    /// Successfully built stack with segments and gap detection.
    Ok(Stack),
    /// The revset range contains merge commits — stack building refused.
    MergeCommits,
    /// The revset range is empty (working copy is at trunk).
    Empty,
}

/// A fully resolved stack from `trunk()..(@  | descendants(@))`.
///
/// `segments` is ordered trunk (index 0) to tip (last index).
/// Only bookmarked commits produce segments. Unbookmarked commits
/// between bookmarked segments are detected as gaps.
#[derive(Debug)]
pub struct Stack {
    /// Bookmark segments from trunk (index 0) to tip (last index).
    pub segments: Vec<BookmarkSegment>,
    /// Gaps: unbookmarked commits between two bookmarked segments.
    pub gaps: Vec<Gap>,
}

/// A gap between two bookmarked segments.
///
/// Contains unbookmarked commits that exist between two bookmarks
/// (or between a bookmark and the start of the stack). These are
/// flagged at submit time — the user must squash, bookmark, or
/// pass `--allow-gaps`.
#[derive(Debug, Clone)]
pub struct Gap {
    /// The unbookmarked changes in this gap.
    pub unbookmarked: Vec<UnbookmarkedChange>,
    /// The bookmark name of the segment AFTER the gap (toward tip).
    pub before_bookmark: String,
    /// The bookmark name of the segment BEFORE the gap (toward trunk),
    /// or `None` if the gap is between trunk and the first bookmark.
    pub after_bookmark: Option<String>,
}

/// An unbookmarked change in a gap, for error messages.
#[derive(Debug, Clone)]
pub struct UnbookmarkedChange {
    /// Short change ID (reverse-hex prefix) for display.
    pub short_id: String,
    /// First line of the description for context.
    pub description_first_line: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_log_entry(desc: &str) -> LogEntry {
        LogEntry {
            commit_id: "aabbccdd".to_string(),
            change_id: "11223344".to_string(),
            author_name: "Test".to_string(),
            author_email: "test@test.com".to_string(),
            description_first_line: desc.lines().next().unwrap_or("").to_string(),
            description: desc.to_string(),
            parents: vec![],
            local_bookmarks: vec![],
            remote_bookmarks: vec![],
            is_working_copy: false,
            is_empty: false,
            authored_at: Utc::now(),
            committed_at: Utc::now(),
        }
    }

    #[test]
    fn test_log_entry_first_line() {
        let entry = make_log_entry("first line\nsecond line\nthird line");
        assert_eq!(entry.first_line(), "first line");
    }

    #[test]
    fn test_log_entry_first_line_single_line() {
        let entry = make_log_entry("only line");
        assert_eq!(entry.first_line(), "only line");
    }

    #[test]
    fn test_log_entry_first_line_empty() {
        let entry = make_log_entry("");
        assert_eq!(entry.first_line(), "");
    }

    #[test]
    fn test_log_entry_is_done_at_start() {
        let entry = make_log_entry("plan-status: ✅\nsome content");
        assert!(entry.is_done());
    }

    #[test]
    fn test_log_entry_is_done_after_newline() {
        let entry = make_log_entry("title\n\nplan-status: ✅");
        assert!(entry.is_done());
    }

    #[test]
    fn test_log_entry_is_not_done() {
        let entry = make_log_entry("title\n\nsome content");
        assert!(!entry.is_done());
    }

    #[test]
    fn test_log_entry_is_done_inline_does_not_match() {
        // "plan-status: ✅" must be at start of line, not embedded in text
        let entry = make_log_entry("title with plan-status: ✅ inline");
        assert!(!entry.is_done());
    }

    #[test]
    fn test_bookmark_equality() {
        let b1 = Bookmark {
            name: "feat-auth".to_string(),
            commit_id: "aabb".to_string(),
            change_id: "ccdd".to_string(),
            has_remote: false,
            is_synced: false,
        };
        let b2 = b1.clone();
        assert_eq!(b1, b2);
    }

    #[test]
    fn test_stack_result_variants() {
        // Smoke test: ensure all variants are constructible
        let empty = StackResult::Empty;
        let merge = StackResult::MergeCommits;
        let ok = StackResult::Ok(Stack {
            segments: vec![],
            gaps: vec![],
        });

        // Pattern matching works
        assert!(matches!(empty, StackResult::Empty));
        assert!(matches!(merge, StackResult::MergeCommits));
        assert!(matches!(ok, StackResult::Ok(_)));
    }
}