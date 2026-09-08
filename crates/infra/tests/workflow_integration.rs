use music_folder_core::ports::{ApplyStore, ManualTargetChange, MetadataReader};
use music_folder_core::usecases::{
    ApplyUseCase, CancellationToken, PlanOptions, PlanUseCase, RevisePlanUseCase, RollbackUseCase,
    ScanOptions, ScanUseCase, VerifyUseCase,
};
use music_folder_infra::{
    lofty_reader::LoftyMetadataReader, sqlite::SqliteScanStore, windows_fs::LocalFileSystem,
};
use std::{fs, path::Path, sync::Arc};
use tempfile::tempdir;

mod support;
use support::fixture;

fn mutation_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    GUARD
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct DiscMetadataReader;

impl MetadataReader for DiscMetadataReader {
    fn read(&self, path: &Path) -> Result<music_folder_core::TrackMetadata, String> {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let (album, disc_no) = match stem {
            "disc-one" => ("Shared Album", 1),
            "disc-two" => ("Shared Album", 2),
            "album-a" => ("Album A", 1),
            "album-b" => ("Album B", 1),
            _ => ("Shared Album", 1),
        };
        Ok(music_folder_core::TrackMetadata {
            artist: Some("Artist".into()),
            album_artist: Some("Album Artist".into()),
            album: Some(album.into()),
            title: Some(stem.into()),
            track_no: Some(1),
            disc_no: Some(disc_no),
            year: None,
        })
    }
}

#[test]
fn persisted_plan_drives_dry_run_apply_verify_and_rollback() {
    let _guard = mutation_test_guard();
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
    let preflight_rows = rusqlite::Connection::open(&database)
        .unwrap()
        .prepare(
            "SELECT run.parent_attempt_id,run.kind,run.mode,run.status,
                    log.outcome,log.code,
                    log.expected_content_sha256,log.observed_content_sha256
               FROM preflight_runs run
               JOIN preflight_logs log ON log.preflight_id=run.id
              WHERE run.parent_attempt_id IN (?1,?2)
              ORDER BY run.mode",
        )
        .unwrap()
        .query_map(
            rusqlite::params![dry.execution_id, applied.execution_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(preflight_rows.len(), 2);
    assert!(preflight_rows.iter().all(|row| {
        row.1 == "apply"
            && row.3 == "passed"
            && row.4 == "passed"
            && row.5.is_none()
            && row.6 == row.7
    }));
    assert_eq!(
        preflight_rows
            .iter()
            .map(|row| row.2.as_str())
            .collect::<Vec<_>>(),
        ["dry_run", "mutation"]
    );
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
    assert!(!first_page[0]
        .target_path
        .as_deref()
        .is_some_and(|path| Path::new(path).exists()));
    let repeated_rollback = RollbackUseCase {
        store: Arc::clone(&store),
        files: Arc::clone(&files),
    }
    .execute(&applied.execution_id, false)
    .unwrap();
    assert_eq!(
        (
            repeated_rollback.success,
            repeated_rollback.skipped,
            repeated_rollback.failed,
        ),
        (0, 1, 0),
        "a completed rollback is idempotent only through its durable journal evidence"
    );
    assert!(original.exists());
    let (operation_action, journal_strategy): (String, String) =
        rusqlite::Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT operation.action,journal.strategy
                   FROM operation_logs operation
                   JOIN operation_journal journal
                     ON journal.attempt_id=operation.execution_id
                    AND journal.plan_item_id=operation.plan_item_id
                  WHERE operation.execution_id=?1",
                rusqlite::params![applied.execution_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    #[cfg(windows)]
    assert_eq!(
        (operation_action.as_str(), journal_strategy.as_str()),
        ("move", "atomic_no_replace_rename")
    );
    #[cfg(not(windows))]
    assert_eq!(
        (operation_action.as_str(), journal_strategy.as_str()),
        ("copy_delete", "copy_publish_delete")
    );
    let history = SqliteScanStore::open(&database)
        .unwrap()
        .list_history(100, None)
        .unwrap();
    assert!(history.iter().any(|row| row.kind == "verify"));
    assert!(history.iter().any(|row| row.kind == "rollback"));
    assert!(history
        .iter()
        .filter(|row| row.kind != "scan")
        .all(|row| row.root_scan_id == scan.scan_id));
    let apply_history = store
        .list_history_filtered(
            100,
            None,
            None,
            Some("apply"),
            Some("completed"),
            Some(&applied.execution_id),
            false,
        )
        .unwrap();
    assert_eq!(apply_history.len(), 1);
    assert_eq!(apply_history[0].success, applied.success);
    assert_eq!(
        apply_history[0].parent_id.as_deref(),
        Some(plan.plan_id.as_str())
    );
    let first_page = store
        .list_history_filtered(1, None, None, None, None, None, false)
        .unwrap();
    let second_page = store
        .list_history_filtered(
            1,
            Some(first_page[0].started_at),
            Some(&first_page[0].id),
            None,
            None,
            None,
            false,
        )
        .unwrap();
    assert_ne!(first_page[0].id, second_page[0].id);
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

    let tamper_error = rusqlite::Connection::open(&database)
        .unwrap()
        .execute(
            // The display path is deliberately not authoritative once a lossless
            // path blob exists. Mutate a canonical authorization field instead.
            "UPDATE plan_items SET action='skip' WHERE plan_id=?1",
            rusqlite::params![plan.plan_id],
        )
        .unwrap_err()
        .to_string();
    assert!(tamper_error.contains("completed_plan_items_immutable"));
    store.validate_plan_snapshot(&plan.plan_id).unwrap();
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
    let page = store
        .list_plan_items(&plan.plan_id, None, 10, Some("cover.jpg"), None)
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.filtered_total, 1);
    assert_eq!(page.counts.moves, 1);
    assert_eq!(page.counts.skips, 0);
    assert_eq!(page.next_cursor, None);
    let items = page.items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].action, "move");
    assert!(items[0]
        .target_path
        .as_deref()
        .unwrap()
        .ends_with("cover.jpg"));
    let first_page = store
        .list_plan_items(&plan.plan_id, None, 1, None, None)
        .unwrap();
    assert_eq!(first_page.total, 2);
    assert_eq!(first_page.filtered_total, 2);
    assert_eq!(first_page.items.len(), 1);
    let second_page = store
        .list_plan_items(&plan.plan_id, first_page.next_cursor, 1, None, None)
        .unwrap();
    assert_eq!(second_page.items.len(), 1);
    assert_eq!(second_page.next_cursor, None);
}

