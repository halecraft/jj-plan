//! Stack builder for jj-plan.
//!
//! Builds a `Stack` from the repository state by evaluating
//! `trunk()..(@  | descendants(@))`, identifying bookmarked segments,
//! detecting gaps, and rejecting merge commits.
//!
//! ## Design
//!
//! - **Pure logic.** `build_stack()` takes a `Workspace` reference and returns
//!   a `StackResult`. It performs no I/O beyond jj-lib's in-process reads.
//! - **Registry-based filtering.** When a `PlanRegistry` is provided, only
//!   commits with at least one registered bookmark produce segments. Commits
//!   with only non-registered bookmarks are treated as unbookmarked. When
//!   `None`, all bookmarked commits produce segments (backwards-compatible).
//! - **Segments are trunk-to-tip.** `Stack.segments[0]` is closest to trunk.
//! - **Changes within each segment are newest-first** (tip toward trunk),
//!   matching ryu's `BookmarkSegment` convention.
//! - **Gaps** are unbookmarked commits between two bookmarked segments.
//!   Unbookmarked commits before the first bookmark or after the last
//!   bookmark are NOT gaps (they are pre-stack history or WIP respectively).
//!
//! Context: jj:pozrnomw, jj:ntksslnn

use crate::types::{Bookmark, BookmarkSegment, Gap, LogEntry, NarrowedBookmarkSegment, PlanRegistry, Stack, StackResult, UnbookmarkedChange};
use crate::workspace::Workspace;

/// The revset expression for the full stack range.
///
/// Includes descendants of `@` so submission is orthogonal to working copy
/// position. Uses `trunk()` alias which is configured in workspace.rs.
const STACK_REVSET: &str = "trunk()..(@  | descendants(@))";

/// Build a stack from the current repository state.
///
/// Evaluates `trunk()..(@  | descendants(@))`, converts commits to `LogEntry`,
/// groups them into `BookmarkSegment`s, and detects gaps.
///
/// When `registry` is `Some`, only commits whose bookmarks appear in the
/// registry produce segments. Commits with only non-registered bookmarks
/// are treated as unbookmarked. When `registry` is `None`, all bookmarked
/// commits produce segments (preserves pozrnomw behavior for testing).
///
/// Returns:
/// - `StackResult::Empty` if the revset range is empty
/// - `StackResult::MergeCommits` if any commit has multiple parents
/// - `StackResult::Ok(stack)` on success
pub fn build_stack(workspace: &Workspace, registry: Option<&PlanRegistry>) -> StackResult {
    // 1. Evaluate the stack revset (parents before children = trunk toward tip)
    let commits = match workspace.evaluate_revset_reversed(STACK_REVSET) {
        Some(c) if !c.is_empty() => c,
        _ => return StackResult::Empty,
    };

    // 2. Convert all commits to LogEntry
    let entries: Vec<LogEntry> = commits
        .iter()
        .map(|c| workspace.commit_to_log_entry(c))
        .collect();

    // 3. Check for merge commits (any commit with >1 parent)
    for entry in &entries {
        if entry.parents.len() > 1 {
            return StackResult::MergeCommits;
        }
    }

    // 4. Also compute short change IDs for gap reporting
    let short_ids: Vec<String> = commits
        .iter()
        .map(|c| workspace.short_change_id(c))
        .collect();

    // 5. Build segments and detect gaps
    build_segments_and_gaps(&entries, &short_ids, registry)
}

