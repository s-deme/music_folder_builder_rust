use crate::{
    DuplicateStrategy, ExecutionDisposition, FileKind, IssueSeverity, NamingRules, PlanAction,
    PlanIssueCode, PlanItem, Risk, ScannedFile, NAMING_RULES_SCHEMA_VERSION,
    WINDOWS_PATH_POLICY_VERSION,
};
use sha2::{Digest, Sha256};
use std::{ffi::OsStr, path::Path};

pub const PLAN_SNAPSHOT_VERSION: u32 = 4;
pub const SCAN_SNAPSHOT_VERSION: u32 = 1;

/// Canonical, length-delimited authorization digest.  Display-only UUIDs and
/// conflict candidates are excluded; source expectation is deliberately part
/// of the authorization surface.
pub fn plan_snapshot_hash(items: &[PlanItem]) -> String {
    let mut digest = Sha256::new();
    field(&mut digest, b"music-folder-plan-item-snapshot");
    field(&mut digest, &PLAN_SNAPSHOT_VERSION.to_le_bytes());
    hash_plan_items(&mut digest, items);
    format!("v{PLAN_SNAPSHOT_VERSION}:{:x}", digest.finalize())
}

/// Canonical authorization digest used by persisted plans.  Context that may
/// change the meaning of an otherwise identical item list is deliberately part
/// of the signed surface.
pub fn plan_authorization_hash(
    scan_id: &str,
    scan_snapshot_hash: &str,
    target_root: &Path,
    naming: &NamingRules,
    items: &[PlanItem],
) -> String {
    let mut hasher = PlanAuthorizationHasher::new(
        scan_id,
        scan_snapshot_hash,
        target_root,
        naming,
        items.len() as u64,
    );
    for item in items {
        hasher.update(item);
    }
    hasher
        .finish()
        .expect("slice length always matches authorization item count")
}

/// Incremental form of `plan_authorization_hash` for plans which are larger
/// than a single in-memory page. Feeding items in ascending ordinal order is
/// part of the canonical encoding contract.
pub struct PlanAuthorizationHasher {
    digest: Sha256,
    expected_items: u64,
    seen_items: u64,
}

impl PlanAuthorizationHasher {
    pub fn new(
        scan_id: &str,
        scan_snapshot_hash: &str,
        target_root: &Path,
        naming: &NamingRules,
        item_count: u64,
    ) -> Self {
        Self::new_with_rules_schema_version(
            scan_id,
            scan_snapshot_hash,
            target_root,
            NAMING_RULES_SCHEMA_VERSION,
            naming,
            item_count,
        )
    }

    /// Rehydrates the persisted authorization context. Callers must reject an
    /// unsupported rules version before accepting the resulting digest.
    pub fn new_with_rules_schema_version(
        scan_id: &str,
        scan_snapshot_hash: &str,
        target_root: &Path,
        rules_schema_version: u32,
        naming: &NamingRules,
        item_count: u64,
    ) -> Self {
        let mut digest = Sha256::new();
        field(&mut digest, b"music-folder-plan-authorization");
        field(&mut digest, &PLAN_SNAPSHOT_VERSION.to_le_bytes());
        field(&mut digest, &WINDOWS_PATH_POLICY_VERSION.to_le_bytes());
        field(&mut digest, scan_id.as_bytes());
        field(&mut digest, scan_snapshot_hash.as_bytes());
        path_field(&mut digest, target_root);
        field(&mut digest, &rules_schema_version.to_le_bytes());
        hash_naming_rules(&mut digest, naming);
        field(&mut digest, &item_count.to_le_bytes());
        Self {
            digest,
            expected_items: item_count,
            seen_items: 0,
        }
    }

    pub fn update(&mut self, item: &PlanItem) {
        hash_plan_item(&mut self.digest, item);
        self.seen_items = self.seen_items.saturating_add(1);
    }

    pub fn finish(self) -> Result<String, String> {
        if self.seen_items != self.expected_items {
            return Err(format!(
                "plan_authorization_item_count_mismatch:{}:{}",
                self.expected_items, self.seen_items
            ));
        }
        Ok(format!(
            "v{PLAN_SNAPSHOT_VERSION}:{:x}",
            self.digest.finalize()
        ))
    }
}

