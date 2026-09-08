use music_folder_core::usecases::{PlanOptions, PlanUseCase};
use music_folder_core::{
    ports::{ApplyStore, ManualTargetChange, PlanRevisionStore, ScanStore},
    ExecutionDisposition, FileFingerprint, FileKind, NamingRules, PlanAction, Risk, RunStatus,
    ScannedFile, TrackMetadata, NAMING_RULES_SCHEMA_VERSION, PLAN_ISSUES_SCHEMA_VERSION,
};
use music_folder_infra::sqlite::SqliteScanStore;
use rusqlite::{params, Connection};
use std::{path::Path, sync::Arc};
use tempfile::tempdir;
use uuid::Uuid;

fn completed_plan(store: Arc<SqliteScanStore>, source: &Path, target_root: &Path) -> String {
    let scan_id = store.begin_scan(source.parent().unwrap()).unwrap();
    store
        .save_batch(
            &scan_id,
            &[ScannedFile {
                id: Uuid::new_v4(),
                path: source.to_path_buf(),
                fingerprint: FileFingerprint {
                    size_bytes: 4,
                    mtime_ns: 7,
                    content_sha256: Some("sha256-content".into()),
                    file_identity: Some("volume:file-id".into()),
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
            batch_size: 16,
            naming: NamingRules::default(),
        },
    )
    .unwrap()
    .plan_id
}

#[test]
fn plan_context_and_lossless_target_are_part_of_authorization_hash() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("state.db");
    let source = temp.path().join("source").join("track.mp3");
    let target_root = temp.path().join("library");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let plan_id = completed_plan(Arc::clone(&store), &source, &target_root);
    store.validate_plan_snapshot(&plan_id).unwrap();

    let external = Connection::open(&database).unwrap();
    let error = external
        .execute(
            "UPDATE plan_runs SET rules_json=?2 WHERE id=?1",
            params![
                plan_id,
                serde_json::to_string(&NamingRules {
                    allow_long_paths: true,
                    ..NamingRules::default()
                })
                .unwrap()
            ],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("completed_plan_authorization_immutable"));
    for statement in [
        "UPDATE plan_runs SET rules_schema_version=99 WHERE id=?1",
        "UPDATE plan_runs SET target_root='tampered' WHERE id=?1",
        "UPDATE plan_items SET execution_disposition='blocked' WHERE plan_id=?1",
        "UPDATE plan_items SET target_path='tampered' WHERE plan_id=?1",
        "DELETE FROM plan_items WHERE plan_id=?1",
    ] {
        let error = external
            .execute(statement, params![plan_id])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("completed_plan_authorization_immutable")
                || error.contains("completed_plan_items_immutable"),
            "unexpected rejection: {error}"
        );
    }
    let status_error = external
        .execute(
            "UPDATE plan_runs SET status='running' WHERE id=?1",
            params![plan_id],
        )
        .unwrap_err()
        .to_string();
    assert!(status_error.contains("completed_plan_status_immutable"));
    let insert_error = external
        .execute(
            "INSERT INTO plan_items(
                 id,plan_id,ordinal,source_path,action,risk,
                 source_size_bytes,source_mtime_ns
             ) VALUES('forged-item',?1,999,'forged','skip','none',0,'0')",
            params![plan_id],
        )
        .unwrap_err()
        .to_string();
    assert!(insert_error.contains("plan_not_running"));
    store.validate_plan_snapshot(&plan_id).unwrap();

    let (rules_version, disposition, issues_version): (i64, String, i64) = external
        .query_row(
            "SELECT plan.rules_schema_version,item.execution_disposition,
                    item.issues_schema_version
               FROM plan_runs plan JOIN plan_items item ON item.plan_id=plan.id
              WHERE plan.id=?1",
            params![plan_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(rules_version, i64::from(NAMING_RULES_SCHEMA_VERSION));
    assert_eq!(disposition, "executable");
    assert_eq!(issues_version, i64::from(PLAN_ISSUES_SCHEMA_VERSION));
}

#[test]
fn legacy_unknown_and_malformed_rules_snapshots_are_non_executable() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("rules-version.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let plan_id = completed_plan(
        Arc::clone(&store),
        &temp.path().join("source/track.mp3"),
        &temp.path().join("library"),
    );
    let external = Connection::open(&database).unwrap();
    external
        .execute_batch("DROP TRIGGER plan_runs_completed_authorization_immutable;")
        .unwrap();

    for version in [0_i64, 99_i64] {
        external
            .execute(
                "UPDATE plan_runs SET rules_schema_version=?2 WHERE id=?1",
                params![plan_id, version],
            )
            .unwrap();
        assert_eq!(
            store.load_completed_plan(&plan_id).unwrap_err(),
            "legacy_plan_non_executable"
        );
    }

    let mut malformed = serde_json::to_value(NamingRules::default()).unwrap();
    malformed
        .as_object_mut()
        .unwrap()
        .insert("unexpected_field".into(), serde_json::Value::Bool(true));
    external
        .execute(
            "UPDATE plan_runs
                SET rules_schema_version=?2,
                    rules_json=?3
              WHERE id=?1",
            params![
                plan_id,
                i64::from(NAMING_RULES_SCHEMA_VERSION),
                serde_json::to_string(&malformed).unwrap()
            ],
        )
        .unwrap();
    assert_eq!(
        store.load_completed_plan(&plan_id).unwrap_err(),
        "plan_rules_json_invalid"
    );
}

#[test]
fn revision_revalidates_manual_target_and_remains_executable() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("revision.db");
    let source = temp.path().join("source").join("track.mp3");
    let target_root = temp.path().join("library");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let plan_id = completed_plan(Arc::clone(&store), &source, &target_root);
    let original = store.load_completed_plan(&plan_id).unwrap();
    let item_id = original[0].plan_item_id.clone();

    let blocked_id = store
        .revise_plan(
            &plan_id,
            &[ManualTargetChange {
                plan_item_id: item_id.clone(),
                target: temp.path().join("outside").join("escaped.mp3"),
                reason: "manual".into(),
            }],
        )
        .unwrap();
    store.validate_plan_snapshot(&blocked_id).unwrap();
    let blocked = store.load_completed_plan(&blocked_id).unwrap();
    assert_eq!(blocked[0].action, PlanAction::Skip);
    assert_eq!(blocked[0].disposition, ExecutionDisposition::Blocked);
    assert_eq!(blocked[0].risk, Risk::InvalidTarget);
    assert!(blocked[0]
        .issues
        .iter()
        .any(|issue| issue.severity == music_folder_core::IssueSeverity::Blocking));
    assert!(blocked[0].target.is_none());

    let valid_target = target_root.join("Manual").join("track.mp3");
    let revised_id = store
        .revise_plan(
            &plan_id,
            &[ManualTargetChange {
                plan_item_id: item_id,
                target: valid_target.clone(),
                reason: "manual".into(),
            }],
        )
        .unwrap();
    store.validate_plan_snapshot(&revised_id).unwrap();
    let revised = store.load_completed_plan(&revised_id).unwrap();
    assert_eq!(revised[0].action, PlanAction::Move);
    assert_eq!(revised[0].disposition, ExecutionDisposition::Executable);
    assert_eq!(revised[0].risk, Risk::None);
    assert_eq!(revised[0].target.as_deref(), Some(valid_target.as_path()));
    assert_eq!(
        store.load_completed_plan(&plan_id).unwrap()[0].target,
        original[0].target
    );
}
