#![allow(clippy::items_after_test_module)]

use crate::{
    assess_windows_path,
    ports::{
        ApplyStore, FileMutator, FileSystem, ManualTargetChange, MetadataReader, PlanRevisionStore,
        PlanStore, RollbackStore, ScanStore, VerifyStore,
    },
    render_template, sanitize_component, windows_path_key, DuplicateStrategy, FileKind,
    NamingRules, OperationAction, OperationLog, OperationResult, PlanAction, PlanConflictCandidate,
    PlanItem, Risk, RunStatus, ScannedFile, TrackMetadata, WorkflowResult,
};
use crossbeam_channel::{bounded, Receiver};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    ffi::OsStr,
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
    pub fn execute(&self, execution_id: &str, dry_run: bool) -> WorkflowResult<RollbackResult> {
        let started = Instant::now();
        let rollback_id = self.store.begin_rollback(execution_id, dry_run)?;
        let mut items = match self.store.load_rollback_items(execution_id) {
            Ok(items) => items,
            Err(error) => {
                let _ = self
                    .store
                    .finish_rollback(&rollback_id, RunStatus::Failed, 0, 0, 0);
                return Err(error.into());
            }
        };
        items.sort_by_key(|item| std::cmp::Reverse(item.sequence_no));
        let (mut success, mut skipped, mut failed) = (0, 0, 0);
        for item in items {
            let (result, error) = match item.target.as_ref() {
                None => {
                    skipped += 1;
                    (OperationResult::Skipped, Some("target_missing_in_log"))
                }
                Some(t) if !self.files.exists(t) => {
                    failed += 1;
                    (OperationResult::Failed, Some("target_missing"))
                }
                Some(t)
                    if item
                        .expected_size
                        .is_some_and(|size| self.files.size(t).ok() != Some(size)) =>
                {
                    failed += 1;
                    (OperationResult::Failed, Some("target_changed_since_apply"))
                }
                Some(t) if item.action == OperationAction::CopySourceRetained => {
                    let source_matches = self.files.exists(&item.source)
                        && item
                            .expected_size
                            .is_none_or(|size| self.files.size(&item.source).ok() == Some(size));
                    if !source_matches {
                        failed += 1;
                        (
                            OperationResult::Failed,
                            Some("source_changed_after_partial_copy"),
                        )
                    } else if dry_run {
                        success += 1;
                        (OperationResult::Success, None)
                    } else {
                        match self.files.delete_file(t) {
                            Ok(()) => {
                                success += 1;
                                (OperationResult::Success, None)
                            }
                            Err(_) => {
                                failed += 1;
                                (OperationResult::Failed, Some("partial_copy_cleanup_failed"))
                            }
                        }
                    }
                }
                Some(_) if self.files.exists(&item.source) => {
                    skipped += 1;
                    (OperationResult::Skipped, Some("source_already_exists"))
                }
                Some(_) if dry_run => {
                    success += 1;
                    (OperationResult::Success, None)
                }
                Some(t) if item.action == OperationAction::Move => {
                    match self.files.move_file(t, &item.source) {
                        Ok(()) => {
                            success += 1;
                            (OperationResult::Success, None)
                        }
                        Err(_) => {
                            failed += 1;
                            (OperationResult::Failed, Some("reverse_move_failed"))
                        }
                    }
                }
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
                            (OperationResult::Success, None)
                        }
                        Err(_) => {
                            failed += 1;
                            (
                                OperationResult::Failed,
                                Some("reverse_target_delete_failed"),
                            )
                        }
                    },
                    Err(_) => {
                        failed += 1;
                        (OperationResult::Failed, Some("reverse_copy_verify_failed"))
                    }
                },
            };
            self.store
                .save_rollback_result(execution_id, &item.operation_id, result, error)?;
        }
        let status = if failed == 0 {
            RunStatus::Completed
        } else {
            RunStatus::Partial
        };
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
    pub fn execute(&self, root: &Path, options: &ScanOptions) -> WorkflowResult<ScanResult> {
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
        if let Err(error) = consume_scan_results(
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
        ) {
            options.cancellation.cancel();
            let _ = self
                .store
                .finish_scan(&scan_id, RunStatus::Failed, warnings);
            return Err(error.into());
        }
        let (enumeration, enumerate_ms, enumerated) = match enumerator.join() {
            Ok(value) => value,
            Err(_) => {
                let _ = self
                    .store
                    .finish_scan(&scan_id, RunStatus::Failed, warnings);
                return Err("scan enumerator panicked".into());
            }
        };
        if let Err(error) = enumeration {
            self.store
                .finish_scan(&scan_id, RunStatus::Failed, warnings)?;
            return Err(error.into());
        }
        for worker in workers {
            if worker.join().is_err() {
                let _ = self
                    .store
                    .finish_scan(&scan_id, RunStatus::Failed, warnings);
                return Err("scan worker panicked".into());
            }
        }
        if !batch.is_empty() {
            if let Err(error) = self.store.save_batch(&scan_id, &batch) {
                let _ = self
                    .store
                    .finish_scan(&scan_id, RunStatus::Failed, warnings);
                return Err(error.into());
            }
        }
        let status = if options.cancellation.is_cancelled() {
            RunStatus::Cancelled
        } else {
            RunStatus::Completed
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
                match store.previous_metadata(&path, &fingerprint) {
                    Ok(value) => value,
                    Err(error) => {
                        let warning =
                            format!("metadata_cache_read_failed:{}:{error}", path.display());
                        let _ = output.send(ScanWorkerResult::Warning(warning));
                        None
                    }
                }
            };
            let tag_started = Instant::now();
            let cache_hit = cached.is_some();
            let mut read_error_reported = false;
            let metadata = if is_image {
                None
            } else if cached.is_some() {
                cached
            } else {
                match metadata.read(&path) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        read_error_reported = true;
                        let warning = format!("metadata_read_failed:{}:{error}", path.display());
                        let _ = output.send(ScanWorkerResult::Warning(warning));
                        None
                    }
                }
            };
            let warning = !is_image && metadata.is_none() && !read_error_reported;
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

