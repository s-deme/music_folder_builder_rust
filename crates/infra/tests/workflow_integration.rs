use music_folder_core::ports::ManualTargetChange;
use music_folder_core::usecases::{
    ApplyUseCase, CancellationToken, PlanOptions, PlanUseCase, RevisePlanUseCase, RollbackUseCase,
    ScanOptions, ScanUseCase, VerifyUseCase,
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
            naming: music_folder_core::NamingRules::default(),
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

#[test]
fn plan_moves_companion_image_to_music_target_directory() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::copy(fixture("mp3/japanese.mp3"), source.join("track.mp3")).unwrap();
    fs::write(source.join("cover.jpg"), b"fixture image").unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("state.db")).unwrap());
    let scan = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(LoftyMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
    assert_eq!(scan.files, 2);
    let plan = PlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &scan.scan_id,
        &PlanOptions {
            target_root: target,
            batch_size: 10,
            naming: music_folder_core::NamingRules::default(),
        },
    )
    .unwrap();
    let items = store
        .list_plan_items(&plan.plan_id, None, 10, Some("cover.jpg"), None)
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].action, "move");
    assert!(items[0]
        .target_path
        .as_deref()
        .unwrap()
        .ends_with("cover.jpg"));
}

#[test]
fn manual_target_creates_immutable_child_plan() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::copy(fixture("mp3/japanese.mp3"), source.join("track.mp3")).unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("state.db")).unwrap());
    let scan = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(LoftyMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
    let parent = PlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &scan.scan_id,
        &PlanOptions {
            target_root: target.clone(),
            batch_size: 10,
            naming: music_folder_core::NamingRules::default(),
        },
    )
    .unwrap();
    let parent_item = store
        .list_plan_items(&parent.plan_id, None, 1, None, None)
        .unwrap()
        .remove(0);
    let manual_target = target
        .join("Manual Artist")
        .join("Manual Album")
        .join("manual.mp3");
    let child = RevisePlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &parent.plan_id,
        &[ManualTargetChange {
            plan_item_id: parent_item.id,
            target: manual_target.clone(),
            reason: "test".into(),
        }],
    )
    .unwrap();
    let parent_after = store
        .list_plan_items(&parent.plan_id, None, 1, None, None)
        .unwrap();
    assert_ne!(
        parent_after[0].target_path.as_deref(),
        Some(manual_target.to_string_lossy().as_ref())
    );
    let child_item = store.list_plan_items(&child, None, 1, None, None).unwrap();
    assert_eq!(
        child_item[0].target_path.as_deref(),
        Some(manual_target.to_string_lossy().as_ref())
    );
    let applied = ApplyUseCase {
        store,
        files: Arc::new(LocalFileSystem),
    }
    .execute(&child, false)
    .unwrap();
    assert_eq!(applied.success, 1);
    assert!(manual_target.exists());
}

#[test]
fn deleting_parent_plan_removes_descendants_without_touching_files() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(&source).unwrap();
    let original = source.join("track.mp3");
    fs::copy(fixture("mp3/japanese.mp3"), &original).unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("state.db")).unwrap());
    let scan = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(LoftyMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
    let parent = PlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &scan.scan_id,
        &PlanOptions {
            target_root: target,
            batch_size: 10,
            naming: music_folder_core::NamingRules::default(),
        },
    )
    .unwrap();
    let item = store
        .list_plan_items(&parent.plan_id, None, 1, None, None)
        .unwrap()
        .remove(0);
    let child = RevisePlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &parent.plan_id,
        &[ManualTargetChange {
            plan_item_id: item.id,
            target: temp.path().join("manual.mp3"),
            reason: "test".into(),
        }],
    )
    .unwrap();
    store.delete_history("plan", &parent.plan_id).unwrap();
    assert!(original.exists());
    assert_eq!(
        store.get_run_detail("plan", &parent.plan_id).unwrap_err(),
        "run_not_found"
    );
    assert_eq!(
        store.get_run_detail("plan", &child).unwrap_err(),
        "run_not_found"
    );
}
