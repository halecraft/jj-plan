use std::borrow::Cow;

use crate::platform::error::PlatformApiError;

/// Errors that can occur during jj-plan operations.
#[derive(Debug, thiserror::Error)]
pub enum JjPlanError {
    // ── Core / shim errors ───────────────────────────────────────────

    #[error("jj-plan: cannot find real jj binary")]
    JjBinaryNotFound,

    #[error("jj-plan: failed to resolve self path: {0}")]
    SelfResolution(std::io::Error),

    #[error("jj-plan: failed to run jj: {0}")]
    JjExecFailed(std::io::Error),

    #[error("jj-plan: I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("jj plan: unknown subcommand '{0}'. Run 'jj plan --help' for usage.")]
    PlanUnknownSubcommand(String),

    // ── Git / write operations ───────────────────────────────────────

    #[error("jj-plan: git error: {0}")]
    Git(String),

    #[error("jj-plan: rebase failed: {0}")]
    RebaseFailed(String),

    // ── Authentication ───────────────────────────────────────────────

    #[error("jj-plan: authentication error: {0}")]
    Auth(String),

    // ── Platform / forge APIs ────────────────────────────────────────

    /// Structured failure from a forge HTTP API call (GitHub via octocrab,
    /// GitLab/Gitea via reqwest). Carries enough context for `hint()` to
    /// produce actionable guidance.
    #[error("jj-plan: {0}")]
    PlatformApi(#[from] PlatformApiError),

    /// Failure from shelling out to a forge CLI tool (e.g. `gh pr ready`
    /// fallback inside GitHub's `publish_pr`). Carries launch-vs-exit
    /// distinction so `hint()` can return install/workaround guidance for
    /// `NotInstalled` and stay quiet for `Failed` (where stderr speaks).
    #[error("{tool} {command}: {kind}")]
    ForgeCli {
        tool: &'static str,
        command: String,
        kind: ForgeCliFailure,
    },

    #[error("jj-plan: platform error: {0}")]
    Platform(String),

    #[error("jj-plan: no supported remotes (GitHub/GitLab/Gitea) found")]
    NoSupportedRemotes,

    #[error("jj-plan: remote not found: {0}")]
    RemoteNotFound(String),

    // ── Stack / bookmark resolution ──────────────────────────────────

    #[error("jj-plan: bookmark not found: {0}")]
    BookmarkNotFound(String),

    #[error("jj-plan: no stack found: {0}")]
    NoStack(String),

    // ── Configuration / CLI ──────────────────────────────────────────

    #[error("jj-plan: invalid configuration: {0}")]
    Config(String),

    #[error("jj-plan: parse error: {0}")]
    Parse(String),

    // ── Foreign error conversions ────────────────────────────────────

    #[error("jj-plan: HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("jj-plan: JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("jj-plan: URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),
}

/// Failure mode for a forge-CLI subprocess invocation. Distinguishes a launch
/// failure (binary not on PATH, permissions, etc.) from a non-zero exit, since
/// they need different user-facing hints.
#[derive(Debug)]
pub enum ForgeCliFailure {
    /// The binary couldn't be launched at all — typically "not installed."
    NotInstalled(std::io::Error),
    /// The binary ran but exited non-zero (or was terminated by a signal).
    /// `stderr` is captured (trimmed) for display.
    Failed {
        exit_code: Option<i32>,
        stderr: String,
    },
}

impl std::fmt::Display for ForgeCliFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled(e) => write!(f, "not installed ({e})"),
            Self::Failed {
                exit_code: Some(c),
                stderr,
            } => write!(f, "exited {c}: {stderr}"),
            Self::Failed {
                exit_code: None,
                stderr,
            } => write!(f, "terminated by signal: {stderr}"),
        }
    }
}

/// Walk an `Error::source()` chain and join messages with `": "`.
/// Replaces the inline walker in `workspace.rs` and is used by
/// `extract_github_error_fields` for octocrab errors that don't match
/// `Error::GitHub`.
pub fn flatten_error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        out = format!("{out}: {cause}");
        source = cause.source();
    }
    out
}

