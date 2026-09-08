use music_folder_core::{
    ports::{PlanStore, ScanStore},
    windows_path_key, ExecutionDisposition, FileFingerprint, FileKind, NamingRules, PlanAction,
    PlanConflictCandidate, PlanItem, Risk, RunStatus, ScannedFile, TrackMetadata,
};
use music_folder_infra::sqlite::SqliteScanStore;
use rusqlite::{params, Connection};
use std::path::Path;
use tempfile::tempdir;
use uuid::Uuid;

fn metadata(artist: &str) -> TrackMetadata {
    TrackMetadata {
        artist: Some(artist.into()),
        album_artist: None,
        album: Some("Album".into()),
        title: Some("Title".into()),
        track_no: Some(1),
        disc_no: Some(1),
        year: Some(2026),
    }
}

fn scanned_file(path: &Path, size_bytes: u64, mtime_ns: i128, artist: &str) -> ScannedFile {
    ScannedFile {
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        fingerprint: FileFingerprint {
            size_bytes,
            mtime_ns,
            content_sha256: Some(format!("hash-{size_bytes}-{mtime_ns}")),
            file_identity: Some(format!("identity-{size_bytes}-{mtime_ns}")),
            version: 1,
        },
        metadata: Some(metadata(artist)),
        kind: FileKind::Music,
    }
}

