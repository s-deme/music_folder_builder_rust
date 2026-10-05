use music_folder_core::{
    ports::{ApplyStore, FileMutator, PlanRevisionStore, RollbackStore, ScanStore, VerifyStore},
    usecases::{ApplyUseCase, PlanOptions, PlanUseCase, RollbackUseCase, VerifyUseCase},
    FileKind, JournalState, MutationDirection, MutationStrategy, OperationIntent, RunStatus,
    ScannedFile, TrackMetadata, VerifySubject,
};
use music_folder_infra::{sqlite::SqliteScanStore, windows_fs::LocalFileSystem};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, sync::Arc};
use tempfile::tempdir;
use uuid::Uuid;

fn mutation_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    GUARD
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn completed_single_file_plan(
    store: Arc<SqliteScanStore>,
    workspace: &std::path::Path,
) -> (String, String) {
    let source_root = workspace.join("source");
    let source = source_root.join("track.mp3");
    fs::create_dir_all(&source_root).unwrap();
    fs::write(&source, b"archive protection fixture").unwrap();
    let fingerprint = LocalFileSystem.mutation_fingerprint(&source).unwrap();
    let scan_id = store.begin_scan(&source_root).unwrap();
    store
        .save_batch(
            &scan_id,
            &[ScannedFile {
                id: Uuid::new_v4(),
                path: source,
                fingerprint,
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
            target_root: workspace.join("target"),
            batch_size: 8,
            naming: music_folder_core::NamingRules::default(),
        },
    )
    .unwrap();
    (scan_id, plan.plan_id)
}

#[test]
fn successful_mutation_requires_completed_rollback_verification_before_archive_or_purge() {
    let _guard = mutation_test_guard();
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("mutation-protection.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let (scan_id, plan_id) = completed_single_file_plan(Arc::clone(&store), temporary.path());
    let files = Arc::new(LocalFileSystem);
    let execution = ApplyUseCase {
        store: Arc::clone(&store),
        files: Arc::clone(&files),
    }
    .execute(&plan_id, false)
    .unwrap();
    assert_eq!(execution.success, 1);
    let after_apply = store.history_cleanup_preview("scan", &scan_id).unwrap();
    assert!(after_apply.blocked);
    assert!(after_apply
        .protection_reasons
        .contains(&"unrolled_mutation".to_string()));
    assert!(store
        .archive_history("scan", &scan_id, None)
        .unwrap_err()
        .contains("unrolled_mutation"));

    VerifyUseCase {
        store: Arc::clone(&store),
        files: Arc::clone(&files),
    }
    .execute(&execution.execution_id)
    .unwrap();
    let rollback = RollbackUseCase {
        store: Arc::clone(&store),
        files: Arc::clone(&files),
    }
    .execute(&execution.execution_id, false)
    .unwrap();
    assert_eq!(rollback.success, 1);
    let after_rollback = store.history_cleanup_preview("scan", &scan_id).unwrap();
    assert!(after_rollback.blocked);
    assert!(after_rollback
        .protection_reasons
        .contains(&"rollback_unverified".to_string()));
    assert!(!after_rollback
        .protection_reasons
        .contains(&"unrolled_mutation".to_string()));

    VerifyUseCase {
        store: Arc::clone(&store),
        files,
    }
    .execute_subject(&rollback.rollback_id, VerifySubject::Rollback)
    .unwrap();
    let verified = store.history_cleanup_preview("scan", &scan_id).unwrap();
    assert!(
        !verified.blocked,
        "unexpected protection: {:?}",
        verified.protection_reasons
    );
    let archive = store.archive_history("scan", &scan_id, None).unwrap();
    assert!(archive.verified);
    store.delete_history("scan", &scan_id).unwrap();
    assert_eq!(
        store.get_run_detail("scan", &scan_id).unwrap_err(),
        "run_not_found"
    );
}

#[test]
fn terminal_failed_pre_publish_journal_does_not_permanently_block_archive() {
    let _guard = mutation_test_guard();
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("terminal-failed.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let (scan_id, plan_id) = completed_single_file_plan(Arc::clone(&store), temporary.path());
    let raw = Connection::open(&database).unwrap();
    let (plan_item_id, source, target): (String, String, String) = raw
        .query_row(
            "SELECT id,source_path,target_path FROM plan_items WHERE plan_id=?1",
            params![plan_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    drop(raw);
    let lease = store
        .acquire_apply_lease(&plan_id, "safe-failure-owner")
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
            strategy: MutationStrategy::CopyPublishDelete,
            source: source.into(),
            target: target.into(),
            temporary: temporary.path().join("never-published.tmp"),
            expected_size: None,
            expected_content_sha256: None,
            expected_file_identity: None,
            fencing_token: lease.fencing_token,
        })
        .unwrap();
    store
        .transition_operation(
            &operation_id,
            JournalState::Prepared,
            JournalState::Failed,
            &lease,
            None,
            Some("safe_pre_publish_failure"),
        )
        .unwrap();
    store
        .finish_execution(&execution_id, RunStatus::Failed, 0, 0, 1)
        .unwrap();
    store.release_apply_lease(&lease).unwrap();

    let preview = store.history_cleanup_preview("scan", &scan_id).unwrap();
    assert!(
        !preview.blocked,
        "unexpected protection: {:?}",
        preview.protection_reasons
    );
    assert!(!preview
        .protection_reasons
        .contains(&"recovery_required".to_string()));
    store.archive_history("scan", &scan_id, None).unwrap();
}

#[test]
fn archive_is_atomically_published_verified_and_idempotent() {
    let _guard = mutation_test_guard();
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("workflow.db");
    let archive_directory = temporary.path().join("archives");
    let store = SqliteScanStore::open(&database).unwrap();
    let scan_id = store.begin_scan(&temporary.path().join("source")).unwrap();
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();

    let preview = store.history_cleanup_preview("scan", &scan_id).unwrap();
    assert_eq!(preview.root_scan_id, scan_id);
    assert!(!preview.blocked);
    assert!(!preview.archived);

    let archived = store
        .archive_history("scan", &scan_id, Some(&archive_directory))
        .unwrap();
    assert!(archived.verified);
    assert!(!archived.already_archived);
    assert!(archived.record_count >= 1);
    assert!(archived.byte_count > 0);
    let bytes = fs::read(&archived.archive_path).unwrap();
    assert_eq!(bytes.len() as u64, archived.byte_count);
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), archived.sha256);
    let header: serde_json::Value =
        serde_json::from_str(std::str::from_utf8(&bytes).unwrap().lines().next().unwrap()).unwrap();
    assert_eq!(header["archive_schema_version"], 3);
    assert_eq!(header["root_scan_id"], scan_id);

    let listed = store
        .list_history_filtered(10, None, None, Some("scan"), None, None, false)
        .unwrap();
    assert_eq!(listed[0].status, "archived");
    let repeated = store
        .archive_history("scan", &scan_id, Some(&archive_directory))
        .unwrap();
    assert!(repeated.already_archived);
    assert_eq!(repeated.archive_id, archived.archive_id);
    let serialized = serde_json::to_value(&repeated).unwrap();
    assert_eq!(serialized["archive_path"]["schema_version"], 1);
    assert_eq!(serialized["archive_path"]["role"], "history_archive");
    assert!(serialized["archive_path"]["raw_base64"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
}

#[cfg(unix)]
#[test]
fn archive_manifest_round_trips_a_non_utf8_native_path_across_reopen() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let _guard = mutation_test_guard();
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("lossless.db");
    let archive_directory = temporary
        .path()
        .join(OsString::from_vec(b"archives-\xff".to_vec()));
    fs::create_dir_all(&archive_directory).unwrap();
    let store = SqliteScanStore::open(&database).unwrap();
    let scan_id = store.begin_scan(&temporary.path().join("source")).unwrap();
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();
    let archived = store
        .archive_history("scan", &scan_id, Some(&archive_directory))
        .unwrap();
    assert!(archived.archive_path.starts_with(&archive_directory));
    let (stored_encoding, stored_raw): (String, Vec<u8>) = Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT archive_path_encoding,archive_path_blob
               FROM archive_manifests WHERE id=?1",
            params![archived.archive_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let (expected_encoding, expected_raw) =
        music_folder_infra::path_codec::encode_path(&archived.archive_path);
    assert_eq!(stored_encoding, expected_encoding);
    assert_eq!(stored_raw, expected_raw);
    let expected_path = archived.archive_path.clone();
    drop(store);

    let reopened = SqliteScanStore::open(&database).unwrap();
    let repeated = reopened
        .archive_history("scan", &scan_id, Some(&archive_directory))
        .unwrap();
    assert!(repeated.already_archived);
    assert_eq!(repeated.archive_path, expected_path);
    assert_eq!(
        fs::read(&repeated.archive_path).unwrap().len() as u64,
        repeated.byte_count
    );
}

#[test]
fn obsolete_archive_schema_is_rebuilt_and_unknown_newer_schema_is_rejected() {
    let _guard = mutation_test_guard();
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("archive-version.db");
    let archive_directory = temporary.path().join("archives");
    let store = SqliteScanStore::open(&database).unwrap();
    let scan_id = store.begin_scan(&temporary.path().join("source")).unwrap();
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();
    let first = store
        .archive_history("scan", &scan_id, Some(&archive_directory))
        .unwrap();
    let external = Connection::open(&database).unwrap();
    external
        .execute(
            "UPDATE archive_manifests SET schema_version=2 WHERE id=?1",
            params![first.archive_id],
        )
        .unwrap();
    drop(external);

    let rebuilt = store
        .archive_history("scan", &scan_id, Some(&archive_directory))
        .unwrap();
    assert!(!rebuilt.already_archived);
    assert_ne!(rebuilt.archive_id, first.archive_id);
    assert_eq!(rebuilt.schema_version, 3);

    Connection::open(&database)
        .unwrap()
        .execute(
            "UPDATE archive_manifests SET schema_version=99 WHERE id=?1",
            params![rebuilt.archive_id],
        )
        .unwrap();
    assert_eq!(
        store
            .archive_history("scan", &scan_id, Some(&archive_directory))
            .unwrap_err(),
        "archive_schema_too_new:99>3"
    );
}

#[test]
fn archive_refuses_active_or_held_workflow_and_detects_later_tampering() {
    let _guard = mutation_test_guard();
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("protected.db");
    let archive_directory = temporary.path().join("archives");
    let store = SqliteScanStore::open(&database).unwrap();
    let running_scan = store.begin_scan(&temporary.path().join("running")).unwrap();
    let running = store
        .history_cleanup_preview("scan", &running_scan)
        .unwrap();
    assert!(running.blocked);
    assert!(running
        .protection_reasons
        .contains(&"running_attempt".to_string()));
    assert!(store
        .archive_history("scan", &running_scan, Some(&archive_directory))
        .unwrap_err()
        .starts_with("history_archive_blocked:"));
    store
        .finish_scan(&running_scan, RunStatus::Completed, 0)
        .unwrap();

    let external = Connection::open(&database).unwrap();
    external
        .execute(
            "UPDATE scan_runs SET legal_hold=1 WHERE id=?1",
            params![running_scan],
        )
        .unwrap();
    let held = store
        .history_cleanup_preview("scan", &running_scan)
        .unwrap();
    assert!(held.protection_reasons.contains(&"legal_hold".to_string()));
    external
        .execute(
            "UPDATE scan_runs SET legal_hold=0 WHERE id=?1",
            params![running_scan],
        )
        .unwrap();
    drop(external);

    let archived = store
        .archive_history("scan", &running_scan, Some(&archive_directory))
        .unwrap();
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&archived.archive_path)
        .unwrap();
    std::io::Write::write_all(&mut file, b"tampered\n").unwrap();
    drop(file);
    assert_eq!(
        store
            .archive_history("scan", &running_scan, Some(&archive_directory))
            .unwrap_err(),
        "archive_digest_mismatch"
    );

    let mut content = Vec::new();
    fs::File::open(&archived.archive_path)
        .unwrap()
        .read_to_end(&mut content)
        .unwrap();
    assert!(content.ends_with(b"tampered\n"));
}

#[test]
fn archive_refuses_a_mutation_lease_acquired_before_the_execution_row_exists() {
    let _guard = mutation_test_guard();
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("leased.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let scan_id = store.begin_scan(&temporary.path().join("source")).unwrap();
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();
    let plan = PlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &scan_id,
        &PlanOptions {
            target_root: temporary.path().join("target"),
            batch_size: 16,
            naming: music_folder_core::NamingRules::default(),
        },
    )
    .unwrap();

    // Apply deliberately acquires this lease before inserting execution_runs.
    // Archive protection must therefore match the persisted root identity,
    // not an owner ID that cannot yet refer to an attempt.
    let lease = store
        .acquire_apply_lease(&plan.plan_id, "pre-execution-owner")
        .unwrap();
    let preview = store.history_cleanup_preview("scan", &scan_id).unwrap();
    assert!(preview.blocked);
    assert!(preview
        .protection_reasons
        .contains(&"active_mutation_lease".to_string()));
    assert!(store
        .archive_history("scan", &scan_id, None)
        .unwrap_err()
        .contains("active_mutation_lease"));
    store.release_apply_lease(&lease).unwrap();
}

#[test]
fn archived_workflow_graph_is_repository_enforced_read_only() {
    let _guard = mutation_test_guard();
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("read-only-archive.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let scan_id = store.begin_scan(&temporary.path().join("source")).unwrap();
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();
    let plan = PlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &scan_id,
        &PlanOptions {
            target_root: temporary.path().join("target"),
            batch_size: 8,
            naming: music_folder_core::NamingRules::default(),
        },
    )
    .unwrap();
    let files = Arc::new(LocalFileSystem);
    let execution = ApplyUseCase {
        store: Arc::clone(&store),
        files: Arc::clone(&files),
    }
    .execute(&plan.plan_id, true)
    .unwrap();
    VerifyUseCase {
        store: Arc::clone(&store),
        files: Arc::clone(&files),
    }
    .execute(&execution.execution_id)
    .unwrap();
    let rollback = RollbackUseCase {
        store: Arc::clone(&store),
        files,
    }
    .execute(&execution.execution_id, true)
    .unwrap();
    let archived = store.archive_history("scan", &scan_id, None).unwrap();
    let archive_text = fs::read_to_string(&archived.archive_path).unwrap();
    assert!(archive_text.contains("\"entity\":\"preflight_runs\""));

    assert_eq!(
        PlanUseCase {
            store: Arc::clone(&store)
        }
        .execute(
            &scan_id,
            &PlanOptions {
                target_root: temporary.path().join("other-target"),
                batch_size: 8,
                naming: music_folder_core::NamingRules::default(),
            },
        )
        .unwrap_err()
        .code(),
        "workflow_archived_read_only"
    );
    assert_eq!(
        store.begin_plan_revision(&plan.plan_id).unwrap_err(),
        "workflow_archived_read_only"
    );
    assert_eq!(
        store
            .acquire_apply_lease(&plan.plan_id, "archived-apply")
            .unwrap_err(),
        "workflow_archived_read_only"
    );
    assert_eq!(
        store.begin_execution(&plan.plan_id, true).unwrap_err(),
        "workflow_archived_read_only"
    );
    assert_eq!(
        store.begin_verify(&execution.execution_id).unwrap_err(),
        "workflow_archived_read_only"
    );
    assert_eq!(
        store
            .begin_verify_subject(&rollback.rollback_id, VerifySubject::Rollback)
            .unwrap_err(),
        "workflow_archived_read_only"
    );
    assert_eq!(
        store
            .begin_rollback(&execution.execution_id, true)
            .unwrap_err(),
        "workflow_archived_read_only"
    );
}
