use music_folder_core::{
    ports::{FileMutator, FileSystem},
    FileFingerprint,
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use walkdir::WalkDir;

pub struct LocalFileSystem;
impl FileSystem for LocalFileSystem {
    fn enumerate(
        &self,
        root: &Path,
        follow_links: bool,
        visitor: &mut dyn FnMut(Result<PathBuf, String>) -> bool,
    ) -> Result<(), String> {
        for entry in WalkDir::new(root).follow_links(follow_links) {
            let entry = match entry {
                Ok(value) => value,
                Err(error) => {
                    if !visitor(Err(error.to_string())) {
                        break;
                    }
                    continue;
                }
            };
            if entry.file_type().is_symlink() && !follow_links {
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
            if matches!(
                entry
                    .path()
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("flac" | "mp3" | "m4a" | "ogg")
            ) && !visitor(Ok(entry.into_path()))
            {
                break;
            }
        }
        Ok(())
    }
    fn fingerprint(&self, path: &Path) -> Result<FileFingerprint, String> {
        let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
        let modified = metadata
            .modified()
            .map_err(|e| e.to_string())?
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?;
        Ok(FileFingerprint {
            size_bytes: metadata.len(),
            mtime_ns: modified.as_nanos() as i128,
        })
    }
}

impl FileMutator for LocalFileSystem {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn same_volume(&self, source: &Path, target: &Path) -> Result<bool, String> {
        Ok(source.components().next() == target.components().next())
    }
    fn move_file(&self, source: &Path, target: &Path) -> Result<(), String> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::rename(source, target).map_err(|e| e.to_string())
    }
    fn copy_file(&self, source: &Path, target: &Path) -> Result<(), String> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::copy(source, target)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn size(&self, path: &Path) -> Result<u64, String> {
        fs::metadata(path)
            .map(|m| m.len())
            .map_err(|e| e.to_string())
    }
    fn delete_file(&self, path: &Path) -> Result<(), String> {
        fs::remove_file(path).map_err(|e| e.to_string())
    }
}
