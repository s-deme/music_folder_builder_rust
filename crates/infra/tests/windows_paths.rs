#![cfg(windows)]

use music_folder_core::{
    ports::{FileSystem, PlanStore, ScanStore},
    sanitize_component, FileFingerprint, FileKind, RunStatus, ScannedFile,
};
use music_folder_infra::{sqlite::SqliteScanStore, windows_fs::LocalFileSystem};
use rusqlite::Connection;
use std::{
    ffi::OsString,
    fs,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::symlink_dir,
    },
};
use tempfile::tempdir;
use uuid::Uuid;

#[test]
fn japanese_long_paths_are_enumerated_and_reserved_names_are_sanitized() {
    let temp = tempdir().unwrap();
    let mut library = temp.path().join("日本語音楽");
    for index in 0..20 {
        library = library.join(format!("長いディレクトリ区間{index:02}"));
    }
    let track = library.join("楽曲.mp3");
    let utf16_units = track.as_os_str().encode_wide().count();
    assert!(
        utf16_units > 260,
        "fixture must cross the legacy Windows path boundary: {utf16_units} UTF-16 units"
    );
    fs::create_dir_all(&library).unwrap();
    fs::write(&track, b"test").unwrap();
    let mut found = Vec::new();
    LocalFileSystem
        .enumerate(temp.path(), false, &mut |item| {
            found.push(item.unwrap());
            true
        })
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0], track);
    assert_eq!(sanitize_component("CON"), "_CON");
    assert_eq!(sanitize_component("曲?.mp3"), "曲_.mp3");
}

#[test]
fn reparse_directory_is_not_followed_by_default() {
    let temp = tempdir().unwrap();
    let outside = temp.path().join("outside");
    let library = temp.path().join("library");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(&library).unwrap();
    fs::write(outside.join("hidden.mp3"), b"test").unwrap();
    match symlink_dir(&outside, library.join("linked")) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(1314) => {
            // Junction creation needs no symlink privilege, and exercises the
            // Windows directory reparse traversal guard instead of skipping it.
            let output = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(library.join("linked"))
                .arg(&outside)
                .output()
                .expect("create test junction");
            assert!(
                output.status.success(),
                "junction creation failed: {:?}",
                output
            );
        }
        Err(error) => panic!("Windows CI must permit symlink creation: {error}"),
    }
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    LocalFileSystem
        .enumerate(&library, false, &mut |item| {
            match item {
                Ok(path) => found.push(path),
                Err(warning) => warnings.push(warning),
            }
            true
        })
        .unwrap();
    assert!(found.is_empty());
    assert!(warnings
        .iter()
        .any(|warning| warning.starts_with("reparse_point_skipped:")));
}

#[test]
fn scan_snapshot_keeps_distinct_utf16_paths_with_the_same_display_text() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("lossless-scan.db");
    let source_root = temp.path().join("source");
    let first = source_root.join(OsString::from_wide(&[
        0xd800,
        u16::from(b'.'),
        u16::from(b'm'),
        u16::from(b'p'),
        u16::from(b'3'),
    ]));
    let second = source_root.join(OsString::from_wide(&[
        0xd801,
        u16::from(b'.'),
        u16::from(b'm'),
        u16::from(b'p'),
        u16::from(b'3'),
    ]));
    assert_ne!(first, second);
    assert_eq!(first.to_string_lossy(), second.to_string_lossy());

    let store = SqliteScanStore::open(&database).unwrap();
    let scan_id = store.begin_scan(&source_root).unwrap();
    let files = [first.clone(), second.clone()].map(|path| ScannedFile {
        id: Uuid::new_v4(),
        path,
        fingerprint: FileFingerprint {
            size_bytes: 1,
            mtime_ns: 2,
            content_sha256: Some("hash".into()),
            file_identity: None,
            version: 1,
        },
        metadata: None,
        kind: FileKind::Image,
    });
    store.save_batch(&scan_id, &files).unwrap();
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();

    let mut expected = vec![
        first.as_os_str().encode_wide().collect::<Vec<_>>(),
        second.as_os_str().encode_wide().collect::<Vec<_>>(),
    ];
    expected.sort();
    let mut loaded = store
        .load_completed_scan(&scan_id)
        .unwrap()
        .into_iter()
        .map(|file| file.path.as_os_str().encode_wide().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    loaded.sort();
    assert_eq!(loaded, expected);

    let raw = Connection::open(&database).unwrap();
    let (rows, display_values, raw_values, keys): (i64, i64, i64, i64) = raw
        .query_row(
            "SELECT COUNT(*),COUNT(DISTINCT path),COUNT(DISTINCT path_blob),
                    COUNT(DISTINCT path_key)
               FROM scan_items WHERE scan_id=?1",
            rusqlite::params![scan_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!((rows, display_values, raw_values, keys), (2, 1, 2, 2));
    let immutable = raw
        .execute(
            "UPDATE scan_items SET path=path || '-display' WHERE scan_id=?1",
            rusqlite::params![scan_id],
        )
        .unwrap_err()
        .to_string();
    assert!(immutable.contains("scan_items_immutable"));
}
