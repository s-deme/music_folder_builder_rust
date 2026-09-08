use music_folder_core::{windows_path_key, windows_path_scopes_overlap};
#[cfg(windows)]
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::path::Path;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Debug)]
struct ActiveReservation {
    id: u64,
    scopes: Vec<PathBuf>,
}

static ACTIVE_ROOT_LOCKS: std::sync::OnceLock<std::sync::Mutex<Vec<ActiveReservation>>> =
    std::sync::OnceLock::new();
static NEXT_RESERVATION_ID: AtomicU64 = AtomicU64::new(1);

/// Process-lifetime guard for one atomic set of mutation scopes.
///
/// On Windows the platform layer takes an exclusive byte-range lock for every
/// exact scope and shared byte-range locks for its ancestors. Descendants thus
/// conflict with an ancestor's exclusive lock, while unrelated siblings may
/// concurrently hold the shared lock for their common parent. All byte ranges
/// are acquired in deterministic order and released on any partial failure.
pub struct RootProcessLock {
    reservation_id: u64,
    #[cfg(windows)]
    platform: WindowsRangeLocks,
}

impl RootProcessLock {
    #[cfg(test)]
    pub fn acquire(root: &Path) -> Result<Self, String> {
        Self::acquire_many(&[root.to_path_buf()])
    }

    pub fn acquire_many(roots: &[PathBuf]) -> Result<Self, String> {
        let scopes = minimal_scopes(roots)?;
        let reservation_id = reserve_in_process(&scopes)?;
        #[cfg(windows)]
        let platform = match acquire_platform(&scopes) {
            Ok(platform) => platform,
            Err(error) => {
                release_in_process(reservation_id);
                return Err(error);
            }
        };
        #[cfg(not(windows))]
        if let Err(error) = acquire_platform(&scopes) {
            release_in_process(reservation_id);
            return Err(error);
        }
        Ok(Self {
            reservation_id,
            #[cfg(windows)]
            platform,
        })
    }
}

fn minimal_scopes(roots: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    if roots.is_empty() {
        return Err("mutation_scope_empty".into());
    }
    let mut roots = roots
        .iter()
        .filter(|root| !root.as_os_str().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if roots.is_empty() {
        return Err("mutation_scope_empty".into());
    }
    roots.sort_by_key(|root| windows_path_key(root));
    roots.dedup_by(|left, right| windows_path_key(left) == windows_path_key(right));
    let mut minimal = Vec::new();
    for candidate in &roots {
        if roots.iter().any(|other| {
            windows_path_key(other) != windows_path_key(candidate)
                && music_folder_core::windows_path_is_same_or_descendant(other, candidate)
        }) {
            continue;
        }
        minimal.push(candidate.clone());
    }
    minimal.sort_by_key(|root| windows_path_key(root));
    Ok(minimal)
}

fn reserve_in_process(scopes: &[PathBuf]) -> Result<u64, String> {
    let active = ACTIVE_ROOT_LOCKS.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    let mut guard = active
        .lock()
        .map_err(|_| "mutation_os_mutex_poisoned".to_string())?;
    if guard.iter().any(|reservation| {
        reservation.scopes.iter().any(|active_scope| {
            scopes
                .iter()
                .any(|scope| windows_path_scopes_overlap(active_scope, scope))
        })
    }) {
        return Err("mutation_scope_busy".into());
    }
    let id = NEXT_RESERVATION_ID.fetch_add(1, Ordering::Relaxed);
    guard.push(ActiveReservation {
        id,
        scopes: scopes.to_vec(),
    });
    Ok(id)
}

fn release_in_process(reservation_id: u64) {
    if let Some(active) = ACTIVE_ROOT_LOCKS.get() {
        if let Ok(mut guard) = active.lock() {
            guard.retain(|reservation| reservation.id != reservation_id);
        }
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
struct RangeRequest {
    offset: u64,
    exclusive: bool,
}

#[cfg(windows)]
fn range_requests(scopes: &[PathBuf]) -> Vec<RangeRequest> {
    use std::collections::BTreeMap;

    let mut requests = BTreeMap::<u64, bool>::new();
    for scope in scopes {
        for (index, ancestor) in scope
            .ancestors()
            .filter(|path| !path.as_os_str().is_empty())
            .enumerate()
        {
            let offset = scope_lock_offset(&windows_path_key(ancestor));
            let exclusive = index == 0;
            requests
                .entry(offset)
                .and_modify(|current| *current |= exclusive)
                .or_insert(exclusive);
        }
    }
    requests
        .into_iter()
        .map(|(offset, exclusive)| RangeRequest { offset, exclusive })
        .collect()
}

#[cfg(windows)]
fn scope_lock_offset(key: &str) -> u64 {
    let digest = Sha256::digest(key.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(bytes)
}

#[cfg(windows)]
struct WindowsRangeLocks {
    file: std::fs::File,
    acquired: Vec<RangeRequest>,
}

#[cfg(windows)]
impl std::fmt::Debug for WindowsRangeLocks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowsRangeLocks")
            .field("acquired", &self.acquired)
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: *mut std::ffi::c_void,
}

#[cfg(windows)]
fn acquire_platform(scopes: &[PathBuf]) -> Result<WindowsRangeLocks, String> {
    use std::{fs::OpenOptions, os::windows::io::AsRawHandle, ptr};

    #[link(name = "kernel32")]
    extern "system" {
        fn LockFileEx(
            file: *mut std::ffi::c_void,
            flags: u32,
            reserved: u32,
            bytes_low: u32,
            bytes_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }
    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;

    let directory = std::env::temp_dir().join("music-folder-builder-locks-v1");
    std::fs::create_dir_all(&directory).map_err(|_| "mutation_os_lock_directory_failed")?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("scope-ranges.lock"))
        .map_err(|_| "mutation_os_lock_open_failed")?;
    let requests = range_requests(scopes);
    let mut acquired = Vec::with_capacity(requests.len());
    for request in requests {
        let mut overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: request.offset as u32,
            offset_high: (request.offset >> 32) as u32,
            event: ptr::null_mut(),
        };
        let flags = LOCKFILE_FAIL_IMMEDIATELY
            | if request.exclusive {
                LOCKFILE_EXCLUSIVE_LOCK
            } else {
                0
            };
        // SAFETY: `file` owns a valid Windows file handle and `overlapped`
        // remains alive for this synchronous, fail-immediately request.
        let locked = unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &mut overlapped) };
        if locked == 0 {
            unlock_ranges(&file, &mut acquired);
            return Err("mutation_scope_busy".into());
        }
        acquired.push(request);
    }
    Ok(WindowsRangeLocks { file, acquired })
}

