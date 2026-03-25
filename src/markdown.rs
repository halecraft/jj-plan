use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Metadata parsing (Obsidian-style callout block format)
// ---------------------------------------------------------------------------

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

/// Check if a line is a `> [!plan]` callout opener (case-insensitive on `plan`).
fn is_callout_opener(line: &str) -> bool {
    let trimmed = line.trim_end();
    if !trimmed.starts_with("> [!") {
        return false;
    }
    let after_prefix = &trimmed[4..];
    let close_bracket = match after_prefix.find(']') {
        Some(p) => p,
        None => return false,
    };
    let tag = &after_prefix[..close_bracket];
    tag.eq_ignore_ascii_case("plan")
}

/// Extract Obsidian-style callout metadata from a plan description.
///
/// Format:
/// ```text
/// feat: my feature          ← line 1: always the title
///
/// > [!plan]                 ← callout opener (case-insensitive)
/// > status: 🔴              ← metadata key: value lines
/// > issue: MERC-123
///
/// # Background              ← body
/// ```
///
/// Returns a map of key-value pairs and the body (input with the title line
/// and callout block lines removed). If no `> [!plan]` block is found,
/// returns an empty map and everything after line 1 as the body.
///
/// Parsing rules:
/// - Line 1 is always the title — never metadata.
/// - Scan all lines (after title) for `> [!plan]` (the callout opener).
/// - Read subsequent `> key: value` lines. The block ends at the first line
///   that doesn't start with `> ` or doesn't match `key: value` pattern.
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

    // Collect metadata lines: lines after the opener that start with "> "
    // and whose content (after "> ") matches the key: value pattern.
    let mut map = BTreeMap::new();
    let mut block_end = opener_idx + 1; // exclusive index past last callout line

    for line in &lines[opener_idx + 1..] {
        if let Some(content) = line.strip_prefix("> ")
            && is_callout_metadata_line(content) {
                let colon_pos = content.find(':').unwrap();
                let key = &content[..colon_pos];
                let value = content[colon_pos + 1..].trim();
                map.insert(key.to_string(), value.to_string());
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

/// Set a metadata field in a callout block, creating the block if needed.
///
/// If a `> [!plan]` block exists, replaces or appends the key within it.
/// If no callout block exists, inserts one after the title line.
/// All other content is preserved byte-for-byte.
pub fn set_metadata_field(input: &str, key: &str, value: &str) -> String {
    let title_end = input.find('\n').unwrap_or(input.len());
    if title_end == input.len() {
        // Single line (title only) — append callout block
        return format!("{}\n\n> [!plan]\n> {}: {}\n", input, key, value);
    }

    let title_line = &input[..title_end];
    let after_title = &input[title_end + 1..];
    let lines: Vec<&str> = after_title.lines().collect();

    // Find the callout opener
    let opener_idx = lines.iter().position(|l| is_callout_opener(l));

    match opener_idx {
        Some(idx) => {
            // Callout exists — find its metadata lines, replace or append key
            let mut meta_lines: Vec<String> = Vec::new();
            let mut found = false;
            let mut block_end = idx + 1;

            for line in &lines[idx + 1..] {
                if let Some(content) = line.strip_prefix("> ")
                    && is_callout_metadata_line(content) {
                        let colon_pos = content.find(':').unwrap();
                        let existing_key = &content[..colon_pos];
                        if existing_key == key {
                            meta_lines.push(format!("> {}: {}", key, value));
                            found = true;
                        } else {
                            meta_lines.push((*line).to_string());
                        }
                        block_end += 1;
                        continue;
                    }
                break;
            }

            if !found {
                meta_lines.push(format!("> {}: {}", key, value));
            }

            // Rebuild: title + lines before callout + opener + meta lines + lines after callout
            let mut result = String::with_capacity(input.len() + 32);
            result.push_str(title_line);
            result.push('\n');
            for line in &lines[..idx] {
                result.push_str(line);
                result.push('\n');
            }
            result.push_str(lines[idx]); // opener line
            result.push('\n');
            for ml in &meta_lines {
                result.push_str(ml);
                result.push('\n');
            }
            for line in &lines[block_end..] {
                result.push_str(line);
                result.push('\n');
            }
            // Match original trailing newline behavior
            if !after_title.ends_with('\n') && result.ends_with('\n') {
                result.pop();
            }
            result
        }
        None => {
            // No callout block — insert one after title line
            format!(
                "{}\n\n> [!plan]\n> {}: {}\n\n{}",
                title_line, key, value, after_title
            )
        }
    }
}

/// Return the input with the callout block and title removed.
///
/// If there is no callout, returns everything after line 1.
pub fn remove_metadata(input: &str) -> String {
    let (_, body) = parse_metadata(input);
    body
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

    /// The complete "mark as done" transformation.
    ///
    /// 1. If `!keep_scratch`, strips `[scratch]` sections from the full document.
    /// 2. Sets metadata `status: ✅` in the callout block.
    ///
    /// Idempotent: if status is already `✅`, still strips scratch (if requested)
    /// but doesn't double-stamp.
    pub fn as_done(&self, keep_scratch: bool) -> String {
        let base = if keep_scratch {
            self.raw.clone()
        } else {
            strip_scratch_sections(&self.raw)
        };
        set_metadata_field(&base, "status", "✅")
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
// Scratch section stripping (pulldown-cmark based)
// ---------------------------------------------------------------------------

/// Strip all `[scratch]`-annotated heading sections from a markdown document.
///
/// Uses `pulldown-cmark` with `into_offset_iter()` for proper CommonMark heading
/// detection (ATX and setext headings, code fence awareness). Front matter is
/// extracted first (pulldown-cmark would misparse `---` as a thematic break),
/// then heading events define byte ranges for scratch sections, which are sliced
/// out of the original body text — preserving all original formatting byte-for-byte
/// in non-scratch regions.
pub fn strip_scratch_sections(input: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    if input.is_empty() {
        return String::new();
    }

    // Parse the entire input with pulldown-cmark to find heading sections.
    // With the callout metadata format, there is no `---` metadata delimiter
    // to confuse pulldown-cmark — callout blocks are valid blockquotes.
    struct HeadingInfo {
        level: u8,
        text: String,
        start: usize, // byte offset into `body`
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
                // Heading text might contain inline code
                current_heading_text.push_str(&code);
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(level) = current_heading_level.take() {
                    headings.push(HeadingInfo {
                        level,
                        text: std::mem::take(&mut current_heading_text),
                        start: current_heading_start,
                    });
                }
            }
            _ => {}
        }
    }

    // 3. Identify scratch section byte ranges to remove
    //    A scratch section: starts at a heading with [scratch] in its text,
    //    extends until the next heading of same or higher level, or end of body.
    let mut removal_ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let mut i = 0;
    while i < headings.len() {
        let h = &headings[i];
        if h.text.to_lowercase().contains("[scratch]") {
            let scratch_start = h.start;
            let scratch_level = h.level;

            // Find where this section ends
            let mut end = input.len();
            for (j, heading) in headings.iter().enumerate().skip(i + 1) {
                if heading.level <= scratch_level {
                    end = heading.start;
                    i = j; // continue scanning from this heading
                    break;
                }
            }
            if end == input.len() {
                i = headings.len(); // consumed everything to the end
            }

            removal_ranges.push(scratch_start..end);
        } else {
            i += 1;
        }
    }

    if removal_ranges.is_empty() {
        return input.to_string();
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

    if result.is_empty() {
        return String::new();
    }

    result
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

    // ── remove_metadata tests (callout format) ───────────────────────

    #[test]
    fn remove_metadata_strips_callout() {
        let input = "feat: title\n\n> [!plan]\n> status: 🔴\n> issue: MERC-123\n\nbody text here\n";
        let result = remove_metadata(input);
        assert!(result.contains("body text here"), "body should remain");
        assert!(!result.contains("> [!plan]"), "callout should be stripped");
        assert!(!result.contains("> status:"), "metadata should be stripped");
    }

    #[test]
    fn remove_metadata_no_callout() {
        let input = "feat: title\n\nbody text";
        let result = remove_metadata(input);
        assert!(result.contains("body text"));
    }

    #[test]
    fn remove_metadata_single_line() {
        let input = "feat: title";
        let result = remove_metadata(input);
        assert!(result.is_empty());
    }

    // ── Existing scratch stripping tests (must pass with new impl) ───

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