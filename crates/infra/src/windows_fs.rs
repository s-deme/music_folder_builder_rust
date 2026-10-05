use music_folder_core::{
    ports::{DeleteExpectation, FileMutator, FileSystem},
    FileFingerprint, MutationStrategy, StagedFile,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use uuid::Uuid;
use walkdir::WalkDir;

pub struct LocalFileSystem;
impl FileSystem for LocalFileSystem {
    fn enumerate(
        &self,
        root: &Path,
        follow_links: bool,
        visitor: &mut dyn FnMut(Result<PathBuf, String>) -> bool,
    ) -> Result<(), String> {
        enumerate_paths(root, follow_links, false, visitor)
    }
    fn fingerprint(&self, path: &Path) -> Result<FileFingerprint, String> {
        fingerprint_file(path)
    }
}

impl FileMutator for LocalFileSystem {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn same_volume(&self, source: &Path, target: &Path) -> Result<bool, String> {
        let source_volume = native_volume_identity(source)?;
        let target_volume = nearest_existing_volume_identity(
            target
                .parent()
                .ok_or_else(|| "target_parent_missing".to_string())?,
        )?;
        Ok(source_volume == target_volume)
    }
    fn move_file(&self, source: &Path, target: &Path) -> Result<(), String> {
        let fingerprint = self.mutation_fingerprint(source)?;
        let expected = DeleteExpectation {
            size_bytes: fingerprint.size_bytes,
            content_sha256: fingerprint
                .content_sha256
                .ok_or_else(|| "source_hash_missing".to_string())?,
            file_identity: fingerprint.file_identity,
        };
        let operation_id = Uuid::new_v4().to_string();
        let temporary = self.temporary_path(target, &operation_id)?;
        self.stage_copy_exclusive(source, &temporary)?;
        if let Err(error) = self.publish_no_replace(&temporary, target) {
            let _ = self.remove_staged_file(&temporary);
            return Err(error);
        }
        self.delete_file_if_matches(source, &expected)
    }
    fn copy_file(&self, source: &Path, target: &Path) -> Result<(), String> {
        let operation_id = Uuid::new_v4().to_string();
        let temporary = self.temporary_path(target, &operation_id)?;
        self.stage_copy_exclusive(source, &temporary)?;
        if let Err(error) = self.publish_no_replace(&temporary, target) {
            let _ = self.remove_staged_file(&temporary);
            return Err(error);
        }
        Ok(())
    }
    fn size(&self, path: &Path) -> Result<u64, String> {
        fs::metadata(path)
            .map(|m| m.len())
            .map_err(|e| e.to_string())
    }
    fn delete_file(&self, path: &Path) -> Result<(), String> {
        fs::remove_file(path).map_err(|e| e.to_string())
    }

    fn select_move_strategy(
        &self,
        source: &Path,
        target: &Path,
    ) -> Result<MutationStrategy, String> {
        #[cfg(windows)]
        {
            self.same_volume(source, target).map(|same| {
                if same {
                    MutationStrategy::AtomicNoReplaceRename
                } else {
                    MutationStrategy::CopyPublishDelete
                }
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (source, target);
            Ok(MutationStrategy::CopyPublishDelete)
        }
    }

    fn atomic_move_no_replace_if_matches(
        &self,
        source: &Path,
        target: &Path,
        expected: &DeleteExpectation,
    ) -> Result<StagedFile, String> {
        atomic_handle_bound_move(source, target, expected)
    }

    fn delete_file_if_matches(
        &self,
        path: &Path,
        expected: &DeleteExpectation,
    ) -> Result<(), String> {
        identity_bound_delete(path, expected)
    }

    fn content_sha256(&self, path: &Path) -> Result<String, String> {
        hash_file(path)
    }

    fn mutation_fingerprint(&self, path: &Path) -> Result<FileFingerprint, String> {
        fingerprint_file(path)
    }

    fn ensure_no_reparse_points(&self, path: &Path) -> Result<(), String> {
        for ancestor in path.ancestors() {
            match fs::symlink_metadata(ancestor) {
                Ok(metadata) if metadata_is_reparse(&metadata) => {
                    return Err(format!("reparse_point_forbidden:{}", ancestor.display()));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }

    fn stage_copy_exclusive(&self, source: &Path, temporary: &Path) -> Result<StagedFile, String> {
        self.ensure_no_reparse_points(source)?;
        let parent = temporary
            .parent()
            .ok_or_else(|| "target_parent_missing".to_string())?;
        create_directories_no_reparse(parent)?;
        self.ensure_no_reparse_points(parent)?;

        let mut source_file = File::open(source).map_err(|error| error.to_string())?;
        let before = source_file.metadata().map_err(|error| error.to_string())?;
        let before_identity = native_file_identity(source, &before);
        if !before.is_file() || metadata_is_reparse(&before) {
            return Err("source_not_regular_file".into());
        }
        let mut temporary_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temporary)
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::AlreadyExists => "staging_path_already_exists".into(),
                _ => error.to_string(),
            })?;

        let copied = (|| -> Result<(u64, String), String> {
            let mut digest = Sha256::new();
            let mut size = 0_u64;
            let mut buffer = [0_u8; 128 * 1024];
            loop {
                let count = source_file
                    .read(&mut buffer)
                    .map_err(|error| error.to_string())?;
                if count == 0 {
                    break;
                }
                temporary_file
                    .write_all(&buffer[..count])
                    .map_err(|error| error.to_string())?;
                digest.update(&buffer[..count]);
                size += count as u64;
            }
            temporary_file
                .flush()
                .and_then(|_| temporary_file.sync_all())
                .map_err(|error| error.to_string())?;
            Ok((size, format!("{:x}", digest.finalize())))
        })();

        let (size_bytes, content_sha256) = match copied {
            Ok(value) => value,
            Err(error) => {
                drop(temporary_file);
                let _ = fs::remove_file(temporary);
                return Err(error);
            }
        };
        let after = source_file.metadata().map_err(|error| error.to_string())?;
        if before.len() != size_bytes
            || before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
            || before_identity.is_some() && native_file_identity(source, &after) != before_identity
        {
            drop(temporary_file);
            let _ = fs::remove_file(temporary);
            return Err("source_changed_during_copy".into());
        }
        Ok(StagedFile {
            temporary: temporary.to_path_buf(),
            size_bytes,
            content_sha256,
            file_identity: native_file_identity_from_file(
                &temporary_file,
                temporary,
                &temporary_file
                    .metadata()
                    .map_err(|error| error.to_string())?,
            ),
        })
    }

    fn publish_no_replace(&self, temporary: &Path, target: &Path) -> Result<(), String> {
        self.ensure_no_reparse_points(temporary)?;
        if let Some(parent) = target.parent() {
            self.ensure_no_reparse_points(parent)?;
        }
        let staged = self.mutation_fingerprint(temporary)?;
        let staged_expected = DeleteExpectation {
            size_bytes: staged.size_bytes,
            content_sha256: staged
                .content_sha256
                .ok_or_else(|| "staged_hash_missing".to_string())?,
            file_identity: staged.file_identity,
        };
        fs::hard_link(temporary, target).map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => "target_already_exists".into(),
            _ => format!("atomic_publish_failed:{error}"),
        })?;
        // FlushFileBuffers requires a handle opened for writing on Windows.
        // Opening without truncate preserves the exclusively staged bytes while
        // giving the published hard-link a handle that can satisfy durability.
        let published = OpenOptions::new()
            .write(true)
            .open(target)
            .and_then(|file| file.sync_all());
        if let Err(error) = published {
            // The target was published and must never be silently removed: the
            // journal will make recovery decide what to do next.
            return Err(format!("published_flush_failed:{error}"));
        }
        self.delete_file_if_matches(temporary, &staged_expected)
            .map_err(|error| format!("published_staging_cleanup_failed:{error}"))?;
        if let Some(parent) = target.parent() {
            sync_parent_directory(parent)
                .map_err(|error| format!("published_directory_flush_failed:{error}"))?;
        }
        Ok(())
    }
}

/// Durably records a directory-entry change where the platform exposes that
/// operation. Windows filesystems may reject directory flushing even with a
/// backup-semantics handle; the platform helper treats only those documented
/// unsupported/access-denied cases as a best-effort success.
pub(crate) fn sync_directory(path: &Path) -> Result<(), String> {
    sync_parent_directory(path)
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), String> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
fn sync_parent_directory(parent: &Path) -> Result<(), String> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr};

    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const ERROR_INVALID_FUNCTION: u32 = 1;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const ERROR_NOT_SUPPORTED: u32 = 50;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *const c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn FlushFileBuffers(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    let mut wide = parent.as_os_str().encode_wide().collect::<Vec<_>>();
    wide.push(0);
    // SAFETY: all pointers are either null or reference live buffers for the
    // duration of the Win32 calls.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if handle as isize == INVALID_HANDLE_VALUE {
        return Err(format!(
            "directory_open_failed:{}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `handle` is valid and is closed exactly once below.
    let flushed = unsafe { FlushFileBuffers(handle) };
    let error_code = if flushed == 0 {
        // SAFETY: GetLastError has no preconditions and is read before another
        // Win32 call can overwrite it.
        unsafe { GetLastError() }
    } else {
        0
    };
    // SAFETY: `handle` is valid and owned by this function.
    unsafe { CloseHandle(handle) };
    if flushed != 0
        || matches!(
            error_code,
            ERROR_INVALID_FUNCTION | ERROR_ACCESS_DENIED | ERROR_NOT_SUPPORTED
        )
    {
        Ok(())
    } else {
        Err(format!("directory_flush_error:{error_code}"))
    }
}

#[cfg(not(any(windows, unix)))]
fn sync_parent_directory(_parent: &Path) -> Result<(), String> {
    Ok(())
}

fn validate_delete_candidate(
    file: &mut File,
    path: &Path,
    expected: &DeleteExpectation,
) -> Result<(), String> {
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata_is_reparse(&metadata) {
        return Err("conditional_delete_not_regular_file".into());
    }
    if metadata.len() != expected.size_bytes {
        return Err("conditional_delete_size_mismatch".into());
    }
    let observed_hash = hash_open_file(file)?;
    if observed_hash != expected.content_sha256 {
        return Err("conditional_delete_hash_mismatch".into());
    }
    if let Some(expected_identity) = expected.file_identity.as_deref() {
        let observed_identity = native_file_identity_from_file(file, path, &metadata)
            .ok_or_else(|| "conditional_delete_identity_unavailable".to_string())?;
        if observed_identity != expected_identity {
            return Err("conditional_delete_identity_mismatch".into());
        }
    }
    Ok(())
}

#[cfg(windows)]
fn identity_bound_delete(path: &Path, expected: &DeleteExpectation) -> Result<(), String> {
    use std::{
        ffi::c_void,
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    };

    const GENERIC_READ: u32 = 0x8000_0000;
    const DELETE: u32 = 0x0001_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_DISPOSITION_INFO_CLASS: i32 = 4;

    #[repr(C)]
    struct FileDispositionInfo {
        delete_file: u8,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn SetFileInformationByHandle(
            file: *mut c_void,
            information_class: i32,
            information: *const c_void,
            buffer_size: u32,
        ) -> i32;
    }

    let mut file = OpenOptions::new()
        .read(true)
        .access_mode(GENERIC_READ | DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("conditional_delete_open_failed:{error}"))?;
    validate_delete_candidate(&mut file, path, expected)?;
    let disposition = FileDispositionInfo { delete_file: 1 };
    // SAFETY: `file` owns a valid handle for the duration of the call and the
    // disposition buffer has the exact Win32 layout and byte length.
    let deleted = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FILE_DISPOSITION_INFO_CLASS,
            (&disposition as *const FileDispositionInfo).cast(),
            std::mem::size_of::<FileDispositionInfo>() as u32,
        )
    };
    if deleted == 0 {
        return Err(format!(
            "conditional_delete_failed:{}",
            std::io::Error::last_os_error()
        ));
    }
    drop(file);
    if let Some(parent) = path.parent() {
        sync_parent_directory(parent)
            .map_err(|error| format!("conditional_delete_directory_flush_failed:{error}"))?;
    }
    Ok(())
}

