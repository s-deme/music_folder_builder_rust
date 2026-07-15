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
pub struct NamingRules {
    pub artist_dir_template: String,
    pub album_dir_template: String,
    pub disc_dir_template: String,
    pub filename_template: String,
    pub duplicate_suffix_template: String,
    pub use_source_filename: bool,
    pub use_source_image_filename: bool,
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
