//! Phase 1: Submission analysis.
//!
//! Identifies what needs to be submitted for a given target bookmark.

use crate::error::{JjPlanError, Result};
use crate::markdown::strip_scratch_sections;
use crate::plan_file::collect_plan_files;
use crate::stack_builder::narrow_segments;
use crate::types::{NarrowedBookmarkSegment, PlanRegistry, Stack};
use std::path::Path;

/// Result of submission analysis.
#[derive(Debug, Clone)]
pub struct SubmissionAnalysis {
    /// Target bookmark name.
    pub target_bookmark: String,
    /// Segments to submit (from trunk towards target), each narrowed to one bookmark.
    pub segments: Vec<NarrowedBookmarkSegment>,
    /// Default branch name (e.g., "main").
    pub default_branch: String,
}

/// Analyze what needs to be submitted for a given bookmark.
pub fn analyze_submission(
    stack: &Stack,
    registry: &PlanRegistry,
    target_bookmark: Option<&str>,
    default_branch: &str,
) -> Result<SubmissionAnalysis> {
    if stack.segments.is_empty() {
        return Err(JjPlanError::NoStack(
            "No bookmarks found between trunk and working copy. \
             Create a bookmark with: jj plan new <name>"
                .to_string(),
        ));
    }

    let narrowed = narrow_segments(stack, registry);

    if narrowed.is_empty() {
        return Err(JjPlanError::NoStack(
            "No plan-registered bookmarks found in stack. \
             Register one with: jj plan track <bookmark>"
                .to_string(),
        ));
    }

    // Determine target
    let target_index = if let Some(target) = target_bookmark {
        narrowed
            .iter()
            .position(|s| s.bookmark.name == target)
            .ok_or_else(|| JjPlanError::BookmarkNotFound(target.to_string()))?
    } else {
        narrowed.len() - 1 // Default: leaf (tip-most)
    };

    let segments = narrowed[0..=target_index].to_vec();

    let actual_target = segments
        .last()
        .map(|s| s.bookmark.name.clone())
        .unwrap_or_default();

    Ok(SubmissionAnalysis {
        target_bookmark: actual_target,
        segments,
        default_branch: default_branch.to_string(),
    })
}

/// Get the base branch for a segment at a given index.
///
/// The base branch is the previous segment's bookmark name,
/// or the default branch (e.g., "main") for the first segment.
pub fn get_base_branch(analysis: &SubmissionAnalysis, index: usize) -> String {
    if index == 0 {
        analysis.default_branch.clone()
    } else {
        analysis.segments[index - 1].bookmark.name.clone()
    }
}

/// Read plan file content and extract PR title and body.
///
/// Returns `(title, body)` or `None` if no plan file exists for this bookmark.
/// The title is the first line of the plan file.
/// The body is the remainder with `[scratch]` sections stripped and
/// `plan-status: ✅` lines removed.
pub fn plan_file_to_pr_content(plan_dir: &Path, bookmark_name: &str) -> Option<(String, String)> {
    let plan_files = collect_plan_files(plan_dir);

    // Find the plan file for this bookmark
    let entry = plan_files
        .iter()
        .find(|f| f.bookmark_name == bookmark_name)?;

    let content = std::fs::read_to_string(plan_dir.join(&entry.filename)).ok()?;

    if content.trim().is_empty() {
        return None;
    }

    // First line = PR title
    let mut lines = content.lines();
    let title = lines.next()?.to_string();

    if title.trim().is_empty() {
        return None;
    }

    // Remainder = PR body
    let body_raw: String = lines.collect::<Vec<_>>().join("\n");

    // Strip [scratch] sections
    let body_stripped = strip_scratch_sections(&body_raw);

    // Strip plan-status: ✅ lines
    let body = body_stripped
        .lines()
        .filter(|line| !line.starts_with("plan-status: ✅"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    Some((title, body))
}