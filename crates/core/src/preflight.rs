use crate::{
    ports::{FileMutator, RecoveryItem},
    ApplyItem, ExecutionDisposition, FileFingerprint, IssueSeverity, JournalState, SafeTargetPath,
    VerifyItem,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightKind {
    Apply,
    Rollback,
    Recovery,
}

impl PreflightKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Rollback => "rollback",
            Self::Recovery => "recovery",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightMode {
    DryRun,
    Mutation,
}

impl PreflightMode {
    pub const fn from_dry_run(dry_run: bool) -> Self {
        if dry_run {
            Self::DryRun
        } else {
            Self::Mutation
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DryRun => "dry_run",
            Self::Mutation => "mutation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightOutcome {
    Passed,
    Blocked,
    Failed,
}

impl PreflightOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightEvidence {
    pub size_bytes: u64,
    pub mtime_ns: i128,
    pub content_sha256: String,
    pub file_identity: Option<String>,
    pub fingerprint_version: u16,
}

#[derive(Debug, Clone)]
pub struct PreflightLog {
    pub item_id: String,
    pub source: PathBuf,
    pub target: Option<PathBuf>,
    pub outcome: PreflightOutcome,
    pub code: Option<String>,
    pub expected: Option<FileFingerprint>,
    pub observed: Option<FileFingerprint>,
}

impl PreflightLog {
    pub fn passed(
        item_id: impl Into<String>,
        source: PathBuf,
        target: Option<PathBuf>,
        expected: Option<FileFingerprint>,
        evidence: &PreflightEvidence,
    ) -> Self {
        Self {
            item_id: item_id.into(),
            source,
            target,
            outcome: PreflightOutcome::Passed,
            code: None,
            expected,
            observed: Some(FileFingerprint {
                size_bytes: evidence.size_bytes,
                mtime_ns: evidence.mtime_ns,
                content_sha256: Some(evidence.content_sha256.clone()),
                file_identity: evidence.file_identity.clone(),
                version: evidence.fingerprint_version,
            }),
        }
    }

    pub fn blocked(
        item_id: impl Into<String>,
        source: PathBuf,
        target: Option<PathBuf>,
        expected: Option<FileFingerprint>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            item_id: item_id.into(),
            source,
            target,
            outcome: PreflightOutcome::Blocked,
            code: Some(code.into()),
            expected,
            observed: None,
        }
    }
}

/// Framework-independent validation pipeline shared by dry-run and mutation.
///
/// The adapter still has to repeat handle-bound checks in its destructive
/// primitive.  A successful report is evidence about the observation at this
/// boundary, never permission to skip the final no-follow/identity check.
pub struct PreflightEngine<'a, F> {
    files: &'a F,
}

impl<'a, F: FileMutator> PreflightEngine<'a, F> {
    pub const fn new(files: &'a F) -> Self {
        Self { files }
    }

    pub fn apply(&self, item: &ApplyItem) -> Result<PreflightEvidence, String> {
        if item.execution_disposition() != ExecutionDisposition::Executable {
            return Err("plan_item_not_executable".into());
        }
        if item
            .issues
            .iter()
            .any(|issue| issue.severity == IssueSeverity::Blocking)
        {
            return Err("plan_item_has_blocking_issue".into());
        }
        let target = item
            .target
            .as_deref()
            .ok_or_else(|| "target_missing".to_string())?;
        let target_root = item
            .target_root
            .as_deref()
            .ok_or_else(|| "plan_target_root_missing".to_string())?;
        SafeTargetPath::from_completed_target(target_root, target, item.allow_long_paths)
            .map_err(|error| error.reason_code())?;
        let expected = item
            .source_fingerprint
            .as_ref()
            .ok_or_else(|| "plan_source_expectation_missing".to_string())?;
        let evidence = self.move_between(&item.source, target, Some(expected))?;
        if evidence.content_sha256
            != expected
                .content_sha256
                .as_deref()
                .ok_or_else(|| "plan_source_content_hash_missing".to_string())?
        {
            return Err("source_changed_since_scan".into());
        }
        Ok(evidence)
    }

    pub fn rollback(&self, item: &VerifyItem) -> Result<PreflightEvidence, String> {
        let source = item
            .target
            .as_deref()
            .ok_or_else(|| "target_missing_in_log".to_string())?;
        let expected = FileFingerprint {
            size_bytes: item
                .expected_size
                .ok_or_else(|| "rollback_source_size_missing".to_string())?,
            mtime_ns: 0,
            content_sha256: Some(
                item.expected_content_sha256
                    .clone()
                    .ok_or_else(|| "rollback_source_hash_missing".to_string())?,
            ),
            file_identity: item.expected_file_identity.clone(),
            version: 1,
        };
        self.move_between(source, &item.source, Some(&expected))
    }

    /// Validates the compensating delete used when publication succeeded but
    /// the original source was deliberately retained. Both copies must still
    /// be ordinary files with the exact expected bytes.
    pub fn duplicate_cleanup(&self, item: &VerifyItem) -> Result<PreflightEvidence, String> {
        let applied_target = item
            .target
            .as_deref()
            .ok_or_else(|| "target_missing_in_log".to_string())?;
        if !self.files.exists(&item.source) || !self.files.exists(applied_target) {
            return Err("partial_copy_path_missing".into());
        }
        self.guard_paths([item.source.as_path(), applied_target])?;
        let original = self.files.mutation_fingerprint(&item.source)?;
        let published = self.files.mutation_fingerprint(applied_target)?;
        let expected_size = item
            .expected_size
            .ok_or_else(|| "partial_copy_expected_size_missing".to_string())?;
        let expected_hash = item
            .expected_content_sha256
            .as_deref()
            .ok_or_else(|| "partial_copy_expected_hash_missing".to_string())?;
        if original.size_bytes != expected_size
            || published.size_bytes != expected_size
            || original.content_sha256.as_deref() != Some(expected_hash)
            || published.content_sha256.as_deref() != Some(expected_hash)
        {
            return Err("source_changed_after_partial_copy".into());
        }
        Ok(PreflightEvidence {
            size_bytes: published.size_bytes,
            mtime_ns: published.mtime_ns,
            content_sha256: expected_hash.to_owned(),
            file_identity: published.file_identity,
            fingerprint_version: published.version,
        })
    }

    pub fn move_between(
        &self,
        source: &Path,
        target: &Path,
        expected: Option<&FileFingerprint>,
    ) -> Result<PreflightEvidence, String> {
        if !self.files.exists(source) {
            return Err("source_missing".into());
        }
        self.guard_paths([source, target])?;
        if self.files.exists(target) {
            return Err("target_already_exists".into());
        }
        let observed = self.files.mutation_fingerprint(source)?;
        let content_sha256 = observed
            .content_sha256
            .clone()
            .ok_or_else(|| "content_hash_not_supported".to_string())?;
        if let Some(expected) = expected {
            let content_matches = expected
                .content_sha256
                .as_deref()
                .is_some_and(|hash| hash == content_sha256);
            if observed.size_bytes != expected.size_bytes
                || (expected.mtime_ns != 0 && observed.mtime_ns != expected.mtime_ns)
                || !content_matches
                || expected.file_identity.is_some()
                    && observed.file_identity != expected.file_identity
            {
                return Err("source_changed_since_scan".into());
            }
        }
        Ok(PreflightEvidence {
            size_bytes: observed.size_bytes,
            mtime_ns: observed.mtime_ns,
            content_sha256,
            file_identity: observed.file_identity,
            fingerprint_version: observed.version,
        })
    }

    pub fn recovery_paths(&self, item: &RecoveryItem) -> Result<(), String> {
        let mut paths = vec![item.source.as_path(), item.target.as_path()];
        if !item.temporary.as_os_str().is_empty() {
            paths.push(item.temporary.as_path());
        }
        self.guard_paths(paths)
    }

    pub fn guard_paths<'p>(&self, paths: impl IntoIterator<Item = &'p Path>) -> Result<(), String> {
        for path in paths {
            self.files.ensure_no_reparse_points(path)?;
        }
        Ok(())
    }
}

