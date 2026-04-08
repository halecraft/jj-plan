//! Stack comment generation and detection.
//!
//! Pure functions for generating the stack navigation markdown comment
//! that is posted on each PR, and for finding an existing jj-plan
//! comment to update in place (idempotent).

use crate::types::PrComment;

/// HTML comment marker that identifies jj-plan stack comments.
/// Used for idempotent find-and-update: if a comment contains this
/// marker, it's ours and should be updated rather than duplicated.
pub const STACK_COMMENT_MARKER: &str = "<!-- jj-plan stack -->";

/// Generate the stack comment body for a specific PR in a chain.
///
/// `chain` is the full ordered list of PRs in the stack (trunk-to-tip),
/// each entry being `(bookmark, pr_number, title)`.
///
/// `current_bookmark` identifies which row should be highlighted as
/// "you are here" with bold formatting and a 👈 indicator.
///
/// Returns a complete markdown string ready to post as a PR comment.
pub fn generate_stack_comment(
    chain: &[(String, u64, String)], // (bookmark, pr_number, title)
    current_bookmark: &str,
    default_branch: &str,
) -> String {
    let mut lines = vec![
        // Marker (invisible in rendered markdown)
        STACK_COMMENT_MARKER.to_string(),
        // Header
        "### Stack".to_string(),
        String::new(),
        // Table header
        "| | PR | Plan |".to_string(),
        "|---|---|---|".to_string(),
    ];

    // Table rows in tip-to-trunk display order.
    // The ordinal is the dependency index (01 = trunk-nearest, matching
    // `jj plan go <N>` and the NN in filenames).
    let num = chain.len();
    let reversed: Vec<_> = chain.iter().rev().collect();
    for (i, (bookmark, pr_number, title)) in reversed.iter().enumerate() {
        // display position i=0 is tip, i=num-1 is trunk-nearest
        // dependency index: tip = num, trunk-nearest = 1
        let dep_index = num - i;
        let is_current = bookmark == current_bookmark;

        if is_current {
            lines.push(format!(
                "| **{dep_index}** | **#{pr_number} {bookmark}** | **{title}** 👈 |"
            ));
        } else {
            lines.push(format!("| {dep_index} | #{pr_number} {bookmark} | {title} |"));
        }
    }

    // Base branch row (trunk)
    lines.push(format!("| | ◆ {default_branch} | |"));

    lines.join("\n")
}

/// Find the jj-plan stack comment in a list of PR comments.
///
/// Returns the comment ID if found, `None` otherwise.
/// Scans comment bodies for the `STACK_COMMENT_MARKER` string.
pub fn find_existing_comment(comments: &[PrComment]) -> Option<u64> {
    comments
        .iter()
        .find(|c| c.body.contains(STACK_COMMENT_MARKER))
        .map(|c| c.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_stack_comment_single_pr() {
        let chain = vec![("feat-auth".to_string(), 42, "Extract auth module".to_string())];
        let result = generate_stack_comment(&chain, "feat-auth", "main");

        assert!(result.contains(STACK_COMMENT_MARKER));
        assert!(result.contains("### Stack"));
        // Single PR: dependency index = 1, bold with 👈
        assert!(result.contains("**1**"));
        assert!(result.contains("**#42 feat-auth**"));
        assert!(result.contains("👈"));
        // Base branch row
        assert!(result.contains("◆ main"));
    }

    #[test]
    fn test_generate_stack_comment_highlights_current() {
        // Chain is passed in trunk-to-tip order (from call site):
        // [feat-auth (trunk, dep=1), feat-session (middle, dep=2), feat-api (tip, dep=3)]
        // After internal reversal, display is tip-first:
        // Row: dep=3 feat-api, dep=2 feat-session (👈), dep=1 feat-auth, ◆ main
        let chain = vec![
            ("feat-auth".to_string(), 42, "Extract auth module".to_string()),
            (
                "feat-session".to_string(),
                43,
                "Implement session management".to_string(),
            ),
            ("feat-api".to_string(), 44, "Add API endpoints".to_string()),
        ];
        let result = generate_stack_comment(&chain, "feat-session", "main");

        // Row dep=3: feat-api (tip), NOT bold
        assert!(result.contains("| 3 | #44 feat-api | Add API endpoints |"));
        // Row dep=2: feat-session (middle), bold with 👈
        assert!(result.contains("| **2** | **#43 feat-session** | **Implement session management** 👈 |"));
        // Row dep=1: feat-auth (trunk-nearest), NOT bold
        assert!(result.contains("| 1 | #42 feat-auth | Extract auth module |"));
        // Base branch row at bottom
        assert!(result.contains("◆ main"));
    }

    #[test]
    fn test_generate_stack_comment_includes_marker() {
        let chain = vec![("feat-a".to_string(), 1, "Title A".to_string())];
        let result = generate_stack_comment(&chain, "feat-a", "main");

        assert!(result.starts_with(STACK_COMMENT_MARKER));
    }

    #[test]
    fn test_find_existing_comment_found() {
        let comments = vec![
            PrComment {
                id: 100,
                body: "Some unrelated comment".to_string(),
            },
            PrComment {
                id: 200,
                body: format!("{}\n### Stack\n\n| | PR | Plan |", STACK_COMMENT_MARKER),
            },
            PrComment {
                id: 300,
                body: "Another comment".to_string(),
            },
        ];

        assert_eq!(find_existing_comment(&comments), Some(200));
    }

    #[test]
    fn test_find_existing_comment_not_found() {
        let comments = vec![
            PrComment {
                id: 100,
                body: "Some unrelated comment".to_string(),
            },
            PrComment {
                id: 200,
                body: "Another comment".to_string(),
            },
        ];

        assert_eq!(find_existing_comment(&comments), None);
    }

    #[test]
    fn test_find_existing_comment_empty_list() {
        let comments: Vec<PrComment> = vec![];
        assert_eq!(find_existing_comment(&comments), None);
    }

    #[test]
    fn test_generate_stack_comment_no_current_match() {
        // If current_bookmark doesn't match any entry, no row is highlighted.
        // Chain trunk-to-tip: [feat-a (dep=1), feat-b (dep=2)]. After reversal: [feat-b, feat-a].
        let chain = vec![
            ("feat-a".to_string(), 1, "Title A".to_string()),
            ("feat-b".to_string(), 2, "Title B".to_string()),
        ];
        let result = generate_stack_comment(&chain, "feat-nonexistent", "main");

        // Neither row should be bold; tip-first with dependency indices
        assert!(result.contains("| 2 | #2 feat-b | Title B |"));
        assert!(result.contains("| 1 | #1 feat-a | Title A |"));
        assert!(!result.contains("👈"));
        // Base branch row
        assert!(result.contains("◆ main"));
    }
}