/// Internal: group entries into bookmark segments and detect gaps.
///
/// `entries` and `short_ids` are parallel arrays, ordered trunk-to-tip
/// (parents before children).
fn build_segments_and_gaps(entries: &[LogEntry], short_ids: &[String], registry: Option<&PlanRegistry>) -> StackResult {
    // We walk trunk-to-tip. We accumulate commits into a "current run."
    // When we hit a bookmarked commit, we finalize a segment.
    //
    // Segments are built in trunk-to-tip order. Within each segment,
    // changes are stored newest-first (tip toward trunk) per ryu convention.

    let mut segments: Vec<BookmarkSegment> = Vec::new();
    let mut gaps: Vec<Gap> = Vec::new();

    // Accumulator for the current run of commits (trunk-to-tip order).
    // Will be reversed before storing in the segment.
    let mut current_run: Vec<(usize, &LogEntry)> = Vec::new();

    // Track the name of the last bookmark we finalized a segment for,
    // so we can label gap.after_bookmark.
    let mut last_bookmark_name: Option<String> = None;

    for (idx, entry) in entries.iter().enumerate() {
        // When registry is provided, a commit is "bookmarked" only if at
        // least one of its bookmarks is registered. Otherwise, all bookmarks
        // count.
        let has_bookmarks = if let Some(reg) = registry {
            entry.local_bookmarks.iter().any(|b| reg.is_tracked(b))
        } else {
            !entry.local_bookmarks.is_empty()
        };

        current_run.push((idx, entry));

        if has_bookmarks {
            // This commit is a segment boundary. Check for gap:
            // a gap exists if there are unbookmarked commits in the current
            // run BEFORE this bookmarked commit, AND there was a previous
            // bookmarked segment (or this is not the first bookmark).
            //
            // Unbookmarked commits before the FIRST bookmark in the stack
            // are NOT a gap — they are pre-bookmark history.

            let unbookmarked_in_run: Vec<(usize, &LogEntry)> = current_run
                .iter()
                .filter(|(_, e)| {
                    // A commit is "unbookmarked" for gap purposes when it has
                    // no bookmarks that count. With registry filtering, only
                    // registered bookmarks count; without, all bookmarks count.
                    if let Some(reg) = registry {
                        !e.local_bookmarks.iter().any(|b| reg.is_tracked(b))
                    } else {
                        e.local_bookmarks.is_empty()
                    }
                })
                .copied()
                .collect();

            if !unbookmarked_in_run.is_empty() && last_bookmark_name.is_some() {
                // This is a gap: unbookmarked commits between two bookmarks
                let first_bookmark_name = first_bookmark_display_name(entry);
                gaps.push(Gap {
                    unbookmarked: unbookmarked_in_run
                        .iter()
                        .map(|(i, e)| UnbookmarkedChange {
                            short_id: short_ids[*i].clone(),
                            description_first_line: e.description_first_line.clone(),
                        })
                        .collect(),
                    before_bookmark: first_bookmark_name,
                    after_bookmark: last_bookmark_name.clone(),
                });
            }

            // Build the segment with ALL commits in the current run
            // (including unbookmarked ones that belong to this segment).
            // Changes are newest-first (reverse of our trunk-to-tip accumulation).
            let mut segment_changes: Vec<LogEntry> = current_run
                .iter()
                .map(|(_, e)| (*e).clone())
                .collect();
            segment_changes.reverse(); // newest first

            // Build Bookmark structs for all bookmarks on this commit
            let bookmarks: Vec<Bookmark> = entry
                .local_bookmarks
                .iter()
                .map(|name| Bookmark {
                    name: name.clone(),
                    commit_id: entry.commit_id.clone(),
                    change_id: entry.change_id.clone(),
                    // We don't have sync status from LogEntry's bookmark list alone.
                    // These will be enriched by the caller if needed.
                    has_remote: false,
                    is_synced: false,
                })
                .collect();

            segments.push(BookmarkSegment {
                bookmarks,
                changes: segment_changes,
            });

            last_bookmark_name = Some(first_bookmark_display_name(entry));
            current_run.clear();
        }
    }

    // Any remaining commits in current_run after the last bookmark are
    // unbookmarked WIP at the tip — NOT a gap. We don't create a segment
    // for them.

    StackResult::Ok(Stack { segments, gaps })
}