pub fn scan_snapshot_hash(files: &[ScannedFile]) -> String {
    let mut ordered = files.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        os_str_bytes(left.path.as_os_str()).cmp(&os_str_bytes(right.path.as_os_str()))
    });
    let mut digest = Sha256::new();
    field(&mut digest, b"music-folder-scan-snapshot");
    field(&mut digest, &SCAN_SNAPSHOT_VERSION.to_le_bytes());
    field(&mut digest, &(ordered.len() as u64).to_le_bytes());
    for file in ordered {
        path_field(&mut digest, &file.path);
        field(&mut digest, &file.fingerprint.size_bytes.to_le_bytes());
        field(&mut digest, &file.fingerprint.mtime_ns.to_le_bytes());
        field(&mut digest, &file.fingerprint.version.to_le_bytes());
        optional_text(&mut digest, file.fingerprint.content_sha256.as_deref());
        optional_text(&mut digest, file.fingerprint.file_identity.as_deref());
        field(
            &mut digest,
            match file.kind {
                FileKind::Music => b"music",
                FileKind::Image => b"image",
            },
        );
        match &file.metadata {
            Some(metadata) => {
                field(&mut digest, &[1]);
                optional_text(&mut digest, metadata.artist.as_deref());
                optional_text(&mut digest, metadata.album_artist.as_deref());
                optional_text(&mut digest, metadata.album.as_deref());
                optional_text(&mut digest, metadata.title.as_deref());
                optional_u32(&mut digest, metadata.track_no);
                optional_u32(&mut digest, metadata.disc_no);
                optional_i32(&mut digest, metadata.year);
            }
            None => field(&mut digest, &[0]),
        }
    }
    format!("v{SCAN_SNAPSHOT_VERSION}:{:x}", digest.finalize())
}

fn hash_plan_items(digest: &mut Sha256, items: &[PlanItem]) {
    field(digest, &(items.len() as u64).to_le_bytes());
    for item in items {
        hash_plan_item(digest, item);
    }
}

fn hash_plan_item(digest: &mut Sha256, item: &PlanItem) {
    field(digest, &item.ordinal.to_le_bytes());
    path_field(digest, &item.file.path);
    field(digest, &item.file.fingerprint.size_bytes.to_le_bytes());
    field(digest, &item.file.fingerprint.mtime_ns.to_le_bytes());
    field(digest, &item.file.fingerprint.version.to_le_bytes());
    optional_text(digest, item.file.fingerprint.content_sha256.as_deref());
    optional_text(digest, item.file.fingerprint.file_identity.as_deref());
    match &item.target {
        Some(target) => {
            field(digest, &[1]);
            path_field(digest, target);
        }
        None => field(digest, &[0]),
    }
    field(
        digest,
        match item.action {
            PlanAction::Move => b"move",
            PlanAction::Skip => b"skip",
        },
    );
    field(
        digest,
        match item.disposition {
            ExecutionDisposition::Executable => b"eligible",
            ExecutionDisposition::Skip => b"skip",
            ExecutionDisposition::Blocked => b"blocked",
        },
    );
    field(
        digest,
        match item.risk {
            Risk::None => b"none",
            Risk::InvalidTarget => b"invalid_target",
            Risk::PathTooLong => b"path_too_long",
            Risk::Conflict => b"conflict",
            Risk::MetadataMissing => b"metadata_missing",
        },
    );
    optional_text(digest, item.reason.as_deref());
    field(digest, &(item.issues.len() as u64).to_le_bytes());
    for issue in &item.issues {
        field(
            digest,
            match issue.severity {
                IssueSeverity::Warning => b"warning",
                IssueSeverity::Blocking => b"blocking",
            },
        );
        field(
            digest,
            match issue.code {
                PlanIssueCode::InvalidTarget => b"invalid_target",
                PlanIssueCode::PathTooLong => b"path_too_long",
                PlanIssueCode::Conflict => b"conflict",
                PlanIssueCode::MetadataMissing => b"metadata_missing",
            },
        );
        optional_text(digest, issue.detail.as_deref());
    }
}

