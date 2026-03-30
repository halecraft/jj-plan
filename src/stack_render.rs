//! Pure stack rendering: Span/Style model, multi-column layout, ANSI formatting.
//!
//! This module implements the FC/IS rendering pipeline for plan stack visualization:
//!
//! - **GATHER** (`prepare_display_rows`, `build_columns`): Bridge workspace data into
//!   the pure `DisplayRow` / `StackColumn` model. These functions call into `Workspace`
//!   and are not pure.
//!
//! - **PLAN** (`render_stack`): Pure function. Takes `&[StackColumn]`, returns
//!   `Vec<Vec<Span>>`. No I/O, no workspace, no side effects. All layout, gutter
//!   construction, and content assembly happens here.
//!
//! - **EXECUTE** (`format_plain` / `format_ansi`): Formatters are pure (map `Span` →
//!   `String`). The `eprintln!` calls in `show_plan_stack()` (in `wrap.rs`) are the
//!   only side effect.

use std::collections::BTreeMap;

use crate::commands::help::ColorWhen;
use crate::plan_file::encode_bookmark_for_filename;
use crate::pr_cache::PrCache;
use crate::stack_builder::narrow_segments;
use crate::types::{Gap, MultiStack, NarrowedBookmarkSegment, PlanRegistry, Stack};
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Span / Style model
// ---------------------------------------------------------------------------

/// Semantic style tags for stack rendering spans.
///
/// Each variant describes the *role* of the text, not its visual appearance.
/// The `format_ansi` function maps these to ANSI escape sequences; `format_plain`
/// ignores them entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Default unstyled text.
    Plain,
    /// Non-working-copy node marker: ○
    Marker,
    /// Working copy node marker: ◉
    WorkingCopyMarker,
    /// Trunk marker: ◆
    TrunkMarker,
    /// Bookmark name (bold).
    BookmarkName,
    /// Unique prefix portion of a change ID (bright magenta).
    ChangeIdPrefix,
    /// Non-unique rest of a change ID (gray).
    ChangeIdRest,
    /// Indicator text like @, ✓ (green), or other indicator text.
    Indicator,
    /// Gutter connectors: │, ├─┴─╯ (dim). Used for trunk merge and single-column.
    Connector,
    /// Stack header text: "stack: name". Used only in single-column mode (unstyled).
    StackHeader,
    /// Warning text: ⚠ messages.
    Warning,
    /// Per-column gutter connector: │, ○ node markers. The `usize` is the column
    /// index, used to select a color from the rainbow palette in `format_ansi`.
    ColumnConnector(usize),
    /// Per-column stack header: "stack: name". The `usize` is the column index.
    /// Rendered with underline + column color in `format_ansi`.
    ColumnHeader(usize),
}

/// Stack visualization format.
///
/// Controls how many lines each plan occupies in the rendered output.
/// - `Compact`: 1 line per plan (node + description inline). Default for terminal.
/// - `Regular`: 3 lines per plan (node, `│` description, spacer). Default for `stack.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackFormat {
    /// One line per plan: marker + bookmark + change ID + indicators + description.
    Compact,
    /// Three lines per plan: node line, `│` description line, blank spacer.
    Regular,
}

/// A span of text with a semantic style.
///
/// The rendering pipeline produces `Vec<Vec<Span>>` (lines of spans).
/// Formatters (`format_plain`, `format_ansi`) convert these to printable strings.
#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// Optional link target for markdown rendering (e.g. `"./01-feat-auth.md"`).
    /// Ignored by `format_plain` and `format_ansi`; used by `format_markdown`.
    pub link_target: Option<String>,
}

impl Span {
    /// Create a new span with the given text and style.
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            link_target: None,
        }
    }

    /// Create a plain-styled span.
    fn plain(text: impl Into<String>) -> Self {
        Self::new(text, Style::Plain)
    }

    /// Create a span with a link target for markdown rendering.
    pub fn linked(text: impl Into<String>, style: Style, target: impl Into<String>) -> Self {
        Self { text: text.into(), style, link_target: Some(target.into()) }
    }
}

// ---------------------------------------------------------------------------
// ANSI constants (private, used only by format_ansi)
// ---------------------------------------------------------------------------

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const UNDERLINE: &str = "\x1b[4m";
// 256-color codes matching jj's change ID rendering:
const BRIGHT_MAGENTA: &str = "\x1b[1m\x1b[38;5;5m"; // bold + 256-color magenta (unique prefix)
const GRAY: &str = "\x1b[38;5;8m"; // 256-color dark gray (rest of ID)

/// Rainbow palette for multi-column gutter coloring.
/// Each column gets a distinct color, rotating through the palette.
const COLUMN_COLORS: &[&str] = &[
    "\x1b[36m", // cyan
    "\x1b[33m", // yellow
    "\x1b[35m", // magenta
    "\x1b[34m", // blue
    "\x1b[32m", // green
    "\x1b[91m", // bright red
];

// ---------------------------------------------------------------------------
// Color mode resolution (moved from wrap.rs)
// ---------------------------------------------------------------------------

/// Resolve whether to use color for plan stack output.
///
/// Checks stderr (not stdout) since all plan stack output goes to stderr.
/// Respects jj's `ui.color` config via `configured_color_mode()`, which
/// calls `jj config get ui.color`.
pub fn should_color() -> bool {
    configured_color_mode().should_color_stderr()
}

/// Read jj's configured color mode, falling back to Auto.
pub fn configured_color_mode() -> ColorWhen {
    let Ok(jj) = crate::jj_binary::JjBinary::resolve() else {
        return ColorWhen::Auto;
    };
    let Ok((status, stdout, _)) = jj.run_silent(&["config", "get", "ui.color"]) else {
        return ColorWhen::Auto;
    };
    if !status.success() {
        return ColorWhen::Auto;
    }
    ColorWhen::parse(stdout.trim()).unwrap_or(ColorWhen::Auto)
}

// ---------------------------------------------------------------------------
// Formatters (EXECUTE phase — pure)
// ---------------------------------------------------------------------------

/// Convert rendered lines to plain text, ignoring all styles.
///
/// Each inner `Vec<Span>` becomes a single `String` by concatenating span text.
pub fn format_plain(lines: &[Vec<Span>]) -> Vec<String> {
    lines
        .iter()
        .map(|spans| spans.iter().map(|s| s.text.as_str()).collect())
        .collect()
}