#[test]
fn completed_scan_is_immutable_when_a_later_scan_observes_the_same_path() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("snapshot.db");
    let store = SqliteScanStore::open(&database).unwrap();
    let source_root = temp.path().join("source");
    let shared_path = source_root.join("track.mp3");

    let scan_a = store.begin_scan(&source_root).unwrap();
    store
        .save_batch(&scan_a, &[scanned_file(&shared_path, 10, 100, "Artist A")])
        .unwrap();
    store.finish_scan(&scan_a, RunStatus::Completed, 0).unwrap();

    let scan_b = store.begin_scan(&source_root).unwrap();
    store
        .save_batch(&scan_b, &[scanned_file(&shared_path, 20, 200, "Artist B")])
        .unwrap();
    store.finish_scan(&scan_b, RunStatus::Completed, 0).unwrap();

    let loaded_a = store.load_completed_scan(&scan_a).unwrap();
    let loaded_b = store.load_completed_scan(&scan_b).unwrap();
    assert_eq!(loaded_a.len(), 1);
    assert_eq!(loaded_a[0].fingerprint.size_bytes, 10);
    assert_eq!(loaded_a[0].fingerprint.mtime_ns, 100);
    assert_eq!(
        loaded_a[0].metadata.as_ref().unwrap().artist.as_deref(),
        Some("Artist A")
    );
    assert_eq!(loaded_b[0].fingerprint.size_bytes, 20);
    assert_eq!(loaded_b[0].fingerprint.mtime_ns, 200);
    assert_eq!(
        loaded_b[0].metadata.as_ref().unwrap().artist.as_deref(),
        Some("Artist B")
    );

    let immutable_error = store
        .save_batch(&scan_a, &[scanned_file(&shared_path, 30, 300, "mutated")])
        .unwrap_err();
    assert!(immutable_error.contains("scan_not_running"));
    assert_eq!(
        store.load_completed_scan(&scan_a).unwrap()[0]
            .metadata
            .as_ref()
            .unwrap()
            .artist
            .as_deref(),
        Some("Artist A")
    );

    let plan_id = store
        .begin_plan(
            &scan_a,
            &temp.path().join("target"),
            &NamingRules::default(),
        )
        .unwrap();
    store
        .save_plan_items(
            &plan_id,
            &[PlanItem {
                id: Uuid::new_v4(),
                conflict_group_id: None,
                ordinal: 1,
                file: loaded_a[0].clone(),
                target: Some(temp.path().join("target/track.mp3")),
                action: PlanAction::Move,
                disposition: music_folder_core::ExecutionDisposition::Executable,
                risk: Risk::None,
                reason: None,
                issues: Vec::new(),
                conflict_candidates: Vec::new(),
            }],
        )
        .unwrap();
    let raw = Connection::open(&database).unwrap();
    let expectation: (i64, String) = raw
        .query_row(
            "SELECT source_size_bytes,source_mtime_ns
               FROM plan_items WHERE plan_id=?1",
            params![plan_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(expectation, (10, "100".into()));
}

#[test]
fn v15_scan_items_are_backfilled_to_lossless_keys_and_ordinals() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("v15-scan-items.db");
    let source_root = temp.path().join("source");
    let first = source_root.join("a.mp3");
    let second = source_root.join("b.mp3");
    let store = SqliteScanStore::open(&database).unwrap();
    let scan_id = store.begin_scan(&source_root).unwrap();
    store
        .save_batch(
            &scan_id,
            &[
                scanned_file(&second, 20, 200, "Artist B"),
                scanned_file(&first, 10, 100, "Artist A"),
            ],
        )
        .unwrap();
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();
    drop(store);

    let legacy = Connection::open(&database).unwrap();
    legacy
        .execute_batch(
            "DROP TRIGGER scan_items_no_update;
             DROP TRIGGER scan_items_only_while_running;
             CREATE TABLE scan_items_v15 (
                 scan_id TEXT NOT NULL REFERENCES scan_runs(id) ON DELETE CASCADE,
                 path TEXT NOT NULL,
                 size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
                 mtime_ns TEXT NOT NULL CHECK(length(mtime_ns) > 0),
                 metadata_json TEXT,
                 metadata_status TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 path_encoding TEXT,
                 path_blob BLOB,
                 content_sha256 TEXT,
                 file_identity TEXT,
                 fingerprint_version INTEGER NOT NULL DEFAULT 1,
                 PRIMARY KEY(scan_id,path)
             );
             INSERT INTO scan_items_v15(
                 scan_id,path,size_bytes,mtime_ns,metadata_json,metadata_status,kind,
                 path_encoding,path_blob,content_sha256,file_identity,fingerprint_version
             )
             SELECT scan_id,path,size_bytes,mtime_ns,metadata_json,metadata_status,kind,
                    path_encoding,path_blob,content_sha256,file_identity,fingerprint_version
               FROM scan_items;
             DROP TABLE scan_items;
             ALTER TABLE scan_items_v15 RENAME TO scan_items;
             CREATE INDEX idx_scan_items_scan ON scan_items(scan_id);
             CREATE INDEX idx_scan_items_path_blob
                 ON scan_items(scan_id,path_encoding,path_blob);
             CREATE TRIGGER scan_items_no_update BEFORE UPDATE ON scan_items
             BEGIN SELECT RAISE(ABORT,'scan_items_immutable'); END;
             CREATE TRIGGER scan_items_only_while_running BEFORE INSERT ON scan_items
             WHEN COALESCE((SELECT status FROM scan_runs WHERE id=NEW.scan_id),'')!='running'
             BEGIN SELECT RAISE(ABORT,'scan_not_running'); END;
             DELETE FROM schema_migrations WHERE version=16;
             PRAGMA user_version=15;",
        )
        .unwrap();
    drop(legacy);

    let upgraded = SqliteScanStore::open(&database).unwrap();
    let loaded = upgraded.load_completed_scan(&scan_id).unwrap();
    assert_eq!(loaded.len(), 2);
    drop(upgraded);
    let raw = Connection::open(&database).unwrap();
    let rows = raw
        .prepare(
            "SELECT ordinal,path,path_key_version,path_key
               FROM scan_items WHERE scan_id=?1 ORDER BY ordinal",
        )
        .unwrap()
        .query_map(params![scan_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        (
            0,
            first.to_string_lossy().into_owned(),
            1,
            windows_path_key(&first)
        )
    );
    assert_eq!(
        rows[1],
        (
            1,
            second.to_string_lossy().into_owned(),
            1,
            windows_path_key(&second)
        )
    );
    let path_pk: i64 = raw
        .query_row(
            "SELECT pk FROM pragma_table_info('scan_items') WHERE name='path'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(path_pk, 0);
    let immutable = raw
        .execute(
            "UPDATE scan_items SET path='changed' WHERE scan_id=?1",
            params![scan_id],
        )
        .unwrap_err()
        .to_string();
    assert!(immutable.contains("scan_items_immutable"));
}

#[test]
fn v16_rejects_running_plan_graph_reparenting_into_a_completed_plan() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("v16-plan-graph.db");
    let store = SqliteScanStore::open(&database).unwrap();
    let scan_id = store.begin_scan(&temp.path().join("source")).unwrap();
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();
    let completed_plan = store
        .begin_plan(
            &scan_id,
            &temp.path().join("completed-target"),
            &NamingRules::default(),
        )
        .unwrap();
    let running_plan = store
        .begin_plan(
            &scan_id,
            &temp.path().join("running-target"),
            &NamingRules::default(),
        )
        .unwrap();
    let raw = Connection::open(&database).unwrap();
    for (prefix, plan_id) in [("p", completed_plan.as_str()), ("q", running_plan.as_str())] {
        raw.execute(
            "INSERT INTO plan_items(
                 id,plan_id,ordinal,source_path,source_path_encoding,source_path_blob,
                 conflict_group_id,action,risk
             ) VALUES(?1,?2,0,?3,'utf8_legacy_v1',CAST(?3 AS BLOB),?4,'skip','none')",
            params![
                format!("{prefix}-item"),
                plan_id,
                format!("{prefix}-source.mp3"),
                format!("{prefix}-group")
            ],
        )
        .unwrap();
        raw.execute(
            "INSERT INTO plan_conflict_groups(
                 id,plan_id,kind,normalized_target_path,target_path
             ) VALUES(?1,?2,'image_destination',?3,?3)",
            params![
                format!("{prefix}-group"),
                plan_id,
                format!("{prefix}-target")
            ],
        )
        .unwrap();
        raw.execute(
            "INSERT INTO plan_conflict_members(conflict_group_id,plan_item_id)
             VALUES(?1,?2)",
            params![format!("{prefix}-group"), format!("{prefix}-item")],
        )
        .unwrap();
        raw.execute(
            "INSERT INTO plan_conflict_candidates(
                 conflict_group_id,ordinal,target_path,target_path_encoding,target_path_blob
             ) VALUES(?1,0,?2,'utf8_legacy_v1',CAST(?2 AS BLOB))",
            params![format!("{prefix}-group"), format!("{prefix}-candidate")],
        )
        .unwrap();
        raw.execute(
            "INSERT INTO plan_conflict_candidate_members(
                 conflict_group_id,candidate_ordinal,plan_item_id
             ) VALUES(?1,0,?2)",
            params![format!("{prefix}-group"), format!("{prefix}-item")],
        )
        .unwrap();
    }
    raw.execute(
        "UPDATE plan_runs SET status='completed',finished_at=1 WHERE id=?1",
        params![completed_plan],
    )
    .unwrap();

    for (statement, parameter, expected) in [
        (
            "UPDATE plan_items SET plan_id=?1 WHERE id='q-item'",
            completed_plan.as_str(),
            "completed_plan_items_immutable",
        ),
        (
            "UPDATE plan_conflict_groups SET plan_id=?1 WHERE id='q-group'",
            completed_plan.as_str(),
            "completed_plan_conflicts_immutable",
        ),
        (
            "UPDATE plan_conflict_members SET conflict_group_id=?1
              WHERE conflict_group_id='q-group'",
            "p-group",
            "completed_plan_conflicts_immutable",
        ),
        (
            "UPDATE plan_conflict_candidates
                SET conflict_group_id=?1,ordinal=1
              WHERE conflict_group_id='q-group'",
            "p-group",
            "completed_plan_conflicts_immutable",
        ),
        (
            "UPDATE plan_conflict_candidate_members
                SET conflict_group_id=?1,candidate_ordinal=0
              WHERE conflict_group_id='q-group'",
            "p-group",
            "completed_plan_conflicts_immutable",
        ),
    ] {
        let error = raw
            .execute(statement, params![parameter])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(expected) || error.contains("plan_graph_scope_mismatch"),
            "unexpected rejection: {error}"
        );
    }
    let cross_plan = raw
        .execute(
            "INSERT INTO plan_conflict_members(conflict_group_id,plan_item_id)
             VALUES('q-group','p-item')",
            [],
        )
        .unwrap_err()
        .to_string();
    assert!(cross_plan.contains("plan_graph_scope_mismatch"));

    raw.execute_batch(
        "DROP TRIGGER plan_conflict_members_completed_immutable;
         DROP TRIGGER plan_conflict_members_same_plan_update;
         DROP TRIGGER plan_items_conflict_group_same_plan_update;
         UPDATE plan_conflict_members
            SET conflict_group_id='p-group' WHERE conflict_group_id='q-group';
         UPDATE plan_items SET conflict_group_id='p-group' WHERE id='q-item';",
    )
    .unwrap();
    drop(raw);
    assert_eq!(
        store
            .resolve_plan_conflict_candidate_target(&completed_plan, "q-item", "p-group", 0,)
            .unwrap_err(),
        "plan_conflict_candidate_not_found"
    );
}

