use music_folder_core::{
    ports::{ApplyStore, RecoveryStore, ScanStore},
    usecases::{PlanOptions, PlanUseCase},
    FileFingerprint, FileKind, JournalState, MutationDirection, NamingRules, OperationIntent,
    RunStatus, ScannedFile, TrackMetadata,
};
use music_folder_infra::sqlite::SqliteScanStore;
use rusqlite::{params, Connection};
use std::{
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};
use tempfile::tempdir;
use uuid::Uuid;

static MUTATION_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn mutation_test_guard() -> std::sync::MutexGuard<'static, ()> {
    MUTATION_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn plan(store: Arc<SqliteScanStore>, source: &Path, target_root: &Path) -> String {
    let scan_id = store.begin_scan(source.parent().unwrap()).unwrap();
    store
        .save_batch(
            &scan_id,
            &[ScannedFile {
                id: Uuid::new_v4(),
                path: source.to_path_buf(),
                fingerprint: FileFingerprint {
                    size_bytes: 7,
                    mtime_ns: 11,
                    content_sha256: Some("content-hash".into()),
                    file_identity: Some("file-id".into()),
                    version: 1,
                },
                metadata: Some(TrackMetadata {
                    artist: Some("Artist".into()),
                    album_artist: None,
                    album: Some("Album".into()),
                    title: Some("Title".into()),
                    track_no: Some(1),
                    disc_no: Some(1),
                    year: Some(2026),
                }),
                kind: FileKind::Music,
            }],
        )
        .unwrap();
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();
    PlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &scan_id,
        &PlanOptions {
            target_root: target_root.to_path_buf(),
            batch_size: 8,
            naming: NamingRules::default(),
        },
    )
    .unwrap()
    .plan_id
}

fn operation_intent(
    execution_id: &str,
    operation_id: &str,
    fencing_token: u64,
    source: &Path,
    target_root: &Path,
) -> OperationIntent {
    OperationIntent {
        operation_id: operation_id.to_owned(),
        attempt_id: execution_id.to_owned(),
        plan_item_id: "item".into(),
        sequence_no: 1,
        direction: MutationDirection::Apply,
        strategy: music_folder_core::MutationStrategy::CopyPublishDelete,
        source: source.to_path_buf(),
        target: target_root.join("Artist").join("track.mp3"),
        temporary: target_root.join("Artist").join(".stage"),
        expected_size: Some(7),
        expected_content_sha256: Some("content-hash".into()),
        expected_file_identity: Some("file-id".into()),
        fencing_token,
    }
}

#[test]
fn nested_target_roots_are_mutually_exclusive() {
    let _guard = mutation_test_guard();
    let temp = tempdir().unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("lease.db")).unwrap());
    let outer = temp.path().join("library");
    let inner = outer.join("nested");
    let first_plan = plan(
        Arc::clone(&store),
        &temp.path().join("source-a").join("a.mp3"),
        &outer,
    );
    let second_plan = plan(
        Arc::clone(&store),
        &temp.path().join("source-b").join("b.mp3"),
        &inner,
    );

    let lease = store.acquire_apply_lease(&first_plan, "owner-a").unwrap();
    let leased_scope_count: i64 = Connection::open(store.database_path())
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM mutation_leases WHERE fencing_token=?1",
            params![lease.fencing_token as i64],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        leased_scope_count, 2,
        "source and target roots are both leased"
    );
    assert_eq!(
        store
            .acquire_apply_lease(&second_plan, "owner-b")
            .unwrap_err(),
        "mutation_scope_busy"
    );
    store.release_apply_lease(&lease).unwrap();
    let second = store.acquire_apply_lease(&second_plan, "owner-b").unwrap();
    assert!(second.fencing_token > lease.fencing_token);
    store.release_apply_lease(&second).unwrap();
}

