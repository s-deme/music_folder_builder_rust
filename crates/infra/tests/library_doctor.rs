use music_folder_core::{
    doctor::{DoctorStore, DoctorUseCase},
    ports::{FileSystem, MetadataReader},
    usecases::{CancellationToken, ScanOptions},
    RunStatus, TrackMetadata,
};
use music_folder_infra::{
    lofty_reader::LoftyMetadataReader, sqlite::SqliteScanStore, windows_fs::DoctorFileSystem,
};
use std::{fs, path::Path, sync::Arc};
use tempfile::tempdir;
mod support;

#[test]
fn wav_pcm_missing_tags_and_doctor_extension_selection() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("無音.wav");
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&40u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&44100u32.to_le_bytes());
    wav.extend_from_slice(&88200u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&4u32.to_le_bytes());
    wav.extend_from_slice(&[0; 4]);
    fs::write(&path, &wav).unwrap();
    let tags = LoftyMetadataReader.read(&path).unwrap();
    assert!(tags.title.is_none());
    assert_eq!(tags.has_artwork, Some(false));
    for extension in ["aac", "opus"] {
        fs::write(temp.path().join(format!("broken.{extension}")), b"invalid").unwrap();
    }
    fs::write(temp.path().join("cover.jpg"), b"not inspected").unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("db")).unwrap());
    let run = execute(temp.path(), store.clone(), &ScanOptions::default());
    assert_eq!(run.files, 3);
    assert_eq!(run.failures, 2);
    assert_eq!(fs::read(path).unwrap(), wav);
}

#[test]
fn hard_links_are_identified_as_one_object() {
    let temp = tempdir().unwrap();
    fs::copy(
        support::fixture("mp3/japanese.mp3"),
        temp.path().join("one.mp3"),
    )
    .unwrap();
    fs::hard_link(temp.path().join("one.mp3"), temp.path().join("two.mp3")).unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("db")).unwrap());
    let run = execute(temp.path(), store.clone(), &ScanOptions::default());
    assert_eq!(
        store
            .doctor_issues(&run.id, None, Some("same_file_paths"), None)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .doctor_issues(&run.id, None, Some("exact_duplicate"), None)
        .unwrap()
        .is_empty());
}

#[test]
fn cancel_during_validation_keeps_a_cancelled_run() {
    let temp = tempdir().unwrap();
    fs::copy(
        support::fixture("mp3/japanese.mp3"),
        temp.path().join("one.mp3"),
    )
    .unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("db")).unwrap());
    let cancellation = CancellationToken::default();
    let signal = cancellation.clone();
    let run = execute(
        temp.path(),
        store.clone(),
        &ScanOptions {
            cancellation,
            progress: Some(Arc::new(move |p| {
                if p.phase == "doctor_validate" {
                    signal.cancel();
                }
            })),
            ..ScanOptions::default()
        },
    );
    assert_eq!(run.status, RunStatus::Cancelled);
    assert_eq!(run.files, 1);
    assert_eq!(
        store.doctor_run(&run.id).unwrap().status,
        RunStatus::Cancelled
    );
}

#[test]
fn changing_file_after_scan_excludes_its_stale_tags() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("one.mp3");
    fs::copy(support::fixture("mp3/japanese.mp3"), &path).unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("db")).unwrap());
    let run = execute(
        temp.path(),
        store.clone(),
        &ScanOptions {
            progress: Some(Arc::new(move |p| {
                if p.phase == "doctor_validate" {
                    fs::write(&path, b"concurrent edit in test copy").unwrap();
                }
            })),
            ..ScanOptions::default()
        },
    );
    assert_eq!(run.status, RunStatus::Partial);
    assert_eq!(run.failures, 1);
    let issues = store.doctor_issues(&run.id, None, None, None).unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].code, "file_changed");
}

#[cfg(unix)]
#[test]
fn symlinks_are_skipped_without_losing_regular_siblings() {
    use std::os::unix::fs::symlink;
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::copy(
        support::fixture("mp3/japanese.mp3"),
        outside.join("hidden.mp3"),
    )
    .unwrap();
    symlink(&outside, source.join("link-dir")).unwrap();
    symlink(outside.join("hidden.mp3"), source.join("link.mp3")).unwrap();
    fs::copy(
        support::fixture("mp3/japanese.mp3"),
        source.join("real.mp3"),
    )
    .unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("db")).unwrap());
    let run = execute(&source, store.clone(), &ScanOptions::default());
    assert_eq!(run.status, RunStatus::Partial);
    assert_eq!(run.files, 1);
    assert_eq!(run.failures, 2);
}