/// Convert rendered lines to ANSI-colored text.
///
/// Each span's text is wrapped in the appropriate ANSI escape codes based on
/// its `Style`. A `RESET` is emitted after each styled span.
pub fn format_ansi(lines: &[Vec<Span>]) -> Vec<String> {
    lines
        .iter()
        .map(|spans| {
            let mut buf = String::new();
            for span in spans {
                match span.style {
                    Style::Plain | Style::StackHeader | Style::Warning => {
                        buf.push_str(&span.text);
                    }
                    Style::Marker | Style::Connector => {
                        buf.push_str(DIM);
                        buf.push_str(&span.text);
                        buf.push_str(RESET);
                    }
                    Style::WorkingCopyMarker => {
                        buf.push_str(BOLD);
                        buf.push_str(GREEN);
                        buf.push_str(&span.text);
                        buf.push_str(RESET);
                    }
                    Style::TrunkMarker => {
                        buf.push_str(CYAN);
                        buf.push_str(&span.text);
                        buf.push_str(RESET);
                    }
                    Style::BookmarkName => {
                        buf.push_str(BOLD);
                        buf.push_str(&span.text);
                        buf.push_str(RESET);
                    }
                    Style::ChangeIdPrefix => {
                        buf.push_str(BRIGHT_MAGENTA);
                        buf.push_str(&span.text);
                        buf.push_str(RESET);
                    }
                    Style::ChangeIdRest => {
                        buf.push_str(GRAY);
                        buf.push_str(&span.text);
                        buf.push_str(RESET);
                    }
                    Style::Indicator => {
                        buf.push_str(GREEN);
                        buf.push_str(&span.text);
                        buf.push_str(RESET);
                    }
                    Style::ColumnConnector(i) => {
                        let color = COLUMN_COLORS[i % COLUMN_COLORS.len()];
                        buf.push_str(color);
                        buf.push_str(&span.text);
                        buf.push_str(RESET);
                    }
                    Style::ColumnHeader(i) => {
                        let color = COLUMN_COLORS[i % COLUMN_COLORS.len()];
                        buf.push_str(UNDERLINE);
                        buf.push_str(color);
                        buf.push_str(&span.text);
                        buf.push_str(RESET);
                    }
                }
            }
            buf
        })
        .collect()
}

/// Convert rendered lines to markdown text.
///
/// Like `format_plain` but wraps spans that have a `link_target` in markdown
/// link syntax: `[text](target)`. Ignores all style information (no ANSI codes).
pub fn format_markdown(lines: &[Vec<Span>]) -> Vec<String> {
    lines
        .iter()
        .map(|spans| {
            let mut buf = String::new();
            for span in spans {
                match &span.link_target {
                    Some(target) => {
                        buf.push('[');
                        buf.push_str(&span.text);
                        buf.push_str("](");
                        buf.push_str(target);
                        buf.push(')');
                    }
                    None => {
                        buf.push_str(&span.text);
                    }
                }
            }
            buf
        })
        .collect()
}

/// Format rendered lines as a complete `stack.md` file content.
///
/// Prepends the generated-file header comment, joins lines with `\n`,
/// and appends a trailing `\n`. Returns the full content as a `String`.
pub fn format_markdown_with_header(lines: &[Vec<Span>]) -> String {
    let md_lines = format_markdown(lines);
    let mut result = String::from("<!-- generated by jj-plan \u{2014} do not edit -->\n");
    for line in &md_lines {
        result.push_str(line);
        result.push('\n');
    }
    result
}

// ---------------------------------------------------------------------------
// Display model (moved from stack_cmd.rs, extended)
// ---------------------------------------------------------------------------

/// A prepared display row for one segment in a stack column.
pub struct DisplayRow {
    /// The bookmark name for this segment.
    pub bookmark_name: String,
    /// Short change ID (reverse hex) for display.
    pub short_change_id: String,
    /// Split change ID: (unique_prefix, rest) for colored rendering.
    /// `None` when the split is unavailable (e.g. in unit tests with synthetic data).
    pub change_id_split: Option<(String, String)>,
    /// Whether this is the working copy commit.
    pub is_wc: bool,
    /// Raw indicator tokens (e.g. `"@"`, `"✓"`, `"~"`, `"synced"`, `"PR #3"`).
    /// Formatting into parenthesized display is done by `build_node_content` (PLAN phase).
    pub indicators: Vec<String>,
    /// First line of the commit description.
    pub first_line: String,
    /// Plan filename (e.g. `"01-feat-auth.md"`), for markdown link generation.
    /// `None` in multi-stack mode or when unavailable.
    pub plan_filename: Option<String>,
    /// Front matter metadata from the plan description (e.g. `status`, `issue`).
    /// Populated from parsed front matter; empty if no front matter present.
    pub metadata: BTreeMap<String, String>,
}

/// A prepared stack column for multi-column rendering.
pub struct StackColumn {
    /// Human-readable stack name.
    pub name: String,
    /// Display rows, tip (index 0) to trunk (last index).
    pub rows: Vec<DisplayRow>,
    /// Gap warnings for this stack.
    pub gaps: Vec<Gap>,
}

// ---------------------------------------------------------------------------
// Gutter helpers (internal, converted to Span model)
// ---------------------------------------------------------------------------

/// Gutter marker types for multi-column rendering.
enum GutterMark {
    /// A regular (non-working-copy) node: ○
    Node,
    /// The working copy node: ◉
    WorkingCopy,
    /// A continuation/pipe line: │
    Continuation,
    /// A header line (stack name): │ for other columns
    Header,
}

/// Build the gutter prefix spans for a line in the multi-column layout.
///
/// `num_cols` is the total number of stack columns.
/// `active_col` is the column that "owns" this line.
/// `mark` controls what character appears in the active column.
/// `started` tracks which columns have begun rendering. Columns where
/// `started[col] == false` emit spaces instead of `│` pipes.
///
/// Returns spans like [ColumnConnector("│ "), Marker("○ ")] (2 chars per column).
fn build_gutter(num_cols: usize, active_col: usize, mark: GutterMark, started: &[bool]) -> Vec<Span> {
    let mut spans = Vec::with_capacity(num_cols);
    for (col, &is_started) in started.iter().enumerate().take(num_cols) {
        if col == active_col {
            match mark {
                GutterMark::Node => spans.push(Span::new("○ ", Style::ColumnConnector(col))),
                GutterMark::WorkingCopy => spans.push(Span::new("◉ ", Style::ColumnConnector(col))),
                GutterMark::Continuation => {
                    spans.push(Span::new("│ ", Style::ColumnConnector(col)))
                }
                GutterMark::Header => {
                    spans.push(Span::plain("  "))
                }
            }
        } else if is_started {
            spans.push(Span::new("│ ", Style::ColumnConnector(col)));
        } else {
            spans.push(Span::plain("  "));
        }
    }
    spans
}

/// Build the trunk merge line spans for multi-column layout.
///
/// For 1 column: "│"
/// For 2 columns: "├─╯"
/// For 3 columns: "├─┴─╯"
/// For N columns: "├─┴─┴─...─╯"
fn build_trunk_merge(num_cols: usize) -> Vec<Span> {
    if num_cols <= 1 {
        return vec![Span::new("│", Style::Connector)];
    }
    let mut line = String::from("├─");
    for i in 1..num_cols {
        if i < num_cols - 1 {
            line.push_str("┴─");
        } else {
            line.push('╯');
        }
    }
    vec![Span::new(line, Style::Connector)]
}



