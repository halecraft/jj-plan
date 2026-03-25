use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Metadata parsing (summary-first format)
// ---------------------------------------------------------------------------

/// Check if a line looks like a metadata key: `^[a-z][a-z0-9_-]*:\s`.
///
/// This prevents false positives from prose lines with colons
/// (e.g. "Note: this is important" — `Note` starts with uppercase).
fn is_metadata_key_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    // First char must be lowercase ascii letter
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    // Find the colon
    let colon_pos = match line.find(':') {
        Some(p) => p,
        None => return false,
    };
    // Everything before the colon must be [a-z0-9_-]
    if !line[..colon_pos]
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return false;
    }
    // Must have a space (or end of line) after the colon
    let after_colon = &line[colon_pos + 1..];
    after_colon.is_empty() || after_colon.starts_with(' ')
}

/// Extract summary-first metadata from a plan description.
///
/// Format:
/// ```text
/// feat: my feature          ← line 1: always the title
/// status: 🔴                ← metadata key: value lines
/// issue: MERC-123
/// ---                       ← separator
///
/// # Background              ← body
/// ```
///
/// Returns a map of key-value pairs and the body slice (everything after
/// the `---` separator). If no metadata block is found, returns an empty
/// map and everything after line 1 as the body.
///
/// Parsing rules:
/// - Line 1 is always the title — never metadata.
/// - Starting at line 2, scan for lines matching `^[a-z][a-z0-9_-]*: `.
/// - The metadata region ends at the first `---` line.
/// - If a non-metadata line is encountered before `---`, there is no
///   metadata — the entire description after line 1 is body.
/// - Key names restricted to `[a-z][a-z0-9_-]*` to prevent false positives.
pub fn parse_metadata(input: &str) -> (BTreeMap<String, String>, &str) {
    // Need at least a title line
    let first_newline = match input.find('\n') {
        Some(pos) => pos,
        None => return (BTreeMap::new(), ""), // single line, no body
    };

    let after_title = &input[first_newline + 1..];

    // Scan metadata lines: contiguous key: value lines terminated by ---
    let mut map = BTreeMap::new();
    let mut cursor = 0; // byte offset within after_title

    for line in after_title.lines() {
        if line == "---" {
            // Found the separator — body starts after "---\n"
            let sep_end = cursor + 3; // skip "---"
            let body_start = if first_newline + 1 + sep_end < input.len() {
                // Skip the \n after ---
                first_newline + 1 + sep_end + 1
            } else {
                input.len()
            };
            let body = &input[body_start..];
            return (map, body);
        }

        if !is_metadata_key_line(line) {
            // Non-metadata line before --- → no metadata block
            // Body is everything after line 1
            return (BTreeMap::new(), after_title);
        }

        // Parse key: value
        let colon_pos = line.find(':').unwrap(); // safe: is_metadata_key_line checks this
        let key = &line[..colon_pos];
        let value = line[colon_pos + 1..].trim();
        map.insert(key.to_string(), value.to_string());

        cursor += line.len() + 1; // +1 for the \n
    }

    // Reached end of input without finding --- → no metadata
    // (all lines looked like metadata keys but no separator)
    (BTreeMap::new(), after_title)
}

