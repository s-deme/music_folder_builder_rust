use music_folder_core::ports::FileSystem;
use music_folder_core::{
    doctor::{diagnose, DoctorStore},
    ports::{MetadataReader, ScanStore},
    usecases::CancellationToken,
    FileFingerprint, FileKind, RunStatus, ScannedFile, TrackMetadata,
};
use music_folder_infra::{
    artwork::ArtworkService, doctor_view::DoctorView, lofty_reader::LoftyMetadataReader,
    sqlite::SqliteScanStore, windows_fs::DoctorFileSystem,
};
use std::{fs, io::Cursor, path::Path, time::Instant};
use tempfile::tempdir;
use uuid::Uuid;
mod support;

fn song(root: &Path, index: usize) -> ScannedFile {
    ScannedFile {
        id: Uuid::new_v4(),
        path: root.join(format!(
            "Album{:04}/CD{}/曲{index}.mp3",
            index / 10,
            1 + index % 10 / 5
        )),
        fingerprint: FileFingerprint::legacy(10, 1),
        kind: FileKind::Music,
        metadata: Some(TrackMetadata {
            artist: Some("Artist".into()),
            album_artist: Some("Artist".into()),
            album: Some(format!("Album{}", index / 10)),
            title: Some(format!("曲{index}")),
            track_no: Some((index % 5 + 1) as u32),
            disc_no: Some((index % 10 / 5 + 1) as u32),
            year: Some(2026),
            genre: Some("Rock".into()),
            has_artwork: Some(false),
        }),
    }
}
fn view(store: &SqliteScanStore, source: &Path, files: &[ScannedFile]) -> DoctorView {
    let scan = store.begin_scan(source).unwrap();
    store.save_batch(&scan, files).unwrap();
    store.finish_scan(&scan, RunStatus::Completed, 0).unwrap();
    let files = store.doctor_files(&scan).unwrap();
    let mut run = store.begin_doctor(&scan).unwrap();
    let issues = diagnose(
        &files.iter().collect::<Vec<_>>(),
        &CancellationToken::default(),
    );
    run.status = RunStatus::Completed;
    run.files = files.len() as u64;
    run.issue_count = issues.len() as u64;
    store.finish_doctor(&run, &issues).unwrap();
    DoctorView::new(
        store.doctor_history_entry(&run.id).unwrap(),
        source.into(),
        files,
        issues,
    )
}

#[test]
fn persistent_history_paging_and_album_projection_preserve_rule_v1() {
    let temp = tempdir().unwrap();
    let db = temp.path().join("db");
    let source = temp.path().join("音楽");
    let store = SqliteScanStore::open(&db).unwrap();
    let mut files: Vec<_> = (0..110).map(|n| song(&source, n)).collect();
    // Missing album and read failure are available in an explicit unclassified group.
    files[0].metadata.as_mut().unwrap().album = None;
    files[1].metadata = None;
    let v = view(&store, &source, &files);
    let run_id = v.history.run.id.clone();
    assert_eq!(v.albums.iter().filter(|a| a.row.unclassified).count(), 1);
    assert_eq!(v.albums.iter().map(|a| a.row.tracks).sum::<usize>(), 110);
    let first = store
        .doctor_issue_page(&run_id, 0, None, None, None)
        .unwrap();
    assert_eq!(first.items.len(), 100);
    assert_eq!(first.next_cursor, Some(100));
    let rest = store
        .doctor_issue_page(&run_id, 100, None, None, None)
        .unwrap();
    assert!(rest.items[0].ordinal > first.items[99].ordinal);
    assert_eq!(rest.next_cursor, None);
    let critical = store
        .doctor_issue_page(
            &run_id,
            0,
            Some("critical"),
            Some("read_failed"),
            Some("read"),
        )
        .unwrap();
    assert_eq!(critical.total, 1);
    let detail = v.issue_detail(critical.items[0].ordinal, 0).unwrap();
    assert_eq!(detail.files.items.len(), 1);
    assert!(detail.files.items[0].metadata.is_none());
    assert!(v.album("invented").is_err());
    assert!(v.issue_detail(usize::MAX, 0).is_err());
    assert!(v.album_page("does-not-exist", 0).items.is_empty());
    let scan = store.begin_scan(&source).unwrap();
    let incomplete = store.begin_doctor(&scan).unwrap();
    drop(store);
    let reopened = SqliteScanStore::open(&db).unwrap();
    assert_eq!(
        reopened.doctor_history_entry(&run_id).unwrap().run.status,
        RunStatus::Completed
    );
    assert_eq!(
        reopened
            .doctor_history_entry(&incomplete.id)
            .unwrap()
            .run
            .status,
        RunStatus::Running
    );
    assert_eq!(reopened.doctor_source(&run_id).unwrap(), source);
    let conn = rusqlite::Connection::open(db).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 17);
}

