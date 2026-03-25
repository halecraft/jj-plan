//! Stack builder for jj-plan.
//!
//! Builds a `Stack` from the repository state by evaluating
//! `trunk()..(@  | descendants(@))`, identifying bookmarked segments,
//! and detecting gaps. Merge commits in the range are treated as ordinary
//! unbookmarked entries and folded into the nearest segment.
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

use crate::types::{Bookmark, BookmarkSegment, Gap, LogEntry, MultiStack, NarrowedBookmarkSegment, PlanRegistry, Stack, StackGroup, StackResult, SubmissionChain, UnbookmarkedChange};
use std::collections::{HashMap, HashSet};
use std::result::Result as StdResult;
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
/// - `StackResult::Ok(stack)` on success (merge commits in the range are
///   treated as ordinary unbookmarked entries)
pub fn build_stack(workspace: &Workspace, registry: Option<&PlanRegistry>) -> StackResult {
    debug_log!("build_stack(revset={:?}, registry={})", STACK_REVSET,
        if registry.is_some() { "Some" } else { "None" });

    // 1. Evaluate the stack revset (parents before children = trunk toward tip)
    let commits = match workspace.evaluate_revset_reversed(STACK_REVSET) {
        Some(c) if !c.is_empty() => {
            debug_log!("  revset returned {} commit(s)", c.len());
            c
        }
        _ => {
            debug_log!("  revset returned empty → StackResult::Empty");
            return StackResult::Empty;
        }
    };

    // 2. Convert all commits to LogEntry
    let entries: Vec<LogEntry> = commits
        .iter()
        .map(|c| workspace.commit_to_log_entry(c))
        .collect();

    // 3. (Merge check removed — merges deep in history should not prevent
    // plan sync. The segment builder walks a topologically-sorted array
    // and groups by bookmarks; merge commits are just unbookmarked entries
    // that get folded into the nearest segment or reported as gaps.)

    // 4. Also compute short change IDs for gap reporting
    let short_ids: Vec<String> = commits
        .iter()
        .map(|c| workspace.short_change_id(c))
        .collect();

    // 5. Build segments and detect gaps
    let result = build_segments_and_gaps(&entries, &short_ids, registry);

    match &result {
        StackResult::Ok(stack) => {
            let bookmark_names: Vec<String> = stack.segments.iter().flat_map(|seg|
                seg.bookmarks.iter().map(|b| b.name.clone())
            ).collect();
            debug_log!("  result: {} segment(s), {} gap(s), bookmarks={:?}",
                stack.segments.len(), stack.gaps.len(), bookmark_names);
        }
        StackResult::Empty => {
            debug_log!("  result: Empty (no bookmarked segments)");
        }
    }

    result
}

/// Group plan bookmarks into connected chains by walking parent links.
///
/// Two bookmarks are in the same chain if one is an ancestor of the other
/// (transitively, through commits in the `commit_map`). Uses union-find
/// for efficient grouping.
///
/// Returns a map from group ID → list of indices into `bookmark_commit_ids`.
/// Each group is one independent chain of plans.
fn group_bookmarks_by_ancestry(
    bookmark_commit_ids: &[(String, String)],
    commit_map: &HashMap<String, LogEntry>,
) -> HashMap<usize, Vec<usize>> {
    let n = bookmark_commit_ids.len();
    let mut parent_uf: Vec<usize> = (0..n).collect();

    fn find(parent: &mut Vec<usize>, mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }

    fn union(parent: &mut Vec<usize>, a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent[ra] = rb;
        }
    }

    // Build commit_id → bookmark index map for quick lookup during walks
    let commit_to_bm_idx: HashMap<&str, usize> = bookmark_commit_ids
        .iter()
        .enumerate()
        .map(|(i, (_, cid))| (cid.as_str(), i))
        .collect();

    // For each bookmark, walk ancestors and union with any other bookmark found
    for (i, (_, start_cid)) in bookmark_commit_ids.iter().enumerate() {
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue: Vec<&str> = vec![start_cid.as_str()];

        while let Some(cid) = queue.pop() {
            if !visited.insert(cid) {
                continue;
            }
            // If this commit is another bookmark (not ourselves), union
            if let Some(&j) = commit_to_bm_idx.get(cid) {
                if j != i {
                    union(&mut parent_uf, i, j);
                }
            }
            // Walk parents that are in our commit map
            if let Some(entry) = commit_map.get(cid) {
                for parent_cid in &entry.parents {
                    if commit_map.contains_key(parent_cid.as_str()) {
                        queue.push(parent_cid.as_str());
                    }
                }
            }
        }
    }

    // Collect groups by union-find root
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let root = find(&mut parent_uf, i);
        groups.entry(root).or_default().push(i);
    }

    groups
}