#[test]
fn plan_moves_album_image_to_common_parent_but_keeps_disc_image_in_disc() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    let disc_one = source.join("disc-one");
    let disc_two = source.join("disc-two");
    let target = temp.path().join("target");
    fs::create_dir_all(&disc_one).unwrap();
    fs::create_dir_all(&disc_two).unwrap();
    fs::copy(fixture("mp3/japanese.mp3"), disc_one.join("disc-one.mp3")).unwrap();
    fs::copy(fixture("mp3/japanese.mp3"), disc_two.join("disc-two.mp3")).unwrap();
    fs::write(source.join("cover.jpg"), b"album image").unwrap();
    fs::write(disc_one.join("booklet.jpg"), b"disc image").unwrap();

    let store = Arc::new(SqliteScanStore::open(&temp.path().join("state.db")).unwrap());
    let scan = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(DiscMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
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

    let album_image = store
        .list_plan_items(&plan.plan_id, None, 10, Some("cover.jpg"), None)
        .unwrap();
    assert_eq!(album_image.items.len(), 1);
    assert_eq!(album_image.items[0].action, "move");
    assert_eq!(album_image.items[0].risk, "none");
    assert_eq!(
        album_image.items[0].target_path.as_deref(),
        Some(
            target
                .join("Album Artist")
                .join("Shared Album")
                .join("cover.jpg")
                .to_string_lossy()
                .as_ref()
        )
    );

    let disc_image = store
        .list_plan_items(&plan.plan_id, None, 10, Some("booklet.jpg"), None)
        .unwrap();
    assert_eq!(disc_image.items.len(), 1);
    assert_eq!(
        disc_image.items[0].target_path.as_deref(),
        Some(
            target
                .join("Album Artist")
                .join("Shared Album")
                .join("01")
                .join("booklet.jpg")
                .to_string_lossy()
                .as_ref()
        )
    );
}

