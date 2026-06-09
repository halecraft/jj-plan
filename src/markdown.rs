use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Metadata parsing (Obsidian-style callout block format)
// ---------------------------------------------------------------------------

/// The canonical callout opener. Defined once so the parser (which recognizes
/// it) and `render_description` (which emits it) cannot drift apart.
///
/// **Invariant:** the opener is canonically *bare* — metadata always lives on
/// the `> key: value` lines that follow. Metadata placed inline on the opener
/// (`> [!plan] status: 🔴`, e.g. from a hand edit or an Obsidian "callout
/// title") is *read-tolerated* by the parser but is never written back.
const CALLOUT_OPENER: &str = "> [!plan]";

/// Check if a line (after stripping `> ` prefix) looks like a metadata key.
///
/// Pattern: `^[a-z][a-z0-9_-]*: ` (lowercase key, colon, space, value).
/// This prevents false positives from prose lines with colons.
fn is_callout_metadata_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    let colon_pos = match line.find(':') {
        Some(p) => p,
        None => return false,
    };
    if !line[..colon_pos]
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return false;
    }
    let after_colon = &line[colon_pos + 1..];
    after_colon.is_empty() || after_colon.starts_with(' ')
}

/// Parse a `key: value` pair from callout content — the text after `> ` on a
/// metadata line, or the inline text after `]` on an opener. Returns `None`
/// unless it matches the metadata key pattern (see `is_callout_metadata_line`).
fn parse_metadata_pair(content: &str) -> Option<(String, String)> {
    if !is_callout_metadata_line(content) {
        return None;
    }
    let colon = content.find(':').unwrap();
    Some((
        content[..colon].to_string(),
        content[colon + 1..].trim().to_string(),
    ))
}

/// If `line` is a `> [!plan]` callout opener (case-insensitive on `plan`),
/// return the inline text after the `]` (trimmed). Returns `None` if the line
/// is not an opener. An opener with no inline text yields `Some("")`.
fn opener_inline(line: &str) -> Option<&str> {
    let trimmed = line.trim_end();
    if !trimmed.starts_with("> [!") {
        return None;
    }
    let after_prefix = &trimmed[4..];
    let close_bracket = after_prefix.find(']')?;
    if !after_prefix[..close_bracket].eq_ignore_ascii_case("plan") {
        return None;
    }
    Some(after_prefix[close_bracket + 1..].trim())
}

/// Check if a line is a `> [!plan]` callout opener (case-insensitive on `plan`).
fn is_callout_opener(line: &str) -> bool {
    opener_inline(line).is_some()
}

/// Extract Obsidian-style callout metadata from a plan description.
///
/// Canonical format — the body leads, the callout trails:
/// ```text
/// feat: my feature          ← line 1: always the title
///
/// # Background              ← body
///
/// > [!plan]                 ← callout opener (case-insensitive)
/// > status: 🔴              ← metadata key: value lines
/// > issue: MERC-123
/// ```
///
/// Returns a map of key-value pairs and the body (input with the title line
/// and callout block lines removed). If no `> [!plan]` block is found,
/// returns an empty map and everything after line 1 as the body.
///
/// Parsing is **position-independent** (the callout may sit anywhere after the
/// title — top-placed callouts from before the format change still parse) and
/// **inline-tolerant** (`> [!plan] status: 🔴` is read, though never written —
/// see [`CALLOUT_OPENER`]). `render_description` is the canonical inverse.
///
/// Parsing rules:
/// - Line 1 is always the title — never metadata.
/// - Scan all lines (after title) for `> [!plan]` (the callout opener).
/// - Read any inline `key: value` on the opener line, then the subsequent
///   `> key: value` lines. The block ends at the first line that doesn't start
///   with `> ` or doesn't match the `key: value` pattern.
/// - Blank lines before/after the callout block do not affect parsing.
/// - Body is everything outside the title line and the callout block lines.
pub fn parse_metadata(input: &str) -> (BTreeMap<String, String>, String) {
    let title_end = input.find('\n').unwrap_or(input.len());
    if title_end == input.len() {
        return (BTreeMap::new(), String::new()); // single line, no body
    }

    let after_title = &input[title_end + 1..];
    let lines: Vec<&str> = after_title.lines().collect();

    // Find the callout opener line index
    let opener_idx = match lines.iter().position(|l| is_callout_opener(l)) {
        Some(idx) => idx,
        None => {
            // No callout block — body is everything after title
            return (BTreeMap::new(), after_title.to_string());
        }
    };

    // Collect metadata. First, any inline metadata on the opener line itself
    // (`> [!plan] status: 🔴`) — read-tolerated for backward/Obsidian compat.
    // Then the `> key: value` lines that follow.
    let mut map = BTreeMap::new();
    let mut block_end = opener_idx + 1; // exclusive index past last callout line

    if let Some(inline) = opener_inline(lines[opener_idx])
        && let Some((key, value)) = parse_metadata_pair(inline) {
            map.insert(key, value);
        }

    for line in &lines[opener_idx + 1..] {
        if let Some(content) = line.strip_prefix("> ")
            && let Some((key, value)) = parse_metadata_pair(content) {
                map.insert(key, value);
                block_end += 1;
                continue;
            }
        break; // non-metadata line ends the block
    }

    // Build body: lines outside the callout block (opener..block_end)
    let mut body = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i >= opener_idx && i < block_end {
            continue; // skip callout block lines
        }
        body.push_str(line);
        body.push('\n');
    }
    // Preserve trailing content: if after_title didn't end with \n,
    // the last line wouldn't have gotten an extra \n from lines().
    // But .lines() strips trailing newlines, so we need to be careful.
    // Trim at most one trailing \n that we may have over-added.
    if !after_title.ends_with('\n') && body.ends_with('\n') {
        body.pop();
    }

    (map, body)
}

/// Canonical serializer — the inverse of [`parse_metadata`].
///
/// Emits the `title`, then the `body` (surrounding blank lines trimmed), then a
/// single **bare-opener** `> [!plan]` block at the END, with `metadata` in
/// deterministic (`BTreeMap`, key-sorted) order. When `metadata` is empty, no
/// callout is emitted.
///
/// The result is a fixpoint under re-parse: `render_description` applied to the
/// output of `parse_metadata` is byte-stable, which is what makes the writers
/// idempotent (the drift gate and three-way reconcile rely on convergence).
fn render_description(
    title: &str,
    body: &str,
    metadata: &BTreeMap<String, String>,
) -> String {
    let body = body.trim_matches('\n');

    let mut out = String::with_capacity(title.len() + body.len() + 64);
    out.push_str(title);
    out.push('\n');

    if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
        out.push('\n');
    }

    if !metadata.is_empty() {
        out.push('\n');
        out.push_str(CALLOUT_OPENER);
        out.push('\n');
        for (key, value) in metadata {
            out.push_str("> ");
            out.push_str(key);
            out.push_str(": ");
            out.push_str(value);
            out.push('\n');
        }
    }

    out
}

