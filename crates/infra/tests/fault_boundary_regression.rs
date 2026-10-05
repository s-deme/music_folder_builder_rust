use music_folder_core::{
    ports::{
        ApplyStore, DeleteExpectation, FileMutator, FileSystem, PreflightStore, RecoveryStore,
        RollbackStore, ScanStore, VerifyStore,
    },
    usecases::{
        ApplyUseCase, PlanOptions, PlanUseCase, RecoveryUseCase, RollbackUseCase, VerifyUseCase,
    },
    FileFingerprint, FileKind, JournalState, MutationDirection, NamingRules, OperationAction,
    OperationIntent, RecoveryAction, RunStatus, ScannedFile, StagedFile, TrackMetadata,
    VerifySubject,
};
use music_folder_infra::{sqlite::SqliteScanStore, windows_fs::LocalFileSystem};
use rusqlite::{params, Connection};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};
use tempfile::tempdir;
use uuid::Uuid;

static MUTATION_BOUNDARY_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn mutation_boundary_test_guard() -> std::sync::MutexGuard<'static, ()> {
    MUTATION_BOUNDARY_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct PersistedPlan {
    store: Arc<SqliteScanStore>,
    database: PathBuf,
    plan_id: String,
    plan_item_id: String,
    source: PathBuf,
    target: PathBuf,
    target_root: PathBuf,
    fingerprint: FileFingerprint,
}

fn persisted_plan(root: &Path, original: &[u8]) -> PersistedPlan {
    let source_root = root.join("source");
    let source = source_root.join("track.mp3");
    let target_root = root.join("target");
    let database = root.join("state.db");
    fs::create_dir_all(&source_root).unwrap();
    fs::write(&source, original).unwrap();

    let fingerprint = FileSystem::fingerprint(&LocalFileSystem, &source).unwrap();
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let scan_id = store.begin_scan(&source_root).unwrap();
    store
        .save_batch(
            &scan_id,
            &[ScannedFile {
                id: Uuid::new_v4(),
                path: source.clone(),
                fingerprint: fingerprint.clone(),
                metadata: Some(TrackMetadata {
                    artist: Some("Boundary Artist".into()),
                    album_artist: None,
                    album: Some("Boundary Album".into()),
                    title: Some("Boundary Track".into()),
                    track_no: Some(1),
                    disc_no: Some(1),
                    year: Some(2026),
                    genre: None,
                    has_artwork: None,
                }),
                kind: FileKind::Music,
            }],
        )
        .unwrap();
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();
    let plan_id = PlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &scan_id,
        &PlanOptions {
            target_root: target_root.clone(),
            batch_size: 8,
            naming: NamingRules::default(),
        },
    )
    .unwrap()
    .plan_id;
    let item = store
        .list_plan_items(&plan_id, None, 1, None, None)
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(item.action, "move");
    assert_eq!(item.risk, "none");

    PersistedPlan {
        store,
        database,
        plan_id,
        plan_item_id: item.id,
        source,
        target: PathBuf::from(item.target_path.unwrap()),
        target_root,
        fingerprint,
    }
}