fn execute(
    source: &Path,
    store: Arc<SqliteScanStore>,
    options: &ScanOptions,
) -> music_folder_core::doctor::DoctorRun {
    DoctorUseCase {
        fs: Arc::new(DoctorFileSystem),
        metadata: Arc::new(LoftyMetadataReader),
        store,
    }
    .execute(source, options)
    .unwrap()
}

#[test]
fn persisted_partial_results_duplicates_cache_and_input_are_unchanged() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("音楽");
    fs::create_dir(&source).unwrap();
    let db = temp.path().join("doctor.db");
    let a = source.join("one.mp3");
    let b = source.join("two.mp3");
    let broken = source.join("broken.mp3");
    fs::copy(support::fixture("mp3/japanese.mp3"), &a).unwrap();
    fs::copy(&a, &b).unwrap();
    fs::write(&broken, b"not audio").unwrap();
    let before = DoctorFileSystem.fingerprint(&a).unwrap();
    let store = Arc::new(SqliteScanStore::open(&db).unwrap());
    let options = ScanOptions::default();
    let first = execute(&source, store.clone(), &options);
    assert_eq!(first.status, RunStatus::Partial);
    assert_eq!(first.failures, 1);
    assert_eq!(first.files, 3);
    let duplicate = store
        .doctor_issues(&first.id, None, Some("exact_duplicate"), None)
        .unwrap();
    assert_eq!(duplicate.len(), 1);
    assert_eq!(duplicate[0].file_ids.len(), 2);
    let files = store.doctor_files(&first.scan_id).unwrap();
    assert!(duplicate[0]
        .file_ids
        .iter()
        .all(|id| files.iter().any(|f| f.id == *id)));
    assert_eq!(
        store
            .doctor_issues(&first.id, Some("critical"), Some("read_failed"), None)
            .unwrap()
            .len(),
        1
    );
    assert!(store.doctor_warnings(&first.scan_id).unwrap()[0].contains("metadata_read_failed"));
    let second = execute(&source, store.clone(), &options);
    assert_eq!(second.cache_hits, 2);
    // Reconstruct legacy reader entries in this disposable DB only.
    let mut conn = rusqlite::Connection::open(&db).unwrap();
    let trigger: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='metadata_cache_entries_no_update'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let tx = conn.transaction().unwrap();
    tx.execute_batch("DROP TRIGGER metadata_cache_entries_no_update; UPDATE metadata_cache_entries SET reader_version='lofty-v1';").unwrap();
    tx.execute_batch(&trigger).unwrap();
    tx.commit().unwrap();
    drop(conn);
    assert_eq!(execute(&source, store.clone(), &options).cache_hits, 0);
    let after = DoctorFileSystem.fingerprint(&a).unwrap();
    assert_eq!(before.content_sha256, after.content_sha256);
    assert_eq!(before.mtime_ns, after.mtime_ns);
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
    assert_eq!(fs::read(&broken).unwrap(), b"not audio");
    drop(store);
    let reopened = SqliteScanStore::open(&db).unwrap();
    assert_eq!(
        reopened.doctor_run(&first.id).unwrap().scan_id,
        first.scan_id
    );
    assert_eq!(
        reopened
            .doctor_issues(&first.id, None, None, None)
            .unwrap()
            .len() as u64,
        first.issue_count
    );
}

#[test]
fn same_size_different_bytes_do_not_match() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("one.mp3"), b"aaaa").unwrap();
    fs::write(source.join("two.mp3"), b"bbbb").unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("db")).unwrap());
    let run = execute(&source, store.clone(), &ScanOptions::default());
    assert!(store
        .doctor_issues(&run.id, None, None, Some("duplicates"))
        .unwrap()
        .is_empty());
}

