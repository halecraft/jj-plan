use std::fs;
use std::path::Path;

/// Built-in default template for new plan changes.
///
/// Intentionally minimal: just the self-referencing summary line. The binary
/// does not impose any plan structure — developers who want sections
/// (Background, Tasks, etc.) should create a `.jj-plan/template.md` or set
/// `JJ_PLAN_TEMPLATE`.
const DEFAULT_TEMPLATE: &str = "(plan: jj:{{CHANGE_ID}})\n";

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

/// Apply a template by interpolating the change ID.
///
/// - Replaces all occurrences of `{{CHANGE_ID}}` with the actual change ID.
/// - If no `{{CHANGE_ID}}` placeholder exists in the template, prepends a
///   self-referencing comment `<!-- jj:CHANGE_ID -->` as the second line
///   (after the title line) so the change always has a self-reference.
pub fn apply_template(template: &str, change_id: &str) -> String {
    if template.contains("{{CHANGE_ID}}") {
        template.replace("{{CHANGE_ID}}", change_id)
    } else {
        // No placeholder found — inject a self-reference after the first line
        let comment = format!("<!-- jj:{} -->", change_id);
        match template.find('\n') {
            Some(pos) => {
                let (first_line, rest) = template.split_at(pos + 1);
                format!("{}{}\n{}", first_line, comment, rest)
            }
            None => {
                // Single-line template (no newline)
                format!("{}\n{}\n", template, comment)
            }
        }
    }
}

/// Convenience: resolve the template for a plan directory and apply it with
/// the given change ID. Returns the fully interpolated description string.
pub fn render_template(plan_dir: &Path, change_id: &str) -> String {
    let raw = resolve_template(plan_dir);
    apply_template(&raw, change_id)
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
            result.starts_with("(plan: jj:{{CHANGE_ID}})"),
            "Default template should start with the plan summary line"
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
        let template = "(plan: jj:{{CHANGE_ID}})\n\n## Background\n";
        let result = apply_template(template, "abcdefgh");
        assert_eq!(result, "(plan: jj:abcdefgh)\n\n## Background\n");
    }

    #[test]
    fn test_apply_template_multiple_placeholders() {
        let template = "Title: {{CHANGE_ID}}\n\nRef: jj:{{CHANGE_ID}}\n";
        let result = apply_template(template, "xyz12345");
        assert_eq!(result, "Title: xyz12345\n\nRef: jj:xyz12345\n");
    }

    #[test]
    fn test_apply_template_no_placeholder_injects_comment() {
        let template = "My custom title\n\n## Section\n\nContent.\n";
        let result = apply_template(template, "abcdefgh");
        assert_eq!(
            result,
            "My custom title\n<!-- jj:abcdefgh -->\n\n## Section\n\nContent.\n"
        );
    }

    #[test]
    fn test_apply_template_no_placeholder_single_line() {
        let template = "Just a title";
        let result = apply_template(template, "mychange");
        assert_eq!(result, "Just a title\n<!-- jj:mychange -->\n");
    }

    #[test]
    fn test_apply_template_no_placeholder_with_trailing_newline() {
        let template = "Title line\n";
        let result = apply_template(template, "testid");
        assert_eq!(result, "Title line\n<!-- jj:testid -->\n");
    }

    // ── render_template integration tests ─────────────────────────────

    #[test]
    fn test_render_template_default() {
        let tmp = tempfile::tempdir().unwrap();
        let result = render_template(tmp.path(), "testid01");
        assert!(result.starts_with("(plan: jj:testid01)"));
        assert!(!result.contains("{{CHANGE_ID}}"));
    }

    #[test]
    fn test_render_template_custom() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("template.md"),
            "Custom: {{CHANGE_ID}}\n\n## Notes\n",
        )
        .unwrap();

        let result = render_template(tmp.path(), "abc");
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

        let result = render_template(tmp.path(), "myid");
        assert!(result.contains("<!-- jj:myid -->"));
        assert!(result.starts_with("No placeholder here\n"));
    }
}