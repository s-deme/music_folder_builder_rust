#![cfg(windows)]

use music_folder_core::{ports::FileSystem, sanitize_component};
use music_folder_infra::windows_fs::LocalFileSystem;
use std::{fs, os::windows::fs::symlink_dir};
use tempfile::tempdir;

#[test]
fn japanese_long_paths_are_enumerated_and_reserved_names_are_sanitized() {
    let temp = tempdir().unwrap();
    let mut library = temp.path().join("日本語音楽");
    for index in 0..6 {
        library = library.join(format!("長いディレクトリ{index:02}"));
    }
    fs::create_dir_all(&library).unwrap();
    fs::write(library.join("楽曲.mp3"), b"test").unwrap();
    let mut found = Vec::new();
    LocalFileSystem
        .enumerate(temp.path(), false, &mut |item| {
            found.push(item.unwrap());
            true
        })
        .unwrap();
    assert_eq!(found.len(), 1);
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
    symlink_dir(&outside, library.join("linked")).expect("Windows CI must permit symlink creation");
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
