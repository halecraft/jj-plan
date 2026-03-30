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

    #[error("jj-plan: GitHub API error: {0}")]
    GitHubApi(String),

    #[error("jj-plan: GitLab API error: {0}")]
    GitLabApi(String),

    #[error("jj-plan: Gitea API error: {0}")]
    GiteaApi(String),

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

    #[error("jj-plan: GitHub client error: {0}")]
    Octocrab(#[from] octocrab::Error),
}

/// Result type alias for jj-plan operations.
pub type Result<T> = std::result::Result<T, JjPlanError>;