fn nearest_existing_volume_identity(path: &Path) -> Result<String, String> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata_is_reparse(&metadata) => {
                return Err(format!("reparse_point_forbidden:{}", ancestor.display()));
            }
            Ok(_) => return native_volume_identity(ancestor),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("volume_identity_ancestor_missing".into())
}

#[cfg(windows)]
fn native_volume_identity(path: &Path) -> Result<String, String> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use std::{ffi::c_void, mem};

    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            file: *mut c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata_is_reparse(&metadata) {
        return Err("volume_identity_reparse_forbidden".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .access_mode(GENERIC_READ)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|error| format!("volume_identity_open_failed:{error}"))?;
    let mut information: ByHandleFileInformation = unsafe { mem::zeroed() };
    // SAFETY: file handle and output structure remain valid for the call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) } == 0 {
        return Err(format!(
            "volume_identity_query_failed:{}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(format!(
        "windows-volume-v1:{:08x}",
        information.volume_serial_number
    ))
}

#[cfg(unix)]
fn native_volume_identity(path: &Path) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata_is_reparse(&metadata) {
        return Err("volume_identity_reparse_forbidden".into());
    }
    Ok(format!("unix-volume-v1:{:x}", metadata.dev()))
}

#[cfg(not(any(windows, unix)))]
fn native_volume_identity(_path: &Path) -> Result<String, String> {
    Err("native_volume_identity_not_supported".into())
}

