//! `jj plan summary` — structured plan introspection for LLMs and humans.
//!
//! This module contains the data model, pure extraction functions, and
//! formatting logic for `jj plan summary`. The design follows GATHER → PLAN
//! → EXECUTE: effectful data collection produces a `SummaryInput`, the pure
//! `build_summary` transforms it into a `SummaryOutput`, and formatters
//! render to text or JSON.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::error::Result;
use crate::jj_binary::JjBinary;
use crate::markdown::{extract_headings, HeadingInfo, PlanDocument};
use crate::plan_dir::{self, StatusIndicators};
use crate::stack_render::StackFormat;
use crate::types::{description_first_line, PlanRegistry};
use crate::workspace::Workspace;
use crate::wrap::SyncChangeView;

// ---------------------------------------------------------------------------
// Output model (serializable for --json)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SummaryOutput {
    pub title: String,
    pub bookmark: String,
    pub change_id: String,
    pub status: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub line_count: usize,
    pub word_count: usize,
    pub outline: Vec<OutlineEntry>,
    pub phases: Vec<PhaseInfo>,
    pub phase_summary: String,
    pub active_phase: Option<ActivePhaseInfo>,
    pub references: Vec<CrossReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_stat: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<StackSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_body: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutlineEntry {
    pub line: usize,
    pub level: u8,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaseInfo {
    pub name: String,
    pub status: String,
    pub task_total: usize,
    pub task_complete: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivePhaseInfo {
    pub name: String,
    pub tasks_complete: usize,
    pub tasks_total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CrossReference {
    pub change_id: String,
    pub bookmark: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StackSummary {
    pub position: usize,
    pub total: usize,
    pub entries: Vec<StackSummaryEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StackSummaryEntry {
    pub bookmark: String,
    pub change_id: String,
    pub is_done: bool,
    pub is_working_copy: bool,
    pub is_target: bool,
    pub title: String,
}

// ---------------------------------------------------------------------------
// Input model (gathered by the imperative shell)
// ---------------------------------------------------------------------------

pub struct SummaryInput {
    pub description: String,
    pub bookmark: String,
    pub change_id: String,
    pub stack_views: Option<Vec<SyncChangeView>>,
    pub diff_stat: Option<String>,
    /// (change_id, bookmark, title) — resolved in the gather phase.
    pub cross_ref_descriptions: Vec<(String, Option<String>, Option<String>)>,
    pub indicators: StatusIndicators,
    pub include_raw_body: bool,
}

// ---------------------------------------------------------------------------
// Stack display mode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackMode {
    Full,
    Minimal,
    Quiet,
}

// ---------------------------------------------------------------------------
// Pure extraction functions
// ---------------------------------------------------------------------------

/// Map `HeadingInfo` to `OutlineEntry` (drop byte_offset, keep line/level/text).
pub fn build_outline(headings: &[HeadingInfo]) -> Vec<OutlineEntry> {
    headings
        .iter()
        .map(|h| OutlineEntry {
            line: h.line,
            level: h.level,
            text: h.text.clone(),
        })
        .collect()
}

/// Extract phase information from level-1 headings whose text contains "phase"
/// (case-insensitive). Tasks are counted by scanning lines between consecutive
/// level-1 headings for `- [indicator]` patterns.
pub fn extract_phases(
    headings: &[HeadingInfo],
    raw: &str,
    indicators: &StatusIndicators,
) -> Vec<PhaseInfo> {
    // Collect indices of level-1 headings that contain "phase"
    let phase_indices: Vec<usize> = headings
        .iter()
        .enumerate()
        .filter(|(_, h)| h.level == 1 && h.text.to_lowercase().contains("phase"))
        .map(|(i, _)| i)
        .collect();

    if phase_indices.is_empty() {
        return Vec::new();
    }

    let all_indicators = indicators.all();

    phase_indices
        .iter()
        .map(|&idx| {
            let h = &headings[idx];

            // Determine which status indicator appears in the heading text
            let status = all_indicators
                .iter()
                .find(|ind| h.text.contains(**ind))
                .map(|s| (*s).to_string())
                .unwrap_or_default();

            // Find the byte range for this phase's content: from this heading's
            // offset to the next level-1 heading's offset (or end of input).
            let start = h.byte_offset;
            let end = headings
                .iter()
                .skip(idx + 1)
                .find(|next| next.level == 1)
                .map(|next| next.byte_offset)
                .unwrap_or(raw.len());

            let section = &raw[start..end];

            // Count tasks: lines starting with `- ` followed by any indicator
            let mut task_total = 0;
            let mut task_complete = 0;
            for line in section.lines() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("- ") {
                    let after_dash = &trimmed[2..];
                    if all_indicators.iter().any(|ind| after_dash.starts_with(ind)) {
                        task_total += 1;
                        if after_dash.starts_with(indicators.done.as_str()) {
                            task_complete += 1;
                        }
                    }
                }
            }

            PhaseInfo {
                name: h.text.clone(),
                status,
                task_total,
                task_complete,
            }
        })
        .collect()
}

/// Produce a compact phase summary string and identify the active (WIP) phase.
///
/// Summary format: `"2/3 complete (✅✅🔴)"`.
/// Active phase: the first phase whose status matches the WIP indicator.
pub fn summarize_phases(
    phases: &[PhaseInfo],
    indicators: &StatusIndicators,
) -> (String, Option<ActivePhaseInfo>) {
    if phases.is_empty() {
        return (String::new(), None);
    }

    let done_count = phases
        .iter()
        .filter(|p| p.status == indicators.done)
        .count();
    let total = phases.len();

    let status_chars: String = phases
        .iter()
        .map(|p| {
            if p.status.is_empty() {
                "?"
            } else {
                p.status.as_str()
            }
        })
        .collect();

    let summary = format!("{}/{} complete ({})", done_count, total, status_chars);

    let active = phases
        .iter()
        .find(|p| p.status == indicators.wip)
        .map(|p| ActivePhaseInfo {
            name: p.name.clone(),
            tasks_complete: p.task_complete,
            tasks_total: p.task_total,
        });

    (summary, active)
}

/// Scan for `jj:[a-z]{8,}` patterns in a string, return deduplicated change IDs.
pub fn extract_cross_ref_ids(raw: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    // Simple scanner: find "jj:" then collect [a-z] chars
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == b'j' && bytes[i + 1] == b'j' && bytes[i + 2] == b':' {
            let start = i + 3;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_lowercase() {
                end += 1;
            }
            if end - start >= 8 {
                let id = &raw[start..end];
                if seen.insert(id.to_string()) {
                    result.push(id.to_string());
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }

    result
}

/// Count words by splitting on whitespace.
pub fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Build a `StackSummary` from resolved `SyncChangeView`s.
///
/// The `target_change_id` is compared against each entry's `change_id`
/// to set the `is_target` flag. Position is 1-based from the tip.
pub fn build_stack_summary(
    views: &[SyncChangeView],
    target_change_id: &str,
) -> StackSummary {
    let entries: Vec<StackSummaryEntry> = views
        .iter()
        .map(|v| StackSummaryEntry {
            bookmark: v.bookmark_name.clone(),
            change_id: v.change_id.clone(),
            is_done: v.is_done(),
            is_working_copy: v.is_working_copy,
            is_target: v.change_id == target_change_id,
            title: description_first_line(&v.description).to_string(),
        })
        .collect();

    let position = entries
        .iter()
        .position(|e| e.is_target)
        .map(|i| i + 1) // 1-based
        .unwrap_or(0);

    StackSummary {
        position,
        total: entries.len(),
        entries,
    }
}

// ---------------------------------------------------------------------------
// Pure assembly: SummaryInput → SummaryOutput
// ---------------------------------------------------------------------------

/// Build a `SummaryOutput` from a `SummaryInput`.
///
/// This is the pure functional core — all side effects (workspace reads,
/// subprocess calls) have already been resolved into the `SummaryInput`.
pub fn build_summary(input: SummaryInput) -> SummaryOutput {
    let doc = PlanDocument::parse(&input.description);
    let headings = extract_headings(&input.description);

    let outline = build_outline(&headings);
    let phases = extract_phases(&headings, &input.description, &input.indicators);
    let (phase_summary, active_phase) = summarize_phases(&phases, &input.indicators);

    let references: Vec<CrossReference> = input
        .cross_ref_descriptions
        .into_iter()
        .map(|(change_id, bookmark, title)| CrossReference {
            change_id,
            bookmark,
            title,
        })
        .collect();

    let stack = input
        .stack_views
        .as_ref()
        .map(|views| build_stack_summary(views, &input.change_id));

    let raw_body = if input.include_raw_body {
        Some(doc.body().to_string())
    } else {
        None
    };

    SummaryOutput {
        title: doc.title().to_string(),
        bookmark: input.bookmark,
        change_id: input.change_id,
        status: doc.metadata().get("status").cloned(),
        metadata: doc.metadata().clone(),
        line_count: input.description.lines().count(),
        word_count: count_words(&input.description),
        outline,
        phases,
        phase_summary,
        active_phase,
        references,
        diff_stat: input.diff_stat,
        stack,
        raw_body,
    }
}

// ---------------------------------------------------------------------------
// Formatters
// ---------------------------------------------------------------------------

/// Format as human-readable text for stdout.
pub fn format_text(summary: &SummaryOutput, stack_mode: StackMode) -> String {
    let mut out = String::new();

    // Header
    out.push_str(&format!("Plan: {}\n", summary.title));
    out.push_str(&format!("Bookmark: {}\n", summary.bookmark));
    out.push_str(&format!("Change-Id: {}\n", summary.change_id));
    if let Some(status) = &summary.status {
        out.push_str(&format!("Status: {}\n", status));
    }

    // Metadata
    out.push_str(&format!(
        "\nMetadata:\n  lines: {}\n  words: {}\n",
        summary.line_count, summary.word_count
    ));
    for (key, value) in &summary.metadata {
        out.push_str(&format!("  {}: {}\n", key, value));
    }

    // Outline
    if !summary.outline.is_empty() {
        out.push_str("\nOutline:\n");
        for entry in &summary.outline {
            let indent = "  ".repeat(entry.level.saturating_sub(1) as usize);
            out.push_str(&format!("  L{:<4} {}{}\n", entry.line, indent, entry.text));
        }
    }

    // Phases
    if !summary.phase_summary.is_empty() {
        out.push_str(&format!("\nPhases: {}\n", summary.phase_summary));
    }
    if let Some(active) = &summary.active_phase {
        out.push_str(&format!(
            "Active phase: {} — {}/{} tasks\n",
            active.name, active.tasks_complete, active.tasks_total
        ));
    }

    // Cross-references
    if !summary.references.is_empty() {
        out.push_str("\nReferences:\n");
        for r in &summary.references {
            let target = match (&r.bookmark, &r.title) {
                (Some(bm), Some(title)) => format!("{} \"{}\"", bm, truncate(title, 60)),
                (Some(bm), None) => bm.clone(),
                (None, Some(title)) => format!("\"{}\"", truncate(title, 60)),
                (None, None) => "(unresolved)".to_string(),
            };
            out.push_str(&format!("  jj:{} → {}\n", r.change_id, target));
        }
    }

    // Diff stat
    if let Some(stat) = &summary.diff_stat {
        out.push_str(&format!("\nDiff stat:\n{}\n", indent_lines(stat, "  ")));
    }

    // Stack
    if let Some(stack) = &summary.stack {
        if stack_mode != StackMode::Quiet {
            out.push_str(&format!("\nStack ({} of {}):\n", stack.position, stack.total));
            for entry in &stack.entries {
                let marker = if entry.is_target { " ← this plan" } else { "" };

                match stack_mode {
                    StackMode::Full => {
                        let status = if entry.is_done {
                            "✓"
                        } else if entry.is_working_copy {
                            "~"
                        } else {
                            " "
                        };
                        let node = if entry.is_working_copy { "◉" } else { "○" };
                        out.push_str(&format!(
                            "  {} {} {} ({}) {}{}\n",
                            node,
                            entry.bookmark,
                            entry.change_id,
                            status,
                            truncate(&entry.title, 60),
                            marker,
                        ));
                    }
                    StackMode::Minimal => {
                        let status = if entry.is_done { "✓" } else { " " };
                        out.push_str(&format!(
                            "  {} {} {}{}\n",
                            entry.bookmark,
                            entry.change_id,
                            status,
                            marker,
                        ));
                    }
                    StackMode::Quiet => unreachable!(),
                }
            }
        }
    }

    out
}

/// Format as JSON for machine consumption.
pub fn format_json(summary: &SummaryOutput) -> String {
    serde_json::to_string_pretty(summary).unwrap_or_else(|e| format!("{{\"error\": \"{}\"}}", e))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.min(s.len())])
    }
}

fn indent_lines(s: &str, prefix: &str) -> String {
    s.lines()
        .map(|line| format!("{}{}", prefix, line))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Command entry point: GATHER → PLAN → EXECUTE
// ---------------------------------------------------------------------------

/// Run `jj plan summary` — output a structured plan summary to stdout.
///
/// ## Args
///
/// - Positional: target revset (default `@`)
/// - `--json`: output as JSON instead of text
/// - `--no-diff-stat`: suppress diff stat section
/// - `--stack=full|minimal|quiet`: control stack verbosity (default: full)
#[allow(clippy::too_many_arguments)]
pub fn run_summary(
    jj: &JjBinary,
    _plan_dir: &crate::plan_dir::PlanDir,
    args: &[String],
    workspace: &Workspace,
    registry: &PlanRegistry,
    _format: StackFormat,
) -> Result<i32> {
    // ------------------------------------------------------------------
    // Parse args
    // ------------------------------------------------------------------
    let mut target = "@".to_string();
    let mut json_mode = false;
    let mut diff_stat = true;
    let mut stack_mode = StackMode::Full;

    for arg in args {
        match arg.as_str() {
            "--json" => json_mode = true,
            "--no-diff-stat" => diff_stat = false,
            s if s.starts_with("--stack=") => {
                stack_mode = match &s[8..] {
                    "minimal" => StackMode::Minimal,
                    "quiet" => StackMode::Quiet,
                    _ => StackMode::Full,
                };
            }
            _ if !arg.starts_with('-') => target = arg.clone(),
            _ => {} // ignore unknown flags
        }
    }

    // ------------------------------------------------------------------
    // GATHER — collect all data from effectful sources
    // ------------------------------------------------------------------

    // Resolve description
    let description = match workspace.read_description_at(&target) {
        Some(d) => d,
        None => {
            eprintln!("jj plan summary: could not read description for '{}'", target);
            return Ok(1);
        }
    };

    // Resolve short change ID
    let change_id = workspace
        .resolve_change_id(&target)
        .unwrap_or_else(|| target.clone());

    // Resolve bookmark name: scan registry for matching change ID
    let bookmark = registry
        .bookmarks
        .iter()
        .find(|b| {
            workspace
                .short_change_id_from_hex(&b.change_id)
                .as_deref()
                == Some(change_id.as_str())
        })
        .map(|b| b.name.clone())
        .unwrap_or_default();

    // Extract cross-ref IDs and batch-resolve
    let cross_ref_ids = extract_cross_ref_ids(&description);
    let cross_ref_descriptions: Vec<(String, Option<String>, Option<String>)> = cross_ref_ids
        .into_iter()
        .map(|ref_id| {
            let title = workspace
                .read_description_at(&ref_id)
                .map(|d| description_first_line(&d).to_string());
            let ref_bookmark = registry
                .bookmarks
                .iter()
                .find(|b| {
                    workspace
                        .short_change_id_from_hex(&b.change_id)
                        .as_deref()
                        == Some(ref_id.as_str())
                })
                .map(|b| b.name.clone());
            (ref_id, ref_bookmark, title)
        })
        .collect();

    // Build stack views
    let stack_views = if stack_mode != StackMode::Quiet {
        crate::wrap::build_sync_views(workspace, registry)
    } else {
        None
    };

    // Capture diff stat
    let diff_stat_output = if diff_stat {
        jj.run_silent(&["diff", "--stat", "-r", &target])
            .ok()
            .and_then(|(status, stdout, _)| {
                if status.success() && !stdout.trim().is_empty() {
                    Some(stdout.trim_end().to_string())
                } else {
                    None
                }
            })
    } else {
        None
    };

    // Read status indicators
    let indicators = plan_dir::resolve_status_indicators();

    // Assemble input
    let input = SummaryInput {
        description,
        bookmark,
        change_id,
        stack_views,
        diff_stat: diff_stat_output,
        cross_ref_descriptions,
        indicators,
        include_raw_body: json_mode,
    };

    // ------------------------------------------------------------------
    // PLAN — pure transformation
    // ------------------------------------------------------------------
    let output = build_summary(input);

    // ------------------------------------------------------------------
    // EXECUTE — format and print to stdout
    // ------------------------------------------------------------------
    if json_mode {
        println!("{}", format_json(&output));
    } else {
        print!("{}", format_text(&output, stack_mode));
    }

    Ok(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_dir::resolve_status_indicators;

    fn default_indicators() -> StatusIndicators {
        resolve_status_indicators()
    }

    // ── build_outline ─────────────────────────────────────────────────

    #[test]
    fn test_build_outline() {
        let headings = vec![
            HeadingInfo {
                level: 1,
                text: "Background".to_string(),
                byte_offset: 20,
                line: 3,
            },
            HeadingInfo {
                level: 2,
                text: "Tasks".to_string(),
                byte_offset: 100,
                line: 10,
            },
        ];
        let outline = build_outline(&headings);
        assert_eq!(outline.len(), 2);
        assert_eq!(outline[0].line, 3);
        assert_eq!(outline[0].level, 1);
        assert_eq!(outline[0].text, "Background");
        assert_eq!(outline[1].line, 10);
        assert_eq!(outline[1].level, 2);
        assert_eq!(outline[1].text, "Tasks");
    }

    // ── extract_phases ────────────────────────────────────────────────

    #[test]
    fn test_extract_phases_with_tasks() {
        let raw = "\
feat: my plan

# ✅ Phase 1: Setup

## Tasks

- ✅ Create the thing
- ✅ Wire the thing

# 🟡 Phase 2: Implementation

## Tasks

- ✅ Write extraction code
- 🟡 Write wiring code
- 🔴 Write tests

# 🔴 Phase 3: Docs

## Tasks

- 🔴 Update README
";
        let headings = extract_headings(raw);
        let indicators = default_indicators();
        let phases = extract_phases(&headings, raw, &indicators);

        assert_eq!(phases.len(), 3);

        assert_eq!(phases[0].name, "✅ Phase 1: Setup");
        assert_eq!(phases[0].status, "✅");
        assert_eq!(phases[0].task_total, 2);
        assert_eq!(phases[0].task_complete, 2);

        assert_eq!(phases[1].name, "🟡 Phase 2: Implementation");
        assert_eq!(phases[1].status, "🟡");
        assert_eq!(phases[1].task_total, 3);
        assert_eq!(phases[1].task_complete, 1);

        assert_eq!(phases[2].name, "🔴 Phase 3: Docs");
        assert_eq!(phases[2].status, "🔴");
        assert_eq!(phases[2].task_total, 1);
        assert_eq!(phases[2].task_complete, 0);
    }

    #[test]
    fn test_extract_phases_no_phases() {
        let raw = "# Background\n\nSome text.\n\n# Tasks\n\n- Do things\n";
        let headings = extract_headings(raw);
        let indicators = default_indicators();
        let phases = extract_phases(&headings, raw, &indicators);
        assert!(phases.is_empty());
    }

    // ── summarize_phases ──────────────────────────────────────────────

    #[test]
    fn test_summarize_phases_mixed() {
        let indicators = default_indicators();
        let phases = vec![
            PhaseInfo {
                name: "✅ Phase 1".to_string(),
                status: "✅".to_string(),
                task_total: 3,
                task_complete: 3,
            },
            PhaseInfo {
                name: "🟡 Phase 2".to_string(),
                status: "🟡".to_string(),
                task_total: 5,
                task_complete: 2,
            },
            PhaseInfo {
                name: "🔴 Phase 3".to_string(),
                status: "🔴".to_string(),
                task_total: 4,
                task_complete: 0,
            },
        ];

        let (summary, active) = summarize_phases(&phases, &indicators);

        assert_eq!(summary, "1/3 complete (✅🟡🔴)");
        assert!(active.is_some());
        let active = active.unwrap();
        assert_eq!(active.name, "🟡 Phase 2");
        assert_eq!(active.tasks_complete, 2);
        assert_eq!(active.tasks_total, 5);
    }

    // ── extract_cross_ref_ids ─────────────────────────────────────────

    #[test]
    fn test_extract_cross_ref_ids() {
        let raw = "\
See jj:abcdefgh for context.
Also references jj:klmnopqr and jj:abcdefgh again (dedup).
Short jj:abc should be ignored.
Long jj:stuvwxyz is fine.
";
        let refs = extract_cross_ref_ids(raw);
        assert_eq!(refs, vec!["abcdefgh", "klmnopqr", "stuvwxyz"]);
    }

    // ── count_words ───────────────────────────────────────────────────

    #[test]
    fn test_count_words() {
        assert_eq!(count_words("hello world"), 2);
        assert_eq!(count_words("  spaced   out  "), 2);
        assert_eq!(count_words(""), 0);
        assert_eq!(count_words("one"), 1);
        assert_eq!(count_words("line one\nline two\n"), 4);
    }

    // ── build_stack_summary ───────────────────────────────────────────

    #[test]
    fn test_build_stack_summary() {
        let views = vec![
            SyncChangeView {
                change_id: "aaa".to_string(),
                bookmark_name: "feat-c".to_string(),
                description: "feat: c\n\n> [!plan]\n> status: 🔴\n".to_string(),
                is_working_copy: true,
            },
            SyncChangeView {
                change_id: "bbb".to_string(),
                bookmark_name: "feat-b".to_string(),
                description: "feat: b\n\n> [!plan]\n> status: ✅\n".to_string(),
                is_working_copy: false,
            },
            SyncChangeView {
                change_id: "ccc".to_string(),
                bookmark_name: "feat-a".to_string(),
                description: "feat: a\n\n> [!plan]\n> status: ✅\n".to_string(),
                is_working_copy: false,
            },
        ];

        let summary = build_stack_summary(&views, "bbb");

        assert_eq!(summary.total, 3);
        assert_eq!(summary.position, 2); // 1-based position of "bbb"

        assert!(summary.entries[0].is_working_copy);
        assert!(!summary.entries[0].is_target);
        assert!(!summary.entries[0].is_done);

        assert!(summary.entries[1].is_target);
        assert!(summary.entries[1].is_done);
        assert_eq!(summary.entries[1].bookmark, "feat-b");

        assert!(!summary.entries[2].is_target);
        assert!(summary.entries[2].is_done);
    }

    // ── build_summary (integration) ───────────────────────────────────

    #[test]
    fn test_build_summary_integration() {
        let description = "\
feat: add summary command

> [!plan]
> status: 🟡

# Background

Some context here.

# 🟡 Phase 1: Core

## Tasks

- ✅ Build data model
- 🟡 Write extraction
- 🔴 Write tests

# 🔴 Phase 2: Wiring

## Tasks

- 🔴 Wire dispatch

See jj:abcdefgh for details.
";

        let input = SummaryInput {
            description: description.to_string(),
            bookmark: "plan-summary".to_string(),
            change_id: "xyzxyzxy".to_string(),
            stack_views: Some(vec![SyncChangeView {
                change_id: "xyzxyzxy".to_string(),
                bookmark_name: "plan-summary".to_string(),
                description: description.to_string(),
                is_working_copy: true,
            }]),
            diff_stat: Some("2 files changed".to_string()),
            cross_ref_descriptions: vec![(
                "abcdefgh".to_string(),
                Some("other-plan".to_string()),
                Some("feat: the other plan".to_string()),
            )],
            indicators: default_indicators(),
            include_raw_body: true,
        };

        let output = build_summary(input);

        assert_eq!(output.title, "feat: add summary command");
        assert_eq!(output.bookmark, "plan-summary");
        assert_eq!(output.change_id, "xyzxyzxy");
        assert_eq!(output.status, Some("🟡".to_string()));
        assert!(output.line_count > 10);
        assert!(output.word_count > 20);

        // Outline
        assert!(!output.outline.is_empty());
        assert_eq!(output.outline[0].text, "Background");

        // Phases
        assert_eq!(output.phases.len(), 2);
        assert_eq!(output.phases[0].status, "🟡");
        assert_eq!(output.phases[0].task_total, 3);
        assert_eq!(output.phases[0].task_complete, 1);
        assert_eq!(output.phases[1].status, "🔴");

        // Phase summary
        assert!(output.phase_summary.contains("0/2 complete"));
        assert!(output.active_phase.is_some());
        let active = output.active_phase.unwrap();
        assert_eq!(active.tasks_complete, 1);
        assert_eq!(active.tasks_total, 3);

        // Cross-references
        assert_eq!(output.references.len(), 1);
        assert_eq!(output.references[0].change_id, "abcdefgh");
        assert_eq!(
            output.references[0].bookmark,
            Some("other-plan".to_string())
        );

        // Diff stat
        assert_eq!(output.diff_stat, Some("2 files changed".to_string()));

        // Stack
        assert!(output.stack.is_some());
        let stack = output.stack.unwrap();
        assert_eq!(stack.position, 1);
        assert_eq!(stack.total, 1);

        // Raw body
        assert!(output.raw_body.is_some());
        assert!(output.raw_body.unwrap().contains("Some context here"));
    }

    // ── format_json roundtrip ─────────────────────────────────────────

    #[test]
    fn test_format_json_roundtrip() {
        let input = SummaryInput {
            description: "feat: test\n\n> [!plan]\n> status: 🔴\n\n# Phase 1\n".to_string(),
            bookmark: "test-bm".to_string(),
            change_id: "testtest".to_string(),
            stack_views: None,
            diff_stat: None,
            cross_ref_descriptions: vec![],
            indicators: default_indicators(),
            include_raw_body: true,
        };

        let output = build_summary(input);
        let json = format_json(&output);

        // Verify it's valid JSON by deserializing
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["title"], "feat: test");
        assert_eq!(parsed["bookmark"], "test-bm");
        assert_eq!(parsed["change_id"], "testtest");
        assert_eq!(parsed["status"], "🔴");
        assert!(parsed["raw_body"].is_string());
    }
}