#[test]
fn changed_files_are_excluded_but_remain_in_issue_details() {
    let temp = tempdir().unwrap();
    let store = SqliteScanStore::open(&temp.path().join("db")).unwrap();
    let mut v = view(
        &store,
        temp.path(),
        &[song(temp.path(), 0), song(temp.path(), 1)],
    );
    let changed = *v.files.keys().next().unwrap();
    let issue = music_folder_core::doctor::DoctorIssue {
        code: "file_changed".into(),
        severity: music_folder_core::doctor::Severity::Critical,
        category: "read".into(),
        file_ids: vec![changed],
        evidence: vec!["changed".into()],
        comparison: None,
        rule_version: 1,
    };
    v.issues.push(issue);
    let files = v.files.into_values().collect();
    let v = DoctorView::new(v.history, v.source, files, v.issues);
    assert_eq!(v.albums.iter().map(|a| a.row.tracks).sum::<usize>(), 1);
    assert_eq!(
        v.issue_detail(v.issues.len() - 1, 0).unwrap().files.items[0].id,
        changed
    );
}

fn png() -> Vec<u8> {
    let img = image::DynamicImage::new_rgb8(600, 400);
    let mut output = Cursor::new(Vec::new());
    img.write_to(&mut output, image::ImageFormat::Png).unwrap();
    output.into_inner()
}
fn add_picture(path: &Path, front: bool) {
    use lofty::{
        config::WriteOptions,
        picture::{MimeType, Picture, PictureType},
        prelude::{TagExt, TaggedFileExt},
        probe::Probe,
    };
    let mut tagged = Probe::open(path).unwrap().read().unwrap();
    tagged
        .primary_tag_mut()
        .unwrap()
        .push_picture(Picture::new_unchecked(
            if front {
                PictureType::CoverFront
            } else {
                PictureType::CoverBack
            },
            Some(MimeType::Png),
            None,
            png(),
        ));
    tagged
        .primary_tag()
        .unwrap()
        .save_to_path(path, WriteOptions::default())
        .unwrap();
}

#[test]
fn artwork_prefers_album_front_cover_then_folder_and_keeps_inputs_unchanged() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("音楽");
    fs::create_dir(&root).unwrap();
    let a = root.join("a.mp3");
    let b = root.join("b.mp3");
    for path in [&a, &b] {
        fs::copy(support::fixture("mp3/japanese.mp3"), path).unwrap();
    }
    add_picture(&a, false);
    add_picture(&b, true);
    let cover = root.join("CoVeR.PNG");
    fs::write(&cover, png()).unwrap();
    let files: Vec<_> = [&a, &b]
        .into_iter()
        .map(|path| ScannedFile {
            id: Uuid::new_v4(),
            path: path.clone(),
            fingerprint: DoctorFileSystem.fingerprint(path).unwrap(),
            metadata: Some(LoftyMetadataReader.read(path).unwrap()),
            kind: FileKind::Music,
        })
        .collect();
    let before: Vec<_> = [&a, &b, &cover]
        .into_iter()
        .map(|p| {
            (
                fs::read(p).unwrap(),
                fs::metadata(p).unwrap().modified().unwrap(),
            )
        })
        .collect();
    let store = SqliteScanStore::open(&temp.path().join("db")).unwrap();
    let v = view(&store, &root, &files);
    let service = ArtworkService::default();
    let result = service.load(&v, &v.albums[0]);
    assert_eq!(result.origin.as_deref(), Some("embedded"));
    assert_eq!(result.source.unwrap().display, b.to_string_lossy());
    assert!(result
        .data_url
        .unwrap()
        .starts_with("data:image/png;base64,"));
    let warm = service.load(&v, &v.albums[0]);
    assert_eq!(warm.origin.as_deref(), Some("embedded"));
    for (p, expected) in [&a, &b, &cover].into_iter().zip(before) {
        assert_eq!(fs::read(p).unwrap(), expected.0);
        assert_eq!(fs::metadata(p).unwrap().modified().unwrap(), expected.1);
    }
    // Stale embedded pictures must not be shown even if the thumbnail was cached.
    fs::write(&a, b"changed").unwrap();
    fs::write(&b, b"changed").unwrap();
    let result = service.load(&v, &v.albums[0]);
    assert_eq!(result.origin.as_deref(), Some("folder"));
    assert_eq!(result.source.unwrap().display, cover.to_string_lossy());
    fs::remove_file(cover).unwrap();
    assert!(service.load(&v, &v.albums[0]).data_url.is_none());
}

