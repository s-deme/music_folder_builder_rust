#[cfg(windows)]
use music_folder_core::ports::DeleteExpectation;
use music_folder_core::{
    ports::{FileMutator, FileSystem},
    MutationStrategy,
};
use music_folder_infra::windows_fs::LocalFileSystem;
use std::fs;
use tempfile::tempdir;

#[test]
fn publish_race_never_overwrites_existing_target_or_deletes_source() {
    let temporary_directory = tempdir().unwrap();
    let source = temporary_directory.path().join("source.bin");
    let target = temporary_directory.path().join("target.bin");
    fs::write(&source, b"source bytes").unwrap();

    let files = LocalFileSystem;
    let staging = files.temporary_path(&target, "race").unwrap();
    let staged = files.stage_copy_exclusive(&source, &staging).unwrap();
    assert_eq!(staged.size_bytes, b"source bytes".len() as u64);

    // Simulates another process winning after preflight but before publish.
    fs::write(&target, b"winner bytes").unwrap();
    assert_eq!(
        files.publish_no_replace(&staging, &target).unwrap_err(),
        "target_already_exists"
    );
    assert_eq!(fs::read(&target).unwrap(), b"winner bytes");
    assert_eq!(fs::read(&source).unwrap(), b"source bytes");
    files.remove_staged_file(&staging).unwrap();
}

#[test]
fn legacy_copy_adapter_is_also_no_replace() {
    let temporary_directory = tempdir().unwrap();
    let source = temporary_directory.path().join("source.bin");
    let target = temporary_directory.path().join("target.bin");
    fs::write(&source, b"new").unwrap();
    fs::write(&target, b"existing").unwrap();

    let error = LocalFileSystem.copy_file(&source, &target).unwrap_err();
    assert_eq!(error, "target_already_exists");
    assert_eq!(fs::read(&target).unwrap(), b"existing");
    assert_eq!(fs::read(&source).unwrap(), b"new");
}

#[test]
fn nonexistent_scan_root_is_a_failed_scan_precondition() {
    let temporary_directory = tempdir().unwrap();
    let missing = temporary_directory.path().join("missing");
    let mut visitor = |_item| true;
    assert_eq!(
        LocalFileSystem
            .enumerate(&missing, false, &mut visitor)
            .unwrap_err(),
        "scan_root_not_found"
    );
}

#[cfg(windows)]
#[test]
fn same_volume_atomic_move_preserves_native_identity_and_never_stages() {
    let temporary_directory = tempdir().unwrap();
    let source = temporary_directory.path().join("source.bin");
    let target = temporary_directory.path().join("nested").join("target.bin");
    fs::write(&source, b"atomic bytes").unwrap();
    let files = LocalFileSystem;
    let before = files.mutation_fingerprint(&source).unwrap();
    assert_eq!(
        files.select_move_strategy(&source, &target).unwrap(),
        MutationStrategy::AtomicNoReplaceRename
    );
    let evidence = files
        .atomic_move_no_replace_if_matches(
            &source,
            &target,
            &DeleteExpectation {
                size_bytes: before.size_bytes,
                content_sha256: before.content_sha256.clone().unwrap(),
                file_identity: before.file_identity.clone(),
            },
        )
        .unwrap();
    assert!(evidence.temporary.as_os_str().is_empty());
    assert!(!source.exists());
    assert_eq!(fs::read(&target).unwrap(), b"atomic bytes");
    let after = files.mutation_fingerprint(&target).unwrap();
    assert_eq!(before.file_identity, after.file_identity);
    assert_eq!(evidence.file_identity, after.file_identity);
}

#[cfg(windows)]
#[test]
fn atomic_move_target_collision_retains_both_files_without_overwrite() {
    let temporary_directory = tempdir().unwrap();
    let source = temporary_directory.path().join("source.bin");
    let target = temporary_directory.path().join("target.bin");
    fs::write(&source, b"source bytes").unwrap();
    fs::write(&target, b"winner bytes").unwrap();
    let files = LocalFileSystem;
    let fingerprint = files.mutation_fingerprint(&source).unwrap();
    let error = files
        .atomic_move_no_replace_if_matches(
            &source,
            &target,
            &DeleteExpectation {
                size_bytes: fingerprint.size_bytes,
                content_sha256: fingerprint.content_sha256.unwrap(),
                file_identity: fingerprint.file_identity,
            },
        )
        .unwrap_err();
    assert_eq!(error, "target_already_exists");
    assert_eq!(fs::read(&source).unwrap(), b"source bytes");
    assert_eq!(fs::read(&target).unwrap(), b"winner bytes");
}

#[cfg(windows)]
#[test]
fn atomic_move_identity_mismatch_retains_source_and_target_absence() {
    let temporary_directory = tempdir().unwrap();
    let source = temporary_directory.path().join("source.bin");
    let target = temporary_directory.path().join("target.bin");
    fs::write(&source, b"source bytes").unwrap();
    let files = LocalFileSystem;
    let fingerprint = files.mutation_fingerprint(&source).unwrap();
    let error = files
        .atomic_move_no_replace_if_matches(
            &source,
            &target,
            &DeleteExpectation {
                size_bytes: fingerprint.size_bytes,
                content_sha256: fingerprint.content_sha256.unwrap(),
                file_identity: Some("windows-v1:deadbeef:0000000000000000".into()),
            },
        )
        .unwrap_err();
    assert_eq!(error, "atomic_move_conditional_delete_identity_mismatch");
    assert!(source.exists());
    assert!(!target.exists());
}

#[cfg(not(windows))]
#[test]
fn portable_product_path_explicitly_keeps_the_copy_protocol() {
    let temporary_directory = tempdir().unwrap();
    let source = temporary_directory.path().join("source.bin");
    let target = temporary_directory.path().join("target.bin");
    fs::write(&source, b"source bytes").unwrap();
    assert_eq!(
        LocalFileSystem
            .select_move_strategy(&source, &target)
            .unwrap(),
        MutationStrategy::CopyPublishDelete
    );
}

#[cfg(unix)]
#[test]
fn target_parent_symlink_is_rejected() {
    use std::os::unix::fs::symlink;

    let temporary_directory = tempdir().unwrap();
    let outside = temporary_directory.path().join("outside");
    let linked = temporary_directory.path().join("linked");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, &linked).unwrap();
    let target = linked.join("song.mp3");
    assert!(LocalFileSystem
        .ensure_no_reparse_points(&target)
        .unwrap_err()
        .starts_with("reparse_point_forbidden:"));
}