/// Core-owned journal state machine. Adapters must reject every transition
/// outside this exhaustive table before attempting a compare-and-swap write.
pub const fn journal_transition_allowed(from: JournalState, to: JournalState) -> bool {
    use JournalState::{
        Completed, ContentVerified, Failed, Prepared, Published, RecoveryRequired, SourceDeleted,
        Staged, Staging,
    };
    matches!(
        (from, to),
        (
            Prepared,
            Staging | ContentVerified | Failed | RecoveryRequired
        ) | (Staging, Staged | Failed | RecoveryRequired)
            | (Staged, ContentVerified | Failed | RecoveryRequired)
            | (
                ContentVerified,
                Published | SourceDeleted | Failed | RecoveryRequired
            )
            | (Published, SourceDeleted | Failed | RecoveryRequired)
            | (SourceDeleted, Completed | Failed | RecoveryRequired)
            | (
                RecoveryRequired,
                Staging
                    | Staged
                    | ContentVerified
                    | Published
                    | SourceDeleted
                    | Completed
                    | Failed
                    | RecoveryRequired
            )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileFingerprint, PlanAction, Risk};

    struct MatrixFiles {
        source_exists: bool,
        target_exists: bool,
        reparse_error: bool,
        observed: FileFingerprint,
    }

    impl FileMutator for MatrixFiles {
        fn exists(&self, path: &Path) -> bool {
            if path == Path::new(r"C:\source\track.mp3") {
                self.source_exists
            } else if path == Path::new(r"C:\target\track.mp3") {
                self.target_exists
            } else {
                false
            }
        }

        fn same_volume(&self, _: &Path, _: &Path) -> Result<bool, String> {
            Ok(true)
        }

        fn move_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            unreachable!()
        }

        fn copy_file(&self, _: &Path, _: &Path) -> Result<(), String> {
            unreachable!()
        }

        fn size(&self, _: &Path) -> Result<u64, String> {
            Ok(self.observed.size_bytes)
        }

        fn delete_file(&self, _: &Path) -> Result<(), String> {
            unreachable!()
        }

        fn mutation_fingerprint(&self, _: &Path) -> Result<FileFingerprint, String> {
            Ok(self.observed.clone())
        }

        fn ensure_no_reparse_points(&self, _: &Path) -> Result<(), String> {
            if self.reparse_error {
                Err("reparse_point_forbidden".into())
            } else {
                Ok(())
            }
        }
    }

    fn apply_item(expected: FileFingerprint) -> ApplyItem {
        ApplyItem {
            plan_item_id: "item".into(),
            ordinal: 1,
            source: PathBuf::from(r"C:\source\track.mp3"),
            target: Some(PathBuf::from(r"C:\target\track.mp3")),
            action: PlanAction::Move,
            disposition: ExecutionDisposition::Executable,
            risk: Risk::None,
            reason: None,
            issues: Vec::new(),
            source_fingerprint: Some(expected),
            target_root: Some(PathBuf::from(r"C:\target")),
            allow_long_paths: false,
        }
    }

    #[test]
    fn journal_state_machine_rejects_terminal_and_skipped_transitions() {
        assert!(journal_transition_allowed(
            JournalState::Prepared,
            JournalState::ContentVerified
        ));
        assert!(!journal_transition_allowed(
            JournalState::Prepared,
            JournalState::Published
        ));
        assert!(!journal_transition_allowed(
            JournalState::Completed,
            JournalState::RecoveryRequired
        ));
        assert!(!journal_transition_allowed(
            JournalState::Failed,
            JournalState::Prepared
        ));
    }

    #[test]
    fn dry_run_and_mutation_use_the_same_preflight_matrix() {
        let expected = FileFingerprint {
            size_bytes: 4,
            mtime_ns: 9,
            content_sha256: Some("hash".into()),
            file_identity: Some("identity".into()),
            version: 1,
        };
        let cases = [
            (
                MatrixFiles {
                    source_exists: true,
                    target_exists: false,
                    reparse_error: false,
                    observed: expected.clone(),
                },
                Ok("hash"),
            ),
            (
                MatrixFiles {
                    source_exists: false,
                    target_exists: false,
                    reparse_error: false,
                    observed: expected.clone(),
                },
                Err("source_missing"),
            ),
            (
                MatrixFiles {
                    source_exists: true,
                    target_exists: true,
                    reparse_error: false,
                    observed: expected.clone(),
                },
                Err("target_already_exists"),
            ),
            (
                MatrixFiles {
                    source_exists: true,
                    target_exists: false,
                    reparse_error: true,
                    observed: expected.clone(),
                },
                Err("reparse_point_forbidden"),
            ),
            (
                MatrixFiles {
                    source_exists: true,
                    target_exists: false,
                    reparse_error: false,
                    observed: FileFingerprint {
                        file_identity: Some("replacement".into()),
                        ..expected.clone()
                    },
                },
                Err("source_changed_since_scan"),
            ),
        ];
        for (files, expected_result) in cases {
            let item = apply_item(expected.clone());
            let observations = [PreflightMode::DryRun, PreflightMode::Mutation].map(|mode| {
                let _typed_mode = mode;
                PreflightEngine::new(&files)
                    .apply(&item)
                    .map(|evidence| evidence.content_sha256)
            });
            assert_eq!(observations[0], observations[1]);
            let observed = observations[0]
                .as_ref()
                .map(String::as_str)
                .map_err(String::as_str);
            match expected_result {
                Ok(hash) => assert_eq!(observed, Ok(hash)),
                Err(code) => assert_eq!(observed, Err(code)),
            }
        }
    }
}
