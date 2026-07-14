use music_folder_core::ports::MetadataReader;
use music_folder_infra::lofty_reader::LoftyMetadataReader;
use std::path::PathBuf;

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

#[test]
fn reads_japanese_tags_from_each_supported_format() {
    let reader = LoftyMetadataReader;
    for path in [
        "flac/japanese.flac",
        "mp3/japanese.mp3",
        "m4a/japanese.m4a",
        "ogg/japanese.ogg",
    ] {
        let tags = reader
            .read(&fixture(path))
            .unwrap_or_else(|error| panic!("{path}: {error}"));
        assert_eq!(tags.artist.as_deref(), Some("日本語アーティスト"), "{path}");
        assert_eq!(tags.album.as_deref(), Some("テストアルバム"), "{path}");
        assert_eq!(tags.title.as_deref(), Some("楽曲"), "{path}");
    }
}

#[test]
fn reports_missing_artist_and_rejects_broken_input() {
    let reader = LoftyMetadataReader;
    assert_eq!(
        reader
            .read(&fixture("missing/no-artist.mp3"))
            .unwrap()
            .artist,
        None
    );
    assert!(reader.read(&fixture("broken/not-audio.mp3")).is_err());
}
