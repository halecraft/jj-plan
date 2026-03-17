use std::path::PathBuf;

/// Errors that can occur during jj-plan operations.
#[derive(Debug, thiserror::Error)]
pub enum JjPlanError {
    #[error("jj-plan: cannot find real jj binary")]
    JjBinaryNotFound,

    #[error("jj-plan: failed to resolve self path: {0}")]
    SelfResolution(std::io::Error),

    #[error("jj-plan: failed to resolve path {path}: {source}")]
    PathResolution {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("jj-plan: failed to run jj: {0}")]
    JjExecFailed(std::io::Error),

    #[error("jj-plan: jj command failed with exit code {code}: {stderr}")]
    JjCommandFailed { code: i32, stderr: String },

    #[error("jj-plan: not in a jj repo")]
    NotInRepo,

    #[error("jj-plan: I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("jj plan: missing subcommand. Run 'jj plan --help' for usage.")]
    PlanMissingSubcommand,

    #[error("jj plan: unknown subcommand '{0}'. Run 'jj plan --help' for usage.")]
    PlanUnknownSubcommand(String),
}

/// Result type alias for jj-plan operations.
pub type Result<T> = std::result::Result<T, JjPlanError>;