#[test]
fn metadata_cache_requires_full_identity_and_provenance_key() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("metadata-cache-key.db");
    let store = SqliteScanStore::open(&database).unwrap();
    let source_root = temp.path().join("source");
    let path = source_root.join("track.mp3");
    let file = scanned_file(&path, 10, 100, "Cached Artist");
    let scan_id = store.begin_scan(&source_root).unwrap();
    store
        .save_batch(&scan_id, std::slice::from_ref(&file))
        .unwrap();

    assert_eq!(
        store
            .previous_metadata(&path, &file.fingerprint)
            .unwrap()
            .unwrap()
            .artist
            .as_deref(),
        Some("Cached Artist")
    );
    for changed in [
        FileFingerprint {
            file_identity: Some("replacement-identity".into()),
            ..file.fingerprint.clone()
        },
        FileFingerprint {
            version: file.fingerprint.version + 1,
            ..file.fingerprint.clone()
        },
        FileFingerprint {
            content_sha256: Some("replacement-content".into()),
            ..file.fingerprint.clone()
        },
        FileFingerprint {
            file_identity: None,
            ..file.fingerprint.clone()
        },
    ] {
        assert!(store.previous_metadata(&path, &changed).unwrap().is_none());
    }

    let raw = Connection::open(&database).unwrap();
    let provenance: (String, String, String, i64, String) = raw
        .query_row(
            "SELECT reader_id,reader_config_hash,fingerprint_algorithm,
                    path_normalization_version,kind
               FROM metadata_cache_entries ORDER BY id DESC LIMIT 1",
            [],
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
    assert_eq!(provenance.0, "lofty");
    assert_ne!(provenance.1, "legacy-unknown");
    assert_eq!(provenance.2, "sha256-v1");
    assert_eq!(
        provenance.3,
        i64::from(music_folder_core::WINDOWS_PATH_KEY_VERSION)
    );
    assert_eq!(provenance.4, "music");

    raw.execute(
        "INSERT INTO metadata_cache_entries(
             path,path_encoding,path_blob,size_bytes,mtime_ns,content_sha256,
             file_identity,fingerprint_version,fingerprint_algorithm,
             reader_id,reader_version,schema_version,reader_config_hash,
             path_normalization_version,metadata_json,metadata_status,kind,
             scan_id,created_at
         )
         SELECT path,path_encoding,path_blob,size_bytes,mtime_ns,'forged-content',
                file_identity,fingerprint_version,fingerprint_algorithm,
                reader_id,reader_version,schema_version,'different-config',
                path_normalization_version,metadata_json,metadata_status,kind,
                scan_id,created_at+1
           FROM metadata_cache_entries ORDER BY id DESC LIMIT 1",
        [],
    )
    .unwrap();
    let forged = FileFingerprint {
        content_sha256: Some("forged-content".into()),
        ..file.fingerprint
    };
    assert!(store.previous_metadata(&path, &forged).unwrap().is_none());
}