/// Set a metadata field, creating the metadata block if it doesn't exist.
///
/// If the key already exists, its value is replaced. If no metadata block
/// exists, inserts `key: value\n---\n` after line 1. All other content is
/// preserved byte-for-byte.
pub fn set_metadata_field(input: &str, key: &str, value: &str) -> String {
    let first_newline = match input.find('\n') {
        Some(pos) => pos,
        None => {
            // Single line (title only) — append metadata + separator
            return format!("{}\n{}: {}\n---\n", input, key, value);
        }
    };

    let title_line = &input[..first_newline];
    let after_title = &input[first_newline + 1..];

    // Check if there's an existing metadata block
    let (existing_map, _) = parse_metadata(input);

    if existing_map.is_empty() {
        // No existing metadata — insert key: value + --- after title
        return format!("{}\n{}: {}\n---\n{}", title_line, key, value, after_title);
    }

    // Has existing metadata — rebuild the metadata lines
    // Find the --- separator position in after_title
    let mut cursor = 0;
    let mut meta_lines: Vec<String> = Vec::new();
    let mut found = false;
    let mut body_after_sep = "";

    for line in after_title.lines() {
        if line == "---" {
            let sep_end = cursor + 3;
            let rest_start = if sep_end < after_title.len() {
                sep_end + 1 // skip \n after ---
            } else {
                after_title.len()
            };
            body_after_sep = &after_title[rest_start..];
            break;
        }
        // This is a metadata line — replace or keep
        let colon_pos = line.find(':').unwrap();
        let existing_key = &line[..colon_pos];
        if existing_key == key {
            meta_lines.push(format!("{}: {}", key, value));
            found = true;
        } else {
            meta_lines.push(line.to_string());
        }
        cursor += line.len() + 1;
    }

    if !found {
        meta_lines.push(format!("{}: {}", key, value));
    }

    format!("{}\n{}\n---\n{}", title_line, meta_lines.join("\n"), body_after_sep)
}

/// Return the body of a document with the metadata block removed.
///
/// If there is no metadata, returns everything after line 1.
/// If there is metadata, returns everything after the `---` separator.
pub fn remove_metadata(input: &str) -> &str {
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
/// This is a **parsing facade**, not a domain entity. It borrows from the
/// input string and should be constructed at the point of use, not stored
/// on long-lived types.
pub struct PlanDocument<'a> {
    raw: &'a str,
    title: &'a str,
    metadata: BTreeMap<String, String>,
    body: &'a str,
}

impl<'a> PlanDocument<'a> {
    /// Parse a description string into a `PlanDocument`.
    ///
    /// Calls `parse_metadata` once and stores the results. The title is
    /// always line 1 of the input (summary-first metadata format).
    pub fn parse(input: &'a str) -> Self {
        let title = input.lines().next().unwrap_or("");
        let (metadata, body) = parse_metadata(input);
        Self {
            raw: input,
            title,
            metadata,
            body,
        }
    }

    // -- Read accessors ----------------------------------------------------

    /// Line 1 of the input — the commit summary / plan title.
    pub fn title(&self) -> &str {
        self.title
    }

    /// Whether the metadata `status` field is `✅`.
    pub fn is_done(&self) -> bool {
        self.metadata.get("status").is_some_and(|v| v == "✅")
    }

    /// Full metadata key-value map.
    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.metadata
    }

    /// Body content after the `---` separator (or after line 1 if no metadata).
    pub fn body(&self) -> &str {
        self.body
    }

    /// The original unparsed input.
    pub fn raw(&self) -> &str {
        self.raw
    }

    // -- Transform methods -------------------------------------------------

    /// Body with `[scratch]` sections stripped.
    ///
    /// Computed on demand, not cached.
    pub fn body_sans_scratch(&self) -> String {
        strip_scratch_sections(self.body)
    }

    /// The complete "mark as done" transformation.
    ///
    /// 1. If `!keep_scratch`, strips `[scratch]` sections from the full document.
    /// 2. Sets metadata `status: ✅`.
    ///
    /// Idempotent: if status is already `✅`, still strips scratch (if requested)
    /// but doesn't double-stamp. Replaces `append_done_marker` entirely.
    pub fn as_done(&self, keep_scratch: bool) -> String {
        let base = if keep_scratch {
            self.raw.to_string()
        } else {
            strip_scratch_sections(self.raw)
        };
        // Migrate old ---/--- front matter if present
        let migrated = migrate_old_front_matter(&base);
        set_metadata_field(&migrated, "status", "✅")
    }

    /// Extract PR title and body for submission.
    ///
    /// Title is `self.title()` (line 1). Body is `self.body()` with
    /// `[scratch]` sections stripped and trimmed. Returns `None` if the
    /// title is empty.
    ///
    /// Replaces `plan_content_to_pr_parts` entirely.
    pub fn pr_parts(&self) -> Option<(String, String)> {
        let title = self.title();
        if title.trim().is_empty() {
            return None;
        }
        let body = strip_scratch_sections(self.body).trim().to_string();
        Some((title.to_string(), body))
    }
}