#[cfg(windows)]
fn unlock_ranges(file: &std::fs::File, acquired: &mut Vec<RangeRequest>) {
    use std::{os::windows::io::AsRawHandle, ptr};

    #[link(name = "kernel32")]
    extern "system" {
        fn UnlockFileEx(
            file: *mut std::ffi::c_void,
            reserved: u32,
            bytes_low: u32,
            bytes_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }
    while let Some(request) = acquired.pop() {
        let mut overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: request.offset as u32,
            offset_high: (request.offset >> 32) as u32,
            event: ptr::null_mut(),
        };
        // SAFETY: this exact byte range was acquired on `file` by this guard.
        let _ = unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) };
    }
}

#[cfg(windows)]
impl Drop for WindowsRangeLocks {
    fn drop(&mut self) {
        unlock_ranges(&self.file, &mut self.acquired);
    }
}

#[cfg(not(windows))]
fn acquire_platform(_scopes: &[PathBuf]) -> Result<(), String> {
    Ok(())
}

impl Drop for RootProcessLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        let _keep_platform_alive = &self.platform;
        release_in_process(self.reservation_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_and_contained_scopes_are_exclusive() {
        let parent = PathBuf::from(r"C:\music");
        let same = parent.clone();
        let child = parent.join("album");
        let guard = RootProcessLock::acquire(&parent).unwrap();
        assert!(matches!(
            RootProcessLock::acquire(&same),
            Err(error) if error == "mutation_scope_busy"
        ));
        assert!(matches!(
            RootProcessLock::acquire(&child),
            Err(error) if error == "mutation_scope_busy"
        ));
        drop(guard);
        assert!(RootProcessLock::acquire(&child).is_ok());
    }

    #[test]
    fn unrelated_siblings_do_not_overblock() {
        let first = RootProcessLock::acquire(Path::new(r"C:\library-a")).unwrap();
        let second = RootProcessLock::acquire(Path::new(r"C:\library-b")).unwrap();
        drop((first, second));
    }

    #[test]
    fn multi_scope_acquisition_is_atomic_and_minimizes_children() {
        let first = RootProcessLock::acquire_many(&[
            PathBuf::from(r"C:\source"),
            PathBuf::from(r"D:\target"),
            PathBuf::from(r"C:\source\nested"),
        ])
        .unwrap();
        assert!(RootProcessLock::acquire(Path::new(r"D:\target\album")).is_err());
        assert!(RootProcessLock::acquire(Path::new(r"C:\source\other")).is_err());
        assert!(RootProcessLock::acquire(Path::new(r"C:\unrelated")).is_ok());
        drop(first);
    }
}
