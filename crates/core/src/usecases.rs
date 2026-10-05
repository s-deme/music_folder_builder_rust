#![allow(clippy::items_after_test_module)]

use crate::{
    ports::{
        ApplyStore, DeleteExpectation, FileMutator, FileSystem, ManualTargetChange, MetadataReader,
        PlanBuildStore, PlanMusicAnchor, PlanRevisionStore, PlanStore, RecoveryStore,
        RollbackStore, ScanStore, StagedPlanItem, VerifyStore,
    },
    render_template, sanitize_component, windows_path_key, DomainError, DuplicateStrategy,
    ExecutionDisposition, FileKind, NamingRules, OperationAction, OperationLog, OperationResult,
    PlanAction, PlanAuthorizationHasher, PlanConflictCandidate, PlanItem, PreflightEngine,
    PreflightKind, PreflightLog, PreflightMode, PreflightOutcome, Risk, RunStatus, SafeTargetPath,
    ScannedFile, TargetPathPolicy, TrackMetadata, WorkflowResult,
};
use crossbeam_channel::{bounded, Receiver};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::Instant,
};
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);
impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone)]
pub struct RecoveryResult {
    pub recovery_id: String,
    pub operation_id: String,
    pub action: crate::RecoveryAction,
    pub dry_run: bool,
}

pub struct RecoveryUseCase<S, F> {
    pub store: Arc<S>,
    pub files: Arc<F>,
}

impl<S: RecoveryStore, F: FileMutator> RecoveryUseCase<S, F> {
    pub fn inspect(&self) -> WorkflowResult<Vec<crate::ports::RecoveryItem>> {
        Ok(self.store.list_recovery_items()?)
    }

    pub fn execute(
        &self,
        operation_id: &str,
        requested_action: Option<crate::RecoveryAction>,
        dry_run: bool,
    ) -> WorkflowResult<RecoveryResult> {
        let initial_item = self
            .store
            .list_recovery_items()?
            .into_iter()
            .find(|item| item.operation_id == operation_id)
            .ok_or_else(|| crate::WorkflowError::from("recovery_item_not_found"))?;
        let action = requested_action.unwrap_or(initial_item.suggested_action);
        if action == crate::RecoveryAction::ManualIntervention {
            return Err("manual_intervention_required".into());
        }
        if dry_run {
            let recovery_id = self.store.begin_recovery(operation_id, action, true)?;
            if let Err(error) = self.persist_recovery_preflight(
                &recovery_id,
                &initial_item,
                action,
                PreflightMode::DryRun,
            ) {
                let _ =
                    self.store
                        .finish_recovery(&recovery_id, RunStatus::Failed, Some(error.code()));
                return Err(error);
            }
            if let Err(error) = self
                .store
                .finish_recovery(&recovery_id, RunStatus::Completed, None)
            {
                let _ = self
                    .store
                    .finish_recovery(&recovery_id, RunStatus::Failed, Some(&error));
                return Err(error.into());
            }
            return Ok(RecoveryResult {
                recovery_id,
                operation_id: operation_id.to_owned(),
                action,
                dry_run,
            });
        }

        let owner_id = Uuid::new_v4().to_string();
        let lease = self.store.acquire_recovery_lease(operation_id, &owner_id)?;
        let locked_item = match self
            .store
            .load_recovery_item_under_lease(operation_id, &lease)
        {
            Ok(item) => item,
            Err(error) => {
                let _ = self.store.release_recovery_lease(&lease);
                return Err(error.into());
            }
        };
        let action = requested_action.unwrap_or(locked_item.suggested_action);
        if action == crate::RecoveryAction::ManualIntervention {
            let _ = self.store.release_recovery_lease(&lease);
            return Err("manual_intervention_required".into());
        }
        let recovery_id = match self.store.begin_recovery(operation_id, action, false) {
            Ok(recovery_id) => recovery_id,
            Err(error) => {
                let _ = self.store.release_recovery_lease(&lease);
                return Err(error.into());
            }
        };
        if let Err(error) = self.persist_recovery_preflight(
            &recovery_id,
            &locked_item,
            action,
            PreflightMode::Mutation,
        ) {
            let _ = self
                .store
                .finish_recovery(&recovery_id, RunStatus::Failed, Some(error.code()));
            let _ = self.store.release_recovery_lease(&lease);
            return Err(error);
        }
        let result = self
            .store
            .heartbeat_recovery_lease(&lease)
            .and_then(|()| self.execute_recovery_action(&locked_item, action, &lease));
        let status = if result.is_ok() {
            RunStatus::Completed
        } else {
            // The underlying journal was already recoverable before this
            // attempt. Any runtime failure must keep that fact explicit even
            // when the recovery mutation itself did not start.
            RunStatus::RecoveryRequired
        };
        let error = result.as_ref().err().map(String::as_str);
        let finish = self.store.finish_recovery(&recovery_id, status, error);
        let release = self.store.release_recovery_lease(&lease);
        if let Err(error) = result {
            if finish.is_err() {
                let _ = self.store.finish_recovery(
                    &recovery_id,
                    RunStatus::RecoveryRequired,
                    Some(&error),
                );
            }
            let _ = release;
            return Err(error.into());
        }
        if let Err(error) = finish {
            let _ =
                self.store
                    .finish_recovery(&recovery_id, RunStatus::RecoveryRequired, Some(&error));
            let _ = release;
            return Err(error.into());
        }
        release?;
        Ok(RecoveryResult {
            recovery_id,
            operation_id: operation_id.to_owned(),
            action,
            dry_run,
        })
    }

    fn persist_recovery_preflight(
        &self,
        recovery_id: &str,
        item: &crate::ports::RecoveryItem,
        action: crate::RecoveryAction,
        mode: PreflightMode,
    ) -> Result<(), crate::WorkflowError> {
        let preflight_id =
            self.store
                .begin_preflight(recovery_id, PreflightKind::Recovery, mode)?;
        let result = self.preflight_recovery(item, action);
        let expected = match (item.expected_size, item.expected_content_sha256.clone()) {
            (Some(size_bytes), content_sha256) => Some(crate::FileFingerprint {
                size_bytes,
                mtime_ns: 0,
                content_sha256,
                file_identity: item.expected_file_identity.clone(),
                version: 1,
            }),
            _ => None,
        };
        let (outcome, code) = match &result {
            Ok(()) => (PreflightOutcome::Passed, None),
            Err(error) => (PreflightOutcome::Blocked, Some(error.code().to_owned())),
        };
        let log = PreflightLog {
            item_id: item.operation_id.clone(),
            source: item.source.clone(),
            target: Some(item.target.clone()),
            outcome,
            code,
            expected,
            observed: None,
        };
        if let Err(error) = self.store.save_preflight_log(&preflight_id, 0, &log) {
            let _ = self
                .store
                .finish_preflight(&preflight_id, PreflightOutcome::Failed, 0, 0);
            return Err(error.into());
        }
        if let Err(error) = self.store.finish_preflight(
            &preflight_id,
            outcome,
            u64::from(outcome == PreflightOutcome::Passed),
            u64::from(outcome == PreflightOutcome::Blocked),
        ) {
            let _ = self.store.finish_preflight(
                &preflight_id,
                PreflightOutcome::Failed,
                u64::from(outcome == PreflightOutcome::Passed),
                u64::from(outcome == PreflightOutcome::Blocked),
            );
            return Err(error.into());
        }
        result
    }

    fn preflight_recovery(
        &self,
        item: &crate::ports::RecoveryItem,
        action: crate::RecoveryAction,
    ) -> Result<(), crate::WorkflowError> {
        PreflightEngine::new(self.files.as_ref())
            .recovery_paths(item)
            .map_err(crate::WorkflowError::from)?;
        match item.strategy {
            crate::MutationStrategy::AtomicNoReplaceRename => {
                self.preflight_atomic_recovery(item, action)
            }
            crate::MutationStrategy::CopyPublishDelete => {
                self.preflight_copy_recovery(item, action)
            }
            crate::MutationStrategy::ConditionalDeleteOnly => {
                Err("manual_intervention_required".into())
            }
        }
    }

    fn preflight_atomic_recovery(
        &self,
        item: &crate::ports::RecoveryItem,
        action: crate::RecoveryAction,
    ) -> Result<(), crate::WorkflowError> {
        if matches!(
            item.state,
            crate::JournalState::Staging | crate::JournalState::Staged
        ) {
            return Err("atomic_recovery_invalid_staging_phase".into());
        }
        let position = self
            .atomic_recovery_position(item)
            .map_err(crate::WorkflowError::from)?;
        match action {
            crate::RecoveryAction::Resume => Ok(()),
            crate::RecoveryAction::RollbackPublished => match position {
                AtomicRecoveryPosition::Source | AtomicRecoveryPosition::Target => Ok(()),
            },
            crate::RecoveryAction::DiscardUnpublishedTemporary => {
                Err("atomic_recovery_has_no_temporary".into())
            }
            crate::RecoveryAction::ManualIntervention => Err("manual_intervention_required".into()),
        }
    }

    fn preflight_copy_recovery(
        &self,
        item: &crate::ports::RecoveryItem,
        action: crate::RecoveryAction,
    ) -> Result<(), crate::WorkflowError> {
        let source_exists = self.files.exists(&item.source);
        let target_exists = self.files.exists(&item.target);
        let temporary_exists = self.files.exists(&item.temporary);
        match action {
            crate::RecoveryAction::DiscardUnpublishedTemporary => {
                if target_exists
                    || matches!(
                        item.state,
                        crate::JournalState::Published
                            | crate::JournalState::SourceDeleted
                            | crate::JournalState::Completed
                    )
                {
                    return Err("recovery_discard_after_publish_forbidden".into());
                }
                if !temporary_exists {
                    return Err("recovery_temporary_missing".into());
                }
                self.verify_recovery_content(
                    item,
                    &item.temporary,
                    item.staged_file_identity.as_deref(),
                )?;
            }
            crate::RecoveryAction::Resume => {
                if target_exists {
                    if matches!(
                        item.state,
                        crate::JournalState::Prepared | crate::JournalState::Staging
                    ) && item.staged_file_identity.is_none()
                    {
                        return Err("manual_intervention_required".into());
                    }
                    self.verify_recovery_content(
                        item,
                        &item.target,
                        item.staged_file_identity.as_deref(),
                    )?;
                } else if temporary_exists {
                    self.verify_recovery_content(
                        item,
                        &item.temporary,
                        item.staged_file_identity.as_deref(),
                    )?;
                } else if source_exists
                    && matches!(
                        item.state,
                        crate::JournalState::Prepared
                            | crate::JournalState::Staging
                            | crate::JournalState::RecoveryRequired
                    )
                {
                    self.verify_recovery_content(
                        item,
                        &item.source,
                        item.expected_file_identity.as_deref(),
                    )?;
                } else {
                    return Err("recovery_content_missing".into());
                }
            }
            crate::RecoveryAction::RollbackPublished => {
                if !source_exists || !target_exists {
                    return Err("manual_intervention_required".into());
                }
                self.verify_recovery_content(
                    item,
                    &item.source,
                    item.expected_file_identity.as_deref(),
                )?;
                self.verify_recovery_content(
                    item,
                    &item.target,
                    item.staged_file_identity.as_deref(),
                )?;
            }
            crate::RecoveryAction::ManualIntervention => {
                return Err("manual_intervention_required".into())
            }
        }
        Ok(())
    }

    fn verify_recovery_content(
        &self,
        item: &crate::ports::RecoveryItem,
        path: &Path,
        expected_identity: Option<&str>,
    ) -> Result<(), String> {
        let observed = self.files.mutation_fingerprint(path)?;
        if item
            .expected_size
            .is_some_and(|expected| observed.size_bytes != expected)
        {
            return Err("recovery_size_mismatch".into());
        }
        if item
            .expected_content_sha256
            .as_deref()
            .is_some_and(|expected| observed.content_sha256.as_deref() != Some(expected))
        {
            return Err("recovery_content_mismatch".into());
        }
        if expected_identity
            .is_some_and(|expected| observed.file_identity.as_deref() != Some(expected))
        {
            return Err("recovery_identity_mismatch".into());
        }
        Ok(())
    }

    fn execute_recovery_action(
        &self,
        item: &crate::ports::RecoveryItem,
        action: crate::RecoveryAction,
        lease: &crate::MutationLease,
    ) -> Result<(), String> {
        self.store.heartbeat_recovery_lease(lease)?;
        if item.strategy == crate::MutationStrategy::AtomicNoReplaceRename {
            return self.execute_atomic_recovery_action(item, action, lease);
        }
        match action {
            crate::RecoveryAction::DiscardUnpublishedTemporary => {
                self.store.heartbeat_recovery_lease(lease)?;
                self.delete_recovery_file(
                    item,
                    item.state,
                    &item.temporary,
                    item.staged_file_identity.as_deref(),
                    lease,
                )?;
                self.store
                    .heartbeat_recovery_lease(lease)
                    .map_err(|cause| {
                        self.mark_recovery_post_commit(
                            item,
                            item.state,
                            lease,
                            None,
                            "recovery_post_commit_heartbeat_failed",
                            &cause,
                        )
                    })?;
                self.transition_recovery_after_commit(
                    item,
                    item.state,
                    crate::JournalState::Failed,
                    lease,
                    None,
                    Some("unpublished_temporary_discarded"),
                )
            }
            crate::RecoveryAction::RollbackPublished => {
                self.store.heartbeat_recovery_lease(lease)?;
                self.verify_recovery_content(
                    item,
                    &item.source,
                    item.expected_file_identity.as_deref(),
                )?;
                self.verify_recovery_content(
                    item,
                    &item.target,
                    item.staged_file_identity.as_deref(),
                )?;
                self.delete_recovery_file(
                    item,
                    item.state,
                    &item.target,
                    item.staged_file_identity.as_deref(),
                    lease,
                )?;
                self.store
                    .heartbeat_recovery_lease(lease)
                    .map_err(|cause| {
                        self.mark_recovery_post_commit(
                            item,
                            item.state,
                            lease,
                            None,
                            "recovery_post_commit_heartbeat_failed",
                            &cause,
                        )
                    })?;
                self.transition_recovery_after_commit(
                    item,
                    item.state,
                    crate::JournalState::Failed,
                    lease,
                    None,
                    Some("published_target_rolled_back"),
                )
            }
            crate::RecoveryAction::Resume => self.resume_copy_recovery(item, lease),
            crate::RecoveryAction::ManualIntervention => Err("manual_intervention_required".into()),
        }
    }

    fn atomic_recovery_position(
        &self,
        item: &crate::ports::RecoveryItem,
    ) -> Result<AtomicRecoveryPosition, String> {
        match (
            self.files.exists(&item.source),
            self.files.exists(&item.target),
        ) {
            (true, false) => {
                self.verify_recovery_content(
                    item,
                    &item.source,
                    item.expected_file_identity.as_deref(),
                )?;
                Ok(AtomicRecoveryPosition::Source)
            }
            (false, true) => {
                self.verify_recovery_content(
                    item,
                    &item.target,
                    item.expected_file_identity.as_deref(),
                )?;
                Ok(AtomicRecoveryPosition::Target)
            }
            (true, true) => Err("atomic_recovery_both_paths_present".into()),
            (false, false) => Err("atomic_recovery_both_paths_missing".into()),
        }
    }

    fn execute_atomic_recovery_action(
        &self,
        item: &crate::ports::RecoveryItem,
        action: crate::RecoveryAction,
        lease: &crate::MutationLease,
    ) -> Result<(), String> {
        let position = self.atomic_recovery_position(item)?;
        match action {
            crate::RecoveryAction::Resume => {
                let mut state = item.state;
                let mut evidence = None;
                if position == AtomicRecoveryPosition::Source {
                    if state == crate::JournalState::Prepared {
                        self.store.transition_recovery_operation(
                            &item.operation_id,
                            state,
                            crate::JournalState::ContentVerified,
                            lease,
                            None,
                            None,
                        )?;
                        state = crate::JournalState::ContentVerified;
                    }
                    let expected = recovery_delete_expectation(item, true)?;
                    let moved = self
                        .files
                        .atomic_move_no_replace_if_matches(&item.source, &item.target, &expected)
                        .map_err(|cause| {
                            self.mark_recovery_post_commit(
                                item,
                                state,
                                lease,
                                None,
                                "recovery_post_commit_atomic_move_failed",
                                &cause,
                            )
                        })?;
                    evidence = Some(moved);
                }
                if !matches!(
                    state,
                    crate::JournalState::Published | crate::JournalState::SourceDeleted
                ) {
                    if state == crate::JournalState::Prepared {
                        self.store.transition_recovery_operation(
                            &item.operation_id,
                            state,
                            crate::JournalState::ContentVerified,
                            lease,
                            None,
                            None,
                        )?;
                        state = crate::JournalState::ContentVerified;
                    }
                    self.transition_recovery_after_commit(
                        item,
                        state,
                        crate::JournalState::Published,
                        lease,
                        evidence.as_ref(),
                        None,
                    )?;
                    state = crate::JournalState::Published;
                }
                if let Err(cause) = self.atomic_recovery_position(item).and_then(|position| {
                    (position == AtomicRecoveryPosition::Target)
                        .then_some(())
                        .ok_or_else(|| "atomic_recovery_resume_result_invalid".to_string())
                }) {
                    return Err(self.mark_recovery_post_commit(
                        item,
                        state,
                        lease,
                        evidence.as_ref(),
                        "recovery_post_commit_atomic_verify_failed",
                        &cause,
                    ));
                }
                if state == crate::JournalState::Published {
                    self.transition_recovery_after_commit(
                        item,
                        state,
                        crate::JournalState::SourceDeleted,
                        lease,
                        evidence.as_ref(),
                        None,
                    )?;
                    state = crate::JournalState::SourceDeleted;
                }
                self.transition_recovery_after_commit(
                    item,
                    state,
                    crate::JournalState::Completed,
                    lease,
                    evidence.as_ref(),
                    None,
                )
            }
            crate::RecoveryAction::RollbackPublished => {
                let evidence = if position == AtomicRecoveryPosition::Target {
                    let expected = recovery_delete_expectation(item, true)?;
                    Some(
                        self.files
                            .atomic_move_no_replace_if_matches(
                                &item.target,
                                &item.source,
                                &expected,
                            )
                            .map_err(|cause| {
                                self.mark_recovery_post_commit(
                                    item,
                                    item.state,
                                    lease,
                                    None,
                                    "recovery_post_commit_atomic_rollback_failed",
                                    &cause,
                                )
                            })?,
                    )
                } else {
                    None
                };
                if let Err(cause) = self.atomic_recovery_position(item).and_then(|position| {
                    (position == AtomicRecoveryPosition::Source)
                        .then_some(())
                        .ok_or_else(|| "atomic_recovery_rollback_result_invalid".to_string())
                }) {
                    return Err(self.mark_recovery_post_commit(
                        item,
                        item.state,
                        lease,
                        evidence.as_ref(),
                        "recovery_post_commit_atomic_verify_failed",
                        &cause,
                    ));
                }
                self.transition_recovery_after_commit(
                    item,
                    item.state,
                    crate::JournalState::Failed,
                    lease,
                    evidence.as_ref(),
                    Some("atomic_rename_rolled_back"),
                )
            }
            crate::RecoveryAction::DiscardUnpublishedTemporary => {
                Err("atomic_recovery_has_no_temporary".into())
            }
            crate::RecoveryAction::ManualIntervention => Err("manual_intervention_required".into()),
        }
    }

