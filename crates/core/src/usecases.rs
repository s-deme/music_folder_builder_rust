#![allow(clippy::items_after_test_module)]

use crate::{
    assess_windows_path,
    ports::{
        ApplyStore, FileMutator, FileSystem, ManualTargetChange, MetadataReader, PlanRevisionStore,
        PlanStore, RollbackStore, ScanStore, VerifyStore,
    },
    render_template, sanitize_component, FileKind, NamingRules, OperationLog, PlanAction, PlanItem,
    Risk, ScannedFile,
};
use crossbeam_channel::{bounded, Receiver};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::Instant,
};
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);
impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScanProgress {
    pub scan_id: String,
    pub phase: String,
    pub enumerated: u64,
    pub processed: u64,
    pub cache_hits: u64,
    pub warnings: u64,
    pub elapsed_ms: u64,
    pub items_per_second: f64,
    pub eta_seconds: Option<u64>,
}
pub type ProgressSink = Arc<dyn Fn(ScanProgress) + Send + Sync>;

#[derive(Clone)]
pub struct ScanOptions {
    pub workers: usize,
    pub batch_size: usize,
    pub queue_capacity: usize,
    pub follow_reparse_points: bool,
    pub cancellation: CancellationToken,
    pub progress: Option<ProgressSink>,
}

pub struct RollbackResult {
    pub rollback_id: String,
    pub success: u64,
    pub skipped: u64,
    pub failed: u64,
}
pub struct RollbackUseCase<S, F> {
    pub store: Arc<S>,
    pub files: Arc<F>,
}
impl<S: RollbackStore, F: FileMutator> RollbackUseCase<S, F> {
    pub fn execute(&self, execution_id: &str, dry_run: bool) -> Result<RollbackResult, String> {
        let started = Instant::now();
        let rollback_id = self.store.begin_rollback(execution_id, dry_run)?;
        let mut items = self.store.load_rollback_items(execution_id)?;
        items.sort_by_key(|item| std::cmp::Reverse(item.sequence_no));
        let (mut success, mut skipped, mut failed) = (0, 0, 0);
        for item in items {
            let (result, error) = match item.target.as_ref() {
                None => {
                    skipped += 1;
                    ("skipped", Some("target_missing_in_log"))
                }
                Some(t) if !self.files.exists(t) => {
                    failed += 1;
                    ("failed", Some("target_missing"))
                }
                Some(_) if self.files.exists(&item.source) => {
                    skipped += 1;
                    ("skipped", Some("source_already_exists"))
                }
                Some(_) if dry_run => {
                    success += 1;
                    ("success", None)
                }
                Some(t) if item.action == "move" => match self.files.move_file(t, &item.source) {
                    Ok(()) => {
                        success += 1;
                        ("success", None)
                    }
                    Err(_) => {
                        failed += 1;
                        ("failed", Some("reverse_move_failed"))
                    }
                },
                Some(t) => match self.files.copy_file(t, &item.source).and_then(|_| {
                    if self.files.size(t)? == self.files.size(&item.source)? {
                        Ok(())
                    } else {
                        Err("reverse_copy_verify_failed".into())
                    }
                }) {
                    Ok(()) => match self.files.delete_file(t) {
                        Ok(()) => {
                            success += 1;
                            ("success", None)
                        }
                        Err(_) => {
                            failed += 1;
                            ("failed", Some("reverse_target_delete_failed"))
                        }
                    },
                    Err(_) => {
                        failed += 1;
                        ("failed", Some("reverse_copy_verify_failed"))
                    }
                },
            };
            self.store
                .save_rollback_result(execution_id, &item.operation_id, result, error)?;
        }
        let status = if failed == 0 { "completed" } else { "partial" };
        self.store
            .finish_rollback(&rollback_id, status, success, skipped, failed)?;
        self.store.record_metric(
            &rollback_id,
            "rollback",
            started.elapsed().as_millis() as u64,
            success + skipped + failed,
        )?;
        Ok(RollbackResult {
            rollback_id,
            success,
            skipped,
            failed,
        })
    }
}
impl Default for ScanOptions {
    fn default() -> Self {
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 8));
        Self {
            workers,
            batch_size: 250,
            queue_capacity: workers * 4,
            follow_reparse_points: false,
            cancellation: CancellationToken::default(),
            progress: None,
        }
    }
}
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub scan_id: String,
    pub files: u64,
    pub cache_hits: u64,
    pub warnings: u64,
}