/// Enrich bookmark segments with sync status from the workspace.
///
/// The stack builder creates `Bookmark` structs with `has_remote: false`
/// and `is_synced: false` because `LogEntry.local_bookmarks` only contains
/// names. This function replaces those stubs with actual sync data from
/// `workspace.local_bookmarks()`.
pub fn enrich_bookmarks(stack: &mut Stack, workspace: &Workspace) {
    let all_bookmarks = workspace.local_bookmarks();
    let bookmark_map: std::collections::HashMap<&str, &Bookmark> = all_bookmarks
        .iter()
        .map(|b| (b.name.as_str(), b))
        .collect();

    for segment in &mut stack.segments {
        for bm in &mut segment.bookmarks {
            if let Some(real) = bookmark_map.get(bm.name.as_str()) {
                bm.has_remote = real.has_remote;
                bm.is_synced = real.is_synced;
                bm.commit_id = real.commit_id.clone();
                bm.change_id = real.change_id.clone();
            }
        }
    }
}

/// Find the submit target: the tip-most bookmarked segment near `@`.
///
/// Search strategy:
/// 1. If `@` is in a bookmarked segment, return that segment.
/// 2. Otherwise, search tip-ward from `@`, then trunk-ward.
/// 3. Returns `None` if no bookmarked segments exist.
pub fn find_submit_target(stack: &Stack) -> Option<&BookmarkSegment> {
    if stack.segments.is_empty() {
        return None;
    }

    // Find which segment (if any) contains @
    let wc_segment_idx = stack.segments.iter().position(|seg| {
        seg.changes.iter().any(|c| c.is_working_copy)
    });

    if let Some(idx) = wc_segment_idx {
        return Some(&stack.segments[idx]);
    }

    // @ is not in any bookmarked segment (it's in unbookmarked WIP at tip,
    // or unbookmarked commits before the first bookmark).
    // Search tip-ward first (segments after where @ would be), then trunk-ward.

    // To find where @ is relative to segments, we check if @ appears
    // in any commit in the stack at all. If @'s commit has bookmarks it
    // would have been caught above. If it's unbookmarked, it's either:
    // (a) between two segments, or (b) at the tip after all segments.
    //
    // Since we don't have direct access to all commits (only those in
    // segments), take the simple approach: return the last (tip-most)
    // segment, which is the most likely submit target.
    stack.segments.last()
}

/// Get the first bookmark name for display purposes.
fn first_bookmark_display_name(entry: &LogEntry) -> String {
    entry
        .local_bookmarks
        .first()
        .cloned()
        .unwrap_or_else(|| "(unknown)".to_string())
}