    fn resume_copy_recovery(
        &self,
        item: &crate::ports::RecoveryItem,
        lease: &crate::MutationLease,
    ) -> Result<(), String> {
        let mut state = item.state;
        let mut staged = None;
        if !self.files.exists(&item.target) {
            if !self.files.exists(&item.temporary) {
                if !matches!(
                    state,
                    crate::JournalState::Prepared
                        | crate::JournalState::Staging
                        | crate::JournalState::RecoveryRequired
                ) {
                    return Err("recovery_temporary_missing".into());
                }
                self.verify_recovery_content(
                    item,
                    &item.source,
                    item.expected_file_identity.as_deref(),
                )?;
                if state != crate::JournalState::Staging {
                    self.store.transition_recovery_operation(
                        &item.operation_id,
                        state,
                        crate::JournalState::Staging,
                        lease,
                        None,
                        None,
                    )?;
                    state = crate::JournalState::Staging;
                }
                staged = Some(
                    self.files
                        .stage_copy_exclusive(&item.source, &item.temporary)?,
                );
            } else {
                self.verify_recovery_content(
                    item,
                    &item.temporary,
                    item.staged_file_identity.as_deref(),
                )?;
                let fingerprint = self.files.mutation_fingerprint(&item.temporary)?;
                staged = Some(staged_from_fingerprint(&item.temporary, fingerprint)?);
                if state == crate::JournalState::Prepared {
                    self.store.transition_recovery_operation(
                        &item.operation_id,
                        state,
                        crate::JournalState::Staging,
                        lease,
                        None,
                        None,
                    )?;
                    state = crate::JournalState::Staging;
                }
            }
            if matches!(
                state,
                crate::JournalState::Staging | crate::JournalState::RecoveryRequired
            ) {
                self.transition_recovery_after_commit(
                    item,
                    state,
                    crate::JournalState::Staged,
                    lease,
                    staged.as_ref(),
                    None,
                )?;
                state = crate::JournalState::Staged;
            }
            if state == crate::JournalState::Staged {
                self.transition_recovery_after_commit(
                    item,
                    state,
                    crate::JournalState::ContentVerified,
                    lease,
                    staged.as_ref(),
                    None,
                )?;
                state = crate::JournalState::ContentVerified;
            }
            if state != crate::JournalState::ContentVerified {
                return Err("recovery_phase_not_publishable".into());
            }
            self.files
                .publish_no_replace(&item.temporary, &item.target)
                .map_err(|cause| {
                    self.mark_recovery_post_commit(
                        item,
                        state,
                        lease,
                        staged.as_ref(),
                        "recovery_post_commit_publish_failed",
                        &cause,
                    )
                })?;
            self.transition_recovery_after_commit(
                item,
                state,
                crate::JournalState::Published,
                lease,
                staged.as_ref(),
                None,
            )?;
            state = crate::JournalState::Published;
        } else {
            self.verify_recovery_content(item, &item.target, item.staged_file_identity.as_deref())
                .map_err(|cause| {
                    self.mark_recovery_post_commit(
                        item,
                        state,
                        lease,
                        None,
                        "recovery_post_commit_target_verify_failed",
                        &cause,
                    )
                })?;
            if matches!(
                state,
                crate::JournalState::Prepared | crate::JournalState::Staging
            ) && item.staged_file_identity.is_none()
            {
                return Err("manual_intervention_required".into());
            }
            if state == crate::JournalState::Staging {
                self.transition_recovery_after_commit(
                    item,
                    state,
                    crate::JournalState::Staged,
                    lease,
                    None,
                    None,
                )?;
                state = crate::JournalState::Staged;
            }
            if state == crate::JournalState::Staged {
                self.transition_recovery_after_commit(
                    item,
                    state,
                    crate::JournalState::ContentVerified,
                    lease,
                    None,
                    None,
                )?;
                state = crate::JournalState::ContentVerified;
            }
            if state == crate::JournalState::Prepared {
                self.transition_recovery_after_commit(
                    item,
                    state,
                    crate::JournalState::ContentVerified,
                    lease,
                    None,
                    None,
                )?;
                state = crate::JournalState::ContentVerified;
            }
            if !matches!(
                state,
                crate::JournalState::Published | crate::JournalState::SourceDeleted
            ) {
                self.transition_recovery_after_commit(
                    item,
                    state,
                    crate::JournalState::Published,
                    lease,
                    None,
                    None,
                )?;
                state = crate::JournalState::Published;
            }
        }
        self.verify_recovery_content(item, &item.target, item.staged_file_identity.as_deref())
            .map_err(|cause| {
                self.mark_recovery_post_commit(
                    item,
                    state,
                    lease,
                    staged.as_ref(),
                    "recovery_post_commit_target_verify_failed",
                    &cause,
                )
            })?;
        if self.files.exists(&item.source) {
            if state == crate::JournalState::SourceDeleted {
                return Err(self.mark_recovery_post_commit(
                    item,
                    state,
                    lease,
                    staged.as_ref(),
                    "recovery_post_commit_source_reappeared",
                    "recovery_source_reappeared",
                ));
            }
            self.verify_recovery_content(
                item,
                &item.source,
                item.expected_file_identity.as_deref(),
            )
            .map_err(|cause| {
                self.mark_recovery_post_commit(
                    item,
                    state,
                    lease,
                    staged.as_ref(),
                    "recovery_post_commit_source_changed",
                    &cause,
                )
            })?;
            self.delete_recovery_file(
                item,
                state,
                &item.source,
                item.expected_file_identity.as_deref(),
                lease,
            )?;
        }
        if state == crate::JournalState::Published {
            self.transition_recovery_after_commit(
                item,
                state,
                crate::JournalState::SourceDeleted,
                lease,
                staged.as_ref(),
                None,
            )?;
            state = crate::JournalState::SourceDeleted;
        }
        self.transition_recovery_after_commit(
            item,
            state,
            crate::JournalState::Completed,
            lease,
            staged.as_ref(),
            None,
        )
    }

    fn delete_recovery_file(
        &self,
        item: &crate::ports::RecoveryItem,
        state: crate::JournalState,
        path: &Path,
        expected_identity: Option<&str>,
        lease: &crate::MutationLease,
    ) -> Result<(), String> {
        let expected = recovery_delete_expectation_for_identity(item, expected_identity)?;
        if let Err(error) = self.files.delete_file_if_matches(path, &expected) {
            return Err(self.mark_recovery_post_commit(
                item,
                state,
                lease,
                None,
                "recovery_delete_failed",
                &error,
            ));
        }
        Ok(())
    }

    fn transition_recovery_after_commit(
        &self,
        item: &crate::ports::RecoveryItem,
        from: crate::JournalState,
        to: crate::JournalState,
        lease: &crate::MutationLease,
        staged: Option<&crate::StagedFile>,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.store
            .transition_recovery_operation(&item.operation_id, from, to, lease, staged, error)
            .map_err(|cause| {
                self.mark_recovery_post_commit(
                    item,
                    from,
                    lease,
                    staged,
                    "recovery_post_commit_journal_transition_failed",
                    &cause,
                )
            })
    }

    fn mark_recovery_post_commit(
        &self,
        item: &crate::ports::RecoveryItem,
        from: crate::JournalState,
        lease: &crate::MutationLease,
        staged: Option<&crate::StagedFile>,
        code: &str,
        cause: &str,
    ) -> String {
        let error = format!("{code}:{cause}");
        if from != crate::JournalState::RecoveryRequired {
            let _ = self.store.transition_recovery_operation(
                &item.operation_id,
                from,
                crate::JournalState::RecoveryRequired,
                lease,
                staged,
                Some(&error),
            );
        }
        error
    }
}

fn recovery_delete_expectation(
    item: &crate::ports::RecoveryItem,
    require_identity: bool,
) -> Result<DeleteExpectation, String> {
    Ok(DeleteExpectation {
        size_bytes: item
            .expected_size
            .ok_or_else(|| "recovery_delete_size_missing".to_string())?,
        content_sha256: item
            .expected_content_sha256
            .clone()
            .ok_or_else(|| "recovery_delete_hash_missing".to_string())?,
        file_identity: require_identity
            .then(|| item.expected_file_identity.clone())
            .flatten(),
    })
}