/// Upsert `key = value` into the plan callout and re-emit a single canonical
/// `> [!plan]` block at the END of the description (see [`render_description`]).
///
/// Implemented as parse → upsert → render: position- and inline-tolerant on
/// read, it collapses any stray/duplicate callouts to one canonical block and
/// is idempotent. All body content is preserved (surrounding blanks normalized).
pub fn set_metadata_field(input: &str, key: &str, value: &str) -> String {
    let title = input.lines().next().unwrap_or("");
    let (mut metadata, body) = parse_metadata(input);
    metadata.insert(key.to_string(), value.to_string());
    render_description(title, &body, &metadata)
}

// ---------------------------------------------------------------------------
// PlanDocument — unified parse-and-transform facade
// ---------------------------------------------------------------------------

/// A parsed plan document that provides read accessors and transform methods.
///
/// Constructed once from a description string via `PlanDocument::parse()`,
/// then used at consumer boundaries (done, submit, display) to access
/// title, metadata, body, and derived transformations without redundant
/// parsing.
///
/// This is a **parsing facade**, not a domain entity. It owns its data
/// and should be constructed at the point of use, not stored on long-lived
/// types.
pub struct PlanDocument {
    raw: String,
    title: String,
    metadata: BTreeMap<String, String>,
    body: String,
}

impl PlanDocument {
    /// Parse a description string into a `PlanDocument`.
    ///
    /// Calls `parse_metadata` once and stores the results. The title is
    /// always line 1 of the input.
    pub fn parse(input: &str) -> Self {
        let title = input.lines().next().unwrap_or("").to_string();
        let (metadata, body) = parse_metadata(input);
        Self {
            raw: input.to_string(),
            title,
            metadata,
            body,
        }
    }

    // -- Read accessors ----------------------------------------------------

    /// Line 1 of the input — the commit summary / plan title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Whether the metadata `status` field is `✅`.
    pub fn is_done(&self) -> bool {
        self.metadata.get("status").is_some_and(|v| v == "✅")
    }

    /// Full metadata key-value map.
    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.metadata
    }

    /// Body content (everything outside title line and callout block).
    pub fn body(&self) -> &str {
        &self.body
    }

    /// The original unparsed input.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    // -- Transform methods -------------------------------------------------

    /// Body with `[scratch]` sections stripped.
    ///
    /// Computed on demand, not cached.
    pub fn body_sans_scratch(&self) -> String {
        strip_scratch_sections(&self.body)
    }

    /// All headings in the raw document (title line + callout + body).
    ///
    /// Uses `extract_headings` (pulldown-cmark) for CommonMark-compliant
    /// heading detection with code fence immunity. Computed on demand.
    pub fn headings(&self) -> Vec<HeadingInfo> {
        extract_headings(&self.raw)
    }

    /// The complete "mark as done" transformation.
    ///
    /// 1. If `!keep_scratch`, strips `[scratch]` sections from the **body**.
    /// 2. Sets metadata `status: ✅` and re-renders the callout at the bottom.
    ///
    /// Idempotent: if status is already `✅`, still strips scratch (if requested)
    /// but doesn't double-stamp.
    ///
    /// Thin wrapper around `as_done_with_report` that discards the report.
    pub fn as_done(&self, keep_scratch: bool) -> String {
        self.as_done_with_report(keep_scratch).0
    }

    /// Like `as_done`, but also returns a structured report of which scratch
    /// sections were stripped (always empty when `keep_scratch` is true).
    ///
    /// Operates on already-parsed components (GATHER→PLAN→EXECUTE): the callout
    /// lives in `self.metadata`, **not** in the body, so scratch-stripping can
    /// never delete it — every metadata key survives, not just `status`. The
    /// returned `Vec<StrippedSection>` carries byte ranges into **`self.body()`**
    /// (not `raw`), so callers slice `self.body()` at `section.range` to recover
    /// the removed content.
    pub fn as_done_with_report(
        &self,
        keep_scratch: bool,
    ) -> (String, Vec<StrippedSection>) {
        let (body, report) = if keep_scratch {
            (self.body.clone(), Vec::new())
        } else {
            strip_scratch_sections_with_report(&self.body)
        };
        let mut metadata = self.metadata.clone();
        metadata.insert("status".to_string(), "✅".to_string());
        (render_description(&self.title, &body, &metadata), report)
    }

    /// Extract PR title and body for submission.
    ///
    /// Title is `self.title()` (line 1). Body is `self.body()` with
    /// `[scratch]` sections stripped and trimmed. Returns `None` if the
    /// title is empty.
    pub fn pr_parts(&self) -> Option<(String, String)> {
        let title = self.title();
        if title.trim().is_empty() {
            return None;
        }
        let body = strip_scratch_sections(&self.body).trim().to_string();
        Some((title.to_string(), body))
    }
}

// ---------------------------------------------------------------------------
// Heading extraction (pulldown-cmark based)
// ---------------------------------------------------------------------------

/// A heading found in a markdown document.
///
/// Extracted via `pulldown-cmark` with `into_offset_iter()` for proper
/// CommonMark compliance (ATX and setext headings, code fence immunity).
#[derive(Debug, Clone)]
pub struct HeadingInfo {
    /// Heading level (1–6).
    pub level: u8,
    /// The heading's text content (inline code included, markup stripped).
    pub text: String,
    /// Byte offset of the heading in the source string.
    pub byte_offset: usize,
    /// 1-based line number of the heading in the source string.
    pub line: usize,
}

