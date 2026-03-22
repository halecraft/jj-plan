//! Phase 1: Submission analysis.
//!
//! Identifies what needs to be submitted for a given target bookmark.

use crate::error::{JjPlanError, Result};
use crate::stack_builder::narrow_segments;
use crate::types::{NarrowedBookmarkSegment, PlanRegistry, Stack};

/// Result of submission analysis.
#[derive(Debug, Clone)]
pub struct SubmissionAnalysis {
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

    Ok(SubmissionAnalysis {
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