#[cfg(windows)]
fn atomic_handle_bound_move(
    source: &Path,
    target: &Path,
    expected: &DeleteExpectation,
) -> Result<StagedFile, String> {
    use std::{
        ffi::c_void,
        os::windows::{
            ffi::OsStrExt,
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle},
        },
        ptr,
    };

    const GENERIC_READ: u32 = 0x8000_0000;
    const DELETE: u32 = 0x0001_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const OPEN_EXISTING: u32 = 3;
    const FILE_RENAME_INFO_CLASS: i32 = 3;
    const FILE_RENAME_INFO_EX_CLASS: i32 = 22;
    const ERROR_INVALID_FUNCTION: i32 = 1;
    const ERROR_NOT_SUPPORTED: i32 = 50;
    const ERROR_INVALID_PARAMETER: i32 = 87;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[repr(C)]
    struct FileRenameInfo {
        flags: u32,
        root_directory: *mut c_void,
        file_name_length: u32,
        file_name: [u16; 1],
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoStatusBlock {
        status_or_pointer: usize,
        information: usize,
    }

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *const c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn GetFileInformationByHandle(
            file: *mut c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
        fn SetFileInformationByHandle(
            file: *mut c_void,
            information_class: i32,
            information: *const c_void,
            buffer_size: u32,
        ) -> i32;
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtSetInformationFile(
            file: *mut c_void,
            io_status_block: *mut IoStatusBlock,
            information: *const c_void,
            length: u32,
            information_class: i32,
        ) -> i32;
        fn RtlNtStatusToDosError(status: i32) -> u32;
    }

    for path in [source, target] {
        for ancestor in path.ancestors() {
            match fs::symlink_metadata(ancestor) {
                Ok(metadata) if metadata_is_reparse(&metadata) => {
                    return Err(format!("reparse_point_forbidden:{}", ancestor.display()));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    let parent = target
        .parent()
        .ok_or_else(|| "target_parent_missing".to_string())?;
    create_directories_no_reparse(parent)?;
    if fs::symlink_metadata(target).is_ok() {
        return Err("target_already_exists".into());
    }

    let mut source_file = OpenOptions::new()
        .read(true)
        .access_mode(GENERIC_READ | DELETE)
        // Excluding FILE_SHARE_WRITE prevents byte changes between validation
        // and the rename while the source handle is held.
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(source)
        .map_err(|error| format!("atomic_move_source_open_failed:{error}"))?;
    validate_delete_candidate(&mut source_file, source, expected)
        .map_err(|error| format!("atomic_move_{error}"))?;
    let source_metadata = source_file
        .metadata()
        .map_err(|error| format!("atomic_move_source_metadata_failed:{error}"))?;
    let source_identity = native_file_identity_from_file(&source_file, source, &source_metadata)
        .ok_or_else(|| "atomic_move_identity_unavailable".to_string())?;
    if expected.file_identity.as_deref() != Some(source_identity.as_str()) {
        return Err("atomic_move_identity_mismatch".into());
    }

    let mut wide_parent = parent.as_os_str().encode_wide().collect::<Vec<_>>();
    wide_parent.push(0);
    // SAFETY: the string buffer remains live throughout CreateFileW and all
    // other pointer arguments are null by contract.
    let raw_parent = unsafe {
        CreateFileW(
            wide_parent.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if raw_parent as isize == INVALID_HANDLE_VALUE {
        return Err(format!(
            "atomic_move_parent_open_failed:{}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: CreateFileW returned an owned valid handle, transferred exactly
    // once to File for RAII closing.
    let parent_file = unsafe { File::from_raw_handle(raw_parent.cast()) };
    let mut source_info = ByHandleFileInformation::default();
    let mut parent_info = ByHandleFileInformation::default();
    // SAFETY: both Files own valid live handles and structures are writable.
    if unsafe { GetFileInformationByHandle(source_file.as_raw_handle().cast(), &mut source_info) }
        == 0
        || unsafe {
            GetFileInformationByHandle(parent_file.as_raw_handle().cast(), &mut parent_info)
        } == 0
    {
        return Err(format!(
            "atomic_move_volume_identity_failed:{}",
            std::io::Error::last_os_error()
        ));
    }
    if parent_info.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("atomic_move_parent_reparse_forbidden".into());
    }
    if source_info.volume_serial_number != parent_info.volume_serial_number {
        return Err("atomic_move_cross_volume".into());
    }

    let leaf = target
        .file_name()
        .ok_or_else(|| "target_name_missing".to_string())?;
    let wide_leaf = leaf.encode_wide().collect::<Vec<_>>();
    if wide_leaf.is_empty() || wide_leaf.contains(&0) {
        return Err("target_name_invalid".into());
    }
    let name_bytes = wide_leaf
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .ok_or_else(|| "target_name_too_long".to_string())?;
    let file_name_offset = std::mem::offset_of!(FileRenameInfo, file_name);
    let total_bytes = file_name_offset
        .checked_add(name_bytes)
        .ok_or_else(|| "target_name_too_long".to_string())?;
    let words = total_bytes.div_ceil(std::mem::size_of::<usize>());
    let mut allocation = vec![0_usize; words];
    let information = allocation.as_mut_ptr().cast::<FileRenameInfo>();
    // SAFETY: allocation has pointer alignment and enough bytes for the header
    // plus the non-NUL-terminated UTF-16 leaf name.
    unsafe {
        (*information).flags = 0;
        (*information).root_directory = parent_file.as_raw_handle().cast();
        (*information).file_name_length =
            u32::try_from(name_bytes).map_err(|_| "target_name_too_long".to_string())?;
        ptr::copy_nonoverlapping(
            wide_leaf.as_ptr(),
            allocation
                .as_mut_ptr()
                .cast::<u8>()
                .add(file_name_offset)
                .cast::<u16>(),
            wide_leaf.len(),
        );
    }

    let call = |information_class| {
        // SAFETY: source handle has DELETE access; the aligned variable-size
        // FILE_RENAME_INFO buffer and parent handle remain valid for the call.
        unsafe {
            SetFileInformationByHandle(
                source_file.as_raw_handle().cast(),
                information_class,
                information.cast(),
                u32::try_from(total_bytes).unwrap_or(u32::MAX),
            )
        }
    };
    let mut renamed = call(FILE_RENAME_INFO_EX_CLASS);
    let mut last_error = None;
    if renamed == 0 {
        let error = std::io::Error::last_os_error();
        last_error = error.raw_os_error();
        if matches!(
            error.raw_os_error(),
            Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_INVALID_PARAMETER)
        ) {
            renamed = call(FILE_RENAME_INFO_CLASS);
            if renamed == 0 {
                last_error = std::io::Error::last_os_error().raw_os_error();
            }
        }
    }
    if renamed == 0
        && matches!(
            last_error,
            Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_INVALID_PARAMETER)
        )
    {
        // SetFileInformationByHandle support for a non-NULL RootDirectory is
        // filesystem/version dependent. NtSetInformationFile is the user-mode
        // native equivalent whose FileRenameInformation contract accepts the
        // same parent-relative structure while retaining the bound handle.
        let mut io_status = IoStatusBlock::default();
        // SAFETY: the source/parent handles and aligned information buffer are
        // live. FileRenameInformation (10) requires DELETE on source, which the
        // source handle has. A nonnegative NTSTATUS indicates success.
        let status = unsafe {
            NtSetInformationFile(
                source_file.as_raw_handle().cast(),
                &mut io_status,
                information.cast(),
                u32::try_from(total_bytes).unwrap_or(u32::MAX),
                10,
            )
        };
        if status >= 0 {
            renamed = 1;
        } else {
            // SAFETY: converting an NTSTATUS to a Win32 error has no additional
            // pointer or lifetime preconditions.
            last_error = Some(unsafe { RtlNtStatusToDosError(status) } as i32);
        }
    }
    if renamed == 0 {
        if fs::symlink_metadata(target).is_ok() {
            return Err("target_already_exists".into());
        }
        let error = last_error
            .map(std::io::Error::from_raw_os_error)
            .unwrap_or_else(std::io::Error::last_os_error);
        return Err(format!("atomic_move_rename_failed:{error}"));
    }
    sync_parent_directory(parent)
        .map_err(|error| format!("renamed_directory_flush_failed:{error}"))?;
    Ok(StagedFile {
        temporary: PathBuf::new(),
        size_bytes: expected.size_bytes,
        content_sha256: expected.content_sha256.clone(),
        file_identity: Some(source_identity),
    })
}

#[cfg(not(windows))]
fn atomic_handle_bound_move(
    _source: &Path,
    _target: &Path,
    _expected: &DeleteExpectation,
) -> Result<StagedFile, String> {
    Err("atomic_handle_bound_move_not_supported".into())
}

/// Portable fallback: atomically move the directory entry to an
/// operation-private quarantine name before validation. A raced-in replacement
/// is restored (or retained in quarantine when the original name was occupied)
/// and is never unlinked. Windows uses the stronger handle disposition above.
#[cfg(not(windows))]
fn identity_bound_delete(path: &Path, expected: &DeleteExpectation) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "conditional_delete_parent_missing".to_string())?;
    let quarantine_directory = (0..8)
        .find_map(|_| {
            let candidate = parent.join(format!(".mfb-delete-{}", Uuid::new_v4()));
            match fs::create_dir(&candidate) {
                Ok(()) => Some(Ok(candidate)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(format!(
                    "conditional_delete_quarantine_create_failed:{error}"
                ))),
            }
        })
        .transpose()?
        .ok_or_else(|| "conditional_delete_quarantine_collision".to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&quarantine_directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("conditional_delete_quarantine_acl_failed:{error}"))?;
    }
    // The destination name is inside an atomically create-new, private empty
    // directory. No path existence probe is used as an exclusion primitive.
    let quarantine = quarantine_directory.join("entry");
    fs::rename(path, &quarantine).map_err(|error| {
        let _ = fs::remove_dir(&quarantine_directory);
        format!("conditional_delete_quarantine_failed:{error}")
    })?;

    let validation = (|| {
        let mut file = File::open(&quarantine)
            .map_err(|error| format!("conditional_delete_open_failed:{error}"))?;
        validate_delete_candidate(&mut file, &quarantine, expected)
    })();
    if let Err(error) = validation {
        match fs::hard_link(&quarantine, path) {
            Ok(()) => {
                fs::remove_file(&quarantine).map_err(|restore_error| {
                    format!("conditional_delete_restore_failed:{restore_error}")
                })?;
                fs::remove_dir(&quarantine_directory).map_err(|restore_error| {
                    format!("conditional_delete_quarantine_cleanup_failed:{restore_error}")
                })?;
            }
            Err(restore_error) => {
                return Err(format!(
                    "{error}:replacement_retained_at={}:restore_error={restore_error}",
                    quarantine.display()
                ));
            }
        }
        return Err(error);
    }
    fs::remove_file(&quarantine)
        .map_err(|error| format!("conditional_delete_unlink_failed:{error}"))?;
    fs::remove_dir(&quarantine_directory)
        .map_err(|error| format!("conditional_delete_quarantine_cleanup_failed:{error}"))?;
    sync_parent_directory(parent)
        .map_err(|error| format!("conditional_delete_directory_flush_failed:{error}"))?;
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    hash_open_file(&mut file)
}

pub(crate) fn hash_open_file(file: &mut File) -> Result<String, String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn fingerprint_file(path: &Path) -> Result<FileFingerprint, String> {
    let before = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !before.is_file() || metadata_is_reparse(&before) {
        return Err("source_not_regular_file".into());
    }
    let content_sha256 = hash_file(path)?;
    let after = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || native_file_identity(path, &before) != native_file_identity(path, &after)
    {
        return Err("source_changed_during_fingerprint".into());
    }
    let modified = after
        .modified()
        .map_err(|error| error.to_string())?
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?;
    Ok(FileFingerprint {
        size_bytes: after.len(),
        mtime_ns: modified.as_nanos() as i128,
        content_sha256: Some(content_sha256),
        file_identity: native_file_identity(path, &after),
        version: 1,
    })
}

#[cfg(windows)]
fn native_file_identity(path: &Path, _metadata: &fs::Metadata) -> Option<String> {
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    native_file_identity_from_file(&file, path, &metadata)
}

#[cfg(windows)]
pub(crate) fn native_file_identity_from_file(
    file: &File,
    _path: &Path,
    _metadata: &fs::Metadata,
) -> Option<String> {
    use std::{ffi::c_void, os::windows::io::AsRawHandle};

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            file: *mut c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let mut information = ByHandleFileInformation::default();
    // SAFETY: `file` remains open for the call and `information` points to a
    // correctly laid-out writable Win32 structure.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) };
    if succeeded == 0 {
        return None;
    }
    Some(format!(
        "windows-v1:{:08x}:{:08x}{:08x}",
        information.volume_serial_number, information.file_index_high, information.file_index_low
    ))
}

#[cfg(unix)]
fn native_file_identity(_path: &Path, metadata: &fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(format!("unix-v1:{:x}:{:x}", metadata.dev(), metadata.ino()))
}

#[cfg(unix)]
pub(crate) fn native_file_identity_from_file(
    _file: &File,
    _path: &Path,
    metadata: &fs::Metadata,
) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(format!("unix-v1:{:x}:{:x}", metadata.dev(), metadata.ino()))
}

#[cfg(not(any(windows, unix)))]
fn native_file_identity(_path: &Path, _metadata: &fs::Metadata) -> Option<String> {
    None
}

#[cfg(not(any(windows, unix)))]
pub(crate) fn native_file_identity_from_file(
    _file: &File,
    _path: &Path,
    _metadata: &fs::Metadata,
) -> Option<String> {
    None
}

fn create_directories_no_reparse(path: &Path) -> Result<(), String> {
    let mut ancestors = path
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .collect::<Vec<_>>();
    ancestors.reverse();
    for directory in ancestors {
        match fs::symlink_metadata(directory) {
            Ok(metadata) if metadata_is_reparse(&metadata) => {
                return Err(format!("reparse_point_forbidden:{}", directory.display()));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "target_parent_not_directory:{}",
                    directory.display()
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(directory).map_err(|create_error| {
                    format!("target_parent_create_failed:{create_error}")
                })?;
                let metadata =
                    fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
                if metadata_is_reparse(&metadata) || !metadata.is_dir() {
                    return Err(format!("unsafe_created_parent:{}", directory.display()));
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn is_reparse_path(path: &Path) -> Result<bool, String> {
    fs::symlink_metadata(path)
        .map(|metadata| metadata_is_reparse(&metadata))
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
pub(crate) fn metadata_is_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
pub(crate) fn metadata_is_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// Doctor shares the scanner and fingerprint policy, with additional reader formats.
pub struct DoctorFileSystem;
impl FileSystem for DoctorFileSystem {
    fn enumerate(
        &self,
        root: &Path,
        _follow_links: bool,
        visitor: &mut dyn FnMut(Result<PathBuf, String>) -> bool,
    ) -> Result<(), String> {
        enumerate_paths(root, false, true, visitor)
    }
    fn fingerprint(&self, path: &Path) -> Result<FileFingerprint, String> {
        LocalFileSystem.ensure_no_reparse_points(path)?;
        fingerprint_file(path)
    }
}
fn enumerate_paths(
    root: &Path,
    follow_links: bool,
    doctor: bool,
    visitor: &mut dyn FnMut(Result<PathBuf, String>) -> bool,
) -> Result<(), String> {
    if !root.is_dir() {
        return Err("scan_root_not_found".into());
    }
    if !follow_links {
        LocalFileSystem.ensure_no_reparse_points(root)?;
    }
    let mut walker = WalkDir::new(root).follow_links(follow_links).into_iter();
    while let Some(entry) = walker.next() {
        let entry = match entry {
            Ok(value) => value,
            Err(error) => {
                if !visitor(Err(error.to_string())) {
                    break;
                }
                continue;
            }
        };
        let reparse = match is_reparse_path(entry.path()) {
            Ok(value) => value,
            Err(error) => {
                if entry.file_type().is_dir() {
                    walker.skip_current_dir();
                }
                if !visitor(Err(format!(
                    "metadata_unavailable:{}:{error}",
                    entry.path().display()
                ))) {
                    break;
                }
                continue;
            }
        };
        if reparse && !follow_links {
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            if !visitor(Err(format!(
                "reparse_point_skipped:{}",
                entry.path().display()
            ))) {
                break;
            }
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let extension = entry
            .path()
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase);
        let supported = matches!(extension.as_deref(), Some("flac" | "mp3" | "m4a" | "ogg"))
            || (doctor && matches!(extension.as_deref(), Some("aac" | "opus" | "wav")))
            || (!doctor
                && matches!(
                    extension.as_deref(),
                    Some("jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp")
                ));
        if supported && !visitor(Ok(entry.into_path())) {
            break;
        }
    }
    Ok(())
}