#[test]
fn plan_keeps_image_ambiguous_when_disc_targets_belong_to_different_albums() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::copy(fixture("mp3/japanese.mp3"), source.join("album-a.mp3")).unwrap();
    fs::copy(fixture("mp3/japanese.mp3"), source.join("album-b.mp3")).unwrap();
    fs::write(source.join("cover.jpg"), b"album image").unwrap();

    let store = Arc::new(SqliteScanStore::open(&temp.path().join("state.db")).unwrap());
    let scan = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(DiscMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
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

    let page = store
        .list_plan_items(&plan.plan_id, None, 10, Some("cover.jpg"), None)
        .unwrap();
    let image = &page.items[0];
    assert_eq!(image.action, "skip");
    assert_eq!(image.reason.as_deref(), Some("companion_target_ambiguous"));
    assert_eq!(image.conflict_member_count, 2);
    let detail = store
        .get_plan_conflict_detail(&plan.plan_id, image.conflict_group_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(detail.candidates.len(), 2);
}

#[test]
fn plan_sequences_same_named_images_collapsed_to_the_album_directory() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    for edition in ["edition-a", "edition-b"] {
        let edition_root = source.join(edition);
        let disc_one = edition_root.join("disc-one");
        let disc_two = edition_root.join("disc-two");
        fs::create_dir_all(&disc_one).unwrap();
        fs::create_dir_all(&disc_two).unwrap();
        fs::copy(fixture("mp3/japanese.mp3"), disc_one.join("disc-one.mp3")).unwrap();
        fs::copy(fixture("mp3/japanese.mp3"), disc_two.join("disc-two.mp3")).unwrap();
        fs::write(edition_root.join("cover.jpg"), b"album image").unwrap();
    }

    let store = Arc::new(SqliteScanStore::open(&temp.path().join("state.db")).unwrap());
    let scan = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(DiscMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
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

    let page = store
        .list_plan_items(&plan.plan_id, None, 10, Some("cover.jpg"), None)
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(page
        .items
        .iter()
        .all(|item| item.action == "move" && item.risk == "none"));
    let mut names = page
        .items
        .iter()
        .map(|item| {
            Path::new(item.target_path.as_deref().unwrap())
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["cover.jpg", "cover_2.jpg"]);
}

#[test]
fn ambiguous_image_persists_every_destination_candidate_and_music_source() {
    let _guard = mutation_test_guard();
    let temp = tempdir().unwrap();
    let database = temp.path().join("state.db");
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::copy(fixture("mp3/japanese.mp3"), source.join("one.mp3")).unwrap();
    fs::copy(fixture("mp3/japanese.mp3"), source.join("two.mp3")).unwrap();
    fs::write(source.join("cover.jpg"), b"fixture image").unwrap();
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let scan = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(LoftyMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
    let naming = music_folder_core::NamingRules {
        artist_dir_template: "{source_stem}".into(),
        ..Default::default()
    };
    let plan = PlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &scan.scan_id,
        &PlanOptions {
            target_root: target,
            batch_size: 10,
            naming,
        },
    )
    .unwrap();
    let page = store
        .list_plan_items(&plan.plan_id, None, 10, Some("cover.jpg"), None)
        .unwrap();
    let image = &page.items[0];
    assert_eq!(image.reason.as_deref(), Some("companion_target_ambiguous"));
    assert_eq!(image.conflict_member_count, 2);
    assert!(image.target_path.is_none());
    let conflict_group_id = image.conflict_group_id.clone().unwrap();
    let detail = store
        .get_plan_conflict_detail(&plan.plan_id, &conflict_group_id)
        .unwrap();
    assert_eq!(detail.kind, "image_destination");
    assert_eq!(detail.candidates.len(), 2);
    assert_eq!(
        detail
            .candidates
            .iter()
            .map(|candidate| candidate.ordinal)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(detail
        .candidates
        .iter()
        .all(|candidate| candidate.members.len() == 1));
    for candidate in &detail.candidates {
        let resolved = store
            .resolve_plan_conflict_candidate_target(
                &plan.plan_id,
                &image.id,
                &conflict_group_id,
                candidate.ordinal,
            )
            .unwrap();
        assert_eq!(resolved.parent(), Some(Path::new(&candidate.target_path)));
        assert_eq!(resolved.file_name().unwrap(), "cover.jpg");
    }
    assert_eq!(
        store
            .resolve_plan_conflict_candidate_target(
                &plan.plan_id,
                "different-item",
                &conflict_group_id,
                1,
            )
            .unwrap_err(),
        "plan_conflict_candidate_not_found"
    );

    let raw = rusqlite::Connection::open(&database).unwrap();
    let lossless_candidates: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM plan_conflict_candidates
              WHERE conflict_group_id=?1
                AND target_path_encoding IS NOT NULL
                AND target_path_blob IS NOT NULL",
            rusqlite::params![conflict_group_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(lossless_candidates, 2);
    for statement in [
        "UPDATE plan_conflict_candidates SET target_path=target_path || '-tampered' WHERE conflict_group_id=?1",
        "DELETE FROM plan_conflict_candidates WHERE conflict_group_id=?1",
        "UPDATE plan_conflict_candidate_members SET candidate_ordinal=candidate_ordinal WHERE conflict_group_id=?1",
        "DELETE FROM plan_conflict_candidate_members WHERE conflict_group_id=?1",
        "UPDATE plan_conflict_members SET plan_item_id=plan_item_id WHERE conflict_group_id=?1",
        "DELETE FROM plan_conflict_members WHERE conflict_group_id=?1",
    ] {
        let error = raw
            .execute(statement, rusqlite::params![conflict_group_id])
            .unwrap_err()
            .to_string();
        assert!(error.contains("completed_plan_conflicts_immutable"));
    }
    let insert_error = raw
        .execute(
            "INSERT INTO plan_conflict_candidates(
                 conflict_group_id,ordinal,target_path,target_path_encoding,target_path_blob
             ) VALUES(?1,99,'x','utf8_legacy_v1',X'78')",
            rusqlite::params![conflict_group_id],
        )
        .unwrap_err()
        .to_string();
    assert!(insert_error.contains("plan_not_running"));
    let insert_member_error = raw
        .execute(
            "INSERT INTO plan_conflict_candidate_members(
                 conflict_group_id,candidate_ordinal,plan_item_id
             ) VALUES(?1,1,?2)",
            rusqlite::params![conflict_group_id, image.id],
        )
        .unwrap_err()
        .to_string();
    assert!(insert_member_error.contains("plan_not_running"));
    drop(raw);

    let files = Arc::new(LocalFileSystem);
    let dry_run = ApplyUseCase {
        store: Arc::clone(&store),
        files: Arc::clone(&files),
    }
    .execute(&plan.plan_id, true)
    .expect("diagnostic image candidates must not invalidate the plan snapshot");
    assert_eq!(dry_run.success, 2);
    assert_eq!(dry_run.skipped, 1);

    let applied = ApplyUseCase {
        store: Arc::clone(&store),
        files,
    }
    .execute(&plan.plan_id, false)
    .expect("the unchanged persisted plan must remain applicable");
    assert_eq!(applied.success, 2);
    assert_eq!(applied.skipped, 1);

    // Even an external writer with schema-changing privileges cannot turn an
    // unknown future codec into a path identity accepted by the adapter.
    let raw = rusqlite::Connection::open(&database).unwrap();
    raw.execute_batch("DROP TRIGGER plan_conflict_candidates_completed_immutable;")
        .unwrap();
    raw.execute(
        "UPDATE plan_conflict_candidates
            SET target_path_encoding='future_path_v99'
          WHERE conflict_group_id=?1 AND ordinal=1",
        rusqlite::params![conflict_group_id],
    )
    .unwrap();
    drop(raw);
    assert_eq!(
        store
            .resolve_plan_conflict_candidate_target(
                &plan.plan_id,
                &image.id,
                &conflict_group_id,
                1,
            )
            .unwrap_err(),
        "path_encoding_unknown:future_path_v99"
    );
}

#[test]
fn missing_metadata_is_skipped_by_default() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::copy(
        fixture("broken/not-audio.mp3"),
        source.join("unreadable.mp3"),
    )
    .unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("state.db")).unwrap());
    let scan = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(LoftyMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
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
    let page = store
        .list_plan_items(&plan.plan_id, None, 10, None, None)
        .unwrap();
    assert_eq!(page.items[0].action, "skip");
    assert_eq!(page.items[0].risk, "metadata_missing");
    assert_eq!(page.items[0].reason.as_deref(), Some("metadata_missing"));
    assert!(page.items[0].target_path.is_none());
}