/// Extract all headings from a markdown string.
///
/// Uses `pulldown-cmark`'s offset iterator for correct CommonMark heading
/// detection (ATX `#` headings, setext underline headings) with automatic
/// code fence immunity. Returns headings in document order.
///
/// The `line` field is computed from `byte_offset` by counting newlines
/// in `input[..byte_offset]`.
pub fn extract_headings(input: &str) -> Vec<HeadingInfo> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    if input.is_empty() {
        return Vec::new();
    }

    let parser = Parser::new_ext(input, Options::all());
    let mut headings: Vec<HeadingInfo> = Vec::new();
    let mut current_heading_level: Option<u8> = None;
    let mut current_heading_text = String::new();
    let mut current_heading_start: usize = 0;

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current_heading_level = Some(level as u8);
                current_heading_text.clear();
                current_heading_start = range.start;
            }
            Event::Text(text) if current_heading_level.is_some() => {
                current_heading_text.push_str(&text);
            }
            Event::Code(code) if current_heading_level.is_some() => {
                current_heading_text.push_str(&code);
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(level) = current_heading_level.take() {
                    let line = input[..current_heading_start]
                        .bytes()
                        .filter(|&b| b == b'\n')
                        .count()
                        + 1;
                    headings.push(HeadingInfo {
                        level,
                        text: std::mem::take(&mut current_heading_text),
                        byte_offset: current_heading_start,
                        line,
                    });
                }
            }
            _ => {}
        }
    }

    headings
}

// ---------------------------------------------------------------------------
// Section bounds (shared primitive)
// ---------------------------------------------------------------------------

/// Compute the byte range a heading "owns" in the document.
///
/// The range starts at `headings[i].byte_offset` and ends at the byte offset
/// of the next heading whose `level <= headings[i].level`. If no such
/// terminator exists, the range runs to `input_len`.
///
/// This is the shared "everything under heading H until the next same-or-
/// higher heading" primitive used by scratch-section stripping and phase
/// extraction (`summary::extract_phases`).
pub fn section_bounds(
    headings: &[HeadingInfo],
    i: usize,
    input_len: usize,
) -> std::ops::Range<usize> {
    let start = headings[i].byte_offset;
    let owner_level = headings[i].level;
    let end = headings
        .iter()
        .skip(i + 1)
        .find(|next| next.level <= owner_level)
        .map(|next| next.byte_offset)
        .unwrap_or(input_len);
    start..end
}

// ---------------------------------------------------------------------------
// Scratch section stripping (pulldown-cmark based)
// ---------------------------------------------------------------------------

/// Identify the byte ranges of all `[scratch]`-annotated heading sections.
///
/// Pure helper shared by `strip_scratch_sections` and
/// `strip_scratch_sections_with_report`. Each returned range corresponds to a
/// top-level scratch heading and extends through everything it owns (see
/// `section_bounds`).
fn scratch_section_ranges(input: &str) -> (Vec<HeadingInfo>, Vec<std::ops::Range<usize>>) {
    let headings = extract_headings(input);
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < headings.len() {
        if headings[i].text.to_lowercase().contains("[scratch]") {
            let range = section_bounds(&headings, i, input.len());
            // Advance past everything this scratch section consumed (skip any
            // descendant headings — they're inside this section, not separate
            // scratch sections to report).
            let owner_level = headings[i].level;
            i = headings
                .iter()
                .enumerate()
                .skip(i + 1)
                .find(|(_, h)| h.level <= owner_level)
                .map(|(j, _)| j)
                .unwrap_or(headings.len());
            ranges.push(range);
        } else {
            i += 1;
        }
    }
    (headings, ranges)
}

/// A section that was removed by scratch stripping.
///
/// Contains structural metadata (heading, descendant headings, byte range)
/// but **not** the body content — callers slice the original input with
/// `range` on demand. This keeps the struct allocation-free for callers that
/// only need the table-of-contents view.
#[derive(Debug, Clone)]
pub struct StrippedSection {
    pub heading: HeadingInfo,
    pub descendant_headings: Vec<HeadingInfo>,
    pub range: std::ops::Range<usize>,
}

/// Strip all `[scratch]`-annotated heading sections from a markdown document.
///
/// Thin wrapper around `strip_scratch_sections_with_report` that discards the
/// structured report. Preserves all original formatting byte-for-byte in
/// non-scratch regions.
pub fn strip_scratch_sections(input: &str) -> String {
    strip_scratch_sections_with_report(input).0
}

/// Strip scratch sections and return a structured report of what was removed.
///
/// Returns the stripped text alongside one `StrippedSection` per top-level
/// scratch heading, in document order. Each `StrippedSection` carries the
/// scratch heading itself, any descendant headings (level deeper than the
/// scratch heading's level) that fell within its byte range, and the exact
/// `Range<usize>` removed from `input`.
pub fn strip_scratch_sections_with_report(
    input: &str,
) -> (String, Vec<StrippedSection>) {
    if input.is_empty() {
        return (String::new(), Vec::new());
    }

    let (headings, removal_ranges) = scratch_section_ranges(input);

    if removal_ranges.is_empty() {
        return (input.to_string(), Vec::new());
    }

    // Build the report: for each removed range, find its owner heading and
    // any descendant headings inside the range.
    let mut report = Vec::with_capacity(removal_ranges.len());
    for range in &removal_ranges {
        let owner_idx = headings
            .iter()
            .position(|h| h.byte_offset == range.start)
            .expect("scratch range start always matches a heading offset");
        let owner = &headings[owner_idx];
        let descendants: Vec<HeadingInfo> = headings
            .iter()
            .skip(owner_idx + 1)
            .take_while(|h| h.byte_offset < range.end)
            .filter(|h| h.level > owner.level)
            .cloned()
            .collect();
        report.push(StrippedSection {
            heading: owner.clone(),
            descendant_headings: descendants,
            range: range.clone(),
        });
    }

    // Slice input around removal ranges, preserving original bytes
    let mut result = String::with_capacity(input.len());
    let mut cursor = 0;
    for range in &removal_ranges {
        if range.start > cursor {
            result.push_str(&input[cursor..range.start]);
        }
        cursor = range.end;
    }
    if cursor < input.len() {
        result.push_str(&input[cursor..]);
    }

    (result, report)
}

// ---------------------------------------------------------------------------
// Legacy helpers (used by strip_scratch_sections_legacy in tests)
// ---------------------------------------------------------------------------



#[cfg(test)]
mod tests {
    use super::*;

    // ── Metadata parser tests (callout format) ───────────────────────

    #[test]
    fn parse_metadata_callout_basic() {
        let input = "feat: my feature\n\n> [!plan]\n> status: 🔴\n> issue: MERC-123\n\n# Background\n";
        let (map, body) = parse_metadata(input);
        assert_eq!(map.get("status").unwrap(), "🔴");
        assert_eq!(map.get("issue").unwrap(), "MERC-123");
        assert!(body.contains("# Background"));
        assert!(!body.contains("> [!plan]"));
        assert!(!body.contains("> status:"));
    }