fn assert_execution_terminal(
    database: &Path,
    execution_id: &str,
    expected_status: &str,
    expected_failed: i64,
) {
    let connection = Connection::open(database).unwrap();
    let (status, finished_at, failed): (String, Option<i64>, i64) = connection
        .query_row(
            "SELECT status,finished_at,failed_count FROM execution_runs WHERE id=?1",
            params![execution_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, expected_status);
    assert!(finished_at.is_some(), "execution must not remain running");
    assert_eq!(failed, expected_failed);
}

fn operation_error(database: &Path, execution_id: &str) -> String {
    Connection::open(database)
        .unwrap()
        .query_row(
            "SELECT error FROM operation_logs WHERE execution_id=?1",
            params![execution_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn journal_state_and_error(database: &Path, execution_id: &str) -> (String, Option<String>) {
    Connection::open(database)
        .unwrap()
        .query_row(
            "SELECT state,error FROM operation_journal WHERE attempt_id=?1",
            params![execution_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn same_size_source_replacement_is_rejected_and_never_enters_the_journal() {
    let _guard = mutation_boundary_test_guard();
    let temp = tempdir().unwrap();
    let original = b"original-eight";
    let replacement = b"replaced-eight";
    assert_eq!(original.len(), replacement.len());
    let plan = persisted_plan(temp.path(), original);

    fs::write(&plan.source, replacement).unwrap();
    let result = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&plan.plan_id, false)
    .unwrap();

    assert_eq!((result.success, result.skipped, result.failed), (0, 0, 1));
    assert_execution_terminal(&plan.database, &result.execution_id, "partial", 1);
    assert_eq!(
        operation_error(&plan.database, &result.execution_id),
        "source_changed_since_scan"
    );
    let journal_count: i64 = Connection::open(&plan.database)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM operation_journal WHERE attempt_id=?1",
            params![result.execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        journal_count, 0,
        "preflight failure must not persist intent"
    );
    assert_eq!(fs::read(&plan.source).unwrap(), replacement);
    assert!(!plan.target.exists());
}

enum FaultInjection {
    None,
    StageIo {
        code: &'static str,
        leave_partial_before_cleanup: bool,
    },
    StageShortWrite,
    PublishIo {
        code: &'static str,
    },
    PublishedIo {
        code: &'static str,
    },
    DeleteIo {
        code: &'static str,
    },
    PublishRace {
        winner: Vec<u8>,
    },
    DeleteRace {
        replacement: Vec<u8>,
    },
    ReparseBeforeStage {
        outside: PathBuf,
    },
}

struct FaultInjectingFileSystem {
    fault: FaultInjection,
    fired: AtomicBool,
}

struct DatabaseTransitionFaultStore {
    inner: Arc<SqliteScanStore>,
    fired: AtomicBool,
}

struct RecoveryTransitionFaultStore {
    inner: Arc<SqliteScanStore>,
    fired: AtomicBool,
}

impl RecoveryTransitionFaultStore {
    fn new(inner: Arc<SqliteScanStore>) -> Self {
        Self {
            inner,
            fired: AtomicBool::new(false),
        }
    }
}

impl DatabaseTransitionFaultStore {
    fn new(inner: Arc<SqliteScanStore>) -> Self {
        Self {
            inner,
            fired: AtomicBool::new(false),
        }
    }
}

impl PreflightStore for DatabaseTransitionFaultStore {
    fn begin_preflight(
        &self,
        parent_attempt_id: &str,
        kind: music_folder_core::PreflightKind,
        mode: music_folder_core::PreflightMode,
    ) -> Result<String, String> {
        self.inner.begin_preflight(parent_attempt_id, kind, mode)
    }

    fn save_preflight_log(
        &self,
        preflight_id: &str,
        ordinal: u64,
        log: &music_folder_core::PreflightLog,
    ) -> Result<(), String> {
        self.inner.save_preflight_log(preflight_id, ordinal, log)
    }

    fn finish_preflight(
        &self,
        preflight_id: &str,
        outcome: music_folder_core::PreflightOutcome,
        passed: u64,
        blocked: u64,
    ) -> Result<(), String> {
        self.inner
            .finish_preflight(preflight_id, outcome, passed, blocked)
    }
}

impl PreflightStore for RecoveryTransitionFaultStore {
    fn begin_preflight(
        &self,
        parent_attempt_id: &str,
        kind: music_folder_core::PreflightKind,
        mode: music_folder_core::PreflightMode,
    ) -> Result<String, String> {
        self.inner.begin_preflight(parent_attempt_id, kind, mode)
    }

    fn save_preflight_log(
        &self,
        preflight_id: &str,
        ordinal: u64,
        log: &music_folder_core::PreflightLog,
    ) -> Result<(), String> {
        self.inner.save_preflight_log(preflight_id, ordinal, log)
    }

    fn finish_preflight(
        &self,
        preflight_id: &str,
        outcome: music_folder_core::PreflightOutcome,
        passed: u64,
        blocked: u64,
    ) -> Result<(), String> {
        self.inner
            .finish_preflight(preflight_id, outcome, passed, blocked)
    }
}

impl RecoveryStore for RecoveryTransitionFaultStore {
    fn list_recovery_items(&self) -> Result<Vec<music_folder_core::ports::RecoveryItem>, String> {
        self.inner.list_recovery_items()
    }

    fn begin_recovery(
        &self,
        operation_id: &str,
        action: RecoveryAction,
        dry_run: bool,
    ) -> Result<String, String> {
        self.inner.begin_recovery(operation_id, action, dry_run)
    }

    fn acquire_recovery_lease(
        &self,
        operation_id: &str,
        owner_id: &str,
    ) -> Result<music_folder_core::MutationLease, String> {
        self.inner.acquire_recovery_lease(operation_id, owner_id)
    }

    fn load_recovery_item_under_lease(
        &self,
        operation_id: &str,
        lease: &music_folder_core::MutationLease,
    ) -> Result<music_folder_core::ports::RecoveryItem, String> {
        self.inner
            .load_recovery_item_under_lease(operation_id, lease)
    }

    fn heartbeat_recovery_lease(
        &self,
        lease: &music_folder_core::MutationLease,
    ) -> Result<(), String> {
        self.inner.heartbeat_recovery_lease(lease)
    }

    fn transition_recovery_operation(
        &self,
        operation_id: &str,
        from: JournalState,
        to: JournalState,
        lease: &music_folder_core::MutationLease,
        staged: Option<&StagedFile>,
        error: Option<&str>,
    ) -> Result<(), String> {
        if from == JournalState::ContentVerified
            && to == JournalState::Published
            && !self.fired.swap(true, Ordering::SeqCst)
        {
            return Err("recovery_transition_db_failed".into());
        }
        self.inner
            .transition_recovery_operation(operation_id, from, to, lease, staged, error)
    }

    fn finish_recovery(
        &self,
        recovery_id: &str,
        status: RunStatus,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.inner.finish_recovery(recovery_id, status, error)
    }

    fn release_recovery_lease(
        &self,
        lease: &music_folder_core::MutationLease,
    ) -> Result<(), String> {
        self.inner.release_recovery_lease(lease)
    }
}

impl ApplyStore for DatabaseTransitionFaultStore {
    fn load_completed_plan(
        &self,
        plan_id: &str,
    ) -> Result<Vec<music_folder_core::ApplyItem>, String> {
        self.inner.load_completed_plan(plan_id)
    }

    fn validate_plan_snapshot(&self, plan_id: &str) -> Result<(), String> {
        self.inner.validate_plan_snapshot(plan_id)
    }

    fn successful_plan_item_ids(&self, plan_id: &str) -> Result<Vec<String>, String> {
        self.inner.successful_plan_item_ids(plan_id)
    }

    fn begin_execution(&self, plan_id: &str, dry_run: bool) -> Result<String, String> {
        self.inner.begin_execution(plan_id, dry_run)
    }

    fn acquire_apply_lease(
        &self,
        plan_id: &str,
        owner_id: &str,
    ) -> Result<music_folder_core::MutationLease, String> {
        self.inner.acquire_apply_lease(plan_id, owner_id)
    }

    fn heartbeat_apply_lease(
        &self,
        lease: &music_folder_core::MutationLease,
    ) -> Result<(), String> {
        self.inner.heartbeat_apply_lease(lease)
    }

    fn release_apply_lease(&self, lease: &music_folder_core::MutationLease) -> Result<(), String> {
        self.inner.release_apply_lease(lease)
    }

    fn prepare_operation(&self, intent: &OperationIntent) -> Result<(), String> {
        self.inner.prepare_operation(intent)
    }

    fn transition_operation(
        &self,
        operation_id: &str,
        from: JournalState,
        to: JournalState,
        lease: &music_folder_core::MutationLease,
        staged: Option<&StagedFile>,
        error: Option<&str>,
    ) -> Result<(), String> {
        if from == JournalState::ContentVerified
            && to == JournalState::Published
            && !self.fired.swap(true, Ordering::SeqCst)
        {
            // The filesystem publish already succeeded. Simulate a transient
            // SQLite write failure before the corresponding CAS is durable.
            return Err("journal_transition_db_failed".into());
        }
        self.inner
            .transition_operation(operation_id, from, to, lease, staged, error)
    }

    fn save_operation(
        &self,
        execution_id: &str,
        operation: &music_folder_core::OperationLog,
    ) -> Result<(), String> {
        self.inner.save_operation(execution_id, operation)
    }

    fn finish_execution(
        &self,
        execution_id: &str,
        status: RunStatus,
        success: u64,
        skipped: u64,
        failed: u64,
    ) -> Result<(), String> {
        self.inner
            .finish_execution(execution_id, status, success, skipped, failed)
    }

    fn record_metric(
        &self,
        run_id: &str,
        phase: &str,
        elapsed_ms: u64,
        item_count: u64,
    ) -> Result<(), String> {
        ApplyStore::record_metric(self.inner.as_ref(), run_id, phase, elapsed_ms, item_count)
    }
}

impl FaultInjectingFileSystem {
    fn new(fault: FaultInjection) -> Self {
        Self {
            fault,
            fired: AtomicBool::new(false),
        }
    }
}

impl FileMutator for FaultInjectingFileSystem {
    fn exists(&self, path: &Path) -> bool {
        FileMutator::exists(&LocalFileSystem, path)
    }

    fn same_volume(&self, source: &Path, target: &Path) -> Result<bool, String> {
        FileMutator::same_volume(&LocalFileSystem, source, target)
    }

    fn move_file(&self, source: &Path, target: &Path) -> Result<(), String> {
        FileMutator::move_file(&LocalFileSystem, source, target)
    }

    fn copy_file(&self, source: &Path, target: &Path) -> Result<(), String> {
        FileMutator::copy_file(&LocalFileSystem, source, target)
    }

    fn size(&self, path: &Path) -> Result<u64, String> {
        FileMutator::size(&LocalFileSystem, path)
    }

    fn delete_file(&self, path: &Path) -> Result<(), String> {
        FileMutator::delete_file(&LocalFileSystem, path)
    }

    fn delete_file_if_matches(
        &self,
        path: &Path,
        expected: &DeleteExpectation,
    ) -> Result<(), String> {
        if let FaultInjection::DeleteIo { code } = &self.fault {
            if !self.fired.swap(true, Ordering::SeqCst) {
                return Err((*code).to_owned());
            }
        }
        if let FaultInjection::DeleteRace { replacement } = &self.fault {
            if !self.fired.swap(true, Ordering::SeqCst) {
                let displaced = path.with_extension("displaced-by-delete-race");
                fs::rename(path, &displaced).map_err(|error| error.to_string())?;
                fs::write(path, replacement).map_err(|error| error.to_string())?;
            }
        }
        FileMutator::delete_file_if_matches(&LocalFileSystem, path, expected)
    }

    fn content_sha256(&self, path: &Path) -> Result<String, String> {
        FileMutator::content_sha256(&LocalFileSystem, path)
    }

    fn mutation_fingerprint(&self, path: &Path) -> Result<FileFingerprint, String> {
        FileMutator::mutation_fingerprint(&LocalFileSystem, path)
    }

    fn ensure_no_reparse_points(&self, path: &Path) -> Result<(), String> {
        FileMutator::ensure_no_reparse_points(&LocalFileSystem, path)
    }

    fn temporary_path(&self, target: &Path, operation_id: &str) -> Result<PathBuf, String> {
        FileMutator::temporary_path(&LocalFileSystem, target, operation_id)
    }

    fn stage_copy_exclusive(&self, source: &Path, temporary: &Path) -> Result<StagedFile, String> {
        if let FaultInjection::StageIo {
            code,
            leave_partial_before_cleanup,
        } = &self.fault
        {
            if !self.fired.swap(true, Ordering::SeqCst) {
                if *leave_partial_before_cleanup {
                    let parent = temporary
                        .parent()
                        .ok_or_else(|| "target_parent_missing".to_string())?;
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    fs::write(temporary, b"partial").map_err(|error| error.to_string())?;
                    // `stage_copy_exclusive` owns its unpublished temporary and
                    // must clean it before reporting an I/O failure.
                    fs::remove_file(temporary).map_err(|error| error.to_string())?;
                }
                return Err((*code).to_owned());
            }
        }
        if matches!(&self.fault, FaultInjection::StageShortWrite)
            && !self.fired.swap(true, Ordering::SeqCst)
        {
            let parent = temporary
                .parent()
                .ok_or_else(|| "target_parent_missing".to_string())?;
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            fs::write(temporary, b"short").map_err(|error| error.to_string())?;
            let observed = FileMutator::mutation_fingerprint(&LocalFileSystem, temporary)?;
            return Ok(StagedFile {
                temporary: temporary.to_path_buf(),
                size_bytes: observed.size_bytes,
                content_sha256: observed
                    .content_sha256
                    .ok_or_else(|| "staged_hash_missing".to_string())?,
                file_identity: observed.file_identity,
            });
        }
        if let FaultInjection::ReparseBeforeStage { outside } = &self.fault {
            if !self.fired.swap(true, Ordering::SeqCst) {
                let parent = temporary
                    .parent()
                    .ok_or_else(|| "target_parent_missing".to_string())?;
                let ancestor = parent
                    .parent()
                    .ok_or_else(|| "target_parent_missing".to_string())?;
                fs::create_dir_all(ancestor).map_err(|error| error.to_string())?;
                fs::create_dir_all(outside).map_err(|error| error.to_string())?;
                create_directory_link(outside, parent)?;
            }
        }
        FileMutator::stage_copy_exclusive(&LocalFileSystem, source, temporary)
    }

    fn publish_no_replace(&self, temporary: &Path, target: &Path) -> Result<(), String> {
        if let FaultInjection::PublishIo { code } = &self.fault {
            if !self.fired.swap(true, Ordering::SeqCst) {
                return Err((*code).to_owned());
            }
        }
        if let FaultInjection::PublishedIo { code } = &self.fault {
            if !self.fired.swap(true, Ordering::SeqCst) {
                FileMutator::publish_no_replace(&LocalFileSystem, temporary, target)?;
                return Err((*code).to_owned());
            }
        }
        if let FaultInjection::PublishRace { winner } = &self.fault {
            if !self.fired.swap(true, Ordering::SeqCst) {
                fs::write(target, winner).map_err(|error| error.to_string())?;
            }
        }
        FileMutator::publish_no_replace(&LocalFileSystem, temporary, target)
    }

    fn remove_staged_file(&self, temporary: &Path) -> Result<(), String> {
        FileMutator::remove_staged_file(&LocalFileSystem, temporary)
    }
}

fn journal_temporary(database: &Path, execution_id: &str) -> PathBuf {
    let (encoding, raw): (String, Vec<u8>) = Connection::open(database)
        .unwrap()
        .query_row(
            "SELECT temporary_path_encoding,temporary_path_blob
               FROM operation_journal WHERE attempt_id=?1",
            params![execution_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    music_folder_infra::path_codec::decode_path(&encoding, &raw).unwrap()
}

#[test]
fn stage_copy_fault_matrix_preserves_source_and_never_publishes_partial_bytes() {
    let _guard = mutation_boundary_test_guard();
    let cases = [
        (
            "disk_full_after_partial_write",
            FaultInjection::StageIo {
                code: "stage_disk_full",
                leave_partial_before_cleanup: true,
            },
            "stage_disk_full",
        ),
        (
            "permission_denied_before_write",
            FaultInjection::StageIo {
                code: "stage_permission_denied",
                leave_partial_before_cleanup: false,
            },
            "stage_permission_denied",
        ),
        (
            "short_write",
            FaultInjection::StageShortWrite,
            "staged_content_mismatch",
        ),
    ];

    for (case_name, fault, expected_error) in cases {
        let temp = tempdir().unwrap();
        let original = b"complete source bytes for stage fault";
        let plan = persisted_plan(temp.path(), original);
        let result = ApplyUseCase {
            store: Arc::clone(&plan.store),
            files: Arc::new(FaultInjectingFileSystem::new(fault)),
        }
        .execute(&plan.plan_id, false)
        .unwrap();

        assert_eq!(
            (result.success, result.skipped, result.failed),
            (0, 0, 1),
            "{case_name}"
        );
        assert_execution_terminal(&plan.database, &result.execution_id, "partial", 1);
        assert_eq!(
            operation_error(&plan.database, &result.execution_id),
            expected_error,
            "{case_name}"
        );
        assert_eq!(
            journal_state_and_error(&plan.database, &result.execution_id),
            ("failed".into(), Some(expected_error.into())),
            "{case_name}"
        );
        assert_eq!(fs::read(&plan.source).unwrap(), original, "{case_name}");
        assert!(!plan.target.exists(), "{case_name}");
        assert!(
            !journal_temporary(&plan.database, &result.execution_id).exists(),
            "{case_name}: unpublished temporary must be removed"
        );
    }
}

#[test]
fn publish_and_delete_io_fault_matrix_preserves_every_authoritative_copy() {
    let _guard = mutation_boundary_test_guard();

    let temp = tempdir().unwrap();
    let original = b"publish permission source";
    let plan = persisted_plan(temp.path(), original);
    let publish = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(FaultInjectingFileSystem::new(FaultInjection::PublishIo {
            code: "atomic_publish_permission_denied",
        })),
    }
    .execute(&plan.plan_id, false)
    .unwrap();
    assert_execution_terminal(&plan.database, &publish.execution_id, "partial", 1);
    assert_eq!(
        journal_state_and_error(&plan.database, &publish.execution_id),
        (
            "failed".into(),
            Some("atomic_publish_permission_denied".into())
        )
    );
    assert_eq!(fs::read(&plan.source).unwrap(), original);
    assert!(!plan.target.exists());
    assert!(!journal_temporary(&plan.database, &publish.execution_id).exists());

    let temp = tempdir().unwrap();
    let original = b"publish completed but flush reported failure";
    let plan = persisted_plan(temp.path(), original);
    let published = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(FaultInjectingFileSystem::new(FaultInjection::PublishedIo {
            code: "published_directory_flush_failed:fault_injected",
        })),
    }
    .execute(&plan.plan_id, false)
    .unwrap();
    assert_execution_terminal(
        &plan.database,
        &published.execution_id,
        "recovery_required",
        1,
    );
    assert_eq!(
        journal_state_and_error(&plan.database, &published.execution_id),
        (
            "recovery_required".into(),
            Some("published_directory_flush_failed:fault_injected".into())
        )
    );
    assert_eq!(fs::read(&plan.source).unwrap(), original);
    assert_eq!(fs::read(&plan.target).unwrap(), original);
    assert!(!journal_temporary(&plan.database, &published.execution_id).exists());

    let temp = tempdir().unwrap();
    let original = b"delete permission source";
    let plan = persisted_plan(temp.path(), original);
    let deletion = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(FaultInjectingFileSystem::new(FaultInjection::DeleteIo {
            code: "conditional_delete_permission_denied",
        })),
    }
    .execute(&plan.plan_id, false)
    .unwrap();
    assert_execution_terminal(
        &plan.database,
        &deletion.execution_id,
        "recovery_required",
        1,
    );
    assert_eq!(
        operation_error(&plan.database, &deletion.execution_id),
        "source_delete_failed:conditional_delete_permission_denied"
    );
    assert_eq!(
        journal_state_and_error(&plan.database, &deletion.execution_id),
        (
            "recovery_required".into(),
            Some("conditional_delete_permission_denied".into())
        )
    );
    assert_eq!(fs::read(&plan.source).unwrap(), original);
    assert_eq!(fs::read(&plan.target).unwrap(), original);
}

#[test]
fn database_failure_after_publish_is_recovery_required_and_keeps_both_copies() {
    let _guard = mutation_boundary_test_guard();
    let temp = tempdir().unwrap();
    let original = b"filesystem committed before database failure";
    let plan = persisted_plan(temp.path(), original);
    let fault_store = Arc::new(DatabaseTransitionFaultStore::new(Arc::clone(&plan.store)));
    let result = ApplyUseCase {
        store: fault_store,
        files: Arc::new(FaultInjectingFileSystem::new(FaultInjection::None)),
    }
    .execute(&plan.plan_id, false)
    .unwrap();

    assert_eq!((result.success, result.skipped, result.failed), (0, 0, 1));
    assert_execution_terminal(&plan.database, &result.execution_id, "recovery_required", 1);
    assert_eq!(fs::read(&plan.source).unwrap(), original);
    assert_eq!(fs::read(&plan.target).unwrap(), original);
    assert!(!journal_temporary(&plan.database, &result.execution_id).exists());
    assert_eq!(
        operation_error(&plan.database, &result.execution_id),
        "published_journal_transition_failed:journal_transition_db_failed"
    );
    assert_eq!(
        journal_state_and_error(&plan.database, &result.execution_id),
        (
            "recovery_required".into(),
            Some("published_journal_transition_failed:journal_transition_db_failed".into())
        )
    );
    let recoverable = plan.store.list_recovery_items().unwrap();
    assert_eq!(recoverable.len(), 1);
    assert_eq!(recoverable[0].state, JournalState::RecoveryRequired);
    assert_eq!(recoverable[0].suggested_action, RecoveryAction::Resume);
    let diagnostic = plan
        .store
        .list_diagnostics(Some(&result.execution_id), 100)
        .unwrap()
        .into_iter()
        .find(|event| {
            event.class == "recovery"
                && event.item_id.as_deref() == Some(plan.plan_item_id.as_str())
                && event.sequence_no == Some(1)
        })
        .expect("post-publish failure must persist item-correlated recovery evidence");
    assert_eq!(diagnostic.run_id.as_deref(), Some(plan.plan_id.as_str()));
    assert_eq!(
        diagnostic.attempt_id.as_deref(),
        Some(result.execution_id.as_str())
    );
    assert_eq!(diagnostic.message_key, "workflow_recovery_required");
    assert!(diagnostic.protected);
    assert!(diagnostic.payload["cause_chain"].is_array());
    assert!(diagnostic.presentation.summary_ja.contains("復旧"));
    assert!(diagnostic
        .presentation
        .next_action_ja
        .contains(&diagnostic.correlation_id));
    let serialized = serde_json::to_string(&diagnostic).unwrap();
    assert!(!serialized.contains(&plan.source.to_string_lossy().into_owned()));
    assert!(!serialized.contains(&plan.target.to_string_lossy().into_owned()));
}

#[test]
fn target_publish_race_preserves_the_winner_source_and_failed_journal() {
    let _guard = mutation_boundary_test_guard();
    let temp = tempdir().unwrap();
    let original = b"planned source bytes";
    let winner = b"concurrent winner bytes";
    let plan = persisted_plan(temp.path(), original);
    let files = Arc::new(FaultInjectingFileSystem::new(FaultInjection::PublishRace {
        winner: winner.to_vec(),
    }));

    let result = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files,
    }
    .execute(&plan.plan_id, false)
    .unwrap();

    assert_eq!((result.success, result.skipped, result.failed), (0, 0, 1));
    assert_execution_terminal(&plan.database, &result.execution_id, "partial", 1);
    assert_eq!(
        operation_error(&plan.database, &result.execution_id),
        "target_already_exists"
    );
    assert_eq!(
        journal_state_and_error(&plan.database, &result.execution_id),
        ("failed".into(), Some("target_already_exists".into()))
    );
    assert_eq!(fs::read(&plan.source).unwrap(), original);
    assert_eq!(fs::read(&plan.target).unwrap(), winner);
    let temporary: String = Connection::open(&plan.database)
        .unwrap()
        .query_row(
            "SELECT temporary_path FROM operation_journal WHERE attempt_id=?1",
            params![result.execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!Path::new(&temporary).exists());
}

#[test]
fn same_size_replacement_at_delete_boundary_is_retained_and_requires_recovery() {
    let _guard = mutation_boundary_test_guard();
    let temp = tempdir().unwrap();
    let original = b"planned-source";
    let replacement = b"raced-replace!";
    assert_eq!(original.len(), replacement.len());
    let plan = persisted_plan(temp.path(), original);
    let files = Arc::new(FaultInjectingFileSystem::new(FaultInjection::DeleteRace {
        replacement: replacement.to_vec(),
    }));

    let result = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files,
    }
    .execute(&plan.plan_id, false)
    .unwrap();

    assert_eq!((result.success, result.skipped, result.failed), (0, 0, 1));
    assert_execution_terminal(&plan.database, &result.execution_id, "recovery_required", 1);
    assert_eq!(fs::read(&plan.source).unwrap(), replacement);
    assert_eq!(fs::read(&plan.target).unwrap(), original);
    let (state, error) = journal_state_and_error(&plan.database, &result.execution_id);
    assert_eq!(state, "recovery_required");
    assert!(error
        .as_deref()
        .is_some_and(|value| value.starts_with("conditional_delete_")));
}

#[test]
fn rollback_delete_race_retains_replacement_and_requires_recovery() {
    let _guard = mutation_boundary_test_guard();
    let temp = tempdir().unwrap();
    let original = b"rollback-source";
    let replacement = b"rollback-raced!";
    assert_eq!(original.len(), replacement.len());
    let plan = persisted_plan(temp.path(), original);
    let applied = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&plan.plan_id, false)
    .unwrap();
    assert_eq!(
        applied.success,
        1,
        "{}",
        operation_error(&plan.database, &applied.execution_id)
    );
    let files = Arc::new(FaultInjectingFileSystem::new(FaultInjection::DeleteRace {
        replacement: replacement.to_vec(),
    }));

    let rollback = RollbackUseCase {
        store: Arc::clone(&plan.store),
        files,
    }
    .execute(&applied.execution_id, false)
    .unwrap();

    assert_eq!(
        (rollback.success, rollback.skipped, rollback.failed),
        (0, 0, 1)
    );
    let status: String = Connection::open(&plan.database)
        .unwrap()
        .query_row(
            "SELECT status FROM rollback_runs WHERE id=?1",
            params![rollback.rollback_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "recovery_required");
    assert_eq!(fs::read(&plan.source).unwrap(), original);
    assert_eq!(fs::read(&plan.target).unwrap(), replacement);
    assert_eq!(
        journal_state_and_error(&plan.database, &rollback.rollback_id).0,
        "recovery_required"
    );
}

#[test]
fn apply_dry_run_does_not_compete_with_a_held_mutation_lease() {
    let _guard = mutation_boundary_test_guard();
    let temp = tempdir().unwrap();
    let original = b"read-only preview";
    let plan = persisted_plan(temp.path(), original);
    let lease = plan
        .store
        .acquire_apply_lease(&plan.plan_id, "held-by-real-mutation")
        .unwrap();

    let preview = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&plan.plan_id, true)
    .unwrap();

    assert_eq!(
        (preview.success, preview.skipped, preview.failed),
        (1, 0, 0)
    );
    assert_eq!(fs::read(&plan.source).unwrap(), original);
    assert!(!plan.target.exists());
    plan.store.release_apply_lease(&lease).unwrap();
}

#[test]
fn rollback_dry_run_does_not_compete_with_a_held_mutation_lease() {
    let _guard = mutation_boundary_test_guard();
    let temp = tempdir().unwrap();
    let original = b"rollback preview";
    let plan = persisted_plan(temp.path(), original);
    let applied = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&plan.plan_id, false)
    .unwrap();
    assert_eq!(
        applied.success,
        1,
        "{}",
        operation_error(&plan.database, &applied.execution_id)
    );
    let lease = plan
        .store
        .acquire_rollback_lease(&applied.execution_id, "held-by-real-rollback")
        .unwrap();

    let preview = RollbackUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&applied.execution_id, true)
    .unwrap();

    assert_eq!(
        (preview.success, preview.skipped, preview.failed),
        (1, 0, 0)
    );
    assert!(!plan.source.exists());
    assert_eq!(fs::read(&plan.target).unwrap(), original);
    plan.store.release_rollback_lease(&lease).unwrap();
}

#[test]
fn repeated_rollback_rejects_a_changed_restored_source_without_mutation() {
    let _guard = mutation_boundary_test_guard();
    let temp = tempdir().unwrap();
    let original = b"rollback-original";
    let changed = b"rollback-tampered";
    assert_eq!(original.len(), changed.len());
    let plan = persisted_plan(temp.path(), original);
    let applied = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&plan.plan_id, false)
    .unwrap();
    assert_eq!(applied.success, 1);
    let first = RollbackUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&applied.execution_id, false)
    .unwrap();
    assert_eq!((first.success, first.failed), (1, 0));
    fs::write(&plan.source, changed).unwrap();

    let repeated = RollbackUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&applied.execution_id, false)
    .unwrap();
    assert_eq!(
        (repeated.success, repeated.skipped, repeated.failed),
        (0, 0, 1)
    );
    assert_eq!(fs::read(&plan.source).unwrap(), changed);
    assert!(!plan.target.exists());
    let error: String = Connection::open(&plan.database)
        .unwrap()
        .query_row(
            "SELECT error FROM rollback_logs WHERE rollback_run_id=?1",
            params![repeated.rollback_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(error, "completed_rollback_state_conflict");
}

#[test]
fn sqlite_busy_fails_before_a_run_or_mutation_and_releases_the_process_lock() {
    let _guard = mutation_boundary_test_guard();
    let temp = tempdir().unwrap();
    let original = b"database boundary bytes";
    let plan = persisted_plan(temp.path(), original);
    let busy_connection = Connection::open(&plan.database).unwrap();
    busy_connection.busy_timeout(Duration::ZERO).unwrap();
    busy_connection.execute_batch("BEGIN IMMEDIATE").unwrap();

    let error = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&plan.plan_id, false)
    .err()
    .expect("a held SQLite writer lock must reject apply");
    assert_eq!(error.code(), "database is locked");
    assert_eq!(fs::read(&plan.source).unwrap(), original);
    assert!(!plan.target.exists());
    busy_connection.execute_batch("ROLLBACK").unwrap();

    let connection = Connection::open(&plan.database).unwrap();
    let execution_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM execution_runs WHERE plan_id=?1",
            params![plan.plan_id],
            |row| row.get(0),
        )
        .unwrap();
    let journal_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM operation_journal", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(execution_count, 0);
    assert_eq!(journal_count, 0);

    let retry = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&plan.plan_id, true)
    .unwrap();
    assert_eq!((retry.success, retry.skipped, retry.failed), (1, 0, 0));
    assert_execution_terminal(&plan.database, &retry.execution_id, "completed", 0);
    assert_eq!(fs::read(&plan.source).unwrap(), original);
    assert!(!plan.target.exists());
}