pub struct ScanUseCase<F, M, S> {
    pub fs: Arc<F>,
    pub metadata: Arc<M>,
    pub store: Arc<S>,
}
impl<F: FileSystem + 'static, M: MetadataReader + 'static, S: ScanStore + 'static>
    ScanUseCase<F, M, S>
{
    pub fn execute(&self, root: &Path, options: &ScanOptions) -> Result<ScanResult, String> {
        let started = Instant::now();
        let scan_id = self.store.begin_scan(root)?;
        let worker_count = options.workers.max(1);
        let queue_capacity = options.queue_capacity.max(worker_count);
        let (path_sender, path_receiver) = bounded(queue_capacity);
        let (result_sender, result_receiver) = bounded(queue_capacity);
        let root = root.to_path_buf();
        let cancellation = options.cancellation.clone();
        let progress = options.progress.clone();
        let progress_id = scan_id.clone();
        let enumerated_count = Arc::new(AtomicU64::new(0));
        let enumerated_for_thread = Arc::clone(&enumerated_count);

        let enumerator_fs = Arc::clone(&self.fs);
        let follow_reparse_points = options.follow_reparse_points;
        let enumerator = thread::spawn(move || {
            let phase_started = Instant::now();
            let mut enumerated = 0u64;
            let mut send = |item| {
                if cancellation.is_cancelled() {
                    return false;
                }
                enumerated += 1;
                enumerated_for_thread.store(enumerated, Ordering::Release);
                let sent = path_sender.send(item).is_ok();
                if let Some(sink) = &progress {
                    sink(ScanProgress {
                        scan_id: progress_id.clone(),
                        phase: "enumerate".into(),
                        enumerated,
                        processed: 0,
                        cache_hits: 0,
                        warnings: 0,
                        elapsed_ms: 0,
                        items_per_second: 0.0,
                        eta_seconds: None,
                    });
                }
                sent
            };
            (
                enumerator_fs.enumerate(&root, follow_reparse_points, &mut send),
                phase_started.elapsed().as_millis() as u64,
                enumerated,
            )
        });

        let mut workers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            workers.push(spawn_scan_worker(
                Arc::clone(&self.fs),
                Arc::clone(&self.metadata),
                Arc::clone(&self.store),
                path_receiver.clone(),
                result_sender.clone(),
                options.cancellation.clone(),
            ));
        }
        drop(result_sender);

        let mut batch = Vec::with_capacity(options.batch_size);
        let mut hits = 0;
        let mut warnings = 0;
        let mut total = 0;
        let mut tag_read_ms = 0;
        let mut db_write_ms = 0;
        consume_scan_results(
            &result_receiver,
            &mut batch,
            &mut hits,
            &mut warnings,
            &mut total,
            options.batch_size.max(1),
            |batch| {
                let started = Instant::now();
                let result = self.store.save_batch(&scan_id, batch);
                db_write_ms += started.elapsed().as_millis() as u64;
                result
            },
            |warning| self.store.save_scan_warning(&scan_id, warning),
            &options.cancellation,
            options.progress.as_ref(),
            &scan_id,
            &started,
            &mut tag_read_ms,
            &enumerated_count,
        )?;
        let (enumeration, enumerate_ms, enumerated) = enumerator
            .join()
            .map_err(|_| "scan enumerator panicked".to_string())?;
        if let Err(error) = enumeration {
            self.store.finish_scan(&scan_id, "failed", warnings)?;
            return Err(error);
        }
        for worker in workers {
            worker
                .join()
                .map_err(|_| "scan worker panicked".to_string())?;
        }
        if !batch.is_empty() {
            self.store.save_batch(&scan_id, &batch)?;
        }
        let status = if options.cancellation.is_cancelled() {
            "cancelled"
        } else {
            "completed"
        };
        self.store.finish_scan(&scan_id, status, warnings)?;
        self.store.record_metric(
            &scan_id,
            "scan",
            started.elapsed().as_millis() as u64,
            total,
        )?;
        self.store
            .record_metric(&scan_id, "enumerate", enumerate_ms, enumerated)?;
        self.store
            .record_metric(&scan_id, "tag_read", tag_read_ms, total)?;
        self.store
            .record_metric(&scan_id, "db_write", db_write_ms, total)?;
        Ok(ScanResult {
            scan_id,
            files: total,
            cache_hits: hits,
            warnings,
        })
    }
}

