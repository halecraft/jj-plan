//! Merge execution.
//!
//! Performs merges via the platform API.

use crate::error::Result;
use crate::platform::PlatformService;

use super::plan::{MergePlan, MergeStep};

/// Result of merge execution.
#[derive(Debug, Default)]
pub struct MergeExecutionResult {
    /// Bookmarks that were successfully merged.
    pub merged_bookmarks: Vec<String>,
    /// Bookmark that failed to merge, if any.
    pub failed_bookmark: Option<String>,
    /// Error message for failure.
    pub error_message: Option<String>,
}

/// Execute a merge plan.
///
/// Stops at the first failure or skip (cannot merge out of order).
pub async fn execute_merge(
    plan: &MergePlan,
    platform: &dyn PlatformService,
) -> Result<MergeExecutionResult> {
    let mut result = MergeExecutionResult::default();

    for step in &plan.steps {
        match step {
            MergeStep::Merge {
                bookmark,
                pr_number,
                method,
                ..
            } => {
                match platform.merge_pr(*pr_number, *method).await {
                    Ok(merge_result) => {
                        if merge_result.merged {
                            result.merged_bookmarks.push(bookmark.clone());
                        } else {
                            result.failed_bookmark = Some(bookmark.clone());
                            result.error_message = merge_result.message;
                            break;
                        }
                    }
                    Err(e) => {
                        result.failed_bookmark = Some(bookmark.clone());
                        result.error_message = Some(format!("{e}"));
                        break;
                    }
                }
            }
            MergeStep::RetargetBase {
                bookmark,
                pr_number,
                new_base,
            } => {
                if let Err(e) = platform.update_pr_base(*pr_number, new_base).await {
                    eprintln!(
                        "Warning: failed to retarget #{} ({}): {}",
                        pr_number, bookmark, e
                    );
                    // Continue — retarget failure is non-fatal
                }
            }
            MergeStep::Skip { .. } => {
                // Stop at first skip
                break;
            }
        }
    }

    Ok(result)
}