#[test]
fn every_copy_journal_crash_boundary_is_reconciled_to_a_terminal_recoverable_state() {
    let _guard = mutation_boundary_test_guard();
    let boundaries = [
        (JournalState::Prepared, Some(RecoveryAction::Resume)),
        (
            JournalState::Staging,
            Some(RecoveryAction::DiscardUnpublishedTemporary),
        ),
        (
            JournalState::Staged,
            Some(RecoveryAction::DiscardUnpublishedTemporary),
        ),
        (JournalState::ContentVerified, Some(RecoveryAction::Resume)),
        (JournalState::Published, Some(RecoveryAction::Resume)),
        (JournalState::SourceDeleted, Some(RecoveryAction::Resume)),
        (JournalState::Completed, None),
    ];

    for (boundary, expected_action) in boundaries {
        let temp = tempdir().unwrap();
        let original = b"journal boundary bytes";
        let plan = persisted_plan(temp.path(), original);
        let files = LocalFileSystem;
        let lease = plan
            .store
            .acquire_apply_lease(&plan.plan_id, "boundary-crashed-owner")
            .unwrap();
        let execution_id = plan.store.begin_execution(&plan.plan_id, false).unwrap();
        let operation_id = Uuid::new_v4().to_string();
        let temporary = FileMutator::temporary_path(&files, &plan.target, &operation_id).unwrap();
        let expected_hash = plan.fingerprint.content_sha256.clone().unwrap();
        plan.store
            .prepare_operation(&OperationIntent {
                operation_id: operation_id.clone(),
                attempt_id: execution_id.clone(),
                plan_item_id: plan.plan_item_id.clone(),
                sequence_no: 1,
                direction: MutationDirection::Apply,
                strategy: music_folder_core::MutationStrategy::CopyPublishDelete,
                source: plan.source.clone(),
                target: plan.target.clone(),
                temporary: temporary.clone(),
                expected_size: Some(plan.fingerprint.size_bytes),
                expected_content_sha256: Some(expected_hash.clone()),
                expected_file_identity: plan.fingerprint.file_identity.clone(),
                fencing_token: lease.fencing_token,
            })
            .unwrap();

        let mut current = JournalState::Prepared;
        let mut staged = None;
        if boundary != JournalState::Prepared {
            plan.store
                .transition_operation(
                    &operation_id,
                    current,
                    JournalState::Staging,
                    &lease,
                    None,
                    None,
                )
                .unwrap();
            current = JournalState::Staging;
        }
        if matches!(
            boundary,
            JournalState::Staged
                | JournalState::ContentVerified
                | JournalState::Published
                | JournalState::SourceDeleted
                | JournalState::Completed
        ) {
            let copied =
                FileMutator::stage_copy_exclusive(&files, &plan.source, &temporary).unwrap();
            plan.store
                .transition_operation(
                    &operation_id,
                    current,
                    JournalState::Staged,
                    &lease,
                    Some(&copied),
                    None,
                )
                .unwrap();
            current = JournalState::Staged;
            staged = Some(copied);
        }
        if matches!(
            boundary,
            JournalState::ContentVerified
                | JournalState::Published
                | JournalState::SourceDeleted
                | JournalState::Completed
        ) {
            plan.store
                .transition_operation(
                    &operation_id,
                    current,
                    JournalState::ContentVerified,
                    &lease,
                    staged.as_ref(),
                    None,
                )
                .unwrap();
            current = JournalState::ContentVerified;
        }
        if matches!(
            boundary,
            JournalState::Published | JournalState::SourceDeleted | JournalState::Completed
        ) {
            FileMutator::publish_no_replace(&files, &temporary, &plan.target).unwrap();
            plan.store
                .transition_operation(
                    &operation_id,
                    current,
                    JournalState::Published,
                    &lease,
                    staged.as_ref(),
                    None,
                )
                .unwrap();
            current = JournalState::Published;
        }
        if matches!(
            boundary,
            JournalState::SourceDeleted | JournalState::Completed
        ) {
            FileMutator::delete_file_if_matches(
                &files,
                &plan.source,
                &DeleteExpectation {
                    size_bytes: plan.fingerprint.size_bytes,
                    content_sha256: expected_hash,
                    file_identity: plan.fingerprint.file_identity.clone(),
                },
            )
            .unwrap();
            plan.store
                .transition_operation(
                    &operation_id,
                    current,
                    JournalState::SourceDeleted,
                    &lease,
                    staged.as_ref(),
                    None,
                )
                .unwrap();
            current = JournalState::SourceDeleted;
        }
        if boundary == JournalState::Completed {
            plan.store
                .transition_operation(
                    &operation_id,
                    current,
                    JournalState::Completed,
                    &lease,
                    staged.as_ref(),
                    None,
                )
                .unwrap();
        }

        assert_eq!(
            journal_state_and_error(&plan.database, &execution_id).0,
            boundary.as_str()
        );
        drop(plan.store);
        let reopened = SqliteScanStore::open(&plan.database).unwrap();
        assert_execution_terminal(
            &plan.database,
            &execution_id,
            if boundary == JournalState::Completed {
                "completed"
            } else {
                "recovery_required"
            },
            0,
        );
        let (state, error) = journal_state_and_error(&plan.database, &execution_id);
        assert_eq!(state, boundary.as_str(), "{boundary:?}");
        if let Some(expected_action) = expected_action {
            assert!(
                error
                    .as_deref()
                    .is_some_and(|value| value.contains("mutation_lease_abandoned")),
                "{boundary:?}: {error:?}"
            );
            let recovery = reopened
                .list_recovery_items()
                .unwrap()
                .into_iter()
                .find(|item| item.operation_id == operation_id)
                .unwrap();
            assert_eq!(recovery.state, boundary);
            assert_eq!(recovery.suggested_action, expected_action);
        } else {
            assert!(error.is_none());
            assert!(reopened.list_recovery_items().unwrap().is_empty());
            let summary: (String, String, i64) = Connection::open(&plan.database)
                .unwrap()
                .query_row(
                    "SELECT action,result,source_deleted FROM operation_logs
                      WHERE execution_id=?1 AND sequence_no=1",
                    params![execution_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(summary, ("copy_delete".into(), "success".into(), 1));
            assert_eq!(
                reopened.successful_plan_item_ids(&plan.plan_id).unwrap(),
                std::slice::from_ref(&plan.plan_item_id)
            );
            let retried = ApplyUseCase {
                store: Arc::new(reopened),
                files: Arc::new(LocalFileSystem),
            }
            .execute(&plan.plan_id, false)
            .unwrap();
            assert_eq!(
                (retried.success, retried.skipped, retried.failed),
                (0, 1, 0)
            );
        }

        let source_expected = !matches!(
            boundary,
            JournalState::SourceDeleted | JournalState::Completed
        );
        let target_expected = matches!(
            boundary,
            JournalState::Published | JournalState::SourceDeleted | JournalState::Completed
        );
        let temporary_expected = matches!(
            boundary,
            JournalState::Staged | JournalState::ContentVerified
        );
        assert_eq!(plan.source.exists(), source_expected, "{boundary:?}");
        assert_eq!(plan.target.exists(), target_expected, "{boundary:?}");
        assert_eq!(temporary.exists(), temporary_expected, "{boundary:?}");
        if plan.source.exists() {
            assert_eq!(fs::read(&plan.source).unwrap(), original, "{boundary:?}");
        }
        if plan.target.exists() {
            assert_eq!(fs::read(&plan.target).unwrap(), original, "{boundary:?}");
        }
    }
}

#[test]
fn content_verified_crash_is_fenced_then_recovered_from_the_durable_journal() {
    let _guard = mutation_boundary_test_guard();
    let temp = tempdir().unwrap();
    let original = b"recoverable staged bytes";
    let plan = persisted_plan(temp.path(), original);
    let files = LocalFileSystem;
    let lease = plan
        .store
        .acquire_apply_lease(&plan.plan_id, "crashed-owner")
        .unwrap();
    let execution_id = plan.store.begin_execution(&plan.plan_id, false).unwrap();
    let operation_id = Uuid::new_v4().to_string();
    let temporary = FileMutator::temporary_path(&files, &plan.target, &operation_id).unwrap();
    let expected_hash = plan.fingerprint.content_sha256.clone().unwrap();
    plan.store
        .prepare_operation(&OperationIntent {
            operation_id: operation_id.clone(),
            attempt_id: execution_id.clone(),
            plan_item_id: plan.plan_item_id.clone(),
            sequence_no: 1,
            direction: MutationDirection::Apply,
            strategy: music_folder_core::MutationStrategy::CopyPublishDelete,
            source: plan.source.clone(),
            target: plan.target.clone(),
            temporary: temporary.clone(),
            expected_size: Some(plan.fingerprint.size_bytes),
            expected_content_sha256: Some(expected_hash),
            expected_file_identity: plan.fingerprint.file_identity.clone(),
            fencing_token: lease.fencing_token,
        })
        .unwrap();
    plan.store
        .transition_operation(
            &operation_id,
            JournalState::Prepared,
            JournalState::Staging,
            &lease,
            None,
            None,
        )
        .unwrap();
    let staged = FileMutator::stage_copy_exclusive(&files, &plan.source, &temporary).unwrap();
    plan.store
        .transition_operation(
            &operation_id,
            JournalState::Staging,
            JournalState::Staged,
            &lease,
            Some(&staged),
            None,
        )
        .unwrap();
    plan.store
        .transition_operation(
            &operation_id,
            JournalState::Staged,
            JournalState::ContentVerified,
            &lease,
            Some(&staged),
            None,
        )
        .unwrap();
    assert!(temporary.exists());
    assert!(plan.source.exists());
    assert!(!plan.target.exists());

    drop(plan.store);
    let reopened = Arc::new(SqliteScanStore::open(&plan.database).unwrap());
    let execution: (String, Option<i64>) = Connection::open(&plan.database)
        .unwrap()
        .query_row(
            "SELECT status,finished_at FROM execution_runs WHERE id=?1",
            params![execution_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(execution.0, "recovery_required");
    assert!(execution.1.is_some());
    let journal: (String, Option<String>) = Connection::open(&plan.database)
        .unwrap()
        .query_row(
            "SELECT state,error FROM operation_journal WHERE id=?1",
            params![operation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(journal.0, "content_verified");
    assert!(journal
        .1
        .as_deref()
        .is_some_and(|error| error.contains("mutation_lease_abandoned")));
    assert_eq!(
        reopened
            .transition_operation(
                &operation_id,
                JournalState::ContentVerified,
                JournalState::Failed,
                &lease,
                None,
                Some("stale writer"),
            )
            .unwrap_err(),
        "journal_transition_rejected"
    );
    let recoverable = reopened.list_recovery_items().unwrap();
    assert_eq!(recoverable.len(), 1);
    assert_eq!(recoverable[0].operation_id, operation_id);
    assert_eq!(recoverable[0].state, JournalState::ContentVerified);
    assert_eq!(recoverable[0].suggested_action, RecoveryAction::Resume);

    assert_eq!(
        reopened
            .acquire_apply_lease(&plan.plan_id, "apply-during-recovery")
            .unwrap_err(),
        "mutation_recovery_required"
    );
    let blocker = reopened
        .acquire_recovery_lease(&operation_id, "recovery-lease-blocker")
        .unwrap();
    let busy = RecoveryUseCase {
        store: Arc::clone(&reopened),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&operation_id, Some(RecoveryAction::Resume), false)
    .unwrap_err();
    assert_eq!(busy.code(), "mutation_scope_busy");
    let active_recovery_count: i64 = Connection::open(&plan.database)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM recovery_runs WHERE status='running'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_recovery_count, 0);
    reopened.release_recovery_lease(&blocker).unwrap();

    let transition_fault = Arc::new(RecoveryTransitionFaultStore::new(Arc::clone(&reopened)));
    let error = RecoveryUseCase {
        store: transition_fault,
        files: Arc::new(LocalFileSystem),
    }
    .execute(&operation_id, Some(RecoveryAction::Resume), false)
    .unwrap_err();
    assert_eq!(
        error.code(),
        "recovery_post_commit_journal_transition_failed:recovery_transition_db_failed"
    );
    assert!(plan.source.exists());
    assert_eq!(fs::read(&plan.target).unwrap(), original);
    assert!(!temporary.exists());
    assert_eq!(
        journal_state_and_error(&plan.database, &execution_id),
        (
            "recovery_required".into(),
            Some(
                "recovery_post_commit_journal_transition_failed:recovery_transition_db_failed"
                    .into()
            )
        )
    );
    let failed_recovery: (String, Option<i64>, Option<String>) = Connection::open(&plan.database)
        .unwrap()
        .query_row(
            "SELECT status,finished_at,error FROM recovery_runs
                  WHERE operation_id=?1 ORDER BY started_at DESC,id DESC LIMIT 1",
            params![operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(failed_recovery.0, "recovery_required");
    assert!(failed_recovery.1.is_some());
    assert_eq!(failed_recovery.2.as_deref(), Some(error.code()));

    let recovery = RecoveryUseCase {
        store: Arc::clone(&reopened),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&operation_id, Some(RecoveryAction::Resume), false)
    .unwrap();
    assert_eq!(recovery.action, RecoveryAction::Resume);
    assert!(!recovery.dry_run);
    assert!(!plan.source.exists());
    assert!(!temporary.exists());
    assert_eq!(fs::read(&plan.target).unwrap(), original);
    assert!(reopened.list_recovery_items().unwrap().is_empty());
    let verify_items = reopened
        .load_verify_subject_items(&recovery.recovery_id, VerifySubject::Recovery)
        .unwrap();
    assert_eq!(verify_items.len(), 1);
    assert_eq!(verify_items[0].action, OperationAction::CopyDelete);
    let connection = Connection::open(&plan.database).unwrap();
    let final_journal: String = connection
        .query_row(
            "SELECT state FROM operation_journal WHERE id=?1",
            params![operation_id],
            |row| row.get(0),
        )
        .unwrap();
    let recovery_status: (String, Option<i64>, String) = connection
        .query_row(
            "SELECT status,finished_at,action FROM recovery_runs WHERE id=?1",
            params![recovery.recovery_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(final_journal, "completed");
    assert_eq!(recovery_status.0, "completed");
    assert!(recovery_status.1.is_some());
    assert_eq!(recovery_status.2, "resume");
    let non_terminal_journal_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM operation_journal
              WHERE state NOT IN ('completed','failed')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(non_terminal_journal_count, 0);
    drop(connection);

    let parent_status: (String, i64, i64, String) = Connection::open(&plan.database)
        .unwrap()
        .query_row(
            "SELECT execution.status,execution.success_count,execution.failed_count,attempt.status
               FROM execution_runs execution
               JOIN workflow_attempts attempt ON attempt.id=execution.id
              WHERE execution.id=?1",
            params![execution_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        parent_status,
        ("completed".into(), 1, 0, "completed".into())
    );

    // Exercise archive protection on an isolated copy so the original graph
    // remains mutable for the rollback assertion below. A successful Recovery
    // Resume resolves the parent attempt, but the completed journal still
    // proves an unrolled mutation and must fence archive before any purge can
    // be considered.
    // The store uses WAL mode; checkpoint before a raw file copy so the copy
    // represents the same committed graph rather than an obsolete main DB
    // without its sidecar WAL records.
    Connection::open(&plan.database)
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    let purge_database = temp.path().join("purge-check.db");
    fs::copy(&plan.database, &purge_database).unwrap();
    let purge_store = SqliteScanStore::open(&purge_database).unwrap();
    let scan_id: String = Connection::open(&purge_database)
        .unwrap()
        .query_row(
            "SELECT scan_id FROM plan_runs WHERE id=?1",
            params![plan.plan_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        purge_store
            .archive_history(
                "scan",
                &scan_id,
                Some(&temp.path().join("purge-check-archive")),
            )
            .unwrap_err(),
        "history_archive_blocked:unrolled_mutation"
    );
    drop(purge_store);

    // A recovery that finishes the original apply journal must also make the
    // authoritative move visible to later rollback/verify, even though the
    // original Apply process died before it could write its operation summary.
    let rollback = RollbackUseCase {
        store: Arc::clone(&reopened),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&execution_id, false)
    .unwrap();
    let rollback_error: Option<String> = Connection::open(&plan.database)
        .unwrap()
        .query_row(
            "SELECT error FROM rollback_logs WHERE rollback_run_id=?1 ORDER BY created_at,id LIMIT 1",
            params![rollback.rollback_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        (rollback.success, rollback.skipped, rollback.failed),
        (1, 0, 0),
        "{rollback_error:?}"
    );
    assert_eq!(fs::read(&plan.source).unwrap(), original);
    assert!(!plan.target.exists());

    let verified = VerifyUseCase {
        store: Arc::clone(&reopened),
        files: Arc::new(LocalFileSystem),
    }
    .execute_subject(&rollback.rollback_id, VerifySubject::Rollback)
    .unwrap();
    assert_eq!((verified.success, verified.failed), (1, 0));
}

#[cfg(windows)]
#[test]
fn atomic_rename_crash_after_commit_is_reconciled_by_native_identity() {
    let _guard = mutation_boundary_test_guard();
    let temp = tempdir().unwrap();
    let original = b"atomic crash bytes";
    let plan = persisted_plan(temp.path(), original);
    let files = LocalFileSystem;
    let lease = plan
        .store
        .acquire_apply_lease(&plan.plan_id, "atomic-crashed-owner")
        .unwrap();
    let execution_id = plan.store.begin_execution(&plan.plan_id, false).unwrap();
    let operation_id = Uuid::new_v4().to_string();
    let expected_hash = plan.fingerprint.content_sha256.clone().unwrap();
    plan.store
        .prepare_operation(&OperationIntent {
            operation_id: operation_id.clone(),
            attempt_id: execution_id,
            plan_item_id: plan.plan_item_id.clone(),
            sequence_no: 1,
            direction: MutationDirection::Apply,
            strategy: music_folder_core::MutationStrategy::AtomicNoReplaceRename,
            source: plan.source.clone(),
            target: plan.target.clone(),
            temporary: PathBuf::new(),
            expected_size: Some(plan.fingerprint.size_bytes),
            expected_content_sha256: Some(expected_hash.clone()),
            expected_file_identity: plan.fingerprint.file_identity.clone(),
            fencing_token: lease.fencing_token,
        })
        .unwrap();
    plan.store
        .transition_operation(
            &operation_id,
            JournalState::Prepared,
            JournalState::ContentVerified,
            &lease,
            None,
            None,
        )
        .unwrap();
    let moved = files
        .atomic_move_no_replace_if_matches(
            &plan.source,
            &plan.target,
            &DeleteExpectation {
                size_bytes: plan.fingerprint.size_bytes,
                content_sha256: expected_hash,
                file_identity: plan.fingerprint.file_identity.clone(),
            },
        )
        .unwrap();
    assert_eq!(moved.file_identity, plan.fingerprint.file_identity);
    assert!(!plan.source.exists());
    assert_eq!(fs::read(&plan.target).unwrap(), original);

    // Simulate process death after the atomic filesystem commit but before the
    // ContentVerified -> Published journal compare-and-swap.
    drop(plan.store);
    let reopened = Arc::new(SqliteScanStore::open(&plan.database).unwrap());
    let item = reopened
        .list_recovery_items()
        .unwrap()
        .into_iter()
        .find(|item| item.operation_id == operation_id)
        .unwrap();
    assert_eq!(
        item.strategy,
        music_folder_core::MutationStrategy::AtomicNoReplaceRename
    );
    assert_eq!(item.state, JournalState::ContentVerified);
    assert_eq!(item.suggested_action, RecoveryAction::Resume);

    let recovered = RecoveryUseCase {
        store: Arc::clone(&reopened),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&operation_id, Some(RecoveryAction::Resume), false)
    .unwrap();
    assert!(!plan.source.exists());
    assert_eq!(fs::read(&plan.target).unwrap(), original);
    let verify_items = reopened
        .load_verify_subject_items(&recovered.recovery_id, VerifySubject::Recovery)
        .unwrap();
    assert_eq!(verify_items.len(), 1);
    assert_eq!(verify_items[0].action, OperationAction::Move);
    let state: String = Connection::open(&plan.database)
        .unwrap()
        .query_row(
            "SELECT state FROM operation_journal WHERE id=?1",
            params![operation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "completed");
}

#[cfg(any(unix, windows))]
#[test]
fn target_parent_reparse_swap_is_rejected_without_escaping_the_target_root() {
    let _guard = mutation_boundary_test_guard();
    let temp = tempdir().unwrap();
    let original = b"reparse boundary bytes";
    let plan = persisted_plan(temp.path(), original);
    let outside = temp.path().join("outside");
    let files = Arc::new(FaultInjectingFileSystem::new(
        FaultInjection::ReparseBeforeStage {
            outside: outside.clone(),
        },
    ));

    let result = ApplyUseCase {
        store: Arc::clone(&plan.store),
        files,
    }
    .execute(&plan.plan_id, false)
    .unwrap();

    assert_eq!((result.success, result.skipped, result.failed), (0, 0, 1));
    assert_execution_terminal(&plan.database, &result.execution_id, "partial", 1);
    let error = operation_error(&plan.database, &result.execution_id);
    if cfg!(windows) && error.contains("(os error 1314)") {
        eprintln!(
            "skipping reparse swap assertion: this Windows account lacks SeCreateSymbolicLinkPrivilege"
        );
        return;
    }
    assert!(error.starts_with("reparse_point_forbidden:"), "{error}");
    let (state, journal_error) = journal_state_and_error(&plan.database, &result.execution_id);
    assert_eq!(state, "failed");
    assert_eq!(journal_error.as_deref(), Some(error.as_str()));
    assert_eq!(fs::read(&plan.source).unwrap(), original);
    assert!(!plan.target.exists());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    assert!(plan.target.starts_with(&plan.target_root));
}

#[cfg(unix)]
fn create_directory_link(target: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link).map_err(|error| error.to_string())
}

#[cfg(windows)]
fn create_directory_link(target: &Path, link: &Path) -> Result<(), String> {
    std::os::windows::fs::symlink_dir(target, link).map_err(|error| error.to_string())
}
