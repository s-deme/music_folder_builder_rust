//! On-demand, read-only artwork. Only reduced PNGs are cached, never source images.
use crate::{
    doctor_view::{Album, DoctorView},
    path_codec::{base64, path_envelope, LosslessPathEnvelope},
    windows_fs::{hash_open_file, metadata_is_reparse, native_file_identity_from_file},
};
use image::{ImageFormat, ImageReader, Limits};
use lofty::{
    config::{apply_global_options, GlobalOptions, ParseOptions},
    picture::PictureType,
    prelude::TaggedFileExt,
    probe::Probe,
};
use music_folder_core::{FileFingerprint, ScannedFile};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Seek, SeekFrom},
    path::{Component, Path},
    sync::{Condvar, Mutex},
    time::UNIX_EPOCH,
};

pub const INPUT_LIMIT: usize = 16 * 1024 * 1024;
pub const CACHE_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Clone, Serialize)]
pub struct Artwork {
    pub data_url: Option<String>,
    pub origin: Option<String>,
    pub source: Option<LosslessPathEnvelope>,
    pub note: String,
}
impl Artwork {
    fn unavailable(note: &str) -> Self {
        Self {
            data_url: None,
            origin: None,
            source: None,
            note: note.into(),
        }
    }
}

#[derive(Default)]
pub struct ThumbnailCache {
    entries: VecDeque<(String, String)>,
    bytes: usize,
}
impl ThumbnailCache {
    pub fn get(&mut self, key: &str) -> Option<String> {
        let index = self.entries.iter().position(|(k, _)| k == key)?;
        let entry = self.entries.remove(index)?;
        let result = entry.1.clone();
        self.entries.push_back(entry);
        Some(result)
    }
    pub fn insert(&mut self, key: String, data: String) {
        if let Some(index) = self.entries.iter().position(|(k, _)| k == &key) {
            let old = self.entries.remove(index).unwrap();
            self.bytes -= old.0.len() + old.1.len();
        }
        let size = key.len() + data.len();
        if size > CACHE_LIMIT {
            return;
        }
        while self.bytes + size > CACHE_LIMIT {
            if let Some(old) = self.entries.pop_front() {
                self.bytes -= old.0.len() + old.1.len();
            } else {
                break;
            }
        }
        self.bytes += size;
        self.entries.push_back((key, data));
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[derive(Default)]
pub struct ArtworkService {
    cache: Mutex<ThumbnailCache>,
    active: Mutex<usize>,
    available: Condvar,
}
struct Slot<'a>(&'a ArtworkService);
impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.0.active.lock() {
            *active -= 1;
            self.0.available.notify_one();
        }
    }
}

