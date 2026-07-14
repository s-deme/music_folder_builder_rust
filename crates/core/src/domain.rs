use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanAction {
    Move,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Risk {
    None,
    InvalidTarget,
    PathTooLong,
    Conflict,
    MetadataMissing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub size_bytes: u64,
    pub mtime_ns: i128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackMetadata {
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub year: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedFile {
    pub id: Uuid,
    pub path: PathBuf,
    pub fingerprint: FileFingerprint,
    pub metadata: Option<TrackMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: Uuid,
    pub ordinal: u64,
    pub file: ScannedFile,
    pub target: Option<PathBuf>,
    pub action: PlanAction,
    pub risk: Risk,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ApplyItem {
    pub plan_item_id: String,
    pub ordinal: u64,
    pub source: PathBuf,
    pub target: Option<PathBuf>,
    pub action: PlanAction,
    pub risk: Risk,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OperationLog {
    pub plan_item_id: String,
    pub sequence_no: u64,
    pub source: PathBuf,
    pub target: Option<PathBuf>,
    pub action: String,
    pub result: String,
    pub error: Option<String>,
    pub source_deleted: bool,
    pub expected_size: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct VerifyItem {
    pub operation_id: String,
    pub sequence_no: u64,
    pub source: PathBuf,
    pub target: Option<PathBuf>,
    pub action: String,
    pub expected_size: Option<u64>,
}

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("target path is unsafe: {0}")]
    UnsafePath(String),
    #[error("a completed plan is required")]
    PlanNotApplicable,
    #[error("source and target are identical")]
    SamePath,
}

const INVALID_WINDOWS_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub fn sanitize_component(value: &str) -> String {
    let mut value: String = value
        .chars()
        .map(|c| {
            if INVALID_WINDOWS_CHARS.contains(&c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    value = value.trim_end_matches([' ', '.']).to_owned();
    if value.is_empty() {
        value = "_".into();
    }
    let stem = value
        .split('.')
        .next()
        .unwrap_or(&value)
        .to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        value.insert(0, '_');
    }
    value
}

pub fn assess_windows_path(path: &Path) -> Result<(), DomainError> {
    if path.as_os_str().is_empty() {
        return Err(DomainError::UnsafePath("empty_path".into()));
    }
    if path.to_string_lossy().chars().count() > 240 {
        return Err(DomainError::UnsafePath("path_too_long".into()));
    }
    for component in path.components() {
        if let Component::Normal(part) = component {
            let text = part.to_string_lossy();
            if text.chars().count() > 80 {
                return Err(DomainError::UnsafePath("component_too_long".into()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sanitizes_reserved_and_invalid_names() {
        assert_eq!(sanitize_component("CON"), "_CON");
        assert_eq!(sanitize_component("track?"), "track_");
    }
    #[test]
    fn detects_long_path() {
        assert!(assess_windows_path(Path::new(&"a".repeat(241))).is_err());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    #[test]
    fn accepts_japanese_path_and_rejects_reserved_component() {
        assert!(assess_windows_path(Path::new(r"C:\音楽\宇多田\曲.mp3")).is_ok());
        assert_eq!(sanitize_component("CON"), "_CON");
    }
}
