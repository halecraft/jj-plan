/// Strip all `[scratch]`-annotated heading sections from a markdown document.
///
/// A scratch section starts with a heading line matching `^#{1,6}\s+.*\[scratch\]\s*$`
/// (case-insensitive match on `[scratch]`). The section includes all lines until
/// a heading of the same or higher level (≤ N) is encountered, or end of document.
///
/// Correctly handles fenced code blocks: lines inside ``` or ~~~ fences are NOT
/// treated as headings, even if they start with `#`.
pub fn strip_scratch_sections(input: &str) -> String {
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
            // Check if this line closes the fence
            if is_code_fence_closer(line, fence_char, fence_count) {
                in_code_fence = false;
            }
            // Lines inside code fences are never headings — just keep or skip
            if stripping.is_some() {
                continue;
            }
            kept.push(line);
            continue;
        }

        // Not in a code fence — check if this line opens one
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

        // Check if this line is a heading
        if let Some(level) = parse_heading_level(line) {
            // If we're stripping and this heading is at the same or higher level, stop stripping
            if let Some(strip_level) = stripping {
                if level <= strip_level {
                    stripping = None;
                    // Fall through to check if THIS heading is also [scratch]
                } else {
                    // Nested heading inside the scratch section — skip it
                    continue;
                }
            }

            // Check if this heading has [scratch]
            if heading_has_scratch(line) {
                stripping = Some(level);
                continue; // Don't keep this line
            }

            // Normal heading, keep it
            kept.push(line);
            continue;
        }

        // Regular line (not heading, not fence)
        if stripping.is_some() {
            continue;
        }
        kept.push(line);
    }

    let mut result = kept.join("\n");
    if has_trailing_newline && !result.is_empty() {
        result.push('\n');
    }
    // Special case: input is just "\n" or similar — lines() yields nothing, kept is empty
    // but input had a trailing newline. The empty string is correct here since there's
    // no content to preserve.
    result
}

/// Parse a code fence opener line. Returns `Some((char, count))` if it's a fence opener.
///
/// A fence opener starts with 3 or more backticks or tildes, optionally followed by an
/// info string (for backticks) or whitespace (for tildes).
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
fn heading_has_scratch(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("[scratch]")
}

#[cfg(test)]
mod tests {
    use super::*;

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
}