// ---------------------------------------------------------------------------
// Row rendering helpers (build spans for a single display row)
// ---------------------------------------------------------------------------

/// Build the content spans for a node line (bookmark + change ID, no indicators).
fn build_node_content(row: &DisplayRow) -> Vec<Span> {
    let mut spans = Vec::new();

    // Bookmark name (with optional link target for markdown rendering)
    match &row.plan_filename {
        Some(filename) => {
            spans.push(Span::linked(&row.bookmark_name, Style::BookmarkName, format!("./{}", filename)));
        }
        None => {
            spans.push(Span::new(&row.bookmark_name, Style::BookmarkName));
        }
    }
    spans.push(Span::plain(" "));

    // Change ID (with optional prefix/rest split)
    match &row.change_id_split {
        Some((prefix, rest)) => {
            spans.push(Span::new(prefix, Style::ChangeIdPrefix));
            spans.push(Span::new(rest, Style::ChangeIdRest));
        }
        None => {
            spans.push(Span::plain(&row.short_change_id));
        }
    }

    spans
}

/// Build the indicator spans for a row (e.g. `(@, ✓)`).
///
/// Returns an empty vec if there are no indicators. Otherwise returns
/// spans including the ` (` prefix and `)` suffix, suitable for appending
/// to a node line (Compact) or prepending to a description line (Regular).
fn build_indicator_spans(row: &DisplayRow) -> Vec<Span> {
    if row.indicators.is_empty() {
        return Vec::new();
    }

    let mut spans = Vec::new();
    spans.push(Span::plain(" ("));
    for (i, indicator) in row.indicators.iter().enumerate() {
        if i > 0 {
            spans.push(Span::plain(", "));
        }
        // Semantic indicators (@, ✓, ~) get Style::Indicator (green in ANSI).
        // Others (synced, PR #N) get Style::Plain.
        let style = match indicator.as_str() {
            "@" | "✓" | "~" => Style::Indicator,
            _ => Style::Plain,
        };
        spans.push(Span::new(indicator, style));
    }
    spans.push(Span::plain(")"));
    spans
}

// ---------------------------------------------------------------------------
// Rendering (PLAN phase — pure)
// ---------------------------------------------------------------------------

/// Render a single-stack column (no gutter).
///
/// - `Regular`: 3 lines per plan (node, `│` description, spacer). Identical to pre-compact output.
/// - `Compact`: 1 line per plan (description appended to node line). No leading blank, no spacers.
///
/// Returns `Vec<Vec<Span>>` — one inner vec per output line.
fn render_single_column(column: &StackColumn, format: StackFormat) -> Vec<Vec<Span>> {
    let mut lines: Vec<Vec<Span>> = Vec::new();

    if format == StackFormat::Regular {
        lines.push(vec![]); // leading blank line (Regular only)
    }

    for (i, row) in column.rows.iter().enumerate() {
        // Node line: "  {marker} {bookmark_name} {short_change_id} {indicator_str}"
        let marker_style = if row.is_wc {
            Style::WorkingCopyMarker
        } else {
            Style::Marker
        };
        let marker_char = if row.is_wc { "◉" } else { "○" };

        let mut node_line = vec![
            Span::plain("  "),
            Span::new(marker_char, marker_style),
            Span::plain(" "),
        ];
        node_line.extend(build_node_content(row));

        let indicator_spans = build_indicator_spans(row);

        match format {
            StackFormat::Compact => {
                // Append indicators + description inline on the node line
                node_line.extend(indicator_spans);
                if !row.first_line.is_empty() {
                    node_line.push(Span::plain(" "));
                    node_line.push(Span::plain(&row.first_line));
                }
                lines.push(node_line);
                // No spacer between segments in compact mode
            }
            StackFormat::Regular => {
                lines.push(node_line);

                // Description line: indicators prepended before the description text
                if !row.first_line.is_empty() || !indicator_spans.is_empty() {
                    let mut desc_line = vec![
                        Span::plain("  "),
                        Span::new("│", Style::Connector),
                    ];
                    desc_line.extend(indicator_spans);
                    if !row.first_line.is_empty() {
                        desc_line.push(Span::plain(" "));
                        desc_line.push(Span::plain(&row.first_line));
                    }
                    lines.push(desc_line);
                }

                // Spacer between segments (not after last)
                if i < column.rows.len() - 1 {
                    lines.push(vec![Span::plain("  "), Span::new("│", Style::Connector)]);
                }
            }
        }
    }

    // Gap warnings (same for both formats)
    if !column.gaps.is_empty() {
        let total: usize = column.gaps.iter().map(|g| g.unbookmarked.len()).sum();
        lines.push(vec![]);
        lines.push(vec![
            Span::plain("  "),
            Span::new(
                format!("⚠ {} unbookmarked change(s) between plans", total),
                Style::Warning,
            ),
        ]);
        for gap in &column.gaps {
            for change in &gap.unbookmarked {
                lines.push(vec![Span::plain(format!(
                    "    {} {}",
                    change.short_id, change.description_first_line
                ))]);
            }
        }
    }

    // Trunk
    match format {
        StackFormat::Compact => {
            // No │ connector before trunk in compact mode
            lines.push(vec![
                Span::plain("  "),
                Span::new("◆", Style::TrunkMarker),
                Span::plain(" trunk()"),
            ]);
        }
        StackFormat::Regular => {
            lines.push(vec![Span::plain("  "), Span::new("│", Style::Connector)]);
            lines.push(vec![
                Span::plain("  "),
                Span::new("◆", Style::TrunkMarker),
                Span::plain(" trunk()"),
            ]);
            lines.push(vec![]);
        }
    }

    lines
}