#[allow(clippy::large_enum_variant)]
enum ScanWorkerResult {
    Record {
        file: ScannedFile,
        cache_hit: bool,
        warning: bool,
        tag_read_ms: u64,
    },
    Warning(String),
}

fn spawn_scan_worker<
    F: FileSystem + 'static,
    M: MetadataReader + 'static,
    S: ScanStore + 'static,
>(
    fs: Arc<F>,
    metadata: Arc<M>,
    store: Arc<S>,
    paths: Receiver<Result<PathBuf, String>>,
    output: crossbeam_channel::Sender<ScanWorkerResult>,
    cancellation: CancellationToken,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while let Ok(path) = paths.recv() {
            if cancellation.is_cancelled() {
                break;
            }
            let path = match path {
                Ok(value) => value,
                Err(error) => {
                    let _ = output.send(ScanWorkerResult::Warning(error));
                    continue;
                }
            };
            let fingerprint = match fs.fingerprint(&path) {
                Ok(value) => value,
                Err(error) => {
                    let _ = output.send(ScanWorkerResult::Warning(error));
                    continue;
                }
            };
            let is_image = is_image_path(&path);
            let cached = if is_image {
                None
            } else {
                store.previous_metadata(&path, &fingerprint).ok().flatten()
            };
            let tag_started = Instant::now();
            let cache_hit = cached.is_some();
            let metadata = if is_image {
                None
            } else {
                cached.or_else(|| metadata.read(&path).ok())
            };
            let warning = !is_image && metadata.is_none();
            let _ = output.send(ScanWorkerResult::Record {
                file: ScannedFile {
                    id: Uuid::new_v4(),
                    path,
                    fingerprint,
                    metadata,
                    kind: if is_image {
                        FileKind::Image
                    } else {
                        FileKind::Music
                    },
                },
                cache_hit,
                warning,
                tag_read_ms: tag_started.elapsed().as_millis() as u64,
            });
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn consume_scan_results(
    results: &Receiver<ScanWorkerResult>,
    batch: &mut Vec<ScannedFile>,
    hits: &mut u64,
    warnings: &mut u64,
    total: &mut u64,
    batch_size: usize,
    mut flush: impl FnMut(&[ScannedFile]) -> Result<(), String>,
    mut save_warning: impl FnMut(&str) -> Result<(), String>,
    cancellation: &CancellationToken,
    progress: Option<&ProgressSink>,
    scan_id: &str,
    started: &Instant,
    tag_read_ms: &mut u64,
    enumerated_count: &AtomicU64,
) -> Result<(), String> {
    while let Ok(result) = results.recv() {
        match result {
            ScanWorkerResult::Warning(warning) => {
                *warnings += 1;
                save_warning(&warning)?;
            }
            ScanWorkerResult::Record {
                file,
                cache_hit,
                warning,
                tag_read_ms: elapsed,
            } => {
                *hits += u64::from(cache_hit);
                *warnings += u64::from(warning);
                *total += 1;
                *tag_read_ms += elapsed;
                batch.push(file);
            }
        }
        if batch.len() >= batch_size {
            flush(batch)?;
            batch.clear();
        }
        if let Some(sink) = progress {
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let items_per_second = if elapsed_ms == 0 {
                0.0
            } else {
                *total as f64 * 1000.0 / elapsed_ms as f64
            };
            let enumerated = enumerated_count.load(Ordering::Acquire);
            let eta_seconds = if items_per_second > 0.0 && enumerated > *total {
                Some(((enumerated - *total) as f64 / items_per_second).ceil() as u64)
            } else {
                None
            };
            sink(ScanProgress {
                scan_id: scan_id.into(),
                phase: "db_write".into(),
                enumerated,
                processed: *total,
                cache_hits: *hits,
                warnings: *warnings,
                elapsed_ms,
                items_per_second,
                eta_seconds,
            });
        }
        if cancellation.is_cancelled() {
            continue;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PlanOptions {
    pub target_root: PathBuf,
    pub batch_size: usize,
    pub naming: NamingRules,
}

#[derive(Debug, Clone)]
pub struct PlanResult {
    pub plan_id: String,
    pub items: u64,
    pub conflicts: u64,
    pub risks: u64,
}

pub struct PlanUseCase<S> {
    pub store: Arc<S>,
}

pub struct RevisePlanUseCase<S> {
    pub store: Arc<S>,
}
impl<S: PlanRevisionStore> RevisePlanUseCase<S> {
    pub fn execute(
        &self,
        parent_plan_id: &str,
        changes: &[ManualTargetChange],
    ) -> Result<String, String> {
        if changes.is_empty() {
            return Err("manual_target_change_required".into());
        }
        self.store.revise_plan(parent_plan_id, changes)
    }
}

impl<S: PlanStore> PlanUseCase<S> {
    pub fn execute(&self, scan_id: &str, options: &PlanOptions) -> Result<PlanResult, String> {
        let started = Instant::now();
        let files = self.store.load_completed_scan(scan_id)?;
        let plan_id = self
            .store
            .begin_plan(scan_id, &options.target_root, &options.naming)?;
        let mut music_targets = HashMap::<PathBuf, Vec<PathBuf>>::new();
        let mut items: Vec<PlanItem> = files
            .into_iter()
            .enumerate()
            .map(|(index, file)| {
                let item = if file.kind == FileKind::Music {
                    make_plan_item(
                        index as u64 + 1,
                        file,
                        &options.target_root,
                        &options.naming,
                    )
                } else {
                    skipped_plan_item(
                        index as u64 + 1,
                        file,
                        Risk::MetadataMissing,
                        "image_pending_anchor",
                    )
                };
                if item.file.kind == FileKind::Music {
                    if let Some(target) = &item.target {
                        let mut source = item.file.path.parent();
                        let target_parent = target.parent().map(Path::to_path_buf);
                        while let (Some(directory), Some(target_parent)) =
                            (source, target_parent.as_ref())
                        {
                            music_targets
                                .entry(directory.to_path_buf())
                                .or_default()
                                .push(target_parent.clone());
                            source = directory.parent();
                        }
                    }
                }
                item
            })
            .collect();
        for item in items
            .iter_mut()
            .filter(|item| item.file.kind == FileKind::Image)
        {
            let mut source = item.file.path.parent();
            let mut candidates = Vec::new();
            while let Some(directory) = source {
                if let Some(values) = music_targets.get(directory) {
                    candidates.extend(values.clone());
                    break;
                }
                source = directory.parent();
            }
            candidates.sort();
            candidates.dedup();
            match candidates.as_slice() {
                [parent] => {
                    let parent = parent.clone();
                    let name = item
                        .file
                        .path
                        .file_name()
                        .and_then(|v| v.to_str())
                        .unwrap_or("image");
                    item.target = Some(parent.join(sanitize_component(name)));
                    item.action = PlanAction::Move;
                    item.risk = Risk::None;
                    item.reason = None;
                }
                [] => {
                    item.risk = Risk::MetadataMissing;
                    item.reason = Some("companion_without_music".into());
                }
                _ => {
                    item.risk = Risk::Conflict;
                    item.reason = Some("companion_target_ambiguous".into());
                }
            }
        }
        resolve_duplicate_targets(&mut items, &options.naming);
        mark_target_conflicts(&mut items);
        let conflicts = items
            .iter()
            .filter(|item| item.risk == Risk::Conflict)
            .count() as u64;
        let risks = items.iter().filter(|item| item.risk != Risk::None).count() as u64;
        for batch in items.chunks(options.batch_size.max(1)) {
            self.store.save_plan_items(&plan_id, batch)?;
        }
        let snapshot_hash = plan_snapshot_hash(&items);
        self.store
            .finish_plan(&plan_id, conflicts, risks, &snapshot_hash)?;
        self.store.record_metric(
            &plan_id,
            "plan",
            started.elapsed().as_millis() as u64,
            items.len() as u64,
        )?;
        Ok(PlanResult {
            plan_id,
            items: items.len() as u64,
            conflicts,
            risks,
        })
    }
}

/// Stable digest over the fields which authorize a filesystem mutation.  UUIDs and
/// display-only metadata are intentionally excluded, so SQLite can recalculate it.
pub fn plan_snapshot_hash(items: &[PlanItem]) -> String {
    let mut digest = Sha256::new();
    for item in items {
        digest.update(item.ordinal.to_le_bytes());
        digest.update(item.file.path.to_string_lossy().as_bytes());
        digest.update([0]);
        if let Some(target) = &item.target {
            digest.update(target.to_string_lossy().as_bytes());
        }
        digest.update([0]);
        digest.update(match item.action {
            PlanAction::Move => b"move",
            PlanAction::Skip => b"skip",
        });
        digest.update([0]);
        let risk: &[u8] = match item.risk {
            Risk::None => b"none",
            Risk::InvalidTarget => b"invalid_target",
            Risk::PathTooLong => b"path_too_long",
            Risk::Conflict => b"conflict",
            Risk::MetadataMissing => b"metadata_missing",
        };
        digest.update(risk);
        digest.update([0]);
        if let Some(reason) = &item.reason {
            digest.update(reason.as_bytes());
        }
        digest.update([0xff]);
    }
    format!("{:x}", digest.finalize())
}

fn make_plan_item(
    ordinal: u64,
    file: ScannedFile,
    target_root: &Path,
    naming: &NamingRules,
) -> PlanItem {
    let Some(metadata) = &file.metadata else {
        return skipped_plan_item(ordinal, file, Risk::MetadataMissing, "metadata_missing");
    };
    if metadata
        .album_artist
        .as_deref()
        .or(metadata.artist.as_deref())
        .is_none()
    {
        return skipped_plan_item(ordinal, file, Risk::MetadataMissing, "artist_missing");
    }
    if metadata.album.is_none() {
        return skipped_plan_item(ordinal, file, Risk::MetadataMissing, "album_missing");
    }
    let source_stem = file
        .path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("_");
    let extension = file
        .path
        .extension()
        .and_then(|value| value.to_str())
        .map(|v| format!(".{v}"))
        .unwrap_or_default();
    let filename = if naming.use_source_filename {
        file.path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("_")
            .into()
    } else {
        render_template(&naming.filename_template, metadata, source_stem, &extension)
    };
    let target = target_root
        .join(sanitize_component(&render_template(
            &naming.artist_dir_template,
            metadata,
            source_stem,
            &extension,
        )))
        .join(sanitize_component(&render_template(
            &naming.album_dir_template,
            metadata,
            source_stem,
            &extension,
        )))
        .join(
            render_template(&naming.disc_dir_template, metadata, source_stem, &extension)
                .trim_matches([' ', '.']),
        )
        .join(sanitize_component(&filename));
    let (action, risk, reason) = if target == file.path {
        (
            PlanAction::Skip,
            Risk::InvalidTarget,
            Some("source_equals_target".into()),
        )
    } else if let Err(error) = assess_windows_path(&target) {
        (PlanAction::Skip, Risk::PathTooLong, Some(error.to_string()))
    } else {
        (PlanAction::Move, Risk::None, None)
    };
    PlanItem {
        id: Uuid::new_v4(),
        ordinal,
        file,
        target: Some(target),
        action,
        risk,
        reason,
    }
}

fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|v| v.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp")
    )
}

fn resolve_duplicate_targets(items: &mut [PlanItem], naming: &NamingRules) {
    let mut seen = HashMap::<String, u32>::new();
    for item in items.iter_mut().filter(|i| i.action == PlanAction::Move) {
        let Some(target) = item.target.clone() else {
            continue;
        };
        let key = target.to_string_lossy().to_lowercase();
        let count = seen.entry(key).or_insert(0);
        *count += 1;
        if *count > 1 {
            if naming.duplicate_suffix_template.is_empty() && item.file.kind != FileKind::Image {
                item.action = PlanAction::Skip;
                item.risk = Risk::Conflict;
                item.reason = Some("target_conflict".into());
                continue;
            }
            let suffix = if item.file.kind == FileKind::Image {
                format!("_{}", count)
            } else {
                let metadata = item.file.metadata.as_ref().expect("music metadata");
                render_template(
                    &naming.duplicate_suffix_template,
                    metadata,
                    item.file
                        .path
                        .file_stem()
                        .and_then(|v| v.to_str())
                        .unwrap_or("_"),
                    "",
                )
            };
            let next = target.with_file_name(format!(
                "{}{}{}",
                target.file_stem().and_then(|v| v.to_str()).unwrap_or("_"),
                if suffix.is_empty() {
                    format!("_{count}")
                } else {
                    suffix
                },
                target
                    .extension()
                    .and_then(|v| v.to_str())
                    .map(|v| format!(".{v}"))
                    .unwrap_or_default()
            ));
            item.target = Some(next);
        }
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use crate::{FileFingerprint, TrackMetadata};

    #[test]
    fn plan_snapshot_hash_changes_when_authorized_target_changes() {
        let file = ScannedFile {
            id: Uuid::nil(),
            path: PathBuf::from("C:/in/a.mp3"),
            fingerprint: FileFingerprint {
                size_bytes: 1,
                mtime_ns: 1,
            },
            metadata: Some(TrackMetadata {
                artist: None,
                album_artist: None,
                album: None,
                title: None,
                track_no: None,
                disc_no: None,
                year: None,
            }),
            kind: FileKind::Music,
        };
        let mut item = PlanItem {
            id: Uuid::nil(),
            ordinal: 1,
            file,
            target: Some(PathBuf::from("C:/out/a.mp3")),
            action: PlanAction::Move,
            risk: Risk::None,
            reason: None,
        };
        let before = plan_snapshot_hash(&[item.clone()]);
        item.target = Some(PathBuf::from("C:/out/b.mp3"));
        assert_ne!(before, plan_snapshot_hash(&[item]));
    }
}

fn skipped_plan_item(ordinal: u64, file: ScannedFile, risk: Risk, reason: &str) -> PlanItem {
    PlanItem {
        id: Uuid::new_v4(),
        ordinal,
        file,
        target: None,
        action: PlanAction::Skip,
        risk,
        reason: Some(reason.into()),
    }
}

fn mark_target_conflicts(items: &mut [PlanItem]) {
    let mut counts = HashMap::<String, usize>::new();
    for item in items.iter().filter(|item| item.action == PlanAction::Move) {
        if let Some(target) = &item.target {
            *counts
                .entry(target.to_string_lossy().to_lowercase())
                .or_default() += 1;
        }
    }
    for item in items.iter_mut() {
        let conflict = item.target.as_ref().is_some_and(|target| {
            counts
                .get(&target.to_string_lossy().to_lowercase())
                .copied()
                .unwrap_or_default()
                > 1
        });
        if conflict {
            item.action = PlanAction::Skip;
            item.risk = Risk::Conflict;
            item.reason = Some("target_conflict".into());
        }
    }
}

pub struct ApplyUseCase<S, F> {
    pub store: Arc<S>,
    pub files: Arc<F>,
}
pub struct ApplyResult {
    pub execution_id: String,
    pub success: u64,
    pub skipped: u64,
    pub failed: u64,
}
pub struct VerifyResult {
    pub verify_id: String,
    pub success: u64,
    pub failed: u64,
}
pub struct VerifyUseCase<S, F> {
    pub store: Arc<S>,
    pub files: Arc<F>,
}
impl<S: VerifyStore, F: FileMutator> VerifyUseCase<S, F> {
    pub fn execute(&self, execution_id: &str) -> Result<VerifyResult, String> {
        let started = Instant::now();
        let verify_id = self.store.begin_verify(execution_id)?;
        let (mut success, mut failed) = (0, 0);
        for item in self.store.load_successful_operations(execution_id)? {
            let expected = item.target.as_ref().is_some_and(|path| {
                self.files.exists(path)
                    && item
                        .expected_size
                        .is_none_or(|size| self.files.size(path).ok() == Some(size))
            }) && !self.files.exists(&item.source);
            if expected {
                success += 1;
                self.store
                    .save_verify_result(execution_id, &item.operation_id, "success", None)?;
            } else {
                failed += 1;
                self.store.save_verify_result(
                    execution_id,
                    &item.operation_id,
                    "failed",
                    Some("expected_move_state_not_found"),
                )?;
            }
        }
        let status = if failed == 0 { "completed" } else { "failed" };
        self.store
            .finish_verify(&verify_id, status, success, failed)?;
        self.store.record_metric(
            &verify_id,
            "verify",
            started.elapsed().as_millis() as u64,
            success + failed,
        )?;
        Ok(VerifyResult {
            verify_id,
            success,
            failed,
        })
    }
}
impl<S: ApplyStore, F: FileMutator> ApplyUseCase<S, F> {
    pub fn execute(&self, plan_id: &str, dry_run: bool) -> Result<ApplyResult, String> {
        let started = Instant::now();
        self.store.validate_plan_snapshot(plan_id)?;
        let already_done = if dry_run {
            Vec::new()
        } else {
            self.store.successful_plan_item_ids(plan_id)?
        };
        let execution_id = self.store.begin_execution(plan_id, dry_run)?;
        let (mut success, mut skipped, mut failed) = (0, 0, 0);
        for item in self.store.load_completed_plan(plan_id)? {
            let expected_size = self.files.size(&item.source).ok();
            let (action, result, error, source_deleted) =
                if already_done.iter().any(|id| id == &item.plan_item_id) {
                    skipped += 1;
                    (
                        "skip".into(),
                        "skipped".into(),
                        Some("already_applied_for_plan".into()),
                        false,
                    )
                } else if item.action == PlanAction::Skip || item.risk != Risk::None {
                    skipped += 1;
                    ("skip".into(), "skipped".into(), item.reason.clone(), false)
                } else if dry_run {
                    success += 1;
                    ("dry_run".into(), "success".into(), None, false)
                } else if item.target.is_none() || !self.files.exists(&item.source) {
                    failed += 1;
                    (
                        "move".into(),
                        "failed".into(),
                        Some("source_or_target_missing".into()),
                        false,
                    )
                } else {
                    let target = match item.target.as_ref() {
                        Some(target) => target,
                        None => unreachable!("target checked above"),
                    };
                    if self.files.exists(target) {
                        skipped += 1;
                        (
                            "skip".into(),
                            "skipped".into(),
                            Some("target_already_exists".into()),
                            false,
                        )
                    } else if self.files.same_volume(&item.source, target)? {
                        match self.files.move_file(&item.source, target) {
                            Ok(()) => {
                                success += 1;
                                ("move".into(), "success".into(), None, true)
                            }
                            Err(e) => {
                                failed += 1;
                                ("move".into(), "failed".into(), Some(e), false)
                            }
                        }
                    } else {
                        match self.files.copy_file(&item.source, target).and_then(|_| {
                            if self.files.size(&item.source)? == self.files.size(target)? {
                                Ok(())
                            } else {
                                Err("cross_volume_verify_failed".into())
                            }
                        }) {
                            Ok(()) => match self.files.delete_file(&item.source) {
                                Ok(()) => {
                                    success += 1;
                                    ("copy_delete".into(), "success".into(), None, true)
                                }
                                Err(error) => {
                                    failed += 1;
                                    ("copy".into(), "failed".into(), Some(error), false)
                                }
                            },
                            Err(error) => {
                                failed += 1;
                                ("copy".into(), "failed".into(), Some(error), false)
                            }
                        }
                    }
                };
            self.store.save_operation(
                &execution_id,
                &OperationLog {
                    plan_item_id: item.plan_item_id,
                    sequence_no: item.ordinal,
                    source: item.source,
                    target: item.target,
                    action,
                    result,
                    error,
                    source_deleted,
                    expected_size,
                },
            )?;
        }
        let status = if failed == 0 { "completed" } else { "partial" };
        self.store
            .finish_execution(&execution_id, status, success, skipped, failed)?;
        self.store.record_metric(
            &execution_id,
            "apply",
            started.elapsed().as_millis() as u64,
            success + skipped + failed,
        )?;
        Ok(ApplyResult {
            execution_id,
            success,
            skipped,
            failed,
        })
    }
}

#[cfg(test)]
mod safety_workflow_tests {
    use super::*;
    use crate::ports::{ApplyStore, FileMutator, RollbackStore};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FailingCrossVolume {
        deleted: Mutex<Vec<PathBuf>>,
        calls: Mutex<Vec<String>>,
    }
    impl FileMutator for FailingCrossVolume {
        fn exists(&self, path: &Path) -> bool {
            path == Path::new("source")
                || (path != Path::new("target") && path.to_string_lossy().starts_with("target"))
        }
        fn same_volume(&self, _: &Path, _: &Path) -> Result<bool, String> {
            Ok(false)
        }
        fn move_file(&self, source: &Path, target: &Path) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}->{}", source.display(), target.display()));
            Ok(())
        }
        fn copy_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn size(&self, path: &Path) -> Result<u64, String> {
            Ok(if path == Path::new("source") { 10 } else { 9 })
        }
        fn delete_file(&self, path: &Path) -> Result<(), String> {
            self.deleted.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }
    }

    struct OneApplyStore;
    impl ApplyStore for OneApplyStore {
        fn load_completed_plan(&self, _: &str) -> Result<Vec<crate::ApplyItem>, String> {
            Ok(vec![crate::ApplyItem {
                plan_item_id: "item".into(),
                ordinal: 1,
                source: "source".into(),
                target: Some("target".into()),
                action: PlanAction::Move,
                risk: Risk::None,
                reason: None,
            }])
        }
        fn validate_plan_snapshot(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn successful_plan_item_ids(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn begin_execution(&self, _: &str, _: bool) -> Result<String, String> {
            Ok("execution".into())
        }
        fn save_operation(&self, _: &str, _: &OperationLog) -> Result<(), String> {
            Ok(())
        }
        fn finish_execution(&self, _: &str, _: &str, _: u64, _: u64, _: u64) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn cross_volume_verification_failure_never_deletes_source() {
        let files = Arc::new(FailingCrossVolume::default());
        let result = ApplyUseCase {
            store: Arc::new(OneApplyStore),
            files: Arc::clone(&files),
        }
        .execute("plan", false)
        .unwrap();
        assert_eq!(result.failed, 1);
        assert!(files.deleted.lock().unwrap().is_empty());
    }

    struct ReverseStore {
        saved: Mutex<Vec<String>>,
    }
    impl RollbackStore for ReverseStore {
        fn begin_rollback(&self, _: &str, _: bool) -> Result<String, String> {
            Ok("rollback".into())
        }
        fn load_rollback_items(&self, _: &str) -> Result<Vec<crate::VerifyItem>, String> {
            Ok(vec![1, 3, 2]
                .into_iter()
                .map(|n| crate::VerifyItem {
                    operation_id: n.to_string(),
                    sequence_no: n,
                    source: format!("source{n}").into(),
                    target: Some(format!("target{n}").into()),
                    action: "move".into(),
                    expected_size: Some(10),
                })
                .collect())
        }
        fn save_rollback_result(
            &self,
            _: &str,
            operation_id: &str,
            _: &str,
            _: Option<&str>,
        ) -> Result<(), String> {
            self.saved.lock().unwrap().push(operation_id.into());
            Ok(())
        }
        fn finish_rollback(&self, _: &str, _: &str, _: u64, _: u64, _: u64) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn rollback_processes_successful_operations_in_reverse_sequence() {
        let store = Arc::new(ReverseStore {
            saved: Mutex::new(vec![]),
        });
        let files = Arc::new(FailingCrossVolume::default());
        RollbackUseCase {
            store: Arc::clone(&store),
            files,
        }
        .execute("execution", true)
        .unwrap();
        assert_eq!(*store.saved.lock().unwrap(), ["3", "2", "1"]);
    }
}
