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

/// First line of a description string, for display in stack summary.
///
/// With summary-first metadata format, the title is always line 1 —
/// no skipping needed.
pub fn description_first_line(desc: &str) -> &str {
    desc.lines().next().unwrap_or("")
}

/// Whether a description's metadata `status` field is `✅`.
///
/// Thin wrapper around `PlanDocument::parse(desc).is_done()`.
/// Kept for `LogEntry::is_done()` and `SyncChangeView::is_done()` which
/// don't need a full `PlanDocument` at their call sites.
pub(crate) fn description_is_done(desc: &str) -> bool {
    crate::markdown::PlanDocument::parse(desc).is_done()
}

impl LogEntry {
    /// First line of the description, for display in stack summary.
    pub fn first_line(&self) -> &str {
        description_first_line(&self.description)
    }

    /// Whether the description's metadata `status` field is `✅`.
    pub fn is_done(&self) -> bool {
        description_is_done(&self.description)
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
/// the revset range is non-empty.
#[derive(Debug)]
pub enum StackResult {
    /// Successfully built stack with segments and gap detection.
    Ok(Stack),
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
    /// Gaps in the chain (unbookmarked changes between segments).
    pub gaps: Vec<Gap>,
}

// ---------------------------------------------------------------------------
// Multi-stack types
// ---------------------------------------------------------------------------

/// A group of related plan segments forming one logical stack.
///
/// In the implicit case (no explicit `stack/*` base bookmark), a StackGroup
/// is a connected chain of plan bookmarks in the DAG — plans that are
/// ancestors/descendants of each other. In the explicit case, the boundary
/// is marked by a `stack/*` base bookmark.
#[derive(Debug)]
pub struct StackGroup {
    /// Human-readable name (derived from base bookmark or first plan bookmark).
    pub name: String,
    /// Segments within this stack, trunk-to-tip order.
    pub segments: Vec<BookmarkSegment>,
    /// Gaps within this stack.
    pub gaps: Vec<Gap>,
}

/// Multiple independent stacks discovered from the repository.
///
/// Built by `build_multi_stack()`. Each `StackGroup` is one independent
/// chain of plans. Stacks are ordered by their base's topological distance
/// from trunk (closest first).
#[derive(Debug)]
pub struct MultiStack {
    /// Independent stack groups, ordered by base distance from trunk.
    pub stacks: Vec<StackGroup>,
}

// ---------------------------------------------------------------------------
// Plan registry types (persistent, serde-enabled)
// ---------------------------------------------------------------------------

/// Version constant for the plan registry file format.
///
/// Version history:
/// - v1: Initial format (name, change_id, remote, planned_at)
/// - v2: Added `stack` field (Option<String>) for stack grouping
pub const PLAN_REGISTRY_VERSION: u32 = 2;

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
    /// Stack grouping: the change ID of the stack's base bookmark.
    /// `None` means "implicit trunk stack" (no explicit boundary).
    /// Plans with the same `stack` value belong to the same logical stack.
    /// Added in v2; v1 files load with `stack = None` via serde default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

impl PlannedBookmark {
    /// Create a new planned bookmark (local only).
    pub fn new(name: impl Into<String>, change_id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            change_id: change_id.into(),
            remote: None,
            planned_at: Utc::now(),
            stack: None,
        }
    }

    /// Create a new planned bookmark assigned to an explicit stack.
    pub fn with_stack(
        name: impl Into<String>,
        change_id: impl Into<String>,
        stack: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            change_id: change_id.into(),
            remote: None,
            planned_at: Utc::now(),
            stack: Some(stack.into()),
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

    /// Return all bookmarks belonging to a given stack.
    ///
    /// When `stack_id` is `Some(id)`, returns bookmarks whose `stack` field
    /// matches that value. When `stack_id` is `None`, returns bookmarks
    /// with `stack = None` (the implicit trunk stack).
    pub fn plans_in_stack(&self, stack_id: Option<&str>) -> Vec<&PlannedBookmark> {
        self.bookmarks
            .iter()
            .filter(|b| b.stack.as_deref() == stack_id)
            .collect()
    }

    /// Resolve an encoded filename portion to the canonical bookmark name.
    ///
    /// For each file `NN-ENCODED.md` on disk, this finds the registry entry
    /// whose `encode_bookmark_for_filename(entry.name)` matches `encoded`.
    /// Returns the canonical bookmark name, or `None` if no entry matches.
    ///
    /// This is the core of registry-authoritative resolution: filenames are
    /// never decoded — they are matched against the registry instead.
    pub fn resolve_encoded(&self, encoded: &str) -> Option<&str> {
        self.bookmarks
            .iter()
            .find(|b| crate::plan_file::encode_bookmark_for_filename(&b.name) == encoded)
            .map(|b| b.name.as_str())
    }

    /// Check whether registering `new_name` would collide with an existing
    /// registry entry at the encoded-filename level.
    ///
    /// Returns the colliding entry's canonical bookmark name, or `None` if
    /// no collision exists. For example, `feat--auth` collides with
    /// `feat/auth` because both encode to filename portion `feat--auth`.
    ///
    /// This should be called **before** the new bookmark is registered.
    /// If it returns `Some(existing)`, the registration should be rejected.
    pub fn would_collide(&self, new_name: &str) -> Option<&str> {
        self.resolve_encoded(&crate::plan_file::encode_bookmark_for_filename(new_name))
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
    Gitea,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitHub => write!(f, "GitHub"),
            Self::GitLab => write!(f, "GitLab"),
            Self::Gitea => write!(f, "Gitea"),
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
    /// Head commit SHA — used by GitHub's `check_merge_readiness` to query
    /// check runs for CI status. Other platforms may leave this as `None`.
    #[serde(default)]
    pub head_sha: Option<String>,
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
    /// Merge commit SHA, if available. Set by both GitHub and GitLab
    /// merge responses — useful for future post-merge confirmation.
    #[allow(dead_code)]
    pub sha: Option<String>,
    pub message: Option<String>,
}

/// Method for merging a PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Merge and Rebase are valid methods handled by both platforms;
                     // only Squash is currently the default but --merge-method flag is planned.
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
    fn test_log_entry_is_done_metadata() {
        let entry = make_log_entry("some content\nstatus: ✅\n---\nbody");
        assert!(entry.is_done());
    }

    #[test]
    fn test_log_entry_is_done_no_metadata() {
        // No metadata → not done (no legacy fallback)
        let entry = make_log_entry("title\n\nsome content");
        assert!(!entry.is_done());
    }

    #[test]
    fn test_log_entry_is_done_body_text_not_false_positive() {
        // Body text contains literal "plan-status: ✅" — must NOT trigger false positive
        let entry = make_log_entry("title\nstatus: 🔴\n---\nplan-status: ✅ in body text");
        assert!(!entry.is_done());
    }

    #[test]
    fn test_log_entry_is_done_old_style_not_detected() {
        // Old-style plan-status line without metadata → not done
        let entry = make_log_entry("title\n\nplan-status: ✅");
        assert!(!entry.is_done());
    }

    #[test]
    fn test_first_line_is_title() {
        assert_eq!(
            description_first_line("feat: my feature\nstatus: 🔴\n---\nbody"),
            "feat: my feature"
        );
    }

    #[test]
    fn test_first_line_no_metadata() {
        assert_eq!(
            description_first_line("feat: my feature\n\nbody"),
            "feat: my feature"
        );
    }

    #[test]
    fn test_first_line_single_line() {
        assert_eq!(
            description_first_line("feat: my feature"),
            "feat: my feature"
        );
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
    fn test_resolve_encoded_simple() {
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat-auth", "aabb"));
        assert_eq!(reg.resolve_encoded("feat-auth"), Some("feat-auth"));
    }

    #[test]
    fn test_resolve_encoded_slash() {
        // `feat/auth` encodes to `feat--auth` in filenames
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat/auth", "aabb"));
        assert_eq!(reg.resolve_encoded("feat--auth"), Some("feat/auth"));
    }

    #[test]
    fn test_resolve_encoded_literal_double_dash() {
        // A bookmark literally named `feat--auth` also encodes to `feat--auth`
        // (encode only replaces `/` → `--`, not `--` → anything).
        // This IS the collision scenario: `feat--auth` and `feat/auth` both
        // encode to `feat--auth`. Phase 4 adds collision detection at
        // registration time to prevent this.
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat--auth", "aabb"));
        assert_eq!(reg.resolve_encoded("feat--auth"), Some("feat--auth"));
    }

    #[test]
    fn test_resolve_encoded_miss() {
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat-auth", "aabb"));
        assert_eq!(reg.resolve_encoded("fix-login"), None);
    }

    #[test]
    fn test_would_collide_slash_vs_double_dash() {
        // feat/auth encodes to feat--auth; registering feat--auth should collide
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat/auth", "aabb"));
        assert_eq!(reg.would_collide("feat--auth"), Some("feat/auth"));
    }

    #[test]
    fn test_would_collide_double_dash_vs_slash() {
        // Reverse direction: feat--auth registered first, feat/auth collides
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat--auth", "aabb"));
        assert_eq!(reg.would_collide("feat/auth"), Some("feat--auth"));
    }

    #[test]
    fn test_would_collide_no_collision() {
        // feat-auth (single dash) does NOT collide with feat/auth (encodes to feat--auth)
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat/auth", "aabb"));
        assert_eq!(reg.would_collide("feat-auth"), None);
    }

    #[test]
    fn test_would_collide_self_match() {
        // A name that is already registered "collides" with itself.
        // Callers should check is_tracked() first to distinguish
        // self-match from a genuine collision with a different name.
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat-auth", "aabb"));
        assert_eq!(reg.would_collide("feat-auth"), Some("feat-auth"));
    }

    #[test]
    fn test_would_collide_empty_registry() {
        let reg = PlanRegistry::new();
        assert_eq!(reg.would_collide("anything"), None);
    }

    #[test]
    fn test_resolve_encoded_collision_returns_first_match() {
        // Both `feat/auth` and `feat--auth` encode to `feat--auth`.
        // resolve_encoded returns the first registered match (feat/auth was
        // registered first). In practice, Phase 4 prevents this state by
        // rejecting colliding registrations.
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat/auth", "aabb"));
        reg.track(PlannedBookmark::new("feat--auth", "ccdd"));
        assert_eq!(reg.resolve_encoded("feat--auth"), Some("feat/auth"));
    }

    #[test]
    fn test_plan_registry_serialization() {
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat-auth", "aabb"));
        let mut deploy = PlannedBookmark::new("feat-deploy", "ccdd");
        deploy.remote = Some("origin".to_string());
        reg.track(deploy);

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

    // -----------------------------------------------------------------------
    // Phase 2: stack field and v1/v2 compatibility tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_registry_v1_loads_as_v2() {
        // A v1 TOML string has no `stack` field on bookmarks.
        // Deserialization should succeed with stack = None on all entries.
        let v1_toml = r#"
version = 1

[[bookmarks]]
name = "feat-auth"
change_id = "abc123"
planned_at = "2025-01-15T10:30:00Z"

[[bookmarks]]
name = "feat-db"
change_id = "def456"
remote = "origin"
planned_at = "2025-01-15T11:00:00Z"
"#;

        let reg: PlanRegistry = toml::from_str(v1_toml).expect("v1 should parse");
        assert_eq!(reg.bookmarks.len(), 2);
        assert!(reg.bookmarks[0].stack.is_none(), "v1 entry should have stack = None");
        assert!(reg.bookmarks[1].stack.is_none(), "v1 entry should have stack = None");
        assert_eq!(reg.bookmarks[0].name, "feat-auth");
        assert_eq!(reg.bookmarks[1].remote.as_deref(), Some("origin"));
    }

    #[test]
    fn test_plans_in_stack() {
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::with_stack("auth-1", "aa", "stack-base-abc"));
        reg.track(PlannedBookmark::with_stack("auth-2", "bb", "stack-base-abc"));
        reg.track(PlannedBookmark::new("bugfix", "cc"));        // implicit trunk stack
        reg.track(PlannedBookmark::new("cleanup", "dd"));        // implicit trunk stack

        // Plans in explicit stack
        let auth_plans = reg.plans_in_stack(Some("stack-base-abc"));
        assert_eq!(auth_plans.len(), 2);
        assert_eq!(auth_plans[0].name, "auth-1");
        assert_eq!(auth_plans[1].name, "auth-2");

        // Plans in implicit trunk stack (stack = None)
        let trunk_plans = reg.plans_in_stack(None);
        assert_eq!(trunk_plans.len(), 2);
        assert_eq!(trunk_plans[0].name, "bugfix");
        assert_eq!(trunk_plans[1].name, "cleanup");

        // Non-existent stack
        let empty = reg.plans_in_stack(Some("no-such-stack"));
        assert!(empty.is_empty());
    }

    #[test]
    fn test_registry_roundtrip_v2() {
        let mut reg = PlanRegistry::new();
        reg.track(PlannedBookmark::new("feat-auth", "aabb"));
        reg.track(PlannedBookmark::with_stack("dashboard", "ccdd", "stack-base-xyz"));
        let mut deploy = PlannedBookmark::new("feat-deploy", "eeff");
        deploy.remote = Some("origin".to_string());
        reg.track(deploy);

        // Serialize to TOML
        let toml_str = toml::to_string(&reg).expect("serialize v2");

        // stack should appear for dashboard but not for feat-auth or feat-deploy
        assert!(toml_str.contains("stack = \"stack-base-xyz\""),
            "v2 TOML should contain stack field for dashboard");
        assert_eq!(toml_str.matches("stack =").count(), 1,
            "only one bookmark should have a stack field serialized");

        // Deserialize back
        let reg2: PlanRegistry = toml::from_str(&toml_str).expect("deserialize v2");
        assert_eq!(reg2.version, PLAN_REGISTRY_VERSION);
        assert_eq!(reg2.bookmarks.len(), 3);
        assert!(reg2.bookmarks[0].stack.is_none());
        assert_eq!(reg2.bookmarks[1].stack.as_deref(), Some("stack-base-xyz"));
        assert!(reg2.bookmarks[2].stack.is_none());

        // Round-trip equality
        assert_eq!(reg.bookmarks, reg2.bookmarks);
    }
}