/// Render multi-column graph with gutter.
///
/// - `Regular`: description on separate line with gutter continuation, spacers between segments.
/// - `Compact`: description appended to node line, no spacers between segments within a column.
///
/// Stack headers and trunk merge are the same in both formats.
fn render_multi_column(columns: &[StackColumn], format: StackFormat) -> Vec<Vec<Span>> {
    let num_cols = columns.len();
    let mut lines: Vec<Vec<Span>> = Vec::new();
    let mut started = vec![false; num_cols];

    if format == StackFormat::Regular {
        lines.push(vec![]); // leading blank line (Regular only)
    }

    for (col_idx, column) in columns.iter().enumerate() {
        // Mark this column as started (its gutter pipe appears from here on)
        started[col_idx] = true;

        // Stack header
        let mut header_line = vec![Span::plain("  ")];
        header_line.extend(build_gutter(num_cols, col_idx, GutterMark::Header, &started));
        header_line.push(Span::new(
            format!("stack: {}", column.name),
            Style::ColumnHeader(col_idx),
        ));
        lines.push(header_line);

        // Segments from tip to trunk
        for (row_idx, row) in column.rows.iter().enumerate() {
            let mark = if row.is_wc {
                GutterMark::WorkingCopy
            } else {
                GutterMark::Node
            };

            // Node line
            let mut node_line = vec![Span::plain("  ")];
            node_line.extend(build_gutter(num_cols, col_idx, mark, &started));
            node_line.extend(build_node_content(row));

            let indicator_spans = build_indicator_spans(row);

            match format {
                StackFormat::Compact => {
                    // Append indicators + description inline on the node line
                    node_line.extend(indicator_spans);
                    if !row.first_line.is_empty() {
                        node_line.push(Span::plain(" "));
                        node_line.push(Span::plain(&row.first_line));
                    }
                    lines.push(node_line);
                    // No spacer between segments in compact mode
                }
                StackFormat::Regular => {
                    lines.push(node_line);

                    // Description line: indicators prepended before the description text
                    if !row.first_line.is_empty() || !indicator_spans.is_empty() {
                        let mut desc_line = vec![Span::plain("  ")];
                        desc_line.extend(build_gutter(num_cols, col_idx, GutterMark::Continuation, &started));
                        desc_line.push(Span::plain(" "));
                        desc_line.extend(indicator_spans);
                        if !row.first_line.is_empty() {
                            desc_line.push(Span::plain(" "));
                            desc_line.push(Span::plain(&row.first_line));
                        }
                        lines.push(desc_line);
                    }

                    // Spacer between segments (not after last)
                    if row_idx < column.rows.len() - 1 {
                        let mut spacer_line = vec![Span::plain("  ")];
                        spacer_line.extend(build_gutter(num_cols, col_idx, GutterMark::Continuation, &started));
                        lines.push(spacer_line);
                    }
                }
            }
        }

        // Gap warnings (same for both formats)
        if !column.gaps.is_empty() {
            let total_unbookmarked: usize =
                column.gaps.iter().map(|g| g.unbookmarked.len()).sum();
            lines.push(vec![]);
            let mut warn_line = vec![Span::plain("  ")];
            warn_line.extend(build_gutter(num_cols, col_idx, GutterMark::Continuation, &started));
            warn_line.push(Span::new(
                format!(
                    "⚠ {} unbookmarked change(s) between plans",
                    total_unbookmarked
                ),
                Style::Warning,
            ));
            lines.push(warn_line);
            for gap in &column.gaps {
                for change in &gap.unbookmarked {
                    let mut gap_line = vec![Span::plain("  ")];
                    gap_line.extend(build_gutter(
                        num_cols,
                        col_idx,
                        GutterMark::Continuation,
                        &started,
                    ));
                    gap_line.push(Span::plain(format!(
                        "  {} {}",
                        change.short_id, change.description_first_line
                    )));
                    lines.push(gap_line);
                }
            }
        }

        // Spacer between stacks (same for both formats)
        if col_idx < num_cols - 1 {
            let mut spacer_line = vec![Span::plain("  ")];
            spacer_line.extend(build_gutter(num_cols, col_idx, GutterMark::Continuation, &started));
            lines.push(spacer_line);
        }
    }

    // Trunk merge line
    let mut merge_line = vec![Span::plain("  ")];
    merge_line.extend(build_trunk_merge(num_cols));
    lines.push(merge_line);

    // Trunk node
    lines.push(vec![
        Span::plain("  "),
        Span::new("◆", Style::TrunkMarker),
        Span::plain(" trunk()"),
    ]);

    if format == StackFormat::Regular {
        lines.push(vec![]); // trailing blank line (Regular only)
    }

    lines
}

/// Render a plan stack visualization as styled spans.
///
/// This is the main PLAN-phase entry point. Pure function — no I/O.
///
/// - Empty input returns empty `Vec` (caller handles empty-state messaging).
/// - Single column dispatches to `render_single_column` (no gutter).
/// - Multiple columns dispatches to `render_multi_column` (with gutter).
/// - `format` controls layout density: `Compact` (1 line/plan) vs `Regular` (3 lines/plan).
///
/// Trunk (`◆ trunk()`) is always included in the output.
pub fn render_stack(columns: &[StackColumn], format: StackFormat) -> Vec<Vec<Span>> {
    if columns.is_empty() {
        return vec![];
    }
    if columns.len() == 1 {
        render_single_column(&columns[0], format)
    } else {
        render_multi_column(columns, format)
    }
}

// ---------------------------------------------------------------------------
// GATHER phase — bridges workspace data into the pure display model
// ---------------------------------------------------------------------------

/// Prepare display rows from narrowed segments.
///
/// Converts `NarrowedBookmarkSegment`s into `DisplayRow`s ready for
/// rendering. Extracts change IDs via the workspace, and collects
/// indicators (working copy, done, synced, PR number).
///
/// This is a GATHER function — it calls into the workspace.
pub fn prepare_display_rows(
    narrowed: &[NarrowedBookmarkSegment],
    workspace: &Workspace,
    pr_cache: Option<&PrCache>,
) -> Vec<DisplayRow> {
    // Reverse to get tip-to-trunk order for display.
    // enumerate() after rev() gives display_idx 0 = tip, 1 = next, etc.
    // Plan file index is 1-based from trunk: narrowed.len() - display_idx.
    let num_segments = narrowed.len();
    narrowed
        .iter()
        .rev()
        .enumerate()
        .map(|(display_idx, seg)| {
            let bookmark_name = &seg.bookmark.name;
            let tip = seg.changes.first();
            let is_wc = tip.is_some_and(|c| c.is_working_copy);
            let is_synced = seg.bookmark.is_synced;
            let has_changes = tip.is_some_and(|c| !c.is_empty);

            let short_change_id = tip
                .and_then(|c| workspace.short_change_id_from_hex(&c.change_id))
                .unwrap_or_default();

            // Split change ID into unique prefix + rest for colored rendering
            let change_id_split =
                tip.and_then(|c| workspace.change_id_with_prefix_split(&c.change_id));

            // Parse once via PlanDocument — extracts is_done, title, and metadata
            let doc = tip.map(|c| crate::markdown::PlanDocument::parse(&c.description));
            let is_done = doc.as_ref().is_some_and(|d| d.is_done());
            let metadata = doc
                .as_ref()
                .map(|d| d.metadata().clone())
                .unwrap_or_default();

            let mut indicators = Vec::new();
            if is_wc {
                indicators.push("@".to_string());
            }
            if is_done {
                indicators.push("✓".to_string());
            } else if has_changes {
                indicators.push("~".to_string());
            }
            if is_synced {
                indicators.push("synced".to_string());
            }
            if let Some(cache) = pr_cache
                && let Some(cached_pr) = cache.get(bookmark_name) {
                    indicators.push(format!("PR #{}", cached_pr.number));
                }
            // Surface `issue` from metadata as an indicator
            if let Some(issue) = metadata.get("issue") {
                indicators.push(issue.clone());
            }

            let first_line = doc.as_ref().map(|d| d.title().to_string()).unwrap_or_default();

            DisplayRow {
                bookmark_name: bookmark_name.clone(),
                short_change_id,
                change_id_split,
                is_wc,
                indicators,
                first_line,
                plan_filename: {
                    let plan_idx = num_segments - display_idx;
                    Some(format!("{:02}-{}.md", plan_idx, encode_bookmark_for_filename(bookmark_name)))
                },
                metadata,
            }
        })
        .collect()
}

