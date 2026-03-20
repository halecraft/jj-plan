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
//! - `LogEntry` and `Bookmark` now have `Serialize`/`Deserialize` derives
//!   (enabled in jj:zypnnqyt for PR cache and API response handling).
//! - `BookmarkSegment.changes` uses newest-first ordering (tip toward trunk
//!   within each segment), matching ryu's convention. `Stack.segments` is
//!   ordered trunk (index 0) to tip (last index).

use chrono::{DateTime, Utc};
use serde::{Serialize, Deserialize};

// ---------------------------------------------------------------------------
// Core domain types (adopted from jj-ryu)
// ---------------------------------------------------------------------------

/// A jj bookmark (branch reference).
///
/// Represents a local bookmark with optional remote tracking status.
/// `commit_id` and `change_id` store full standard hex (64 chars).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
/// Rich representation of a single commit/change from jj log.
///
/// `change_id` stores full standard hex (64 chars). Use
/// `Workspace::short_change_id()` for CLI-facing short reverse-hex.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub fn first_line(&self) -> &str {
        self.description.lines().next().unwrap_or("")
    }

    /// Whether the description contains `plan-status: ✅`.
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

/// A chain of segments from trunk to a target bookmark, for submission.
///
/// Built by `collect_submission_chain()`. Includes gap information
/// for the `--allow-gaps` check at submit time.
#[derive(Debug)]
pub struct SubmissionChain {
    /// Segments from trunk (index 0) to target (last index).
    pub segments: Vec<BookmarkSegment>,
    /// Gaps in the chain (unbookmarked changes between segments).
    pub gaps: Vec<Gap>,
}

// ---------------------------------------------------------------------------
// Plan registry types (persistent, serde-enabled)
// ---------------------------------------------------------------------------

/// Version constant for the plan registry file format.
pub const PLAN_REGISTRY_VERSION: u32 = 1;

/// Persistent record that a bookmark is a plan.
///
/// Stored in the plan registry file. Unlike the in-memory `Bookmark` type,
/// this carries `Serialize`/`Deserialize` for TOML persistence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannedBookmark {
    pub name: String,
    pub change_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    pub planned_at: DateTime<Utc>,
}

impl PlannedBookmark {
    /// Create a new planned bookmark (local only).
    pub fn new(name: impl Into<String>, change_id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            change_id: change_id.into(),
            remote: None,
            planned_at: Utc::now(),
        }
    }

    /// Create a new planned bookmark with a remote.
    pub fn with_remote(
        name: impl Into<String>,
        change_id: impl Into<String>,
        remote: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            change_id: change_id.into(),
            remote: Some(remote.into()),
            planned_at: Utc::now(),
        }
    }
}

/// Persistent plan registry — the on-disk state of tracked plan bookmarks.
///
/// Serialized to/from TOML. Adopted from ryu's `TrackingState`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanRegistry {
    pub version: u32,
    #[serde(default)]
    pub bookmarks: Vec<PlannedBookmark>,
}

impl PlanRegistry {
    /// Create a new empty registry with the current version.
    pub const fn new() -> Self {
        Self {
            version: PLAN_REGISTRY_VERSION,
            bookmarks: Vec::new(),
        }
    }

    /// Whether a bookmark with the given name is tracked.
    pub fn is_tracked(&self, name: &str) -> bool {
        self.bookmarks.iter().any(|b| b.name == name)
    }

    /// Get a tracked bookmark by name.
    pub fn get(&self, name: &str) -> Option<&PlannedBookmark> {
        self.bookmarks.iter().find(|b| b.name == name)
    }

    /// Track a bookmark. No-op if already tracked.
    pub fn track(&mut self, bookmark: PlannedBookmark) {
        if !self.is_tracked(&bookmark.name) {
            self.bookmarks.push(bookmark);
        }
    }

    /// Untrack a bookmark by name. Returns `true` if it was removed.
    pub fn untrack(&mut self, name: &str) -> bool {
        let len_before = self.bookmarks.len();
        self.bookmarks.retain(|b| b.name != name);
        self.bookmarks.len() < len_before
    }

    /// Names of all tracked bookmarks.
    pub fn tracked_names(&self) -> Vec<&str> {
        self.bookmarks.iter().map(|b| b.name.as_str()).collect()
    }
}

/// A narrowed bookmark segment for downstream submit/merge operations.
///
/// Pairs a single `Bookmark` with the changes that belong to it.
#[derive(Debug, Clone)]
pub struct NarrowedBookmarkSegment {
    pub bookmark: Bookmark,
    pub changes: Vec<LogEntry>,
}

// ---------------------------------------------------------------------------
// PR and platform types (deferred from jj:pozrnomw, ported from jj-ryu)
// ---------------------------------------------------------------------------

/// A git remote with name and URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRemote {
    pub name: String,
    pub url: String,
}

/// Supported hosting platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    GitHub,
    GitLab,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitHub => write!(f, "GitHub"),
            Self::GitLab => write!(f, "GitLab"),
        }
    }
}