#[test]
fn v14_conflict_candidate_display_paths_are_backfilled_with_a_legacy_codec() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("v14-candidate.db");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    let music_file = scanned_file(&source_root.join("track.mp3"), 10, 100, "Artist");
    let image_file = ScannedFile {
        id: Uuid::new_v4(),
        path: source_root.join("cover.jpg"),
        fingerprint: FileFingerprint {
            size_bytes: 20,
            mtime_ns: 200,
            content_sha256: Some("image-hash".into()),
            file_identity: Some("image-identity".into()),
            version: 1,
        },
        metadata: None,
        kind: FileKind::Image,
    };
    let store = SqliteScanStore::open(&database).unwrap();
    let scan_id = store.begin_scan(&source_root).unwrap();
    store
        .save_batch(&scan_id, &[music_file.clone(), image_file.clone()])
        .unwrap();
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();
    let plan_id = store
        .begin_plan(&scan_id, &target_root, &NamingRules::default())
        .unwrap();
    let music_item_id = Uuid::new_v4();
    let group_id = Uuid::new_v4();
    let candidate_directory = target_root.join("Artist").join("Album");
    let music_item = PlanItem {
        id: music_item_id,
        conflict_group_id: None,
        ordinal: 1,
        file: music_file,
        target: Some(candidate_directory.join("track.mp3")),
        action: PlanAction::Move,
        disposition: ExecutionDisposition::Executable,
        risk: Risk::None,
        reason: None,
        issues: Vec::new(),
        conflict_candidates: Vec::new(),
    };
    let mut image_item = PlanItem {
        id: Uuid::new_v4(),
        conflict_group_id: Some(group_id),
        ordinal: 2,
        file: image_file,
        target: None,
        action: PlanAction::Skip,
        disposition: ExecutionDisposition::Blocked,
        risk: Risk::Conflict,
        reason: None,
        issues: Vec::new(),
        conflict_candidates: vec![PlanConflictCandidate {
            target_directory: candidate_directory.clone(),
            music_item_ids: vec![music_item_id],
        }],
    };
    image_item.set_outcome(
        PlanAction::Skip,
        ExecutionDisposition::Blocked,
        Risk::Conflict,
        Some("companion_target_ambiguous".into()),
    );
    store
        .save_plan_items(&plan_id, &[music_item, image_item])
        .unwrap();
    store.finish_plan(&plan_id, 1, 1, "snapshot").unwrap();
    drop(store);

    let legacy = Connection::open(&database).unwrap();
    legacy
        .execute_batch(
            "DROP TRIGGER plan_conflict_candidates_lossless_required_insert;
             DROP TRIGGER plan_conflict_candidates_lossless_required_update;
             ALTER TABLE plan_conflict_candidates DROP COLUMN target_path_encoding;
             ALTER TABLE plan_conflict_candidates DROP COLUMN target_path_blob;
             DELETE FROM schema_migrations WHERE version=15;
             PRAGMA user_version=14;",
        )
        .unwrap();
    drop(legacy);

    drop(SqliteScanStore::open(&database).unwrap());
    let upgraded = Connection::open(&database).unwrap();
    let (persisted_group, display, encoding, raw): (String, String, String, Vec<u8>) = upgraded
        .query_row(
            "SELECT conflict_group_id,target_path,target_path_encoding,target_path_blob
               FROM plan_conflict_candidates
              WHERE ordinal=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_ne!(persisted_group, group_id.to_string());
    assert_eq!(encoding, "utf8_legacy_v1");
    assert_eq!(raw, display.as_bytes());
    assert_eq!(Path::new(&display), candidate_directory);
    drop(upgraded);

    let store = SqliteScanStore::open(&database).unwrap();
    let archived = store
        .archive_history("scan", &scan_id, Some(&temp.path().join("archives")))
        .unwrap();
    assert!(archived.record_count > 0);
    let archive_jsonl = std::fs::read_to_string(&archived.archive_path).unwrap();
    assert!(archive_jsonl.contains("\"entity\":\"plan_conflict_candidates\""));
    assert!(archive_jsonl.contains("\"target_path_encoding\""));
    assert!(archive_jsonl.contains("\"target_path_blob\""));
    store.delete_history("scan", &scan_id).unwrap();
    let remaining_candidates: i64 = Connection::open(&database)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM plan_conflict_candidates", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(remaining_candidates, 0);
}