fn hash_naming_rules(digest: &mut Sha256, naming: &NamingRules) {
    field(digest, naming.artist_dir_template.as_bytes());
    field(digest, naming.album_dir_template.as_bytes());
    field(digest, naming.disc_dir_template.as_bytes());
    field(digest, naming.filename_template.as_bytes());
    field(digest, naming.duplicate_suffix_template.as_bytes());
    field(digest, &[u8::from(naming.use_source_filename)]);
    field(digest, &[u8::from(naming.use_source_image_filename)]);
    field(digest, &[u8::from(naming.allow_missing_metadata)]);
    field(digest, &[u8::from(naming.allow_long_paths)]);
    field(
        digest,
        match naming.duplicate_strategy {
            DuplicateStrategy::Legacy => b"legacy",
            DuplicateStrategy::Skip => b"skip",
            DuplicateStrategy::Sequence => b"sequence",
            DuplicateStrategy::Template => b"template",
        },
    );
}

fn optional_u32(digest: &mut Sha256, value: Option<u32>) {
    match value {
        Some(value) => {
            field(digest, &[1]);
            field(digest, &value.to_le_bytes());
        }
        None => field(digest, &[0]),
    }
}

fn optional_i32(digest: &mut Sha256, value: Option<i32>) {
    match value {
        Some(value) => {
            field(digest, &[1]);
            field(digest, &value.to_le_bytes());
        }
        None => field(digest, &[0]),
    }
}

fn optional_text(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            field(digest, &[1]);
            field(digest, value.as_bytes());
        }
        None => field(digest, &[0]),
    }
}

fn field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn path_field(digest: &mut Sha256, path: &Path) {
    field(digest, &os_str_bytes(path.as_os_str()));
}

#[cfg(windows)]
fn os_str_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    value
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>()
}

