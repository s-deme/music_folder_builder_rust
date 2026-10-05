use music_folder_core::{
    ports::{ApplyStore, RollbackStore, ScanStore, VerifyStore},
    usecases::{PlanOptions, PlanUseCase},
    FileFingerprint, FileKind, JournalState, MutationDirection, MutationStrategy, OperationAction,
    OperationIntent, OperationLog, OperationResult, RunStatus, ScannedFile, TrackMetadata,
    VerifySubject,
};
use music_folder_infra::sqlite::SqliteScanStore;
use rusqlite::{params, Connection};
use std::{
    collections::HashSet,
    fs,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};
use tempfile::tempdir;
use uuid::Uuid;

fn mutation_guard() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn completed_plan(
    store: Arc<SqliteScanStore>,
    source: &Path,
    target_root: &Path,
) -> (String, String, String) {
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
    let plan = PlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &scan_id,
        &PlanOptions {
            target_root: target_root.to_path_buf(),
            batch_size: 8,
            naming: music_folder_core::NamingRules::default(),
        },
    )
    .unwrap();
    let plan_item_id: String = Connection::open(store.database_path())
        .unwrap()
        .query_row(
            "SELECT id FROM plan_items WHERE plan_id=?1",
            params![plan.plan_id],
            |row| row.get(0),
        )
        .unwrap();
    (scan_id, plan.plan_id, plan_item_id)
}