#[test]
fn source_and_target_scopes_are_acquired_as_one_lease() {
    let _guard = mutation_test_guard();
    let temp = tempdir().unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("source-scope.db")).unwrap());
    let shared_source = temp.path().join("shared-source");
    let first_plan = plan(
        Arc::clone(&store),
        &shared_source.join("first").join("a.mp3"),
        &temp.path().join("target-a"),
    );
    let second_plan = plan(
        Arc::clone(&store),
        &shared_source.join("first").join("nested").join("b.mp3"),
        &temp.path().join("target-b"),
    );

    let lease = store.acquire_apply_lease(&first_plan, "owner-a").unwrap();
    assert_eq!(
        store
            .acquire_apply_lease(&second_plan, "owner-b")
            .unwrap_err(),
        "mutation_scope_busy"
    );
    store.release_apply_lease(&lease).unwrap();
}

#[test]
fn unrelated_sibling_roots_can_hold_leases_concurrently() {
    let _guard = mutation_test_guard();
    let temp = tempdir().unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("siblings.db")).unwrap());
    let first_plan = plan(
        Arc::clone(&store),
        &temp.path().join("source-a").join("a.mp3"),
        &temp.path().join("target-a"),
    );
    let second_plan = plan(
        Arc::clone(&store),
        &temp.path().join("source-b").join("b.mp3"),
        &temp.path().join("target-b"),
    );

    let first = store.acquire_apply_lease(&first_plan, "owner-a").unwrap();
    let second = store.acquire_apply_lease(&second_plan, "owner-b").unwrap();
    store.release_apply_lease(&second).unwrap();
    store.release_apply_lease(&first).unwrap();
}

#[test]
fn overlapping_roots_are_exclusive_across_distinct_databases() {
    let _guard = mutation_test_guard();
    let temp = tempdir().unwrap();
    let first_store = Arc::new(SqliteScanStore::open(&temp.path().join("first.db")).unwrap());
    let second_store = Arc::new(SqliteScanStore::open(&temp.path().join("second.db")).unwrap());
    let shared_target = temp.path().join("shared-target");
    let first_plan = plan(
        Arc::clone(&first_store),
        &temp.path().join("source-a").join("a.mp3"),
        &shared_target,
    );
    let second_plan = plan(
        Arc::clone(&second_store),
        &temp.path().join("source-b").join("b.mp3"),
        &shared_target.join("nested"),
    );

    let first = first_store
        .acquire_apply_lease(&first_plan, "owner-a")
        .unwrap();
    assert_eq!(
        second_store
            .acquire_apply_lease(&second_plan, "owner-b")
            .unwrap_err(),
        "mutation_scope_busy"
    );
    first_store.release_apply_lease(&first).unwrap();
    let second = second_store
        .acquire_apply_lease(&second_plan, "owner-b")
        .unwrap();
    second_store.release_apply_lease(&second).unwrap();
}