#[cfg(unix)]
fn os_str_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(not(any(windows, unix)))]
fn os_str_bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileFingerprint, FileKind, PlanConflictCandidate, ScannedFile};
    use std::path::PathBuf;
    use uuid::Uuid;

    fn item() -> PlanItem {
        PlanItem {
            id: Uuid::nil(),
            conflict_group_id: None,
            ordinal: 1,
            file: ScannedFile {
                id: Uuid::nil(),
                path: PathBuf::from("C:/source/a.mp3"),
                fingerprint: FileFingerprint::legacy(4, 7),
                metadata: None,
                kind: FileKind::Music,
            },
            target: Some(PathBuf::from("C:/target/a.mp3")),
            action: PlanAction::Move,
            disposition: ExecutionDisposition::Executable,
            risk: Risk::None,
            reason: None,
            issues: Vec::new(),
            conflict_candidates: Vec::new(),
        }
    }

    #[test]
    fn source_expectation_and_target_are_authorized() {
        let original = item();
        let before = plan_snapshot_hash(std::slice::from_ref(&original));
        let mut changed = original;
        changed.file.fingerprint.mtime_ns += 1;
        assert_ne!(before, plan_snapshot_hash(&[changed]));
    }

    #[test]
    fn display_only_conflict_candidates_are_not_authorized() {
        let mut changed = item();
        let before = plan_snapshot_hash(std::slice::from_ref(&changed));
        changed.conflict_candidates.push(PlanConflictCandidate {
            target_directory: PathBuf::from("C:/elsewhere"),
            music_item_ids: vec![Uuid::new_v4()],
        });
        assert_eq!(before, plan_snapshot_hash(&[changed]));
    }

    #[test]
    fn authorization_hash_covers_scan_root_and_rules_context() {
        let item = item();
        let naming = NamingRules::default();
        let original = plan_authorization_hash(
            "scan-a",
            "v1:scan-hash-a",
            Path::new("C:/target"),
            &naming,
            std::slice::from_ref(&item),
        );
        assert_ne!(
            original,
            plan_authorization_hash(
                "scan-b",
                "v1:scan-hash-a",
                Path::new("C:/target"),
                &naming,
                std::slice::from_ref(&item),
            )
        );
        assert_ne!(
            original,
            plan_authorization_hash(
                "scan-a",
                "v1:scan-hash-b",
                Path::new("C:/target"),
                &naming,
                std::slice::from_ref(&item),
            )
        );
        let mut changed_rules = naming.clone();
        changed_rules.allow_long_paths = true;
        assert_ne!(
            original,
            plan_authorization_hash(
                "scan-a",
                "v1:scan-hash-a",
                Path::new("C:/target"),
                &changed_rules,
                std::slice::from_ref(&item),
            )
        );
        assert_ne!(
            original,
            plan_authorization_hash(
                "scan-a",
                "v1:scan-hash-a",
                Path::new("C:/other-target"),
                &naming,
                std::slice::from_ref(&item),
            )
        );

        let version_tampered = {
            let mut hasher = PlanAuthorizationHasher::new_with_rules_schema_version(
                "scan-a",
                "v1:scan-hash-a",
                Path::new("C:/target"),
                NAMING_RULES_SCHEMA_VERSION + 1,
                &naming,
                1,
            );
            hasher.update(&item);
            hasher.finish().unwrap()
        };
        assert_ne!(original, version_tampered);
    }

    #[test]
    fn disposition_and_typed_issues_are_authorized_independently_from_risk() {
        let original = item();
        let before = plan_snapshot_hash(std::slice::from_ref(&original));

        let mut changed = original.clone();
        changed.disposition = ExecutionDisposition::Blocked;
        assert_ne!(before, plan_snapshot_hash(std::slice::from_ref(&changed)));

        let mut changed = original;
        changed.issues.push(crate::PlanIssue {
            severity: IssueSeverity::Warning,
            code: PlanIssueCode::MetadataMissing,
            detail: Some("artist_missing".into()),
        });
        assert_eq!(changed.disposition, ExecutionDisposition::Executable);
        assert_ne!(before, plan_snapshot_hash(&[changed]));
    }

    #[test]
    fn incremental_authorization_matches_slice_encoding_and_checks_count() {
        let first = item();
        let mut second = item();
        second.ordinal = 2;
        second.file.path = PathBuf::from("C:/source/b.mp3");
        second.target = Some(PathBuf::from("C:/target/b.mp3"));
        let naming = NamingRules::default();
        let expected = plan_authorization_hash(
            "scan-a",
            "v1:scan-hash",
            Path::new("C:/target"),
            &naming,
            &[first.clone(), second.clone()],
        );
        let mut incremental = PlanAuthorizationHasher::new(
            "scan-a",
            "v1:scan-hash",
            Path::new("C:/target"),
            &naming,
            2,
        );
        incremental.update(&first);
        incremental.update(&second);
        assert_eq!(incremental.finish().unwrap(), expected);

        let mut incomplete = PlanAuthorizationHasher::new(
            "scan-a",
            "v1:scan-hash",
            Path::new("C:/target"),
            &naming,
            2,
        );
        incomplete.update(&first);
        assert_eq!(
            incomplete.finish().unwrap_err(),
            "plan_authorization_item_count_mismatch:2:1"
        );
    }

    #[test]
    fn scan_snapshot_is_order_independent_but_metadata_sensitive() {
        let first = item().file;
        let mut second = first.clone();
        second.path = PathBuf::from("C:/source/b.mp3");
        second.metadata = Some(crate::TrackMetadata {
            artist: Some("Artist".into()),
            album_artist: None,
            album: Some("Album".into()),
            title: Some("Title".into()),
            track_no: Some(2),
            disc_no: Some(1),
            year: Some(2026),
            genre: None,
            has_artwork: None,
        });
        let original = scan_snapshot_hash(&[first.clone(), second.clone()]);
        assert_eq!(
            original,
            scan_snapshot_hash(&[second.clone(), first.clone()])
        );
        second.metadata.as_mut().unwrap().title = Some("Changed".into());
        assert_ne!(original, scan_snapshot_hash(&[first, second]));
    }
}