#[test]
fn verify_and_rollback_retries_have_distinct_ordered_attempts_and_logs() {
    let _guard = mutation_guard();
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("attempts.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let source = temporary.path().join("source").join("track.mp3");
    let target = temporary.path().join("target");
    let (_scan_id, plan_id, plan_item_id) = completed_plan(Arc::clone(&store), &source, &target);

    let execution_id = store.begin_execution(&plan_id, false).unwrap();
    store
        .save_operation(
            &execution_id,
            &OperationLog {
                plan_item_id,
                sequence_no: 1,
                source,
                target: Some(target.join("Artist").join("track.mp3")),
                action: OperationAction::CopyDelete,
                result: OperationResult::Success,
                error: None,
                source_deleted: true,
                expected_size: Some(7),
                expected_content_sha256: Some("content-hash".into()),
                expected_file_identity: Some("file-id".into()),
            },
        )
        .unwrap();
    store
        .finish_execution(&execution_id, RunStatus::Completed, 1, 0, 0)
        .unwrap();
    let operation_id: String = Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT id FROM operation_logs WHERE execution_id=?1",
            params![execution_id],
            |row| row.get(0),
        )
        .unwrap();

    let verify_one = store.begin_verify(&execution_id).unwrap();
    store
        .save_verify_result(&verify_one, &operation_id, OperationResult::Success, None)
        .unwrap();
    store
        .finish_verify(&verify_one, RunStatus::Completed, 1, 0)
        .unwrap();
    let verify_two = store.begin_verify(&execution_id).unwrap();
    store
        .save_verify_result(
            &verify_two,
            &operation_id,
            OperationResult::Failed,
            Some("second_attempt_mismatch"),
        )
        .unwrap();
    store
        .finish_verify(&verify_two, RunStatus::Failed, 0, 1)
        .unwrap();

    let rollback_one = store.begin_rollback(&execution_id, true).unwrap();
    store
        .save_rollback_result(&rollback_one, &operation_id, OperationResult::Success, None)
        .unwrap();
    store
        .finish_rollback(&rollback_one, RunStatus::Completed, 1, 0, 0)
        .unwrap();
    let rollback_two = store.begin_rollback(&execution_id, true).unwrap();
    store
        .save_rollback_result(
            &rollback_two,
            &operation_id,
            OperationResult::Failed,
            Some("second_attempt_failed"),
        )
        .unwrap();
    store
        .finish_rollback(&rollback_two, RunStatus::Partial, 0, 0, 1)
        .unwrap();

    let database = Connection::open(&database).unwrap();
    let verify_attempts: Vec<(String, i64, Option<String>, String, i64, i64)> = database
        .prepare(
            "SELECT id,attempt_no,predecessor_attempt_id,status,success_count,failed_count
               FROM workflow_attempts WHERE kind='verify' AND parent_attempt_id=?1
              ORDER BY attempt_no",
        )
        .unwrap()
        .query_map(params![execution_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(verify_attempts.len(), 2);
    assert_eq!(
        verify_attempts[0],
        (verify_one.clone(), 1, None, "completed".into(), 1, 0)
    );
    assert_eq!(verify_attempts[1].0, verify_two);
    assert_eq!(verify_attempts[1].1, 2);
    assert_eq!(verify_attempts[1].2.as_deref(), Some(verify_one.as_str()));
    assert_eq!(&verify_attempts[1].3, "failed");
    assert_eq!((verify_attempts[1].4, verify_attempts[1].5), (0, 1));

    let rollback_attempts: Vec<(String, i64, Option<String>, String)> = database
        .prepare(
            "SELECT id,attempt_no,predecessor_attempt_id,status
               FROM workflow_attempts
              WHERE kind='rollback_dry_run' AND parent_attempt_id=?1
              ORDER BY attempt_no",
        )
        .unwrap()
        .query_map(params![execution_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rollback_attempts.len(), 2);
    assert_eq!(rollback_attempts[0].0, rollback_one);
    assert_eq!(rollback_attempts[0].1, 1);
    assert_eq!(rollback_attempts[1].0, rollback_two);
    assert_eq!(rollback_attempts[1].1, 2);
    assert_eq!(
        rollback_attempts[1].2.as_deref(),
        Some(rollback_one.as_str())
    );
    assert_eq!(&rollback_attempts[1].3, "partial");

    for attempt in [&verify_one, &verify_two] {
        let count: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM verify_logs WHERE verify_run_id=?1",
                params![attempt],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
    for attempt in [&rollback_one, &rollback_two] {
        let count: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM rollback_logs WHERE rollback_run_id=?1",
                params![attempt],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}

#[test]
fn journal_events_are_ordered_fenced_archived_and_purged_with_the_workflow() {
    let _guard = mutation_guard();
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("journal-events.db");
    let archive_directory = temporary.path().join("archives");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let source = temporary.path().join("source").join("track.mp3");
    let target_root = temporary.path().join("target");
    let (scan_id, plan_id, plan_item_id) =
        completed_plan(Arc::clone(&store), &source, &target_root);
    let lease = store
        .acquire_apply_lease(&plan_id, "journal-owner")
        .unwrap();
    let execution_id = store.begin_execution(&plan_id, false).unwrap();
    let operation_id = Uuid::new_v4().to_string();
    store
        .prepare_operation(&OperationIntent {
            operation_id: operation_id.clone(),
            attempt_id: execution_id.clone(),
            plan_item_id,
            sequence_no: 1,
            direction: MutationDirection::Apply,
            strategy: MutationStrategy::AtomicNoReplaceRename,
            source,
            target: target_root.join("Artist").join("track.mp3"),
            temporary: target_root.join("Artist").join(".unused-stage"),
            expected_size: Some(7),
            expected_content_sha256: Some("content-hash".into()),
            expected_file_identity: Some("file-id".into()),
            fencing_token: lease.fencing_token,
        })
        .unwrap();
    for (from, to) in [
        (JournalState::Prepared, JournalState::ContentVerified),
        (JournalState::ContentVerified, JournalState::Published),
        (JournalState::Published, JournalState::SourceDeleted),
        (JournalState::SourceDeleted, JournalState::Completed),
    ] {
        store
            .transition_operation(&operation_id, from, to, &lease, None, None)
            .unwrap();
    }
    store.release_apply_lease(&lease).unwrap();
    store
        .finish_execution(&execution_id, RunStatus::Completed, 1, 0, 0)
        .unwrap();

    let apply_operation_id = store
        .load_rollback_items(&execution_id)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .operation_id;
    let rollback_id = store.begin_rollback(&execution_id, false).unwrap();
    let rollback_lease = store
        .acquire_rollback_lease(&execution_id, &rollback_id)
        .unwrap();
    let rollback_operation_id = Uuid::new_v4().to_string();
    store
        .prepare_rollback_operation(&OperationIntent {
            operation_id: rollback_operation_id.clone(),
            attempt_id: rollback_id.clone(),
            plan_item_id: apply_operation_id,
            sequence_no: 1,
            direction: MutationDirection::Rollback,
            strategy: MutationStrategy::AtomicNoReplaceRename,
            source: target_root.join("Artist").join("track.mp3"),
            target: temporary.path().join("source").join("track.mp3"),
            temporary: target_root.join("Artist").join(".unused-rollback-stage"),
            expected_size: Some(7),
            expected_content_sha256: Some("content-hash".into()),
            expected_file_identity: Some("file-id".into()),
            fencing_token: rollback_lease.fencing_token,
        })
        .unwrap();
    for (from, to) in [
        (JournalState::Prepared, JournalState::ContentVerified),
        (JournalState::ContentVerified, JournalState::Published),
        (JournalState::Published, JournalState::SourceDeleted),
        (JournalState::SourceDeleted, JournalState::Completed),
    ] {
        store
            .transition_rollback_operation(
                &rollback_operation_id,
                from,
                to,
                &rollback_lease,
                None,
                None,
            )
            .unwrap();
    }
    store
        .finish_rollback(&rollback_id, RunStatus::Completed, 1, 0, 0)
        .unwrap();
    store.release_rollback_lease(&rollback_lease).unwrap();
    let rollback_verify_id = store
        .begin_verify_subject(&rollback_id, VerifySubject::Rollback)
        .unwrap();
    store
        .finish_verify(&rollback_verify_id, RunStatus::Completed, 1, 0)
        .unwrap();

    let raw = Connection::open(&database).unwrap();
    let events: Vec<(i64, Option<String>, String, i64, i64)> = raw
        .prepare(
            "SELECT transition_no,from_state,to_state,fencing_token,
                    payload_schema_version
               FROM operation_journal_events WHERE operation_id=?1
              ORDER BY transition_no",
        )
        .unwrap()
        .query_map(params![operation_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(
        events[0],
        (0, None, "prepared".into(), lease.fencing_token as i64, 1)
    );
    assert_eq!(events[4].0, 4);
    assert_eq!(events[4].1.as_deref(), Some("source_deleted"));
    assert_eq!(&events[4].2, "completed");
    assert!(events
        .iter()
        .all(|event| event.3 == lease.fencing_token as i64 && event.4 == 1));
    let transition_no: i64 = raw
        .query_row(
            "SELECT transition_no FROM operation_journal WHERE id=?1",
            params![operation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(transition_no, 4);
    drop(raw);

    let archive = store
        .archive_history("scan", &scan_id, Some(&archive_directory))
        .unwrap();
    let entities: HashSet<String> = fs::read_to_string(&archive.archive_path)
        .unwrap()
        .lines()
        .skip(1)
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter_map(|value| value["entity"].as_str().map(str::to_owned))
        .collect();
    for required in [
        "workflow_attempts",
        "plan_builds",
        "operation_journal",
        "operation_journal_events",
    ] {
        assert!(
            entities.contains(required),
            "missing archive entity {required}"
        );
    }

    store.delete_history("scan", &scan_id).unwrap();
    let raw = Connection::open(&database).unwrap();
    for table in [
        "workflow_attempts",
        "operation_journal",
        "operation_journal_events",
        "plan_builds",
        "plan_revision_changes",
    ] {
        let count: i64 = raw
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "purge left rows in {table}");
    }
    let manifests: i64 = raw
        .query_row("SELECT COUNT(*) FROM archive_manifests", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(manifests, 1);
}

#[test]
fn v10_journal_rows_backfill_auditable_current_state_and_attempt_owner() {
    let _guard = mutation_guard();
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("legacy-v10.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let source = temporary.path().join("source").join("track.mp3");
    let target_root = temporary.path().join("target");
    let (_scan_id, plan_id, plan_item_id) =
        completed_plan(Arc::clone(&store), &source, &target_root);
    let lease = store.acquire_apply_lease(&plan_id, "legacy-owner").unwrap();
    let execution_id = store.begin_execution(&plan_id, false).unwrap();
    let operation_id = Uuid::new_v4().to_string();
    store
        .prepare_operation(&OperationIntent {
            operation_id: operation_id.clone(),
            attempt_id: execution_id.clone(),
            plan_item_id,
            sequence_no: 1,
            direction: MutationDirection::Apply,
            strategy: MutationStrategy::CopyPublishDelete,
            source,
            target: target_root.join("track.mp3"),
            temporary: target_root.join(".stage"),
            expected_size: Some(7),
            expected_content_sha256: Some("content-hash".into()),
            expected_file_identity: Some("file-id".into()),
            fencing_token: lease.fencing_token,
        })
        .unwrap();
    drop(store);

    let legacy = Connection::open(&database).unwrap();
    legacy
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
         DROP TABLE operation_journal_events;
         DROP TABLE workflow_attempts;
         DROP TABLE preflight_logs;
         DROP TABLE preflight_runs;
         ALTER TABLE operation_journal DROP COLUMN transition_no;
         DELETE FROM schema_migrations WHERE version>=11;
         PRAGMA user_version=10;
         UPDATE operation_journal SET attempt_id='missing-legacy-owner';",
        )
        .unwrap();
    drop(legacy);

    let migration_error = match SqliteScanStore::open(&database) {
        Ok(_) => panic!("orphaned legacy journal migration must fail"),
        Err(error) => error,
    };
    assert_eq!(
        migration_error,
        "workflow_attempt_backfill_orphan_journal:1"
    );
    let interrupted = Connection::open(&database).unwrap();
    let v11_applied: i64 = interrupted
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=11)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let attempts_table: i64 = interrupted
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master
                            WHERE type='table' AND name='workflow_attempts')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let transition_column: i64 = interrupted
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('operation_journal')
              WHERE name='transition_no'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((v11_applied, attempts_table, transition_column), (0, 0, 0));
    interrupted
        .execute(
            "UPDATE operation_journal SET attempt_id=?1",
            params![execution_id],
        )
        .unwrap();
    drop(interrupted);

    let reopened = SqliteScanStore::open(&database).unwrap();
    let raw = Connection::open(reopened.database_path()).unwrap();
    let owner: (String, String, i64) = raw
        .query_row(
            "SELECT id,kind,attempt_no FROM workflow_attempts WHERE id=?1",
            params![execution_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(owner, (execution_id, "apply".into(), 1));
    let backfill: (Option<String>, String, String) = raw
        .query_row(
            "SELECT from_state,to_state,payload_json
               FROM operation_journal_events
              WHERE operation_id=?1 AND transition_no=0",
            params![operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(backfill.0, None);
    assert_eq!(backfill.1, "prepared");
    assert!(backfill.2.contains("legacy_v10_backfill"));
    let user_version: i64 = raw
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(user_version, 17);
}

#[test]
fn database_with_unknown_newer_user_version_is_rejected_without_mutation() {
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("future.db");
    drop(SqliteScanStore::open(&database).unwrap());
    let raw = Connection::open(&database).unwrap();
    raw.pragma_update(None, "user_version", 99).unwrap();
    drop(raw);

    let error = match SqliteScanStore::open(&database) {
        Ok(_) => panic!("a newer database must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error, "database_schema_too_new:99>17");
    let raw = Connection::open(&database).unwrap();
    let user_version: i64 = raw
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(user_version, 99);
}
