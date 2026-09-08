use crate::domain::RunStatus;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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
            Self::RecoveryRequired => "recovery_required",
            Self::Archived => "archived",
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

/// The durable state of one filesystem mutation.  A state is persisted before
/// the following filesystem step is attempted; terminal audit logs are a
/// projection of this journal rather than the only recovery source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalState {
    Prepared,
    Staging,
    Staged,
    ContentVerified,
    Published,
    SourceDeleted,
    Completed,
    Failed,
    RecoveryRequired,
}

impl JournalState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Staging => "staging",
            Self::Staged => "staged",
            Self::ContentVerified => "content_verified",
            Self::Published => "published",
            Self::SourceDeleted => "source_deleted",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::RecoveryRequired => "recovery_required",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationDirection {
    Apply,
    Rollback,
    Recovery,
}

/// Filesystem protocol selected before an operation journal is prepared.
///
/// The value is part of the durable recovery contract: recovery must never
/// guess whether staging is meaningful merely from which paths happen to
/// exist after a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationStrategy {
    /// A handle-bound, same-volume, atomic rename that never replaces target.
    AtomicNoReplaceRename,
    /// Exclusive staging, verified publication, then identity-bound deletion.
    CopyPublishDelete,
    /// No publication is needed; only a conditionally matched duplicate is
    /// removed (used to compensate a retained source after partial copy).
    ConditionalDeleteOnly,
}

impl MutationStrategy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AtomicNoReplaceRename => "atomic_no_replace_rename",
            Self::CopyPublishDelete => "copy_publish_delete",
            Self::ConditionalDeleteOnly => "conditional_delete_only",
        }
    }

    pub fn from_code(value: &str) -> Option<Self> {
        match value {
            "atomic_no_replace_rename" => Some(Self::AtomicNoReplaceRename),
            "copy_publish_delete" => Some(Self::CopyPublishDelete),
            "conditional_delete_only" => Some(Self::ConditionalDeleteOnly),
            _ => None,
        }
    }
}

impl MutationDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Rollback => "rollback",
            Self::Recovery => "recovery",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationLease {
    pub owner_id: String,
    pub scope_key: String,
    pub fencing_token: u64,
}

#[derive(Debug, Clone)]
pub struct OperationIntent {
    pub operation_id: String,
    pub attempt_id: String,
    pub plan_item_id: String,
    pub sequence_no: u64,
    pub direction: MutationDirection,
    pub strategy: MutationStrategy,
    pub source: PathBuf,
    pub target: PathBuf,
    pub temporary: PathBuf,
    pub expected_size: Option<u64>,
    pub expected_content_sha256: Option<String>,
    pub expected_file_identity: Option<String>,
    pub fencing_token: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    pub temporary: PathBuf,
    pub size_bytes: u64,
    pub content_sha256: String,
    /// Native identity of the staged/published object when the adapter can
    /// provide one. Hard-link publication preserves this identity; an atomic
    /// rename always preserves it.
    pub file_identity: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifySubject {
    Execution,
    Rollback,
    Recovery,
}

impl VerifySubject {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Execution => "execution",
            Self::Rollback => "rollback",
            Self::Recovery => "recovery",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    Resume,
    RollbackPublished,
    DiscardUnpublishedTemporary,
    ManualIntervention,
}

impl RecoveryAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resume => "resume",
            Self::RollbackPublished => "rollback_published",
            Self::DiscardUnpublishedTemporary => "discard_unpublished_temporary",
            Self::ManualIntervention => "manual_intervention",
        }
    }
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
