//! Read-only library diagnosis, independent of organizing plans and adapters.
use crate::{
    ports::{FileSystem, MetadataReader, ScanStore},
    usecases::{CancellationToken, ScanOptions, ScanUseCase},
    FileKind, RunStatus, ScannedFile, WorkflowResult,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

pub const RULE_VERSION: u32 = 1;
pub const ISSUE_CODES: &[&str] = &[
    "read_failed",
    "file_changed",
    "scan_warning",
    "missing_title",
    "missing_artist",
    "missing_album",
    "missing_album_artist",
    "missing_track",
    "missing_disc",
    "missing_year",
    "missing_artwork",
    "artist_variant",
    "album_variant",
    "exact_duplicate",
    "same_file_paths",
    "track_gap",
    "track_duplicate",
    "disc_inconsistent",
    "album_artist_inconsistent",
    "year_inconsistent",
    "genre_inconsistent",
    "format_mixed",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorIssue {
    pub code: String,
    pub severity: Severity,
    pub category: String,
    pub file_ids: Vec<Uuid>,
    pub evidence: Vec<String>,
    pub comparison: Option<String>,
    pub rule_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorRun {
    #[serde(rename = "doctor_run_id")]
    pub id: String,
    #[serde(rename = "scan_run_id")]
    pub scan_id: String,
    pub status: RunStatus,
    pub rule_version: u32,
    pub files: u64,
    pub cache_hits: u64,
    pub failures: u64,
    pub issue_count: u64,
    pub error: Option<String>,
}

pub trait DoctorStore: ScanStore {
    fn begin_doctor(&self, scan_id: &str) -> Result<DoctorRun, String>;
    fn doctor_files(&self, scan_id: &str) -> Result<Vec<ScannedFile>, String>;
    fn doctor_warnings(&self, scan_id: &str) -> Result<Vec<String>, String>;
    fn finish_doctor(&self, run: &DoctorRun, issues: &[DoctorIssue]) -> Result<(), String>;
}

pub struct DoctorUseCase<F, M, S> {
    pub fs: Arc<F>,
    pub metadata: Arc<M>,
    pub store: Arc<S>,
}

impl<F: FileSystem + 'static, M: MetadataReader + 'static, S: DoctorStore + 'static>
    DoctorUseCase<F, M, S>
{
    pub fn execute(&self, source: &Path, options: &ScanOptions) -> WorkflowResult<DoctorRun> {
        let mut options = options.clone();
        options.follow_reparse_points = false;
        let scan_id = self.store.begin_scan(source)?;
        let mut run = match self.store.begin_doctor(&scan_id) {
            Ok(run) => run,
            Err(error) => {
                let _ = self.store.finish_scan(&scan_id, RunStatus::Failed, 0);
                return Err(error.into());
            }
        };
        if let Some(progress) = &options.progress {
            progress(crate::usecases::ScanProgress {
                scan_id: run.scan_id.clone(),
                phase: "doctor_start".into(),
                enumerated: 0,
                processed: 0,
                cache_hits: 0,
                warnings: 0,
                elapsed_ms: 0,
                items_per_second: 0.0,
                eta_seconds: None,
            });
        }
        let mut issues = Vec::new();
        let result = (|| -> WorkflowResult<()> {
            let scan = ScanUseCase {
                fs: Arc::clone(&self.fs),
                metadata: Arc::clone(&self.metadata),
                store: Arc::clone(&self.store),
            }
            .execute_started(source, &options, scan_id)?;
            run.files = scan.files;
            run.cache_hits = scan.cache_hits;
            run.failures = scan.warnings;
            if options.cancellation.is_cancelled() {
                run.status = RunStatus::Cancelled;
                return Ok(());
            }
            let files = self.store.doctor_files(&run.scan_id)?;
            run.files = files.iter().filter(|f| f.kind == FileKind::Music).count() as u64;
            let mut stable = Vec::with_capacity(files.len());
            let validation_started = std::time::Instant::now();
            for (index, file) in files.iter().enumerate() {
                if options.cancellation.is_cancelled() {
                    break;
                }
                if file.kind != FileKind::Music {
                    continue;
                }
                // Revalidate after metadata reading. Never trust a stale cached hash alone.
                if let Some(sink) = &options.progress {
                    sink(crate::usecases::ScanProgress {
                        scan_id: run.scan_id.clone(),
                        phase: "doctor_validate".into(),
                        enumerated: files.len() as u64,
                        processed: index as u64,
                        cache_hits: run.cache_hits,
                        warnings: run.failures,
                        elapsed_ms: validation_started.elapsed().as_millis() as u64,
                        items_per_second: index as f64
                            / validation_started.elapsed().as_secs_f64().max(0.001),
                        eta_seconds: None,
                    });
                }
                let observed = self.fs.fingerprint(&file.path);
                let error = match observed {
                    Ok(fp)
                        if fp.size_bytes == file.fingerprint.size_bytes
                            && fp.mtime_ns == file.fingerprint.mtime_ns
                            && fp.version == file.fingerprint.version
                            && fp.file_identity == file.fingerprint.file_identity
                            && fp.content_sha256 == file.fingerprint.content_sha256 =>
                    {
                        None
                    }
                    Ok(_) => Some("file_changed_since_scan".to_owned()),
                    Err(error) => Some(error),
                };
                if let Some(error) = error {
                    run.failures += 1;
                    issues.push(issue(
                        "file_changed",
                        Severity::Critical,
                        "read",
                        &[file],
                        vec![error],
                    ));
                } else {
                    stable.push(file);
                }
            }
            if !options.cancellation.is_cancelled() {
                issues.extend(diagnose(&stable, &options.cancellation));
            }
            run.status = if options.cancellation.is_cancelled() {
                RunStatus::Cancelled
            } else if run.failures > 0 {
                RunStatus::Partial
            } else {
                RunStatus::Completed
            };
            Ok(())
        })();
        if let Err(error) = result {
            run.status = RunStatus::Failed;
            run.error = Some(error.to_string());
        }
        // Retain scan failure evidence even when cancellation or a fatal error
        // bypasses the rule stage. These warnings remain tied to the scan too.
        let warnings = self.store.doctor_warnings(&run.scan_id)?;
        run.failures = run.failures.max(warnings.len() as u64);
        issues.extend(warnings.into_iter().map(|warning| {
            issue(
                "scan_warning",
                Severity::Critical,
                "read",
                &[],
                vec![warning],
            )
        }));
        run.issue_count = issues.len() as u64;
        self.store.finish_doctor(&run, &issues)?;
        Ok(run)
    }
}

/// NFKC + Unicode lowercase + whitespace collapse. Only these invisible codepoints
/// are removed: U+200B ZERO WIDTH SPACE, U+2060 WORD JOINER, U+FEFF BOM.
/// ZWJ/ZWNJ are preserved: removing them can change meaningful spelling.
pub fn comparison_key(value: &str) -> String {
    value
        .nfkc()
        .filter(|c| !matches!(c, '\u{200b}' | '\u{2060}' | '\u{feff}'))
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn present(value: Option<&str>) -> bool {
    value.is_some_and(|s| !comparison_key(s).is_empty())
}

fn issue(
    code: &str,
    severity: Severity,
    category: &str,
    files: &[&ScannedFile],
    evidence: Vec<String>,
) -> DoctorIssue {
    DoctorIssue {
        code: code.into(),
        severity,
        category: category.into(),
        file_ids: files.iter().map(|f| f.id).collect(),
        evidence,
        comparison: None,
        rule_version: RULE_VERSION,
    }
}

pub fn diagnose(files: &[&ScannedFile], cancellation: &CancellationToken) -> Vec<DoctorIssue> {
    let mut issues = Vec::new();
    let mut artists: BTreeMap<String, Vec<(&str, &ScannedFile)>> = BTreeMap::new();
    let mut albums: BTreeMap<(String, String), Vec<(&str, &ScannedFile)>> = BTreeMap::new();
    let mut duplicates: BTreeMap<(u64, &str), Vec<&ScannedFile>> = BTreeMap::new();
    for &file in files {
        if cancellation.is_cancelled() {
            return issues;
        }
        if file.kind != FileKind::Music {
            continue;
        }
        if let Some(hash) = file.fingerprint.content_sha256.as_deref() {
            duplicates
                .entry((file.fingerprint.size_bytes, hash))
                .or_default()
                .push(file);
        }
        let Some(tags) = &file.metadata else {
            issues.push(issue("read_failed", Severity::Critical, "read", &[file], vec!["Metadata unavailable; see scan warnings for reader error. Missing-tag rules were not evaluated.".into()]));
            continue;
        };
        for (code, missing, severity) in [
            (
                "missing_title",
                !present(tags.title.as_deref()),
                Severity::Warning,
            ),
            (
                "missing_artist",
                !present(tags.artist.as_deref()),
                Severity::Warning,
            ),
            (
                "missing_album",
                !present(tags.album.as_deref()),
                Severity::Warning,
            ),
            (
                "missing_album_artist",
                !present(tags.album_artist.as_deref()),
                Severity::Info,
            ),
            (
                "missing_track",
                tags.track_no.is_none_or(|n| n == 0),
                Severity::Warning,
            ),
            (
                "missing_disc",
                tags.disc_no.is_none_or(|n| n == 0),
                Severity::Info,
            ),
            (
                "missing_year",
                tags.year.is_none_or(|n| n <= 0),
                Severity::Info,
            ),
            (
                "missing_artwork",
                tags.has_artwork == Some(false),
                Severity::Info,
            ),
        ] {
            if missing {
                issues.push(issue(
                    code,
                    severity,
                    "tags",
                    &[file],
                    vec!["Not present in readable embedded tags; this can be intentional.".into()],
                ));
            }
        }
        if let Some(artist) = tags.artist.as_deref().filter(|s| present(Some(s))) {
            artists
                .entry(comparison_key(artist))
                .or_default()
                .push((artist, file));
        }
        if let Some(album) = tags.album.as_deref().filter(|s| present(Some(s))) {
            let context = tags
                .album_artist
                .as_deref()
                .filter(|s| present(Some(s)))
                .or(tags.artist.as_deref())
                .map(comparison_key);
            if let Some(context) = context.filter(|s| !s.is_empty()) {
                albums
                    .entry((context, comparison_key(album)))
                    .or_default()
                    .push((album, file));
            }
        }
    }
    for (key, values) in artists {
        variants(&mut issues, "artist_variant", key, values);
    }
    for ((artist, album), values) in albums {
        variants(
            &mut issues,
            "album_variant",
            format!("{artist} / {album}"),
            values,
        );
    }
    for ((size, hash), group) in duplicates {
        if cancellation.is_cancelled() {
            return issues;
        }
        if group.len() < 2 {
            continue;
        }
        let identities: BTreeSet<_> = group
            .iter()
            .filter_map(|f| f.fingerprint.file_identity.as_deref())
            .collect();
        let same =
            identities.len() == 1 && group.iter().all(|f| f.fingerprint.file_identity.is_some());
        issues.push(issue(if same { "same_file_paths" } else { "exact_duplicate" }, Severity::Info, "duplicates", &group,
            vec![format!("size={size}; sha256={hash}; known_identities={}; paths={}; {}", identities.len(), group.len(),
                if same { "All paths identify one file object." } else { "Byte-identical candidates. Unknown identities may include aliases; no keeper selected." })]));
    }
    for (_, group) in album_groups(files, cancellation) {
        if cancellation.is_cancelled() {
            return issues;
        }
        album_issues(&group, &mut issues);
    }
    issues
}

/// The rule-v1 album grouping used by diagnosis and read-only library views.
pub fn album_groups<'a>(
    files: &[&'a ScannedFile],
    cancellation: &CancellationToken,
) -> Vec<(PathBuf, Vec<&'a ScannedFile>)> {
    let mut groups: BTreeMap<(PathBuf, String), Vec<&ScannedFile>> = BTreeMap::new();
    for &file in files {
        if cancellation.is_cancelled() {
            return Vec::new();
        }
        if file.kind != FileKind::Music {
            continue;
        }
        if let Some(album) = file
            .metadata
            .as_ref()
            .and_then(|m| m.album.as_deref())
            .filter(|s| present(Some(s)))
        {
            groups
                .entry((album_folder(&file.path), comparison_key(album)))
                .or_default()
                .push(file);
        }
    }
    let mut result = Vec::new();
    for ((folder, _), group) in groups {
        if cancellation.is_cancelled() {
            return result;
        }
        let artists: BTreeSet<_> = group
            .iter()
            .filter_map(|f| f.metadata.as_ref()?.artist.as_deref())
            .map(comparison_key)
            .collect();
        let album_artists: BTreeSet<_> = group
            .iter()
            .map(|f| {
                f.metadata
                    .as_ref()
                    .and_then(|m| m.album_artist.as_deref())
                    .filter(|s| present(Some(s)))
                    .map(comparison_key)
            })
            .collect();
        if artists.len() <= 1 || (album_artists.len() == 1 && !album_artists.contains(&None)) {
            result.push((folder, group));
        } else {
            // Ambiguous compilation: prefer under-grouping to mixing unrelated artists.
            let mut partitions: BTreeMap<String, Vec<&ScannedFile>> = BTreeMap::new();
            for file in group {
                let tags = file.metadata.as_ref().expect("readable group");
                let context = tags
                    .album_artist
                    .as_deref()
                    .filter(|s| present(Some(s)))
                    .or(tags.artist.as_deref())
                    .map(comparison_key)
                    .unwrap_or_default();
                partitions.entry(context).or_default().push(file);
            }
            for group in partitions.into_values() {
                result.push((folder.clone(), group));
            }
        }
    }
    result
}

fn variants(
    issues: &mut Vec<DoctorIssue>,
    code: &str,
    key: String,
    values: Vec<(&str, &ScannedFile)>,
) {
    let originals: BTreeSet<_> = values.iter().map(|(s, _)| *s).collect();
    if originals.len() > 1 {
        let mut found = issue(
            code,
            Severity::Info,
            "variants",
            &values.iter().map(|(_, f)| *f).collect::<Vec<_>>(),
            originals.into_iter().map(str::to_owned).collect(),
        );
        found.comparison = Some(key);
        issues.push(found);
    }
}

fn album_folder(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or(Path::new(""));
    let name = parent
        .file_name()
        .and_then(|s| s.to_str())
        .map(comparison_key)
        .unwrap_or_default();
    let disc_folder = ["cd", "disc", "disk"].iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|s| s.trim().parse::<u32>().is_ok_and(|n| n > 0))
    });
    if disc_folder {
        parent.parent().unwrap_or(parent).to_path_buf()
    } else {
        parent.to_path_buf()
    }
}

fn album_issues(group: &[&ScannedFile], issues: &mut Vec<DoctorIssue>) {
    let mut discs: BTreeMap<u32, BTreeMap<u32, Vec<&ScannedFile>>> = BTreeMap::new();
    let mut disc_values = BTreeSet::new();
    let mut attrs: [BTreeSet<String>; 4] = Default::default();
    for &file in group {
        let tags = file.metadata.as_ref().expect("readable group");
        disc_values.insert(tags.disc_no.filter(|n| *n > 0));
        if let Some(track) = tags.track_no.filter(|n| *n > 0) {
            discs
                .entry(tags.disc_no.filter(|n| *n > 0).unwrap_or(1))
                .or_default()
                .entry(track)
                .or_default()
                .push(file);
        }
        for (set, value) in attrs.iter_mut().zip([
            tags.album_artist.clone(),
            tags.year.map(|v| v.to_string()),
            tags.genre.clone(),
            file.path
                .extension()
                .and_then(|s| s.to_str())
                .map(str::to_ascii_lowercase),
        ]) {
            if let Some(value) = value.filter(|s| present(Some(s))) {
                set.insert(value);
            }
        }
    }
    let numbered: Vec<_> = disc_values.iter().flatten().copied().collect();
    if (disc_values.contains(&None) && disc_values.len() > 1)
        || numbered.first().is_some_and(|n| *n != 1)
        || numbered.windows(2).any(|w| w[1] - w[0] > 1)
    {
        issues.push(issue(
            "disc_inconsistent",
            Severity::Warning,
            "albums",
            group,
            vec![format!(
                "Observed disc numbers: {disc_values:?}. Missing values are provisionally disc 1."
            )],
        ));
    }
    for (disc, tracks) in discs {
        let disc_files: Vec<_> = tracks.values().flatten().copied().collect();
        let mut previous = 0;
        let mut gaps = Vec::new();
        for (track, members) in tracks {
            if track - previous > 1 {
                gaps.push(format!("{}..{}", previous + 1, track - 1));
            }
            previous = track;
            if members.len() > 1 {
                issues.push(issue(
                    "track_duplicate",
                    Severity::Warning,
                    "albums",
                    &members,
                    vec![format!("disc={disc}; track={track}")],
                ));
            }
        }
        if !gaps.is_empty() {
            issues.push(issue("track_gap", Severity::Warning, "albums", &disc_files, vec![format!("disc={disc}; candidate missing ranges: {}. Trailing tracks and completeness are unknown.", gaps.join(", "))]));
        }
    }
    for (code, values) in [
        "album_artist_inconsistent",
        "year_inconsistent",
        "genre_inconsistent",
        "format_mixed",
    ]
    .into_iter()
    .zip(attrs)
    {
        if values.len() > 1 {
            issues.push(issue(
                code,
                Severity::Info,
                "albums",
                group,
                values.into_iter().collect(),
            ));
        }
    }
}
