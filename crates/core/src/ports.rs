use crate::{
    ApplyItem, FileFingerprint, JournalState, MutationLease, NamingRules, OperationIntent,
    OperationLog, OperationResult, PlanItem, PreflightKind, PreflightLog, PreflightMode,
    PreflightOutcome, RecoveryAction, RunStatus, ScannedFile, StagedFile, TrackMetadata,
    VerifyItem, VerifySubject,
};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub trait MetadataReader: Send + Sync {
    fn read(&self, path: &Path) -> Result<TrackMetadata, String>;
}
pub trait ScanStore: Send + Sync {
    fn previous_metadata(
        &self,
        path: &Path,
        fingerprint: &FileFingerprint,
    ) -> Result<Option<TrackMetadata>, String>;
    fn begin_scan(&self, source: &Path) -> Result<String, String>;
    fn save_batch(&self, scan_id: &str, files: &[ScannedFile]) -> Result<(), String>;
    fn finish_scan(&self, scan_id: &str, status: RunStatus, warnings: u64) -> Result<(), String>;
    fn save_scan_warning(&self, _scan_id: &str, _warning: &str) -> Result<(), String> {
        Ok(())
    }
    fn record_metric(
        &self,
        _run_id: &str,
        _phase: &str,
        _elapsed_ms: u64,
        _item_count: u64,
    ) -> Result<(), String> {
        Ok(())
    }
}
pub trait FileSystem: Send + Sync {
    /// Calls `visitor` once per candidate. Implementations must not accumulate the
    /// entire library in memory before calling the visitor.
    fn enumerate(
        &self,
        root: &Path,
        follow_links: bool,
        visitor: &mut dyn FnMut(Result<std::path::PathBuf, String>) -> bool,
    ) -> Result<(), String>;
    fn fingerprint(&self, path: &Path) -> Result<FileFingerprint, String>;
}