/// Migrate old `---`-delimited front matter to summary-first metadata format.
///
/// Old format:
/// ```text
/// ---
/// status: 🔴
/// ---
/// feat: my feature
/// ```
///
/// New format:
/// ```text
/// feat: my feature
/// status: 🔴
/// ---
/// ```
///
/// If the input doesn't start with `---\n`, returns it unchanged.
/// If the old front matter has no body (no real title after it), returns unchanged.
pub fn migrate_old_front_matter(input: &str) -> String {
    if !input.starts_with("---\n") {
        return input.to_string();
    }

    let after_open = &input[4..]; // skip "---\n"

    // Find closing "---" delimiter
    let (fm_end, body_start_in_after_open) = if after_open.starts_with("---\n") {
        (0, 4)
    } else if after_open == "---" {
        (0, 3)
    } else if let Some(pos) = after_open.find("\n---\n") {
        (pos, pos + 5)
    } else if after_open.ends_with("\n---") {
        let pos = after_open.len() - 4;
        (pos, after_open.len())
    } else {
        // No closing delimiter — not valid old front matter
        return input.to_string();
    };

    let fm_content = &after_open[..fm_end];
    let body_offset = 4 + body_start_in_after_open;
    let body = if body_offset <= input.len() {
        &input[body_offset..]
    } else {
        ""
    };

    // The "real" title is the first line of the body after old front matter
    let title = body.lines().next().unwrap_or("");
    if title.is_empty() {
        return input.to_string(); // no real title to migrate
    }

    // Body after the title line
    let body_after_title = if let Some(pos) = body.find('\n') {
        &body[pos + 1..]
    } else {
        ""
    };

    // Rebuild: title\nmetadata_lines\n---\nbody_after_title
    if fm_content.is_empty() {
        // Old front matter was empty (---\n---\n) — just drop it
        return body.to_string();
    }

    let mut result = String::with_capacity(input.len());
    result.push_str(title);
    result.push('\n');
    for line in fm_content.lines() {
        result.push_str(line);
        result.push('\n');
    }
    result.push_str("---\n");
    result.push_str(body_after_title);
    result
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

    // 0. Migrate old ---/--- front matter to summary-first format if needed.
    //    This ensures pulldown-cmark doesn't misparse `---` as a thematic break.
    let migrated;
    let input = if input.starts_with("---\n") {
        migrated = migrate_old_front_matter(input);
        &migrated
    } else {
        input
    };

    // 1. Extract metadata (if any). When metadata IS present, the metadata
    //    header (title + key:value lines + ---) is preserved verbatim and
    //    only the body is processed through pulldown-cmark. When there is
    //    NO metadata, the entire input is processed (the title line is just
    //    normal content that could be a [scratch] heading).
    let (meta_map, body) = parse_metadata(input);
    let has_metadata = !meta_map.is_empty();

    let (meta_raw, parseable) = if has_metadata {
        // Preserve metadata header, only scratch-strip the body
        let header = &input[..input.len() - body.len()];
        (header, body)
    } else {
        // No metadata — process the entire input
        ("", input)
    };

    if parseable.is_empty() {
        return input.to_string();
    }

    // 2. Parse body with pulldown-cmark to find heading sections
    //    We collect (heading_level, heading_text, section_start_byte) tuples
    struct HeadingInfo {
        level: u8,
        text: String,
        start: usize, // byte offset into `body`
    }

    let parser = Parser::new_ext(parseable, Options::all());
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
            let mut end = parseable.len();
            for (j, heading) in headings.iter().enumerate().skip(i + 1) {
                if heading.level <= scratch_level {
                    end = heading.start;
                    i = j; // continue scanning from this heading
                    break;
                }
            }
            if end == parseable.len() {
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

    // 4. Slice body around removal ranges, preserving original bytes
    let mut result = String::with_capacity(input.len());
    result.push_str(meta_raw);

    let mut cursor = 0;
    for range in &removal_ranges {
        if range.start > cursor {
            result.push_str(&parseable[cursor..range.start]);
        }
        cursor = range.end;
    }
    if cursor < parseable.len() {
        result.push_str(&parseable[cursor..]);
    }

    // 5. Handle edge case: if everything was stripped, return empty
    //    (but preserve metadata header if present)
    if result == meta_raw && !has_metadata {
        return String::new();
    }

    result
}

// ---------------------------------------------------------------------------
// Legacy helpers (used by strip_scratch_sections_legacy in tests)
// ---------------------------------------------------------------------------

/// Parse a code fence opener line. Returns `Some((char, count))` if it's a fence opener.
///
/// A fence opener starts with 3 or more backticks or tildes, optionally followed by an
/// info string (for backticks) or whitespace (for tildes).
#[cfg(test)]
fn parse_code_fence_opener(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return None;
    }

    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }

    let count = trimmed.chars().take_while(|&c| c == first).count();
    if count < 3 {
        return None;
    }

    // For backtick fences, the info string must not contain backticks
    if first == '`' {
        let rest = &trimmed[count..];
        if rest.contains('`') {
            return None;
        }
    }

    Some((first, count))
}