/// Build a single `StackColumn` from a `Stack` + metadata.
///
/// Shared inner function used by both `build_column_from_stack` (single-stack
/// hot path) and `build_columns` (multi-stack `--all` path). Performs the
/// narrow → prepare → assemble pipeline for one stack.
fn build_single_column(
    stack: &Stack,
    name: &str,
    registry: &PlanRegistry,
    workspace: &Workspace,
    pr_cache: Option<&PrCache>,
) -> StackColumn {
    let narrowed = narrow_segments(stack, registry);
    let rows = prepare_display_rows(&narrowed, workspace, pr_cache);
    StackColumn {
        name: name.to_string(),
        rows,
        gaps: stack.gaps.clone(),
    }
}

/// Build a single stack column from the @-relative stack, ready for rendering.
///
/// This is the "current stack only" entry point used by the sync/display hot
/// path. Always produces plan file links (no multi-stack link suppression).
/// Returns `None` if the stack has no segments.
pub fn build_column_from_stack(
    stack: &Stack,
    name: &str,
    registry: &PlanRegistry,
    workspace: &Workspace,
    pr_cache: Option<&PrCache>,
) -> Option<StackColumn> {
    if stack.segments.is_empty() {
        return None;
    }
    Some(build_single_column(stack, name, registry, workspace, pr_cache))
}

