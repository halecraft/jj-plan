//! Progress reporting for submit operations.

use crate::error::Result;
use async_trait::async_trait;

/// Phases of the submission process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Analyzing,
    Planning,
    Executing,
    AddingComments,
    Complete,
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Analyzing => write!(f, "Analyzing"),
            Self::Planning => write!(f, "Planning"),
            Self::Executing => write!(f, "Executing"),
            Self::AddingComments => write!(f, "Updating stack comments"),
            Self::Complete => write!(f, "Done"),
        }
    }
}

/// Status of a bookmark push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushStatus {
    Started,
    Success,
    AlreadySynced,
    Failed(String),
}

impl std::fmt::Display for PushStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Started => write!(f, "started"),
            Self::Success => write!(f, "success"),
            Self::AlreadySynced => write!(f, "already synced"),
            Self::Failed(msg) => write!(f, "failed: {msg}"),
        }
    }
}

/// Callback trait for progress updates during submission.
///
/// Implement this trait to receive progress updates during submission.
/// - CLI implementations can print to terminal
/// - Web servers can send SSE or WebSocket messages
#[async_trait]
pub trait ProgressCallback: Send + Sync {
    async fn on_phase(&self, phase: Phase) -> Result<()>;
    async fn on_bookmark_push(&self, bookmark: &str, status: PushStatus) -> Result<()>;
    async fn on_pr_created(&self, bookmark: &str, pr_number: u64, url: &str) -> Result<()>;
    async fn on_pr_updated(&self, bookmark: &str, pr_number: u64) -> Result<()>;
    async fn on_error(&self, message: &str) -> Result<()>;
    async fn on_message(&self, message: &str) -> Result<()>;
}

/// No-op progress callback for testing and dry-run.
pub struct NoopProgress;

#[async_trait]
impl ProgressCallback for NoopProgress {
    async fn on_phase(&self, _phase: Phase) -> Result<()> {
        Ok(())
    }
    async fn on_bookmark_push(&self, _bookmark: &str, _status: PushStatus) -> Result<()> {
        Ok(())
    }
    async fn on_pr_created(&self, _bookmark: &str, _pr_number: u64, _url: &str) -> Result<()> {
        Ok(())
    }
    async fn on_pr_updated(&self, _bookmark: &str, _pr_number: u64) -> Result<()> {
        Ok(())
    }
    async fn on_error(&self, _message: &str) -> Result<()> {
        Ok(())
    }
    async fn on_message(&self, _message: &str) -> Result<()> {
        Ok(())
    }
}