#[test]
fn cancellation_and_fatal_failure_remain_queryable() {
    let temp = tempdir().unwrap();
    let store = Arc::new(SqliteScanStore::open(&temp.path().join("db")).unwrap());
    let cancellation = CancellationToken::default();
    cancellation.cancel();
    let run = execute(
        temp.path(),
        store.clone(),
        &ScanOptions {
            cancellation,
            ..ScanOptions::default()
        },
    );
    assert_eq!(run.status, RunStatus::Cancelled);
    assert_eq!(
        store.doctor_run(&run.id).unwrap().status,
        RunStatus::Cancelled
    );
    let run = execute(
        &temp.path().join("absent"),
        store.clone(),
        &ScanOptions::default(),
    );
    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.error.unwrap().contains("scan_root_not_found"));
    assert_eq!(store.doctor_run(&run.id).unwrap().status, RunStatus::Failed);
}

struct EditingReader;
impl MetadataReader for EditingReader {
    fn read(&self, path: &Path) -> Result<TrackMetadata, String> {
        let tags = LoftyMetadataReader.read(path)?;
        fs::write(path, b"changed during metadata read").unwrap();
        Ok(tags)
    }
}

#[test]
fn changed_during_read_is_not_cached_or_diagnosed_as_stable() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::copy(support::fixture("mp3/japanese.mp3"), source.join("one.mp3")).unwrap();
    let db = temp.path().join("db");
    let store = Arc::new(SqliteScanStore::open(&db).unwrap());
    let run = DoctorUseCase {
        fs: Arc::new(DoctorFileSystem),
        metadata: Arc::new(EditingReader),
        store: store.clone(),
    }
    .execute(&source, &ScanOptions::default())
    .unwrap();
    assert_eq!(run.status, RunStatus::Partial);
    assert_eq!(run.files, 0);
    assert!(
        store.doctor_warnings(&run.scan_id).unwrap()[0].contains("source_changed_during_metadata")
    );
    let count: i64 = rusqlite::Connection::open(db)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM metadata_cache_entries", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn version_16_database_upgrades_without_rewriting_existing_scans() {
    let temp = tempdir().unwrap();
    let db = temp.path().join("db");
    let store = Arc::new(SqliteScanStore::open(&db).unwrap());
    let run = execute(temp.path(), store.clone(), &ScanOptions::default());
    drop(store);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let hash: String = conn
        .query_row(
            "SELECT snapshot_hash FROM scan_runs WHERE id=?1",
            [&run.scan_id],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute_batch("DROP TABLE doctor_issues; DROP TABLE doctor_runs; DELETE FROM schema_migrations WHERE version=17; PRAGMA user_version=16;").unwrap();
    drop(conn);
    let store = SqliteScanStore::open(&db).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&db).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        17
    );
    assert_eq!(
        conn.query_row::<String, _, _>(
            "SELECT snapshot_hash FROM scan_runs WHERE id=?1",
            [&run.scan_id],
            |r| r.get(0)
        )
        .unwrap(),
        hash
    );
}

#[test]
fn embedded_artwork_and_genre_are_read_without_retaining_image_bytes() {
    use lofty::{
        config::WriteOptions,
        picture::{MimeType, Picture, PictureType},
        prelude::{Accessor, TagExt, TaggedFileExt},
        probe::Probe,
    };
    let temp = tempdir().unwrap();
    let path = temp.path().join("art.mp3");
    fs::copy(support::fixture("mp3/japanese.mp3"), &path).unwrap();
    assert_eq!(
        LoftyMetadataReader.read(&path).unwrap().has_artwork,
        Some(false)
    );
    let mut tagged = Probe::open(&path).unwrap().read().unwrap();
    let tag = tagged.primary_tag_mut().unwrap();
    tag.set_genre("Jazz".into());
    // Minimal synthetic PNG payload; Phase 1 tests presence, not image validity.
    tag.push_picture(Picture::new_unchecked(
        PictureType::CoverFront,
        Some(MimeType::Png),
        None,
        b"\x89PNG\r\n\x1a\n".to_vec(),
    ));
    tag.save_to_path(&path, WriteOptions::default()).unwrap();
    let tags = LoftyMetadataReader.read(&path).unwrap();
    assert_eq!(tags.has_artwork, Some(true));
    assert_eq!(tags.genre.as_deref(), Some("Jazz"));
}