#[test]
fn folder_artwork_has_fixed_priority_and_does_not_read_outside_root() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("音楽");
    let disc = root.join("CD1");
    fs::create_dir_all(&disc).unwrap();
    let a = disc.join("a.mp3");
    fs::copy(support::fixture("mp3/japanese.mp3"), &a).unwrap();
    let file = ScannedFile {
        id: Uuid::new_v4(),
        path: a.clone(),
        fingerprint: DoctorFileSystem.fingerprint(&a).unwrap(),
        metadata: Some(LoftyMetadataReader.read(&a).unwrap()),
        kind: FileKind::Music,
    };
    let store = SqliteScanStore::open(&temp.path().join("db")).unwrap();
    let v = view(&store, &root, &[file]);
    let service = ArtworkService::default();
    fs::write(root.join("cover.jpg"), b"broken").unwrap();
    fs::write(root.join("folder.png"), png()).unwrap();
    fs::write(disc.join("cover.png"), png()).unwrap();
    assert_eq!(
        service.load(&v, &v.albums[0]).source.unwrap().display,
        root.join("folder.png").to_string_lossy()
    );
    fs::remove_file(root.join("folder.png")).unwrap();
    assert_eq!(
        service.load(&v, &v.albums[0]).source.unwrap().display,
        disc.join("cover.png").to_string_lossy()
    );
    let mut narrow = v;
    narrow.source = disc.clone();
    assert_eq!(
        service
            .load(&narrow, &narrow.albums[0])
            .source
            .unwrap()
            .display,
        disc.join("cover.png").to_string_lossy()
    );
    fs::remove_file(disc.join("cover.png")).unwrap();
    assert!(service.load(&narrow, &narrow.albums[0]).data_url.is_none());
}

#[test]
fn ten_thousand_tracks_have_bounded_pages_and_one_thousand_albums() {
    let started = Instant::now();
    let temp = tempdir().unwrap();
    let store = SqliteScanStore::open(&temp.path().join("db")).unwrap();
    let files: Vec<_> = (0..10_000).map(|n| song(temp.path(), n)).collect();
    let v = view(&store, temp.path(), &files);
    assert_eq!(v.albums.len(), 1000);
    let first = v.album_page("", 0);
    assert_eq!(first.items.len(), 48);
    assert_eq!(first.next_cursor, Some(48));
    let detail = v.album_detail(&first.items[0].id, 0, 0).unwrap();
    assert_eq!(detail.files.items.len(), 10);
    let result = serde_json::to_vec(&first).unwrap();
    assert!(result.len() < 100_000);
    println!("doctor_desktop: 10000 tracks/1000 albums; database+projection+pages={:?}; album page bytes={}", started.elapsed(), result.len());
    #[cfg(windows)]
    {
        #[repr(C)]
        #[derive(Default)]
        struct MemoryCounters {
            cb: u32,
            page_faults: u32,
            peak_working_set: usize,
            working_set: usize,
            quota_peak_paged: usize,
            quota_paged: usize,
            quota_peak_nonpaged: usize,
            quota_nonpaged: usize,
            pagefile: usize,
            peak_pagefile: usize,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
        }
        #[link(name = "psapi")]
        extern "system" {
            fn GetProcessMemoryInfo(
                handle: *mut std::ffi::c_void,
                counters: *mut MemoryCounters,
                size: u32,
            ) -> i32;
        }
        let mut counters = MemoryCounters {
            cb: std::mem::size_of::<MemoryCounters>() as u32,
            ..Default::default()
        };
        // SAFETY: the pseudo-handle belongs to this process; the output buffer
        // has the Win32 PROCESS_MEMORY_COUNTERS layout and remains alive.
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
        assert_ne!(ok, 0);
        println!("doctor_desktop: process working_set={} bytes; peak_working_set={} bytes (includes Rust test harness and SQLite)", counters.working_set, counters.peak_working_set);
    }
}
