use music_folder_core::usecases::{
    ApplyUseCase, CancellationToken, PlanOptions, PlanUseCase, RollbackUseCase, ScanOptions,
    ScanUseCase, VerifyUseCase,
};
use music_folder_infra::{
    lofty_reader::LoftyMetadataReader, sqlite::SqliteScanStore, windows_fs::LocalFileSystem,
};
use std::{fs, path::PathBuf, sync::Arc};
use tempfile::tempdir;

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

#[test]
fn persisted_plan_drives_dry_run_apply_verify_and_rollback() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(&source).unwrap();
    let original = source.join("track.mp3");
    fs::copy(fixture("mp3/japanese.mp3"), &original).unwrap();
    let database = temp.path().join("state.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let files = Arc::new(LocalFileSystem);
    let scan = ScanUseCase {
        fs: Arc::clone(&files),
        metadata: Arc::new(LoftyMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
    assert_eq!(scan.files, 1);
    let warm = ScanUseCase {
        fs: Arc::clone(&files),
        metadata: Arc::new(LoftyMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
    assert_eq!(warm.cache_hits, 1);
    let plan = PlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &scan.scan_id,
        &PlanOptions {
            target_root: target.clone(),
            batch_size: 10,
        },
    )
    .unwrap();
    let dry = ApplyUseCase {
        store: Arc::clone(&store),
        files: Arc::clone(&files),
    }
    .execute(&plan.plan_id, true)
    .unwrap();
    assert_eq!(dry.success, 1);
    assert!(original.exists());
    let applied = ApplyUseCase {
        store: Arc::clone(&store),
        files: Arc::clone(&files),
    }
    .execute(&plan.plan_id, false)
    .unwrap();
    assert_eq!(applied.success, 1);
    assert!(!original.exists());
    let repeated = ApplyUseCase {
        store: Arc::clone(&store),
        files: Arc::clone(&files),
    }
    .execute(&plan.plan_id, false)
    .unwrap();
    assert_eq!(repeated.success, 0);
    assert_eq!(repeated.skipped, 1);
    let first_page = store
        .list_operation_logs(
            &applied.execution_id,
            None,
            1,
            Some("track"),
            Some("success"),
        )
        .unwrap();
    assert_eq!(first_page.len(), 1);
    assert!(store
        .list_operation_logs(
            &applied.execution_id,
            Some(first_page[0].sequence_no),
            1,
            None,
            None
        )
        .unwrap()
        .is_empty());
    assert!(store
        .list_metrics(&applied.execution_id)
        .unwrap()
        .iter()
        .any(|metric| metric.phase == "apply"));
    let verified = VerifyUseCase {
        store: Arc::clone(&store),
        files: Arc::clone(&files),
    }
    .execute(&applied.execution_id)
    .unwrap();
    assert_eq!(verified.failed, 0);
    let rollback = RollbackUseCase {
        store: Arc::clone(&store),
        files: Arc::clone(&files),
    }
    .execute(&applied.execution_id, false)
    .unwrap();
    assert_eq!(rollback.failed, 0);
    assert!(original.exists());
    let history = SqliteScanStore::open(&database)
        .unwrap()
        .list_history(100, None)
        .unwrap();
    assert!(history.iter().any(|row| row.kind == "verify"));
    assert!(history.iter().any(|row| row.kind == "rollback"));
    let plan_detail = store.get_run_detail("plan", &plan.plan_id).unwrap();
    assert_eq!(
        plan_detail.parent_id.as_deref(),
        Some(scan.scan_id.as_str())
    );
    assert_eq!(plan_detail.success, plan.items);
    let apply_detail = store
        .get_run_detail("apply", &applied.execution_id)
        .unwrap();
    assert_eq!(
        apply_detail.parent_id.as_deref(),
        Some(plan.plan_id.as_str())
    );
    let verify_detail = store.get_run_detail("verify", &verified.verify_id).unwrap();
    assert_eq!(
        verify_detail.parent_id.as_deref(),
        Some(applied.execution_id.as_str())
    );
    let rollback_detail = store
        .get_run_detail("rollback", &rollback.rollback_id)
        .unwrap();
    assert_eq!(
        rollback_detail.parent_id.as_deref(),
        Some(applied.execution_id.as_str())
    );
    assert_eq!(
        store.get_run_detail("unknown", "id").unwrap_err(),
        "invalid_run_kind"
    );

    rusqlite::Connection::open(&database)
        .unwrap()
        .execute(
            "UPDATE plan_items SET target_path=target_path || '.tampered' WHERE plan_id=?1",
            rusqlite::params![plan.plan_id],
        )
        .unwrap();
    let mismatch = ApplyUseCase { store, files }
        .execute(&plan.plan_id, true)
        .err()
        .expect("tampered snapshot must be rejected");
    assert_eq!(mismatch, "plan_snapshot_mismatch");
}

#[test]
fn cancelled_scan_commits_received_work_and_records_cancelled_status() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::copy(fixture("mp3/japanese.mp3"), source.join("track.mp3")).unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("cancel.db")).unwrap());
    let token = CancellationToken::default();
    token.cancel();
    let options = ScanOptions {
        cancellation: token,
        ..ScanOptions::default()
    };
    let result = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(LoftyMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &options)
    .unwrap();
    assert_eq!(result.files, 0);
    let history = store.list_history(10, None).unwrap();
    assert!(history
        .iter()
        .any(|row| row.id == result.scan_id && row.status == "cancelled"));
}
