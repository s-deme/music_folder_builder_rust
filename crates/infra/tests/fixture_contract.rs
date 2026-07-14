//! Fixture manifest contract. Audio binaries are intentionally fixed test data;
//! this test prevents the metadata matrix from silently losing a format.

#[test]
fn fixture_manifest_covers_supported_formats_and_broken_input() {
    let manifest = include_str!("fixtures/manifest.json");
    for format in ["flac", "mp3", "m4a", "ogg"] {
        assert!(manifest.contains(&format!("\"format\":\"{format}\"")));
    }
    assert!(manifest.contains("\"broken\":true"));
    assert!(manifest.contains("日本語アーティスト"));
}