#[cfg(windows)]
#[test]
fn overlapping_roots_are_exclusive_across_processes_and_databases() {
    use std::process::Command;

    let _guard = mutation_test_guard();
    let temp = tempdir().unwrap();
    let first_store = Arc::new(SqliteScanStore::open(&temp.path().join("parent.db")).unwrap());
    let child_database = temp.path().join("child.db");
    let child_store = Arc::new(SqliteScanStore::open(&child_database).unwrap());
    let shared_target = temp.path().join("shared-target");
    let parent_plan = plan(
        Arc::clone(&first_store),
        &temp.path().join("source-a").join("a.mp3"),
        &shared_target,
    );
    let child_plan = plan(
        child_store,
        &temp.path().join("source-b").join("b.mp3"),
        &shared_target.join("nested"),
    );
    let parent_lease = first_store
        .acquire_apply_lease(&parent_plan, "parent-owner")
        .unwrap();

    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process_lease_attempt_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("MFB_PROCESS_LEASE_DB", &child_database)
        .env("MFB_PROCESS_LEASE_PLAN", &child_plan)
        .output()
        .unwrap();
    assert!(output.status.success(), "child process failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("MFB_PROCESS_LEASE_RESULT=mutation_scope_busy"),
        "unexpected child output: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    first_store.release_apply_lease(&parent_lease).unwrap();
}

#[cfg(windows)]
#[test]
fn process_lease_attempt_helper() {
    let Ok(database) = std::env::var("MFB_PROCESS_LEASE_DB") else {
        return;
    };
    let plan_id = std::env::var("MFB_PROCESS_LEASE_PLAN").unwrap();
    let store = SqliteScanStore::open(Path::new(&database)).unwrap();
    let result = match store.acquire_apply_lease(&plan_id, "child-owner") {
        Ok(lease) => {
            store.release_apply_lease(&lease).unwrap();
            "acquired".to_string()
        }
        Err(error) => error,
    };
    println!("MFB_PROCESS_LEASE_RESULT={result}");
}

#[test]
fn expired_live_owner_renews_every_scope_before_prepare_and_transition() {
    let _guard = mutation_test_guard();
    let temp = tempdir().unwrap();
    let database = temp.path().join("long-operation.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let source = temp.path().join("source").join("track.mp3");
    let target_root = temp.path().join("library");
    let plan_id = plan(Arc::clone(&store), &source, &target_root);
    let lease = store.acquire_apply_lease(&plan_id, "long-owner").unwrap();
    let execution_id = store.begin_execution(&plan_id, false).unwrap();
    let operation_id = Uuid::new_v4().to_string();

    let expire = || {
        Connection::open(&database)
            .unwrap()
            .execute(
                "UPDATE mutation_leases SET heartbeat_at=-1,expires_at=0
                  WHERE fencing_token=?1",
                params![lease.fencing_token as i64],
            )
            .unwrap();
    };

    expire();
    store.heartbeat_apply_lease(&lease).unwrap();
    expire();
    store
        .prepare_operation(&operation_intent(
            &execution_id,
            &operation_id,
            lease.fencing_token,
            &source,
            &target_root,
        ))
        .unwrap();
    store
        .transition_operation(
            &operation_id,
            JournalState::Prepared,
            JournalState::Staging,
            &lease,
            None,
            None,
        )
        .unwrap();

    // Simulate a single filesystem item running past the wall-clock TTL. A
    // concurrent opener must observe the still-held process guard and must not
    // reconcile this owner merely because the persisted deadline elapsed.
    expire();
    let observer = SqliteScanStore::open(&database).unwrap();
    assert!(observer.list_recovery_items().unwrap().is_empty());
    store
        .transition_operation(
            &operation_id,
            JournalState::Staging,
            JournalState::Staged,
            &lease,
            None,
            None,
        )
        .unwrap();

    let (count, minimum_expiry, maximum_expiry, state, error): (
        i64,
        i64,
        i64,
        String,
        Option<String>,
    ) = Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT COUNT(*),MIN(lease.expires_at),MAX(lease.expires_at),
                    journal.state,journal.error
               FROM mutation_leases lease
               JOIN operation_journal journal ON journal.id=?2
              WHERE lease.fencing_token=?1",
            params![lease.fencing_token as i64, operation_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(count, 2);
    assert!(minimum_expiry > 0);
    assert_eq!(minimum_expiry, maximum_expiry);
    assert_eq!(state, "staged");
    assert_eq!(error, None);

    drop(observer);
    store.release_apply_lease(&lease).unwrap();
}

#[test]
fn unrelated_acquire_does_not_delete_an_expired_live_owners_rows() {
    let _guard = mutation_test_guard();
    let temp = tempdir().unwrap();
    let database = temp.path().join("unrelated-expired.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let first_plan = plan(
        Arc::clone(&store),
        &temp.path().join("source-a").join("a.mp3"),
        &temp.path().join("target-a"),
    );
    let second_plan = plan(
        Arc::clone(&store),
        &temp.path().join("source-b").join("b.mp3"),
        &temp.path().join("target-b"),
    );
    let first = store.acquire_apply_lease(&first_plan, "owner-a").unwrap();
    Connection::open(&database)
        .unwrap()
        .execute(
            "UPDATE mutation_leases SET heartbeat_at=-1,expires_at=0
              WHERE fencing_token=?1",
            params![first.fencing_token as i64],
        )
        .unwrap();

    let second = store.acquire_apply_lease(&second_plan, "owner-b").unwrap();
    store.heartbeat_apply_lease(&first).unwrap();
    let first_scope_count: i64 = Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM mutation_leases WHERE fencing_token=?1",
            params![first.fencing_token as i64],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(first_scope_count, 2);

    store.release_apply_lease(&second).unwrap();
    store.release_apply_lease(&first).unwrap();
}

#[test]
fn prepare_rejects_a_lease_with_one_scope_row_missing() {
    let _guard = mutation_test_guard();
    let temp = tempdir().unwrap();
    let database = temp.path().join("partial-prepare.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let source = temp.path().join("source").join("track.mp3");
    let target_root = temp.path().join("library");
    let plan_id = plan(Arc::clone(&store), &source, &target_root);
    let lease = store
        .acquire_apply_lease(&plan_id, "partial-owner")
        .unwrap();
    let execution_id = store.begin_execution(&plan_id, false).unwrap();
    let operation_id = Uuid::new_v4().to_string();
    Connection::open(&database)
        .unwrap()
        .execute(
            "DELETE FROM mutation_leases
              WHERE scope_key=(SELECT MIN(scope_key) FROM mutation_leases
                                WHERE fencing_token=?1)",
            params![lease.fencing_token as i64],
        )
        .unwrap();

    assert_eq!(
        store
            .prepare_operation(&operation_intent(
                &execution_id,
                &operation_id,
                lease.fencing_token,
                &source,
                &target_root,
            ))
            .unwrap_err(),
        "journal_prepare_rejected"
    );
    let journal_count: i64 = Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM operation_journal WHERE id=?1",
            params![operation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(journal_count, 0);
    assert_eq!(
        store.release_apply_lease(&lease).unwrap_err(),
        "mutation_lease_stale"
    );
}

#[test]
fn transition_rejects_a_lease_with_one_scope_row_missing() {
    let _guard = mutation_test_guard();
    let temp = tempdir().unwrap();
    let database = temp.path().join("partial-transition.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let source = temp.path().join("source").join("track.mp3");
    let target_root = temp.path().join("library");
    let plan_id = plan(Arc::clone(&store), &source, &target_root);
    let lease = store
        .acquire_apply_lease(&plan_id, "partial-owner")
        .unwrap();
    let execution_id = store.begin_execution(&plan_id, false).unwrap();
    let operation_id = Uuid::new_v4().to_string();
    store
        .prepare_operation(&operation_intent(
            &execution_id,
            &operation_id,
            lease.fencing_token,
            &source,
            &target_root,
        ))
        .unwrap();
    Connection::open(&database)
        .unwrap()
        .execute(
            "DELETE FROM mutation_leases
              WHERE scope_key=(SELECT MIN(scope_key) FROM mutation_leases
                                WHERE fencing_token=?1)",
            params![lease.fencing_token as i64],
        )
        .unwrap();

    assert_eq!(
        store
            .transition_operation(
                &operation_id,
                JournalState::Prepared,
                JournalState::Staging,
                &lease,
                None,
                None,
            )
            .unwrap_err(),
        "journal_transition_rejected"
    );
    let state: String = Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT state FROM operation_journal WHERE id=?1",
            params![operation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "prepared");
    assert_eq!(
        store.release_apply_lease(&lease).unwrap_err(),
        "mutation_lease_stale"
    );
}

#[test]
fn abandoned_journal_is_recovered_and_stale_owner_is_fenced() {
    let _guard = mutation_test_guard();
    let temp = tempdir().unwrap();
    let database = temp.path().join("journal.db");
    let target_root = temp.path().join("library");
    let (execution_id, operation_id, stale_lease) = {
        let store = Arc::new(SqliteScanStore::open(&database).unwrap());
        let plan_id = plan(
            Arc::clone(&store),
            &temp.path().join("source").join("track.mp3"),
            &target_root,
        );
        let lease = store.acquire_apply_lease(&plan_id, "dead-owner").unwrap();
        let execution_id = store.begin_execution(&plan_id, false).unwrap();
        let operation_id = Uuid::new_v4().to_string();
        store
            .prepare_operation(&OperationIntent {
                operation_id: operation_id.clone(),
                attempt_id: execution_id.clone(),
                plan_item_id: "item".into(),
                sequence_no: 1,
                direction: MutationDirection::Apply,
                strategy: music_folder_core::MutationStrategy::CopyPublishDelete,
                source: temp.path().join("source").join("track.mp3"),
                target: target_root.join("Artist").join("track.mp3"),
                temporary: target_root.join("Artist").join(".stage"),
                expected_size: Some(7),
                expected_content_sha256: Some("content-hash".into()),
                expected_file_identity: Some("file-id".into()),
                fencing_token: lease.fencing_token,
            })
            .unwrap();
        store
            .transition_operation(
                &operation_id,
                JournalState::Prepared,
                JournalState::Staging,
                &lease,
                None,
                None,
            )
            .unwrap();
        (execution_id, operation_id, lease)
    };

    let reopened = SqliteScanStore::open(&database).unwrap();
    let recovery = reopened.list_recovery_items().unwrap();
    assert_eq!(recovery.len(), 1);
    assert_eq!(recovery[0].operation_id, operation_id);
    assert_eq!(recovery[0].state, JournalState::Staging);
    assert_eq!(
        recovery[0].expected_file_identity.as_deref(),
        Some("file-id")
    );
    let diagnostic = reopened
        .list_diagnostics(Some(&execution_id), 20)
        .unwrap()
        .into_iter()
        .find(|event| event.code == "mutation_lease_abandoned")
        .expect("lease reconciliation must leave correlated recovery evidence");
    assert_eq!(
        diagnostic.attempt_id.as_deref(),
        Some(execution_id.as_str())
    );
    assert!(diagnostic.run_id.is_some());
    assert_eq!(diagnostic.item_id.as_deref(), Some("item"));
    assert_eq!(diagnostic.sequence_no, Some(1));
    assert_eq!(diagnostic.class, "recovery");
    assert_eq!(diagnostic.severity, "error");
    assert_eq!(diagnostic.message_key, "diagnostic_recovery_required");
    assert_eq!(
        diagnostic.payload["cause_chain"],
        serde_json::json!(["mutation_lease_abandoned"])
    );
    assert!(diagnostic.presentation.summary_ja.contains("復旧"));
    let serialized = serde_json::to_string(&diagnostic).unwrap();
    assert!(!serialized.contains(&target_root.to_string_lossy().into_owned()));
    let recovery_lease = reopened
        .acquire_recovery_lease(&operation_id, "recovery-owner")
        .unwrap();
    assert!(recovery_lease.fencing_token > stale_lease.fencing_token);
    assert_eq!(
        reopened
            .transition_operation(
                &operation_id,
                JournalState::Staging,
                JournalState::Failed,
                &stale_lease,
                None,
                Some("late-write"),
            )
            .unwrap_err(),
        "journal_transition_rejected"
    );
    let active_fencing_token: i64 = Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT fencing_token FROM operation_journal WHERE id=?1",
            params![operation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_fencing_token, recovery_lease.fencing_token as i64);
    reopened.release_recovery_lease(&recovery_lease).unwrap();
    let status: String = Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT status FROM execution_runs WHERE id=?1",
            params![execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "recovery_required");
}
