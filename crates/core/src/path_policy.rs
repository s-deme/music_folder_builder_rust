use std::path::Path;

/// A single comparison key for Windows target collision checks and persistence.
pub fn windows_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .split('\\')
        .map(|component| component.trim_end_matches([' ', '.']).to_lowercase())
        .collect::<Vec<_>>()
        .join("\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_windows_separators_case_and_trailing_dots() {
        assert_eq!(
            windows_path_key(Path::new("C:/Music/Album./SONG.MP3")),
            windows_path_key(Path::new("c:\\music\\album\\song.mp3"))
        );
    }
}