impl ArtworkService {
    fn slot(&self) -> Result<Slot<'_>, String> {
        let mut active = self.active.lock().map_err(|e| e.to_string())?;
        while *active >= 2 {
            active = self.available.wait(active).map_err(|e| e.to_string())?;
        }
        *active += 1;
        Ok(Slot(self))
    }
    fn thumbnail(&self, bytes: &[u8]) -> Result<String, String> {
        if bytes.len() > INPUT_LIMIT {
            return Err("artwork_input_too_large".into());
        }
        let key = format!("png256-v1:{:x}", Sha256::digest(bytes));
        if let Some(data) = self.cache.lock().map_err(|e| e.to_string())?.get(&key) {
            return Ok(data);
        }
        let format = image::guess_format(bytes).map_err(|e| e.to_string())?;
        if !matches!(
            format,
            ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP
        ) {
            return Err("artwork_format_unsupported".into());
        }
        let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
        let mut limits = Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let decoded = reader.decode().map_err(|e| e.to_string())?;
        let thumbnail = decoded.thumbnail(256, 256);
        let mut output = Cursor::new(Vec::new());
        thumbnail
            .write_to(&mut output, ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        let data = format!("data:image/png;base64,{}", base64(output.get_ref()));
        self.cache
            .lock()
            .map_err(|e| e.to_string())?
            .insert(key, data.clone());
        Ok(data)
    }
    pub fn load(&self, view: &DoctorView, album: &Album) -> Artwork {
        self.load_current(view, album, || true)
    }

    pub fn load_current(
        &self,
        view: &DoctorView,
        album: &Album,
        current: impl Fn() -> bool,
    ) -> Artwork {
        let Ok(_slot) = self.slot() else {
            return Artwork::unavailable("画像処理を開始できません");
        };
        if !current() {
            return Artwork::unavailable("画像の取得対象が切り替わりました");
        }
        if album.row.unclassified {
            return Artwork::unavailable("未分類の曲はアルバム画像を選択しません");
        }
        let mut files: Vec<_> = album
            .file_ids
            .iter()
            .filter_map(|id| view.files.get(id))
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let mut fallback = None;
        let mut failed = false;
        for file in &files {
            if !current() {
                return Artwork::unavailable("画像の取得対象が切り替わりました");
            }
            if !file
                .metadata
                .as_ref()
                .is_some_and(|m| m.has_artwork == Some(true))
            {
                continue;
            }
            match embedded(&view.source, file) {
                Ok(pictures) => {
                    for (front, bytes) in pictures {
                        if front {
                            if let Ok(data_url) = self.thumbnail(&bytes) {
                                return artwork(data_url, "embedded", &file.path);
                            }
                            failed = true;
                        } else if fallback.is_none() {
                            if let Ok(data_url) = self.thumbnail(&bytes) {
                                fallback = Some(artwork(data_url, "embedded", &file.path));
                            } else {
                                failed = true;
                            }
                        }
                    }
                }
                Err(_) => failed = true,
            }
        }
        if let Some(found) = fallback {
            return found;
        }
        let mut directories = vec![album.folder.clone()];
        let discs: BTreeSet<_> = files
            .iter()
            .filter_map(|f| f.path.parent())
            .filter(|p| *p != album.folder)
            .map(Path::to_path_buf)
            .collect();
        directories.extend(discs);
        for directory in directories {
            if !current() {
                return Artwork::unavailable("画像の取得対象が切り替わりました");
            }
            if checked_path(&view.source, &directory).is_err() {
                failed = true;
                continue;
            }
            let Ok(entries) = fs::read_dir(&directory) else {
                failed = true;
                continue;
            };
            let mut candidates = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                for (stem_order, stem) in ["cover", "folder", "front"].iter().enumerate() {
                    for (ext_order, ext) in ["jpg", "jpeg", "png", "webp"].iter().enumerate() {
                        if name == format!("{stem}.{ext}") {
                            candidates.push((stem_order, ext_order, entry.path()));
                        }
                    }
                }
            }
            candidates.sort();
            for (_, _, path) in candidates {
                match external(&view.source, &path).and_then(|bytes| self.thumbnail(&bytes)) {
                    Ok(data_url) => return artwork(data_url, "folder", &path),
                    Err(_) => failed = true,
                }
            }
        }
        Artwork::unavailable(if failed {
            "画像を表示できません（読取失敗・変更・非対応・上限超過）"
        } else {
            "ジャケット画像なし"
        })
    }
}

fn artwork(data_url: String, origin: &str, path: &Path) -> Artwork {
    Artwork {
        data_url: Some(data_url),
        origin: Some(origin.into()),
        source: Some(path_envelope(path, "artwork")),
        note: "閲覧時の画像です。診断時点の画像は保存していません。".into(),
    }
}

