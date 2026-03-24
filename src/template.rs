use std::fs;
use std::path::Path;

/// Built-in default template for new plan changes.
///
/// Uses summary-first metadata format: the title/summary line comes first
/// (as jj/git expect), followed by metadata key-value lines, then `---`
/// separator. Developers who want sections (Background, Tasks, etc.)
/// should create a `.jj-plan/template.md` or set `JJ_PLAN_TEMPLATE`.
const DEFAULT_TEMPLATE: &str = "(plan: jj:{{CHANGE_ID}})\nstatus: 🔴\n---\n";

/// Resolve the template content using the standard fallback chain:
///
/// 1. `JJ_PLAN_TEMPLATE` env var → read file at that path
/// 2. `{plan_dir}/template.md` → read file if it exists
/// 3. Built-in default (embedded in binary as `DEFAULT_TEMPLATE`)
///
/// Returns the raw template string (before `{{CHANGE_ID}}` interpolation).
pub fn resolve_template(plan_dir: &Path) -> String {
    // 1. JJ_PLAN_TEMPLATE env var
    if let Ok(env_path) = std::env::var("JJ_PLAN_TEMPLATE")
        && !env_path.is_empty() {
            if let Ok(content) = fs::read_to_string(&env_path)
                && !content.is_empty() {
                    return content;
                }
            // If the env var points to a non-existent or empty file, warn
            // and fall through to the next source.
            eprintln!(
                "jj-plan: warning: JJ_PLAN_TEMPLATE={} could not be read, using fallback",
                env_path
            );
        }

    // 2. {plan_dir}/template.md
    let template_file = plan_dir.join("template.md");
    if let Ok(content) = fs::read_to_string(&template_file)
        && !content.is_empty() {
            return content;
        }

    // 3. Built-in default
    DEFAULT_TEMPLATE.to_string()
}

/// Apply a template by interpolating the change ID and optional bookmark name.
///
/// - Replaces all occurrences of `{{CHANGE_ID}}` with the actual change ID.
/// - Replaces all occurrences of `{{BOOKMARK}}` with the bookmark name (if provided).
/// - If no `{{CHANGE_ID}}` placeholder exists in the template, prepends a
///   self-referencing comment `<!-- jj:CHANGE_ID -->` as the second line.
pub fn apply_template_full(template: &str, change_id: &str, bookmark: Option<&str>) -> String {
    // First pass: replace {{BOOKMARK}} if provided
    let after_bookmark = if let Some(bm) = bookmark {
        template.replace("{{BOOKMARK}}", bm)
    } else {
        template.to_string()
    };

    // Second pass: replace {{CHANGE_ID}} or inject self-reference
    if after_bookmark.contains("{{CHANGE_ID}}") {
        after_bookmark.replace("{{CHANGE_ID}}", change_id)
    } else {
        // No placeholder found — inject a self-reference after the first line
        let comment = format!("<!-- jj:{} -->", change_id);
        match after_bookmark.find('\n') {
            Some(pos) => {
                let (first_line, rest) = after_bookmark.split_at(pos + 1);
                format!("{}{}\n{}", first_line, comment, rest)
            }
            None => {
                // Single-line template (no newline)
                format!("{}\n{}\n", after_bookmark, comment)
            }
        }
    }
}