/// Check if a line closes a code fence opened with `fence_char` repeated `fence_count` times.
///
/// A closer has at least `fence_count` of the same character, with nothing else (except whitespace).
#[cfg(test)]
fn is_code_fence_closer(line: &str, fence_char: char, fence_count: usize) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return false;
    }

    let first = trimmed.chars().next().unwrap();
    if first != fence_char {
        return false;
    }

    let count = trimmed.chars().take_while(|&c| c == fence_char).count();
    if count < fence_count {
        return false;
    }

    // The rest of the line (after the fence chars) must be only whitespace
    trimmed[count..].trim().is_empty()
}

/// Parse a heading line and return its level (1–6), or None if it's not a heading.
///
/// An ATX heading starts with 1–6 `#` characters followed by at least one space
/// (or the line is just `#` characters, though that's non-standard).
#[cfg(test)]
fn parse_heading_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }

    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }

    // Must be followed by a space or be the entire line
    let rest = &trimmed[hashes..];
    if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
        Some(hashes)
    } else {
        None
    }
}

/// Check if a heading line contains `[scratch]` (case-insensitive).
#[cfg(test)]
fn heading_has_scratch(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("[scratch]")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Legacy implementation for comparison testing ──────────────────

    /// The original line-based scratch section stripper (ATX headings only).
    /// Kept for comparison testing during the pulldown-cmark transition.
    #[allow(dead_code)]
    fn strip_scratch_sections_legacy(input: &str) -> String {
        let has_trailing_newline = input.ends_with('\n');
        let lines: Vec<&str> = input.lines().collect();

        let mut kept: Vec<&str> = Vec::new();

        // Code fence tracking
        let mut in_code_fence = false;
        let mut fence_char: char = '`';
        let mut fence_count: usize = 0;

        // Scratch stripping state: Some(level) means we're stripping at that heading level
        let mut stripping: Option<usize> = None;

        for line in &lines {
            if in_code_fence {
                if is_code_fence_closer(line, fence_char, fence_count) {
                    in_code_fence = false;
                }
                if stripping.is_some() {
                    continue;
                }
                kept.push(line);
                continue;
            }

            if let Some((ch, count)) = parse_code_fence_opener(line) {
                in_code_fence = true;
                fence_char = ch;
                fence_count = count;
                if stripping.is_some() {
                    continue;
                }
                kept.push(line);
                continue;
            }

            if let Some(level) = parse_heading_level(line) {
                if let Some(strip_level) = stripping {
                    if level <= strip_level {
                        stripping = None;
                    } else {
                        continue;
                    }
                }

                if heading_has_scratch(line) {
                    stripping = Some(level);
                    continue;
                }

                kept.push(line);
                continue;
            }

            if stripping.is_some() {
                continue;
            }
            kept.push(line);
        }

        let mut result = kept.join("\n");
        if has_trailing_newline && !result.is_empty() {
            result.push('\n');
        }
        result
    }

    // ── Metadata parser tests ────────────────────────────────────────

    #[test]
    fn test_parse_metadata_basic() {
        let input = "feat: my feature\nstatus: 🔴\nissue: MERC-123\n---\nbody";
        let (map, body) = parse_metadata(input);
        assert_eq!(map.get("status").unwrap(), "🔴");
        assert_eq!(map.get("issue").unwrap(), "MERC-123");
        assert_eq!(body, "body");
    }

    #[test]
    fn test_parse_metadata_none_blank_line() {
        // Blank line after title → no metadata
        let input = "feat: title\n\n# Background\nSome content.";
        let (map, body) = parse_metadata(input);
        assert!(map.is_empty());
        assert_eq!(body, "\n# Background\nSome content.");
    }

    #[test]
    fn test_parse_metadata_none_heading() {
        // Heading after title → no metadata
        let input = "feat: title\n# Background\nSome content.";
        let (map, body) = parse_metadata(input);
        assert!(map.is_empty());
        assert_eq!(body, "# Background\nSome content.");
    }

    #[test]
    fn test_parse_metadata_no_body_after_separator() {
        let input = "feat: title\nstatus: ✅\n---\n";
        let (map, body) = parse_metadata(input);
        assert_eq!(map.get("status").unwrap(), "✅");
        assert_eq!(body, "");
    }

    #[test]
    fn test_parse_metadata_no_body_eof_after_separator() {
        let input = "feat: title\nstatus: ✅\n---";
        let (map, body) = parse_metadata(input);
        assert_eq!(map.get("status").unwrap(), "✅");
        assert!(body.is_empty(), "body should be empty, got: {:?}", body);
    }

    #[test]
    fn test_parse_metadata_single_line_input() {
        let input = "feat: title";
        let (map, body) = parse_metadata(input);
        assert!(map.is_empty());
        assert!(body.is_empty());
    }

    #[test]
    fn test_parse_metadata_no_separator_means_no_metadata() {
        // All lines look like metadata keys but no --- → no metadata
        let input = "feat: title\nstatus: 🔴\nissue: MERC-123";
        let (map, body) = parse_metadata(input);
        assert!(map.is_empty(), "No --- means no metadata block");
        assert_eq!(body, "status: 🔴\nissue: MERC-123");
    }

    #[test]
    fn test_parse_metadata_no_false_positive_from_prose() {
        // "Note:" starts with uppercase → not a metadata key
        let input = "feat: title\nNote: this has a colon\n---\nbody";
        let (map, body) = parse_metadata(input);
        assert!(map.is_empty(), "Uppercase key should not match metadata pattern");
        assert_eq!(body, "Note: this has a colon\n---\nbody");
    }

    #[test]
    fn test_parse_metadata_thematic_break_in_body() {
        // --- in body (after non-metadata content) is NOT a metadata delimiter
        let input = "feat: title\n\n# Background\n\nSome content.\n\n---\n\nMore content.";
        let (map, body) = parse_metadata(input);
        assert!(map.is_empty());
        // Body is everything after title line (blank line stops metadata scan)
        assert!(body.contains("---"), "thematic break should be in body");
        assert!(body.contains("More content."), "content after --- should be in body");
    }

    #[test]
    fn test_parse_metadata_blank_line_stops_scan() {
        // Blank line between title and would-be metadata → no metadata
        let input = "feat: title\n\nstatus: 🔴\n---\nbody";
        let (map, body) = parse_metadata(input);
        assert!(map.is_empty());
        assert_eq!(body, "\nstatus: 🔴\n---\nbody");
    }

    #[test]
    fn test_parse_metadata_key_with_hyphens_underscores() {
        let input = "feat: title\nmy-key: value1\nmy_key2: value2\n---\nbody";
        let (map, body) = parse_metadata(input);
        assert_eq!(map.get("my-key").unwrap(), "value1");
        assert_eq!(map.get("my_key2").unwrap(), "value2");
        assert_eq!(body, "body");
    }

    // ── set_metadata_field tests ─────────────────────────────────────

    #[test]
    fn test_set_metadata_field_new() {
        let input = "feat: my feature\n\n# Background\n";
        let result = set_metadata_field(input, "status", "🔴");
        assert_eq!(result, "feat: my feature\nstatus: 🔴\n---\n\n# Background\n");
    }

    #[test]
    fn test_set_metadata_field_new_single_line() {
        let input = "feat: my feature";
        let result = set_metadata_field(input, "status", "🔴");
        assert_eq!(result, "feat: my feature\nstatus: 🔴\n---\n");
    }

    #[test]
    fn test_set_metadata_field_replace() {
        let input = "feat: title\nstatus: 🔴\nissue: MERC-123\n---\nbody";
        let result = set_metadata_field(input, "status", "✅");
        assert!(result.contains("status: ✅"), "status should be replaced");
        assert!(result.contains("issue: MERC-123"), "other fields preserved");
        assert!(!result.contains("status: 🔴"), "old value should be gone");
        assert!(result.ends_with("body"), "body preserved");
        assert!(result.starts_with("feat: title\n"), "title preserved");
    }

    #[test]
    fn test_set_metadata_field_append() {
        let input = "feat: title\nstatus: 🔴\n---\nbody";
        let result = set_metadata_field(input, "issue", "MERC-456");
        assert!(result.contains("status: 🔴"), "existing field preserved");
        assert!(result.contains("issue: MERC-456"), "new field appended");
        assert!(result.ends_with("body"), "body preserved");
        assert!(result.starts_with("feat: title\n"), "title preserved");
    }

    #[test]
    fn test_set_metadata_field_preserves_body_byte_for_byte() {
        let body = "\n# Background\n\n  Indented text.\n\n- list item\n";
        let input = format!("feat: my feature\nstatus: 🔴\n---\n{}", body);
        let result = set_metadata_field(&input, "status", "✅");
        assert!(result.ends_with(body), "body must be preserved byte-for-byte");
    }

    // ── remove_metadata tests ────────────────────────────────────────

    #[test]
    fn test_remove_metadata() {
        let input = "feat: title\nstatus: 🔴\nissue: MERC-123\n---\nbody text here";
        let result = remove_metadata(input);
        assert_eq!(result, "body text here");
    }

    #[test]
    fn test_remove_metadata_no_metadata() {
        let input = "feat: title\n\nbody text";
        let result = remove_metadata(input);
        assert_eq!(result, "\nbody text");
    }

    #[test]
    fn test_remove_metadata_single_line() {
        let input = "feat: title";
        let result = remove_metadata(input);
        assert!(result.is_empty());
    }

    // ── Existing scratch stripping tests (must pass with new impl) ───

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
    fn test_scratch_strip_with_metadata() {
        let input = "\
feat: my feature
status: 🔴
issue: MERC-123
---

# Background

Some info.

# Notes [scratch]

Private notes here.

# Results

Final results.
";
        let result = strip_scratch_sections(input);
        assert!(result.contains("feat: my feature\nstatus: 🔴\nissue: MERC-123\n---\n"),
            "metadata header should be preserved");
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

    // ── Comparison: legacy vs new implementation ─────────────────────

    /// Run both implementations against the same ATX-heading inputs and verify
    /// they produce the same output. (Setext headings intentionally excluded
    /// since the legacy parser can't handle them.)
    #[test]
    fn test_legacy_vs_new_comparison() {
        let inputs = [
            "# Title\n\nSome content.\n\n## Section\n\nMore content.\n",
            "# Title\n\nSome intro.\n\n## Notes [scratch]\n\nThese are scratch notes.\nThey should be removed.\n\n## Real Section\n\nKeep this.\n",
            "# Title\n\nContent here.\n\n## Scratch Pad [scratch]\n\nThis is at the end.\nNo more headings follow.\n",
            "# Title\n\n## Section\n\n### Notes [scratch]\n\nScratch content.\n\n### Another Section\n\nKeep this.\n",
            "# Title\n\n```\n# This is not a heading\n## Neither is this\n### [scratch] — not a real heading\n```\n\n## Real section\n\nContent.\n",
            "# Title\n\n~~~\n# Fake heading\n## Also fake [scratch]\n~~~\n\n## Real section\n\nContent.\n",
            "# Title\n\n## Intro\n\nHello.\n\n## Notes [scratch]\n\nScratch 1.\n\n## Middle\n\nKeep this.\n\n## Draft [scratch]\n\nScratch 2.\n\n## Conclusion\n\nDone.\n",
            "## A [scratch]\n## B\n## C\n",
            "## Notes [Scratch]\n\nContent.\n\n## Next\n",
            "## Notes [SCRATCH]\n\nContent.\n\n## Next\n",
            "",
            "## Everything [scratch]\n\nAll content here.\n",
        ];

        for (i, input) in inputs.iter().enumerate() {
            let new_result = strip_scratch_sections(input);
            let legacy_result = strip_scratch_sections_legacy(input);
            assert_eq!(
                new_result, legacy_result,
                "Input #{i} differs between new and legacy implementation.\n\
                 Input: {:?}\n\
                 New:    {:?}\n\
                 Legacy: {:?}",
                input, new_result, legacy_result
            );
        }
    }

    // ── PlanDocument unit tests ──────────────────────────────────────

    #[test]
    fn test_plan_document_parse_with_metadata() {
        let input = "feat: my feature\nstatus: 🔴\nissue: MERC-123\n---\n\n# Background\n\nSome content.\n";
        let doc = PlanDocument::parse(input);
        assert_eq!(doc.title(), "feat: my feature");
        assert_eq!(doc.metadata().get("status").unwrap(), "🔴");
        assert_eq!(doc.metadata().get("issue").unwrap(), "MERC-123");
        assert!(!doc.is_done());
        assert_eq!(doc.body(), "\n# Background\n\nSome content.\n");
        assert_eq!(doc.raw(), input);
    }

    #[test]
    fn test_plan_document_parse_no_metadata() {
        let input = "feat: my feature\n\n# Background\n\nSome content.\n";
        let doc = PlanDocument::parse(input);
        assert_eq!(doc.title(), "feat: my feature");
        assert!(doc.metadata().is_empty());
        assert!(!doc.is_done());
        assert_eq!(doc.body(), "\n# Background\n\nSome content.\n");
    }

    #[test]
    fn test_plan_document_body_sans_scratch() {
        let input = "feat: title\nstatus: 🔴\n---\n\n# Background\n\nVisible.\n\n# Notes [scratch]\n\nHidden.\n\n# Results\n\nAlso visible.\n";
        let doc = PlanDocument::parse(input);
        let stripped = doc.body_sans_scratch();
        assert!(stripped.contains("# Background"), "non-scratch heading preserved");
        assert!(stripped.contains("Visible."), "non-scratch content preserved");
        assert!(stripped.contains("# Results"), "post-scratch heading preserved");
        assert!(stripped.contains("Also visible."), "post-scratch content preserved");
        assert!(!stripped.contains("[scratch]"), "scratch heading removed");
        assert!(!stripped.contains("Hidden."), "scratch content removed");
        // body() still has the scratch section
        assert!(doc.body().contains("[scratch]"), "body() is unstripped");
    }

    #[test]
    fn test_plan_document_empty_input() {
        let doc = PlanDocument::parse("");
        assert_eq!(doc.title(), "");
        assert!(!doc.is_done());
        assert!(doc.metadata().is_empty());
        assert!(doc.body().is_empty());
    }

    #[test]
    fn test_plan_document_as_done_sets_status() {
        let input = "feat: title\nstatus: 🔴\n---\n\n# Background\n\nContent.\n";
        let doc = PlanDocument::parse(input);
        let result = doc.as_done(false);
        assert!(result.contains("status: ✅"), "status should be ✅");
        assert!(!result.contains("status: 🔴"), "old status gone");
        assert!(result.starts_with("feat: title\n"), "title preserved");
        assert!(result.contains("# Background"), "body preserved");
    }

    #[test]
    fn test_plan_document_as_done_idempotent() {
        let input = "feat: title\nstatus: ✅\n---\n\n# Body\n";
        let doc = PlanDocument::parse(input);
        let result = doc.as_done(false);
        assert!(result.contains("status: ✅"));
        assert_eq!(result.matches("status:").count(), 1, "no duplicate status");
    }

    #[test]
    fn test_plan_document_as_done_strips_scratch() {
        let input = "feat: title\nstatus: 🔴\n---\n\n# Keep\n\nVisible.\n\n# Notes [scratch]\n\nHidden.\n";
        let doc = PlanDocument::parse(input);
        let result = doc.as_done(false);
        assert!(result.contains("status: ✅"), "status set");
        assert!(result.contains("# Keep"), "non-scratch preserved");
        assert!(!result.contains("[scratch]"), "scratch stripped");
        assert!(!result.contains("Hidden."), "scratch content stripped");
    }

    #[test]
    fn test_plan_document_as_done_keep_scratch() {
        let input = "feat: title\nstatus: 🔴\n---\n\n# Notes [scratch]\n\nHidden.\n";
        let doc = PlanDocument::parse(input);
        let result = doc.as_done(true);
        assert!(result.contains("status: ✅"), "status set");
        assert!(result.contains("[scratch]"), "scratch preserved with keep_scratch=true");
        assert!(result.contains("Hidden."), "scratch content preserved");
    }

    #[test]
    fn test_plan_document_as_done_creates_metadata() {
        let input = "feat: title\n\n# Background\n\nSome details.";
        let doc = PlanDocument::parse(input);
        let result = doc.as_done(false);
        assert!(result.starts_with("feat: title\n"), "title preserved as line 1");
        assert!(result.contains("status: ✅"), "status created");
        assert!(result.contains("---\n"), "separator created");
        assert!(result.contains("# Background"), "body preserved");
    }

    #[test]
    fn test_plan_document_pr_parts_basic() {
        let input = "feat: my feature\nstatus: 🔴\nissue: MERC-123\n---\n\n# Background\n\nVisible.\n\n# Notes [scratch]\n\nHidden.\n\n# Results\n\nFinal.\n";
        let doc = PlanDocument::parse(input);
        let (title, body) = doc.pr_parts().unwrap();
        assert_eq!(title, "feat: my feature");
        assert!(!body.contains("status:"), "no metadata in PR body");
        assert!(!body.contains("issue:"), "no metadata in PR body");
        assert!(!body.contains("[scratch]"), "no scratch in PR body");
        assert!(!body.contains("Hidden."), "no scratch content in PR body");
        assert!(body.contains("# Background"), "non-scratch content in PR body");
        assert!(body.contains("Final."), "non-scratch content in PR body");
    }

    #[test]
    fn test_plan_document_pr_parts_empty_title() {
        let doc = PlanDocument::parse("");
        assert!(doc.pr_parts().is_none());

        let doc2 = PlanDocument::parse("   \n\nbody");
        assert!(doc2.pr_parts().is_none());
    }

    #[test]
    fn test_plan_document_pr_parts_preserves_magic_words() {
        let input = "feat: title\nstatus: 🔴\n---\n\nCompletes MERC-123\n\n# Details\n\nWork done.\n";
        let doc = PlanDocument::parse(input);
        let (_, body) = doc.pr_parts().unwrap();
        assert!(body.contains("Completes MERC-123"), "Linear magic words survive");
    }

    #[test]
    fn test_plan_document_title_edge_cases() {
        // Single line, no body
        let doc = PlanDocument::parse("feat: title");
        assert_eq!(doc.title(), "feat: title");
        assert!(doc.body().is_empty());
        assert!(doc.metadata().is_empty());

        // Body starting with blank line
        let doc2 = PlanDocument::parse("feat: title\n\nsome body");
        assert_eq!(doc2.title(), "feat: title");
        assert_eq!(doc2.body(), "\nsome body");

        // Metadata with no body after separator
        let doc3 = PlanDocument::parse("feat: title\nstatus: 🔴\n---\n");
        assert_eq!(doc3.title(), "feat: title");
        assert_eq!(doc3.metadata().get("status").unwrap(), "🔴");
        assert_eq!(doc3.body(), "");

        // Body that is only whitespace
        let doc4 = PlanDocument::parse("feat: title\nstatus: 🔴\n---\n   \n  \n");
        assert_eq!(doc4.title(), "feat: title");
        assert!(doc4.pr_parts().unwrap().1.is_empty(), "whitespace-only body should trim to empty");
    }
}