    #[test]
    fn parse_metadata_callout_multiple_keys() {
        let input = "title\n\n> [!plan]\n> status: 🔴\n> issue: MERC-123\n> priority: high\n";
        let (map, _body) = parse_metadata(input);
        assert_eq!(map.len(), 3);
        assert_eq!(map.get("status").unwrap(), "🔴");
        assert_eq!(map.get("issue").unwrap(), "MERC-123");
        assert_eq!(map.get("priority").unwrap(), "high");
    }

    #[test]
    fn parse_metadata_callout_with_blank_lines() {
        // Blank lines before and after callout don't affect parsing
        let input = "feat: title\n\n\n\n> [!plan]\n> status: 🔴\n\n\n# Background\n";
        let (map, body) = parse_metadata(input);
        assert_eq!(map.get("status").unwrap(), "🔴");
        assert!(body.contains("# Background"));
    }

    #[test]
    fn parse_metadata_no_callout() {
        let input = "feat: title\n\n# Background\nSome content.";
        let (map, body) = parse_metadata(input);
        assert!(map.is_empty());
        assert!(body.contains("# Background"));
        assert!(body.contains("Some content."));
    }

    #[test]
    fn parse_metadata_callout_body_extraction() {
        let input = "feat: title\nsome preamble\n\n> [!plan]\n> status: 🔴\n\n# Body\ntext\n";
        let (map, body) = parse_metadata(input);
        assert_eq!(map.get("status").unwrap(), "🔴");
        // Body should contain preamble and body section, but not callout
        assert!(body.contains("some preamble"));
        assert!(body.contains("# Body"));
        assert!(body.contains("text"));
        assert!(!body.contains("> [!plan]"));
        assert!(!body.contains("> status:"));
    }