impl JjPlanError {
    /// Return an actionable hint for this error, if one is available.
    ///
    /// Single entry point for hint extraction — rendering sites can call
    /// `err.hint()` uniformly without first matching on the variant.
    /// Dispatches to:
    ///   - `PlatformApiError::hint()` for the `PlatformApi` variant.
    ///   - A connectivity hint for `Http` when `reqwest::Error::is_connect()`
    ///     or `is_timeout()` (excludes body-decode failures).
    ///   - An install/workaround hint for `ForgeCli` with `NotInstalled` kind.
    pub fn hint(&self) -> Option<Cow<'static, str>> {
        match self {
            Self::PlatformApi(e) => e.hint(),
            Self::Http(e) if e.is_connect() || e.is_timeout() => Some(Cow::Borrowed(
                "Network error — check your internet connection and proxy/VPN settings.",
            )),
            Self::ForgeCli {
                tool,
                kind: ForgeCliFailure::NotInstalled(_),
                ..
            } => Some(Cow::Borrowed(match *tool {
                "gh" => {
                    "Install GitHub CLI from https://cli.github.com/, or grant your token GraphQL access so the native API path is used instead."
                }
                "glab" => "Install GitLab CLI from https://gitlab.com/gitlab-org/cli.",
                _ => "Install the forge CLI used by this fallback path.",
            })),
            _ => None,
        }
    }
}

/// Result type alias for jj-plan operations.
pub type Result<T> = std::result::Result<T, JjPlanError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::error::{Operation, build_platform_api_error};
    use crate::types::Platform;

    // --- flatten_error_chain --------------------------------------------

    #[derive(Debug)]
    struct ChainErr {
        msg: &'static str,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    }
    impl std::fmt::Display for ChainErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.msg)
        }
    }
    impl std::error::Error for ChainErr {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source.as_deref().map(|s| s as _)
        }
    }

    #[test]
    fn flatten_chain_single_error() {
        let e = ChainErr { msg: "outer", source: None };
        assert_eq!(flatten_error_chain(&e), "outer");
    }

    #[test]
    fn flatten_chain_nested() {
        let inner = ChainErr { msg: "inner", source: None };
        let middle = ChainErr {
            msg: "middle",
            source: Some(Box::new(inner)),
        };
        let outer = ChainErr {
            msg: "outer",
            source: Some(Box::new(middle)),
        };
        assert_eq!(flatten_error_chain(&outer), "outer: middle: inner");
    }

    // --- ForgeCliFailure / ForgeCli display ------------------------------

    #[test]
    fn forge_cli_failure_display_not_installed() {
        let f = ForgeCliFailure::NotInstalled(std::io::Error::from(std::io::ErrorKind::NotFound));
        let s = f.to_string();
        assert!(s.starts_with("not installed ("));
    }

    #[test]
    fn forge_cli_failure_display_exited() {
        let f = ForgeCliFailure::Failed {
            exit_code: Some(1),
            stderr: "auth required".to_string(),
        };
        assert_eq!(f.to_string(), "exited 1: auth required");
    }

    #[test]
    fn forge_cli_failure_display_signalled() {
        let f = ForgeCliFailure::Failed {
            exit_code: None,
            stderr: "killed".to_string(),
        };
        assert_eq!(f.to_string(), "terminated by signal: killed");
    }

    #[test]
    fn forge_cli_outer_display_composes() {
        let err = JjPlanError::ForgeCli {
            tool: "gh",
            command: "pr ready 42 --repo o/r".to_string(),
            kind: ForgeCliFailure::Failed {
                exit_code: Some(1),
                stderr: "boom".to_string(),
            },
        };
        let s = err.to_string();
        assert!(s.contains("gh pr ready 42 --repo o/r:"));
        assert!(s.contains("exited 1: boom"));
    }

    // --- JjPlanError::hint() --------------------------------------------

    #[test]
    fn hint_platform_api_delegates() {
        let pae = build_platform_api_error(
            Platform::GitHub,
            Operation::TestAuth,
            None,
            Some(401),
            String::new(),
            None,
        );
        let err = JjPlanError::PlatformApi(pae);
        let h = err.hint().unwrap();
        assert!(h.contains("gh auth login"));
    }

    #[test]
    fn hint_forge_cli_not_installed_for_gh() {
        let err = JjPlanError::ForgeCli {
            tool: "gh",
            command: "pr ready 42 --repo o/r".to_string(),
            kind: ForgeCliFailure::NotInstalled(std::io::Error::from(
                std::io::ErrorKind::NotFound,
            )),
        };
        let h = err.hint().unwrap();
        assert!(h.contains("GitHub CLI"));
        assert!(h.contains("GraphQL"));
    }

    #[test]
    fn hint_forge_cli_failed_returns_none() {
        let err = JjPlanError::ForgeCli {
            tool: "gh",
            command: "pr ready 42 --repo o/r".to_string(),
            kind: ForgeCliFailure::Failed {
                exit_code: Some(1),
                stderr: "auth required".to_string(),
            },
        };
        assert!(err.hint().is_none());
    }

    #[test]
    fn hint_auth_config_git_return_none() {
        assert!(JjPlanError::Auth("x".into()).hint().is_none());
        assert!(JjPlanError::Config("x".into()).hint().is_none());
        assert!(JjPlanError::Git("x".into()).hint().is_none());
        assert!(JjPlanError::Parse("x".into()).hint().is_none());
    }
}
