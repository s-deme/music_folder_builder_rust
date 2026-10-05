use music_folder_core::{
    doctor::*, usecases::CancellationToken, FileFingerprint, FileKind, ScannedFile, TrackMetadata,
};
use uuid::Uuid;

fn song(path: &str, artist: &str, track: u32, disc: Option<u32>) -> ScannedFile {
    ScannedFile {
        id: Uuid::new_v4(),
        path: path.into(),
        fingerprint: FileFingerprint::legacy(10, 1),
        kind: FileKind::Music,
        metadata: Some(TrackMetadata {
            artist: Some(artist.into()),
            album_artist: Some(artist.into()),
            album: Some("Album".into()),
            title: Some("Song".into()),
            track_no: Some(track),
            disc_no: disc,
            year: Some(2026),
            genre: Some("Rock".into()),
            has_artwork: Some(true),
        }),
    }
}
fn check(files: &[ScannedFile]) -> Vec<DoctorIssue> {
    diagnose(
        &files.iter().collect::<Vec<_>>(),
        &CancellationToken::default(),
    )
}
fn has(issues: &[DoctorIssue], code: &str) -> bool {
    issues.iter().any(|i| i.code == code)
}

#[test]
fn normalization_preserves_originals_and_meaningful_joiners() {
    assert_eq!(
        comparison_key(" \tＡＢＣ\n  ｶﾞ\u{200b}\u{2060}\u{feff} "),
        "abc ガ"
    );
    assert_eq!(comparison_key("Cafe\u{301}"), comparison_key("CAFÉ"));
    assert_ne!(comparison_key("a\u{200d}b"), "ab");
    assert_ne!(comparison_key("a\u{200c}b"), "ab");
    let mut a = song("a/1.mp3", " ＡＢＣ\t", 1, None);
    let mut b = song("a/2.mp3", "abc", 2, None);
    a.metadata.as_mut().unwrap().album = Some(" Ａlbum ".into());
    b.metadata.as_mut().unwrap().album = Some("album".into());
    let found = check(&[a, b]);
    assert!(has(&found, "artist_variant"));
    assert!(has(&found, "album_variant"));
    assert!(found
        .iter()
        .find(|i| i.code == "artist_variant")
        .unwrap()
        .evidence
        .contains(&" ＡＢＣ\t".into()));
}

#[test]
fn readable_missing_tags_are_distinct_from_reader_failure_and_unknown_artwork() {
    let mut a = song("a/1.mp3", "A", 0, None);
    let tags = a.metadata.as_mut().unwrap();
    tags.title = Some("\u{200b}  ".into());
    tags.album_artist = None;
    tags.has_artwork = Some(false);
    tags.year = None;
    let found = check(std::slice::from_ref(&a));
    assert!(has(&found, "missing_title"));
    assert!(has(&found, "missing_artwork"));
    assert_eq!(
        found
            .iter()
            .find(|i| i.code == "missing_disc")
            .unwrap()
            .severity,
        Severity::Info
    );
    a.metadata.as_mut().unwrap().has_artwork = None;
    assert!(!has(&check(std::slice::from_ref(&a)), "missing_artwork"));
    a.metadata = None;
    let found = check(&[a]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, "read_failed");
}

#[test]
fn album_context_separates_artists_and_editions() {
    let a = song("one/1.mp3", "A", 1, Some(1));
    let b = song("one/2.mp3", "B", 1, Some(1));
    let c = song("edition/1.mp3", "A", 1, Some(1));
    assert!(!has(&check(&[a, b, c]), "track_duplicate"));
    let mut a = song("one/1.mp3", "A", 1, Some(1));
    let mut b = song("one/2.mp3", "B", 2, Some(1));
    a.metadata.as_mut().unwrap().album_artist = Some("Various Artists".into());
    b.metadata.as_mut().unwrap().album_artist = Some("Various Artists".into());
    assert!(!has(&check(&[a, b]), "track_gap"));
}

#[test]
fn multi_disc_gaps_duplicates_and_attribute_evidence() {
    let a = song("album/CD1/1.mp3", "A", 1, Some(1));
    let mut b = song("album/CD2/1.flac", "A", 1, Some(2));
    assert!(!has(&check(&[a.clone(), b.clone()]), "track_duplicate"));
    b.metadata.as_mut().unwrap().year = Some(2025);
    b.metadata.as_mut().unwrap().genre = Some("Pop".into());
    b.metadata.as_mut().unwrap().album_artist = Some("Other spelling".into());
    let c = song("album/CD2/3.mp3", "A", 3, Some(2));
    let d = song("album/CD2/copy.mp3", "A", 3, Some(2));
    let found = check(&[a, b, c, d]);
    for code in [
        "track_gap",
        "track_duplicate",
        "year_inconsistent",
        "genre_inconsistent",
        "album_artist_inconsistent",
        "format_mixed",
    ] {
        assert!(has(&found, code), "{code}");
    }
    let found = check(&[
        song("album/1.mp3", "A", 1, None),
        song("album/2.mp3", "A", 2, Some(2)),
    ]);
    assert!(has(&found, "disc_inconsistent"));
}

#[test]
fn duplicate_hashes_and_native_identity_are_separate() {
    let mut a = song("one.mp3", "A", 1, None);
    let mut b = song("two.mp3", "A", 2, None);
    a.fingerprint.content_sha256 = Some("same".into());
    b.fingerprint.content_sha256 = Some("different".into());
    assert!(!has(&check(&[a.clone(), b.clone()]), "exact_duplicate"));
    b.fingerprint.content_sha256 = Some("same".into());
    assert!(has(&check(&[a.clone(), b.clone()]), "exact_duplicate"));
    a.fingerprint.file_identity = Some("object".into());
    b.fingerprint.file_identity = Some("object".into());
    assert!(has(&check(&[a, b]), "same_file_paths"));
}

#[test]
fn huge_track_number_is_a_compact_range_and_cancel_stops_rules() {
    let a = song("a/1.mp3", "A", u32::MAX, Some(1));
    assert!(
        check(std::slice::from_ref(&a))
            .iter()
            .find(|i| i.code == "track_gap")
            .unwrap()
            .evidence[0]
            .len()
            < 200
    );
    let cancel = CancellationToken::default();
    cancel.cancel();
    assert!(diagnose(&[&a], &cancel).is_empty());
}

#[test]
#[ignore = "manual performance measurement"]
fn ten_thousand_tracks() {
    let files: Vec<_> = (0..10_000)
        .map(|n| {
            song(
                &format!("album{}/{}.mp3", n / 10, n),
                "Artist",
                n % 10 + 1,
                Some(1),
            )
        })
        .collect();
    let started = std::time::Instant::now();
    let issues = check(&files);
    eprintln!(
        "doctor rules: {} tracks, {} issues, {:?}; debug build, synthetic tags, no I/O",
        files.len(),
        issues.len(),
        started.elapsed()
    );
    assert!(issues.is_empty());
}