    #[test]
    fn parse_metadata_non_metadata_line_ends_block() {
        // A "> " line without key: value pattern ends the metadata block
        let input = "title\n\n> [!plan]\n> status: 🔴\n> some prose line\n\n# Body\n";
        let (map, body) = parse_metadata(input);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("status").unwrap(), "🔴");
        // The "> some prose line" is NOT part of the metadata block and stays in body
        assert!(body.contains("> some prose line"));
    }

    #[test]
    fn parse_metadata_thematic_break_in_body() {
        // --- in body is not mistaken for metadata separator
        let input = "feat: title\n\n> [!plan]\n> status: 🔴\n\n---\n\ntext\n";
        let (map, body) = parse_metadata(input);
        assert_eq!(map.get("status").unwrap(), "🔴");
        assert!(body.contains("---"));
        assert!(body.contains("text"));
    }

    #[test]
    fn parse_metadata_case_insensitive_opener() {
        let input_lower = "title\n\n> [!plan]\n> status: 🔴\n";
        let input_upper = "title\n\n> [!PLAN]\n> status: 🔴\n";
        let input_mixed = "title\n\n> [!Plan]\n> status: 🔴\n";

        for input in [input_lower, input_upper, input_mixed] {
            let (map, _) = parse_metadata(input);
            assert_eq!(map.get("status").unwrap(), "🔴", "Failed for: {:?}", input);
        }
    }

    #[test]
    fn parse_metadata_single_line_input() {
        let input = "feat: title only";
        let (map, body) = parse_metadata(input);
        assert!(map.is_empty());
        assert_eq!(body, "");
    }

    // ── set_metadata_field tests (callout format) ────────────────────

    #[test]
    fn set_metadata_field_replace_existing() {
        let input = "feat: title\n\n> [!plan]\n> status: 🔴\n> issue: MERC-123\n\nbody\n";
        let result = set_metadata_field(input, "status", "✅");
        assert!(result.contains("> status: ✅"), "status should be replaced");
        assert!(result.contains("> issue: MERC-123"), "other fields preserved");
        assert!(!result.contains("> status: 🔴"), "old value should be gone");
        assert!(result.contains("body"), "body preserved");
        assert!(result.starts_with("feat: title\n"), "title preserved");
    }

    #[test]
    fn set_metadata_field_append_new_key() {
        let input = "feat: title\n\n> [!plan]\n> status: 🔴\n\nbody\n";
        let result = set_metadata_field(input, "issue", "MERC-456");
        assert!(result.contains("> status: 🔴"), "existing field preserved");
        assert!(result.contains("> issue: MERC-456"), "new field appended");
        assert!(result.contains("body"), "body preserved");
        assert!(result.starts_with("feat: title\n"), "title preserved");
    }

    #[test]
    fn set_metadata_field_creates_callout() {
        let input = "feat: my feature\n\n# Background\n";
        let result = set_metadata_field(input, "status", "🔴");
        assert!(result.contains("> [!plan]"), "callout block should be created");
        assert!(result.contains("> status: 🔴"), "field should be in callout");
        assert!(result.contains("# Background"), "body preserved");
        assert!(result.starts_with("feat: my feature\n"), "title preserved");
    }

    #[test]
    fn set_metadata_field_preserves_body() {
        let body_section = "\n# Background\n\n  Indented text.\n\n- list item\n";
        let input = format!("feat: my feature\n\n> [!plan]\n> status: 🔴\n{}", body_section);
        let result = set_metadata_field(&input, "status", "✅");
        assert!(result.contains(body_section), "body must be preserved byte-for-byte");
    }

    #[test]
    fn set_metadata_field_single_line_input() {
        let input = "feat: my feature";
        let result = set_metadata_field(input, "status", "🔴");
        assert!(result.contains("> [!plan]"), "callout block should be created");
        assert!(result.contains("> status: 🔴"), "field should be in callout");
        assert!(result.starts_with("feat: my feature\n"), "title preserved");
    }

    // ── canonical writer: placement, inline-tolerance, idempotency ───

    #[test]
    fn set_metadata_field_emits_callout_at_bottom() {
        let input = "feat: title\n\n# Background\n\nDetails.\n";
        let result = set_metadata_field(input, "status", "🔴");
        // The body precedes the callout; the callout is the trailing block.
        let body_pos = result.find("# Background").unwrap();
        let callout_pos = result.find("> [!plan]").unwrap();
        assert!(body_pos < callout_pos, "body must precede the callout:\n{}", result);
        assert!(result.trim_end().ends_with("> status: 🔴"),
            "callout must be the final block:\n{}", result);
    }

    #[test]
    fn set_metadata_field_inline_opener_upserts_in_place() {
        // Regression: status inline on the opener (`> [!plan] status: 🔴`) must
        // be upserted, not duplicated, and the stale 🔴 must not survive.
        let input = "refactor: x\n\n(plan: jj:abcd)\n\n> [!plan] status: 🔴\n";
        let result = set_metadata_field(input, "status", "✅");
        assert_eq!(result.matches("status:").count(), 1,
            "exactly one status line, got:\n{}", result);
        assert!(!result.contains('🔴'), "stale 🔴 must be gone:\n{}", result);
        assert!(result.contains("> [!plan]\n> status: ✅"),
            "canonical bare opener with ✅:\n{}", result);
        assert!(result.contains("(plan: jj:abcd)"), "self-reference preserved");
    }

    #[test]
    fn set_metadata_field_is_idempotent_fixpoint() {
        // f(x) == f(f(x)) byte-for-byte across representative shapes.
        let inputs = [
            "feat: a",
            "feat: b\n\n# Background\n\nText.\n",
            "feat: c\n\n> [!plan]\n> status: 🔴\n> issue: M-1\n\n# Body\n",
            "feat: d\n\n> [!plan] status: 🔴\n",
        ];
        for input in inputs {
            let once = set_metadata_field(input, "status", "✅");
            let twice = set_metadata_field(&once, "status", "✅");
            assert_eq!(once, twice, "writer must be a fixpoint for input:\n{}", input);
        }
    }

    #[test]
    fn set_metadata_field_dedupes_contradictory_callout() {
        // The exact reported contradiction: a stray inline 🔴 plus a `status: ✅`
        // line. The writer collapses to one canonical block.
        let input = "feat: x\n\n> [!plan] status: 🔴\n> status: ✅\n";
        let result = set_metadata_field(input, "status", "✅");
        assert_eq!(result.matches("> [!plan]").count(), 1, "one callout:\n{}", result);
        assert_eq!(result.matches("status:").count(), 1, "one status:\n{}", result);
        assert!(!result.contains('🔴'));
    }

    // ── Existing scratch stripping tests (must pass with new impl) ───

    // ── strip_scratch_sections_with_report tests ─────────────────────

    #[test]
    fn strip_scratch_sections_with_report_no_scratch() {
        let input = "# Title\n\n## Section\n\ncontent\n";
        let (stripped, report) = strip_scratch_sections_with_report(input);
        assert_eq!(stripped, input);
        assert!(report.is_empty());
    }

    #[test]
    fn strip_scratch_sections_with_report_single_top_level() {
        let input = "# Title\n\n## Notes [scratch]\n\nhidden content\n";
        let (stripped, report) = strip_scratch_sections_with_report(input);
        assert!(!stripped.contains("[scratch]"));
        assert!(!stripped.contains("hidden content"));
        assert_eq!(report.len(), 1);
        assert!(report[0].heading.text.contains("[scratch]"));
        assert_eq!(report[0].heading.level, 2);
        assert!(report[0].descendant_headings.is_empty());
        // The range should slice back to the exact removed bytes.
        assert_eq!(
            &input[report[0].range.clone()],
            "## Notes [scratch]\n\nhidden content\n",
            "range should match the removed slice byte-for-byte"
        );
    }

    #[test]
    fn strip_scratch_sections_with_report_with_descendants() {
        let input = "# Title\n\n## Notes [scratch]\n\nbody\n\n### Sub\n\nnested\n\n#### Deeper\n\nmore nested\n";
        let (_stripped, report) = strip_scratch_sections_with_report(input);
        assert_eq!(report.len(), 1);
        let section = &report[0];
        assert_eq!(section.heading.level, 2);
        assert_eq!(section.descendant_headings.len(), 2);
        assert_eq!(section.descendant_headings[0].text, "Sub");
        assert_eq!(section.descendant_headings[0].level, 3);
        assert_eq!(section.descendant_headings[1].text, "Deeper");
        assert_eq!(section.descendant_headings[1].level, 4);
    }

    #[test]
    fn strip_scratch_sections_with_report_multiple_top_levels() {
        let input = "# Title\n\n## A [scratch]\n\nfirst\n\n## Keep\n\nmiddle\n\n## B [scratch]\n\nsecond\n";
        let (stripped, report) = strip_scratch_sections_with_report(input);
        assert!(stripped.contains("## Keep"));
        assert!(stripped.contains("middle"));
        assert!(!stripped.contains("[scratch]"));
        assert_eq!(report.len(), 2);
        assert!(report[0].heading.text.contains("A"));
        assert!(report[1].heading.text.contains("B"));
        // Report is in document order
        assert!(report[0].range.start < report[1].range.start);
    }

    #[test]
    fn as_done_with_report_keep_scratch_preserves_content_and_returns_empty_report() {
        // Protects the data-loss invariant: if keep_scratch=true is set, neither
        // the returned String nor the report should silently strip content.
        let input = "feat: title\n\n> [!plan]\n> status: 🔴\n\n## Notes [scratch]\n\nimportant learnings\n";
        let doc = PlanDocument::parse(input);
        let (final_desc, report) = doc.as_done_with_report(true);
        assert!(report.is_empty(), "keep_scratch must produce an empty report");
        assert!(final_desc.contains("important learnings"),
            "keep_scratch must preserve scratch body in the returned String");
        assert!(final_desc.contains("> status: ✅"), "status should still be stamped");
    }

    #[test]
    fn strip_scratch_sections_with_report_no_double_count() {
        let input = "# Title\n\n## A [scratch]\n\nfirst\n\n## Keep\n\nmiddle content\n\n## B [scratch]\n\nsecond\n";
        let (_stripped, report) = strip_scratch_sections_with_report(input);
        assert_eq!(report.len(), 2);
        // The "middle content" between the two scratch sections must not fall
        // within either reported range.
        let middle_offset = input.find("middle content").unwrap();
        for section in &report {
            assert!(
                !section.range.contains(&middle_offset),
                "middle content should not be in any scratch range, was in {:?}",
                section.range
            );
        }
    }

    // ── PlanDocument tests (callout format) ──────────────────────────

    #[test]
    fn plan_document_parse_with_callout() {
        let input = "feat: my feature\n\n> [!plan]\n> status: 🔴\n> issue: MERC-123\n\n# Background\n\nDetails.\n";
        let doc = PlanDocument::parse(input);
        assert_eq!(doc.title(), "feat: my feature");
        assert_eq!(doc.metadata().get("status").unwrap(), "🔴");
        assert_eq!(doc.metadata().get("issue").unwrap(), "MERC-123");
        assert!(doc.body().contains("# Background"));
        assert!(!doc.body().contains("> [!plan]"));
    }

    #[test]
    fn plan_document_is_done_callout() {
        let input = "feat: title\n\n> [!plan]\n> status: ✅\n";
        let doc = PlanDocument::parse(input);
        assert!(doc.is_done());
    }

    #[test]
    fn plan_document_is_done_not_done() {
        let input = "feat: title\n\n> [!plan]\n> status: 🔴\n";
        let doc = PlanDocument::parse(input);
        assert!(!doc.is_done());
    }

    #[test]
    fn plan_document_as_done_sets_status() {
        let input = "feat: title\n\n> [!plan]\n> status: 🔴\n\n# Body\n";
        let doc = PlanDocument::parse(input);
        let result = doc.as_done(false);
        assert!(result.contains("> status: ✅"));
        assert!(!result.contains("> status: 🔴"));
        assert!(result.contains("# Body"));
    }

    #[test]
    fn plan_document_as_done_creates_callout() {
        let input = "feat: add something\n\n# Background\n\nSome details.";
        let doc = PlanDocument::parse(input);
        let result = doc.as_done(false);
        assert!(result.contains("> [!plan]"));
        assert!(result.contains("> status: ✅"));
        assert!(result.contains("# Background"));
    }

    #[test]
    fn plan_document_as_done_strips_scratch() {
        let input = "feat: title\n\n> [!plan]\n> status: 🔴\n\n# Keep\n\nVisible.\n\n# Notes [scratch]\n\nHidden.\n";
        let doc = PlanDocument::parse(input);
        let result = doc.as_done(false);
        assert!(result.contains("> status: ✅"));
        assert!(result.contains("# Keep"));
        assert!(result.contains("Visible."));
        assert!(!result.contains("[scratch]"));
        assert!(!result.contains("Hidden."));
    }

    #[test]
    fn plan_document_as_done_keep_scratch() {
        let input = "feat: title\n\n> [!plan]\n> status: 🔴\n\n# Notes [scratch]\n\nKept.\n";
        let doc = PlanDocument::parse(input);
        let result = doc.as_done(true);
        assert!(result.contains("> status: ✅"));
        assert!(result.contains("[scratch]"));
        assert!(result.contains("Kept."));
    }

    #[test]
    fn plan_document_as_done_trailing_scratch_preserves_metadata() {
        // Headline regression: with the callout at the bottom, a trailing
        // `[scratch]` section must NOT swallow it. `done` strips the body (where
        // the scratch lives), not the callout — so `issue` survives, not just
        // `status`. Naively stripping the raw string would lose `issue` here.
        let input = "feat: title\n\n\
                     > [!plan]\n> status: 🔴\n> issue: M-1\n\n\
                     # Resources [scratch]\n\nthrowaway\n";
        let doc = PlanDocument::parse(input);
        let result = doc.as_done(false);
        assert!(result.contains("> issue: M-1"),
            "non-status metadata must survive a trailing scratch section:\n{}", result);
        assert!(result.contains("> status: ✅"), "status stamped:\n{}", result);
        assert!(!result.contains("[scratch]"), "scratch removed");
        assert!(!result.contains("throwaway"), "scratch body removed");
    }

    #[test]
    fn parse_metadata_top_placed_callout_still_parses() {
        // Backward-compat guard: callouts written under the old (top) layout
        // must still parse — reads are position-independent.
        let input = "feat: title\n\n> [!plan]\n> status: ✅\n\n# Background\n\nBody.\n";
        let doc = PlanDocument::parse(input);
        assert!(doc.is_done(), "top-placed status: ✅ must read as done");
        assert!(doc.body().contains("# Background"));
    }

    #[test]
    fn parse_metadata_inline_only_status_is_read() {
        // is_done() must be correct when the only status is inline on the opener.
        let doc = PlanDocument::parse("feat: title\n\n> [!plan] status: ✅\n");
        assert!(doc.is_done(), "inline-opener status must be read");
    }

    #[test]
    fn plan_document_pr_parts_strips_callout() {
        let input = "feat: my feature\n\n> [!plan]\n> status: 🔴\n> issue: MERC-123\n\n# Background\n\nDetails.\n";
        let (title, body) = PlanDocument::parse(input).pr_parts().unwrap();
        assert_eq!(title, "feat: my feature");
        assert!(!body.contains("> [!plan]"));
        assert!(!body.contains("> status:"));
        assert!(!body.contains("> issue:"));
        assert!(body.contains("# Background"));
    }

    #[test]
    fn plan_document_pr_parts_basic() {
        let input = "feat: title\n\n# Background\n\nContent.\n";
        let (title, body) = PlanDocument::parse(input).pr_parts().unwrap();
        assert_eq!(title, "feat: title");
        assert!(body.contains("# Background"));
    }

    #[test]
    fn plan_document_pr_parts_empty_title() {
        assert!(PlanDocument::parse("").pr_parts().is_none());
        assert!(PlanDocument::parse("   \n\nbody").pr_parts().is_none());
    }

    #[test]
    fn plan_document_title_edge_cases() {
        // Empty input
        let doc = PlanDocument::parse("");
        assert_eq!(doc.title(), "");
        assert!(doc.body().is_empty());
        assert!(doc.metadata().is_empty());

        // Whitespace-only title
        let doc = PlanDocument::parse("   ");
        assert_eq!(doc.title(), "   ");
        assert!(doc.body().is_empty());

        // Title with no body
        let doc = PlanDocument::parse("feat: just a title");
        assert_eq!(doc.title(), "feat: just a title");
        assert!(doc.body().is_empty());
    }

    // ── extract_headings tests ──────────────────────────────────────

    #[test]
    fn test_extract_headings_basic() {
        let input = "Title line\n\n# First\n\nSome text.\n\n## Second\n\n### Third\n";
        let headings = extract_headings(input);
        assert_eq!(headings.len(), 3);

        assert_eq!(headings[0].level, 1);
        assert_eq!(headings[0].text, "First");
        assert_eq!(headings[0].line, 3);

        assert_eq!(headings[1].level, 2);
        assert_eq!(headings[1].text, "Second");
        assert_eq!(headings[1].line, 7);

        assert_eq!(headings[2].level, 3);
        assert_eq!(headings[2].text, "Third");
        assert_eq!(headings[2].line, 9);
    }

    #[test]
    fn test_extract_headings_ignores_fenced_code() {
        let input = "# Real heading\n\n```\n# Not a heading\n```\n\n## Also real\n";
        let headings = extract_headings(input);
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].text, "Real heading");
        assert_eq!(headings[1].text, "Also real");

        // Tilde fences too
        let input2 = "# Top\n\n~~~\n## Fake\n~~~\n\n### Bottom\n";
        let headings2 = extract_headings(input2);
        assert_eq!(headings2.len(), 2);
        assert_eq!(headings2[0].text, "Top");
        assert_eq!(headings2[1].text, "Bottom");
    }

    #[test]
    fn test_extract_headings_setext() {
        let input = "Setext H1\n=========\n\nSome text.\n\nSetext H2\n---------\n";
        let headings = extract_headings(input);
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].level, 1);
        assert_eq!(headings[0].text, "Setext H1");
        assert_eq!(headings[0].line, 1);
        assert_eq!(headings[1].level, 2);
        assert_eq!(headings[1].text, "Setext H2");
        assert_eq!(headings[1].line, 6);
    }

    #[test]
    fn test_extract_headings_empty_input() {
        let headings = extract_headings("");
        assert!(headings.is_empty());
    }

    // ── section_bounds tests ────────────────────────────────────────

    #[test]
    fn section_bounds_next_same_level_terminates() {
        let input = "## A\n\nbody a\n\n## B\n\nbody b\n";
        let headings = extract_headings(input);
        let range = section_bounds(&headings, 0, input.len());
        assert_eq!(&input[range], "## A\n\nbody a\n\n",
            "range should end at the next level-2 heading");
    }

    #[test]
    fn section_bounds_higher_level_terminates() {
        let input = "### A\n\nbody a\n\n## B\n\nbody b\n";
        let headings = extract_headings(input);
        let range = section_bounds(&headings, 0, input.len());
        assert_eq!(&input[range], "### A\n\nbody a\n\n",
            "a higher-level heading (fewer #s) terminates the section");
    }

    #[test]
    fn section_bounds_deeper_level_does_not_terminate() {
        let input = "## A\n\n### A.1\n\nnested\n\n## B\n\nbody b\n";
        let headings = extract_headings(input);
        let range = section_bounds(&headings, 0, input.len());
        assert_eq!(&input[range], "## A\n\n### A.1\n\nnested\n\n",
            "a deeper-level heading is owned by A; range ends only at the next ## B");
    }

    #[test]
    fn section_bounds_runs_to_eof() {
        let input = "## Only\n\nbody, no terminator\n";
        let headings = extract_headings(input);
        let range = section_bounds(&headings, 0, input.len());
        assert_eq!(range.end, input.len(),
            "with no terminator the range runs to input_len");
    }

    // ── Scratch stripping tests ─────────────────────────────────────

    #[test]
    fn test_no_scratch_sections() {
        let input = "# Title\n\nSome content.\n\n## Section\n\nMore content.\n";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, input,
            "Input with no [scratch] sections should be returned unchanged"
        );
    }

    #[test]
    fn test_basic_scratch_strip() {
        let input = "\
# Title

Some intro.

## Notes [scratch]

These are scratch notes.
They should be removed.

## Real Section

Keep this.
";
        let expected = "\
# Title

Some intro.

## Real Section

Keep this.
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, expected,
            "A single ## [scratch] section should be stripped, preserving content before and after"
        );
    }

    #[test]
    fn test_scratch_at_eof() {
        let input = "\
# Title

Content here.

## Scratch Pad [scratch]

This is at the end.
No more headings follow.
";
        let expected = "\
# Title

Content here.

";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, expected,
            "A [scratch] section at end of document should strip everything to end"
        );
    }

    #[test]
    fn test_multi_level_strip() {
        let input = "\
# Title

## Section

### Notes [scratch]

Scratch content.

### Another Section

Keep this.
";
        let expected = "\
# Title

## Section

### Another Section

Keep this.
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, expected,
            "### [scratch] should strip until next ### or higher level"
        );
    }

    #[test]
    fn test_nested_headings_stripped() {
        let input = "\
# Title

### Deep scratch [scratch]

Some text.

#### Even deeper

This is nested inside the scratch section.

##### Way deeper

Still inside.

### Next section

Kept.
";
        let expected = "\
# Title

### Next section

Kept.
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, expected,
            "Headings deeper than the scratch level (####, #####) should also be stripped"
        );
    }

    #[test]
    fn test_code_fence_immunity() {
        let input = "\
# Title

```
# This is not a heading
## Neither is this
### [scratch] — not a real heading
```

## Real section

Content.
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, input,
            "Lines inside ``` code fences must NOT be treated as headings"
        );
    }

    #[test]
    fn test_tilde_fence_immunity() {
        let input = "\
# Title

~~~
# Fake heading
## Also fake [scratch]
~~~

## Real section

Content.
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, input,
            "Lines inside ~~~ code fences must NOT be treated as headings"
        );
    }

    #[test]
    fn test_multiple_scratch_sections() {
        let input = "\
# Title

## Intro

Hello.

## Notes [scratch]

Scratch 1.

## Middle

Keep this.

## Draft [scratch]

Scratch 2.

## Conclusion

Done.
";
        let expected = "\
# Title

## Intro

Hello.

## Middle

Keep this.

## Conclusion

Done.
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, expected,
            "Multiple [scratch] sections should each be stripped independently"
        );
    }

    #[test]
    fn test_adjacent_headings() {
        let input = "\
## A [scratch]
## B
## C
";
        let expected = "\
## B
## C
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, expected,
            "When ## A [scratch] is immediately followed by ## B, only ## A line should be removed"
        );
    }

    #[test]
    fn test_mixed_case() {
        let input_variants = [
            "## Notes [Scratch]\n\nContent.\n\n## Next\n",
            "## Notes [SCRATCH]\n\nContent.\n\n## Next\n",
            "## Notes [sCrAtCh]\n\nContent.\n\n## Next\n",
        ];
        let expected = "## Next\n";
        for (i, input) in input_variants.iter().enumerate() {
            let result = strip_scratch_sections(input);
            assert_eq!(
                result, expected,
                "Case variant {} ([scratch] in mixed case) should be detected and stripped",
                i
            );
        }
    }

    #[test]
    fn test_scratch_in_heading_text() {
        let input = "\
# Title

## Analysis [scratch]

Deep thoughts here.
Very important scratch work.

## Results

Final results.
";
        let expected = "\
# Title

## Results

Final results.
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, expected,
            "[scratch] appearing after heading text (e.g. '## Analysis [scratch]') should strip the whole section"
        );
    }

    #[test]
    fn test_preserves_trailing_newline() {
        let with_newline = "# Title\n\nContent.\n";
        let result = strip_scratch_sections(with_newline);
        assert!(
            result.ends_with('\n'),
            "Output should end with newline when input ends with newline"
        );

        let without_newline = "# Title\n\nContent.";
        let result = strip_scratch_sections(without_newline);
        assert!(
            !result.ends_with('\n'),
            "Output should NOT end with newline when input doesn't end with newline"
        );
    }

    #[test]
    fn test_empty_input() {
        let result = strip_scratch_sections("");
        assert_eq!(result, "", "Empty input should return empty string");
    }

    // ── Additional edge-case tests ────────────────────────────────────

    #[test]
    fn test_code_fence_with_info_string() {
        let input = "\
# Title

```rust
# this is a rust attribute-style comment, not a heading
## [scratch] — still not a heading
```

## Kept

Content.
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, input,
            "Code fences with info strings should still protect contents from heading parsing"
        );
    }

    #[test]
    fn test_code_fence_inside_scratch_section() {
        let input = "\
# Title

## Scratch [scratch]

Some code:

```
# inside fence inside scratch
```

More scratch text.

## After

Kept.
";
        let expected = "\
# Title

## After

Kept.
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, expected,
            "Code fences within a scratch section should be stripped along with the section"
        );
    }

    #[test]
    fn test_higher_level_heading_stops_strip() {
        let input = "\
### Scratch [scratch]

Content to strip.

## Higher level heading

This is kept.
";
        let expected = "\
## Higher level heading

This is kept.
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, expected,
            "A heading at a higher level (fewer #s) than the scratch heading should stop stripping"
        );
    }

    #[test]
    fn test_fence_closer_must_match_opener_char() {
        let input = "\
# Title

```
~~~
# Not a heading — still inside backtick fence
~~~
```

## Kept
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, input,
            "A ~~~ line should not close a ``` fence; fence char types must match"
        );
    }

    #[test]
    fn test_fence_closer_needs_enough_chars() {
        let input = "\
# Title

````
```
# Not a heading — three backticks don't close a four-backtick fence
```
````

## Kept
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, input,
            "A closer must have at least as many fence chars as the opener"
        );
    }

    #[test]
    fn test_scratch_not_in_heading_ignored() {
        let input = "\
# Title

This line mentions [scratch] but is not a heading.

## Real section

Content.
";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, input,
            "[scratch] in body text (not a heading) should be ignored"
        );
    }

    #[test]
    fn test_only_scratch_content() {
        let input = "## Everything [scratch]\n\nAll content here.\n";
        let result = strip_scratch_sections(input);
        assert_eq!(
            result, "",
            "If the entire document is a single scratch section, result should be empty"
        );
    }

    // ── New pulldown-cmark-specific tests ────────────────────────────

    #[test]
    fn test_scratch_strip_with_callout_metadata() {
        let input = "\
feat: my feature

> [!plan]
> status: 🔴
> issue: MERC-123

# Background

Some info.

# Notes [scratch]

Private notes here.

# Results

Final results.
";
        let result = strip_scratch_sections(input);
        assert!(result.contains("> [!plan]"),
            "callout block should be preserved");
        assert!(result.contains("> status: 🔴"),
            "callout metadata should be preserved");
        assert!(result.contains("# Background\n\nSome info.\n"),
            "non-scratch content preserved");
        assert!(result.contains("# Results\n\nFinal results.\n"),
            "content after scratch preserved");
        assert!(!result.contains("[scratch]"),
            "scratch section should be removed");
        assert!(!result.contains("Private notes"),
            "scratch content should be removed");
    }

    #[test]
    fn test_scratch_strip_setext_heading() {
        let input = "\
# Title

Some intro.

Notes [scratch]
-----------

These are scratch notes.
They should be removed.

## Real Section

Keep this.
";
        let result = strip_scratch_sections(input);
        assert!(result.contains("# Title\n\nSome intro.\n"),
            "content before scratch should be preserved");
        assert!(result.contains("## Real Section\n\nKeep this.\n"),
            "content after scratch should be preserved");
        assert!(!result.contains("[scratch]"),
            "setext scratch heading should be stripped");
        assert!(!result.contains("scratch notes"),
            "scratch content should be stripped");
    }

    #[test]
    fn test_scratch_strip_preserves_formatting() {
        let input = "\
# Title

| Col A | Col B |
|-------|-------|
| 1     | 2     |

## Notes [scratch]

Scratch content.

## Details

- [ ] task item
- [x] done item

    indented code block

> blockquote here
";
        let result = strip_scratch_sections(input);
        // Table must be byte-identical
        assert!(result.contains("| Col A | Col B |\n|-------|-------|\n| 1     | 2     |"),
            "table formatting must be byte-identical");
        // Task lists preserved
        assert!(result.contains("- [ ] task item\n- [x] done item"),
            "task lists must be preserved");
        // Indented code block preserved
        assert!(result.contains("    indented code block"),
            "indented code block must be preserved");
        // Blockquote preserved
        assert!(result.contains("> blockquote here"),
            "blockquote must be preserved");
        // Scratch gone
        assert!(!result.contains("Scratch content"),
            "scratch content must be removed");
    }

    #[test]
    fn test_scratch_strip_setext_h1() {
        // Setext H1 heading (=== underline) is level 1.
        // Only another H1 (or end of document) stops a level-1 scratch section.
        // H2 sub-headings are consumed by the scratch section.
        let input = "\
# Title

Analysis [scratch]
===================

Private analysis.

## Sub-heading inside scratch

Also removed.

Conclusion
==========

Done.
";
        let result = strip_scratch_sections(input);
        assert!(!result.contains("[scratch]"), "scratch heading removed");
        assert!(!result.contains("Private analysis"), "scratch content removed");
        assert!(!result.contains("Sub-heading inside scratch"), "nested H2 inside H1 scratch removed");
        assert!(result.contains("Conclusion\n==========\n\nDone."), "same-level setext H1 stops the scratch section");
    }

    // ── Scratch stripping with thematic break ────────────────────────

    #[test]
    fn test_scratch_strip_thematic_break_in_body() {
        let input = "\
# Title

---

## Notes [scratch]

Hidden.

## Kept

Visible.
";
        let result = strip_scratch_sections(input);
        assert!(result.contains("# Title"), "title preserved");
        assert!(result.contains("---"), "thematic break preserved");
        assert!(result.contains("## Kept"), "non-scratch section preserved");
        assert!(result.contains("Visible."), "non-scratch content preserved");
        assert!(!result.contains("[scratch]"), "scratch heading removed");
        assert!(!result.contains("Hidden."), "scratch content removed");
    }
}