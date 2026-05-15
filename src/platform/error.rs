//! Structured error type for forge API failures + shared response helpers.
//!
//! Three layers:
//! 1. `Operation` enum — names the high-level API call (CreatePr, MergePr, ...)
//!    in a single source of truth used by both error construction and `Display`.
//! 2. `PlatformApiError` struct — carries platform, operation, HTTP status,
//!    server message, optional detail, and an optional target identifier
//!    (bookmark name, `#42`, etc.) used by `hint()` to produce contextual
//!    guidance.
//! 3. `classify_response` (pure) + `checked_response` / `checked_status`
//!    (async shells) — the GitLab/Gitea conversion pipeline that reads a
//!    response body once, classifies it, and either deserializes or returns
//!    `()` on success.
//!
//! GitHub does not use `checked_response` — it routes through `octocrab_err`
//! in `src/platform/github.rs`, which calls `build_platform_api_error`
//! (re-exported here as the pure constructor).

use std::borrow::Cow;

use serde::de::DeserializeOwned;

use crate::error::{JjPlanError, Result};
use crate::types::Platform;

/// High-level forge API operation. Used both as the error's `operation` field
/// and as a `Display` label in user-facing messages. The mapping from
/// `ExecutionStep` variants is intentionally not bijective (`Push` has no
/// `Operation`; `AddStackComment` is bivalent depending on whether an existing
/// comment id is present), so each call site names its `Operation` directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    FindExistingPr,
    CreatePr,
    UpdateBase,
    UpdateDescription,
    PublishPr,
    ListComments,
    CreateComment,
    UpdateComment,
    GetPrDetails,
    CheckMergeReadiness,
    MergePr,
    /// Token-validation API call from `auth::test_*_auth`. Surfaced separately
    /// from `GetPrDetails` so that auth probes get the platform-specific
    /// auth-command hint via the 401 row in `hint()`.
    TestAuth,
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::FindExistingPr => "FindExistingPr",
            Self::CreatePr => "CreatePr",
            Self::UpdateBase => "UpdateBase",
            Self::UpdateDescription => "UpdateDescription",
            Self::PublishPr => "PublishPr",
            Self::ListComments => "ListComments",
            Self::CreateComment => "CreateComment",
            Self::UpdateComment => "UpdateComment",
            Self::GetPrDetails => "GetPrDetails",
            Self::CheckMergeReadiness => "CheckMergeReadiness",
            Self::MergePr => "MergePr",
            Self::TestAuth => "TestAuth",
        };
        f.write_str(s)
    }
}

/// Structured error from a forge API call.
///
/// Captures enough context for both diagnostic display and actionable user
/// guidance. Constructed by per-platform helpers (`octocrab_err` for GitHub;
/// `checked_response`/`checked_status` for GitLab/Gitea) — never by callers
/// outside `src/platform/` and `src/auth/`.
#[derive(Debug, Clone)]
pub struct PlatformApiError {
    pub platform: Platform,
    pub operation: Operation,
    /// HTTP status when available. `None` means the call never produced an
    /// HTTP response (octocrab `Hyper`/`Service`/`Json` variants etc.). The
    /// hint table does not key on `None` — connectivity hints live on
    /// `JjPlanError::Http`.
    pub status: Option<u16>,
    /// The server's error message (or for status=None GitHub errors, the
    /// flattened source chain).
    pub message: String,
    /// Optional auxiliary detail: GitHub field errors, GitLab/Gitea
    /// `{"error": ...}` secondary field, doc URLs, etc.
    pub detail: Option<String>,
    /// The identifier the operation acted on — bookmark name, `#42`, or host.
    /// Used by `hint()` to parameterise guidance.
    pub target: Option<String>,
}