#[test]
fn legacy_database_is_transactionally_upgraded_and_backfilled() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("legacy.db");
    let metadata_json = serde_json::to_string(&metadata("Legacy Artist")).unwrap();
    {
        let legacy = Connection::open(&database).unwrap();
        legacy
            .execute_batch(
                "PRAGMA foreign_keys=ON;
                 CREATE TABLE schema_migrations (
                     version INTEGER PRIMARY KEY,
                     applied_at INTEGER NOT NULL
                 );
                 CREATE TABLE scan_runs (
                     id TEXT PRIMARY KEY,
                     source_root TEXT NOT NULL,
                     status TEXT NOT NULL,
                     started_at INTEGER NOT NULL,
                     finished_at INTEGER,
                     warning_count INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE library_files (
                     path TEXT PRIMARY KEY,
                     size_bytes INTEGER NOT NULL,
                     mtime_ns TEXT NOT NULL,
                     metadata_json TEXT,
                     metadata_status TEXT NOT NULL,
                     last_seen_scan_id TEXT NOT NULL
                 );
                 CREATE TABLE scan_items (
                     scan_id TEXT NOT NULL REFERENCES scan_runs(id),
                     path TEXT NOT NULL,
                     PRIMARY KEY(scan_id,path)
                 );",
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO scan_runs(
                     id,source_root,status,started_at,finished_at,warning_count
                 ) VALUES('scan-a','C:\\Music','completed',1,2,0)",
                [],
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO library_files(
                     path,size_bytes,mtime_ns,metadata_json,metadata_status,last_seen_scan_id
                 ) VALUES(?1,42,'1234',?2,'ok','scan-a')",
                params![r"C:\Music\legacy.mp3", metadata_json],
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO scan_items(scan_id,path) VALUES('scan-a',?1)",
                params![r"C:\Music\legacy.mp3"],
            )
            .unwrap();
    }

    let store = SqliteScanStore::open(&database).unwrap();
    let loaded = store.load_completed_scan("scan-a").unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].fingerprint.size_bytes, 42);
    assert_eq!(loaded[0].fingerprint.mtime_ns, 1234);
    assert_eq!(
        loaded[0].metadata.as_ref().unwrap().artist.as_deref(),
        Some("Legacy Artist")
    );
    drop(store);

    let upgraded = Connection::open(&database).unwrap();
    let versions: i64 = upgraded
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(versions, 16);
    let journal_columns: Vec<String> = upgraded
        .prepare("PRAGMA table_info(operation_journal)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(journal_columns.iter().any(|column| column == "strategy"));
    assert!(journal_columns
        .iter()
        .any(|column| column == "staged_file_identity"));
    let archive_columns: Vec<String> = upgraded
        .prepare("PRAGMA table_info(archive_manifests)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(archive_columns
        .iter()
        .any(|column| column == "archive_path_encoding"));
    assert!(archive_columns
        .iter()
        .any(|column| column == "archive_path_blob"));
    let snapshot_columns: Vec<String> = upgraded
        .prepare("PRAGMA table_info(scan_items)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for required in [
        "ordinal",
        "path",
        "size_bytes",
        "mtime_ns",
        "metadata_json",
        "metadata_status",
        "kind",
        "path_encoding",
        "path_blob",
        "path_key_version",
        "path_key",
        "content_sha256",
        "file_identity",
        "fingerprint_version",
    ] {
        assert!(snapshot_columns.iter().any(|column| column == required));
    }
    let legacy_cache_version: (String, i64) = upgraded
        .query_row(
            "SELECT reader_version,schema_version FROM metadata_cache_entries",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(legacy_cache_version, ("legacy-unknown".into(), 0));
    assert!(upgraded
        .execute(
            "UPDATE metadata_cache_entries SET metadata_status='error'",
            [],
        )
        .is_err());

    drop(upgraded);
    SqliteScanStore::open(&database).unwrap();
}