fn recovery_delete_expectation_for_identity(
    item: &crate::ports::RecoveryItem,
    expected_identity: Option<&str>,
) -> Result<DeleteExpectation, String> {
    Ok(DeleteExpectation {
        size_bytes: item
            .expected_size
            .ok_or_else(|| "recovery_delete_size_missing".to_string())?,
        content_sha256: item
            .expected_content_sha256
            .clone()
            .ok_or_else(|| "recovery_delete_hash_missing".to_string())?,
        file_identity: expected_identity.map(str::to_owned),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtomicRecoveryPosition {
    Source,
    Target,
}

fn staged_from_fingerprint(
    path: &Path,
    fingerprint: crate::FileFingerprint,
) -> Result<crate::StagedFile, String> {
    Ok(crate::StagedFile {
        temporary: path.to_path_buf(),
        size_bytes: fingerprint.size_bytes,
        content_sha256: fingerprint
            .content_sha256
            .ok_or_else(|| "recovery_staged_hash_missing".to_string())?,
        file_identity: fingerprint.file_identity,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScanProgress {
    pub scan_id: String,
    pub phase: String,
    pub enumerated: u64,
    pub processed: u64,
    pub cache_hits: u64,
    pub warnings: u64,
    pub elapsed_ms: u64,
    pub items_per_second: f64,
    pub eta_seconds: Option<u64>,
}
pub type ProgressSink = Arc<dyn Fn(ScanProgress) + Send + Sync>;

#[derive(Clone)]
pub struct ScanOptions {
    pub workers: usize,
    pub batch_size: usize,
    pub queue_capacity: usize,
    pub follow_reparse_points: bool,
    pub cancellation: CancellationToken,
    pub progress: Option<ProgressSink>,
}

pub struct RollbackResult {
    pub rollback_id: String,
    pub success: u64,
    pub skipped: u64,
    pub failed: u64,
}
pub struct RollbackUseCase<S, F> {
    pub store: Arc<S>,
    pub files: Arc<F>,
}
impl<S: RollbackStore, F: FileMutator> RollbackUseCase<S, F> {
    pub fn execute(&self, execution_id: &str, dry_run: bool) -> WorkflowResult<RollbackResult> {
        if dry_run {
            return self.execute_with_lease(execution_id, true, None);
        }
        let owner_id = Uuid::new_v4().to_string();
        let lease = self.store.acquire_rollback_lease(execution_id, &owner_id)?;
        let result = self.execute_with_lease(execution_id, false, Some(&lease));
        let release = self.store.release_rollback_lease(&lease);
        match (result, release) {
            (Ok(result), Ok(())) => Ok(result),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error.into()),
        }
    }

    fn execute_with_lease(
        &self,
        execution_id: &str,
        dry_run: bool,
        lease: Option<&crate::MutationLease>,
    ) -> WorkflowResult<RollbackResult> {
        let started = Instant::now();
        let rollback_id = self.store.begin_rollback(execution_id, dry_run)?;
        let preflight_id = match self.store.begin_preflight(
            &rollback_id,
            PreflightKind::Rollback,
            PreflightMode::from_dry_run(dry_run),
        ) {
            Ok(preflight_id) => preflight_id,
            Err(error) => {
                let _ = self
                    .store
                    .finish_rollback(&rollback_id, RunStatus::Failed, 0, 0, 0);
                return Err(error.into());
            }
        };
        let mut items = match self.store.load_rollback_items(execution_id) {
            Ok(items) => items,
            Err(error) => {
                let _ = self
                    .store
                    .finish_preflight(&preflight_id, PreflightOutcome::Failed, 0, 0);
                let _ = self
                    .store
                    .finish_rollback(&rollback_id, RunStatus::Failed, 0, 0, 0);
                return Err(error.into());
            }
        };
        items.sort_by_key(|item| std::cmp::Reverse(item.sequence_no));
        let (mut success, mut skipped, mut failed) = (0, 0, 0);
        let (mut preflight_passed, mut preflight_blocked) = (0, 0);
        let mut recovery_required = false;
        for item in items {
            if let Some(lease) = lease {
                if let Err(error) = self.store.heartbeat_rollback_lease(lease) {
                    self.terminate_rollback_attempt(
                        &rollback_id,
                        &preflight_id,
                        RunStatus::Failed,
                        success,
                        skipped,
                        failed,
                        preflight_passed,
                        preflight_blocked,
                    );
                    return Err(error.into());
                }
            }
            let completed_evidence = match self
                .store
                .completed_rollback_evidence(execution_id, &item.operation_id)
            {
                Ok(evidence) => evidence,
                Err(error) => {
                    self.terminate_rollback_attempt(
                        &rollback_id,
                        &preflight_id,
                        RunStatus::Failed,
                        success,
                        skipped,
                        failed,
                        preflight_passed,
                        preflight_blocked,
                    );
                    return Err(error.into());
                }
            };
            let expected = item.expected_size.map(|size_bytes| crate::FileFingerprint {
                size_bytes,
                mtime_ns: 0,
                content_sha256: item.expected_content_sha256.clone(),
                file_identity: item.expected_file_identity.clone(),
                version: 1,
            });
            let completed_result = completed_evidence.as_ref().map(|evidence| {
                let restored = item.target.as_ref().is_some_and(|applied_target| {
                    !self.files.exists(applied_target)
                        && self.files.exists(&item.source)
                        && self
                            .files
                            .mutation_fingerprint(&item.source)
                            .is_ok_and(|fingerprint| {
                                fingerprint.size_bytes == evidence.expected_size
                                    && fingerprint.content_sha256.as_deref()
                                        == Some(&evidence.expected_content_sha256)
                                    && (evidence.restored_file_identity.is_none()
                                        || fingerprint.file_identity
                                            == evidence.restored_file_identity)
                            })
                });
                if restored {
                    (
                        OperationResult::Skipped,
                        Some("already_restored_from_completed_rollback"),
                    )
                } else {
                    (
                        OperationResult::Failed,
                        Some("completed_rollback_state_conflict"),
                    )
                }
            });
            let preflight = completed_result.as_ref().map_or_else(
                || {
                    let engine = PreflightEngine::new(self.files.as_ref());
                    if item.action == OperationAction::CopySourceRetained {
                        engine.duplicate_cleanup(&item)
                    } else {
                        engine.rollback(&item)
                    }
                },
                |_| Err("rollback_already_completed".to_string()),
            );
            let preflight_code = completed_result
                .as_ref()
                .and_then(|(_, code)| code.map(str::to_owned))
                .or_else(|| {
                    preflight
                        .as_ref()
                        .err()
                        .map(|error| rollback_preflight_error(&item, error))
                });
            let preflight_outcome = if completed_result
                .as_ref()
                .is_some_and(|(result, _)| *result != OperationResult::Failed)
                || completed_result.is_none() && preflight.is_ok()
            {
                preflight_passed += 1;
                PreflightOutcome::Passed
            } else {
                preflight_blocked += 1;
                PreflightOutcome::Blocked
            };
            let log = match (preflight_outcome, preflight.as_ref().ok()) {
                (PreflightOutcome::Passed, Some(evidence)) => PreflightLog::passed(
                    item.operation_id.clone(),
                    item.target.clone().unwrap_or_else(|| item.source.clone()),
                    Some(item.source.clone()),
                    expected.clone(),
                    evidence,
                ),
                _ => PreflightLog {
                    item_id: item.operation_id.clone(),
                    source: item.target.clone().unwrap_or_else(|| item.source.clone()),
                    target: Some(item.source.clone()),
                    outcome: preflight_outcome,
                    code: preflight_code.clone(),
                    expected: expected.clone(),
                    observed: None,
                },
            };
            if let Err(save_error) =
                self.store
                    .save_preflight_log(&preflight_id, item.sequence_no, &log)
            {
                self.terminate_rollback_attempt(
                    &rollback_id,
                    &preflight_id,
                    RunStatus::Failed,
                    success,
                    skipped,
                    failed,
                    preflight_passed,
                    preflight_blocked,
                );
                return Err(save_error.into());
            }
            let (result, error) = if let Some((result, error)) = completed_result {
                match result {
                    OperationResult::Skipped => skipped += 1,
                    OperationResult::Failed => failed += 1,
                    OperationResult::Success => success += 1,
                }
                (result, error.map(str::to_owned))
            } else if let Err(error) = preflight.as_ref() {
                let error = rollback_preflight_error(&item, error);
                if error == "target_missing_in_log" {
                    skipped += 1;
                    (OperationResult::Skipped, Some(error))
                } else {
                    failed += 1;
                    (OperationResult::Failed, Some(error))
                }
            } else if dry_run {
                success += 1;
                (OperationResult::Success, None)
            } else {
                let Some(lease) = lease else {
                    self.terminate_rollback_attempt(
                        &rollback_id,
                        &preflight_id,
                        RunStatus::Failed,
                        success,
                        skipped,
                        failed,
                        preflight_passed,
                        preflight_blocked,
                    );
                    return Err("rollback_mutation_lease_missing".into());
                };
                let expectation = preflight
                    .as_ref()
                    .expect("successful rollback preflight evidence must be present");
                let mutation = if item.action == OperationAction::CopySourceRetained {
                    self.cleanup_partial_copy(
                        &rollback_id,
                        &item,
                        item.target
                            .as_deref()
                            .expect("duplicate-cleanup preflight requires target"),
                        lease,
                    )
                } else {
                    self.rollback_one(
                        &rollback_id,
                        &item,
                        item.target
                            .as_deref()
                            .expect("rollback preflight requires target"),
                        expectation,
                        lease,
                    )
                };
                match mutation {
                    Ok(()) => {
                        success += 1;
                        (OperationResult::Success, None)
                    }
                    Err(error) => {
                        recovery_required |= mutation_error_requires_recovery(&error);
                        failed += 1;
                        (OperationResult::Failed, Some(error))
                    }
                }
            };
            if let Err(save_error) = self.store.save_rollback_result(
                &rollback_id,
                &item.operation_id,
                result,
                error.as_deref(),
            ) {
                self.terminate_rollback_attempt(
                    &rollback_id,
                    &preflight_id,
                    if dry_run {
                        RunStatus::Failed
                    } else {
                        RunStatus::RecoveryRequired
                    },
                    success,
                    skipped,
                    failed + 1,
                    preflight_passed,
                    preflight_blocked,
                );
                return Err(save_error.into());
            }
            if recovery_required {
                break;
            }
        }
        let status = if recovery_required {
            RunStatus::RecoveryRequired
        } else if failed == 0 {
            RunStatus::Completed
        } else {
            RunStatus::Partial
        };
        if let Err(error) = self.store.finish_preflight(
            &preflight_id,
            if preflight_blocked == 0 {
                PreflightOutcome::Passed
            } else {
                PreflightOutcome::Blocked
            },
            preflight_passed,
            preflight_blocked,
        ) {
            self.terminate_rollback_attempt(
                &rollback_id,
                &preflight_id,
                if recovery_required {
                    RunStatus::RecoveryRequired
                } else {
                    RunStatus::Failed
                },
                success,
                skipped,
                failed,
                preflight_passed,
                preflight_blocked,
            );
            return Err(error.into());
        }
        if let Err(error) =
            self.store
                .finish_rollback(&rollback_id, status, success, skipped, failed)
        {
            let _ = self
                .store
                .finish_rollback(&rollback_id, status, success, skipped, failed);
            return Err(error.into());
        }
        self.store.record_metric(
            &rollback_id,
            "rollback",
            started.elapsed().as_millis() as u64,
            success + skipped + failed,
        )?;
        Ok(RollbackResult {
            rollback_id,
            success,
            skipped,
            failed,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn terminate_rollback_attempt(
        &self,
        rollback_id: &str,
        preflight_id: &str,
        status: RunStatus,
        success: u64,
        skipped: u64,
        failed: u64,
        preflight_passed: u64,
        preflight_blocked: u64,
    ) {
        let _ = self.store.finish_preflight(
            preflight_id,
            PreflightOutcome::Failed,
            preflight_passed,
            preflight_blocked,
        );
        let _ = self
            .store
            .finish_rollback(rollback_id, status, success, skipped, failed);
    }

    fn rollback_one(
        &self,
        rollback_id: &str,
        item: &crate::VerifyItem,
        applied_target: &Path,
        expectation: &MovePreflight,
        lease: &crate::MutationLease,
    ) -> Result<(), String> {
        let operation_id = Uuid::new_v4().to_string();
        let strategy = self
            .files
            .select_move_strategy(applied_target, &item.source)?;
        let temporary = if strategy == crate::MutationStrategy::CopyPublishDelete {
            self.files.temporary_path(&item.source, &operation_id)?
        } else {
            PathBuf::new()
        };
        let intent = crate::OperationIntent {
            operation_id: operation_id.clone(),
            attempt_id: rollback_id.to_owned(),
            plan_item_id: item.operation_id.clone(),
            sequence_no: item.sequence_no,
            direction: crate::MutationDirection::Rollback,
            strategy,
            source: applied_target.to_path_buf(),
            target: item.source.clone(),
            temporary: temporary.clone(),
            expected_size: Some(expectation.size_bytes),
            expected_content_sha256: Some(expectation.content_sha256.clone()),
            expected_file_identity: expectation.file_identity.clone(),
            fencing_token: lease.fencing_token,
        };
        self.store.prepare_rollback_operation(&intent)?;
        if strategy == crate::MutationStrategy::AtomicNoReplaceRename {
            return self.rollback_one_atomic(
                &operation_id,
                applied_target,
                &item.source,
                expectation,
                lease,
            );
        }
        if strategy != crate::MutationStrategy::CopyPublishDelete {
            self.store.transition_rollback_operation(
                &operation_id,
                crate::JournalState::Prepared,
                crate::JournalState::Failed,
                lease,
                None,
                Some("rollback_strategy_invalid"),
            )?;
            return Err("rollback_strategy_invalid".into());
        }
        self.store.transition_rollback_operation(
            &operation_id,
            crate::JournalState::Prepared,
            crate::JournalState::Staging,
            lease,
            None,
            None,
        )?;
        let staged = match self.files.stage_copy_exclusive(applied_target, &temporary) {
            Ok(staged) => staged,
            Err(error) => {
                let _ = self.store.transition_rollback_operation(
                    &operation_id,
                    crate::JournalState::Staging,
                    crate::JournalState::Failed,
                    lease,
                    None,
                    Some(&error),
                );
                return Err(error);
            }
        };
        self.store.transition_rollback_operation(
            &operation_id,
            crate::JournalState::Staging,
            crate::JournalState::Staged,
            lease,
            Some(&staged),
            None,
        )?;
        if staged.size_bytes != expectation.size_bytes
            || staged.content_sha256 != expectation.content_sha256
        {
            let expected = staged_delete_expectation(&staged);
            let _ = self
                .files
                .delete_file_if_matches(&staged.temporary, &expected);
            self.store.transition_rollback_operation(
                &operation_id,
                crate::JournalState::Staged,
                crate::JournalState::Failed,
                lease,
                Some(&staged),
                Some("staged_content_mismatch"),
            )?;
            return Err("staged_content_mismatch".into());
        }
        self.store.transition_rollback_operation(
            &operation_id,
            crate::JournalState::Staged,
            crate::JournalState::ContentVerified,
            lease,
            Some(&staged),
            None,
        )?;
        if let Err(error) = self
            .files
            .publish_no_replace(&staged.temporary, &item.source)
        {
            let _ = self.store.transition_rollback_operation(
                &operation_id,
                crate::JournalState::ContentVerified,
                if error.starts_with("published_") {
                    crate::JournalState::RecoveryRequired
                } else {
                    crate::JournalState::Failed
                },
                lease,
                Some(&staged),
                Some(&error),
            );
            return Err(error);
        }
        self.transition_rollback_after_commit(
            &operation_id,
            crate::JournalState::ContentVerified,
            crate::JournalState::Published,
            lease,
            Some(&staged),
        )?;
        let restored_hash = self.files.content_sha256(&item.source).map_err(|cause| {
            self.mark_rollback_recovery(
                &operation_id,
                crate::JournalState::Published,
                lease,
                Some(&staged),
                "published_rollback_source_hash_failed",
                &cause,
            )
        })?;
        if restored_hash != expectation.content_sha256 {
            self.set_rollback_recovery(
                &operation_id,
                crate::JournalState::Published,
                lease,
                Some(&staged),
                "published_content_mismatch",
            )?;
            return Err("published_content_mismatch".into());
        }
        let applied_now = self
            .files
            .mutation_fingerprint(applied_target)
            .map_err(|cause| {
                self.mark_rollback_recovery(
                    &operation_id,
                    crate::JournalState::Published,
                    lease,
                    Some(&staged),
                    "published_rollback_target_fingerprint_failed",
                    &cause,
                )
            })?;
        if applied_now.size_bytes != expectation.size_bytes
            || applied_now.content_sha256.as_deref() != Some(&expectation.content_sha256)
            || expectation.file_identity.is_some()
                && applied_now.file_identity != expectation.file_identity
        {
            self.set_rollback_recovery(
                &operation_id,
                crate::JournalState::Published,
                lease,
                Some(&staged),
                "rollback_target_changed_before_delete",
            )?;
            return Err("rollback_target_changed_before_delete".into());
        }
        let delete_expected = DeleteExpectation {
            size_bytes: expectation.size_bytes,
            content_sha256: expectation.content_sha256.clone(),
            file_identity: expectation.file_identity.clone(),
        };
        if let Err(error) = self
            .files
            .delete_file_if_matches(applied_target, &delete_expected)
        {
            self.set_rollback_recovery(
                &operation_id,
                crate::JournalState::Published,
                lease,
                Some(&staged),
                &error,
            )?;
            return Err(format!("rollback_target_delete_failed:{error}"));
        }
        self.transition_rollback_after_commit(
            &operation_id,
            crate::JournalState::Published,
            crate::JournalState::SourceDeleted,
            lease,
            Some(&staged),
        )?;
        self.transition_rollback_after_commit(
            &operation_id,
            crate::JournalState::SourceDeleted,
            crate::JournalState::Completed,
            lease,
            Some(&staged),
        )?;
        Ok(())
    }

    fn rollback_one_atomic(
        &self,
        operation_id: &str,
        applied_target: &Path,
        restored_source: &Path,
        expectation: &MovePreflight,
        lease: &crate::MutationLease,
    ) -> Result<(), String> {
        self.store.transition_rollback_operation(
            operation_id,
            crate::JournalState::Prepared,
            crate::JournalState::ContentVerified,
            lease,
            None,
            None,
        )?;
        let expected = expectation.delete_expectation();
        let moved = match self.files.atomic_move_no_replace_if_matches(
            applied_target,
            restored_source,
            &expected,
        ) {
            Ok(moved) => moved,
            Err(error) => {
                let requires_recovery = atomic_error_requires_recovery(&error);
                let reported_error = if requires_recovery {
                    format!("published_atomic_rollback_failed:{error}")
                } else {
                    error.clone()
                };
                let state = if requires_recovery {
                    crate::JournalState::RecoveryRequired
                } else {
                    crate::JournalState::Failed
                };
                let _ = self.store.transition_rollback_operation(
                    operation_id,
                    crate::JournalState::ContentVerified,
                    state,
                    lease,
                    None,
                    Some(&reported_error),
                );
                return Err(reported_error);
            }
        };
        self.transition_rollback_after_commit(
            operation_id,
            crate::JournalState::ContentVerified,
            crate::JournalState::Published,
            lease,
            Some(&moved),
        )?;
        if let Err(cause) = verify_atomic_move_result(
            self.files.as_ref(),
            applied_target,
            restored_source,
            expectation,
        ) {
            let error = format!("published_atomic_rollback_verify_failed:{cause}");
            self.set_rollback_recovery(
                operation_id,
                crate::JournalState::Published,
                lease,
                Some(&moved),
                &error,
            )?;
            return Err(error);
        }
        self.transition_rollback_after_commit(
            operation_id,
            crate::JournalState::Published,
            crate::JournalState::SourceDeleted,
            lease,
            Some(&moved),
        )?;
        self.transition_rollback_after_commit(
            operation_id,
            crate::JournalState::SourceDeleted,
            crate::JournalState::Completed,
            lease,
            Some(&moved),
        )
    }

    fn transition_rollback_after_commit(
        &self,
        operation_id: &str,
        from: crate::JournalState,
        to: crate::JournalState,
        lease: &crate::MutationLease,
        staged: Option<&crate::StagedFile>,
    ) -> Result<(), String> {
        self.store
            .transition_rollback_operation(operation_id, from, to, lease, staged, None)
            .map_err(|cause| {
                self.mark_rollback_recovery(
                    operation_id,
                    from,
                    lease,
                    staged,
                    "published_journal_transition_failed",
                    &cause,
                )
            })
    }

    fn mark_rollback_recovery(
        &self,
        operation_id: &str,
        from: crate::JournalState,
        lease: &crate::MutationLease,
        staged: Option<&crate::StagedFile>,
        code: &str,
        cause: &str,
    ) -> String {
        let error = format!("{code}:{cause}");
        let _ = self.store.transition_rollback_operation(
            operation_id,
            from,
            crate::JournalState::RecoveryRequired,
            lease,
            staged,
            Some(&error),
        );
        error
    }

    fn set_rollback_recovery(
        &self,
        operation_id: &str,
        from: crate::JournalState,
        lease: &crate::MutationLease,
        staged: Option<&crate::StagedFile>,
        error: &str,
    ) -> Result<(), String> {
        self.store
            .transition_rollback_operation(
                operation_id,
                from,
                crate::JournalState::RecoveryRequired,
                lease,
                staged,
                Some(error),
            )
            .map_err(|cause| {
                self.mark_rollback_recovery(
                    operation_id,
                    from,
                    lease,
                    staged,
                    "published_journal_transition_failed",
                    &cause,
                )
            })
    }

    fn cleanup_partial_copy(
        &self,
        rollback_id: &str,
        item: &crate::VerifyItem,
        copied_target: &Path,
        lease: &crate::MutationLease,
    ) -> Result<(), String> {
        let operation_id = Uuid::new_v4().to_string();
        let expected_size = self.files.size(copied_target)?;
        let expected_content_sha256 = self.files.content_sha256(copied_target)?;
        let expected_file_identity = self
            .files
            .mutation_fingerprint(copied_target)?
            .file_identity;
        let intent = crate::OperationIntent {
            operation_id: operation_id.clone(),
            attempt_id: rollback_id.to_owned(),
            plan_item_id: item.operation_id.clone(),
            sequence_no: item.sequence_no,
            direction: crate::MutationDirection::Rollback,
            strategy: crate::MutationStrategy::ConditionalDeleteOnly,
            source: copied_target.to_path_buf(),
            target: item.source.clone(),
            temporary: PathBuf::new(),
            expected_size: Some(expected_size),
            expected_content_sha256: Some(expected_content_sha256.clone()),
            expected_file_identity: expected_file_identity.clone(),
            fencing_token: lease.fencing_token,
        };
        self.store.prepare_rollback_operation(&intent)?;
        let restored_fingerprint = self.files.mutation_fingerprint(&item.source)?;
        let restored_evidence = crate::StagedFile {
            temporary: PathBuf::new(),
            size_bytes: restored_fingerprint.size_bytes,
            content_sha256: restored_fingerprint
                .content_sha256
                .clone()
                .ok_or_else(|| "restored_source_hash_missing".to_string())?,
            file_identity: restored_fingerprint.file_identity,
        };
        self.store.transition_rollback_operation(
            &operation_id,
            crate::JournalState::Prepared,
            crate::JournalState::ContentVerified,
            lease,
            Some(&restored_evidence),
            None,
        )?;
        if self.files.content_sha256(&item.source)? != expected_content_sha256
            || self.files.size(&item.source)? != expected_size
        {
            self.store.transition_rollback_operation(
                &operation_id,
                crate::JournalState::ContentVerified,
                crate::JournalState::Failed,
                lease,
                None,
                Some("source_changed_after_partial_copy"),
            )?;
            return Err("source_changed_after_partial_copy".into());
        }
        let copied_now = self.files.mutation_fingerprint(copied_target)?;
        if copied_now.size_bytes != expected_size
            || copied_now.content_sha256.as_deref() != Some(&expected_content_sha256)
            || expected_file_identity.is_some()
                && copied_now.file_identity != expected_file_identity
        {
            self.store.transition_rollback_operation(
                &operation_id,
                crate::JournalState::ContentVerified,
                crate::JournalState::RecoveryRequired,
                lease,
                None,
                Some("partial_copy_changed_before_delete"),
            )?;
            return Err("partial_copy_changed_before_delete".into());
        }
        let delete_expected = DeleteExpectation {
            size_bytes: expected_size,
            content_sha256: expected_content_sha256,
            file_identity: expected_file_identity,
        };
        if let Err(error) = self
            .files
            .delete_file_if_matches(copied_target, &delete_expected)
        {
            self.store.transition_rollback_operation(
                &operation_id,
                crate::JournalState::ContentVerified,
                crate::JournalState::RecoveryRequired,
                lease,
                None,
                Some(&error),
            )?;
            return Err(format!("partial_copy_delete_failed:{error}"));
        }
        self.transition_rollback_after_commit(
            &operation_id,
            crate::JournalState::ContentVerified,
            crate::JournalState::SourceDeleted,
            lease,
            Some(&restored_evidence),
        )?;
        self.transition_rollback_after_commit(
            &operation_id,
            crate::JournalState::SourceDeleted,
            crate::JournalState::Completed,
            lease,
            Some(&restored_evidence),
        )
    }
}
impl Default for ScanOptions {
    fn default() -> Self {
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 8));
        Self {
            workers,
            batch_size: 250,
            queue_capacity: workers * 4,
            follow_reparse_points: false,
            cancellation: CancellationToken::default(),
            progress: None,
        }
    }
}
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub scan_id: String,
    pub files: u64,
    pub cache_hits: u64,
    pub warnings: u64,
}

pub struct ScanUseCase<F, M, S> {
    pub fs: Arc<F>,
    pub metadata: Arc<M>,
    pub store: Arc<S>,
}
impl<F: FileSystem + 'static, M: MetadataReader + 'static, S: ScanStore + 'static>
    ScanUseCase<F, M, S>
{
    pub fn execute(&self, root: &Path, options: &ScanOptions) -> WorkflowResult<ScanResult> {
        let scan_id = self.store.begin_scan(root)?;
        self.execute_started(root, options, scan_id)
    }

    /// Continues a scan already persisted by a composing use case.
    pub fn execute_started(
        &self,
        root: &Path,
        options: &ScanOptions,
        scan_id: String,
    ) -> WorkflowResult<ScanResult> {
        let started = Instant::now();
        let worker_count = options.workers.max(1);
        let queue_capacity = options.queue_capacity.max(worker_count);
        let (path_sender, path_receiver) = bounded(queue_capacity);
        let (result_sender, result_receiver) = bounded(queue_capacity);
        let root = root.to_path_buf();
        let cancellation = options.cancellation.clone();
        let progress = options.progress.clone();
        let progress_id = scan_id.clone();
        let enumerated_count = Arc::new(AtomicU64::new(0));
        let enumerated_for_thread = Arc::clone(&enumerated_count);

        let enumerator_fs = Arc::clone(&self.fs);
        let follow_reparse_points = options.follow_reparse_points;
        let enumerator = thread::spawn(move || {
            let phase_started = Instant::now();
            let mut enumerated = 0u64;
            let mut send = |item| {
                if cancellation.is_cancelled() {
                    return false;
                }
                enumerated += 1;
                enumerated_for_thread.store(enumerated, Ordering::Release);
                let sent = path_sender.send(item).is_ok();
                if let Some(sink) = &progress {
                    sink(ScanProgress {
                        scan_id: progress_id.clone(),
                        phase: "enumerate".into(),
                        enumerated,
                        processed: 0,
                        cache_hits: 0,
                        warnings: 0,
                        elapsed_ms: 0,
                        items_per_second: 0.0,
                        eta_seconds: None,
                    });
                }
                sent
            };
            (
                enumerator_fs.enumerate(&root, follow_reparse_points, &mut send),
                phase_started.elapsed().as_millis() as u64,
                enumerated,
            )
        });

        let mut workers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            workers.push(spawn_scan_worker(
                Arc::clone(&self.fs),
                Arc::clone(&self.metadata),
                Arc::clone(&self.store),
                path_receiver.clone(),
                result_sender.clone(),
                options.cancellation.clone(),
            ));
        }
        drop(result_sender);

        let mut batch = Vec::with_capacity(options.batch_size);
        let mut hits = 0;
        let mut warnings = 0;
        let mut total = 0;
        let mut tag_read_ms = 0;
        let mut db_write_ms = 0;
        if let Err(error) = consume_scan_results(
            &result_receiver,
            &mut batch,
            &mut hits,
            &mut warnings,
            &mut total,
            options.batch_size.max(1),
            |batch| {
                let started = Instant::now();
                let result = self.store.save_batch(&scan_id, batch);
                db_write_ms += started.elapsed().as_millis() as u64;
                result
            },
            |warning| self.store.save_scan_warning(&scan_id, warning),
            &options.cancellation,
            options.progress.as_ref(),
            &scan_id,
            &started,
            &mut tag_read_ms,
            &enumerated_count,
        ) {
            options.cancellation.cancel();
            let _ = self
                .store
                .finish_scan(&scan_id, RunStatus::Failed, warnings);
            return Err(error.into());
        }
        let (enumeration, enumerate_ms, enumerated) = match enumerator.join() {
            Ok(value) => value,
            Err(_) => {
                let _ = self
                    .store
                    .finish_scan(&scan_id, RunStatus::Failed, warnings);
                return Err("scan enumerator panicked".into());
            }
        };
        if let Err(error) = enumeration {
            self.store
                .finish_scan(&scan_id, RunStatus::Failed, warnings)?;
            return Err(error.into());
        }
        for worker in workers {
            if worker.join().is_err() {
                let _ = self
                    .store
                    .finish_scan(&scan_id, RunStatus::Failed, warnings);
                return Err("scan worker panicked".into());
            }
        }
        if !batch.is_empty() {
            if let Err(error) = self.store.save_batch(&scan_id, &batch) {
                let _ = self
                    .store
                    .finish_scan(&scan_id, RunStatus::Failed, warnings);
                return Err(error.into());
            }
        }
        let status = if options.cancellation.is_cancelled() {
            RunStatus::Cancelled
        } else {
            RunStatus::Completed
        };
        self.store.finish_scan(&scan_id, status, warnings)?;
        self.store.record_metric(
            &scan_id,
            "scan",
            started.elapsed().as_millis() as u64,
            total,
        )?;
        self.store
            .record_metric(&scan_id, "enumerate", enumerate_ms, enumerated)?;
        self.store
            .record_metric(&scan_id, "tag_read", tag_read_ms, total)?;
        self.store
            .record_metric(&scan_id, "db_write", db_write_ms, total)?;
        Ok(ScanResult {
            scan_id,
            files: total,
            cache_hits: hits,
            warnings,
        })
    }
}

#[allow(clippy::large_enum_variant)]
enum ScanWorkerResult {
    Record {
        file: ScannedFile,
        cache_hit: bool,
        warning: bool,
        tag_read_ms: u64,
    },
    Warning(String),
}

fn spawn_scan_worker<
    F: FileSystem + 'static,
    M: MetadataReader + 'static,
    S: ScanStore + 'static,
>(
    fs: Arc<F>,
    metadata: Arc<M>,
    store: Arc<S>,
    paths: Receiver<Result<PathBuf, String>>,
    output: crossbeam_channel::Sender<ScanWorkerResult>,
    cancellation: CancellationToken,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while let Ok(path) = paths.recv() {
            if cancellation.is_cancelled() {
                break;
            }
            let path = match path {
                Ok(value) => value,
                Err(error) => {
                    let _ = output.send(ScanWorkerResult::Warning(error));
                    continue;
                }
            };
            let fingerprint = match fs.fingerprint(&path) {
                Ok(value) => value,
                Err(error) => {
                    let _ = output.send(ScanWorkerResult::Warning(format!(
                        "fingerprint_failed:{}:{error}",
                        path.display()
                    )));
                    continue;
                }
            };
            let is_image = is_image_path(&path);
            let cached = if is_image {
                None
            } else {
                match store.previous_metadata(&path, &fingerprint) {
                    Ok(value) => value,
                    Err(error) => {
                        let warning =
                            format!("metadata_cache_read_failed:{}:{error}", path.display());
                        let _ = output.send(ScanWorkerResult::Warning(warning));
                        None
                    }
                }
            };
            let tag_started = Instant::now();
            let cache_hit = cached.is_some();
            let mut read_error_reported = false;
            let metadata = if is_image {
                None
            } else if cached.is_some() {
                cached
            } else {
                match metadata.read(&path) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        read_error_reported = true;
                        let warning = format!("metadata_read_failed:{}:{error}", path.display());
                        let _ = output.send(ScanWorkerResult::Warning(warning));
                        None
                    }
                }
            };
            // Do not store tags under a fingerprint from before a concurrent edit.
            if !is_image && !cache_hit && metadata.is_some() {
                let validation = fs.fingerprint(&path).and_then(|after| {
                    if after.size_bytes == fingerprint.size_bytes
                        && after.mtime_ns == fingerprint.mtime_ns
                        && after.version == fingerprint.version
                        && after.file_identity == fingerprint.file_identity
                        && after.content_sha256 == fingerprint.content_sha256
                    {
                        Ok(())
                    } else {
                        Err("fingerprint_changed".into())
                    }
                });
                if let Err(error) = validation {
                    let _ = output.send(ScanWorkerResult::Warning(format!(
                        "source_changed_during_metadata:{}:{error}",
                        path.display()
                    )));
                    continue;
                }
            }
            let warning = !is_image && metadata.is_none() && !read_error_reported;
            let _ = output.send(ScanWorkerResult::Record {
                file: ScannedFile {
                    id: Uuid::new_v4(),
                    path,
                    fingerprint,
                    metadata,
                    kind: if is_image {
                        FileKind::Image
                    } else {
                        FileKind::Music
                    },
                },
                cache_hit,
                warning,
                tag_read_ms: tag_started.elapsed().as_millis() as u64,
            });
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn consume_scan_results(
    results: &Receiver<ScanWorkerResult>,
    batch: &mut Vec<ScannedFile>,
    hits: &mut u64,
    warnings: &mut u64,
    total: &mut u64,
    batch_size: usize,
    mut flush: impl FnMut(&[ScannedFile]) -> Result<(), String>,
    mut save_warning: impl FnMut(&str) -> Result<(), String>,
    cancellation: &CancellationToken,
    progress: Option<&ProgressSink>,
    scan_id: &str,
    started: &Instant,
    tag_read_ms: &mut u64,
    enumerated_count: &AtomicU64,
) -> Result<(), String> {
    while let Ok(result) = results.recv() {
        match result {
            ScanWorkerResult::Warning(warning) => {
                *warnings += 1;
                save_warning(&warning)?;
            }
            ScanWorkerResult::Record {
                file,
                cache_hit,
                warning,
                tag_read_ms: elapsed,
            } => {
                *hits += u64::from(cache_hit);
                *warnings += u64::from(warning);
                *total += 1;
                *tag_read_ms += elapsed;
                batch.push(file);
            }
        }
        if batch.len() >= batch_size {
            flush(batch)?;
            batch.clear();
        }
        if let Some(sink) = progress {
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let items_per_second = if elapsed_ms == 0 {
                0.0
            } else {
                *total as f64 * 1000.0 / elapsed_ms as f64
            };
            let enumerated = enumerated_count.load(Ordering::Acquire);
            let eta_seconds = if items_per_second > 0.0 && enumerated > *total {
                Some(((enumerated - *total) as f64 / items_per_second).ceil() as u64)
            } else {
                None
            };
            sink(ScanProgress {
                scan_id: scan_id.into(),
                phase: "db_write".into(),
                enumerated,
                processed: *total,
                cache_hits: *hits,
                warnings: *warnings,
                elapsed_ms,
                items_per_second,
                eta_seconds,
            });
        }
        if cancellation.is_cancelled() {
            continue;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PlanOptions {
    pub target_root: PathBuf,
    pub batch_size: usize,
    pub naming: NamingRules,
}

#[derive(Debug, Clone)]
pub struct PlanResult {
    pub plan_id: String,
    pub items: u64,
    pub conflicts: u64,
    pub risks: u64,
}

pub struct PlanUseCase<S> {
    pub store: Arc<S>,
}

pub struct RevisePlanUseCase<S> {
    pub store: Arc<S>,
}

impl<S: PlanRevisionStore> RevisePlanUseCase<S> {
    pub fn execute(
        &self,
        parent_plan_id: &str,
        changes: &[ManualTargetChange],
    ) -> WorkflowResult<String> {
        if changes.is_empty() {
            return Err("manual_target_change_required".into());
        }
        execute_bounded_plan_revision(self.store.as_ref(), parent_plan_id, changes, 512)
    }
}

impl<S: PlanStore> PlanUseCase<S> {
    pub fn execute(&self, scan_id: &str, options: &PlanOptions) -> WorkflowResult<PlanResult> {
        let issues = crate::validate_naming_rules(&options.naming);
        if !issues.is_empty() {
            return Err(format!("invalid_naming_rules:{}", issues[0].code).into());
        }
        TargetPathPolicy::new(options.naming.allow_long_paths)
            .from_relative_components(&options.target_root, [".target-root-validation"])
            .map_err(|error| format!("invalid_target_root:{}", error.reason_code()))?;
        let started = Instant::now();
        let scan_snapshot_hash = self.store.completed_scan_snapshot_hash(scan_id)?;
        let plan_id = self
            .store
            .begin_plan(scan_id, &options.target_root, &options.naming)?;
        let batch_size = options.batch_size.max(1);
        let build_result = (|| -> Result<PlanResult, String> {
            let mut after_path = None::<PathBuf>;
            let mut ordinal = 0_u64;
            loop {
                let files = self.store.load_completed_scan_page(
                    scan_id,
                    after_path.as_deref(),
                    batch_size,
                )?;
                if files.is_empty() {
                    break;
                }
                let mut staged = Vec::with_capacity(files.len());
                let mut anchors = Vec::new();
                for file in files {
                    after_path = Some(file.path.clone());
                    ordinal = ordinal.saturating_add(1);
                    let item = if file.kind == FileKind::Music {
                        make_plan_item(ordinal, file, &options.target_root, &options.naming)
                    } else {
                        skipped_plan_item(
                            ordinal,
                            file,
                            ExecutionDisposition::Skip,
                            Risk::MetadataMissing,
                            "image_pending_anchor",
                        )
                    };
                    if item.file.kind == FileKind::Music {
                        append_music_anchors(&item, &options.naming, &mut anchors);
                    }
                    staged.push(StagedPlanItem {
                        item,
                        parent_plan_item_id: None,
                        change_origin: None,
                        change_reason: None,
                    });
                }
                self.store.stage_plan_batch(&plan_id, &staged, &anchors)?;
            }

            resolve_staged_images(
                self.store.as_ref(),
                &plan_id,
                &options.target_root,
                &options.naming,
                batch_size,
            )?;
            resolve_staged_duplicate_targets(
                self.store.as_ref(),
                &plan_id,
                &options.naming,
                &options.target_root,
                batch_size,
            )?;
            mark_staged_target_conflicts(self.store.as_ref(), &plan_id, batch_size)?;
            let (items, conflicts, risks, snapshot_hash) = publish_staged_plan(
                self.store.as_ref(),
                &plan_id,
                scan_id,
                &scan_snapshot_hash,
                &options.target_root,
                &options.naming,
                batch_size,
            )?;
            self.store
                .complete_plan_build(&plan_id, conflicts, risks, &snapshot_hash)?;
            Ok(PlanResult {
                plan_id: plan_id.clone(),
                items,
                conflicts,
                risks,
            })
        })();
        let result = match build_result {
            Ok(result) => result,
            Err(error) => {
                let _ = self.store.abort_plan_build(&plan_id, RunStatus::Failed);
                return Err(error.into());
            }
        };
        self.store.record_metric(
            &plan_id,
            "plan",
            started.elapsed().as_millis() as u64,
            result.items,
        )?;
        Ok(result)
    }
}

fn append_music_anchors(item: &PlanItem, naming: &NamingRules, anchors: &mut Vec<PlanMusicAnchor>) {
    let Some(target) = &item.target else {
        return;
    };
    let mut source = item.file.path.parent();
    let target_parent = target.parent().map(Path::to_path_buf);
    let disc_parent = target_parent
        .as_deref()
        .and_then(|directory| disc_parent_for_music_item(directory, &item.file, naming));
    while let (Some(directory), Some(target_parent)) = (source, target_parent.as_ref()) {
        anchors.push(PlanMusicAnchor {
            source_directory: directory.to_path_buf(),
            target_directory: target_parent.clone(),
            disc_parent: disc_parent.clone(),
            music_item_id: item.id,
        });
        source = directory.parent();
    }
}

fn disc_parent_for_music_item(
    target_directory: &Path,
    file: &ScannedFile,
    naming: &NamingRules,
) -> Option<PathBuf> {
    let metadata = file.metadata.as_ref()?;
    let source_stem = file
        .path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("_");
    let extension = file
        .path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let rendered_disc =
        render_template(&naming.disc_dir_template, metadata, source_stem, &extension);
    let rendered_disc = rendered_disc.trim_matches([' ', '.']);
    if rendered_disc.is_empty() {
        return None;
    }
    let disc_component = sanitize_component(rendered_disc);
    if target_directory.file_name() != Some(OsStr::new(&disc_component)) {
        return None;
    }
    target_directory.parent().map(Path::to_path_buf)
}

fn image_destination_candidates(mut anchors: Vec<PlanMusicAnchor>) -> Vec<PlanConflictCandidate> {
    anchors.sort_by(|left, right| {
        left.target_directory
            .cmp(&right.target_directory)
            .then(left.music_item_id.cmp(&right.music_item_id))
    });
    anchors.dedup_by(|left, right| {
        left.target_directory == right.target_directory && left.music_item_id == right.music_item_id
    });

    let mut candidates = Vec::<PlanConflictCandidate>::new();
    for anchor in &anchors {
        if let Some(candidate) = candidates
            .iter_mut()
            .find(|candidate| candidate.target_directory == anchor.target_directory)
        {
            candidate.music_item_ids.push(anchor.music_item_id);
        } else {
            candidates.push(PlanConflictCandidate {
                target_directory: anchor.target_directory.clone(),
                music_item_ids: vec![anchor.music_item_id],
            });
        }
    }

    if candidates.len() <= 1 {
        return candidates;
    }
    let Some(common_parent) = anchors
        .first()
        .and_then(|anchor| anchor.disc_parent.clone())
    else {
        return candidates;
    };
    if anchors.iter().any(|anchor| {
        anchor.disc_parent.as_ref() != Some(&common_parent)
            || anchor.target_directory.parent() != Some(common_parent.as_path())
    }) {
        return candidates;
    }

    let mut music_item_ids = anchors
        .into_iter()
        .map(|anchor| anchor.music_item_id)
        .collect::<Vec<_>>();
    music_item_ids.sort();
    music_item_ids.dedup();
    vec![PlanConflictCandidate {
        target_directory: common_parent,
        music_item_ids,
    }]
}

fn resolve_staged_images<S: PlanBuildStore + ?Sized>(
    store: &S,
    build_id: &str,
    target_root: &Path,
    naming: &NamingRules,
    batch_size: usize,
) -> Result<(), String> {
    let mut after_ordinal = 0_u64;
    loop {
        let page = store.load_staged_plan_page(build_id, after_ordinal, batch_size)?;
        if page.is_empty() {
            break;
        }
        let mut changed = Vec::new();
        for mut staged in page {
            after_ordinal = checked_next_ordinal(after_ordinal, staged.item.ordinal)?;
            if staged.item.file.kind != FileKind::Image {
                continue;
            }
            let ancestors = staged
                .item
                .file
                .path
                .ancestors()
                .skip(1)
                .map(Path::to_path_buf)
                .collect::<Vec<_>>();
            let candidates = image_destination_candidates(
                store.load_nearest_music_anchors(build_id, &ancestors)?,
            );
            match candidates.as_slice() {
                [candidate] => {
                    let name = staged
                        .item
                        .file
                        .path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("image");
                    let candidate_target =
                        candidate.target_directory.join(sanitize_component(name));
                    match SafeTargetPath::from_completed_target(
                        target_root,
                        &candidate_target,
                        naming.allow_long_paths,
                    ) {
                        Ok(target) => {
                            staged.item.target = Some(target.into_path_buf());
                            staged.item.set_outcome(
                                PlanAction::Move,
                                ExecutionDisposition::Executable,
                                Risk::None,
                                None,
                            );
                        }
                        Err(error) => {
                            mark_invalid_target(&mut staged.item, candidate_target, error)
                        }
                    }
                }
                [] => {
                    staged.item.set_outcome(
                        PlanAction::Skip,
                        ExecutionDisposition::Skip,
                        Risk::MetadataMissing,
                        Some("companion_without_music".into()),
                    );
                }
                _ => {
                    staged.item.conflict_group_id = Some(deterministic_conflict_group_id(
                        &format!("image:{}", staged.item.ordinal),
                    ));
                    staged.item.conflict_candidates = candidates;
                    staged.item.set_outcome(
                        PlanAction::Skip,
                        ExecutionDisposition::Blocked,
                        Risk::Conflict,
                        Some("companion_target_ambiguous".into()),
                    );
                }
            }
            changed.push(staged);
        }
        if !changed.is_empty() {
            store.update_staged_plan_batch(build_id, &changed, true, &[])?;
        }
    }
    Ok(())
}

fn resolve_staged_duplicate_targets<S: PlanBuildStore + ?Sized>(
    store: &S,
    build_id: &str,
    naming: &NamingRules,
    target_root: &Path,
    batch_size: usize,
) -> Result<(), String> {
    let mut after_ordinal = 0_u64;
    loop {
        let mut page = store.load_staged_plan_page(build_id, after_ordinal, batch_size)?;
        if page.is_empty() {
            break;
        }
        let mut page_allocated = HashSet::<String>::with_capacity(page.len());
        for staged in &mut page {
            after_ordinal = checked_next_ordinal(after_ordinal, staged.item.ordinal)?;
            if staged.item.action != PlanAction::Move {
                continue;
            }
            let Some(target) = staged.item.target.clone() else {
                continue;
            };
            let original_key = windows_path_key(&target);
            let rank = store.original_target_rank(build_id, &original_key, staged.item.ordinal)?;
            let already_allocated = page_allocated.contains(&original_key)
                || store.target_is_allocated(build_id, &original_key)?;
            if rank == 1 && !already_allocated {
                page_allocated.insert(original_key);
                continue;
            }
            if (naming.duplicate_strategy == DuplicateStrategy::Skip
                || (naming.duplicate_strategy == DuplicateStrategy::Legacy
                    && naming.duplicate_suffix_template.is_empty()))
                && staged.item.file.kind != FileKind::Image
            {
                continue;
            }
            let metadata_fallback = TrackMetadata {
                artist: None,
                album_artist: None,
                album: None,
                title: None,
                track_no: None,
                disc_no: None,
                year: None,
                genre: None,
                has_artwork: None,
            };
            let metadata = staged
                .item
                .file
                .metadata
                .as_ref()
                .unwrap_or(&metadata_fallback);
            let mut candidate_number = rank.max(2);
            let next = loop {
                let suffix = if staged.item.file.kind == FileKind::Image
                    || naming.duplicate_strategy == DuplicateStrategy::Sequence
                {
                    format!("_{candidate_number}")
                } else {
                    let rendered = render_template(
                        &naming.duplicate_suffix_template,
                        metadata,
                        staged
                            .item
                            .file
                            .path
                            .file_stem()
                            .and_then(|value| value.to_str())
                            .unwrap_or("_"),
                        "",
                    );
                    if rendered.is_empty() {
                        format!("_{candidate_number}")
                    } else if candidate_number == 2 {
                        rendered
                    } else {
                        format!("{rendered}_{candidate_number}")
                    }
                };
                let filename = sanitize_component(&format!(
                    "{}{}{}",
                    target
                        .file_stem()
                        .and_then(|value| value.to_str())
                        .unwrap_or("_"),
                    suffix,
                    target
                        .extension()
                        .and_then(|value| value.to_str())
                        .map(|value| format!(".{value}"))
                        .unwrap_or_default()
                ));
                let candidate = target.with_file_name(filename);
                let candidate_key = windows_path_key(&candidate);
                let reserved_by_future = store.original_target_exists_after(
                    build_id,
                    &candidate_key,
                    staged.item.ordinal,
                )?;
                let allocated = page_allocated.contains(&candidate_key)
                    || store.target_is_allocated(build_id, &candidate_key)?;
                if !allocated && !reserved_by_future {
                    break candidate;
                }
                candidate_number = candidate_number.saturating_add(1);
            };
            match SafeTargetPath::from_completed_target(target_root, &next, naming.allow_long_paths)
            {
                Ok(next) => {
                    page_allocated.insert(next.comparison_key().as_str().to_owned());
                    staged.item.target = Some(next.into_path_buf());
                }
                Err(error) => mark_invalid_target(&mut staged.item, next, error),
            }
        }
        let allocated = page_allocated.into_iter().collect::<Vec<_>>();
        store.update_staged_plan_batch(build_id, &page, false, &allocated)?;
    }
    Ok(())
}

fn mark_staged_target_conflicts<S: PlanBuildStore + ?Sized>(
    store: &S,
    build_id: &str,
    batch_size: usize,
) -> Result<(), String> {
    let mut after_ordinal = 0_u64;
    loop {
        let mut page = store.load_staged_plan_page(build_id, after_ordinal, batch_size)?;
        if page.is_empty() {
            break;
        }
        for staged in &mut page {
            after_ordinal = checked_next_ordinal(after_ordinal, staged.item.ordinal)?;
            if staged.item.action != PlanAction::Move {
                continue;
            }
            let Some(target) = &staged.item.target else {
                continue;
            };
            let target_key = windows_path_key(target);
            if store.staged_target_count(build_id, &target_key)? > 1 {
                staged.item.conflict_group_id = Some(deterministic_conflict_group_id(&target_key));
                staged.item.set_outcome(
                    PlanAction::Skip,
                    ExecutionDisposition::Blocked,
                    Risk::Conflict,
                    Some("target_conflict".into()),
                );
            }
        }
        store.update_staged_plan_batch(build_id, &page, false, &[])?;
    }
    Ok(())
}

fn publish_staged_plan<S: PlanBuildStore + ?Sized>(
    store: &S,
    build_id: &str,
    scan_id: &str,
    scan_snapshot_hash: &str,
    target_root: &Path,
    naming: &NamingRules,
    batch_size: usize,
) -> Result<(u64, u64, u64, String), String> {
    let expected_items = store.staged_plan_item_count(build_id)?;
    let mut hasher = PlanAuthorizationHasher::new(
        scan_id,
        scan_snapshot_hash,
        target_root,
        naming,
        expected_items,
    );
    let mut after_ordinal = 0_u64;
    let mut items = 0_u64;
    let mut conflicts = 0_u64;
    let mut risks = 0_u64;
    loop {
        let page = store.publish_staged_plan_page(build_id, after_ordinal, batch_size)?;
        if page.is_empty() {
            break;
        }
        for item in page {
            after_ordinal = checked_next_ordinal(after_ordinal, item.ordinal)?;
            hasher.update(&item);
            items = items.saturating_add(1);
            conflicts = conflicts.saturating_add(u64::from(item.risk == Risk::Conflict));
            risks = risks.saturating_add(u64::from(item.risk != Risk::None));
        }
    }
    let snapshot_hash = hasher.finish()?;
    Ok((items, conflicts, risks, snapshot_hash))
}

fn checked_next_ordinal(previous: u64, next: u64) -> Result<u64, String> {
    if next <= previous {
        Err(format!("plan_page_not_strictly_ordered:{previous}:{next}"))
    } else {
        Ok(next)
    }
}

fn deterministic_conflict_group_id(key: &str) -> Uuid {
    let digest = Sha256::digest(key.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

pub fn execute_bounded_plan_revision<S: PlanRevisionStore + ?Sized>(
    store: &S,
    parent_plan_id: &str,
    changes: &[ManualTargetChange],
    batch_size: usize,
) -> WorkflowResult<String> {
    if changes.is_empty() {
        return Err("manual_target_change_required".into());
    }
    let batch_size = batch_size.max(1);
    let mut overrides = HashMap::<&str, &ManualTargetChange>::with_capacity(changes.len());
    for change in changes {
        if overrides
            .insert(change.plan_item_id.as_str(), change)
            .is_some()
        {
            return Err("duplicate_plan_revision_change".into());
        }
    }
    let build = store.begin_plan_revision(parent_plan_id)?;
    let result = (|| -> Result<String, String> {
        let mut seen_changes = HashSet::<String>::with_capacity(changes.len());
        let mut after_ordinal = 0_u64;
        loop {
            let page = store.load_parent_plan_page(parent_plan_id, after_ordinal, batch_size)?;
            if page.is_empty() {
                break;
            }
            let mut staged_page = Vec::with_capacity(page.len());
            for parent in page {
                after_ordinal = checked_next_ordinal(after_ordinal, parent.item.ordinal)?;
                let parent_item_id = parent
                    .parent_plan_item_id
                    .as_deref()
                    .ok_or_else(|| "parent_plan_item_id_missing".to_string())?;
                let change = overrides.get(parent_item_id).copied();
                if change.is_some() {
                    seen_changes.insert(parent_item_id.to_owned());
                }
                staged_page.push(revise_staged_plan_item(
                    parent,
                    change,
                    &build.target_root,
                    &build.naming,
                ));
            }
            store.stage_plan_batch(&build.build_id, &staged_page, &[])?;
        }
        if seen_changes.len() != overrides.len() {
            return Err("plan_revision_item_not_found".into());
        }
        resolve_staged_duplicate_targets(
            store,
            &build.build_id,
            &build.naming,
            &build.target_root,
            batch_size,
        )?;
        mark_staged_target_conflicts(store, &build.build_id, batch_size)?;
        let (_, conflicts, risks, snapshot_hash) = publish_staged_plan(
            store,
            &build.build_id,
            &build.scan_id,
            &build.scan_snapshot_hash,
            &build.target_root,
            &build.naming,
            batch_size,
        )?;
        store.complete_plan_build(&build.build_id, conflicts, risks, &snapshot_hash)?;
        Ok(build.child_plan_id.clone())
    })();
    match result {
        Ok(plan_id) => Ok(plan_id),
        Err(error) => {
            let _ = store.abort_plan_build(&build.build_id, RunStatus::Failed);
            Err(error.into())
        }
    }
}

fn revise_staged_plan_item(
    mut parent: StagedPlanItem,
    change: Option<&ManualTargetChange>,
    target_root: &Path,
    naming: &NamingRules,
) -> StagedPlanItem {
    let source_path = parent.item.file.path.clone();
    let old_target = parent.item.target.take();
    parent.item.id = Uuid::new_v4();
    parent.item.conflict_group_id = None;
    if parent.item.risk == Risk::Conflict
        && parent.item.reason.as_deref() == Some("target_conflict")
    {
        parent.item.set_outcome(
            PlanAction::Move,
            ExecutionDisposition::Executable,
            Risk::None,
            None,
        );
    }
    let (target, origin, change_reason) = if let Some(change) = change {
        if windows_path_key(&source_path) == windows_path_key(&change.target) {
            parent.item.set_outcome(
                PlanAction::Skip,
                ExecutionDisposition::Blocked,
                Risk::InvalidTarget,
                Some("source_equals_target".into()),
            );
            (None, "manual", change.reason.clone())
        } else {
            match SafeTargetPath::from_manual_absolute_target(
                target_root,
                &change.target,
                naming.allow_long_paths,
            ) {
                Ok(safe_target) => {
                    parent.item.set_outcome(
                        PlanAction::Move,
                        ExecutionDisposition::Executable,
                        Risk::None,
                        Some(change.reason.clone()),
                    );
                    (
                        Some(safe_target.into_path_buf()),
                        "manual",
                        change.reason.clone(),
                    )
                }
                Err(error) => {
                    let risk = if matches!(error, DomainError::PathTooLong { .. }) {
                        Risk::PathTooLong
                    } else {
                        Risk::InvalidTarget
                    };
                    parent.item.set_outcome(
                        PlanAction::Skip,
                        ExecutionDisposition::Blocked,
                        risk,
                        Some(error.reason_code()),
                    );
                    (None, "manual", change.reason.clone())
                }
            }
        }
    } else {
        match old_target
            .map(|target| {
                SafeTargetPath::from_completed_target(target_root, &target, naming.allow_long_paths)
                    .map(SafeTargetPath::into_path_buf)
            })
            .transpose()
        {
            Ok(target) => (target, "rule", "unchanged".into()),
            Err(error) => {
                let risk = if matches!(error, DomainError::PathTooLong { .. }) {
                    Risk::PathTooLong
                } else {
                    Risk::InvalidTarget
                };
                parent.item.set_outcome(
                    PlanAction::Skip,
                    ExecutionDisposition::Blocked,
                    risk,
                    Some(error.reason_code()),
                );
                (None, "rule", "revalidated".into())
            }
        }
    };
    parent.item.target = target;
    parent.change_origin = Some(origin.into());
    parent.change_reason = Some(change_reason);
    parent
}

/// Stable digest over the fields which authorize a filesystem mutation.  UUIDs and
/// display-only metadata are intentionally excluded, so SQLite can recalculate it.
pub fn plan_snapshot_hash(items: &[PlanItem]) -> String {
    crate::snapshot::plan_snapshot_hash(items)
}

fn make_plan_item(
    ordinal: u64,
    file: ScannedFile,
    target_root: &Path,
    naming: &NamingRules,
) -> PlanItem {
    let metadata_unreadable = file.metadata.is_none();
    let mut metadata = file.metadata.clone().unwrap_or(TrackMetadata {
        artist: None,
        album_artist: None,
        album: None,
        title: None,
        track_no: None,
        disc_no: None,
        year: None,
        genre: None,
        has_artwork: None,
    });
    let artist_missing = metadata
        .album_artist
        .as_deref()
        .or(metadata.artist.as_deref())
        .is_none();
    let album_missing = metadata.album.is_none();
    let missing_reason = if metadata_unreadable {
        Some("metadata_missing")
    } else if artist_missing && album_missing {
        Some("artist_album_missing")
    } else if artist_missing {
        Some("artist_missing")
    } else if album_missing {
        Some("album_missing")
    } else {
        None
    };
    if let Some(reason) = missing_reason.filter(|_| !naming.allow_missing_metadata) {
        return skipped_plan_item(
            ordinal,
            file,
            ExecutionDisposition::Blocked,
            Risk::MetadataMissing,
            reason,
        );
    }
    if artist_missing {
        metadata.artist = Some("Unknown Artist".into());
        metadata.album_artist = Some("Unknown Artist".into());
    }
    if album_missing {
        metadata.album = Some("Unknown Album".into());
    }
    let source_stem = file
        .path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("_");
    let extension = file
        .path
        .extension()
        .and_then(|value| value.to_str())
        .map(|v| format!(".{v}"))
        .unwrap_or_default();
    let filename = if naming.use_source_filename || metadata_unreadable {
        file.path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("_")
            .into()
    } else {
        render_template(
            &naming.filename_template,
            &metadata,
            source_stem,
            &extension,
        )
    };
    let artist = sanitize_component(&render_template(
        &naming.artist_dir_template,
        &metadata,
        source_stem,
        &extension,
    ));
    let album = sanitize_component(&render_template(
        &naming.album_dir_template,
        &metadata,
        source_stem,
        &extension,
    ));
    let rendered_disc = render_template(
        &naming.disc_dir_template,
        &metadata,
        source_stem,
        &extension,
    );
    let rendered_disc = rendered_disc.trim_matches([' ', '.']);
    let mut components = vec![artist, album];
    if !rendered_disc.is_empty() {
        components.push(sanitize_component(rendered_disc));
    }
    components.push(sanitize_component(&filename));
    let attempted_target = components
        .iter()
        .fold(target_root.to_path_buf(), |path, component| {
            path.join(component)
        });
    let target_result = SafeTargetPath::from_relative_components(
        target_root,
        components.iter().map(String::as_str),
        naming.allow_long_paths,
    );
    let (target, action, disposition, risk, reason) = match target_result {
        Ok(target) if windows_path_key(target.as_path()) == windows_path_key(&file.path) => (
            Some(target.into_path_buf()),
            PlanAction::Skip,
            ExecutionDisposition::Blocked,
            Risk::InvalidTarget,
            Some("source_equals_target".into()),
        ),
        Ok(target) => (
            Some(target.into_path_buf()),
            PlanAction::Move,
            ExecutionDisposition::Executable,
            if missing_reason.is_some() {
                Risk::MetadataMissing
            } else {
                Risk::None
            },
            missing_reason.map(str::to_owned),
        ),
        Err(error @ DomainError::PathTooLong { .. }) => (
            Some(attempted_target),
            PlanAction::Skip,
            ExecutionDisposition::Blocked,
            Risk::PathTooLong,
            Some(error.reason_code()),
        ),
        Err(error) => (
            Some(attempted_target),
            PlanAction::Skip,
            ExecutionDisposition::Blocked,
            Risk::InvalidTarget,
            Some(error.reason_code()),
        ),
    };
    let issues = crate::PlanIssue::from_diagnostic(risk, reason.as_deref(), disposition)
        .into_iter()
        .collect();
    PlanItem {
        id: Uuid::new_v4(),
        conflict_group_id: None,
        ordinal,
        file,
        target,
        action,
        disposition,
        risk,
        reason,
        issues,
        conflict_candidates: Vec::new(),
    }
}

fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|v| v.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp")
    )
}

#[cfg(test)]
fn resolve_duplicate_targets(items: &mut [PlanItem], naming: &NamingRules, target_root: &Path) {
    let mut future = HashMap::<String, u32>::new();
    for item in items.iter().filter(|item| item.action == PlanAction::Move) {
        if let Some(target) = &item.target {
            *future.entry(windows_path_key(target)).or_default() += 1;
        }
    }
    let mut allocated = HashSet::<String>::new();
    let mut seen = HashMap::<String, u32>::new();
    for item in items.iter_mut().filter(|i| i.action == PlanAction::Move) {
        let Some(target) = item.target.clone() else {
            continue;
        };
        let key = windows_path_key(&target);
        if let Some(remaining) = future.get_mut(&key) {
            *remaining = remaining.saturating_sub(1);
        }
        let count = seen.entry(key.clone()).or_insert(0);
        *count += 1;
        if *count == 1 && !allocated.contains(&key) {
            allocated.insert(key);
            continue;
        }
        if *count > 1 {
            if (naming.duplicate_strategy == DuplicateStrategy::Skip
                || (naming.duplicate_strategy == DuplicateStrategy::Legacy
                    && naming.duplicate_suffix_template.is_empty()))
                && item.file.kind != FileKind::Image
            {
                // Keep every candidate movable until `mark_target_conflicts` so
                // all sides of the collision receive the same diagnostic group.
                continue;
            }
            let metadata_fallback = TrackMetadata {
                artist: None,
                album_artist: None,
                album: None,
                title: None,
                track_no: None,
                disc_no: None,
                year: None,
                genre: None,
                has_artwork: None,
            };
            let metadata = item.file.metadata.as_ref().unwrap_or(&metadata_fallback);
            let mut candidate_number = (*count).max(2);
            let next = loop {
                let suffix = if item.file.kind == FileKind::Image
                    || naming.duplicate_strategy == DuplicateStrategy::Sequence
                {
                    format!("_{candidate_number}")
                } else {
                    let rendered = render_template(
                        &naming.duplicate_suffix_template,
                        metadata,
                        item.file
                            .path
                            .file_stem()
                            .and_then(|v| v.to_str())
                            .unwrap_or("_"),
                        "",
                    );
                    if rendered.is_empty() {
                        format!("_{candidate_number}")
                    } else if candidate_number == 2 {
                        rendered
                    } else {
                        format!("{rendered}_{candidate_number}")
                    }
                };
                let filename = sanitize_component(&format!(
                    "{}{}{}",
                    target.file_stem().and_then(|v| v.to_str()).unwrap_or("_"),
                    suffix,
                    target
                        .extension()
                        .and_then(|v| v.to_str())
                        .map(|v| format!(".{v}"))
                        .unwrap_or_default()
                ));
                let candidate = target.with_file_name(filename);
                let candidate_key = windows_path_key(&candidate);
                let reserved_by_future = future.get(&candidate_key).copied().unwrap_or(0) > 0;
                if !allocated.contains(&candidate_key) && !reserved_by_future {
                    break candidate;
                }
                candidate_number += 1;
            };
            match SafeTargetPath::from_completed_target(target_root, &next, naming.allow_long_paths)
            {
                Ok(next) => {
                    allocated.insert(next.comparison_key().as_str().to_owned());
                    item.target = Some(next.into_path_buf());
                }
                Err(error) => mark_invalid_target(item, next, error),
            }
        }
    }
}

fn mark_invalid_target(item: &mut PlanItem, attempted_target: PathBuf, error: DomainError) {
    item.target = Some(attempted_target);
    let risk = if matches!(error, DomainError::PathTooLong { .. }) {
        Risk::PathTooLong
    } else {
        Risk::InvalidTarget
    };
    item.set_outcome(
        PlanAction::Skip,
        ExecutionDisposition::Blocked,
        risk,
        Some(error.reason_code()),
    );
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use crate::{FileFingerprint, TrackMetadata};

    #[test]
    fn plan_snapshot_hash_changes_when_authorized_target_changes() {
        let file = ScannedFile {
            id: Uuid::nil(),
            path: PathBuf::from("C:/in/a.mp3"),
            fingerprint: FileFingerprint::legacy(1, 1),
            metadata: Some(TrackMetadata {
                artist: None,
                album_artist: None,
                album: None,
                title: None,
                track_no: None,
                disc_no: None,
                year: None,
                genre: None,
                has_artwork: None,
            }),
            kind: FileKind::Music,
        };
        let mut item = PlanItem {
            id: Uuid::nil(),
            conflict_group_id: None,
            ordinal: 1,
            file,
            target: Some(PathBuf::from("C:/out/a.mp3")),
            action: PlanAction::Move,
            disposition: ExecutionDisposition::Executable,
            risk: Risk::None,
            reason: None,
            issues: Vec::new(),
            conflict_candidates: Vec::new(),
        };
        let before = plan_snapshot_hash(&[item.clone()]);
        item.target = Some(PathBuf::from("C:/out/b.mp3"));
        assert_ne!(before, plan_snapshot_hash(&[item]));
    }

    #[test]
    fn plan_snapshot_hash_ignores_diagnostic_conflict_candidates() {
        let file = ScannedFile {
            id: Uuid::nil(),
            path: PathBuf::from("C:/in/cover.jpg"),
            fingerprint: FileFingerprint::legacy(1, 1),
            metadata: None,
            kind: FileKind::Image,
        };
        let mut item = PlanItem {
            id: Uuid::nil(),
            conflict_group_id: Some(Uuid::nil()),
            ordinal: 1,
            file,
            target: None,
            action: PlanAction::Skip,
            disposition: ExecutionDisposition::Blocked,
            risk: Risk::Conflict,
            reason: Some("companion_target_ambiguous".into()),
            issues: vec![crate::PlanIssue::from_diagnostic(
                Risk::Conflict,
                Some("companion_target_ambiguous"),
                ExecutionDisposition::Blocked,
            )
            .unwrap()],
            conflict_candidates: Vec::new(),
        };
        let before = plan_snapshot_hash(&[item.clone()]);
        item.conflict_candidates.push(PlanConflictCandidate {
            target_directory: PathBuf::from("C:/out/Album"),
            music_item_ids: vec![Uuid::new_v4()],
        });
        assert_eq!(before, plan_snapshot_hash(&[item]));
    }

    #[test]
    fn missing_metadata_is_skipped_unless_explicitly_allowed() {
        let file = ScannedFile {
            id: Uuid::nil(),
            path: PathBuf::from("C:/in/song.mp3"),
            fingerprint: FileFingerprint::legacy(1, 1),
            metadata: None,
            kind: FileKind::Music,
        };
        let skipped = make_plan_item(
            1,
            file.clone(),
            Path::new("C:/out"),
            &NamingRules::default(),
        );
        assert_eq!(skipped.action, PlanAction::Skip);
        assert_eq!(skipped.risk, Risk::MetadataMissing);
        assert_eq!(skipped.disposition, ExecutionDisposition::Blocked);
        assert_eq!(skipped.issues[0].severity, crate::IssueSeverity::Blocking);
        assert!(skipped.target.is_none());

        let allowed = make_plan_item(
            1,
            file,
            Path::new("C:/out"),
            &NamingRules {
                allow_missing_metadata: true,
                ..NamingRules::default()
            },
        );
        assert_eq!(allowed.action, PlanAction::Move);
        assert_eq!(allowed.risk, Risk::MetadataMissing);
        assert_eq!(allowed.disposition, ExecutionDisposition::Executable);
        assert_eq!(allowed.issues[0].severity, crate::IssueSeverity::Warning);
        assert_eq!(
            allowed.target,
            Some(PathBuf::from(
                "C:/out/Unknown Artist/Unknown Album/song.mp3"
            ))
        );
    }

    #[test]
    fn disc_template_and_duplicate_suffix_cannot_escape_target_root() {
        let file = ScannedFile {
            id: Uuid::nil(),
            path: PathBuf::from("C:/in/song.mp3"),
            fingerprint: FileFingerprint::legacy(1, 1),
            metadata: Some(TrackMetadata {
                artist: Some("Artist".into()),
                album_artist: Some("Artist".into()),
                album: Some("Album".into()),
                title: Some("Song".into()),
                track_no: Some(1),
                disc_no: Some(1),
                year: None,
                genre: None,
                has_artwork: None,
            }),
            kind: FileKind::Music,
        };
        let naming = NamingRules {
            disc_dir_template: "../../outside".into(),
            ..NamingRules::default()
        };
        let item = make_plan_item(1, file, Path::new("C:/out"), &naming);
        assert_eq!(item.action, PlanAction::Move);
        let target = item.target.unwrap();
        assert!(SafeTargetPath::from_completed_target("C:/out".as_ref(), &target, false).is_ok());
        assert!(!target.to_string_lossy().contains("/../"));
    }

    #[test]
    fn duplicate_template_with_missing_metadata_does_not_panic() {
        let file = ScannedFile {
            id: Uuid::nil(),
            path: PathBuf::from("C:/in/song.mp3"),
            fingerprint: FileFingerprint::legacy(1, 1),
            metadata: None,
            kind: FileKind::Music,
        };
        let naming = NamingRules {
            allow_missing_metadata: true,
            duplicate_strategy: DuplicateStrategy::Template,
            duplicate_suffix_template: "{artist}".into(),
            ..NamingRules::default()
        };
        let mut items = vec![
            make_plan_item(1, file.clone(), Path::new("C:/out"), &naming),
            make_plan_item(2, file, Path::new("C:/out"), &naming),
        ];
        resolve_duplicate_targets(&mut items, &naming, Path::new("C:/out"));
        assert!(items
            .iter()
            .all(|item| item.execution_disposition() == ExecutionDisposition::Executable));
        assert_ne!(items[0].target, items[1].target);
    }

    #[test]
    fn duplicate_allocator_reserves_future_original_names() {
        let file = |id, target: &str| PlanItem {
            id: Uuid::from_u128(id),
            conflict_group_id: None,
            ordinal: id as u64,
            file: ScannedFile {
                id: Uuid::from_u128(id),
                path: PathBuf::from(format!("C:/in/{id}.mp3")),
                fingerprint: FileFingerprint::legacy(1, 1),
                metadata: None,
                kind: FileKind::Music,
            },
            target: Some(PathBuf::from(target)),
            action: PlanAction::Move,
            disposition: ExecutionDisposition::Executable,
            risk: Risk::None,
            reason: None,
            issues: Vec::new(),
            conflict_candidates: Vec::new(),
        };
        let naming = NamingRules {
            duplicate_strategy: DuplicateStrategy::Sequence,
            ..NamingRules::default()
        };
        let mut items = vec![
            file(1, "C:/out/a.mp3"),
            file(2, "C:/out/a.mp3"),
            file(3, "C:/out/a_2.mp3"),
        ];
        resolve_duplicate_targets(&mut items, &naming, Path::new("C:/out"));
        let names = items
            .iter()
            .map(|item| {
                item.target
                    .as_ref()
                    .and_then(|target| target.file_name())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["a.mp3", "a_3.mp3", "a_2.mp3"]);
    }

    #[test]
    fn image_candidates_collapse_only_proven_disc_directories() {
        let album = PathBuf::from("C:/out/Artist/Album");
        let collapsed = image_destination_candidates(vec![
            PlanMusicAnchor {
                source_directory: PathBuf::from("C:/in"),
                target_directory: album.join("Disc 01"),
                disc_parent: Some(album.clone()),
                music_item_id: Uuid::from_u128(1),
            },
            PlanMusicAnchor {
                source_directory: PathBuf::from("C:/in"),
                target_directory: album.join("Disc 02"),
                disc_parent: Some(album.clone()),
                music_item_id: Uuid::from_u128(2),
            },
        ]);
        assert_eq!(collapsed.len(), 1);
        assert_eq!(collapsed[0].target_directory, album);
        assert_eq!(collapsed[0].music_item_ids.len(), 2);

        let ambiguous = image_destination_candidates(vec![
            PlanMusicAnchor {
                source_directory: PathBuf::from("C:/in"),
                target_directory: PathBuf::from("C:/out/Artist/Album A/01"),
                disc_parent: Some(PathBuf::from("C:/out/Artist/Album A")),
                music_item_id: Uuid::from_u128(1),
            },
            PlanMusicAnchor {
                source_directory: PathBuf::from("C:/in"),
                target_directory: PathBuf::from("C:/out/Artist/Album B/01"),
                disc_parent: Some(PathBuf::from("C:/out/Artist/Album B")),
                music_item_id: Uuid::from_u128(2),
            },
        ]);
        assert_eq!(ambiguous.len(), 2);
    }

    #[test]
    fn disc_parent_requires_a_nonempty_rendered_disc_component() {
        let file = ScannedFile {
            id: Uuid::nil(),
            path: PathBuf::from("C:/in/song.flac"),
            fingerprint: FileFingerprint::legacy(1, 1),
            metadata: Some(TrackMetadata {
                artist: Some("Artist".into()),
                album_artist: Some("Artist".into()),
                album: Some("Album".into()),
                title: Some("Song".into()),
                track_no: Some(1),
                disc_no: Some(2),
                year: None,
                genre: None,
                has_artwork: None,
            }),
            kind: FileKind::Music,
        };
        let custom = NamingRules {
            disc_dir_template: "Disc {disc_no:02d}".into(),
            ..NamingRules::default()
        };
        assert_eq!(
            disc_parent_for_music_item(Path::new("C:/out/Artist/Album/Disc 02"), &file, &custom),
            Some(PathBuf::from("C:/out/Artist/Album"))
        );
        assert_eq!(
            disc_parent_for_music_item(
                Path::new("C:/out/Artist/Album"),
                &file,
                &NamingRules {
                    disc_dir_template: String::new(),
                    ..NamingRules::default()
                }
            ),
            None
        );
    }
}

fn skipped_plan_item(
    ordinal: u64,
    file: ScannedFile,
    disposition: ExecutionDisposition,
    risk: Risk,
    reason: &str,
) -> PlanItem {
    let reason = Some(reason.to_owned());
    let issues = crate::PlanIssue::from_diagnostic(risk, reason.as_deref(), disposition)
        .into_iter()
        .collect();
    PlanItem {
        id: Uuid::new_v4(),
        conflict_group_id: None,
        ordinal,
        file,
        target: None,
        action: PlanAction::Skip,
        disposition,
        risk,
        reason,
        issues,
        conflict_candidates: Vec::new(),
    }
}

pub struct ApplyUseCase<S, F> {
    pub store: Arc<S>,
    pub files: Arc<F>,
}
pub struct ApplyResult {
    pub execution_id: String,
    pub success: u64,
    pub skipped: u64,
    pub failed: u64,
}
pub struct VerifyResult {
    pub verify_id: String,
    pub success: u64,
    pub failed: u64,
}
pub struct VerifyUseCase<S, F> {
    pub store: Arc<S>,
    pub files: Arc<F>,
}
impl<S: VerifyStore, F: FileMutator> VerifyUseCase<S, F> {
    pub fn execute(&self, execution_id: &str) -> WorkflowResult<VerifyResult> {
        self.execute_subject(execution_id, crate::VerifySubject::Execution)
    }

    pub fn execute_subject(
        &self,
        subject_id: &str,
        subject: crate::VerifySubject,
    ) -> WorkflowResult<VerifyResult> {
        let started = Instant::now();
        let verify_id = self.store.begin_verify_subject(subject_id, subject)?;
        let (mut success, mut failed) = (0, 0);
        let items = match self.store.load_verify_subject_items(subject_id, subject) {
            Ok(items) => items,
            Err(error) => {
                let _ = self
                    .store
                    .finish_verify(&verify_id, RunStatus::Failed, success, failed);
                return Err(error.into());
            }
        };
        for item in items {
            let path_matches = |path: &Path, require_identity: bool| {
                self.files
                    .mutation_fingerprint(path)
                    .is_ok_and(|fingerprint| {
                        item.expected_size
                            .is_none_or(|size| fingerprint.size_bytes == size)
                            && item.expected_content_sha256.as_deref().is_none_or(|hash| {
                                fingerprint.content_sha256.as_deref() == Some(hash)
                            })
                            && (!require_identity
                                || item
                                    .expected_file_identity
                                    .as_deref()
                                    .is_none_or(|identity| {
                                        fingerprint.file_identity.as_deref() == Some(identity)
                                    }))
                    })
            };
            let forward_state = || {
                item.target.as_ref().is_some_and(|path| {
                    self.files.exists(path)
                        && path_matches(path, item.action == OperationAction::Move)
                }) && !self.files.exists(&item.source)
            };
            let reverse_state = || {
                self.files.exists(&item.source)
                    && path_matches(&item.source, item.action == OperationAction::Move)
                    && item
                        .target
                        .as_ref()
                        .is_none_or(|target| !self.files.exists(target))
            };
            let expected = match subject {
                crate::VerifySubject::Execution => forward_state(),
                crate::VerifySubject::Rollback => reverse_state(),
                crate::VerifySubject::Recovery
                    if matches!(
                        item.action,
                        OperationAction::Move | OperationAction::CopyDelete
                    ) =>
                {
                    forward_state()
                }
                crate::VerifySubject::Recovery => reverse_state(),
            };
            if expected {
                success += 1;
                self.store.save_verify_result(
                    &verify_id,
                    &item.operation_id,
                    OperationResult::Success,
                    None,
                )?;
            } else {
                failed += 1;
                self.store.save_verify_result(
                    &verify_id,
                    &item.operation_id,
                    OperationResult::Failed,
                    Some("expected_move_state_not_found"),
                )?;
            }
        }
        let status = if failed == 0 {
            RunStatus::Completed
        } else {
            RunStatus::Failed
        };
        self.store
            .finish_verify(&verify_id, status, success, failed)?;
        self.store.record_metric(
            &verify_id,
            "verify",
            started.elapsed().as_millis() as u64,
            success + failed,
        )?;
        Ok(VerifyResult {
            verify_id,
            success,
            failed,
        })
    }
}
impl<S: ApplyStore, F: FileMutator> ApplyUseCase<S, F> {
    pub fn execute(&self, plan_id: &str, dry_run: bool) -> WorkflowResult<ApplyResult> {
        if dry_run {
            return self.execute_with_lease(plan_id, true, None);
        }
        let owner_id = Uuid::new_v4().to_string();
        let lease = self.store.acquire_apply_lease(plan_id, &owner_id)?;
        let result = self.execute_with_lease(plan_id, false, Some(&lease));
        let release = self.store.release_apply_lease(&lease);
        match (result, release) {
            (Ok(result), Ok(())) => Ok(result),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error.into()),
        }
    }

    fn execute_with_lease(
        &self,
        plan_id: &str,
        dry_run: bool,
        lease: Option<&crate::MutationLease>,
    ) -> WorkflowResult<ApplyResult> {
        let started = Instant::now();
        self.store.validate_plan_snapshot(plan_id)?;
        let already_done = if dry_run {
            HashSet::new()
        } else {
            self.store
                .successful_plan_item_ids(plan_id)?
                .into_iter()
                .collect::<HashSet<_>>()
        };
        let execution_id = self.store.begin_execution(plan_id, dry_run)?;
        let (mut success, mut skipped, mut failed) = (0, 0, 0);
        let items = match self.store.load_completed_plan(plan_id) {
            Ok(items) => items,
            Err(error) => {
                let _ = self.store.finish_execution(
                    &execution_id,
                    RunStatus::Failed,
                    success,
                    skipped,
                    failed,
                );
                return Err(error.into());
            }
        };
        let preflight_id = match self.store.begin_preflight(
            &execution_id,
            PreflightKind::Apply,
            PreflightMode::from_dry_run(dry_run),
        ) {
            Ok(preflight_id) => preflight_id,
            Err(error) => {
                let _ = self.store.finish_execution(
                    &execution_id,
                    RunStatus::Failed,
                    success,
                    skipped,
                    failed,
                );
                return Err(error.into());
            }
        };
        let (mut preflight_passed, mut preflight_blocked) = (0, 0);
        for item in items {
            if let Some(lease) = lease {
                if let Err(error) = self.store.heartbeat_apply_lease(lease) {
                    self.terminate_apply_attempt(
                        &execution_id,
                        &preflight_id,
                        RunStatus::Failed,
                        success,
                        skipped,
                        failed,
                        preflight_passed,
                        preflight_blocked,
                    );
                    return Err(error.into());
                }
            }
            let preflight = preflight_apply(self.files.as_ref(), &item);
            let preflight_log = match &preflight {
                Ok(evidence) => {
                    preflight_passed += 1;
                    PreflightLog::passed(
                        item.plan_item_id.clone(),
                        item.source.clone(),
                        item.target.clone(),
                        item.source_fingerprint.clone(),
                        evidence,
                    )
                }
                Err(error) => {
                    preflight_blocked += 1;
                    PreflightLog::blocked(
                        item.plan_item_id.clone(),
                        item.source.clone(),
                        item.target.clone(),
                        item.source_fingerprint.clone(),
                        error.clone(),
                    )
                }
            };
            if let Err(error) =
                self.store
                    .save_preflight_log(&preflight_id, item.ordinal, &preflight_log)
            {
                let _ = self.store.finish_preflight(
                    &preflight_id,
                    PreflightOutcome::Failed,
                    preflight_passed,
                    preflight_blocked,
                );
                let _ = self.store.finish_execution(
                    &execution_id,
                    RunStatus::Failed,
                    success,
                    skipped,
                    failed,
                );
                return Err(error.into());
            }
            let expected_size = preflight.as_ref().ok().map(|value| value.size_bytes);
            let expected_content_sha256 = preflight
                .as_ref()
                .ok()
                .map(|value| value.content_sha256.clone());
            let expected_file_identity = item
                .source_fingerprint
                .as_ref()
                .and_then(|fingerprint| fingerprint.file_identity.clone());
            let (action, result, error, source_deleted) = if already_done
                .contains(&item.plan_item_id)
            {
                skipped += 1;
                (
                    OperationAction::Skip,
                    OperationResult::Skipped,
                    Some("already_applied_for_plan".into()),
                    false,
                )
            } else if item.execution_disposition() != ExecutionDisposition::Executable {
                skipped += 1;
                (
                    OperationAction::Skip,
                    OperationResult::Skipped,
                    item.reason.clone(),
                    false,
                )
            } else if let Err(error) = preflight.as_ref() {
                if error == "target_already_exists" {
                    skipped += 1;
                    (
                        OperationAction::Skip,
                        OperationResult::Skipped,
                        Some("target_already_exists".into()),
                        false,
                    )
                } else {
                    failed += 1;
                    (
                        OperationAction::Move,
                        OperationResult::Failed,
                        Some(error.to_string()),
                        false,
                    )
                }
            } else if dry_run {
                success += 1;
                (
                    OperationAction::DryRun,
                    OperationResult::Success,
                    None,
                    false,
                )
            } else {
                let lease = lease
                    .ok_or_else(|| crate::WorkflowError::from("apply_mutation_lease_missing"))?;
                let move_result: Result<OperationAction, (OperationAction, String)> =
                    match (item.target.as_ref(), preflight) {
                        (Some(target), Ok(expectation)) => self
                            .files
                            .select_move_strategy(&item.source, target)
                            .map_err(|error| (OperationAction::Move, error))
                            .and_then(|strategy| {
                                let failure_action = match strategy {
                                    crate::MutationStrategy::AtomicNoReplaceRename => {
                                        OperationAction::Move
                                    }
                                    crate::MutationStrategy::CopyPublishDelete
                                    | crate::MutationStrategy::ConditionalDeleteOnly => {
                                        OperationAction::CopySourceRetained
                                    }
                                };
                                self.apply_one(
                                    &execution_id,
                                    &item,
                                    target,
                                    &expectation,
                                    strategy,
                                    lease,
                                )
                                .map_err(|error| (failure_action, error))
                            }),
                        (None, _) => Err((
                            OperationAction::Move,
                            "apply_target_missing_after_preflight".into(),
                        )),
                        (_, Err(error)) => Err((
                            OperationAction::Move,
                            format!("apply_preflight_state_changed_before_mutation:{error}"),
                        )),
                    };
                match move_result {
                    Ok(action) => {
                        success += 1;
                        (action, OperationResult::Success, None, true)
                    }
                    Err((action, error)) => {
                        failed += 1;
                        (action, OperationResult::Failed, Some(error), false)
                    }
                }
            };
            let item_requires_recovery = error
                .as_deref()
                .is_some_and(mutation_error_requires_recovery);
            let mutation_was_attempted = !dry_run
                && matches!(
                    action,
                    OperationAction::Move
                        | OperationAction::CopyDelete
                        | OperationAction::CopySourceRetained
                );
            if let Err(save_error) = self.store.save_operation(
                &execution_id,
                &OperationLog {
                    plan_item_id: item.plan_item_id,
                    sequence_no: item.ordinal,
                    source: item.source,
                    target: item.target,
                    action,
                    result,
                    error,
                    source_deleted,
                    expected_size,
                    expected_content_sha256,
                    expected_file_identity,
                },
            ) {
                self.terminate_apply_attempt(
                    &execution_id,
                    &preflight_id,
                    if mutation_was_attempted {
                        RunStatus::RecoveryRequired
                    } else {
                        RunStatus::Failed
                    },
                    success,
                    skipped,
                    failed + 1,
                    preflight_passed,
                    preflight_blocked,
                );
                return Err(save_error.into());
            }
            if item_requires_recovery {
                let preflight_finish = self.store.finish_preflight(
                    &preflight_id,
                    if preflight_blocked == 0 {
                        PreflightOutcome::Passed
                    } else {
                        PreflightOutcome::Blocked
                    },
                    preflight_passed,
                    preflight_blocked,
                );
                let execution_finish = self.store.finish_execution(
                    &execution_id,
                    RunStatus::RecoveryRequired,
                    success,
                    skipped,
                    failed,
                );
                if let Err(error) = preflight_finish {
                    let _ = execution_finish;
                    self.terminate_apply_attempt(
                        &execution_id,
                        &preflight_id,
                        RunStatus::RecoveryRequired,
                        success,
                        skipped,
                        failed,
                        preflight_passed,
                        preflight_blocked,
                    );
                    return Err(error.into());
                }
                if let Err(error) = execution_finish {
                    let _ = self.store.finish_execution(
                        &execution_id,
                        RunStatus::RecoveryRequired,
                        success,
                        skipped,
                        failed,
                    );
                    return Err(error.into());
                }
                return Ok(ApplyResult {
                    execution_id,
                    success,
                    skipped,
                    failed,
                });
            }
        }
        let status = if failed == 0 {
            RunStatus::Completed
        } else {
            RunStatus::Partial
        };
        if let Err(error) = self.store.finish_preflight(
            &preflight_id,
            if preflight_blocked == 0 {
                PreflightOutcome::Passed
            } else {
                PreflightOutcome::Blocked
            },
            preflight_passed,
            preflight_blocked,
        ) {
            self.terminate_apply_attempt(
                &execution_id,
                &preflight_id,
                RunStatus::Failed,
                success,
                skipped,
                failed,
                preflight_passed,
                preflight_blocked,
            );
            return Err(error.into());
        }
        if let Err(error) =
            self.store
                .finish_execution(&execution_id, status, success, skipped, failed)
        {
            let _ = self
                .store
                .finish_execution(&execution_id, status, success, skipped, failed);
            return Err(error.into());
        }
        self.store.record_metric(
            &execution_id,
            "apply",
            started.elapsed().as_millis() as u64,
            success + skipped + failed,
        )?;
        Ok(ApplyResult {
            execution_id,
            success,
            skipped,
            failed,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn terminate_apply_attempt(
        &self,
        execution_id: &str,
        preflight_id: &str,
        status: RunStatus,
        success: u64,
        skipped: u64,
        failed: u64,
        preflight_passed: u64,
        preflight_blocked: u64,
    ) {
        let _ = self.store.finish_preflight(
            preflight_id,
            PreflightOutcome::Failed,
            preflight_passed,
            preflight_blocked,
        );
        let _ = self
            .store
            .finish_execution(execution_id, status, success, skipped, failed);
    }

    fn apply_one(
        &self,
        execution_id: &str,
        item: &crate::ApplyItem,
        target: &Path,
        expectation: &MovePreflight,
        strategy: crate::MutationStrategy,
        lease: &crate::MutationLease,
    ) -> Result<OperationAction, String> {
        let operation_id = Uuid::new_v4().to_string();
        let temporary = if strategy == crate::MutationStrategy::CopyPublishDelete {
            self.files.temporary_path(target, &operation_id)?
        } else {
            PathBuf::new()
        };
        let intent = crate::OperationIntent {
            operation_id: operation_id.clone(),
            attempt_id: execution_id.to_owned(),
            plan_item_id: item.plan_item_id.clone(),
            sequence_no: item.ordinal,
            direction: crate::MutationDirection::Apply,
            strategy,
            source: item.source.clone(),
            target: target.to_path_buf(),
            temporary: temporary.clone(),
            expected_size: Some(expectation.size_bytes),
            expected_content_sha256: Some(expectation.content_sha256.clone()),
            expected_file_identity: expectation.file_identity.clone(),
            fencing_token: lease.fencing_token,
        };
        self.store.prepare_operation(&intent)?;
        if strategy == crate::MutationStrategy::AtomicNoReplaceRename {
            return self.apply_one_atomic(&operation_id, item, target, expectation, lease);
        }
        if strategy != crate::MutationStrategy::CopyPublishDelete {
            self.store.transition_operation(
                &operation_id,
                crate::JournalState::Prepared,
                crate::JournalState::Failed,
                lease,
                None,
                Some("apply_strategy_invalid"),
            )?;
            return Err("apply_strategy_invalid".into());
        }
        self.store.transition_operation(
            &operation_id,
            crate::JournalState::Prepared,
            crate::JournalState::Staging,
            lease,
            None,
            None,
        )?;

        let staged = match self.files.stage_copy_exclusive(&item.source, &temporary) {
            Ok(staged) => staged,
            Err(error) => {
                let _ = self.store.transition_operation(
                    &operation_id,
                    crate::JournalState::Staging,
                    crate::JournalState::Failed,
                    lease,
                    None,
                    Some(&error),
                );
                return Err(error);
            }
        };
        self.store.transition_operation(
            &operation_id,
            crate::JournalState::Staging,
            crate::JournalState::Staged,
            lease,
            Some(&staged),
            None,
        )?;
        if staged.size_bytes != expectation.size_bytes
            || staged.content_sha256 != expectation.content_sha256
        {
            let expected = staged_delete_expectation(&staged);
            let _ = self
                .files
                .delete_file_if_matches(&staged.temporary, &expected);
            let error = "staged_content_mismatch";
            self.store.transition_operation(
                &operation_id,
                crate::JournalState::Staged,
                crate::JournalState::Failed,
                lease,
                Some(&staged),
                Some(error),
            )?;
            return Err(error.into());
        }
        self.store.transition_operation(
            &operation_id,
            crate::JournalState::Staged,
            crate::JournalState::ContentVerified,
            lease,
            Some(&staged),
            None,
        )?;
        if let Err(error) = self.files.publish_no_replace(&staged.temporary, target) {
            if !error.starts_with("published_") {
                let expected = staged_delete_expectation(&staged);
                let _ = self
                    .files
                    .delete_file_if_matches(&staged.temporary, &expected);
            }
            let next = if error.starts_with("published_") {
                crate::JournalState::RecoveryRequired
            } else {
                crate::JournalState::Failed
            };
            let _ = self.store.transition_operation(
                &operation_id,
                crate::JournalState::ContentVerified,
                next,
                lease,
                Some(&staged),
                Some(&error),
            );
            return Err(error);
        }
        self.transition_apply_after_commit(
            &operation_id,
            crate::JournalState::ContentVerified,
            crate::JournalState::Published,
            lease,
            Some(&staged),
        )?;

        let published_hash = self.files.content_sha256(target).map_err(|cause| {
            self.mark_apply_recovery(
                &operation_id,
                crate::JournalState::Published,
                lease,
                Some(&staged),
                "published_target_hash_failed",
                &cause,
            )
        })?;
        let published_size = self.files.size(target).map_err(|cause| {
            self.mark_apply_recovery(
                &operation_id,
                crate::JournalState::Published,
                lease,
                Some(&staged),
                "published_target_size_failed",
                &cause,
            )
        })?;
        if published_size != expectation.size_bytes || published_hash != expectation.content_sha256
        {
            let error = "published_content_mismatch";
            self.set_apply_recovery(
                &operation_id,
                crate::JournalState::Published,
                lease,
                Some(&staged),
                error,
            )?;
            return Err(error.into());
        }
        let source_now = self
            .files
            .mutation_fingerprint(&item.source)
            .map_err(|cause| {
                self.mark_apply_recovery(
                    &operation_id,
                    crate::JournalState::Published,
                    lease,
                    Some(&staged),
                    "published_source_fingerprint_failed",
                    &cause,
                )
            })?;
        if source_now.size_bytes != expectation.size_bytes
            || source_now.content_sha256.as_deref() != Some(&expectation.content_sha256)
            || expectation.file_identity.is_some()
                && source_now.file_identity != expectation.file_identity
        {
            let error = "source_changed_before_delete";
            self.set_apply_recovery(
                &operation_id,
                crate::JournalState::Published,
                lease,
                Some(&staged),
                error,
            )?;
            return Err(error.into());
        }
        let delete_expected = DeleteExpectation {
            size_bytes: expectation.size_bytes,
            content_sha256: expectation.content_sha256.clone(),
            file_identity: expectation.file_identity.clone(),
        };
        if let Err(error) = self
            .files
            .delete_file_if_matches(&item.source, &delete_expected)
        {
            self.set_apply_recovery(
                &operation_id,
                crate::JournalState::Published,
                lease,
                Some(&staged),
                &error,
            )?;
            return Err(format!("source_delete_failed:{error}"));
        }
        self.transition_apply_after_commit(
            &operation_id,
            crate::JournalState::Published,
            crate::JournalState::SourceDeleted,
            lease,
            Some(&staged),
        )?;
        self.transition_apply_after_commit(
            &operation_id,
            crate::JournalState::SourceDeleted,
            crate::JournalState::Completed,
            lease,
            Some(&staged),
        )?;
        Ok(OperationAction::CopyDelete)
    }

    fn apply_one_atomic(
        &self,
        operation_id: &str,
        item: &crate::ApplyItem,
        target: &Path,
        expectation: &MovePreflight,
        lease: &crate::MutationLease,
    ) -> Result<OperationAction, String> {
        self.store.transition_operation(
            operation_id,
            crate::JournalState::Prepared,
            crate::JournalState::ContentVerified,
            lease,
            None,
            None,
        )?;
        let expected = expectation.delete_expectation();
        let moved =
            match self
                .files
                .atomic_move_no_replace_if_matches(&item.source, target, &expected)
            {
                Ok(moved) => moved,
                Err(error) => {
                    let requires_recovery = atomic_error_requires_recovery(&error);
                    let reported_error = if requires_recovery {
                        format!("published_atomic_move_failed:{error}")
                    } else {
                        error.clone()
                    };
                    let state = if requires_recovery {
                        crate::JournalState::RecoveryRequired
                    } else {
                        crate::JournalState::Failed
                    };
                    let _ = self.store.transition_operation(
                        operation_id,
                        crate::JournalState::ContentVerified,
                        state,
                        lease,
                        None,
                        Some(&reported_error),
                    );
                    return Err(reported_error);
                }
            };
        self.transition_apply_after_commit(
            operation_id,
            crate::JournalState::ContentVerified,
            crate::JournalState::Published,
            lease,
            Some(&moved),
        )?;
        if let Err(cause) =
            verify_atomic_move_result(self.files.as_ref(), &item.source, target, expectation)
        {
            let error = format!("published_atomic_verify_failed:{cause}");
            self.set_apply_recovery(
                operation_id,
                crate::JournalState::Published,
                lease,
                Some(&moved),
                &error,
            )?;
            return Err(error);
        }
        self.transition_apply_after_commit(
            operation_id,
            crate::JournalState::Published,
            crate::JournalState::SourceDeleted,
            lease,
            Some(&moved),
        )?;
        self.transition_apply_after_commit(
            operation_id,
            crate::JournalState::SourceDeleted,
            crate::JournalState::Completed,
            lease,
            Some(&moved),
        )?;
        Ok(OperationAction::Move)
    }

    fn transition_apply_after_commit(
        &self,
        operation_id: &str,
        from: crate::JournalState,
        to: crate::JournalState,
        lease: &crate::MutationLease,
        staged: Option<&crate::StagedFile>,
    ) -> Result<(), String> {
        self.store
            .transition_operation(operation_id, from, to, lease, staged, None)
            .map_err(|cause| {
                self.mark_apply_recovery(
                    operation_id,
                    from,
                    lease,
                    staged,
                    "published_journal_transition_failed",
                    &cause,
                )
            })
    }

    fn mark_apply_recovery(
        &self,
        operation_id: &str,
        from: crate::JournalState,
        lease: &crate::MutationLease,
        staged: Option<&crate::StagedFile>,
        code: &str,
        cause: &str,
    ) -> String {
        let error = format!("{code}:{cause}");
        let _ = self.store.transition_operation(
            operation_id,
            from,
            crate::JournalState::RecoveryRequired,
            lease,
            staged,
            Some(&error),
        );
        error
    }

    fn set_apply_recovery(
        &self,
        operation_id: &str,
        from: crate::JournalState,
        lease: &crate::MutationLease,
        staged: Option<&crate::StagedFile>,
        error: &str,
    ) -> Result<(), String> {
        self.store
            .transition_operation(
                operation_id,
                from,
                crate::JournalState::RecoveryRequired,
                lease,
                staged,
                Some(error),
            )
            .map_err(|cause| {
                self.mark_apply_recovery(
                    operation_id,
                    from,
                    lease,
                    staged,
                    "published_journal_transition_failed",
                    &cause,
                )
            })
    }
}

type MovePreflight = crate::PreflightEvidence;

impl MovePreflight {
    fn delete_expectation(&self) -> DeleteExpectation {
        DeleteExpectation {
            size_bytes: self.size_bytes,
            content_sha256: self.content_sha256.clone(),
            file_identity: self.file_identity.clone(),
        }
    }
}

fn atomic_error_requires_recovery(error: &str) -> bool {
    error.starts_with("renamed_") || error.starts_with("atomic_move_result_")
}

fn verify_atomic_move_result<F: FileMutator>(
    files: &F,
    source: &Path,
    target: &Path,
    expectation: &MovePreflight,
) -> Result<(), String> {
    if files.exists(source) {
        return Err("atomic_move_result_source_present".into());
    }
    if !files.exists(target) {
        return Err("atomic_move_result_target_missing".into());
    }
    let observed = files.mutation_fingerprint(target)?;
    if observed.size_bytes != expectation.size_bytes
        || observed.content_sha256.as_deref() != Some(&expectation.content_sha256)
        || expectation.file_identity.is_some()
            && observed.file_identity != expectation.file_identity
    {
        return Err("atomic_move_result_mismatch".into());
    }
    Ok(())
}

fn staged_delete_expectation(staged: &crate::StagedFile) -> DeleteExpectation {
    DeleteExpectation {
        size_bytes: staged.size_bytes,
        content_sha256: staged.content_sha256.clone(),
        file_identity: None,
    }
}

fn mutation_error_requires_recovery(error: &str) -> bool {
    error.starts_with("published_")
        || error.starts_with("source_delete_failed")
        || error == "source_changed_before_delete"
        || error.starts_with("rollback_target_delete_failed")
        || error == "rollback_target_changed_before_delete"
        || error == "partial_copy_changed_before_delete"
        || error.starts_with("partial_copy_delete_failed")
        || error.starts_with("conditional_delete_")
        || error.starts_with("journal_transition_")
        || error == "mutation_lease_stale"
}

fn rollback_preflight_error(item: &crate::VerifyItem, error: &str) -> String {
    match error {
        "source_missing" => "target_missing".into(),
        "source_changed_since_scan" => "target_changed_since_apply".into(),
        "target_already_exists" if item.action != OperationAction::CopySourceRetained => {
            "rollback_source_exists_without_journal_evidence".into()
        }
        code => code.to_owned(),
    }
}

fn preflight_apply<F: FileMutator>(
    files: &F,
    item: &crate::ApplyItem,
) -> Result<MovePreflight, String> {
    PreflightEngine::new(files).apply(item)
}

#[cfg(test)]
mod safety_workflow_tests {
    use super::*;
    use crate::ports::{ApplyStore, FileMutator, RollbackStore, VerifyStore};
    use std::sync::{atomic::AtomicUsize, Mutex};

    #[derive(Default)]
    struct FailingCrossVolume {
        deleted: Mutex<Vec<PathBuf>>,
        calls: Mutex<Vec<String>>,
    }
    impl FileMutator for FailingCrossVolume {
        fn exists(&self, path: &Path) -> bool {
            path == Path::new("source")
                || (path != Path::new("target") && path.to_string_lossy().starts_with("target"))
        }
        fn same_volume(&self, _: &Path, _: &Path) -> Result<bool, String> {
            Ok(false)
        }
        fn move_file(&self, source: &Path, target: &Path) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}->{}", source.display(), target.display()));
            Ok(())
        }
        fn copy_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn size(&self, path: &Path) -> Result<u64, String> {
            Ok(if path == Path::new("source") { 10 } else { 9 })
        }
        fn delete_file(&self, path: &Path) -> Result<(), String> {
            self.deleted.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }
    }

    struct OneApplyStore;
    impl crate::ports::PreflightStore for OneApplyStore {}
    impl ApplyStore for OneApplyStore {
        fn load_completed_plan(&self, _: &str) -> Result<Vec<crate::ApplyItem>, String> {
            Ok(vec![crate::ApplyItem {
                plan_item_id: "item".into(),
                ordinal: 1,
                source: "source".into(),
                target: Some("target".into()),
                action: PlanAction::Move,
                disposition: ExecutionDisposition::Executable,
                risk: Risk::None,
                reason: None,
                issues: Vec::new(),
                source_fingerprint: None,
                target_root: None,
                allow_long_paths: false,
            }])
        }
        fn validate_plan_snapshot(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn successful_plan_item_ids(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn begin_execution(&self, _: &str, _: bool) -> Result<String, String> {
            Ok("execution".into())
        }
        fn save_operation(&self, _: &str, _: &OperationLog) -> Result<(), String> {
            Ok(())
        }
        fn finish_execution(
            &self,
            _: &str,
            _: RunStatus,
            _: u64,
            _: u64,
            _: u64,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn cross_volume_verification_failure_never_deletes_source() {
        let files = Arc::new(FailingCrossVolume::default());
        let result = ApplyUseCase {
            store: Arc::new(OneApplyStore),
            files: Arc::clone(&files),
        }
        .execute("plan", false)
        .unwrap();
        assert_eq!(result.failed, 1);
        assert!(files.deleted.lock().unwrap().is_empty());
    }

    #[derive(Clone, Copy)]
    enum ApplyOuterFault {
        Heartbeat,
        FinishPreflight,
    }

    struct ApplyOuterFaultStore {
        fault: ApplyOuterFault,
        finish_preflight_calls: AtomicUsize,
        execution_statuses: Mutex<Vec<RunStatus>>,
    }

    impl ApplyOuterFaultStore {
        fn new(fault: ApplyOuterFault) -> Self {
            Self {
                fault,
                finish_preflight_calls: AtomicUsize::new(0),
                execution_statuses: Mutex::new(Vec::new()),
            }
        }
    }

    impl crate::ports::PreflightStore for ApplyOuterFaultStore {
        fn begin_preflight(
            &self,
            _: &str,
            _: PreflightKind,
            _: PreflightMode,
        ) -> Result<String, String> {
            Ok("preflight".into())
        }

        fn finish_preflight(
            &self,
            _: &str,
            _: PreflightOutcome,
            _: u64,
            _: u64,
        ) -> Result<(), String> {
            let call = self.finish_preflight_calls.fetch_add(1, Ordering::AcqRel);
            if matches!(self.fault, ApplyOuterFault::FinishPreflight) && call == 0 {
                Err("finish_preflight_db_failed".into())
            } else {
                Ok(())
            }
        }
    }

    impl ApplyStore for ApplyOuterFaultStore {
        fn load_completed_plan(&self, _: &str) -> Result<Vec<crate::ApplyItem>, String> {
            if matches!(self.fault, ApplyOuterFault::Heartbeat) {
                OneApplyStore.load_completed_plan("plan")
            } else {
                Ok(Vec::new())
            }
        }

        fn validate_plan_snapshot(&self, _: &str) -> Result<(), String> {
            Ok(())
        }

        fn successful_plan_item_ids(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn begin_execution(&self, _: &str, _: bool) -> Result<String, String> {
            Ok("execution".into())
        }

        fn heartbeat_apply_lease(&self, _: &crate::MutationLease) -> Result<(), String> {
            if matches!(self.fault, ApplyOuterFault::Heartbeat) {
                Err("heartbeat_db_failed".into())
            } else {
                Ok(())
            }
        }

        fn save_operation(&self, _: &str, _: &OperationLog) -> Result<(), String> {
            panic!("outer fault must abort before operation persistence")
        }

        fn finish_execution(
            &self,
            _: &str,
            status: RunStatus,
            _: u64,
            _: u64,
            _: u64,
        ) -> Result<(), String> {
            self.execution_statuses.lock().unwrap().push(status);
            Ok(())
        }
    }

    #[test]
    fn apply_outer_database_faults_best_effort_terminalize_the_started_run() {
        for (fault, expected_error, expected_preflight_finishes) in [
            (ApplyOuterFault::Heartbeat, "heartbeat_db_failed", 1),
            (
                ApplyOuterFault::FinishPreflight,
                "finish_preflight_db_failed",
                2,
            ),
        ] {
            let store = Arc::new(ApplyOuterFaultStore::new(fault));
            let result = ApplyUseCase {
                store: Arc::clone(&store),
                files: Arc::new(FailingCrossVolume::default()),
            }
            .execute("plan", false);
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("injected outer database fault must fail apply"),
            };
            assert_eq!(error.code(), expected_error);
            assert_eq!(
                store.finish_preflight_calls.load(Ordering::Acquire),
                expected_preflight_finishes
            );
            assert_eq!(
                *store.execution_statuses.lock().unwrap(),
                [RunStatus::Failed]
            );
        }
    }

    #[derive(Default)]
    struct AtomicMoveFiles {
        moved: AtomicBool,
    }

    impl AtomicMoveFiles {
        fn fingerprint() -> crate::FileFingerprint {
            crate::FileFingerprint {
                size_bytes: 12,
                mtime_ns: 7,
                content_sha256: Some("atomic-hash".into()),
                file_identity: Some("native-object-1".into()),
                version: 1,
            }
        }
    }

    impl FileMutator for AtomicMoveFiles {
        fn exists(&self, path: &Path) -> bool {
            let moved = self.moved.load(Ordering::Acquire);
            (path == Path::new(r"C:\source\track.mp3") && !moved)
                || (path == Path::new(r"C:\target\track.mp3") && moved)
        }
        fn same_volume(&self, _: &Path, _: &Path) -> Result<bool, String> {
            Ok(true)
        }
        fn move_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            Err("legacy_move_must_not_run".into())
        }
        fn copy_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            Err("copy_must_not_run".into())
        }
        fn size(&self, _: &Path) -> Result<u64, String> {
            Ok(12)
        }
        fn delete_file(&self, _: &Path) -> Result<(), String> {
            Err("delete_must_not_run".into())
        }
        fn content_sha256(&self, _: &Path) -> Result<String, String> {
            Ok("atomic-hash".into())
        }
        fn mutation_fingerprint(&self, path: &Path) -> Result<crate::FileFingerprint, String> {
            if self.exists(path) {
                Ok(Self::fingerprint())
            } else {
                Err("missing".into())
            }
        }
        fn ensure_no_reparse_points(&self, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn select_move_strategy(
            &self,
            _: &Path,
            _: &Path,
        ) -> Result<crate::MutationStrategy, String> {
            Ok(crate::MutationStrategy::AtomicNoReplaceRename)
        }
        fn atomic_move_no_replace_if_matches(
            &self,
            source: &Path,
            target: &Path,
            expected: &DeleteExpectation,
        ) -> Result<crate::StagedFile, String> {
            assert_eq!(source, Path::new(r"C:\source\track.mp3"));
            assert_eq!(target, Path::new(r"C:\target\track.mp3"));
            assert_eq!(expected.file_identity.as_deref(), Some("native-object-1"));
            assert!(!self.moved.swap(true, Ordering::AcqRel));
            Ok(crate::StagedFile {
                temporary: PathBuf::new(),
                size_bytes: 12,
                content_sha256: "atomic-hash".into(),
                file_identity: Some("native-object-1".into()),
            })
        }
    }

    #[derive(Default)]
    struct AtomicMoveStore {
        strategies: Mutex<Vec<crate::MutationStrategy>>,
        transitions: Mutex<Vec<(crate::JournalState, crate::JournalState)>>,
        actions: Mutex<Vec<OperationAction>>,
    }

    impl crate::ports::PreflightStore for AtomicMoveStore {}
    impl ApplyStore for AtomicMoveStore {
        fn load_completed_plan(&self, _: &str) -> Result<Vec<crate::ApplyItem>, String> {
            Ok(vec![crate::ApplyItem {
                plan_item_id: "item".into(),
                ordinal: 1,
                source: PathBuf::from(r"C:\source\track.mp3"),
                target: Some(PathBuf::from(r"C:\target\track.mp3")),
                action: PlanAction::Move,
                disposition: ExecutionDisposition::Executable,
                risk: Risk::None,
                reason: None,
                issues: Vec::new(),
                source_fingerprint: Some(AtomicMoveFiles::fingerprint()),
                target_root: Some(PathBuf::from(r"C:\target")),
                allow_long_paths: false,
            }])
        }
        fn validate_plan_snapshot(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn successful_plan_item_ids(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }
        fn begin_execution(&self, _: &str, _: bool) -> Result<String, String> {
            Ok("execution".into())
        }
        fn prepare_operation(&self, intent: &crate::OperationIntent) -> Result<(), String> {
            self.strategies.lock().unwrap().push(intent.strategy);
            assert!(intent.temporary.as_os_str().is_empty());
            Ok(())
        }
        fn transition_operation(
            &self,
            _: &str,
            from: crate::JournalState,
            to: crate::JournalState,
            _: &crate::MutationLease,
            _: Option<&crate::StagedFile>,
            _: Option<&str>,
        ) -> Result<(), String> {
            self.transitions.lock().unwrap().push((from, to));
            Ok(())
        }
        fn save_operation(&self, _: &str, operation: &OperationLog) -> Result<(), String> {
            self.actions.lock().unwrap().push(operation.action);
            Ok(())
        }
        fn finish_execution(
            &self,
            _: &str,
            _: RunStatus,
            _: u64,
            _: u64,
            _: u64,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn atomic_strategy_records_move_and_explicitly_skips_staging_phases() {
        let store = Arc::new(AtomicMoveStore::default());
        let result = ApplyUseCase {
            store: Arc::clone(&store),
            files: Arc::new(AtomicMoveFiles::default()),
        }
        .execute("plan", false)
        .unwrap();
        assert_eq!((result.success, result.failed), (1, 0));
        assert_eq!(
            *store.strategies.lock().unwrap(),
            [crate::MutationStrategy::AtomicNoReplaceRename]
        );
        assert_eq!(*store.actions.lock().unwrap(), [OperationAction::Move]);
        assert_eq!(
            *store.transitions.lock().unwrap(),
            [
                (
                    crate::JournalState::Prepared,
                    crate::JournalState::ContentVerified,
                ),
                (
                    crate::JournalState::ContentVerified,
                    crate::JournalState::Published,
                ),
                (
                    crate::JournalState::Published,
                    crate::JournalState::SourceDeleted,
                ),
                (
                    crate::JournalState::SourceDeleted,
                    crate::JournalState::Completed,
                ),
            ]
        );
    }

    struct ReverseStore {
        saved: Mutex<Vec<String>>,
    }
    impl crate::ports::PreflightStore for ReverseStore {}
    impl RollbackStore for ReverseStore {
        fn begin_rollback(&self, _: &str, _: bool) -> Result<String, String> {
            Ok("rollback".into())
        }
        fn load_rollback_items(&self, _: &str) -> Result<Vec<crate::VerifyItem>, String> {
            Ok(vec![1, 3, 2]
                .into_iter()
                .map(|n| crate::VerifyItem {
                    operation_id: n.to_string(),
                    sequence_no: n,
                    source: format!("source{n}").into(),
                    target: Some(format!("target{n}").into()),
                    action: OperationAction::Move,
                    expected_size: None,
                    expected_content_sha256: None,
                    expected_file_identity: None,
                })
                .collect())
        }
        fn save_rollback_result(
            &self,
            _: &str,
            operation_id: &str,
            _: OperationResult,
            _: Option<&str>,
        ) -> Result<(), String> {
            self.saved.lock().unwrap().push(operation_id.into());
            Ok(())
        }
        fn finish_rollback(
            &self,
            _: &str,
            _: RunStatus,
            _: u64,
            _: u64,
            _: u64,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn rollback_processes_successful_operations_in_reverse_sequence() {
        let store = Arc::new(ReverseStore {
            saved: Mutex::new(vec![]),
        });
        let files = Arc::new(FailingCrossVolume::default());
        RollbackUseCase {
            store: Arc::clone(&store),
            files,
        }
        .execute("execution", true)
        .unwrap();
        assert_eq!(*store.saved.lock().unwrap(), ["3", "2", "1"]);
    }

    struct MismatchRollbackStore;
    impl crate::ports::PreflightStore for MismatchRollbackStore {}
    impl RollbackStore for MismatchRollbackStore {
        fn begin_rollback(&self, _: &str, _: bool) -> Result<String, String> {
            Ok("rollback".into())
        }
        fn load_rollback_items(&self, _: &str) -> Result<Vec<crate::VerifyItem>, String> {
            Ok(vec![crate::VerifyItem {
                operation_id: "operation".into(),
                sequence_no: 1,
                source: "source".into(),
                target: Some("target".into()),
                action: OperationAction::Move,
                expected_size: Some(10),
                expected_content_sha256: Some("expected-hash".into()),
                expected_file_identity: None,
            }])
        }
        fn save_rollback_result(
            &self,
            _: &str,
            _: &str,
            result: OperationResult,
            error: Option<&str>,
        ) -> Result<(), String> {
            assert_eq!(result, OperationResult::Failed);
            assert_eq!(error, Some("target_changed_since_apply"));
            Ok(())
        }
        fn finish_rollback(
            &self,
            _: &str,
            _: RunStatus,
            _: u64,
            _: u64,
            _: u64,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct MismatchFiles {
        mutations: Mutex<Vec<String>>,
    }
    impl FileMutator for MismatchFiles {
        fn exists(&self, path: &Path) -> bool {
            path == Path::new("target")
        }
        fn same_volume(&self, _: &Path, _: &Path) -> Result<bool, String> {
            Ok(true)
        }
        fn move_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            self.mutations.lock().unwrap().push("move".into());
            Ok(())
        }
        fn copy_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            self.mutations.lock().unwrap().push("copy".into());
            Ok(())
        }
        fn size(&self, _: &Path) -> Result<u64, String> {
            Ok(9)
        }
        fn content_sha256(&self, _: &Path) -> Result<String, String> {
            Ok("expected-hash".into())
        }
        fn ensure_no_reparse_points(&self, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn delete_file(&self, _: &Path) -> Result<(), String> {
            self.mutations.lock().unwrap().push("delete".into());
            Ok(())
        }
    }

    #[test]
    fn rollback_does_not_mutate_a_target_that_changed_after_apply() {
        let files = Arc::new(MismatchFiles::default());
        let result = RollbackUseCase {
            store: Arc::new(MismatchRollbackStore),
            files: Arc::clone(&files),
        }
        .execute("execution", false)
        .unwrap();
        assert_eq!(result.failed, 1);
        assert!(files.mutations.lock().unwrap().is_empty());
    }

    #[derive(Default)]
    struct FailingRollbackPreflightStore {
        mode: Mutex<Option<PreflightMode>>,
        logs: Mutex<Vec<PreflightLog>>,
        rollback_statuses: Mutex<Vec<RunStatus>>,
    }

    impl crate::ports::PreflightStore for FailingRollbackPreflightStore {
        fn begin_preflight(
            &self,
            _: &str,
            _: PreflightKind,
            mode: PreflightMode,
        ) -> Result<String, String> {
            *self.mode.lock().unwrap() = Some(mode);
            Ok("preflight".into())
        }

        fn save_preflight_log(&self, _: &str, _: u64, log: &PreflightLog) -> Result<(), String> {
            self.logs.lock().unwrap().push(log.clone());
            Err("preflight_log_commit_failed".into())
        }
    }

    impl RollbackStore for FailingRollbackPreflightStore {
        fn begin_rollback(&self, _: &str, _: bool) -> Result<String, String> {
            Ok("rollback".into())
        }

        fn load_rollback_items(&self, _: &str) -> Result<Vec<crate::VerifyItem>, String> {
            Ok(vec![crate::VerifyItem {
                operation_id: "operation".into(),
                sequence_no: 1,
                source: "source".into(),
                target: Some("target".into()),
                action: OperationAction::Move,
                expected_size: Some(10),
                expected_content_sha256: Some("expected-hash".into()),
                expected_file_identity: Some("target-id".into()),
            }])
        }

        fn save_rollback_result(
            &self,
            _: &str,
            _: &str,
            _: OperationResult,
            _: Option<&str>,
        ) -> Result<(), String> {
            panic!("rollback result must not be saved before preflight evidence")
        }

        fn finish_rollback(
            &self,
            _: &str,
            status: RunStatus,
            _: u64,
            _: u64,
            _: u64,
        ) -> Result<(), String> {
            self.rollback_statuses.lock().unwrap().push(status);
            Ok(())
        }
    }

    #[derive(Default)]
    struct PreflightOnlyFiles {
        mutations: Mutex<Vec<String>>,
    }

    impl FileMutator for PreflightOnlyFiles {
        fn exists(&self, path: &Path) -> bool {
            path == Path::new("target")
        }

        fn same_volume(&self, _: &Path, _: &Path) -> Result<bool, String> {
            Ok(true)
        }

        fn move_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            self.mutations.lock().unwrap().push("move".into());
            Err("mutation_must_not_run".into())
        }

        fn copy_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            self.mutations.lock().unwrap().push("copy".into());
            Err("mutation_must_not_run".into())
        }

        fn size(&self, _: &Path) -> Result<u64, String> {
            Ok(10)
        }

        fn content_sha256(&self, _: &Path) -> Result<String, String> {
            Ok("expected-hash".into())
        }

        fn mutation_fingerprint(&self, _: &Path) -> Result<crate::FileFingerprint, String> {
            Ok(crate::FileFingerprint {
                size_bytes: 10,
                mtime_ns: 42,
                content_sha256: Some("expected-hash".into()),
                file_identity: Some("target-id".into()),
                version: 1,
            })
        }

        fn ensure_no_reparse_points(&self, _: &Path) -> Result<(), String> {
            Ok(())
        }

        fn delete_file(&self, _: &Path) -> Result<(), String> {
            self.mutations.lock().unwrap().push("delete".into());
            Err("mutation_must_not_run".into())
        }
    }

    #[test]
    fn rollback_commits_identical_preflight_evidence_before_any_mutation() {
        let mut observed_logs = Vec::new();
        for dry_run in [true, false] {
            let store = Arc::new(FailingRollbackPreflightStore::default());
            let files = Arc::new(PreflightOnlyFiles::default());
            let result = RollbackUseCase {
                store: Arc::clone(&store),
                files: Arc::clone(&files),
            }
            .execute("execution", dry_run);
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("preflight log commit failure must abort rollback"),
            };
            assert_eq!(error.code(), "preflight_log_commit_failed");
            assert!(files.mutations.lock().unwrap().is_empty());
            assert_eq!(
                *store.rollback_statuses.lock().unwrap(),
                [RunStatus::Failed]
            );
            observed_logs.push(store.logs.lock().unwrap()[0].clone());
        }
        assert_eq!(observed_logs[0].outcome, PreflightOutcome::Passed);
        assert_eq!(observed_logs[0].item_id, observed_logs[1].item_id);
        assert_eq!(observed_logs[0].source, observed_logs[1].source);
        assert_eq!(observed_logs[0].target, observed_logs[1].target);
        assert_eq!(
            format!("{:?}", observed_logs[0].expected),
            format!("{:?}", observed_logs[1].expected)
        );
        assert_eq!(
            format!("{:?}", observed_logs[0].observed),
            format!("{:?}", observed_logs[1].observed)
        );
    }

    #[derive(Clone, Copy)]
    enum RollbackOuterFault {
        Heartbeat,
        FinishPreflight,
    }

    struct RollbackOuterFaultStore {
        fault: RollbackOuterFault,
        finish_preflight_calls: AtomicUsize,
        rollback_statuses: Mutex<Vec<RunStatus>>,
    }

    impl RollbackOuterFaultStore {
        fn new(fault: RollbackOuterFault) -> Self {
            Self {
                fault,
                finish_preflight_calls: AtomicUsize::new(0),
                rollback_statuses: Mutex::new(Vec::new()),
            }
        }
    }

    impl crate::ports::PreflightStore for RollbackOuterFaultStore {
        fn finish_preflight(
            &self,
            _: &str,
            _: PreflightOutcome,
            _: u64,
            _: u64,
        ) -> Result<(), String> {
            let call = self.finish_preflight_calls.fetch_add(1, Ordering::AcqRel);
            if matches!(self.fault, RollbackOuterFault::FinishPreflight) && call == 0 {
                Err("rollback_finish_preflight_db_failed".into())
            } else {
                Ok(())
            }
        }
    }

    impl RollbackStore for RollbackOuterFaultStore {
        fn begin_rollback(&self, _: &str, _: bool) -> Result<String, String> {
            Ok("rollback".into())
        }

        fn load_rollback_items(&self, _: &str) -> Result<Vec<crate::VerifyItem>, String> {
            if matches!(self.fault, RollbackOuterFault::Heartbeat) {
                Ok(vec![crate::VerifyItem {
                    operation_id: "operation".into(),
                    sequence_no: 1,
                    source: "source".into(),
                    target: Some("target".into()),
                    action: OperationAction::Move,
                    expected_size: Some(10),
                    expected_content_sha256: Some("expected-hash".into()),
                    expected_file_identity: None,
                }])
            } else {
                Ok(Vec::new())
            }
        }

        fn heartbeat_rollback_lease(&self, _: &crate::MutationLease) -> Result<(), String> {
            if matches!(self.fault, RollbackOuterFault::Heartbeat) {
                Err("rollback_heartbeat_db_failed".into())
            } else {
                Ok(())
            }
        }

        fn save_rollback_result(
            &self,
            _: &str,
            _: &str,
            _: OperationResult,
            _: Option<&str>,
        ) -> Result<(), String> {
            panic!("outer rollback fault must abort before result persistence")
        }

        fn finish_rollback(
            &self,
            _: &str,
            status: RunStatus,
            _: u64,
            _: u64,
            _: u64,
        ) -> Result<(), String> {
            self.rollback_statuses.lock().unwrap().push(status);
            Ok(())
        }
    }

    #[test]
    fn rollback_outer_database_faults_best_effort_terminalize_the_started_run() {
        for (fault, expected_error, expected_preflight_finishes) in [
            (
                RollbackOuterFault::Heartbeat,
                "rollback_heartbeat_db_failed",
                1,
            ),
            (
                RollbackOuterFault::FinishPreflight,
                "rollback_finish_preflight_db_failed",
                2,
            ),
        ] {
            let store = Arc::new(RollbackOuterFaultStore::new(fault));
            let result = RollbackUseCase {
                store: Arc::clone(&store),
                files: Arc::new(PreflightOnlyFiles::default()),
            }
            .execute("execution", false);
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("injected outer database fault must fail rollback"),
            };
            assert_eq!(error.code(), expected_error);
            assert_eq!(
                store.finish_preflight_calls.load(Ordering::Acquire),
                expected_preflight_finishes
            );
            assert_eq!(
                *store.rollback_statuses.lock().unwrap(),
                [RunStatus::Failed]
            );
        }
    }

    struct RecoveryVerifyStore {
        action: OperationAction,
    }

    impl VerifyStore for RecoveryVerifyStore {
        fn begin_verify(&self, _: &str) -> Result<String, String> {
            Err("execution_verify_not_expected".into())
        }
        fn load_successful_operations(&self, _: &str) -> Result<Vec<crate::VerifyItem>, String> {
            Err("execution_verify_not_expected".into())
        }
        fn begin_verify_subject(
            &self,
            _: &str,
            subject: crate::VerifySubject,
        ) -> Result<String, String> {
            assert_eq!(subject, crate::VerifySubject::Recovery);
            Ok("verify".into())
        }
        fn load_verify_subject_items(
            &self,
            _: &str,
            subject: crate::VerifySubject,
        ) -> Result<Vec<crate::VerifyItem>, String> {
            assert_eq!(subject, crate::VerifySubject::Recovery);
            Ok(vec![crate::VerifyItem {
                operation_id: "operation".into(),
                sequence_no: 1,
                source: "source".into(),
                target: Some("target".into()),
                action: self.action,
                expected_size: Some(10),
                expected_content_sha256: None,
                expected_file_identity: None,
            }])
        }
        fn save_verify_result(
            &self,
            _: &str,
            _: &str,
            result: OperationResult,
            error: Option<&str>,
        ) -> Result<(), String> {
            assert_eq!(result, OperationResult::Success);
            assert!(error.is_none());
            Ok(())
        }
        fn finish_verify(
            &self,
            _: &str,
            status: RunStatus,
            success: u64,
            failed: u64,
        ) -> Result<(), String> {
            assert_eq!(status, RunStatus::Completed);
            assert_eq!((success, failed), (1, 0));
            Ok(())
        }
    }

    struct RecoveryVerifyFiles {
        source_exists: bool,
        target_exists: bool,
    }

    impl FileMutator for RecoveryVerifyFiles {
        fn exists(&self, path: &Path) -> bool {
            match path.to_string_lossy().as_ref() {
                "source" => self.source_exists,
                "target" => self.target_exists,
                _ => false,
            }
        }
        fn same_volume(&self, _: &Path, _: &Path) -> Result<bool, String> {
            Ok(true)
        }
        fn move_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            Err("mutation_not_expected".into())
        }
        fn copy_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            Err("mutation_not_expected".into())
        }
        fn size(&self, _: &Path) -> Result<u64, String> {
            Ok(10)
        }
        fn content_sha256(&self, _: &Path) -> Result<String, String> {
            Ok("recovery-test-hash".into())
        }
        fn delete_file(&self, _: &Path) -> Result<(), String> {
            Err("mutation_not_expected".into())
        }
    }

    #[test]
    fn recovery_verify_uses_the_persisted_action_direction() {
        for (action, source_exists, target_exists) in [
            (OperationAction::CopyDelete, false, true),
            (OperationAction::CopySourceRetained, true, false),
            (OperationAction::Skip, true, false),
        ] {
            let result = VerifyUseCase {
                store: Arc::new(RecoveryVerifyStore { action }),
                files: Arc::new(RecoveryVerifyFiles {
                    source_exists,
                    target_exists,
                }),
            }
            .execute_subject("recovery", crate::VerifySubject::Recovery)
            .unwrap();
            assert_eq!((result.success, result.failed), (1, 0));
        }
    }
}