fn checked_path(root: &Path, path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || !path.starts_with(root)
        || path.components().any(|c| matches!(c, Component::ParentDir))
    {
        return Err("artwork_outside_source".into());
    }
    for ancestor in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
        let m = fs::symlink_metadata(ancestor).map_err(|e| e.to_string())?;
        if metadata_is_reparse(&m) {
            return Err("artwork_reparse_forbidden".into());
        }
    }
    Ok(())
}
fn open_checked(root: &Path, path: &Path) -> Result<File, String> {
    checked_path(root, path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata_is_reparse(&metadata) {
        return Err("artwork_not_regular".into());
    }
    checked_path(root, path)?;
    Ok(file)
}
fn fingerprint(file: &mut File, path: &Path) -> Result<FileFingerprint, String> {
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let m = file.metadata().map_err(|e| e.to_string())?;
    Ok(FileFingerprint {
        size_bytes: m.len(),
        mtime_ns: m
            .modified()
            .map_err(|e| e.to_string())?
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos() as i128,
        content_sha256: Some(hash_open_file(file)?),
        file_identity: native_file_identity_from_file(file, path, &m),
        version: 1,
    })
}
fn same(a: &FileFingerprint, b: &FileFingerprint) -> bool {
    a.size_bytes == b.size_bytes
        && a.mtime_ns == b.mtime_ns
        && a.file_identity == b.file_identity
        && a.content_sha256 == b.content_sha256
        && a.version == b.version
}
fn embedded(root: &Path, source: &ScannedFile) -> Result<Vec<(bool, Vec<u8>)>, String> {
    let mut file = open_checked(root, &source.path)?;
    let before = fingerprint(&mut file, &source.path)?;
    if !same(&before, &source.fingerprint) {
        return Err("artwork_music_changed".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    // Dedicated artwork worker threads: these options cannot change scan workers.
    apply_global_options(
        GlobalOptions::new()
            .allocation_limit(INPUT_LIMIT)
            .preserve_format_specific_items(false),
    );
    // Limit total bytes consumed by the tag parser, as its allocation limit
    // applies to one item rather than the entire tag collection.
    let tagged = Probe::new(TagReadBudget {
        file: &mut file,
        remaining: 64 * 1024 * 1024,
    })
    .guess_file_type()
    .map_err(|e| e.to_string())?
    .options(ParseOptions::new().read_properties(false))
    .read()
    .map_err(|e| e.to_string())?;
    let mut pictures = Vec::new();
    let mut bytes = 0;
    for picture in tagged.tags().iter().flat_map(|t| t.pictures()) {
        bytes += picture.data().len();
        if bytes > INPUT_LIMIT {
            return Err("artwork_total_input_too_large".into());
        }
        pictures.push((
            picture.pic_type() == PictureType::CoverFront,
            picture.data().to_vec(),
        ));
    }
    if !same(&before, &fingerprint(&mut file, &source.path)?) {
        return Err("artwork_changed_during_read".into());
    }
    let mut current = open_checked(root, &source.path)?;
    if !same(&before, &fingerprint(&mut current, &source.path)?) {
        return Err("artwork_music_replaced".into());
    }
    Ok(pictures)
}

struct TagReadBudget<'a> {
    file: &'a mut File,
    remaining: usize,
}
impl Read for TagReadBudget<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            return Err(std::io::Error::other("artwork_tag_read_budget_exceeded"));
        }
        let limit = buffer.len().min(self.remaining);
        let count = self.file.read(&mut buffer[..limit])?;
        self.remaining -= count;
        Ok(count)
    }
}
impl Seek for TagReadBudget<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(pos)
    }
}
fn external(root: &Path, path: &Path) -> Result<Vec<u8>, String> {
    let mut file = open_checked(root, path)?;
    if file.metadata().map_err(|e| e.to_string())?.len() > INPUT_LIMIT as u64 {
        return Err("artwork_input_too_large".into());
    }
    let before = fingerprint(&mut file, path)?;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(INPUT_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > INPUT_LIMIT
        || before.content_sha256.as_deref() != Some(&format!("{:x}", Sha256::digest(&bytes)))
        || !same(&before, &fingerprint(&mut file, path)?)
    {
        return Err("artwork_changed_during_read".into());
    }
    let mut current = open_checked(root, path)?;
    if !same(&before, &fingerprint(&mut current, path)?) {
        return Err("artwork_replaced".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thumbnail_limits_and_cache_are_enforced() {
        let service = ArtworkService::default();
        assert!(service.thumbnail(&vec![0; INPUT_LIMIT + 1]).is_err());
        assert!(service.thumbnail(b"broken").is_err());
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(4097, 1)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        assert!(service.thumbnail(bytes.get_ref()).is_err());
        let mut cache = ThumbnailCache::default();
        for n in 0..40 {
            cache.insert(n.to_string(), "x".repeat(1024 * 1024));
            assert!(cache.bytes() <= CACHE_LIMIT);
        }
        assert!(cache.get("0").is_none());
        assert!(cache.get("39").is_some());
        cache.insert("39".into(), "tiny".into());
        assert!(cache.bytes() < CACHE_LIMIT);
    }
    #[test]
    fn two_artwork_workers_are_the_maximum() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let service = Arc::new(ArtworkService::default());
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (service, active, peak) = (service.clone(), active.clone(), peak.clone());
                std::thread::spawn(move || {
                    let _slot = service.slot().unwrap();
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    active.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn links_and_parent_escape_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let outside = temp.path().join("cover.png");
        fs::write(&outside, b"outside").unwrap();
        assert!(external(&root, &outside).is_err());
        assert!(external(&root, &root.join("../cover.png")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("cover.png")).unwrap();
            assert!(external(&root, &root.join("cover.png")).is_err());
        }
        #[cfg(windows)]
        {
            let linked = root.join("linked");
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&linked)
                .arg(temp.path())
                .output()
                .unwrap();
            assert!(output.status.success());
            assert!(external(&root, &linked.join("cover.png")).is_err());
            fs::remove_dir(linked).unwrap();
        }
    }
}