impl std::fmt::Display for PlatformApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(s) => write!(f, "{} {} failed ({}): {}", self.platform, self.operation, s, self.message)?,
            None => write!(f, "{} {} failed: {}", self.platform, self.operation, self.message)?,
        }
        if let Some(detail) = &self.detail {
            write!(f, "\n{detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for PlatformApiError {}

impl PlatformApiError {
    /// Map `(status, operation)` to a human-actionable hint. Returns `Cow` so
    /// static hints don't allocate; only parameterised hints (those
    /// interpolating `target`) build a `String`.
    ///
    /// There is **no `status: None` row** — connectivity hints live on
    /// `JjPlanError::Http` (gated on `is_connect`/`is_timeout`).
    pub fn hint(&self) -> Option<Cow<'static, str>> {
        let status = self.status?;
        match (status, self.operation) {
            (401, _) => Some(Cow::Borrowed(match self.platform {
                Platform::GitHub => {
                    "Authentication failed — your token is missing or invalid. Run: gh auth login (or set GITHUB_TOKEN)."
                }
                Platform::GitLab => {
                    "Authentication failed — your token is missing or invalid. Run: glab auth login (or set GITLAB_TOKEN)."
                }
                Platform::Gitea => {
                    "Authentication failed — your token is missing or invalid. Set GITEA_TOKEN (and GITEA_HOST if self-hosted)."
                }
            })),
            (403, _) => Some(Cow::Borrowed(
                "Permission denied — your token may lack required scopes. For GitHub, try: gh auth refresh -s repo",
            )),
            (404, _) => Some(Cow::Borrowed(
                "Resource not found — the repository or branch may not exist on the remote. Check: jj git remote list",
            )),
            (422, Operation::CreatePr) => match &self.target {
                Some(t) => Some(Cow::Owned(format!(
                    "PR creation failed validation. The branch may have no new commits relative to its base, or a PR may already exist. Inspect with: jj log -r 'trunk()..{t}'"
                ))),
                None => Some(Cow::Borrowed(
                    "PR creation failed validation. The branch may have no new commits relative to its base, or a PR may already exist.",
                )),
            },
            _ => None,
        }
    }
}

/// Pure constructor for `PlatformApiError`. Trivial — exists so that GitHub's
/// `octocrab_err` adapter can be split into a pure builder + thin extractor,
/// keeping the bulk of the logic testable without going through snafu's
/// generated context selectors.
pub fn build_platform_api_error(
    platform: Platform,
    operation: Operation,
    target: Option<String>,
    status: Option<u16>,
    message: String,
    detail: Option<String>,
) -> PlatformApiError {
    PlatformApiError {
        platform,
        operation,
        status,
        message,
        detail,
        target,
    }
}

/// Classify a `(status, body)` pair into either a `PlatformApiError` (on
/// 4xx/5xx) or `None` (on 2xx). Pure — testable with literal arguments.
///
/// Body parsing is best-effort: GitLab returns `{"message": "...", "error": "..."}`
/// (rarely `{"message": ["...", ...]}`); Gitea returns `{"message": "..."}`.
/// Falls back to the raw body string when JSON parsing fails or the expected
/// fields aren't present.
pub fn classify_response(
    status: u16,
    body: &str,
    platform: Platform,
    operation: Operation,
    target: Option<String>,
) -> Option<PlatformApiError> {
    if (200..300).contains(&status) {
        return None;
    }

    let (message, detail) = extract_message_and_detail(body);
    Some(build_platform_api_error(
        platform,
        operation,
        target,
        Some(status),
        message,
        detail,
    ))
}

fn extract_message_and_detail(body: &str) -> (String, Option<String>) {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return ("(no response body)".to_string(), None);
    }

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(obj) = v.as_object()
    {
        let message = match obj.get("message") {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Array(arr)) => Some(
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Some(other) => Some(other.to_string()),
            None => None,
        };
        let detail = match obj.get("error") {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(other) if !other.is_null() => Some(other.to_string()),
            _ => None,
        };

        if let Some(msg) = message {
            return (msg, detail);
        }
    }

    (trimmed.to_string(), None)
}