#[derive(Clone)]
struct MusicImageAnchor {
    target_directory: PathBuf,
    disc_parent: Option<PathBuf>,
    music_item_id: Uuid,
}
impl<S: PlanRevisionStore> RevisePlanUseCase<S> {
    pub fn execute(
        &self,
        parent_plan_id: &str,
        changes: &[ManualTargetChange],
    ) -> WorkflowResult<String> {
        if changes.is_empty() {
            return Err("manual_target_change_required".into());
        }
        Ok(self.store.revise_plan(parent_plan_id, changes)?)
    }
}

impl<S: PlanStore> PlanUseCase<S> {
    pub fn execute(&self, scan_id: &str, options: &PlanOptions) -> WorkflowResult<PlanResult> {
        let issues = crate::validate_naming_rules(&options.naming);
        if !issues.is_empty() {
            return Err(format!("invalid_naming_rules:{}", issues[0].code).into());
        }
        let started = Instant::now();
        let files = self.store.load_completed_scan(scan_id)?;
        let plan_id = self
            .store
            .begin_plan(scan_id, &options.target_root, &options.naming)?;
        let mut music_targets = HashMap::<PathBuf, Vec<MusicImageAnchor>>::new();
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
                        let disc_parent = target_parent.as_deref().and_then(|directory| {
                            disc_parent_for_music_item(directory, &item.file, &options.naming)
                        });
                        while let (Some(directory), Some(target_parent)) =
                            (source, target_parent.as_ref())
                        {
                            music_targets
                                .entry(directory.to_path_buf())
                                .or_default()
                                .push(MusicImageAnchor {
                                    target_directory: target_parent.clone(),
                                    disc_parent: disc_parent.clone(),
                                    music_item_id: item.id,
                                });
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
            let mut candidate_items = Vec::new();
            while let Some(directory) = source {
                if let Some(values) = music_targets.get(directory) {
                    candidate_items.extend(values.clone());
                    break;
                }
                source = directory.parent();
            }
            let candidates = image_destination_candidates(candidate_items);
            match candidates.as_slice() {
                [candidate] => {
                    let parent = candidate.target_directory.clone();
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
                    item.conflict_group_id = Some(Uuid::new_v4());
                    item.conflict_candidates = candidates;
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
            if let Err(error) = self.store.save_plan_items(&plan_id, batch) {
                let _ = self.store.fail_plan(&plan_id);
                return Err(error.into());
            }
        }
        let snapshot_hash = plan_snapshot_hash(&items);
        if let Err(error) = self
            .store
            .finish_plan(&plan_id, conflicts, risks, &snapshot_hash)
        {
            let _ = self.store.fail_plan(&plan_id);
            return Err(error.into());
        }
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

fn disc_parent_for_music_item(
    target_directory: &Path,
    file: &ScannedFile,
    naming: &NamingRules,
) -> Option<PathBuf> {
    let metadata = file.metadata.as_ref()?;
    let source_stem = file
        .path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("_");
    let extension = file
        .path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let disc_component =
        render_template(&naming.disc_dir_template, metadata, source_stem, &extension);
    let disc_component = disc_component.trim_matches([' ', '.']);
    if disc_component.is_empty() || target_directory.file_name() != Some(OsStr::new(disc_component))
    {
        return None;
    }
    target_directory.parent().map(Path::to_path_buf)
}

fn image_destination_candidates(mut anchors: Vec<MusicImageAnchor>) -> Vec<PlanConflictCandidate> {
    anchors.sort_by(|left, right| {
        left.target_directory
            .cmp(&right.target_directory)
            .then(left.music_item_id.cmp(&right.music_item_id))
    });
    anchors.dedup_by(|left, right| {
        left.target_directory == right.target_directory && left.music_item_id == right.music_item_id
    });

    let mut candidates = Vec::<PlanConflictCandidate>::new();
    for anchor in &anchors {
        if let Some(candidate) = candidates
            .iter_mut()
            .find(|candidate| candidate.target_directory == anchor.target_directory)
        {
            candidate.music_item_ids.push(anchor.music_item_id);
        } else {
            candidates.push(PlanConflictCandidate {
                target_directory: anchor.target_directory.clone(),
                music_item_ids: vec![anchor.music_item_id],
            });
        }
    }

    if candidates.len() <= 1 {
        return candidates;
    }
    let Some(common_parent) = anchors
        .first()
        .and_then(|anchor| anchor.disc_parent.clone())
    else {
        return candidates;
    };
    if anchors.iter().any(|anchor| {
        anchor.disc_parent.as_ref() != Some(&common_parent)
            || anchor.target_directory.parent() != Some(common_parent.as_path())
    }) {
        return candidates;
    }

    let mut music_item_ids = anchors
        .into_iter()
        .map(|anchor| anchor.music_item_id)
        .collect::<Vec<_>>();
    music_item_ids.sort();
    music_item_ids.dedup();
    vec![PlanConflictCandidate {
        target_directory: common_parent,
        music_item_ids,
    }]
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
    let metadata_unreadable = file.metadata.is_none();
    let mut metadata = file.metadata.clone().unwrap_or(TrackMetadata {
        artist: None,
        album_artist: None,
        album: None,
        title: None,
        track_no: None,
        disc_no: None,
        year: None,
    });
    let artist_missing = metadata
        .album_artist
        .as_deref()
        .or(metadata.artist.as_deref())
        .is_none();
    let album_missing = metadata.album.is_none();
    let missing_reason = if metadata_unreadable {
        Some("metadata_missing")
    } else if artist_missing && album_missing {
        Some("artist_album_missing")
    } else if artist_missing {
        Some("artist_missing")
    } else if album_missing {
        Some("album_missing")
    } else {
        None
    };
    if let Some(reason) = missing_reason.filter(|_| !naming.allow_missing_metadata) {
        return skipped_plan_item(ordinal, file, Risk::MetadataMissing, reason);
    }
    if artist_missing {
        metadata.artist = Some("Unknown Artist".into());
        metadata.album_artist = Some("Unknown Artist".into());
    }
    if album_missing {
        metadata.album = Some("Unknown Album".into());
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
    let filename = if naming.use_source_filename || metadata_unreadable {
        file.path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("_")
            .into()
    } else {
        render_template(
            &naming.filename_template,
            &metadata,
            source_stem,
            &extension,
        )
    };
    let target = target_root
        .join(sanitize_component(&render_template(
            &naming.artist_dir_template,
            &metadata,
            source_stem,
            &extension,
        )))
        .join(sanitize_component(&render_template(
            &naming.album_dir_template,
            &metadata,
            source_stem,
            &extension,
        )))
        .join(
            render_template(
                &naming.disc_dir_template,
                &metadata,
                source_stem,
                &extension,
            )
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
        (
            PlanAction::Skip,
            Risk::PathTooLong,
            Some(error.reason_code()),
        )
    } else {
        (
            PlanAction::Move,
            if missing_reason.is_some() {
                Risk::MetadataMissing
            } else {
                Risk::None
            },
            missing_reason.map(str::to_owned),
        )
    };
    PlanItem {
        id: Uuid::new_v4(),
        conflict_group_id: None,
        ordinal,
        file,
        target: Some(target),
        action,
        risk,
        reason,
        conflict_candidates: Vec::new(),
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
        let key = windows_path_key(&target);
        let count = seen.entry(key).or_insert(0);
        *count += 1;
        if *count > 1 {
            if (naming.duplicate_strategy == DuplicateStrategy::Skip
                || (naming.duplicate_strategy == DuplicateStrategy::Legacy
                    && naming.duplicate_suffix_template.is_empty()))
                && item.file.kind != FileKind::Image
            {
                // Keep every candidate movable until `mark_target_conflicts` so
                // all sides of the collision receive the same diagnostic group.
                continue;
            }
            let suffix = if item.file.kind == FileKind::Image
                || naming.duplicate_strategy == DuplicateStrategy::Sequence
            {
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
            conflict_group_id: None,
            ordinal: 1,
            file,
            target: Some(PathBuf::from("C:/out/a.mp3")),
            action: PlanAction::Move,
            risk: Risk::None,
            reason: None,
            conflict_candidates: Vec::new(),
        };
        let before = plan_snapshot_hash(&[item.clone()]);
        item.target = Some(PathBuf::from("C:/out/b.mp3"));
        assert_ne!(before, plan_snapshot_hash(&[item]));
    }

    #[test]
    fn plan_snapshot_hash_ignores_diagnostic_conflict_candidates() {
        let file = ScannedFile {
            id: Uuid::nil(),
            path: PathBuf::from("C:/in/cover.jpg"),
            fingerprint: FileFingerprint {
                size_bytes: 1,
                mtime_ns: 1,
            },
            metadata: None,
            kind: FileKind::Image,
        };
        let mut item = PlanItem {
            id: Uuid::nil(),
            conflict_group_id: Some(Uuid::nil()),
            ordinal: 1,
            file,
            target: None,
            action: PlanAction::Skip,
            risk: Risk::Conflict,
            reason: Some("companion_target_ambiguous".into()),
            conflict_candidates: Vec::new(),
        };
        let before = plan_snapshot_hash(&[item.clone()]);
        item.conflict_candidates.push(PlanConflictCandidate {
            target_directory: PathBuf::from("C:/out/Album"),
            music_item_ids: vec![Uuid::new_v4()],
        });
        assert_eq!(before, plan_snapshot_hash(&[item]));
    }

    #[test]
    fn missing_metadata_is_skipped_unless_explicitly_allowed() {
        let file = ScannedFile {
            id: Uuid::nil(),
            path: PathBuf::from("C:/in/song.mp3"),
            fingerprint: FileFingerprint {
                size_bytes: 1,
                mtime_ns: 1,
            },
            metadata: None,
            kind: FileKind::Music,
        };
        let skipped = make_plan_item(
            1,
            file.clone(),
            Path::new("C:/out"),
            &NamingRules::default(),
        );
        assert_eq!(skipped.action, PlanAction::Skip);
        assert_eq!(skipped.risk, Risk::MetadataMissing);
        assert!(skipped.target.is_none());

        let allowed = make_plan_item(
            1,
            file,
            Path::new("C:/out"),
            &NamingRules {
                allow_missing_metadata: true,
                ..NamingRules::default()
            },
        );
        assert_eq!(allowed.action, PlanAction::Move);
        assert_eq!(allowed.risk, Risk::MetadataMissing);
        assert_eq!(
            allowed.target,
            Some(PathBuf::from(
                "C:/out/Unknown Artist/Unknown Album/song.mp3"
            ))
        );
    }

    #[test]
    fn image_candidates_collapse_only_proven_disc_directories() {
        let album = PathBuf::from("C:/out/Artist/Album");
        let collapsed = image_destination_candidates(vec![
            MusicImageAnchor {
                target_directory: album.join("Disc 01"),
                disc_parent: Some(album.clone()),
                music_item_id: Uuid::from_u128(1),
            },
            MusicImageAnchor {
                target_directory: album.join("Disc 02"),
                disc_parent: Some(album.clone()),
                music_item_id: Uuid::from_u128(2),
            },
        ]);
        assert_eq!(collapsed.len(), 1);
        assert_eq!(collapsed[0].target_directory, album);
        assert_eq!(collapsed[0].music_item_ids.len(), 2);

        let ambiguous = image_destination_candidates(vec![
            MusicImageAnchor {
                target_directory: PathBuf::from("C:/out/Artist/Album A/01"),
                disc_parent: Some(PathBuf::from("C:/out/Artist/Album A")),
                music_item_id: Uuid::from_u128(1),
            },
            MusicImageAnchor {
                target_directory: PathBuf::from("C:/out/Artist/Album B/01"),
                disc_parent: Some(PathBuf::from("C:/out/Artist/Album B")),
                music_item_id: Uuid::from_u128(2),
            },
        ]);
        assert_eq!(ambiguous.len(), 2);
    }

    #[test]
    fn disc_parent_requires_a_nonempty_rendered_disc_component() {
        let file = ScannedFile {
            id: Uuid::nil(),
            path: PathBuf::from("C:/in/song.flac"),
            fingerprint: FileFingerprint {
                size_bytes: 1,
                mtime_ns: 1,
            },
            metadata: Some(TrackMetadata {
                artist: Some("Artist".into()),
                album_artist: Some("Artist".into()),
                album: Some("Album".into()),
                title: Some("Song".into()),
                track_no: Some(1),
                disc_no: Some(2),
                year: None,
            }),
            kind: FileKind::Music,
        };
        let custom = NamingRules {
            disc_dir_template: "Disc {disc_no:02d}".into(),
            ..NamingRules::default()
        };
        assert_eq!(
            disc_parent_for_music_item(Path::new("C:/out/Artist/Album/Disc 02"), &file, &custom),
            Some(PathBuf::from("C:/out/Artist/Album"))
        );
        assert_eq!(
            disc_parent_for_music_item(
                Path::new("C:/out/Artist/Album"),
                &file,
                &NamingRules {
                    disc_dir_template: String::new(),
                    ..NamingRules::default()
                }
            ),
            None
        );
    }
}

fn skipped_plan_item(ordinal: u64, file: ScannedFile, risk: Risk, reason: &str) -> PlanItem {
    PlanItem {
        id: Uuid::new_v4(),
        conflict_group_id: None,
        ordinal,
        file,
        target: None,
        action: PlanAction::Skip,
        risk,
        reason: Some(reason.into()),
        conflict_candidates: Vec::new(),
    }
}

fn mark_target_conflicts(items: &mut [PlanItem]) {
    let mut groups = HashMap::<String, (usize, Uuid)>::new();
    for item in items.iter().filter(|item| item.action == PlanAction::Move) {
        if let Some(target) = &item.target {
            let entry = groups
                .entry(windows_path_key(target))
                .or_insert_with(|| (0, Uuid::new_v4()));
            entry.0 += 1;
        }
    }
    for item in items.iter_mut() {
        let group = item.target.as_ref().and_then(|target| {
            groups
                .get(&windows_path_key(target))
                .filter(|(count, _)| *count > 1)
                .map(|(_, id)| *id)
        });
        if let Some(group_id) = group {
            item.conflict_group_id = Some(group_id);
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
    pub fn execute(&self, execution_id: &str) -> WorkflowResult<VerifyResult> {
        let started = Instant::now();
        let verify_id = self.store.begin_verify(execution_id)?;
        let (mut success, mut failed) = (0, 0);
        let items = match self.store.load_successful_operations(execution_id) {
            Ok(items) => items,
            Err(error) => {
                let _ = self
                    .store
                    .finish_verify(&verify_id, RunStatus::Failed, success, failed);
                return Err(error.into());
            }
        };
        for item in items {
            let expected = item.target.as_ref().is_some_and(|path| {
                self.files.exists(path)
                    && item
                        .expected_size
                        .is_none_or(|size| self.files.size(path).ok() == Some(size))
            }) && !self.files.exists(&item.source);
            if expected {
                success += 1;
                self.store.save_verify_result(
                    execution_id,
                    &item.operation_id,
                    OperationResult::Success,
                    None,
                )?;
            } else {
                failed += 1;
                self.store.save_verify_result(
                    execution_id,
                    &item.operation_id,
                    OperationResult::Failed,
                    Some("expected_move_state_not_found"),
                )?;
            }
        }
        let status = if failed == 0 {
            RunStatus::Completed
        } else {
            RunStatus::Failed
        };
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
    pub fn execute(&self, plan_id: &str, dry_run: bool) -> WorkflowResult<ApplyResult> {
        let started = Instant::now();
        self.store.validate_plan_snapshot(plan_id)?;
        let already_done = if dry_run {
            Vec::new()
        } else {
            self.store.successful_plan_item_ids(plan_id)?
        };
        let execution_id = self.store.begin_execution(plan_id, dry_run)?;
        let (mut success, mut skipped, mut failed) = (0, 0, 0);
        let items = match self.store.load_completed_plan(plan_id) {
            Ok(items) => items,
            Err(error) => {
                let _ = self.store.finish_execution(
                    &execution_id,
                    RunStatus::Failed,
                    success,
                    skipped,
                    failed,
                );
                return Err(error.into());
            }
        };
        for item in items {
            let expected_size = self.files.size(&item.source).ok();
            let (action, result, error, source_deleted) =
                if already_done.iter().any(|id| id == &item.plan_item_id) {
                    skipped += 1;
                    (
                        OperationAction::Skip,
                        OperationResult::Skipped,
                        Some("already_applied_for_plan".into()),
                        false,
                    )
                } else if item.action == PlanAction::Skip || item.risk != Risk::None {
                    skipped += 1;
                    (
                        OperationAction::Skip,
                        OperationResult::Skipped,
                        item.reason.clone(),
                        false,
                    )
                } else if dry_run {
                    success += 1;
                    (
                        OperationAction::DryRun,
                        OperationResult::Success,
                        None,
                        false,
                    )
                } else if item.target.is_none() || !self.files.exists(&item.source) {
                    failed += 1;
                    (
                        OperationAction::Move,
                        OperationResult::Failed,
                        Some("source_or_target_missing".into()),
                        false,
                    )
                } else {
                    let target = match item.target.as_ref() {
                        Some(target) => target,
                        None => unreachable!("target checked above"),
                    };
                    let same_volume = self.files.same_volume(&item.source, target);
                    if self.files.exists(target) {
                        skipped += 1;
                        (
                            OperationAction::Skip,
                            OperationResult::Skipped,
                            Some("target_already_exists".into()),
                            false,
                        )
                    } else if let Err(error) = same_volume.as_ref() {
                        failed += 1;
                        (
                            OperationAction::Move,
                            OperationResult::Failed,
                            Some(error.clone()),
                            false,
                        )
                    } else if same_volume == Ok(true) {
                        match self.files.move_file(&item.source, target) {
                            Ok(()) => {
                                success += 1;
                                (OperationAction::Move, OperationResult::Success, None, true)
                            }
                            Err(e) => {
                                failed += 1;
                                (
                                    OperationAction::Move,
                                    OperationResult::Failed,
                                    Some(e),
                                    false,
                                )
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
                                    (
                                        OperationAction::CopyDelete,
                                        OperationResult::Success,
                                        None,
                                        true,
                                    )
                                }
                                Err(error) => {
                                    failed += 1;
                                    (
                                        OperationAction::CopySourceRetained,
                                        OperationResult::Failed,
                                        Some(error),
                                        false,
                                    )
                                }
                            },
                            Err(error) => {
                                failed += 1;
                                (
                                    OperationAction::CopySourceRetained,
                                    OperationResult::Failed,
                                    Some(error),
                                    false,
                                )
                            }
                        }
                    }
                };
            if let Err(save_error) = self.store.save_operation(
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
            ) {
                let _ = self.store.finish_execution(
                    &execution_id,
                    RunStatus::Partial,
                    success,
                    skipped,
                    failed + 1,
                );
                return Err(save_error.into());
            }
        }
        let status = if failed == 0 {
            RunStatus::Completed
        } else {
            RunStatus::Partial
        };
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
        fn finish_execution(
            &self,
            _: &str,
            _: RunStatus,
            _: u64,
            _: u64,
            _: u64,
        ) -> Result<(), String> {
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
                    action: OperationAction::Move,
                    expected_size: None,
                })
                .collect())
        }
        fn save_rollback_result(
            &self,
            _: &str,
            operation_id: &str,
            _: OperationResult,
            _: Option<&str>,
        ) -> Result<(), String> {
            self.saved.lock().unwrap().push(operation_id.into());
            Ok(())
        }
        fn finish_rollback(
            &self,
            _: &str,
            _: RunStatus,
            _: u64,
            _: u64,
            _: u64,
        ) -> Result<(), String> {
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

    struct MismatchRollbackStore;
    impl RollbackStore for MismatchRollbackStore {
        fn begin_rollback(&self, _: &str, _: bool) -> Result<String, String> {
            Ok("rollback".into())
        }
        fn load_rollback_items(&self, _: &str) -> Result<Vec<crate::VerifyItem>, String> {
            Ok(vec![crate::VerifyItem {
                operation_id: "operation".into(),
                sequence_no: 1,
                source: "source".into(),
                target: Some("target".into()),
                action: OperationAction::Move,
                expected_size: Some(10),
            }])
        }
        fn save_rollback_result(
            &self,
            _: &str,
            _: &str,
            result: OperationResult,
            error: Option<&str>,
        ) -> Result<(), String> {
            assert_eq!(result, OperationResult::Failed);
            assert_eq!(error, Some("target_changed_since_apply"));
            Ok(())
        }
        fn finish_rollback(
            &self,
            _: &str,
            _: RunStatus,
            _: u64,
            _: u64,
            _: u64,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct MismatchFiles {
        mutations: Mutex<Vec<String>>,
    }
    impl FileMutator for MismatchFiles {
        fn exists(&self, path: &Path) -> bool {
            path == Path::new("target")
        }
        fn same_volume(&self, _: &Path, _: &Path) -> Result<bool, String> {
            Ok(true)
        }
        fn move_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            self.mutations.lock().unwrap().push("move".into());
            Ok(())
        }
        fn copy_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            self.mutations.lock().unwrap().push("copy".into());
            Ok(())
        }
        fn size(&self, _: &Path) -> Result<u64, String> {
            Ok(9)
        }
        fn delete_file(&self, _: &Path) -> Result<(), String> {
            self.mutations.lock().unwrap().push("delete".into());
            Ok(())
        }
    }

    #[test]
    fn rollback_does_not_mutate_a_target_that_changed_after_apply() {
        let files = Arc::new(MismatchFiles::default());
        let result = RollbackUseCase {
            store: Arc::new(MismatchRollbackStore),
            files: Arc::clone(&files),
        }
        .execute("execution", false)
        .unwrap();
        assert_eq!(result.failed, 1);
        assert!(files.mutations.lock().unwrap().is_empty());
    }
}