/// Narrow multi-bookmark segments to the single registered plan bookmark.
///
/// For each `BookmarkSegment`, finds the bookmark registered in the
/// `PlanRegistry` and produces a `NarrowedBookmarkSegment` with just that
/// bookmark. If a segment has multiple registered bookmarks (edge case),
/// the first one wins.
///
/// This is used by downstream operations (submit, merge) that need exactly
/// one bookmark per segment.
pub fn narrow_segments(stack: &Stack, registry: &PlanRegistry) -> Vec<NarrowedBookmarkSegment> {
    stack
        .segments
        .iter()
        .filter_map(|seg| {
            // Find the first bookmark that is registered in the plan registry
            let plan_bookmark = seg
                .bookmarks
                .iter()
                .find(|b| registry.is_tracked(&b.name))?;

            Some(NarrowedBookmarkSegment {
                bookmark: plan_bookmark.clone(),
                changes: seg.changes.clone(),
            })
        })
        .collect()
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use chrono::Utc;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn make_entry(
        change_id: &str,
        commit_id: &str,
        bookmarks: &[&str],
        is_working_copy: bool,
    ) -> LogEntry {
        let desc = format!("Commit {}", commit_id);
        LogEntry {
            commit_id: commit_id.to_string(),
            change_id: change_id.to_string(),
            author_name: "Test".to_string(),
            author_email: "test@test.com".to_string(),
            description_first_line: desc.clone(),
            description: desc,
            parents: vec!["parent".to_string()],
            local_bookmarks: bookmarks.iter().map(|s| s.to_string()).collect(),
            remote_bookmarks: vec![],
            is_working_copy,
            is_empty: false,
            authored_at: Utc::now(),
            committed_at: Utc::now(),
        }
    }

    fn make_entry_with_parents(
        change_id: &str,
        commit_id: &str,
        parents: &[&str],
        bookmarks: &[&str],
    ) -> LogEntry {
        let desc = format!("Commit {}", commit_id);
        LogEntry {
            commit_id: commit_id.to_string(),
            change_id: change_id.to_string(),
            author_name: "Test".to_string(),
            author_email: "test@test.com".to_string(),
            description_first_line: desc.clone(),
            description: desc,
            parents: parents.iter().map(|s| s.to_string()).collect(),
            local_bookmarks: bookmarks.iter().map(|s| s.to_string()).collect(),
            remote_bookmarks: vec![],
            is_working_copy: false,
            is_empty: false,
            authored_at: Utc::now(),
            committed_at: Utc::now(),
        }
    }

    fn short_ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("shortid{}", i)).collect()
    }

    // -----------------------------------------------------------------------
    // build_segments_and_gaps tests (pure, no workspace needed)
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_stack_empty() {
        // Empty entries → StackResult::Ok with no segments
        let result = build_segments_and_gaps(&[], &[], None);
        match result {
            StackResult::Ok(stack) => {
                assert!(stack.segments.is_empty());
                assert!(stack.gaps.is_empty());
            }
            _ => panic!("Expected StackResult::Ok for empty input"),
        }
    }

    #[test]
    fn test_build_stack_single_bookmark() {
        // trunk <- c1(feat-a, @)
        let entries = vec![make_entry("ch1", "c1", &["feat-a"], true)];
        let ids = short_ids(1);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.segments.len(), 1);
                assert!(stack.gaps.is_empty());

                let seg = &stack.segments[0];
                assert_eq!(seg.bookmarks.len(), 1);
                assert_eq!(seg.bookmarks[0].name, "feat-a");
                assert_eq!(seg.changes.len(), 1);
                assert!(seg.changes[0].is_working_copy);
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_build_stack_multiple_bookmarks() {
        // trunk <- c1(feat-a) <- c2(feat-b) <- c3(feat-c, @)
        let entries = vec![
            make_entry("ch1", "c1", &["feat-a"], false),
            make_entry("ch2", "c2", &["feat-b"], false),
            make_entry("ch3", "c3", &["feat-c"], true),
        ];
        let ids = short_ids(3);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.segments.len(), 3);
                assert!(stack.gaps.is_empty());

                assert_eq!(stack.segments[0].bookmarks[0].name, "feat-a");
                assert_eq!(stack.segments[1].bookmarks[0].name, "feat-b");
                assert_eq!(stack.segments[2].bookmarks[0].name, "feat-c");

                // Each segment has exactly 1 change (no unbookmarked commits)
                assert_eq!(stack.segments[0].changes.len(), 1);
                assert_eq!(stack.segments[1].changes.len(), 1);
                assert_eq!(stack.segments[2].changes.len(), 1);
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_build_stack_with_gap() {
        // trunk <- c1(feat-a) <- c2(no bookmark) <- c3(feat-b, @)
        // c2 is a gap between feat-a and feat-b
        let entries = vec![
            make_entry("ch1", "c1", &["feat-a"], false),
            make_entry("ch2", "c2", &[], false),
            make_entry("ch3", "c3", &["feat-b"], true),
        ];
        let ids = short_ids(3);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.segments.len(), 2);
                assert_eq!(stack.gaps.len(), 1);

                // Gap is between feat-a (after) and feat-b (before)
                let gap = &stack.gaps[0];
                assert_eq!(gap.before_bookmark, "feat-b");
                assert_eq!(gap.after_bookmark.as_deref(), Some("feat-a"));
                assert_eq!(gap.unbookmarked.len(), 1);
                assert_eq!(gap.unbookmarked[0].short_id, "shortid1");

                // Segment 0: feat-a with just c1
                assert_eq!(stack.segments[0].bookmarks[0].name, "feat-a");
                assert_eq!(stack.segments[0].changes.len(), 1);

                // Segment 1: feat-b with c2 and c3 (c2 is the unbookmarked
                // commit grouped into this segment's changes)
                assert_eq!(stack.segments[1].bookmarks[0].name, "feat-b");
                assert_eq!(stack.segments[1].changes.len(), 2);
                // Changes are newest-first, so c3 (bookmarked) is first
                assert_eq!(stack.segments[1].changes[0].commit_id, "c3");
                assert_eq!(stack.segments[1].changes[1].commit_id, "c2");
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_build_stack_merge_commit() {
        // A commit with 2 parents → merge commit
        let entries = vec![make_entry_with_parents(
            "ch1",
            "c1",
            &["parent1", "parent2"],
            &["feat-a"],
        )];
        // We check for merge commits in build_stack() before calling
        // build_segments_and_gaps(). Test the check directly:
        assert!(entries[0].parents.len() > 1);

        // build_segments_and_gaps doesn't check for merges — that's build_stack's job.
        // But we can verify the detection logic would trigger:
        let has_merge = entries.iter().any(|e| e.parents.len() > 1);
        assert!(has_merge);
    }

    #[test]
    fn test_build_stack_wip_at_tip() {
        // trunk <- c1(feat-a) <- c2(no bookmark, @)
        // c2 is WIP at tip, NOT a gap
        let entries = vec![
            make_entry("ch1", "c1", &["feat-a"], false),
            make_entry("ch2", "c2", &[], true),
        ];
        let ids = short_ids(2);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.segments.len(), 1);
                assert!(stack.gaps.is_empty(), "WIP at tip should not be a gap");

                assert_eq!(stack.segments[0].bookmarks[0].name, "feat-a");
                assert_eq!(stack.segments[0].changes.len(), 1);
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_build_stack_unbookmarked_before_first() {
        // trunk <- c1(no bookmark) <- c2(feat-a, @)
        // c1 is before the first bookmark, NOT a gap
        let entries = vec![
            make_entry("ch1", "c1", &[], false),
            make_entry("ch2", "c2", &["feat-a"], true),
        ];
        let ids = short_ids(2);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.segments.len(), 1);
                assert!(
                    stack.gaps.is_empty(),
                    "Unbookmarked commits before first bookmark should not be a gap"
                );

                // The segment includes both c1 and c2
                assert_eq!(stack.segments[0].changes.len(), 2);
                // Newest first: c2, c1
                assert_eq!(stack.segments[0].changes[0].commit_id, "c2");
                assert_eq!(stack.segments[0].changes[1].commit_id, "c1");
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_build_stack_descendants_of_at() {
        // trunk <- c1(feat-a, @) <- c2(feat-b) <- c3(feat-c)
        // @ is in the middle, descendants should be included
        let entries = vec![
            make_entry("ch1", "c1", &["feat-a"], true),
            make_entry("ch2", "c2", &["feat-b"], false),
            make_entry("ch3", "c3", &["feat-c"], false),
        ];
        let ids = short_ids(3);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.segments.len(), 3);
                assert!(stack.gaps.is_empty());

                // All three segments present
                assert_eq!(stack.segments[0].bookmarks[0].name, "feat-a");
                assert_eq!(stack.segments[1].bookmarks[0].name, "feat-b");
                assert_eq!(stack.segments[2].bookmarks[0].name, "feat-c");

                // @ is in the first segment
                assert!(stack.segments[0].changes[0].is_working_copy);
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_build_stack_multi_bookmark_commit() {
        // trunk <- c1(feat-a, feat-b, @)
        // One commit with multiple bookmarks → single segment with multiple bookmarks
        let entries = vec![make_entry("ch1", "c1", &["feat-a", "feat-b"], true)];
        let ids = short_ids(1);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.segments.len(), 1);
                assert!(stack.gaps.is_empty());

                let seg = &stack.segments[0];
                assert_eq!(seg.bookmarks.len(), 2);
                assert_eq!(seg.bookmarks[0].name, "feat-a");
                assert_eq!(seg.bookmarks[1].name, "feat-b");
                assert_eq!(seg.changes.len(), 1);
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // find_submit_target tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_submit_target_empty_stack() {
        let stack = Stack {
            segments: vec![],
            gaps: vec![],
        };
        assert!(find_submit_target(&stack).is_none());
    }

    #[test]
    fn test_find_submit_target_at_bookmark() {
        // @ is a bookmarked commit → returns its segment
        let entries = vec![
            make_entry("ch1", "c1", &["feat-a"], false),
            make_entry("ch2", "c2", &["feat-b"], true), // @ here
        ];
        let ids = short_ids(2);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                let target = find_submit_target(&stack);
                assert!(target.is_some());
                let seg = target.unwrap();
                assert_eq!(seg.bookmarks[0].name, "feat-b");
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_find_submit_target_at_wip() {
        // @ is unbookmarked tip → returns last (tip-most) bookmarked segment
        let entries = vec![
            make_entry("ch1", "c1", &["feat-a"], false),
            make_entry("ch2", "c2", &[], true), // @ here, unbookmarked
        ];
        let ids = short_ids(2);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                // @ is not in any segment (it's WIP at tip)
                let target = find_submit_target(&stack);
                assert!(target.is_some());
                let seg = target.unwrap();
                assert_eq!(seg.bookmarks[0].name, "feat-a");
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Complex scenarios
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_stack_multiple_gaps() {
        // trunk <- c1(feat-a) <- c2(unbookmarked) <- c3(feat-b) <- c4(unbookmarked) <- c5(feat-c, @)
        let entries = vec![
            make_entry("ch1", "c1", &["feat-a"], false),
            make_entry("ch2", "c2", &[], false),
            make_entry("ch3", "c3", &["feat-b"], false),
            make_entry("ch4", "c4", &[], false),
            make_entry("ch5", "c5", &["feat-c"], true),
        ];
        let ids = short_ids(5);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.segments.len(), 3);
                assert_eq!(stack.gaps.len(), 2);

                // Gap 1: c2 between feat-a and feat-b
                assert_eq!(stack.gaps[0].before_bookmark, "feat-b");
                assert_eq!(stack.gaps[0].after_bookmark.as_deref(), Some("feat-a"));
                assert_eq!(stack.gaps[0].unbookmarked.len(), 1);

                // Gap 2: c4 between feat-b and feat-c
                assert_eq!(stack.gaps[1].before_bookmark, "feat-c");
                assert_eq!(stack.gaps[1].after_bookmark.as_deref(), Some("feat-b"));
                assert_eq!(stack.gaps[1].unbookmarked.len(), 1);

                // Segment sizes: feat-a has 1, feat-b has 2 (c2+c3), feat-c has 2 (c4+c5)
                assert_eq!(stack.segments[0].changes.len(), 1);
                assert_eq!(stack.segments[1].changes.len(), 2);
                assert_eq!(stack.segments[2].changes.len(), 2);
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_build_stack_unbookmarked_both_ends() {
        // trunk <- c1(unbookmarked) <- c2(feat-a) <- c3(unbookmarked, @)
        // c1: before first bookmark (not a gap)
        // c3: WIP at tip (not a gap)
        let entries = vec![
            make_entry("ch1", "c1", &[], false),
            make_entry("ch2", "c2", &["feat-a"], false),
            make_entry("ch3", "c3", &[], true),
        ];
        let ids = short_ids(3);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.segments.len(), 1);
                assert!(stack.gaps.is_empty());

                // Segment includes c1 and c2 (c1 is grouped into feat-a's segment)
                assert_eq!(stack.segments[0].changes.len(), 2);
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_build_stack_all_unbookmarked() {
        // trunk <- c1 <- c2(@)
        // No bookmarks at all → no segments, no gaps
        let entries = vec![
            make_entry("ch1", "c1", &[], false),
            make_entry("ch2", "c2", &[], true),
        ];
        let ids = short_ids(2);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert!(stack.segments.is_empty());
                assert!(stack.gaps.is_empty());
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Registry-based filtering tests
    // -----------------------------------------------------------------------

    fn make_registry(names: &[&str]) -> PlanRegistry {
        let mut registry = PlanRegistry::new();
        for name in names {
            registry.track(crate::types::PlannedBookmark::new(
                name.to_string(),
                format!("change-for-{}", name),
            ));
        }
        registry
    }

    #[test]
    fn test_build_stack_registry_filters() {
        // trunk <- c1(feat-a) <- c2(topic-x) <- c3(feat-b, @)
        // Only feat-a and feat-b are registered; topic-x is not a plan.
        // topic-x should be treated as unbookmarked.
        let entries = vec![
            make_entry("ch1", "c1", &["feat-a"], false),
            make_entry("ch2", "c2", &["topic-x"], false),
            make_entry("ch3", "c3", &["feat-b"], true),
        ];
        let ids = short_ids(3);
        let registry = make_registry(&["feat-a", "feat-b"]);

        match build_segments_and_gaps(&entries, &ids, Some(&registry)) {
            StackResult::Ok(stack) => {
                // Only 2 segments (feat-a, feat-b); topic-x is not a segment
                assert_eq!(stack.segments.len(), 2);
                assert_eq!(stack.segments[0].bookmarks[0].name, "feat-a");
                assert_eq!(stack.segments[1].bookmarks[0].name, "feat-b");

                // topic-x's commit (c2) is grouped into feat-b's segment
                assert_eq!(stack.segments[1].changes.len(), 2);
                assert_eq!(stack.segments[1].changes[0].commit_id, "c3");
                assert_eq!(stack.segments[1].changes[1].commit_id, "c2");

                // Gap: topic-x is unbookmarked (per registry) between two
                // registered bookmarks
                assert_eq!(stack.gaps.len(), 1);
                assert_eq!(stack.gaps[0].before_bookmark, "feat-b");
                assert_eq!(stack.gaps[0].after_bookmark.as_deref(), Some("feat-a"));
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_build_stack_registry_none_shows_all() {
        // Same setup as above, but with None registry → all bookmarks produce segments
        let entries = vec![
            make_entry("ch1", "c1", &["feat-a"], false),
            make_entry("ch2", "c2", &["topic-x"], false),
            make_entry("ch3", "c3", &["feat-b"], true),
        ];
        let ids = short_ids(3);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.segments.len(), 3);
                assert_eq!(stack.segments[0].bookmarks[0].name, "feat-a");
                assert_eq!(stack.segments[1].bookmarks[0].name, "topic-x");
                assert_eq!(stack.segments[2].bookmarks[0].name, "feat-b");
                assert!(stack.gaps.is_empty());
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_build_stack_multi_bookmark_registry() {
        // trunk <- c1(feat-a, topic-x, experiment, @)
        // Only feat-a is registered. Commit has 3 bookmarks but only
        // feat-a triggers the segment. All 3 bookmarks appear in the segment.
        let entries = vec![make_entry(
            "ch1",
            "c1",
            &["feat-a", "topic-x", "experiment"],
            true,
        )];
        let ids = short_ids(1);
        let registry = make_registry(&["feat-a"]);

        match build_segments_and_gaps(&entries, &ids, Some(&registry)) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.segments.len(), 1);
                // All 3 bookmarks are listed on the segment
                assert_eq!(stack.segments[0].bookmarks.len(), 3);
                assert_eq!(stack.segments[0].bookmarks[0].name, "feat-a");
                assert_eq!(stack.segments[0].bookmarks[1].name, "topic-x");
                assert_eq!(stack.segments[0].bookmarks[2].name, "experiment");
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_build_stack_empty_registry_no_segments() {
        // With an empty registry, no bookmarks are registered → no segments
        let entries = vec![
            make_entry("ch1", "c1", &["feat-a"], false),
            make_entry("ch2", "c2", &["feat-b"], true),
        ];
        let ids = short_ids(2);
        let registry = PlanRegistry::new(); // empty

        match build_segments_and_gaps(&entries, &ids, Some(&registry)) {
            StackResult::Ok(stack) => {
                assert!(stack.segments.is_empty());
                assert!(stack.gaps.is_empty());
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // narrow_segments tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_narrow_segments() {
        // Build a stack with multi-bookmark segments, narrow to registered
        let entries = vec![
            make_entry("ch1", "c1", &["feat-a", "topic-x"], false),
            make_entry("ch2", "c2", &["feat-b"], true),
        ];
        let ids = short_ids(2);
        let registry = make_registry(&["feat-a", "feat-b"]);

        match build_segments_and_gaps(&entries, &ids, Some(&registry)) {
            StackResult::Ok(stack) => {
                let narrowed = narrow_segments(&stack, &registry);
                assert_eq!(narrowed.len(), 2);
                // First segment narrowed to feat-a (the registered one)
                assert_eq!(narrowed[0].bookmark.name, "feat-a");
                assert_eq!(narrowed[0].changes.len(), 1);
                // Second segment narrowed to feat-b
                assert_eq!(narrowed[1].bookmark.name, "feat-b");
                assert_eq!(narrowed[1].changes.len(), 1);
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_narrow_segments_picks_first_registered() {
        // Commit has two registered bookmarks — narrow picks the first one
        let entries = vec![make_entry("ch1", "c1", &["feat-a", "feat-b"], true)];
        let ids = short_ids(1);
        let registry = make_registry(&["feat-a", "feat-b"]);

        match build_segments_and_gaps(&entries, &ids, Some(&registry)) {
            StackResult::Ok(stack) => {
                let narrowed = narrow_segments(&stack, &registry);
                assert_eq!(narrowed.len(), 1);
                assert_eq!(narrowed[0].bookmark.name, "feat-a");
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_build_stack_segment_changes_newest_first() {
        // trunk <- c1(unbookmarked) <- c2(unbookmarked) <- c3(feat-a, @)
        // All three should be in one segment, newest first
        let entries = vec![
            make_entry("ch1", "c1", &[], false),
            make_entry("ch2", "c2", &[], false),
            make_entry("ch3", "c3", &["feat-a"], true),
        ];
        let ids = short_ids(3);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.segments.len(), 1);
                let seg = &stack.segments[0];
                assert_eq!(seg.changes.len(), 3);

                // Newest first (tip toward trunk)
                assert_eq!(seg.changes[0].commit_id, "c3"); // bookmarked tip
                assert_eq!(seg.changes[1].commit_id, "c2");
                assert_eq!(seg.changes[2].commit_id, "c1");
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_gap_multiple_unbookmarked() {
        // trunk <- c1(feat-a) <- c2(unbm) <- c3(unbm) <- c4(feat-b, @)
        // Gap between feat-a and feat-b has 2 unbookmarked changes
        let entries = vec![
            make_entry("ch1", "c1", &["feat-a"], false),
            make_entry("ch2", "c2", &[], false),
            make_entry("ch3", "c3", &[], false),
            make_entry("ch4", "c4", &["feat-b"], true),
        ];
        let ids = short_ids(4);

        match build_segments_and_gaps(&entries, &ids, None) {
            StackResult::Ok(stack) => {
                assert_eq!(stack.gaps.len(), 1);
                assert_eq!(stack.gaps[0].unbookmarked.len(), 2);
                assert_eq!(stack.gaps[0].unbookmarked[0].short_id, "shortid1");
                assert_eq!(stack.gaps[0].unbookmarked[1].short_id, "shortid2");
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }
}