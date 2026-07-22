use crate::domain::RunStatus;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{code}")]
pub struct WorkflowError {
    code: String,
}

impl WorkflowError {
    pub fn code(&self) -> &str {
        &self.code
    }
}

impl From<String> for WorkflowError {
    fn from(code: String) -> Self {
        Self { code }
    }
}

impl From<&str> for WorkflowError {
    fn from(code: &str) -> Self {
        Self { code: code.into() }
    }
}

pub type WorkflowResult<T> = Result<T, WorkflowError>;

impl RunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Partial => "partial",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationAction {
    Move,
    CopyDelete,
    CopySourceRetained,
    Skip,
    DryRun,
}

impl OperationAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::CopyDelete => "copy_delete",
            Self::CopySourceRetained => "copy_source_retained",
            Self::Skip => "skip",
            Self::DryRun => "dry_run",
        }
    }

    pub fn from_code(value: &str) -> Option<Self> {
        match value {
            "move" => Some(Self::Move),
            "copy_delete" => Some(Self::CopyDelete),
            "copy_source_retained" | "copy" => Some(Self::CopySourceRetained),
            "skip" => Some(Self::Skip),
            "dry_run" => Some(Self::DryRun),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationResult {
    Success,
    Skipped,
    Failed,
}

impl OperationResult {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}