/// Build stack columns from a `MultiStack`, ready for rendering.
///
/// Encapsulates the per-group narrow → prepare → assemble loop. This is a
/// GATHER function — it calls into the workspace and PR cache.
///
/// Used by `jj stack --all` for the global multi-stack view.
pub fn build_columns(
    multi: &MultiStack,
    registry: &PlanRegistry,
    workspace: &Workspace,
    pr_cache: Option<&PrCache>,
) -> Vec<StackColumn> {
    let is_multi = multi.stacks.len() > 1;
    multi
        .stacks
        .iter()
        .map(|group| {
            let group_stack = Stack {
                segments: group.segments.clone(),
                gaps: group.gaps.clone(),
            };
            let mut column = build_single_column(&group_stack, &group.name, registry, workspace, pr_cache);

            // Multi-stack: per-group indices don't match global plan file indices,
            // so clear plan_filename to prevent incorrect markdown links.
            if is_multi {
                for row in &mut column.rows {
                    row.plan_filename = None;
                }
            }

            column
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Test helpers --------------------------------------------------------

    fn make_row(name: &str, desc: &str, is_wc: bool) -> DisplayRow {
        DisplayRow {
            bookmark_name: name.to_string(),
            short_change_id: "abcd1234".to_string(),
            change_id_split: None,
            is_wc,
            indicators: if is_wc {
                vec!["@".to_string()]
            } else {
                vec![]
            },
            first_line: desc.to_string(),
            plan_filename: None,
            metadata: BTreeMap::new(),
        }
    }

    fn make_column(name: &str, rows: Vec<DisplayRow>) -> StackColumn {
        StackColumn {
            name: name.to_string(),
            rows,
            gaps: vec![],
        }
    }

    // -- Moved + adapted tests from stack_cmd.rs ----------------------------

    #[test]
    fn trunk_merge_single_column() {
        let lines = format_plain(&[build_trunk_merge(1)]);
        assert_eq!(lines, vec!["│"]);
    }

    #[test]
    fn trunk_merge_two_columns() {
        let lines = format_plain(&[build_trunk_merge(2)]);
        assert_eq!(lines, vec!["├─╯"]);
    }

    #[test]
    fn trunk_merge_three_columns() {
        let lines = format_plain(&[build_trunk_merge(3)]);
        assert_eq!(lines, vec!["├─┴─╯"]);
    }

    #[test]
    fn trunk_merge_four_columns() {
        let lines = format_plain(&[build_trunk_merge(4)]);
        assert_eq!(lines, vec!["├─┴─┴─╯"]);
    }

    #[test]
    fn single_stack_renders_without_gutter() {
        let col = make_column(
            "auth",
            vec![
                make_row("auth-tests", "Add tests", false),
                make_row("auth-refactor", "Refactor auth", false),
            ],
        );
        let lines = format_plain(&render_stack(&[col], StackFormat::Regular));
        let output = lines.join("\n");

        // Should have ○ markers (not ◉ since neither is WC)
        assert!(
            output.contains("○ auth-tests"),
            "should show auth-tests node"
        );
        assert!(
            output.contains("○ auth-refactor"),
            "should show auth-refactor node"
        );
        // Should have trunk
        assert!(output.contains("◆ trunk()"), "should show trunk");
        // Should NOT have "stack:" header (single-stack case)
        assert!(
            !output.contains("stack:"),
            "single-stack should not show stack header"
        );
        // Should NOT have multi-column gutter (no "│ ○" pattern)
        assert!(
            !output.contains("│ ○"),
            "single-stack should not have column gutter"
        );
    }

    #[test]
    fn single_stack_shows_working_copy_marker() {
        let col = make_column("feat", vec![make_row("feat-api", "Feature API", true)]);
        let lines = format_plain(&render_stack(&[col], StackFormat::Regular));
        let output = lines.join("\n");

        assert!(
            output.contains("◉ feat-api"),
            "working copy should use ◉ marker"
        );
    }

    #[test]
    fn multi_stack_shows_stack_headers() {
        let cols = vec![
            make_column(
                "auth",
                vec![make_row("auth-refactor", "Refactor auth", false)],
            ),
            make_column(
                "dashboard",
                vec![make_row("dash-api", "Dashboard API", true)],
            ),
        ];
        let lines = format_plain(&render_stack(&cols, StackFormat::Regular));
        let output = lines.join("\n");

        assert!(
            output.contains("stack: auth"),
            "should show auth stack header"
        );
        assert!(
            output.contains("stack: dashboard"),
            "should show dashboard stack header"
        );
    }

    #[test]
    fn multi_stack_shows_column_gutter() {
        let cols = vec![
            make_column(
                "auth",
                vec![make_row("auth-refactor", "Refactor auth", false)],
            ),
            make_column(
                "dashboard",
                vec![make_row("dash-api", "Dashboard API", true)],
            ),
        ];
        let lines = format_plain(&render_stack(&cols, StackFormat::Regular));
        let output = lines.join("\n");

        // While rendering auth (col 0), dashboard (col 1) hasn't started yet → spaces, not │
        // So the auth node line should show "○   " (node + spaces for unstarted col 1)
        assert!(
            !output.contains("○ │"),
            "auth column should NOT have │ gutter for unstarted dashboard"
        );
        // The dashboard column (col 1) should show ◉ with a │ gutter for col 0 (already started)
        assert!(
            output.contains("│ ◉"),
            "dashboard column node should have gutter for auth"
        );
        // Trunk merge
        assert!(
            output.contains("├─╯"),
            "two columns should merge at trunk with ├─╯"
        );
        assert!(output.contains("◆ trunk()"), "should show trunk");
    }

    #[test]
    fn multi_stack_three_columns_merge() {
        let cols = vec![
            make_column("a", vec![make_row("a1", "A", false)]),
            make_column("b", vec![make_row("b1", "B", false)]),
            make_column("c", vec![make_row("c1", "C", true)]),
        ];
        let lines = format_plain(&render_stack(&cols, StackFormat::Regular));
        let output = lines.join("\n");

        assert!(
            output.contains("├─┴─╯"),
            "three columns should merge with ├─┴─╯"
        );
    }

    #[test]
    fn empty_stacks_returns_empty() {
        let result = render_stack(&[], StackFormat::Regular);
        assert!(result.is_empty(), "empty input should return empty Vec");
    }

    #[test]
    fn column_assignment_matches_input_order() {
        // build_multi_stack sorts by segment count descending.
        // render_stack preserves that order: index 0 = leftmost column.
        let cols = vec![
            make_column(
                "largest",
                vec![
                    make_row("l-2", "L2", false),
                    make_row("l-1", "L1", false),
                ],
            ),
            make_column("medium", vec![make_row("m-1", "M1", true)]),
            make_column("small", vec![make_row("s-1", "S1", false)]),
        ];
        let lines = format_plain(&render_stack(&cols, StackFormat::Regular));

        // Find the line indices of each stack header
        let largest_idx = lines
            .iter()
            .position(|l| l.contains("stack: largest"))
            .unwrap();
        let medium_idx = lines
            .iter()
            .position(|l| l.contains("stack: medium"))
            .unwrap();
        let small_idx = lines
            .iter()
            .position(|l| l.contains("stack: small"))
            .unwrap();

        // Stacks should appear in order: largest first (top), then medium, then small
        assert!(
            largest_idx < medium_idx,
            "largest stack should render before medium"
        );
        assert!(
            medium_idx < small_idx,
            "medium stack should render before small"
        );
    }

    // -- New tests -----------------------------------------------------------

    #[test]
    fn test_format_plain_no_ansi() {
        let col = make_column(
            "test",
            vec![make_row("test-bookmark", "Test description", true)],
        );
        let lines = format_plain(&render_stack(&[col], StackFormat::Regular));
        let output = lines.join("\n");

        assert!(
            !output.contains("\x1b["),
            "format_plain output should not contain ANSI escape sequences"
        );
    }

    #[test]
    fn test_format_ansi_has_color_codes() {
        let col = make_column(
            "test",
            vec![make_row("test-bookmark", "Test description", false)],
        );
        let lines = format_ansi(&render_stack(&[col], StackFormat::Regular));
        let output = lines.join("\n");

        // Should contain ANSI codes for markers and bookmark names
        assert!(
            output.contains("\x1b["),
            "format_ansi output should contain ANSI escape sequences"
        );
        // Bookmark name should be bold
        assert!(
            output.contains(&format!("{BOLD}test-bookmark{RESET}")),
            "bookmark name should be bold"
        );
    }

    #[test]
    fn test_format_ansi_working_copy_green() {
        let col = make_column(
            "test",
            vec![make_row("test-bookmark", "Test description", true)],
        );
        let lines = format_ansi(&render_stack(&[col], StackFormat::Regular));
        let output = lines.join("\n");

        // ◉ should get bold green escape
        assert!(
            output.contains(&format!("{BOLD}{GREEN}◉{RESET}")),
            "working copy marker ◉ should be bold green"
        );
    }

    #[test]
    fn test_format_ansi_change_id_split() {
        let mut row = make_row("feat-api", "Feature", false);
        row.change_id_split = Some(("kpqx".to_string(), "ywon".to_string()));
        row.short_change_id = "kpqxywon".to_string();

        let col = make_column("test", vec![row]);
        let lines = format_ansi(&render_stack(&[col], StackFormat::Regular));
        let output = lines.join("\n");

        // Prefix should get bright magenta
        assert!(
            output.contains(&format!("{BRIGHT_MAGENTA}kpqx{RESET}")),
            "change ID prefix should be bright magenta"
        );
        // Rest should get gray
        assert!(
            output.contains(&format!("{GRAY}ywon{RESET}")),
            "change ID rest should be gray"
        );
    }

    #[test]
    fn test_span_round_trip() {
        // Verify that format_plain(render_stack(cols)) matches expected plain-text layout
        let col = make_column(
            "auth",
            vec![
                make_row("auth-tests", "Add tests", true),
                make_row("auth-refactor", "Refactor auth", false),
            ],
        );
        let lines = format_plain(&render_stack(&[col], StackFormat::Regular));
        let output = lines.join("\n");

        // Verify the structural elements are all present and in order
        assert!(output.contains("◉ auth-tests abcd1234"));
        assert!(output.contains("│ (@) Add tests"));
        assert!(output.contains("○ auth-refactor abcd1234"));
        assert!(output.contains("│ Refactor auth"));
        assert!(output.contains("◆ trunk()"));

        // Verify ordering: auth-tests before auth-refactor before trunk
        let tests_idx = output.find("auth-tests").unwrap();
        let refactor_idx = output.find("auth-refactor").unwrap();
        let trunk_idx = output.find("trunk()").unwrap();
        assert!(tests_idx < refactor_idx, "tip should render before trunk");
        assert!(refactor_idx < trunk_idx, "segments should render before trunk");
    }

    #[test]
    fn test_display_row_shows_issue_indicator() {
        let row = DisplayRow {
            bookmark_name: "feat-auth".to_string(),
            short_change_id: "abc123".to_string(),
            change_id_split: None,
            is_wc: false,
            indicators: vec!["~".to_string(), "MERC-123".to_string()],
            first_line: "feat: add auth".to_string(),
            plan_filename: None,
            metadata: {
                let mut m = BTreeMap::new();
                m.insert("status".to_string(), "🔴".to_string());
                m.insert("issue".to_string(), "MERC-123".to_string());
                m
            },
        };
        // The issue indicator should appear in the indicator spans (not node content)
        let spans = build_indicator_spans(&row);
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert!(text.contains("MERC-123"), "issue indicator should appear in indicator spans: {}", text);
    }

    #[test]
    fn multi_stack_includes_change_id() {
        // Verify the fix: multi-column node lines include short_change_id,
        // matching single-column behavior.
        let cols = vec![
            make_column(
                "auth",
                vec![make_row("auth-refactor", "Refactor auth", false)],
            ),
            make_column(
                "dashboard",
                vec![make_row("dash-api", "Dashboard API", true)],
            ),
        ];
        let lines = format_plain(&render_stack(&cols, StackFormat::Regular));
        let output = lines.join("\n");

        // Both columns should include the change ID
        assert!(
            output.contains("auth-refactor abcd1234"),
            "multi-column should include change ID for auth-refactor"
        );
        assert!(
            output.contains("dash-api abcd1234"),
            "multi-column should include change ID for dash-api"
        );
    }

    #[test]
    fn single_stack_column_has_plan_file_links_in_markdown() {
        // Verify that single-stack rendering produces markdown links to plan files.
        // This is the key property that was broken in multi-stack mode (links were
        // cleared) and is now always correct on the hot path via build_column_from_stack.
        let col = StackColumn {
            name: "feat".to_string(),
            rows: vec![
                DisplayRow {
                    bookmark_name: "feat-api".to_string(),
                    short_change_id: "abcd1234".to_string(),
                    change_id_split: None,
                    is_wc: true,
                    indicators: vec!["@".to_string()],
                    first_line: "Add API".to_string(),
                    plan_filename: Some("02-feat-api.md".to_string()),
                    metadata: BTreeMap::new(),
                },
                DisplayRow {
                    bookmark_name: "feat-auth".to_string(),
                    short_change_id: "efgh5678".to_string(),
                    change_id_split: None,
                    is_wc: false,
                    indicators: vec![],
                    first_line: "Auth module".to_string(),
                    plan_filename: Some("01-feat-auth.md".to_string()),
                    metadata: BTreeMap::new(),
                },
            ],
            gaps: vec![],
        };

        let rendered = render_stack(&[col], StackFormat::Regular);
        let md_lines = format_markdown(&rendered);
        let md_output = md_lines.join("\n");

        // Markdown output should contain clickable links to plan files
        assert!(
            md_output.contains("[feat-api](./02-feat-api.md)"),
            "single-stack markdown should link feat-api to its plan file"
        );
        assert!(
            md_output.contains("[feat-auth](./01-feat-auth.md)"),
            "single-stack markdown should link feat-auth to its plan file"
        );
    }

    // -- Compact format tests ------------------------------------------------

    #[test]
    fn compact_single_column_one_line_per_plan() {
        let col = make_column(
            "auth",
            vec![
                make_row("auth-tests", "Add tests", true),
                make_row("auth-refactor", "Refactor auth", false),
            ],
        );
        let lines = format_plain(&render_stack(&[col], StackFormat::Compact));
        let output = lines.join("\n");

        // Should NOT have │ description lines
        assert!(
            !output.contains("│ Add tests"),
            "compact should not have separate description line: {output}"
        );
        assert!(
            !output.contains("│ Refactor auth"),
            "compact should not have separate description line: {output}"
        );
        // Should NOT have blank spacer lines between segments (│ alone on a line)
        // In compact mode there should be no "  │" lines at all
        for line in &lines {
            let trimmed = line.trim();
            assert!(
                trimmed != "│",
                "compact should not have bare │ spacer lines: {output}"
            );
        }
        // Should still have trunk
        assert!(output.contains("◆ trunk()"), "should show trunk");
        // Trunk should NOT be preceded by │ connector
        let trunk_idx = lines.iter().position(|l| l.contains("◆ trunk()")).unwrap();
        if trunk_idx > 0 {
            assert!(
                !lines[trunk_idx - 1].contains("│"),
                "compact trunk should not have │ connector above it: {output}"
            );
        }
        // No leading blank line
        assert!(
            !lines[0].is_empty(),
            "compact should not have leading blank line"
        );
    }

    #[test]
    fn regular_single_column_matches_existing() {
        // Regression test: Regular mode must produce the same 3-line layout
        let col = make_column(
            "auth",
            vec![
                make_row("auth-tests", "Add tests", true),
                make_row("auth-refactor", "Refactor auth", false),
            ],
        );
        let lines = format_plain(&render_stack(&[col], StackFormat::Regular));
        let output = lines.join("\n");

        // Must have │ description lines (with indicators prepended for WC row)
        assert!(output.contains("│ (@) Add tests"), "regular must have description line with indicators");
        assert!(output.contains("│ Refactor auth"), "regular must have description line");
        // Must have │ spacer between segments
        assert!(output.contains("◉ auth-tests"), "should show auth-tests");
        assert!(output.contains("○ auth-refactor"), "should show auth-refactor");
        // Must have │ connector before trunk
        let trunk_idx = lines.iter().position(|l| l.contains("◆ trunk()")).unwrap();
        assert!(
            lines[trunk_idx - 1].contains("│"),
            "regular trunk should have │ connector above it"
        );
        // Leading blank line
        assert!(
            lines[0].is_empty(),
            "regular should have leading blank line"
        );
        // Trailing blank line
        assert!(
            lines.last().unwrap().is_empty(),
            "regular should have trailing blank line"
        );
    }

    #[test]
    fn compact_includes_description_on_node_line() {
        let col = make_column(
            "feat",
            vec![make_row("feat-api", "Add API endpoints", true)],
        );
        let lines = format_plain(&render_stack(&[col], StackFormat::Compact));
        let output = lines.join("\n");

        // Description should appear on the same line as the bookmark
        assert!(
            output.contains("feat-api abcd1234 (@) Add API endpoints"),
            "compact should have description on same line as bookmark: {output}"
        );
    }

    #[test]
    fn compact_multi_column_no_spacers() {
        let cols = vec![
            make_column(
                "auth",
                vec![
                    make_row("auth-tests", "Add tests", false),
                    make_row("auth-refactor", "Refactor auth", false),
                ],
            ),
            make_column(
                "dashboard",
                vec![make_row("dash-api", "Dashboard API", true)],
            ),
        ];
        let lines = format_plain(&render_stack(&cols, StackFormat::Compact));
        let output = lines.join("\n");

        // Should NOT have separate description lines (with double-space indent after gutter)
        assert!(
            !output.contains("  Add tests\n"),
            "compact multi-column should not have separate description line"
        );
        // Descriptions should be inline on node lines
        assert!(
            output.contains("auth-tests abcd1234 Add tests"),
            "compact multi-column should have description inline: {output}"
        );
        assert!(
            output.contains("dash-api abcd1234 (@) Dashboard API"),
            "compact multi-column should have description inline: {output}"
        );
        // Stack headers should still be present
        assert!(output.contains("stack: auth"), "should show auth stack header");
        assert!(output.contains("stack: dashboard"), "should show dashboard stack header");
        // Trunk merge should still be present
        assert!(output.contains("├─╯"), "should show trunk merge");
        assert!(output.contains("◆ trunk()"), "should show trunk");
        // No leading blank line
        assert!(
            !lines[0].is_empty(),
            "compact multi-column should not have leading blank line"
        );
    }

    #[test]
    fn compact_empty_description_no_trailing_space() {
        let mut row = make_row("feat-api", "", false);
        row.first_line = String::new();
        let col = make_column("feat", vec![row]);
        let lines = format_plain(&render_stack(&[col], StackFormat::Compact));

        // Find the node line
        let node_line = lines.iter().find(|l| l.contains("feat-api")).unwrap();
        // Should not end with a trailing space from the missing description
        assert!(
            !node_line.ends_with(' '),
            "node line should not have trailing space when description is empty: '{node_line}'"
        );
    }

    // -- Multi-stack polish tests (rainbow, smart gutter, headers) -----------

    #[test]
    fn column_connector_style_has_color() {
        let cols = vec![
            make_column("auth", vec![make_row("auth-refactor", "Refactor auth", false)]),
            make_column("dashboard", vec![make_row("dash-api", "Dashboard API", true)]),
        ];
        let lines = format_ansi(&render_stack(&cols, StackFormat::Compact));
        let output = lines.join("\n");

        // Should contain ANSI color codes from the column palette (not just DIM gray)
        // Column 0 = cyan (\x1b[36m), Column 1 = yellow (\x1b[33m)
        assert!(
            output.contains("\x1b[36m"),
            "column 0 gutter should use cyan from palette: {output}"
        );
        assert!(
            output.contains("\x1b[33m"),
            "column 1 gutter should use yellow from palette: {output}"
        );
    }

    #[test]
    fn column_header_is_underlined() {
        let cols = vec![
            make_column("auth", vec![make_row("auth-refactor", "Refactor auth", false)]),
            make_column("dashboard", vec![make_row("dash-api", "Dashboard API", true)]),
        ];
        let lines = format_ansi(&render_stack(&cols, StackFormat::Compact));
        let output = lines.join("\n");

        // Headers should be underlined + colored
        assert!(
            output.contains(&format!("{UNDERLINE}\x1b[36mstack: auth{RESET}")),
            "auth header should be underlined + cyan: {output}"
        );
        assert!(
            output.contains(&format!("{UNDERLINE}\x1b[33mstack: dashboard{RESET}")),
            "dashboard header should be underlined + yellow: {output}"
        );
    }

    #[test]
    fn unstarted_columns_show_spaces() {
        let cols = vec![
            make_column("a", vec![make_row("a1", "A", false)]),
            make_column("b", vec![make_row("b1", "B", false)]),
            make_column("c", vec![make_row("c1", "C", true)]),
        ];
        let lines = format_plain(&render_stack(&cols, StackFormat::Compact));

        // While rendering column 0 (a), columns 1 and 2 haven't started.
        // The header line for "a" should NOT have │ for columns 1 and 2.
        let header_a = lines.iter().find(|l| l.contains("stack: a")).unwrap();
        // Column 0 header: "  │ stack: a" (just the active col's │, no trailing │ │)
        assert!(
            !header_a.contains("│ │"),
            "unstarted columns should not show pipes on column 0 header: '{header_a}'"
        );

        // The node line for "a1" should have ○ but no trailing │ for cols 1, 2
        let node_a = lines.iter().find(|l| l.contains("a1")).unwrap();
        assert!(
            !node_a.contains("│"),
            "unstarted columns should show spaces, not pipes, on column 0 node: '{node_a}'"
        );
    }

    #[test]
    fn started_columns_show_pipes() {
        let cols = vec![
            make_column("a", vec![make_row("a1", "A", false)]),
            make_column("b", vec![make_row("b1", "B", false)]),
            make_column("c", vec![make_row("c1", "C", true)]),
        ];
        let lines = format_plain(&render_stack(&cols, StackFormat::Compact));

        // While rendering column 2 (c), columns 0 and 1 have already started.
        // The node line for "c1" should show │ │ before ◉
        let node_c = lines.iter().find(|l| l.contains("c1")).unwrap();
        assert!(
            node_c.contains("│ │"),
            "started columns should show pipes on column 2 node: '{node_c}'"
        );
    }

    #[test]
    fn format_plain_ignores_column_styles() {
        let cols = vec![
            make_column("auth", vec![make_row("auth-refactor", "Refactor auth", false)]),
            make_column("dashboard", vec![make_row("dash-api", "Dashboard API", true)]),
        ];
        let lines = format_plain(&render_stack(&cols, StackFormat::Compact));
        let output = lines.join("\n");

        // Plain format should have no ANSI codes
        assert!(
            !output.contains("\x1b["),
            "format_plain should not contain ANSI escape sequences: {output}"
        );
        // Should still contain structural elements
        assert!(output.contains("│"), "should still have │ pipes");
        assert!(output.contains("stack: auth"), "should still have stack header text");
        assert!(output.contains("stack: dashboard"), "should still have stack header text");
    }

    #[test]
    fn implicit_stack_gets_counter_name_in_output() {
        // Simulate implicit stacks by giving columns counter-style names
        // (this is what build_multi_stack produces for implicit stacks)
        let cols = vec![
            make_column("Stack 1", vec![make_row("feat-auth", "Auth", false)]),
            make_column("Stack 2", vec![make_row("fix-bug", "Bug fix", true)]),
        ];
        let lines = format_plain(&render_stack(&cols, StackFormat::Compact));
        let output = lines.join("\n");

        assert!(
            output.contains("stack: Stack 1"),
            "implicit stack should show counter name: {output}"
        );
        assert!(
            output.contains("stack: Stack 2"),
            "implicit stack should show counter name: {output}"
        );
        // Bookmark names should NOT appear in headers (no redundancy)
        assert!(
            !output.contains("stack: feat-auth"),
            "implicit stack should NOT use bookmark as header name"
        );
    }

    #[test]
    fn explicit_stack_keeps_human_name_in_output() {
        // Simulate explicit stack with a human-chosen name
        // (this is what build_multi_stack produces for stack/* base bookmarks)
        let cols = vec![
            make_column("auth", vec![
                make_row("auth-tests", "Add tests", false),
                make_row("auth-refactor", "Refactor auth", false),
            ]),
            make_column("Stack 1", vec![make_row("fix-bug", "Bug fix", true)]),
        ];
        let lines = format_plain(&render_stack(&cols, StackFormat::Compact));
        let output = lines.join("\n");

        // Explicit stack keeps its human name
        assert!(
            output.contains("stack: auth"),
            "explicit stack should keep human-chosen name: {output}"
        );
        // Implicit stack gets counter
        assert!(
            output.contains("stack: Stack 1"),
            "implicit stack should get counter name: {output}"
        );
    }
}