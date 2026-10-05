//! Bounded desktop responses derived from immutable diagnostic snapshots.
use crate::path_codec::{path_envelope, LosslessPathEnvelope};
use music_folder_core::{
    doctor::{album_groups, comparison_key, DoctorIssue, DoctorRun, Severity},
    ScannedFile, TrackMetadata,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
};
use uuid::Uuid;

#[derive(Clone, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub next_cursor: Option<usize>,
}

pub fn page<T: Clone>(items: &[T], cursor: usize, limit: usize) -> Page<T> {
    let end = cursor.saturating_add(limit).min(items.len());
    Page {
        items: items.get(cursor..end).unwrap_or_default().to_vec(),
        total: items.len(),
        next_cursor: (end < items.len()).then_some(end),
    }
}

#[derive(Clone, Serialize)]
pub struct DoctorHistory {
    pub run: DoctorRun,
    pub source: LosslessPathEnvelope,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Clone, Serialize)]
pub struct IssueRow {
    pub ordinal: usize,
    pub code: String,
    pub severity: Severity,
    pub category: String,
    pub file_count: usize,
}
impl IssueRow {
    pub fn new(ordinal: usize, issue: &DoctorIssue) -> Self {
        Self {
            ordinal,
            code: issue.code.clone(),
            severity: issue.severity,
            category: issue.category.clone(),
            file_count: issue.file_ids.len(),
        }
    }
}

#[derive(Clone, Serialize)]
pub struct FileRow {
    pub id: Uuid,
    pub path: LosslessPathEnvelope,
    pub metadata: Option<TrackMetadata>,
    pub format: String,
    pub size_bytes: u64,
    pub mtime_ns: String,
    pub sha256: Option<String>,
    pub identity: Option<String>,
}
impl From<&ScannedFile> for FileRow {
    fn from(f: &ScannedFile) -> Self {
        Self {
            id: f.id,
            path: path_envelope(&f.path, "source"),
            metadata: f.metadata.clone(),
            format: f
                .path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase(),
            size_bytes: f.fingerprint.size_bytes,
            mtime_ns: f.fingerprint.mtime_ns.to_string(),
            sha256: f.fingerprint.content_sha256.clone(),
            identity: f.fingerprint.file_identity.clone(),
        }
    }
}

#[derive(Serialize)]
pub struct IssueDetail {
    pub issue: IssueRow,
    pub evidence: Vec<String>,
    pub comparison: Option<String>,
    pub rule_version: u32,
    pub files: Page<FileRow>,
}

#[derive(Clone, Serialize)]
pub struct AlbumRow {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub folder: LosslessPathEnvelope,
    pub tracks: usize,
    pub issue_count: usize,
    pub unclassified: bool,
}
pub struct Album {
    pub row: AlbumRow,
    pub folder: PathBuf,
    pub file_ids: Vec<Uuid>,
    pub issue_ordinals: Vec<usize>,
}
#[derive(Serialize)]
pub struct AlbumDetail {
    pub album: AlbumRow,
    pub files: Page<FileRow>,
    pub issues: Page<IssueRow>,
}
#[derive(Serialize)]
pub struct DoctorSummary {
    pub history: DoctorHistory,
    pub severities: BTreeMap<String, usize>,
}