/// Build a multi-stack view of ALL registered plan bookmarks across the repo.
///
/// Unlike `build_stack()` which only sees `trunk()..@`, this function
/// discovers all registered plan bookmarks regardless of working copy
/// position, groups them into independent chains by walking parent links,
/// and returns a `MultiStack` with one `StackGroup` per chain.
///
/// Used by `jj stack` visualization. The sync/flush pipeline continues
/// to use `build_stack()` for the @-relative view.
pub fn build_multi_stack(workspace: &Workspace, registry: &PlanRegistry) -> MultiStack {
    let tracked = registry.tracked_names();
    if tracked.is_empty() {
        return MultiStack { stacks: vec![] };
    }

    // 1. For each registered bookmark, evaluate trunk()..bookmark and collect
    //    all commits into a unified map. Bookmarks that don't resolve are skipped.
    let mut commit_map: HashMap<String, LogEntry> = HashMap::new();
    let mut bookmark_commit_ids: Vec<(String, String)> = Vec::new(); // (bookmark_name, commit_id)

    for bookmark_name in &tracked {
        let revset = format!("trunk()..{}", bookmark_name);
        let commits = match workspace.evaluate_revset_reversed(&revset) {
            Some(c) if !c.is_empty() => c,
            _ => continue,
        };

        for commit in &commits {
            let entry = workspace.commit_to_log_entry(commit);
            let cid = entry.commit_id.clone();
            // Record if this commit carries the plan bookmark
            if entry.local_bookmarks.contains(&bookmark_name.to_string()) {
                bookmark_commit_ids.push((bookmark_name.to_string(), cid.clone()));
            }
            commit_map.entry(cid).or_insert(entry);
        }
    }

    if bookmark_commit_ids.is_empty() {
        return MultiStack { stacks: vec![] };
    }

    // 2. Partition bookmarks: registry `stack` field is the primary grouping
    //    signal. Bookmarks with stack = None fall through to DAG-topology grouping.
    //
    //    Build a map: bookmark_name → index in bookmark_commit_ids
    let bm_name_to_idx: HashMap<&str, usize> = bookmark_commit_ids
        .iter()
        .enumerate()
        .map(|(i, (name, _))| (name.as_str(), i))
        .collect();

    // Partition into registry-grouped (by stack value) and ungrouped (stack = None)
    let mut registry_groups: HashMap<String, Vec<usize>> = HashMap::new(); // stack_id → indices
    let mut ungrouped_indices: Vec<usize> = Vec::new();

    for (bm_name, idx) in &bm_name_to_idx {
        if let Some(planned) = registry.get(bm_name) {
            if let Some(ref stack_id) = planned.stack {
                registry_groups.entry(stack_id.clone()).or_default().push(*idx);
            } else {
                ungrouped_indices.push(*idx);
            }
        } else {
            ungrouped_indices.push(*idx);
        }
    }

    // 3. For ungrouped bookmarks (stack = None), use DAG-topology grouping.
    let ungrouped_bm_cids: Vec<(String, String)> = ungrouped_indices
        .iter()
        .map(|&i| bookmark_commit_ids[i].clone())
        .collect();

    let dag_groups = if !ungrouped_bm_cids.is_empty() {
        let raw_groups = group_bookmarks_by_ancestry(&ungrouped_bm_cids, &commit_map);
        // Map indices back from ungrouped-local to bookmark_commit_ids-global
        let mut remapped: Vec<Vec<usize>> = Vec::new();
        for (_root, local_indices) in raw_groups {
            let global_indices: Vec<usize> = local_indices
                .iter()
                .map(|&li| ungrouped_indices[li])
                .collect();
            remapped.push(global_indices);
        }
        remapped
    } else {
        Vec::new()
    };

    // Collect all bookmark names that belong to explicit (registry-grouped) stacks
    // BEFORE consuming registry_groups. These must be excluded from DAG-topology
    // groups to prevent duplicates.
    let explicitly_grouped: HashSet<&str> = registry_groups
        .values()
        .flat_map(|indices| indices.iter().map(|&i| bookmark_commit_ids[i].0.as_str()))
        .collect();

    // 4. Combine all groups: registry-grouped first, then DAG-grouped.
    //    Each group is (Option<stack_id>, Vec<index into bookmark_commit_ids>).
    let mut all_groups: Vec<(Option<String>, Vec<usize>)> = Vec::new();

    for (stack_id, indices) in registry_groups {
        all_groups.push((Some(stack_id), indices));
    }
    for indices in dag_groups {
        all_groups.push((None, indices));
    }

    // 5. For each group, collect the commits that form that chain and
    //    run build_segments_and_gaps to produce segments.
    let stack_prefix = crate::plan_dir::stack_prefix();
    let mut stack_groups: Vec<StackGroup> = Vec::new();

    for (stack_id, member_indices) in &all_groups {
        // Collect all bookmark names in this group
        let group_bookmark_names: HashSet<&str> = member_indices
            .iter()
            .map(|&i| bookmark_commit_ids[i].0.as_str())
            .collect();

        // Re-evaluate a union revset for this group's bookmarks to get
        // topologically-sorted commits.
        let bookmark_list: Vec<&str> = group_bookmark_names.iter().copied().collect();
        let union_revset = if bookmark_list.len() == 1 {
            format!("trunk()..{}", bookmark_list[0])
        } else {
            let parts: Vec<String> = bookmark_list.iter().map(|b| b.to_string()).collect();
            format!("trunk()..({})", parts.join(" | "))
        };

        let commits = match workspace.evaluate_revset_reversed(&union_revset) {
            Some(c) if !c.is_empty() => c,
            _ => continue,
        };

        let entries: Vec<LogEntry> = commits
            .iter()
            .map(|c| workspace.commit_to_log_entry(c))
            .collect();
        let short_ids: Vec<String> = commits
            .iter()
            .map(|c| workspace.short_change_id(c))
            .collect();

        // Build a per-group filtered registry so that only this group's
        // bookmarks produce segments. Without filtering, bookmarks from
        // other groups that happen to share commits (e.g. a shared trunk-
        // adjacent commit) would appear as segments in multiple groups.
        let mut group_registry = PlanRegistry::new();
        for bm in &registry.bookmarks {
            if group_bookmark_names.contains(bm.name.as_str()) {
                group_registry.track(bm.clone());
            } else if stack_id.is_none() {
                // For DAG-topology groups (implicit stacks), also include
                // ungrouped bookmarks that are NOT claimed by an explicit
                // stack. This lets shared ancestors like `start` appear
                // as segments in the implicit group without duplicating
                // bookmarks that belong to explicit stacks.
                if !explicitly_grouped.contains(bm.name.as_str()) {
                    group_registry.track(bm.clone());
                }
            }
        }

        let result = build_segments_and_gaps(&entries, &short_ids, Some(&group_registry));

        if let StackResult::Ok(stack) = result {
            if stack.segments.is_empty() {
                continue;
            }

            // Check for explicit stack/* base bookmark, but ONLY for groups
            // that have an explicit stack_id. Implicit (DAG-topology) groups
            // don't have base bookmarks. Also only scan segment tip commits
            // (changes[0]) to avoid picking up stack/* bookmarks from shared
            // ancestor commits that belong to a different group.
            let base_bookmark = if stack_id.is_some() {
                stack.segments.iter()
                    .filter_map(|seg| seg.changes.first())
                    .find_map(|entry| {
                        entry.local_bookmarks.iter().find(|b| b.starts_with(&stack_prefix)).cloned()
                    })
            } else {
                None
            };

            // Explicit stack/* base bookmark → use stripped name.
            // Implicit stack (no base bookmark) → empty sentinel, replaced
            // with counter label ("Stack 1", "Stack 2", ...) after sorting.
            let name = base_bookmark.as_ref()
                .map(|b| b.strip_prefix(&stack_prefix).unwrap_or(b).to_string())
                .unwrap_or_default();

            stack_groups.push(StackGroup {
                name,
                segments: stack.segments,
                gaps: stack.gaps,
            });
        }
    }

    // Sort stacks by segment count descending (largest/most-established first),
    // with alphabetical name as tiebreaker for stable ordering.
    stack_groups.sort_by(|a, b| {
        b.segments.len().cmp(&a.segments.len())
            .then_with(|| a.name.cmp(&b.name))
    });

    // Assign counter names to implicit stacks (those with empty sentinel name).
    // Explicit stacks (with stack/* base bookmark) keep their human-chosen name.
    let mut counter = 1usize;
    for group in &mut stack_groups {
        if group.name.is_empty() {
            group.name = format!("Stack {}", counter);
            counter += 1;
        }
    }

    MultiStack { stacks: stack_groups }
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

/// Collect the submission chain from trunk to a target bookmark.
///
/// Walks the stack from trunk to the segment containing `target_bookmark`,
/// collecting all segments and gaps along the way. This is the input to
/// the submit engine's analysis phase.
///
/// Returns an error string if the target bookmark is not found.
pub fn collect_submission_chain(
    stack: &Stack,
    target_bookmark: &str,
) -> StdResult<SubmissionChain, String> {
    // Find the target segment index
    let target_index = stack
        .segments
        .iter()
        .position(|seg| seg.bookmarks.iter().any(|b| b.name == target_bookmark))
        .ok_or_else(|| format!("bookmark '{target_bookmark}' not found in stack"))?;

    // Collect segments from trunk (0) to target (inclusive) for gap filtering
    let segment_bookmark_names: Vec<&str> = stack.segments[0..=target_index]
        .iter()
        .flat_map(|seg| seg.bookmarks.iter().map(|b| b.name.as_str()))
        .collect();

    // Collect gaps that fall within the chain
    // A gap is relevant if its before_bookmark is one of the collected segments' bookmarks
    let gaps: Vec<Gap> = stack
        .gaps
        .iter()
        .filter(|gap| segment_bookmark_names.contains(&gap.before_bookmark.as_str()))
        .cloned()
        .collect();

    Ok(SubmissionChain { gaps })
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
    fn test_build_stack_merge_commit_is_handled() {
        // A merge commit (2 parents) in the range should NOT prevent
        // segment building. The merge is treated as an ordinary entry
        // and folded into the nearest segment.
        let entries = vec![
            make_entry("ch1", "c1", &[], false),                                   // unbookmarked
            make_entry_with_parents("ch2", "c2", &["c1", "other"], &[]),            // merge, unbookmarked
            make_entry("ch3", "c3", &["feat-a"], false),                            // bookmarked
        ];
        let ids = short_ids(entries.len());
        let result = build_segments_and_gaps(&entries, &ids, None);
        match result {
            StackResult::Ok(stack) => {
                // All 3 commits (including the merge) belong to the single segment
                assert_eq!(stack.segments.len(), 1);
                assert_eq!(stack.segments[0].changes.len(), 3);
            }
            other => panic!("expected Ok, got {:?}", other),
        }
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

    // -----------------------------------------------------------------------
    // collect_submission_chain tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_collect_chain_clean() {
        // Build a stack with 3 segments, no gaps
        let stack = Stack {
            segments: vec![
                BookmarkSegment {
                    bookmarks: vec![Bookmark {
                        name: "feat-a".to_string(),
                        commit_id: "aa".to_string(),
                        change_id: "a1".to_string(),
                        has_remote: false,
                        is_synced: false,
                    }],
                    changes: vec![],
                },
                BookmarkSegment {
                    bookmarks: vec![Bookmark {
                        name: "feat-b".to_string(),
                        commit_id: "bb".to_string(),
                        change_id: "b1".to_string(),
                        has_remote: false,
                        is_synced: false,
                    }],
                    changes: vec![],
                },
                BookmarkSegment {
                    bookmarks: vec![Bookmark {
                        name: "feat-c".to_string(),
                        commit_id: "cc".to_string(),
                        change_id: "c1".to_string(),
                        has_remote: false,
                        is_synced: false,
                    }],
                    changes: vec![],
                },
            ],
            gaps: vec![],
        };

        // Chain up to feat-b should include feat-a and feat-b
        let chain = collect_submission_chain(&stack, "feat-b").unwrap();
        // SubmissionChain no longer carries segments (deleted as dead code);
        // verify via gaps only.
        assert!(chain.gaps.is_empty());
    }

    #[test]
    fn test_collect_chain_with_gap() {
        let stack = Stack {
            segments: vec![
                BookmarkSegment {
                    bookmarks: vec![Bookmark {
                        name: "feat-a".to_string(),
                        commit_id: "aa".to_string(),
                        change_id: "a1".to_string(),
                        has_remote: false,
                        is_synced: false,
                    }],
                    changes: vec![],
                },
                BookmarkSegment {
                    bookmarks: vec![Bookmark {
                        name: "feat-b".to_string(),
                        commit_id: "bb".to_string(),
                        change_id: "b1".to_string(),
                        has_remote: false,
                        is_synced: false,
                    }],
                    changes: vec![],
                },
            ],
            gaps: vec![Gap {
                unbookmarked: vec![UnbookmarkedChange {
                    short_id: "xxxx".to_string(),
                    description_first_line: "wip commit".to_string(),
                }],
                before_bookmark: "feat-b".to_string(),
                after_bookmark: Some("feat-a".to_string()),
            }],
        };

        let chain = collect_submission_chain(&stack, "feat-b").unwrap();
        assert_eq!(chain.gaps.len(), 1);
        assert_eq!(chain.gaps[0].before_bookmark, "feat-b");
    }

    // -----------------------------------------------------------------------
    // group_bookmarks_by_ancestry tests (pure, no workspace needed)
    // -----------------------------------------------------------------------

    #[test]
    fn test_group_single_chain() {
        // trunk <- c1 (bm-a) <- c2 (bm-b) <- c3 (bm-c)
        // All three bookmarks are in one linear chain → 1 group
        let mut commit_map = HashMap::new();
        commit_map.insert("c1".to_string(), make_entry_with_parents("ch1", "c1", &["trunk"], &["bm-a"]));
        commit_map.insert("c2".to_string(), make_entry_with_parents("ch2", "c2", &["c1"], &["bm-b"]));
        commit_map.insert("c3".to_string(), make_entry_with_parents("ch3", "c3", &["c2"], &["bm-c"]));

        let bookmark_commit_ids = vec![
            ("bm-a".to_string(), "c1".to_string()),
            ("bm-b".to_string(), "c2".to_string()),
            ("bm-c".to_string(), "c3".to_string()),
        ];

        let groups = group_bookmarks_by_ancestry(&bookmark_commit_ids, &commit_map);
        assert_eq!(groups.len(), 1, "3 linear bookmarks should form 1 group");
        let group = groups.values().next().unwrap();
        assert_eq!(group.len(), 3);
    }

    #[test]
    fn test_group_two_branches() {
        // trunk <- c1 (bm-a) <- c2 (bm-b)
        // trunk <- c3 (bm-c)
        // bm-a and bm-b are connected; bm-c is independent → 2 groups
        let mut commit_map = HashMap::new();
        commit_map.insert("c1".to_string(), make_entry_with_parents("ch1", "c1", &["trunk"], &["bm-a"]));
        commit_map.insert("c2".to_string(), make_entry_with_parents("ch2", "c2", &["c1"], &["bm-b"]));
        commit_map.insert("c3".to_string(), make_entry_with_parents("ch3", "c3", &["trunk"], &["bm-c"]));

        let bookmark_commit_ids = vec![
            ("bm-a".to_string(), "c1".to_string()),
            ("bm-b".to_string(), "c2".to_string()),
            ("bm-c".to_string(), "c3".to_string()),
        ];

        let groups = group_bookmarks_by_ancestry(&bookmark_commit_ids, &commit_map);
        assert_eq!(groups.len(), 2, "2 branches from trunk should form 2 groups");

        // One group should have 2 members (bm-a, bm-b), the other 1 (bm-c)
        let mut sizes: Vec<usize> = groups.values().map(|g| g.len()).collect();
        sizes.sort();
        assert_eq!(sizes, vec![1, 2]);
    }

    #[test]
    fn test_group_empty() {
        let commit_map: HashMap<String, LogEntry> = HashMap::new();
        let bookmark_commit_ids: Vec<(String, String)> = vec![];
        let groups = group_bookmarks_by_ancestry(&bookmark_commit_ids, &commit_map);
        assert!(groups.is_empty());
    }

    #[test]
    fn test_group_three_independent() {
        // Three bookmarks with no shared ancestry in the map → 3 groups
        let mut commit_map = HashMap::new();
        commit_map.insert("c1".to_string(), make_entry_with_parents("ch1", "c1", &["trunk"], &["bm-a"]));
        commit_map.insert("c2".to_string(), make_entry_with_parents("ch2", "c2", &["trunk"], &["bm-b"]));
        commit_map.insert("c3".to_string(), make_entry_with_parents("ch3", "c3", &["trunk"], &["bm-c"]));

        let bookmark_commit_ids = vec![
            ("bm-a".to_string(), "c1".to_string()),
            ("bm-b".to_string(), "c2".to_string()),
            ("bm-c".to_string(), "c3".to_string()),
        ];

        let groups = group_bookmarks_by_ancestry(&bookmark_commit_ids, &commit_map);
        assert_eq!(groups.len(), 3, "3 independent bookmarks should form 3 groups");
    }

    #[test]
    fn test_group_diamond_merge() {
        // trunk <- c1 (bm-a) <- c2
        //                    <- c3 (bm-b)
        // c2 has parents [c1], c3 has parents [c1]
        // bm-a and bm-b share ancestor c1 → 1 group
        let mut commit_map = HashMap::new();
        commit_map.insert("c1".to_string(), make_entry_with_parents("ch1", "c1", &["trunk"], &["bm-a"]));
        commit_map.insert("c2".to_string(), make_entry_with_parents("ch2", "c2", &["c1"], &[]));
        commit_map.insert("c3".to_string(), make_entry_with_parents("ch3", "c3", &["c1"], &["bm-b"]));

        let bookmark_commit_ids = vec![
            ("bm-a".to_string(), "c1".to_string()),
            ("bm-b".to_string(), "c3".to_string()),
        ];

        let groups = group_bookmarks_by_ancestry(&bookmark_commit_ids, &commit_map);
        assert_eq!(groups.len(), 1, "bm-b descends from bm-a via c1 → same group");
    }

    #[test]
    fn test_collect_chain_not_found() {
        let stack = Stack {
            segments: vec![BookmarkSegment {
                bookmarks: vec![Bookmark {
                    name: "feat-a".to_string(),
                    commit_id: "aa".to_string(),
                    change_id: "a1".to_string(),
                    has_remote: false,
                    is_synced: false,
                }],
                changes: vec![],
            }],
            gaps: vec![],
        };

        let result = collect_submission_chain(&stack, "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }
}