/// Configuration for a hosting platform repository.
#[derive(Debug, Clone)]
pub struct PlatformConfig {
    pub platform: Platform,
    pub owner: String,
    pub repo: String,
    pub host: Option<String>,
}

/// A pull request / merge request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub html_url: String,
    pub base_ref: String,
    pub head_ref: String,
    pub title: String,
    pub node_id: Option<String>,
    pub is_draft: bool,
}

/// A comment on a pull request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrComment {
    pub id: u64,
    pub body: String,
}

/// State of a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

impl std::fmt::Display for PrState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open => write!(f, "open"),
            Self::Closed => write!(f, "closed"),
            Self::Merged => write!(f, "merged"),
        }
    }
}

/// Extended PR details for merge operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestDetails {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: PrState,
    pub is_draft: bool,
    pub mergeable: Option<bool>,
    pub head_ref: String,
    pub base_ref: String,
    pub html_url: String,
}

/// Merge readiness assessment.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct MergeReadiness {
    pub is_approved: bool,
    pub ci_passed: bool,
    pub is_mergeable: Option<bool>,
    pub is_draft: bool,
    pub blocking_reasons: Vec<String>,
    pub uncertainties: Vec<String>,
}

impl MergeReadiness {
    pub const fn is_blocked(&self) -> bool {
        !self.is_approved
            || !self.ci_passed
            || self.is_draft
            || matches!(self.is_mergeable, Some(false))
    }

    pub fn uncertainty(&self) -> Option<&str> {
        self.uncertainties.first().map(String::as_str)
    }
}

/// Result of a merge operation.
#[derive(Debug, Clone)]
pub struct MergeResult {
    pub merged: bool,
    pub sha: Option<String>,
    pub message: Option<String>,
}

/// Method for merging a PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

impl std::fmt::Display for MergeMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Squash => write!(f, "squash"),
            Self::Merge => write!(f, "merge"),
            Self::Rebase => write!(f, "rebase"),
        }
    }
}

/// A cached PR association for persistence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedPr {
    /// Bookmark name this PR is associated with.
    pub bookmark: String,
    /// PR/MR number.
    pub number: u64,
    /// Web URL for the PR.
    pub url: String,
    /// Remote this PR was pushed to.
    pub remote: String,
    /// When this cache entry was last updated.
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use toml;

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

    // -----------------------------------------------------------------------
    // PlanRegistry tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_plan_registry_track_untrack() {
        let mut reg = PlanRegistry::new();
        assert_eq!(reg.version, PLAN_REGISTRY_VERSION);
        assert!(reg.bookmarks.is_empty());

        // Track a bookmark
        reg.track(PlannedBookmark::new("feat-auth", "aabb"));
        assert!(reg.is_tracked("feat-auth"));
        assert!(!reg.is_tracked("feat-other"));
        assert_eq!(reg.tracked_names(), vec!["feat-auth"]);

        // Get it back
        let got = reg.get("feat-auth").unwrap();
        assert_eq!(got.name, "feat-auth");
        assert_eq!(got.change_id, "aabb");

        // Track another
        reg.track(PlannedBookmark::new("feat-other", "ccdd"));
        assert_eq!(reg.tracked_names().len(), 2);

        // Untrack first
        assert!(reg.untrack("feat-auth"));
        assert!(!reg.is_tracked("feat-auth"));
        assert_eq!(reg.tracked_names(), vec!["feat-other"]);

        // Untrack non-existent returns false
        assert!(!reg.untrack("no-such"));
    }

    #[test]
    fn test_plan_registry_duplicate_track_is_noop() {
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat-auth", "aabb"));
        reg.track(PlannedBookmark::new("feat-auth", "different-id"));

        // Should still have exactly one entry with the original change_id
        assert_eq!(reg.bookmarks.len(), 1);
        assert_eq!(reg.get("feat-auth").unwrap().change_id, "aabb");
    }

    #[test]
    fn test_plan_registry_serialization() {
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat-auth", "aabb"));
        reg.track(PlannedBookmark::with_remote("feat-deploy", "ccdd", "origin"));

        // Serialize to TOML
        let toml_str = toml::to_string(&reg).expect("serialize");

        // Deserialize back
        let reg2: PlanRegistry = toml::from_str(&toml_str).expect("deserialize");

        assert_eq!(reg2.version, PLAN_REGISTRY_VERSION);
        assert_eq!(reg2.bookmarks.len(), 2);
        assert_eq!(reg2.bookmarks[0].name, "feat-auth");
        assert!(reg2.bookmarks[0].remote.is_none());
        assert_eq!(reg2.bookmarks[1].name, "feat-deploy");
        assert_eq!(reg2.bookmarks[1].remote.as_deref(), Some("origin"));

        // Verify round-trip equality for the bookmarks
        assert_eq!(reg.bookmarks, reg2.bookmarks);
    }
}