pub struct DoctorView {
    pub history: DoctorHistory,
    pub source: PathBuf,
    pub files: HashMap<Uuid, ScannedFile>,
    pub issues: Vec<DoctorIssue>,
    pub albums: Vec<Album>,
}
impl DoctorView {
    pub fn new(
        history: DoctorHistory,
        source: PathBuf,
        files: Vec<ScannedFile>,
        issues: Vec<DoctorIssue>,
    ) -> Self {
        let changed: HashSet<_> = issues
            .iter()
            .filter(|i| i.code == "file_changed")
            .flat_map(|i| i.file_ids.iter().copied())
            .collect();
        let stable: Vec<_> = files.iter().filter(|f| !changed.contains(&f.id)).collect();
        let groups = album_groups(
            &stable,
            &music_folder_core::usecases::CancellationToken::default(),
        );
        let mut classified = HashSet::new();
        let mut groups_owned = Vec::new();
        for (folder, group) in groups {
            let ids: Vec<_> = group.iter().map(|f| f.id).collect();
            classified.extend(ids.iter().copied());
            let tags = group[0].metadata.as_ref().expect("classified album");
            let artists: HashSet<_> = group
                .iter()
                .filter_map(|f| {
                    f.metadata.as_ref()?.album_artist.as_deref().or(f
                        .metadata
                        .as_ref()?
                        .artist
                        .as_deref())
                })
                .collect();
            let artist = if artists.len() == 1 {
                artists.into_iter().next().unwrap().to_owned()
            } else {
                "Various Artists".into()
            };
            groups_owned.push((
                folder,
                ids,
                tags.album.clone().unwrap_or_default(),
                artist,
                false,
            ));
        }
        let unknown: Vec<_> = stable
            .iter()
            .filter(|f| !classified.contains(&f.id))
            .map(|f| f.id)
            .collect();
        if !unknown.is_empty() {
            groups_owned.push((source.clone(), unknown, "未分類".into(), "".into(), true));
        }
        let files: HashMap<_, _> = files.into_iter().map(|f| (f.id, f)).collect();
        let mut file_issues: HashMap<Uuid, Vec<usize>> = HashMap::new();
        for (ordinal, issue) in issues.iter().enumerate() {
            for id in &issue.file_ids {
                file_issues.entry(*id).or_default().push(ordinal);
            }
        }
        let mut albums = Vec::new();
        for (folder, mut ids, title, artist, unclassified) in groups_owned {
            let mut issue_ordinals: Vec<_> = ids
                .iter()
                .flat_map(|id| file_issues.get(id).into_iter().flatten().copied())
                .collect();
            issue_ordinals.sort_unstable();
            issue_ordinals.dedup();
            let id = if unclassified {
                "unclassified".into()
            } else {
                ids[0].to_string()
            };
            ids.sort_by_key(|id| {
                let f = &files[id];
                let m = f.metadata.as_ref();
                (
                    m.and_then(|m| m.disc_no).filter(|n| *n > 0).unwrap_or(1),
                    m.and_then(|m| m.track_no)
                        .filter(|n| *n > 0)
                        .unwrap_or(u32::MAX),
                    f.path.clone(),
                )
            });
            albums.push(Album {
                row: AlbumRow {
                    id,
                    title,
                    artist,
                    folder: path_envelope(&folder, "source"),
                    tracks: ids.len(),
                    issue_count: issue_ordinals.len(),
                    unclassified,
                },
                folder,
                file_ids: ids,
                issue_ordinals,
            });
        }
        albums.sort_by_key(|a| {
            (
                a.row.unclassified,
                comparison_key(&a.row.artist),
                comparison_key(&a.row.title),
                a.row.id.clone(),
            )
        });
        Self {
            history,
            source,
            files,
            issues,
            albums,
        }
    }
    pub fn summary(&self) -> DoctorSummary {
        let mut severities = BTreeMap::from([
            ("critical".into(), 0),
            ("warning".into(), 0),
            ("info".into(), 0),
        ]);
        for issue in &self.issues {
            *severities
                .get_mut(match issue.severity {
                    Severity::Critical => "critical",
                    Severity::Warning => "warning",
                    Severity::Info => "info",
                })
                .unwrap() += 1;
        }
        DoctorSummary {
            history: self.history.clone(),
            severities,
        }
    }
    fn file_page(&self, ids: &[Uuid], cursor: usize) -> Page<FileRow> {
        let p = page(ids, cursor, 100);
        Page {
            items: p
                .items
                .iter()
                .filter_map(|id| self.files.get(id))
                .map(FileRow::from)
                .collect(),
            total: p.total,
            next_cursor: p.next_cursor,
        }
    }
    pub fn issue_detail(&self, ordinal: usize, cursor: usize) -> Result<IssueDetail, String> {
        let i = self.issues.get(ordinal).ok_or("doctor_issue_not_found")?;
        Ok(IssueDetail {
            issue: IssueRow::new(ordinal, i),
            evidence: i.evidence.clone(),
            comparison: i.comparison.clone(),
            rule_version: i.rule_version,
            files: self.file_page(&i.file_ids, cursor),
        })
    }
    pub fn album(&self, id: &str) -> Result<&Album, String> {
        self.albums
            .iter()
            .find(|a| a.row.id == id)
            .ok_or_else(|| "doctor_album_not_found".into())
    }
    pub fn album_detail(
        &self,
        id: &str,
        cursor: usize,
        issue_cursor: usize,
    ) -> Result<AlbumDetail, String> {
        let a = self.album(id)?;
        let p = page(&a.issue_ordinals, issue_cursor, 100);
        Ok(AlbumDetail {
            album: a.row.clone(),
            files: self.file_page(&a.file_ids, cursor),
            issues: Page {
                items: p
                    .items
                    .into_iter()
                    .map(|n| IssueRow::new(n, &self.issues[n]))
                    .collect(),
                total: p.total,
                next_cursor: p.next_cursor,
            },
        })
    }
    pub fn album_page(&self, query: &str, cursor: usize) -> Page<AlbumRow> {
        let q = comparison_key(query);
        let rows: Vec<_> = self
            .albums
            .iter()
            .filter(|a| comparison_key(&format!("{} {}", a.row.title, a.row.artist)).contains(&q))
            .map(|a| a.row.clone())
            .collect();
        page(&rows, cursor, 48)
    }
}