#[derive(Debug, Clone)]
pub struct PlanMusicAnchor {
    pub source_directory: PathBuf,
    pub target_directory: PathBuf,
    pub disc_parent: Option<PathBuf>,
    pub music_item_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct StagedPlanItem {
    pub item: PlanItem,
    pub parent_plan_item_id: Option<String>,
    pub change_origin: Option<String>,
    pub change_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PlanRevisionBuild {
    pub child_plan_id: String,
    pub build_id: String,
    pub scan_id: String,
    pub scan_snapshot_hash: String,
    pub target_root: PathBuf,
    pub naming: NamingRules,
}

/// Durable scratch space used by bounded-memory Plan creation and revision.
///
/// Implementations keep global duplicate/conflict state outside Core's heap.
/// Every page returned by this port must be strictly ordered by `ordinal`.
pub trait PlanBuildStore: Send + Sync {
    fn stage_plan_batch(
        &self,
        build_id: &str,
        items: &[StagedPlanItem],
        anchors: &[PlanMusicAnchor],
    ) -> Result<(), String>;
    fn load_staged_plan_page(
        &self,
        build_id: &str,
        after_ordinal: u64,
        limit: usize,
    ) -> Result<Vec<StagedPlanItem>, String>;
    fn update_staged_plan_batch(
        &self,
        build_id: &str,
        items: &[StagedPlanItem],
        update_original_target: bool,
        allocated_target_keys: &[String],
    ) -> Result<(), String>;
    fn load_nearest_music_anchors(
        &self,
        build_id: &str,
        source_ancestors: &[PathBuf],
    ) -> Result<Vec<PlanMusicAnchor>, String>;
    fn original_target_rank(
        &self,
        build_id: &str,
        normalized_target_key: &str,
        ordinal: u64,
    ) -> Result<u64, String>;
    fn original_target_exists_after(
        &self,
        build_id: &str,
        normalized_target_key: &str,
        ordinal: u64,
    ) -> Result<bool, String>;
    fn target_is_allocated(
        &self,
        build_id: &str,
        normalized_target_key: &str,
    ) -> Result<bool, String>;
    fn staged_target_count(
        &self,
        build_id: &str,
        normalized_target_key: &str,
    ) -> Result<u64, String>;
    fn staged_plan_item_count(&self, build_id: &str) -> Result<u64, String>;
    /// Copies one bounded page from scratch storage into the immutable Plan.
    /// The Plan remains non-executable until `complete_plan_build` succeeds.
    fn publish_staged_plan_page(
        &self,
        build_id: &str,
        after_ordinal: u64,
        limit: usize,
    ) -> Result<Vec<PlanItem>, String>;
    fn complete_plan_build(
        &self,
        build_id: &str,
        conflict_count: u64,
        risk_count: u64,
        snapshot_hash: &str,
    ) -> Result<(), String>;
    /// Removes all scratch and partially published rows. `status` is limited to
    /// Failed or Cancelled so no incomplete Plan can become executable.
    fn abort_plan_build(&self, build_id: &str, status: RunStatus) -> Result<(), String>;
}

pub trait PlanStore: PlanBuildStore + Send + Sync {
    fn load_completed_scan(&self, scan_id: &str) -> Result<Vec<ScannedFile>, String>;
    fn load_completed_scan_page(
        &self,
        scan_id: &str,
        after_path: Option<&Path>,
        limit: usize,
    ) -> Result<Vec<ScannedFile>, String>;
    fn completed_scan_snapshot_hash(&self, scan_id: &str) -> Result<String, String> {
        Ok(format!("legacy_scan:{scan_id}"))
    }
    fn begin_plan(
        &self,
        scan_id: &str,
        target_root: &Path,
        naming: &NamingRules,
    ) -> Result<String, String>;
    fn save_plan_items(&self, plan_id: &str, items: &[PlanItem]) -> Result<(), String>;
    fn finish_plan(
        &self,
        plan_id: &str,
        conflict_count: u64,
        risk_count: u64,
        snapshot_hash: &str,
    ) -> Result<(), String>;
    fn fail_plan(&self, plan_id: &str) -> Result<(), String>;
    fn record_metric(
        &self,
        _run_id: &str,
        _phase: &str,
        _elapsed_ms: u64,
        _item_count: u64,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ManualTargetChange {
    pub plan_item_id: String,
    pub target: std::path::PathBuf,
    pub reason: String,
}

pub trait PlanRevisionStore: PlanBuildStore + Send + Sync {
    fn begin_plan_revision(&self, parent_plan_id: &str) -> Result<PlanRevisionBuild, String>;
    fn load_parent_plan_page(
        &self,
        parent_plan_id: &str,
        after_ordinal: u64,
        limit: usize,
    ) -> Result<Vec<StagedPlanItem>, String>;

    /// Compatibility entry point backed by the same bounded Core workflow used
    /// by `RevisePlanUseCase`.
    fn revise_plan(
        &self,
        parent_plan_id: &str,
        changes: &[ManualTargetChange],
    ) -> Result<String, String>
    where
        Self: Sized,
    {
        crate::usecases::execute_bounded_plan_revision(self, parent_plan_id, changes, 512)
            .map_err(|error| error.to_string())
    }
}

pub trait PreflightStore: Send + Sync {
    /// Starts a child validation attempt. Durable adapters override these
    /// defaults; lightweight test adapters may intentionally keep them in-memory.
    fn begin_preflight(
        &self,
        _parent_attempt_id: &str,
        _kind: PreflightKind,
        _mode: PreflightMode,
    ) -> Result<String, String> {
        Ok(Uuid::new_v4().to_string())
    }

    fn save_preflight_log(
        &self,
        _preflight_id: &str,
        _ordinal: u64,
        _log: &PreflightLog,
    ) -> Result<(), String> {
        Ok(())
    }

    fn finish_preflight(
        &self,
        _preflight_id: &str,
        _outcome: PreflightOutcome,
        _passed: u64,
        _blocked: u64,
    ) -> Result<(), String> {
        Ok(())
    }
}

pub trait ApplyStore: PreflightStore + Send + Sync {
    fn load_completed_plan(&self, plan_id: &str) -> Result<Vec<ApplyItem>, String>;
    /// Reject execution when the persisted plan rows no longer match their snapshot.
    fn validate_plan_snapshot(&self, plan_id: &str) -> Result<(), String>;
    /// Successful non-dry-run items are never mutated twice for the same plan.
    fn successful_plan_item_ids(&self, plan_id: &str) -> Result<Vec<String>, String>;
    fn begin_execution(&self, plan_id: &str, dry_run: bool) -> Result<String, String>;
    /// Acquires the cross-process mutation lease for this persisted plan.  The
    /// default is intentionally suitable only for in-memory adapters; durable
    /// adapters must override it and fence every journal transition.
    fn acquire_apply_lease(&self, _plan_id: &str, owner_id: &str) -> Result<MutationLease, String> {
        Ok(MutationLease {
            owner_id: owner_id.to_owned(),
            scope_key: "in_memory".into(),
            fencing_token: 0,
        })
    }
    fn heartbeat_apply_lease(&self, _lease: &MutationLease) -> Result<(), String> {
        Ok(())
    }
    fn release_apply_lease(&self, _lease: &MutationLease) -> Result<(), String> {
        Ok(())
    }
    /// Commits intent before any filesystem mutation.
    fn prepare_operation(&self, _intent: &OperationIntent) -> Result<(), String> {
        Ok(())
    }
    fn transition_operation(
        &self,
        _operation_id: &str,
        _from: JournalState,
        _to: JournalState,
        _lease: &MutationLease,
        _staged: Option<&StagedFile>,
        _error: Option<&str>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn save_operation(&self, execution_id: &str, operation: &OperationLog) -> Result<(), String>;
    fn finish_execution(
        &self,
        execution_id: &str,
        status: RunStatus,
        success: u64,
        skipped: u64,
        failed: u64,
    ) -> Result<(), String>;
    fn record_metric(
        &self,
        _run_id: &str,
        _phase: &str,
        _elapsed_ms: u64,
        _item_count: u64,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// Immutable evidence that must be validated against the exact file object
/// which is deleted.  A path-only check followed by an ordinary unlink is not
/// sufficient because another process can replace the directory entry between
/// those two operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteExpectation {
    pub size_bytes: u64,
    pub content_sha256: String,
    pub file_identity: Option<String>,
}

pub trait FileMutator: Send + Sync {
    fn exists(&self, path: &Path) -> bool;
    fn same_volume(&self, source: &Path, target: &Path) -> Result<bool, String>;
    fn move_file(&self, source: &Path, target: &Path) -> Result<(), String>;
    fn copy_file(&self, source: &Path, target: &Path) -> Result<(), String>;
    fn size(&self, path: &Path) -> Result<u64, String>;
    fn delete_file(&self, path: &Path) -> Result<(), String>;

    /// Selects the durable move protocol. The conservative default retains the
    /// copy protocol so non-Windows/product-test adapters do not accidentally
    /// opt into a rename primitive they cannot prove safe.
    fn select_move_strategy(
        &self,
        _source: &Path,
        _target: &Path,
    ) -> Result<crate::MutationStrategy, String> {
        Ok(crate::MutationStrategy::CopyPublishDelete)
    }

    /// Renames the exact source object to target in one same-volume atomic
    /// operation. Implementations must validate size, SHA-256, native identity,
    /// and reparse status on the same source handle; bind the final name to a
    /// no-follow target-parent handle; and fail if target exists.
    fn atomic_move_no_replace_if_matches(
        &self,
        _source: &Path,
        _target: &Path,
        _expected: &DeleteExpectation,
    ) -> Result<StagedFile, String> {
        Err("atomic_handle_bound_move_not_supported".into())
    }

    /// Deletes only the file object whose bytes and (when available) native
    /// identity match `expected`. Implementations must bind validation and
    /// deletion to the same opened file object. The conservative default
    /// refuses the mutation rather than providing a racy path-based fallback.
    fn delete_file_if_matches(
        &self,
        _path: &Path,
        _expected: &DeleteExpectation,
    ) -> Result<(), String> {
        Err("identity_bound_delete_not_supported".into())
    }

    /// Returns a stable SHA-256 of the current file bytes.
    fn content_sha256(&self, _path: &Path) -> Result<String, String> {
        Err("content_hash_not_supported".into())
    }

    fn mutation_fingerprint(&self, path: &Path) -> Result<FileFingerprint, String> {
        Ok(FileFingerprint {
            size_bytes: self.size(path)?,
            mtime_ns: 0,
            content_sha256: Some(self.content_sha256(path)?),
            file_identity: None,
            version: 1,
        })
    }

    /// Validates that an existing path and its existing parents are not
    /// symlinks/reparse points.  Implementations must use no-follow metadata.
    fn ensure_no_reparse_points(&self, _path: &Path) -> Result<(), String> {
        Err("reparse_check_not_supported".into())
    }

    /// Chooses an operation-owned temporary path in the target directory.
    fn temporary_path(&self, target: &Path, operation_id: &str) -> Result<PathBuf, String> {
        let parent = target
            .parent()
            .ok_or_else(|| "target_parent_missing".to_string())?;
        Ok(parent.join(format!(".mfb-{operation_id}.staging")))
    }

    /// Exclusively creates and fills a temporary file.  It must never write to
    /// the final target and must leave source untouched.
    fn stage_copy_exclusive(
        &self,
        _source: &Path,
        _temporary: &Path,
    ) -> Result<StagedFile, String> {
        Err("atomic_no_replace_not_supported".into())
    }

    /// Atomically publishes an already verified temporary file and fails when
    /// the target exists.  Overwrite-capable rename/copy is forbidden here.
    fn publish_no_replace(&self, _temporary: &Path, _target: &Path) -> Result<(), String> {
        Err("atomic_no_replace_not_supported".into())
    }

    fn remove_staged_file(&self, temporary: &Path) -> Result<(), String> {
        if self.exists(temporary) {
            self.delete_file(temporary)
        } else {
            Ok(())
        }
    }
}

pub trait VerifyStore: Send + Sync {
    fn begin_verify(&self, execution_id: &str) -> Result<String, String>;
    fn load_successful_operations(&self, execution_id: &str) -> Result<Vec<VerifyItem>, String>;
    fn save_verify_result(
        &self,
        execution_id: &str,
        operation_id: &str,
        result: OperationResult,
        error: Option<&str>,
    ) -> Result<(), String>;
    fn finish_verify(
        &self,
        verify_id: &str,
        status: RunStatus,
        success: u64,
        failed: u64,
    ) -> Result<(), String>;
    fn record_metric(
        &self,
        _run_id: &str,
        _phase: &str,
        _elapsed_ms: u64,
        _item_count: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    fn begin_verify_subject(
        &self,
        subject_id: &str,
        subject: VerifySubject,
    ) -> Result<String, String> {
        match subject {
            VerifySubject::Execution => self.begin_verify(subject_id),
            VerifySubject::Rollback | VerifySubject::Recovery => {
                Err("verify_subject_not_supported".into())
            }
        }
    }
    fn load_verify_subject_items(
        &self,
        subject_id: &str,
        subject: VerifySubject,
    ) -> Result<Vec<VerifyItem>, String> {
        match subject {
            VerifySubject::Execution => self.load_successful_operations(subject_id),
            VerifySubject::Rollback | VerifySubject::Recovery => {
                Err("verify_subject_not_supported".into())
            }
        }
    }
}

pub trait RollbackStore: PreflightStore + Send + Sync {
    fn begin_rollback(&self, execution_id: &str, dry_run: bool) -> Result<String, String>;
    fn acquire_rollback_lease(
        &self,
        _execution_id: &str,
        owner_id: &str,
    ) -> Result<MutationLease, String> {
        Ok(MutationLease {
            owner_id: owner_id.to_owned(),
            scope_key: "in_memory".into(),
            fencing_token: 0,
        })
    }
    fn heartbeat_rollback_lease(&self, _lease: &MutationLease) -> Result<(), String> {
        Ok(())
    }
    fn release_rollback_lease(&self, _lease: &MutationLease) -> Result<(), String> {
        Ok(())
    }
    fn prepare_rollback_operation(&self, _intent: &OperationIntent) -> Result<(), String> {
        Ok(())
    }
    fn transition_rollback_operation(
        &self,
        _operation_id: &str,
        _from: JournalState,
        _to: JournalState,
        _lease: &MutationLease,
        _staged: Option<&StagedFile>,
        _error: Option<&str>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn load_rollback_items(&self, execution_id: &str) -> Result<Vec<VerifyItem>, String>;
    /// Returns durable evidence for an earlier completed rollback of the exact
    /// apply operation. Callers must still verify current paths and fingerprint.
    fn completed_rollback_evidence(
        &self,
        _execution_id: &str,
        _operation_id: &str,
    ) -> Result<Option<RollbackCompletionEvidence>, String> {
        Ok(None)
    }
    fn save_rollback_result(
        &self,
        execution_id: &str,
        operation_id: &str,
        result: OperationResult,
        error: Option<&str>,
    ) -> Result<(), String>;
    fn finish_rollback(
        &self,
        rollback_id: &str,
        status: RunStatus,
        success: u64,
        skipped: u64,
        failed: u64,
    ) -> Result<(), String>;
    fn record_metric(
        &self,
        _run_id: &str,
        _phase: &str,
        _elapsed_ms: u64,
        _item_count: u64,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RecoveryItem {
    pub operation_id: String,
    pub attempt_id: String,
    pub sequence_no: u64,
    pub state: JournalState,
    pub strategy: crate::MutationStrategy,
    pub source: PathBuf,
    pub target: PathBuf,
    pub temporary: PathBuf,
    pub expected_size: Option<u64>,
    pub expected_content_sha256: Option<String>,
    pub expected_file_identity: Option<String>,
    pub staged_file_identity: Option<String>,
    pub suggested_action: RecoveryAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackCompletionEvidence {
    pub rollback_id: String,
    pub expected_size: u64,
    pub expected_content_sha256: String,
    pub restored_file_identity: Option<String>,
}

pub trait RecoveryStore: PreflightStore + Send + Sync {
    fn list_recovery_items(&self) -> Result<Vec<RecoveryItem>, String>;
    fn begin_recovery(
        &self,
        operation_id: &str,
        action: RecoveryAction,
        dry_run: bool,
    ) -> Result<String, String>;
    fn acquire_recovery_lease(
        &self,
        operation_id: &str,
        owner_id: &str,
    ) -> Result<MutationLease, String>;
    fn load_recovery_item_under_lease(
        &self,
        operation_id: &str,
        lease: &MutationLease,
    ) -> Result<RecoveryItem, String>;
    fn heartbeat_recovery_lease(&self, lease: &MutationLease) -> Result<(), String>;
    fn transition_recovery_operation(
        &self,
        operation_id: &str,
        from: JournalState,
        to: JournalState,
        lease: &MutationLease,
        staged: Option<&StagedFile>,
        error: Option<&str>,
    ) -> Result<(), String>;
    fn finish_recovery(
        &self,
        recovery_id: &str,
        status: RunStatus,
        error: Option<&str>,
    ) -> Result<(), String>;
    fn release_recovery_lease(&self, lease: &MutationLease) -> Result<(), String>;
}