/// Async shell: read body once, classify, and deserialize JSON on success.
/// Used for endpoints that return a JSON body we consume.
pub async fn checked_response<T: DeserializeOwned>(
    response: reqwest::Response,
    platform: Platform,
    operation: Operation,
    target: Option<String>,
) -> Result<T> {
    let status = response.status().as_u16();
    let body = response.text().await?;

    if let Some(err) = classify_response(status, &body, platform, operation, target) {
        return Err(JjPlanError::PlatformApi(err));
    }

    serde_json::from_str(&body).map_err(JjPlanError::Json)
}

/// Async shell variant: read body once, classify, return `()` on success.
/// Used for endpoints whose response body we don't consume (comment
/// create/update on GitLab+Gitea; Gitea `merge_pr` which re-fetches via
/// `get_pr`).
pub async fn checked_status(
    response: reqwest::Response,
    platform: Platform,
    operation: Operation,
    target: Option<String>,
) -> Result<()> {
    let status = response.status().as_u16();
    let body = response.text().await?;

    if let Some(err) = classify_response(status, &body, platform, operation, target) {
        return Err(JjPlanError::PlatformApi(err));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_response_2xx_returns_none() {
        assert!(
            classify_response(
                200,
                r#"{"ok": true}"#,
                Platform::GitLab,
                Operation::CreatePr,
                None,
            )
            .is_none()
        );
        assert!(
            classify_response(
                201,
                "",
                Platform::Gitea,
                Operation::CreatePr,
                Some("feat/x".into()),
            )
            .is_none()
        );
    }

    #[test]
    fn classify_response_gitlab_style_body() {
        let err = classify_response(
            422,
            r#"{"message":"Validation failed","error":"head missing"}"#,
            Platform::GitLab,
            Operation::CreatePr,
            Some("feat/x".to_string()),
        )
        .unwrap();
        assert_eq!(err.status, Some(422));
        assert_eq!(err.message, "Validation failed");
        assert_eq!(err.detail.as_deref(), Some("head missing"));
        assert_eq!(err.target.as_deref(), Some("feat/x"));
    }

    #[test]
    fn classify_response_gitea_style_body() {
        let err = classify_response(
            422,
            r#"{"message":"branch does not exist"}"#,
            Platform::Gitea,
            Operation::CreatePr,
            None,
        )
        .unwrap();
        assert_eq!(err.status, Some(422));
        assert_eq!(err.message, "branch does not exist");
        assert!(err.detail.is_none());
    }

    #[test]
    fn classify_response_malformed_body_falls_back_to_raw() {
        let err = classify_response(
            500,
            "internal server error (plain text)",
            Platform::GitLab,
            Operation::MergePr,
            Some("#42".into()),
        )
        .unwrap();
        assert_eq!(err.status, Some(500));
        assert_eq!(err.message, "internal server error (plain text)");
        assert!(err.detail.is_none());
    }

    #[test]
    fn classify_response_empty_body() {
        let err = classify_response(
            500,
            "",
            Platform::Gitea,
            Operation::MergePr,
            None,
        )
        .unwrap();
        assert_eq!(err.message, "(no response body)");
    }

    #[test]
    fn classify_response_array_message() {
        let err = classify_response(
            422,
            r#"{"message":["a","b"]}"#,
            Platform::GitLab,
            Operation::CreatePr,
            None,
        )
        .unwrap();
        assert_eq!(err.message, "a, b");
    }

    #[test]
    fn build_platform_api_error_constructor() {
        let err = build_platform_api_error(
            Platform::GitHub,
            Operation::CreatePr,
            Some("feat/x".to_string()),
            Some(422),
            "validation".to_string(),
            Some("head missing".to_string()),
        );
        assert_eq!(err.platform, Platform::GitHub);
        assert_eq!(err.operation, Operation::CreatePr);
        assert_eq!(err.status, Some(422));
        assert_eq!(err.message, "validation");
        assert_eq!(err.detail.as_deref(), Some("head missing"));
        assert_eq!(err.target.as_deref(), Some("feat/x"));
    }

    #[test]
    fn display_includes_platform_operation_status_and_message() {
        let err = build_platform_api_error(
            Platform::GitLab,
            Operation::CreatePr,
            None,
            Some(422),
            "Validation failed".to_string(),
            Some("head missing".to_string()),
        );
        let s = err.to_string();
        assert!(s.contains("GitLab"));
        assert!(s.contains("CreatePr"));
        assert!(s.contains("422"));
        assert!(s.contains("Validation failed"));
        assert!(s.contains("head missing"));
    }

    #[test]
    fn display_without_status() {
        let err = build_platform_api_error(
            Platform::GitHub,
            Operation::GetPrDetails,
            None,
            None,
            "Network error: ...".to_string(),
            None,
        );
        let s = err.to_string();
        assert!(s.contains("GitHub GetPrDetails failed:"));
        assert!(!s.contains("()"));
    }

    #[test]
    fn hint_401_is_platform_specific_and_borrowed() {
        let github = build_platform_api_error(
            Platform::GitHub,
            Operation::TestAuth,
            None,
            Some(401),
            String::new(),
            None,
        );
        let h = github.hint().unwrap();
        assert!(matches!(h, Cow::Borrowed(_)));
        assert!(h.contains("gh auth login"));

        let gitlab = build_platform_api_error(
            Platform::GitLab,
            Operation::TestAuth,
            None,
            Some(401),
            String::new(),
            None,
        );
        assert!(gitlab.hint().unwrap().contains("glab auth login"));

        let gitea = build_platform_api_error(
            Platform::Gitea,
            Operation::TestAuth,
            None,
            Some(401),
            String::new(),
            None,
        );
        assert!(gitea.hint().unwrap().contains("GITEA_TOKEN"));
    }

    #[test]
    fn hint_403_404_are_borrowed_and_generic() {
        let e = build_platform_api_error(
            Platform::GitHub,
            Operation::CreatePr,
            None,
            Some(403),
            String::new(),
            None,
        );
        assert!(matches!(e.hint().unwrap(), Cow::Borrowed(_)));

        let e = build_platform_api_error(
            Platform::GitHub,
            Operation::GetPrDetails,
            None,
            Some(404),
            String::new(),
            None,
        );
        assert!(matches!(e.hint().unwrap(), Cow::Borrowed(_)));
    }

    #[test]
    fn hint_422_create_pr_with_target_interpolates() {
        let e = build_platform_api_error(
            Platform::GitHub,
            Operation::CreatePr,
            Some("feat/x".to_string()),
            Some(422),
            String::new(),
            None,
        );
        let h = e.hint().unwrap();
        assert!(matches!(h, Cow::Owned(_)));
        assert!(h.contains("feat/x"));
        assert!(h.contains("jj log -r"));
    }

    #[test]
    fn hint_422_create_pr_without_target_is_borrowed() {
        let e = build_platform_api_error(
            Platform::GitHub,
            Operation::CreatePr,
            None,
            Some(422),
            String::new(),
            None,
        );
        let h = e.hint().unwrap();
        assert!(matches!(h, Cow::Borrowed(_)));
    }

    #[test]
    fn hint_unrecognized_combinations_return_none() {
        let e = build_platform_api_error(
            Platform::GitHub,
            Operation::MergePr,
            None,
            Some(422),
            String::new(),
            None,
        );
        assert!(e.hint().is_none());

        let e = build_platform_api_error(
            Platform::GitHub,
            Operation::CreatePr,
            None,
            Some(500),
            String::new(),
            None,
        );
        assert!(e.hint().is_none());
    }

    #[test]
    fn hint_status_none_returns_none() {
        let e = build_platform_api_error(
            Platform::GitHub,
            Operation::CreatePr,
            None,
            None,
            String::new(),
            None,
        );
        assert!(e.hint().is_none());
    }
}