#[test]
fn missing_metadata_uses_unknown_folders_when_allowed() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::copy(
        fixture("broken/not-audio.mp3"),
        source.join("unreadable.mp3"),
    )
    .unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("state.db")).unwrap());
    let scan = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(LoftyMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
    let plan = PlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &scan.scan_id,
        &PlanOptions {
            target_root: target,
            batch_size: 10,
            naming: music_folder_core::NamingRules {
                allow_missing_metadata: true,
                ..Default::default()
            },
        },
    )
    .unwrap();
    let page = store
        .list_plan_items(&plan.plan_id, None, 10, None, None)
        .unwrap();
    assert_eq!(page.items[0].action, "move");
    assert_eq!(page.items[0].risk, "metadata_missing");
    let target = page.items[0].target_path.as_deref().unwrap();
    assert!(target.contains("Unknown Artist"));
    assert!(target.contains("Unknown Album"));
    assert!(target.ends_with("unreadable.mp3"));
}

#[test]
fn conflict_detail_lists_every_source_and_revision_rechecks_the_group() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::copy(fixture("mp3/japanese.mp3"), source.join("one.mp3")).unwrap();
    fs::copy(fixture("mp3/japanese.mp3"), source.join("two.mp3")).unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("state.db")).unwrap());
    let scan = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(LoftyMetadataReader),
        store: Arc::clone(&store),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
    let naming = music_folder_core::NamingRules {
        duplicate_strategy: music_folder_core::DuplicateStrategy::Skip,
        ..Default::default()
    };
    let plan = PlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &scan.scan_id,
        &PlanOptions {
            target_root: target.clone(),
            batch_size: 1,
            naming,
        },
    )
    .unwrap();
    assert_eq!(plan.conflicts, 2);
    let page = store
        .list_plan_items(&plan.plan_id, None, 1, None, Some("conflict"))
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].conflict_member_count, 2);
    let group_id = page.items[0].conflict_group_id.as_deref().unwrap();
    let detail = store
        .get_plan_conflict_detail(&plan.plan_id, group_id)
        .unwrap();
    assert_eq!(detail.kind, "plan_items");
    assert_eq!(detail.members.len(), 2);
    assert!(detail
        .members
        .iter()
        .any(|member| member.source_path.ends_with("one.mp3")));
    assert!(detail
        .members
        .iter()
        .any(|member| member.source_path.ends_with("two.mp3")));

    let changed_id = detail.members[0].item_id.clone();
    let child = RevisePlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &plan.plan_id,
        &[ManualTargetChange {
            plan_item_id: changed_id,
            target: target.join("manual.mp3"),
            reason: "衝突を解消".into(),
        }],
    )
    .unwrap();
    let child_page = store.list_plan_items(&child, None, 10, None, None).unwrap();
    assert!(child_page.items.iter().all(|item| item.risk != "conflict"));
    assert!(child_page
        .items
        .iter()
        .all(|item| item.conflict_group_id.is_none()));
}

#[test]
fn manual_target_creates_immutable_child_plan() {
    let _guard = mutation_test_guard();
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
        .items
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
        .unwrap()
        .items;
    assert_ne!(
        parent_after[0].target_path.as_deref(),
        Some(manual_target.to_string_lossy().as_ref())
    );
    let child_item = store
        .list_plan_items(&child, None, 1, None, None)
        .unwrap()
        .items;
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
fn purging_verified_root_scan_removes_descendants_without_touching_files() {
    let _guard = mutation_test_guard();
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
        .items
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
    assert_eq!(
        store.delete_history("plan", &parent.plan_id).unwrap_err(),
        "history_purge_requires_root_scan"
    );
    store.archive_history("scan", &scan.scan_id, None).unwrap();
    store.delete_history("scan", &scan.scan_id).unwrap();
    assert!(original.exists());
    assert_eq!(
        store.get_run_detail("scan", &scan.scan_id).unwrap_err(),
        "run_not_found"
    );
    assert_eq!(
        store.get_run_detail("plan", &parent.plan_id).unwrap_err(),
        "run_not_found"
    );
    assert_eq!(
        store.get_run_detail("plan", &child).unwrap_err(),
        "run_not_found"
    );
}
