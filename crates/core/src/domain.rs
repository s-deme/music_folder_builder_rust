use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::execution::{OperationAction, OperationResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Partial,
    RecoveryRequired,
    Archived,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Warning,
    Blocking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDisposition {
    Executable,
    Skip,
    #[default]
    Blocked,
}

/// Version of the persisted `NamingRules` authorization surface.
///
/// This is deliberately independent from the Plan snapshot encoder version:
/// adding a rule changes how a target is interpreted even when no Plan item
/// column changes.
pub const NAMING_RULES_SCHEMA_VERSION: u32 = 1;
pub const PLAN_ISSUES_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NamingRulesSchemaError {
    #[error("legacy naming-rules schema is not executable")]
    Legacy,
    #[error("unsupported naming-rules schema version: {found} (supported: {supported})")]
    Unsupported { found: u32, supported: u32 },
}

pub const fn require_current_naming_rules_schema(
    version: u32,
) -> Result<(), NamingRulesSchemaError> {
    if version == NAMING_RULES_SCHEMA_VERSION {
        Ok(())
    } else if version == 0 {
        Err(NamingRulesSchemaError::Legacy)
    } else {
        Err(NamingRulesSchemaError::Unsupported {
            found: version,
            supported: NAMING_RULES_SCHEMA_VERSION,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanIssueCode {
    InvalidTarget,
    PathTooLong,
    Conflict,
    MetadataMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanIssue {
    pub severity: IssueSeverity,
    pub code: PlanIssueCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl PlanIssue {
    pub fn from_diagnostic(
        risk: Risk,
        detail: Option<&str>,
        disposition: ExecutionDisposition,
    ) -> Option<Self> {
        let code = match risk {
            Risk::None => return None,
            Risk::InvalidTarget => PlanIssueCode::InvalidTarget,
            Risk::PathTooLong => PlanIssueCode::PathTooLong,
            Risk::Conflict => PlanIssueCode::Conflict,
            Risk::MetadataMissing => PlanIssueCode::MetadataMissing,
        };
        let severity = if disposition == ExecutionDisposition::Blocked {
            IssueSeverity::Blocking
        } else {
            risk.issue_severity().unwrap_or(IssueSeverity::Warning)
        };
        Some(Self {
            severity,
            code,
            detail: detail.map(str::to_owned),
        })
    }
}

impl Risk {
    /// Returns the severity of this diagnostic independently from an item's action.
    ///
    /// `MetadataMissing` is intentionally a warning: naming rules may explicitly
    /// allow a fallback target for a file without readable metadata.
    pub const fn issue_severity(self) -> Option<IssueSeverity> {
        match self {
            Self::None => None,
            Self::MetadataMissing => Some(IssueSeverity::Warning),
            Self::InvalidTarget | Self::PathTooLong | Self::Conflict => {
                Some(IssueSeverity::Blocking)
            }
        }
    }

    pub const fn is_blocking(self) -> bool {
        matches!(self.issue_severity(), Some(IssueSeverity::Blocking))
    }

    pub const fn execution_disposition(self, action: PlanAction) -> ExecutionDisposition {
        if matches!(action, PlanAction::Skip) {
            ExecutionDisposition::Skip
        } else if self.is_blocking() {
            ExecutionDisposition::Blocked
        } else {
            ExecutionDisposition::Executable
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub size_bytes: u64,
    pub mtime_ns: i128,
    #[serde(default)]
    pub content_sha256: Option<String>,
    #[serde(default)]
    pub file_identity: Option<String>,
    #[serde(default = "default_fingerprint_version")]
    pub version: u16,
}

const fn default_fingerprint_version() -> u16 {
    1
}

impl FileFingerprint {
    pub const fn legacy(size_bytes: u64, mtime_ns: i128) -> Self {
        Self {
            size_bytes,
            mtime_ns,
            content_sha256: None,
            file_identity: None,
            version: 1,
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    Music,
    Image,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedFile {
    pub id: Uuid,
    pub path: PathBuf,
    pub fingerprint: FileFingerprint,
    pub metadata: Option<TrackMetadata>,
    pub kind: FileKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamingRules {
    pub artist_dir_template: String,
    pub album_dir_template: String,
    pub disc_dir_template: String,
    pub filename_template: String,
    pub duplicate_suffix_template: String,
    pub use_source_filename: bool,
    pub use_source_image_filename: bool,
    #[serde(default)]
    pub allow_missing_metadata: bool,
    #[serde(default)]
    pub allow_long_paths: bool,
    #[serde(default)]
    pub duplicate_strategy: DuplicateStrategy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateStrategy {
    #[default]
    Legacy,
    Skip,
    Sequence,
    Template,
}

impl Default for NamingRules {
    fn default() -> Self {
        Self {
            artist_dir_template: "{album_artist}".into(),
            album_dir_template: "{album}".into(),
            disc_dir_template: "[{disc_no:02d}]".into(),
            filename_template: "[{track_no:02d}_]{title}{extension}".into(),
            duplicate_suffix_template: "".into(),
            use_source_filename: false,
            use_source_image_filename: false,
            allow_missing_metadata: false,
            allow_long_paths: false,
            duplicate_strategy: DuplicateStrategy::Skip,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamingIssue {
    pub field: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamingPreview {
    pub relative_path: PathBuf,
    pub issues: Vec<NamingIssue>,
}

const NAMING_FIELDS: &[&str] = &[
    "artist",
    "album_artist",
    "album",
    "title",
    "track_no",
    "disc_no",
    "year",
    "extension",
    "source_stem",
];

pub fn validate_naming_rules(rules: &NamingRules) -> Vec<NamingIssue> {
    let mut issues = Vec::new();
    let templates = [
        (
            "artist_dir_template",
            rules.artist_dir_template.as_str(),
            true,
        ),
        (
            "album_dir_template",
            rules.album_dir_template.as_str(),
            true,
        ),
        ("disc_dir_template", rules.disc_dir_template.as_str(), false),
        (
            "filename_template",
            rules.filename_template.as_str(),
            !rules.use_source_filename,
        ),
    ];
    for (field, template, required) in templates {
        if required && template.trim().is_empty() {
            issues.push(issue(field, "empty_required", "必須のテンプレートが空です"));
        }
        validate_template(field, template, &mut issues);
    }
    if rules.duplicate_strategy == DuplicateStrategy::Template
        || (rules.duplicate_strategy == DuplicateStrategy::Legacy
            && !rules.duplicate_suffix_template.is_empty())
    {
        if rules.duplicate_suffix_template.trim().is_empty() {
            issues.push(issue(
                "duplicate_suffix_template",
                "empty_required",
                "カスタム重複末尾を入力してください",
            ));
        }
        validate_template(
            "duplicate_suffix_template",
            &rules.duplicate_suffix_template,
            &mut issues,
        );
    }
    issues
}

pub fn preview_naming(rules: &NamingRules, metadata: &TrackMetadata) -> NamingPreview {
    let mut issues = validate_naming_rules(rules);
    let source_stem = "source_track";
    let extension = ".flac";
    let filename = if rules.use_source_filename {
        format!("{source_stem}{extension}")
    } else {
        render_template(&rules.filename_template, metadata, source_stem, extension)
    };
    let mut path = PathBuf::new();
    for (field, template, optional) in [
        (
            "artist_dir_template",
            rules.artist_dir_template.as_str(),
            false,
        ),
        (
            "album_dir_template",
            rules.album_dir_template.as_str(),
            false,
        ),
        ("disc_dir_template", rules.disc_dir_template.as_str(), true),
    ] {
        let value = render_template(template, metadata, source_stem, extension);
        let value = value.trim_matches([' ', '.']);
        if value.is_empty() {
            if !optional {
                issues.push(issue(
                    field,
                    "empty_component",
                    "生成されるフォルダ名が空です",
                ));
            }
        } else {
            path.push(sanitize_component(value));
        }
    }
    if filename.trim().is_empty() {
        issues.push(issue(
            "filename_template",
            "empty_component",
            "生成されるファイル名が空です",
        ));
    } else {
        path.push(sanitize_component(&filename));
    }
    if let Err(error) = assess_windows_path(&path) {
        issues.push(issue("path", "unsafe_path", &error.to_string()));
    }
    NamingPreview {
        relative_path: path,
        issues,
    }
}

fn issue(field: &str, code: &str, message: &str) -> NamingIssue {
    NamingIssue {
        field: field.into(),
        code: code.into(),
        message: message.into(),
    }
}

fn validate_template(field: &str, template: &str, issues: &mut Vec<NamingIssue>) {
    if template.chars().filter(|c| *c == '{').count()
        != template.chars().filter(|c| *c == '}').count()
        || template.chars().filter(|c| *c == '[').count()
            != template.chars().filter(|c| *c == ']').count()
    {
        issues.push(issue(
            field,
            "unbalanced_delimiter",
            "{} または [] が閉じていません",
        ));
        return;
    }
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start + 1..].find('}') else {
            break;
        };
        let token = &rest[start + 1..start + 1 + end];
        let name = token.split(':').next().unwrap_or_default();
        if !NAMING_FIELDS.contains(&name) {
            issues.push(issue(
                field,
                "unknown_field",
                &format!("未知のフィールド: {name}"),
            ));
        }
        rest = &rest[start + end + 2..];
    }
}

pub fn render_template(
    template: &str,
    values: &TrackMetadata,
    source_stem: &str,
    extension: &str,
) -> String {
    fn field(
        name: &str,
        spec: Option<&str>,
        values: &TrackMetadata,
        source_stem: &str,
        extension: &str,
    ) -> Option<String> {
        let text = match name {
            "artist" => values.artist.clone(),
            "album_artist" => values
                .album_artist
                .clone()
                .or_else(|| values.artist.clone()),
            "album" => values.album.clone(),
            "title" => values.title.clone().or_else(|| Some(source_stem.into())),
            "source_stem" => Some(source_stem.into()),
            "extension" => Some(extension.into()),
            "track_no" => values.track_no.map(|v| v.to_string()),
            "disc_no" => values.disc_no.map(|v| v.to_string()),
            "year" => values.year.map(|v| v.to_string()),
            _ => None,
        }?;
        if let (Some(spec), Ok(number)) = (spec, text.parse::<u32>()) {
            if let Some(width) = spec
                .strip_prefix('0')
                .and_then(|s| s.trim_end_matches('d').parse::<usize>().ok())
            {
                return Some(format!("{number:0width$}"));
            }
        }
        Some(text)
    }
    fn render(
        input: &str,
        values: &TrackMetadata,
        stem: &str,
        ext: &str,
        optional: bool,
    ) -> (String, bool) {
        let mut out = String::new();
        let mut used = false;
        let chars: Vec<char> = input.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '{' {
                if let Some(end) = chars[i + 1..].iter().position(|c| *c == '}') {
                    let token: String = chars[i + 1..i + 1 + end].iter().collect();
                    let mut parts = token.splitn(2, ':');
                    let value = field(
                        parts.next().unwrap_or_default(),
                        parts.next(),
                        values,
                        stem,
                        ext,
                    );
                    if let Some(value) = value {
                        used = true;
                        out.push_str(&value);
                    } else if !optional { /* missing fields render empty */
                    }
                    i += end + 2;
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        (out, used)
    }
    let mut remaining = template.to_string();
    while let Some(start) = remaining.find('[') {
        let Some(relative_end) = remaining[start + 1..].find(']') else {
            break;
        };
        let end = start + 1 + relative_end;
        let (body, used) = render(
            &remaining[start + 1..end],
            values,
            source_stem,
            extension,
            true,
        );
        remaining.replace_range(start..=end, if used { &body } else { "" });
    }
    render(&remaining, values, source_stem, extension, false).0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanConflictCandidate {
    pub target_directory: PathBuf,
    pub music_item_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: Uuid,
    pub conflict_group_id: Option<Uuid>,
    pub ordinal: u64,
    pub file: ScannedFile,
    pub target: Option<PathBuf>,
    pub action: PlanAction,
    /// The authoritative mutation eligibility. It must never be inferred from
    /// `risk`, which is diagnostic and may be only a warning.
    #[serde(default)]
    pub disposition: ExecutionDisposition,
    pub risk: Risk,
    pub reason: Option<String>,
    #[serde(default)]
    pub issues: Vec<PlanIssue>,
    #[serde(default)]
    pub conflict_candidates: Vec<PlanConflictCandidate>,
}

impl PlanItem {
    pub const fn execution_disposition(&self) -> ExecutionDisposition {
        self.disposition
    }

    pub fn set_outcome(
        &mut self,
        action: PlanAction,
        disposition: ExecutionDisposition,
        risk: Risk,
        reason: Option<String>,
    ) {
        self.action = action;
        self.disposition = disposition;
        self.risk = risk;
        self.reason = reason;
        self.issues = PlanIssue::from_diagnostic(risk, self.reason.as_deref(), disposition)
            .into_iter()
            .collect();
    }

    pub fn validate_execution_contract(&self) -> Result<(), &'static str> {
        match self.disposition {
            ExecutionDisposition::Executable => {
                if self.action != PlanAction::Move {
                    return Err("executable_plan_item_action_invalid");
                }
                if self.target.is_none() {
                    return Err("executable_plan_item_target_missing");
                }
                if self
                    .issues
                    .iter()
                    .any(|issue| issue.severity == IssueSeverity::Blocking)
                {
                    return Err("executable_plan_item_has_blocking_issue");
                }
            }
            ExecutionDisposition::Skip => {
                if self.action != PlanAction::Skip {
                    return Err("skipped_plan_item_action_invalid");
                }
            }
            ExecutionDisposition::Blocked => {
                if !self
                    .issues
                    .iter()
                    .any(|issue| issue.severity == IssueSeverity::Blocking)
                {
                    return Err("blocked_plan_item_issue_missing");
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ApplyItem {
    pub plan_item_id: String,
    pub ordinal: u64,
    pub source: PathBuf,
    pub target: Option<PathBuf>,
    pub action: PlanAction,
    pub disposition: ExecutionDisposition,
    pub risk: Risk,
    pub reason: Option<String>,
    pub issues: Vec<PlanIssue>,
    pub source_fingerprint: Option<FileFingerprint>,
    pub target_root: Option<PathBuf>,
    pub allow_long_paths: bool,
}

impl ApplyItem {
    pub const fn execution_disposition(&self) -> ExecutionDisposition {
        self.disposition
    }
}

#[derive(Debug, Clone)]
pub struct OperationLog {
    pub plan_item_id: String,
    pub sequence_no: u64,
    pub source: PathBuf,
    pub target: Option<PathBuf>,
    pub action: OperationAction,
    pub result: OperationResult,
    pub error: Option<String>,
    pub source_deleted: bool,
    pub expected_size: Option<u64>,
    pub expected_content_sha256: Option<String>,
    pub expected_file_identity: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VerifyItem {
    pub operation_id: String,
    pub sequence_no: u64,
    pub source: PathBuf,
    pub target: Option<PathBuf>,
    pub action: OperationAction,
    pub expected_size: Option<u64>,
    pub expected_content_sha256: Option<String>,
    pub expected_file_identity: Option<String>,
}

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("パスが空です")]
    EmptyPath,
    #[error("パス全体が長すぎます: {actual}文字（上限{limit}文字）")]
    PathTooLong { actual: usize, limit: usize },
    #[error("a completed plan is required")]
    PlanNotApplicable,
    #[error("source and target are identical")]
    SamePath,
    #[error("target root must be a fully-qualified absolute path")]
    TargetRootNotAbsolute,
    #[error("device and extended-length path prefixes are not accepted")]
    AmbiguousPathPrefix,
    #[error("target path is outside the configured target root")]
    TargetOutsideRoot,
    #[error("target path must identify an item below the target root")]
    TargetEqualsRoot,
    #[error("path cannot be represented losslessly by this platform")]
    PathEncodingUnsupported,
    #[error("invalid target path component: {reason}")]
    InvalidPathComponent { reason: PathComponentViolation },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(rename_all = "snake_case")]
pub enum PathComponentViolation {
    #[error("component is empty")]
    Empty,
    #[error("absolute or prefixed component is not allowed")]
    AbsoluteOrPrefixed,
    #[error("current-directory component is not allowed")]
    CurrentDirectory,
    #[error("parent-directory component is not allowed")]
    ParentDirectory,
    #[error("a path separator is not allowed inside a component")]
    Separator,
    #[error("NUL is not allowed")]
    Nul,
    #[error("control characters are not allowed")]
    ControlCharacter,
    #[error("alternate data stream syntax is not allowed")]
    AlternateDataStream,
    #[error("a Windows-forbidden character is not allowed")]
    ForbiddenCharacter,
    #[error("a Windows reserved name is not allowed")]
    ReservedName,
    #[error("a component may not end in a space or period")]
    TrailingSpaceOrPeriod,
}

impl DomainError {
    pub fn reason_code(&self) -> String {
        match self {
            Self::EmptyPath => "empty_path".into(),
            Self::PathTooLong { actual, limit } => {
                format!("path_too_long:{actual}:{limit}")
            }
            Self::PlanNotApplicable => "plan_not_applicable".into(),
            Self::SamePath => "source_equals_target".into(),
            Self::TargetRootNotAbsolute => "target_root_not_absolute".into(),
            Self::AmbiguousPathPrefix => "ambiguous_path_prefix".into(),
            Self::TargetOutsideRoot => "target_outside_root".into(),
            Self::TargetEqualsRoot => "target_equals_root".into(),
            Self::PathEncodingUnsupported => "path_encoding_unsupported".into(),
            Self::InvalidPathComponent { reason } => {
                format!("invalid_path_component:{}", reason.reason_code())
            }
        }
    }
}

impl PathComponentViolation {
    pub const fn reason_code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::AbsoluteOrPrefixed => "absolute_or_prefixed",
            Self::CurrentDirectory => "current_directory",
            Self::ParentDirectory => "parent_directory",
            Self::Separator => "separator",
            Self::Nul => "nul",
            Self::ControlCharacter => "control_character",
            Self::AlternateDataStream => "alternate_data_stream",
            Self::ForbiddenCharacter => "forbidden_character",
            Self::ReservedName => "reserved_name",
            Self::TrailingSpaceOrPeriod => "trailing_space_or_period",
        }
    }
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
            if INVALID_WINDOWS_CHARS.contains(&c) || c == '\0' || c.is_control() {
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
    crate::path_policy::assess_windows_path_with_options(path, false)
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
    fn path_length_limit_is_inclusive_and_reports_actual_and_limit() {
        assert!(assess_windows_path(Path::new(&"a".repeat(80))).is_ok());
        let path = ["a".repeat(80), "b".repeat(80), "c".repeat(78)].join("/");
        assert_eq!(path.chars().count(), 240);
        assert!(assess_windows_path(Path::new(&path)).is_ok());

        let path = format!("{path}d");
        let error = assess_windows_path(Path::new(&path)).unwrap_err();
        assert_eq!(error.reason_code(), "path_too_long:241:240");
        assert_eq!(
            error.to_string(),
            "パス全体が長すぎます: 241文字（上限240文字）"
        );
    }

    #[test]
    fn component_over_80_is_allowed_when_the_whole_path_is_within_limit() {
        assert!(assess_windows_path(Path::new(&"a".repeat(239))).is_ok());
    }
    #[test]
    fn renders_optional_numbered_template_with_album_artist_fallback() {
        let metadata = TrackMetadata {
            artist: Some("Artist".into()),
            album_artist: None,
            album: Some("Album".into()),
            title: Some("Song".into()),
            track_no: Some(3),
            disc_no: None,
            year: Some(2024),
        };
        assert_eq!(
            render_template(
                "{album_artist}/[{disc_no:02d}-]{track_no:02d}_{title}{extension}",
                &metadata,
                "source",
                ".mp3"
            ),
            "Artist/03_Song.mp3"
        );
    }
    #[test]
    fn validates_unknown_fields_and_previews_a_path() {
        let metadata = TrackMetadata {
            artist: Some("Artist".into()),
            album_artist: None,
            album: Some("Album".into()),
            title: Some("Song".into()),
            track_no: Some(3),
            disc_no: Some(1),
            year: Some(2026),
        };
        let preview = preview_naming(&NamingRules::default(), &metadata);
        assert!(preview.issues.is_empty());
        assert_eq!(
            preview.relative_path,
            PathBuf::from("Artist/Album/01/03_Song.flac")
        );
        let invalid = NamingRules {
            filename_template: "{unknown}".into(),
            ..NamingRules::default()
        };
        assert_eq!(validate_naming_rules(&invalid)[0].code, "unknown_field");
    }
    #[test]
    fn old_naming_json_defaults_duplicate_strategy_to_skip() {
        let json = r#"{"artist_dir_template":"{artist}","album_dir_template":"{album}","disc_dir_template":"","filename_template":"{title}{extension}","duplicate_suffix_template":"","use_source_filename":false,"use_source_image_filename":false}"#;
        let rules: NamingRules = serde_json::from_str(json).expect("old naming rules");
        assert_eq!(rules.duplicate_strategy, DuplicateStrategy::Legacy);
        assert!(!rules.allow_missing_metadata);
        assert!(!rules.allow_long_paths);
    }

    #[test]
    fn risk_severity_does_not_make_metadata_warning_blocking() {
        assert_eq!(
            Risk::MetadataMissing.issue_severity(),
            Some(IssueSeverity::Warning)
        );
        assert!(!Risk::MetadataMissing.is_blocking());
        assert_eq!(
            Risk::MetadataMissing.execution_disposition(PlanAction::Move),
            ExecutionDisposition::Executable
        );
        assert_eq!(
            Risk::Conflict.execution_disposition(PlanAction::Move),
            ExecutionDisposition::Blocked
        );
        assert_eq!(
            Risk::None.execution_disposition(PlanAction::Skip),
            ExecutionDisposition::Skip
        );
        assert_eq!(
            ExecutionDisposition::default(),
            ExecutionDisposition::Blocked
        );
    }

    #[test]
    fn naming_rules_schema_rejects_legacy_and_unknown_versions() {
        assert!(require_current_naming_rules_schema(NAMING_RULES_SCHEMA_VERSION).is_ok());
        assert_eq!(
            require_current_naming_rules_schema(0),
            Err(NamingRulesSchemaError::Legacy)
        );
        assert_eq!(
            require_current_naming_rules_schema(NAMING_RULES_SCHEMA_VERSION + 1),
            Err(NamingRulesSchemaError::Unsupported {
                found: NAMING_RULES_SCHEMA_VERSION + 1,
                supported: NAMING_RULES_SCHEMA_VERSION,
            })
        );
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