/// Convenience: resolve the template and apply it with both change ID and
/// bookmark name. Returns the fully interpolated description string.
///
/// This is the preferred entry point for `jj plan new <bookmark>`, where
/// both `{{CHANGE_ID}}` and `{{BOOKMARK}}` should be interpolated.
pub fn render_template_with_bookmark(plan_dir: &Path, change_id: &str, bookmark: &str) -> String {
    let raw = resolve_template(plan_dir);
    apply_template_full(&raw, change_id, Some(bookmark))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ── resolve_template tests ────────────────────────────────────────

    #[test]
    fn test_resolve_default_template() {
        let tmp = tempfile::tempdir().unwrap();
        // No template.md, no env var → should return the built-in default
        let result = resolve_template(tmp.path());
        assert!(
            result.contains("{{CHANGE_ID}}"),
            "Default template should contain {{{{CHANGE_ID}}}} placeholder"
        );
        assert!(
            result.starts_with("(plan: jj:{{CHANGE_ID}})\n"),
            "Default template should start with the plan summary line"
        );
        assert!(
            result.contains("status: 🔴\n---\n"),
            "Default template should contain metadata block with separator"
        );
    }

    #[test]
    fn test_resolve_template_from_plan_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let custom = "Custom template for {{CHANGE_ID}}\n\n## My Section\n";
        fs::write(tmp.path().join("template.md"), custom).unwrap();

        let result = resolve_template(tmp.path());
        assert_eq!(result, custom);
    }

    #[test]
    fn test_resolve_template_empty_file_falls_through() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("template.md"), "").unwrap();

        let result = resolve_template(tmp.path());
        // Empty file should fall through to built-in default
        assert!(result.contains("{{CHANGE_ID}}"));
    }

    #[test]
    fn test_resolve_template_env_var_override() {
        let tmp = tempfile::tempdir().unwrap();

        // Create a custom template via env var
        let env_template_path = tmp.path().join("env_template.md");
        let env_content = "ENV template: {{CHANGE_ID}}\n";
        fs::write(&env_template_path, env_content).unwrap();

        // Also create a plan_dir template that should NOT be used
        let plan_dir = tmp.path().join("plan");
        fs::create_dir(&plan_dir).unwrap();
        fs::write(plan_dir.join("template.md"), "Plan dir template\n").unwrap();

        // Temporarily set the env var
        // Note: this is not thread-safe, but cargo test runs each test in
        // its own process by default with --test-threads=1 for env mutations.
        unsafe {
            std::env::set_var("JJ_PLAN_TEMPLATE", env_template_path.to_str().unwrap());
        }
        let result = resolve_template(&plan_dir);
        unsafe {
            std::env::remove_var("JJ_PLAN_TEMPLATE");
        }

        assert_eq!(result, env_content);
    }

    // ── apply_template tests ──────────────────────────────────────────

    #[test]
    fn test_apply_template_with_placeholder() {
        let template = "(plan: jj:{{CHANGE_ID}})\nstatus: 🔴\n---\n\n## Background\n";
        let result = apply_template_full(template, "abcdefgh", None);
        assert_eq!(result, "(plan: jj:abcdefgh)\nstatus: 🔴\n---\n\n## Background\n");
    }

    #[test]
    fn test_apply_template_multiple_placeholders() {
        let template = "Title: {{CHANGE_ID}}\n\nRef: jj:{{CHANGE_ID}}\n";
        let result = apply_template_full(template, "xyz12345", None);
        assert_eq!(result, "Title: xyz12345\n\nRef: jj:xyz12345\n");
    }

    #[test]
    fn test_apply_template_no_placeholder_injects_comment() {
        let template = "My custom title\n\n## Section\n\nContent.\n";
        let result = apply_template_full(template, "abcdefgh", None);
        assert_eq!(
            result,
            "My custom title\n<!-- jj:abcdefgh -->\n\n## Section\n\nContent.\n"
        );
    }

    #[test]
    fn test_apply_template_no_placeholder_single_line() {
        let template = "Just a title";
        let result = apply_template_full(template, "mychange", None);
        assert_eq!(result, "Just a title\n<!-- jj:mychange -->\n");
    }

    #[test]
    fn test_apply_template_no_placeholder_with_trailing_newline() {
        let template = "Title line\n";
        let result = apply_template_full(template, "testid", None);
        assert_eq!(result, "Title line\n<!-- jj:testid -->\n");
    }

    // ── {{BOOKMARK}} interpolation tests ──────────────────────────────

    #[test]
    fn test_apply_template_bookmark_placeholder() {
        let template = "(plan: {{BOOKMARK}} jj:{{CHANGE_ID}})\n";
        let result = apply_template_full(template, "abcdefgh", Some("feat-auth"));
        assert_eq!(result, "(plan: feat-auth jj:abcdefgh)\n");
    }

    #[test]
    fn test_apply_template_bookmark_multiple_occurrences() {
        let template = "# {{BOOKMARK}}\n\n(plan: {{BOOKMARK}} jj:{{CHANGE_ID}})\n";
        let result = apply_template_full(template, "xyz", Some("my-feature"));
        assert_eq!(result, "# my-feature\n\n(plan: my-feature jj:xyz)\n");
    }

    #[test]
    fn test_apply_template_bookmark_only_no_change_id() {
        let template = "# {{BOOKMARK}}\n\nSome content\n";
        let result = apply_template_full(template, "testid", Some("feat-x"));
        // No {{CHANGE_ID}} → injects comment
        assert_eq!(
            result,
            "# feat-x\n<!-- jj:testid -->\n\nSome content\n"
        );
    }

    #[test]
    fn test_render_template_with_bookmark() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("template.md"),
            "# {{BOOKMARK}}\n\n(plan: jj:{{CHANGE_ID}})\n",
        )
        .unwrap();

        let result = render_template_with_bookmark(tmp.path(), "abc123", "feat-auth");
        assert_eq!(result, "# feat-auth\n\n(plan: jj:abc123)\n");
    }

    // ── render_template_with_bookmark integration tests ───────────────

    #[test]
    fn test_render_template_default() {
        let tmp = tempfile::tempdir().unwrap();
        let result = render_template_with_bookmark(tmp.path(), "testid01", "feat-test");
        assert!(result.starts_with("(plan: jj:testid01)\n"), "should start with summary line");
        assert!(result.contains("status: 🔴\n---\n"), "should contain metadata block");
        assert!(!result.contains("{{CHANGE_ID}}"), "placeholder should be replaced");
    }

    #[test]
    fn test_render_template_custom() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("template.md"),
            "Custom: {{CHANGE_ID}}\n\n## Notes\n",
        )
        .unwrap();

        let result = render_template_with_bookmark(tmp.path(), "abc", "feat-test");
        assert_eq!(result, "Custom: abc\n\n## Notes\n");
    }

    #[test]
    fn test_render_template_custom_without_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("template.md"),
            "No placeholder here\n\n## Content\n",
        )
        .unwrap();

        let result = render_template_with_bookmark(tmp.path(), "myid", "feat-test");
        assert!(result.contains("<!-- jj:myid -->"));
        assert!(result.starts_with("No placeholder here\n"));
    }
}