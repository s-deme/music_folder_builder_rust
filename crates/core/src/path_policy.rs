use std::char::decode_utf16;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    ApplyItem, DomainError, ExecutionDisposition, IssueSeverity, PathComponentViolation,
    PlanAction, PlanItem,
};

pub const WINDOWS_PATH_POLICY_VERSION: u16 = 1;
pub const WINDOWS_PATH_KEY_VERSION: u16 = 1;
pub const WINDOWS_COMPATIBLE_PATH_LIMIT: usize = 240;
pub const WINDOWS_EXTENDED_PATH_LIMIT: usize = 32_767;

/// A versioned, persistence-safe comparison key for Windows paths.
///
/// The key is encoded instead of being made with `to_string_lossy`, so an
/// unpaired UTF-16 surrogate on Windows cannot silently collide with the
/// replacement character used for display. This is a comparison key only;
/// filesystem operations must continue to use the original native path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowsPathKey(String);

impl WindowsPathKey {
    pub fn from_path(path: &Path) -> Self {
        let (encoding, units) = path_key_units(path);
        let normalized = normalize_path_key_units(&units);
        let mut encoded = format!("windows_path_key_v{WINDOWS_PATH_KEY_VERSION}:{encoding}:");
        for unit in normalized {
            use std::fmt::Write as _;
            let _ = write!(encoded, "{unit:04x}");
        }
        Self(encoded)
    }

    pub const fn version(&self) -> u16 {
        WINDOWS_PATH_KEY_VERSION
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for WindowsPathKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Backwards-compatible string boundary for existing SQLite and use-case code.
pub fn windows_path_key(path: &Path) -> String {
    WindowsPathKey::from_path(path).into_string()
}

/// Compares Windows paths by normalized, lossless code units and component
/// boundaries. This is suitable for authorization/lease scopes; display strings
/// and host-dependent `Path::starts_with` are not.
pub fn windows_path_is_same_or_descendant(parent: &Path, candidate: &Path) -> bool {
    let (_, parent) = path_key_units(parent);
    let (_, candidate) = path_key_units(candidate);
    let parent = normalize_path_key_units(&parent);
    let candidate = normalize_path_key_units(&candidate);
    candidate == parent
        || candidate.starts_with(&parent)
            && (parent.last().is_some_and(|unit| is_separator(*unit))
                || candidate
                    .get(parent.len())
                    .is_some_and(|unit| is_separator(*unit)))
}

pub fn windows_path_scopes_overlap(left: &Path, right: &Path) -> bool {
    windows_path_is_same_or_descendant(left, right)
        || windows_path_is_same_or_descendant(right, left)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetPathPolicy {
    allow_long_paths: bool,
}

impl TargetPathPolicy {
    pub const fn new(allow_long_paths: bool) -> Self {
        Self { allow_long_paths }
    }

    pub const fn allow_long_paths(self) -> bool {
        self.allow_long_paths
    }

    /// Builds a target structurally from already separated relative components.
    /// A component containing a separator is rejected rather than reinterpreted.
    pub fn from_relative_components<I, S>(
        self,
        target_root: &Path,
        components: I,
    ) -> Result<SafeTargetPath, DomainError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        validate_absolute_path(target_root, true)?;

        let mut target = target_root.to_path_buf();
        let mut relative_components = Vec::new();
        for component in components {
            let component = component.as_ref();
            validate_component(component)?;
            target.push(component);
            relative_components.push(component.to_os_string());
        }
        if relative_components.is_empty() {
            return Err(DomainError::TargetEqualsRoot);
        }

        let actual_utf16_units = validate_path_length(&target, self.allow_long_paths)?;
        Ok(SafeTargetPath::new(
            target_root.to_path_buf(),
            relative_components,
            target,
            actual_utf16_units,
            self.allow_long_paths,
        ))
    }

    /// Revalidates a fully built target (for example after a duplicate suffix).
    pub fn validate_completed_target(
        self,
        target_root: &Path,
        completed_target: &Path,
    ) -> Result<SafeTargetPath, DomainError> {
        self.validate_absolute_target(target_root, completed_target)
    }

    /// Validates an absolute target supplied by a plan revision boundary.
    pub fn validate_manual_absolute_target(
        self,
        target_root: &Path,
        manual_target: &Path,
    ) -> Result<SafeTargetPath, DomainError> {
        self.validate_absolute_target(target_root, manual_target)
    }

    fn validate_absolute_target(
        self,
        target_root: &Path,
        candidate: &Path,
    ) -> Result<SafeTargetPath, DomainError> {
        let root = validate_absolute_path(target_root, true)?;
        let target = validate_absolute_path(candidate, false)?;
        if root.prefix != target.prefix || target.components.len() < root.components.len() {
            return Err(DomainError::TargetOutsideRoot);
        }

        for (root_component, target_component) in
            root.components.iter().zip(target.components.iter())
        {
            if component_key(root_component)? != component_key(target_component)? {
                return Err(DomainError::TargetOutsideRoot);
            }
        }

        let relative_components = target.components[root.components.len()..].to_vec();
        if relative_components.is_empty() {
            return Err(DomainError::TargetEqualsRoot);
        }
        let actual_utf16_units = validate_path_length(candidate, self.allow_long_paths)?;
        Ok(SafeTargetPath::new(
            target_root.to_path_buf(),
            relative_components,
            candidate.to_path_buf(),
            actual_utf16_units,
            self.allow_long_paths,
        ))
    }
}

impl Default for TargetPathPolicy {
    fn default() -> Self {
        Self::new(false)
    }
}

/// Capability type proving that a target was validated against one absolute root.
///
/// Fields are deliberately private and this type does not implement Deserialize;
/// persisted paths have to pass through a constructor again before mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeTargetPath {
    target_root: PathBuf,
    relative_components: Vec<OsString>,
    target: PathBuf,
    comparison_key: WindowsPathKey,
    actual_utf16_units: usize,
    allow_long_paths: bool,
}

impl SafeTargetPath {
    fn new(
        target_root: PathBuf,
        relative_components: Vec<OsString>,
        target: PathBuf,
        actual_utf16_units: usize,
        allow_long_paths: bool,
    ) -> Self {
        let comparison_key = WindowsPathKey::from_path(&target);
        Self {
            target_root,
            relative_components,
            target,
            comparison_key,
            actual_utf16_units,
            allow_long_paths,
        }
    }

    pub fn from_relative_components<I, S>(
        target_root: &Path,
        components: I,
        allow_long_paths: bool,
    ) -> Result<Self, DomainError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        TargetPathPolicy::new(allow_long_paths).from_relative_components(target_root, components)
    }

    pub fn from_completed_target(
        target_root: &Path,
        completed_target: &Path,
        allow_long_paths: bool,
    ) -> Result<Self, DomainError> {
        TargetPathPolicy::new(allow_long_paths)
            .validate_completed_target(target_root, completed_target)
    }

    pub fn from_manual_absolute_target(
        target_root: &Path,
        manual_target: &Path,
        allow_long_paths: bool,
    ) -> Result<Self, DomainError> {
        TargetPathPolicy::new(allow_long_paths)
            .validate_manual_absolute_target(target_root, manual_target)
    }

    pub fn as_path(&self) -> &Path {
        &self.target
    }

    pub fn target_root(&self) -> &Path {
        &self.target_root
    }

    pub fn relative_components(&self) -> &[OsString] {
        &self.relative_components
    }

    pub fn comparison_key(&self) -> &WindowsPathKey {
        &self.comparison_key
    }

    pub const fn policy_version(&self) -> u16 {
        WINDOWS_PATH_POLICY_VERSION
    }

    pub const fn actual_utf16_units(&self) -> usize {
        self.actual_utf16_units
    }

    pub const fn allows_long_paths(&self) -> bool {
        self.allow_long_paths
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.target
    }
}

/// Adapter-boundary validation for an immutable Plan row. Only Core may
/// decide whether a diagnostic item is executable; adapters merely persist
/// the result and must re-prove the target capability before publishing it.
pub fn validate_plan_item_for_persistence(
    item: &PlanItem,
    target_root: &Path,
    allow_long_paths: bool,
) -> Result<(), String> {
    item.validate_execution_contract().map_err(str::to_owned)?;
    if item.disposition != ExecutionDisposition::Executable {
        return Ok(());
    }
    let target = item
        .target
        .as_deref()
        .ok_or_else(|| "executable_plan_item_target_missing".to_owned())?;
    SafeTargetPath::from_completed_target(target_root, target, allow_long_paths)
        .map(|_| ())
        .map_err(|error| format!("plan_item_target_unsafe:{}", error.reason_code()))
}

/// Rebuilds the target capability after loading an Apply row. This check is
/// intentionally repeated by preflight immediately before mutation.
pub fn validate_apply_item_target(item: &ApplyItem) -> Result<(), String> {
    if item.disposition != ExecutionDisposition::Executable {
        return Ok(());
    }
    if item.action != PlanAction::Move {
        return Err("executable_plan_item_action_invalid".into());
    }
    if item
        .issues
        .iter()
        .any(|issue| issue.severity == IssueSeverity::Blocking)
    {
        return Err("executable_plan_item_has_blocking_issue".into());
    }
    let target = item
        .target
        .as_deref()
        .ok_or_else(|| "executable_plan_item_target_missing".to_owned())?;
    let target_root = item
        .target_root
        .as_deref()
        .ok_or_else(|| "plan_target_root_missing".to_owned())?;
    SafeTargetPath::from_completed_target(target_root, target, item.allow_long_paths)
        .map(|_| ())
        .map_err(|error| format!("plan_item_target_unsafe:{}", error.reason_code()))
}

impl AsRef<Path> for SafeTargetPath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AbsolutePrefix {
    Drive(u16),
    Unc(Vec<u16>, Vec<u16>),
    #[cfg(not(windows))]
    Posix,
}

#[derive(Debug)]
struct ParsedAbsolutePath {
    prefix: AbsolutePrefix,
    components: Vec<OsString>,
}

fn validate_absolute_path(
    path: &Path,
    allow_trailing_separator: bool,
) -> Result<ParsedAbsolutePath, DomainError> {
    let units = path_utf16_units(path)?;
    if units.is_empty() {
        return Err(DomainError::EmptyPath);
    }
    if has_device_prefix(&units) {
        return Err(DomainError::AmbiguousPathPrefix);
    }

    let (prefix, mut cursor) =
        if units.len() >= 2 && is_ascii_letter(units[0]) && units[1] == u16::from(b':') {
            if units.get(2).is_none_or(|unit| !is_separator(*unit)) {
                return Err(DomainError::TargetRootNotAbsolute);
            }
            (AbsolutePrefix::Drive(ascii_upper(units[0])), 3)
        } else if units.len() >= 2 && is_separator(units[0]) && is_separator(units[1]) {
            let (server, next) = take_component_units(&units, 2)?;
            let share_start = next
                .checked_add(1)
                .filter(|index| *index <= units.len())
                .ok_or(DomainError::TargetRootNotAbsolute)?;
            let (share, next) = take_component_units(&units, share_start)?;
            validate_component_units(server)?;
            validate_component_units(share)?;
            let cursor = if next < units.len() { next + 1 } else { next };
            (
                AbsolutePrefix::Unc(
                    normalize_component_units(server),
                    normalize_component_units(share),
                ),
                cursor,
            )
        } else {
            #[cfg(not(windows))]
            {
                if units[0] == u16::from(b'/') {
                    (AbsolutePrefix::Posix, 1)
                } else {
                    return Err(DomainError::TargetRootNotAbsolute);
                }
            }
            #[cfg(windows)]
            {
                return Err(DomainError::TargetRootNotAbsolute);
            }
        };

    let mut components = Vec::new();
    while cursor < units.len() {
        let start = cursor;
        while cursor < units.len() && !is_separator(units[cursor]) {
            cursor += 1;
        }
        if start == cursor {
            return Err(invalid_component(PathComponentViolation::Empty));
        }
        let component_units = &units[start..cursor];
        validate_component_units(component_units)?;
        components.push(os_string_from_utf16(component_units)?);

        if cursor < units.len() {
            cursor += 1;
            if cursor == units.len() && !allow_trailing_separator {
                return Err(invalid_component(PathComponentViolation::Empty));
            }
        }
    }
    Ok(ParsedAbsolutePath { prefix, components })
}

fn take_component_units(units: &[u16], start: usize) -> Result<(&[u16], usize), DomainError> {
    if start >= units.len() || is_separator(units[start]) {
        return Err(DomainError::TargetRootNotAbsolute);
    }
    let mut end = start;
    while end < units.len() && !is_separator(units[end]) {
        end += 1;
    }
    if end == start {
        return Err(DomainError::TargetRootNotAbsolute);
    }
    Ok((&units[start..end], end))
}

fn validate_component(component: &OsStr) -> Result<(), DomainError> {
    validate_component_units(&os_str_utf16_units(component)?)
}

fn validate_component_units(units: &[u16]) -> Result<(), DomainError> {
    if units.is_empty() {
        return Err(invalid_component(PathComponentViolation::Empty));
    }
    if (units.len() >= 2 && is_ascii_letter(units[0]) && units[1] == u16::from(b':'))
        || units.first().is_some_and(|unit| is_separator(*unit))
    {
        return Err(invalid_component(
            PathComponentViolation::AbsoluteOrPrefixed,
        ));
    }
    if units == [u16::from(b'.')] {
        return Err(invalid_component(PathComponentViolation::CurrentDirectory));
    }
    if units == [u16::from(b'.'), u16::from(b'.')] {
        return Err(invalid_component(PathComponentViolation::ParentDirectory));
    }
    if units.iter().any(|unit| is_separator(*unit)) {
        return Err(invalid_component(PathComponentViolation::Separator));
    }
    if units.contains(&0) {
        return Err(invalid_component(PathComponentViolation::Nul));
    }
    if units.iter().any(|unit| *unit < 0x20) {
        return Err(invalid_component(PathComponentViolation::ControlCharacter));
    }
    if units.contains(&u16::from(b':')) {
        return Err(invalid_component(
            PathComponentViolation::AlternateDataStream,
        ));
    }
    if units.iter().any(|unit| {
        *unit == u16::from(b'<')
            || *unit == u16::from(b'>')
            || *unit == u16::from(b'"')
            || *unit == u16::from(b'|')
            || *unit == u16::from(b'?')
            || *unit == u16::from(b'*')
    }) {
        return Err(invalid_component(
            PathComponentViolation::ForbiddenCharacter,
        ));
    }
    if units
        .last()
        .is_some_and(|unit| *unit == u16::from(b' ') || *unit == u16::from(b'.'))
    {
        return Err(invalid_component(
            PathComponentViolation::TrailingSpaceOrPeriod,
        ));
    }
    if is_reserved_component(units) {
        return Err(invalid_component(PathComponentViolation::ReservedName));
    }
    Ok(())
}

fn invalid_component(reason: PathComponentViolation) -> DomainError {
    DomainError::InvalidPathComponent { reason }
}

fn is_reserved_component(units: &[u16]) -> bool {
    let stem = units
        .split(|unit| *unit == u16::from(b'.'))
        .next()
        .unwrap_or(units);
    let upper = stem.iter().copied().map(ascii_upper).collect::<Vec<_>>();
    const CON: &[u16] = &[b'C' as u16, b'O' as u16, b'N' as u16];
    const PRN: &[u16] = &[b'P' as u16, b'R' as u16, b'N' as u16];
    const AUX: &[u16] = &[b'A' as u16, b'U' as u16, b'X' as u16];
    const NUL: &[u16] = &[b'N' as u16, b'U' as u16, b'L' as u16];
    const CONIN: &[u16] = &[
        b'C' as u16,
        b'O' as u16,
        b'N' as u16,
        b'I' as u16,
        b'N' as u16,
        b'$' as u16,
    ];
    const CONOUT: &[u16] = &[
        b'C' as u16,
        b'O' as u16,
        b'N' as u16,
        b'O' as u16,
        b'U' as u16,
        b'T' as u16,
        b'$' as u16,
    ];
    if upper.as_slice() == CON
        || upper.as_slice() == PRN
        || upper.as_slice() == AUX
        || upper.as_slice() == NUL
        || upper.as_slice() == CONIN
        || upper.as_slice() == CONOUT
    {
        return true;
    }
    if upper.len() == 4
        && (upper.starts_with(&[b'C' as u16, b'O' as u16, b'M' as u16])
            || upper.starts_with(&[b'L' as u16, b'P' as u16, b'T' as u16]))
    {
        return matches!(upper[3], 0x31..=0x39 | 0x00b9 | 0x00b2 | 0x00b3);
    }
    false
}

fn validate_path_length(path: &Path, allow_long_paths: bool) -> Result<usize, DomainError> {
    let actual = path_utf16_units(path)?.len();
    let limit = if allow_long_paths {
        WINDOWS_EXTENDED_PATH_LIMIT
    } else {
        WINDOWS_COMPATIBLE_PATH_LIMIT
    };
    if actual > limit {
        return Err(DomainError::PathTooLong { actual, limit });
    }
    Ok(actual)
}

pub fn assess_windows_path_with_options(
    path: &Path,
    allow_long_paths: bool,
) -> Result<(), DomainError> {
    if path.as_os_str().is_empty() {
        return Err(DomainError::EmptyPath);
    }
    validate_path_length(path, allow_long_paths).map(|_| ())
}

fn component_key(component: &OsStr) -> Result<Vec<u16>, DomainError> {
    Ok(normalize_component_units(&os_str_utf16_units(component)?))
}

fn normalize_component_units(units: &[u16]) -> Vec<u16> {
    let end = units
        .iter()
        .rposition(|unit| *unit != u16::from(b' ') && *unit != u16::from(b'.'))
        .map_or(0, |position| position + 1);
    lowercase_utf16(&units[..end])
}

fn normalize_path_key_units(units: &[u16]) -> Vec<u16> {
    let leading_separators = units.iter().take_while(|unit| is_separator(**unit)).count();
    let mut components = Vec::new();
    let mut start = leading_separators;
    let mut cursor = start;
    while cursor <= units.len() {
        if cursor == units.len() || is_separator(units[cursor]) {
            if cursor > start {
                let original = &units[start..cursor];
                if original != [u16::from(b'.')] {
                    let component = normalize_component_units(original);
                    components.push(component);
                }
            }
            start = cursor.saturating_add(1);
        }
        cursor += 1;
    }

    let mut normalized = vec![u16::from(b'\\'); leading_separators.min(2)];
    for (index, component) in components.into_iter().enumerate() {
        if index > 0 {
            normalized.push(u16::from(b'\\'));
        }
        normalized.extend(component);
    }
    normalized
}

fn lowercase_utf16(units: &[u16]) -> Vec<u16> {
    let mut lowered = Vec::with_capacity(units.len());
    for decoded in decode_utf16(units.iter().copied()) {
        match decoded {
            Ok(character) => {
                for character in character.to_lowercase() {
                    let mut buffer = [0; 2];
                    lowered.extend_from_slice(character.encode_utf16(&mut buffer));
                }
            }
            Err(error) => lowered.push(error.unpaired_surrogate()),
        }
    }
    lowered
}

fn has_device_prefix(units: &[u16]) -> bool {
    let starts_two_separators = units.len() >= 4
        && is_separator(units[0])
        && is_separator(units[1])
        && matches!(units[2], unit if unit == u16::from(b'?') || unit == u16::from(b'.'))
        && is_separator(units[3]);
    let starts_nt_prefix = units.len() >= 4
        && is_separator(units[0])
        && units[1] == u16::from(b'?')
        && units[2] == u16::from(b'?')
        && is_separator(units[3]);
    let starts_double_nt_prefix = units.len() >= 5
        && is_separator(units[0])
        && is_separator(units[1])
        && units[2] == u16::from(b'?')
        && units[3] == u16::from(b'?')
        && is_separator(units[4]);
    starts_two_separators || starts_nt_prefix || starts_double_nt_prefix
}

fn is_separator(unit: u16) -> bool {
    unit == u16::from(b'\\') || unit == u16::from(b'/')
}

fn is_ascii_letter(unit: u16) -> bool {
    (u16::from(b'a')..=u16::from(b'z')).contains(&unit)
        || (u16::from(b'A')..=u16::from(b'Z')).contains(&unit)
}

fn ascii_upper(unit: u16) -> u16 {
    if (u16::from(b'a')..=u16::from(b'z')).contains(&unit) {
        unit - u16::from(b'a') + u16::from(b'A')
    } else {
        unit
    }
}

#[cfg(windows)]
fn os_str_utf16_units(value: &OsStr) -> Result<Vec<u16>, DomainError> {
    use std::os::windows::ffi::OsStrExt as _;
    Ok(value.encode_wide().collect())
}

#[cfg(not(windows))]
fn os_str_utf16_units(value: &OsStr) -> Result<Vec<u16>, DomainError> {
    value
        .to_str()
        .map(|value| value.encode_utf16().collect())
        .ok_or(DomainError::PathEncodingUnsupported)
}

fn path_utf16_units(path: &Path) -> Result<Vec<u16>, DomainError> {
    os_str_utf16_units(path.as_os_str())
}

#[cfg(windows)]
fn os_string_from_utf16(units: &[u16]) -> Result<OsString, DomainError> {
    use std::os::windows::ffi::OsStringExt as _;
    Ok(OsString::from_wide(units))
}

#[cfg(not(windows))]
fn os_string_from_utf16(units: &[u16]) -> Result<OsString, DomainError> {
    String::from_utf16(units)
        .map(OsString::from)
        .map_err(|_| DomainError::PathEncodingUnsupported)
}

#[cfg(windows)]
fn path_key_units(path: &Path) -> (&'static str, Vec<u16>) {
    use std::os::windows::ffi::OsStrExt as _;
    ("utf16", path.as_os_str().encode_wide().collect())
}

#[cfg(not(windows))]
fn path_key_units(path: &Path) -> (&'static str, Vec<u16>) {
    if let Some(path) = path.to_str() {
        return ("utf16", path.encode_utf16().collect());
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        return (
            "native_bytes",
            path.as_os_str()
                .as_bytes()
                .iter()
                .copied()
                .map(u16::from)
                .collect(),
        );
    }

    #[allow(unreachable_code)]
    ("unsupported", Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new(r"C:\Music")
    }

    #[test]
    fn normalizes_windows_separators_case_trailing_dots_and_dot_components() {
        assert_eq!(
            windows_path_key(Path::new("C:/Music/./Album./SONG.MP3")),
            windows_path_key(Path::new("c:\\music\\album\\song.mp3"))
        );
        assert!(windows_path_key(Path::new("C:/Music")).starts_with("windows_path_key_v1:"));
    }

    #[test]
    fn lease_scope_overlap_is_component_aware() {
        assert!(windows_path_scopes_overlap(
            Path::new(r"C:\Music"),
            Path::new(r"c:/music/Album")
        ));
        assert!(windows_path_scopes_overlap(
            Path::new(r"\\server\share\library"),
            Path::new(r"//SERVER/share/library/album")
        ));
        assert!(!windows_path_scopes_overlap(
            Path::new(r"C:\music"),
            Path::new(r"C:\music-backup")
        ));
        assert!(!windows_path_scopes_overlap(
            Path::new(r"C:\library-a"),
            Path::new(r"C:\library-b")
        ));
    }

    #[test]
    fn builds_safe_unicode_target_from_separate_components() {
        let target = SafeTargetPath::from_relative_components(
            root(),
            ["宇多田ヒカル", "初恋", "01_あなた.flac"],
            false,
        )
        .expect("safe Unicode target");
        assert_eq!(
            target.relative_components(),
            [
                OsString::from("宇多田ヒカル"),
                OsString::from("初恋"),
                OsString::from("01_あなた.flac")
            ]
        );
        assert!(target.as_path().to_string_lossy().contains("あなた.flac"));
    }

    #[test]
    fn relative_component_table_rejects_traversal_prefixes_and_windows_names() {
        let cases = [
            ".",
            "..",
            r"Album\..\outside.flac",
            "/absolute.flac",
            r"\\server\share",
            r"\\?\C:\device",
            r"C:\absolute.flac",
            "track:alternate",
            "track?.flac",
            "NUL",
            "con.txt",
            "album ",
            "album.",
            "nul\0byte",
            "control\u{001f}",
        ];
        for component in cases {
            assert!(
                SafeTargetPath::from_relative_components(root(), [component], false).is_err(),
                "component should be rejected: {component:?}"
            );
        }
    }

    #[test]
    fn rejects_relative_drive_relative_device_and_ambiguous_roots() {
        let roots = [
            Path::new("relative"),
            Path::new(r"C:relative"),
            Path::new(r"\root-relative"),
            Path::new(r"\\?\C:\Music"),
            Path::new(r"\\.\C:\Music"),
            Path::new(r"\??\C:\Music"),
        ];
        for invalid_root in roots {
            assert!(
                SafeTargetPath::from_relative_components(invalid_root, ["song.flac"], false)
                    .is_err(),
                "root should be rejected: {invalid_root:?}"
            );
        }
    }

    #[test]
    fn accepts_regular_unc_root_but_rejects_outside_or_prefixed_candidates() {
        let unc_root = Path::new(r"\\server\share\Music");
        let inside = Path::new(r"\\SERVER\SHARE\music\Artist\song.flac");
        assert!(SafeTargetPath::from_manual_absolute_target(unc_root, inside, false).is_ok());

        for candidate in [
            Path::new(r"\\server\share\MusicElsewhere\song.flac"),
            Path::new(r"\\server\other\Music\song.flac"),
            Path::new(r"\\?\UNC\server\share\Music\song.flac"),
        ] {
            assert!(
                SafeTargetPath::from_manual_absolute_target(unc_root, candidate, false).is_err(),
                "candidate should be rejected: {candidate:?}"
            );
        }
    }

    #[test]
    fn completed_target_is_revalidated_after_suffix_generation() {
        let safe = Path::new(r"C:\Music\Artist\song (2).flac");
        assert!(SafeTargetPath::from_completed_target(root(), safe, false).is_ok());

        for unsafe_target in [
            Path::new(r"C:\Music\Artist\song (2).flac."),
            Path::new(r"C:\Music\Artist\..\outside.flac"),
            Path::new(r"C:\MusicElsewhere\song (2).flac"),
        ] {
            assert!(
                SafeTargetPath::from_completed_target(root(), unsafe_target, false).is_err(),
                "completed target should be rejected: {unsafe_target:?}"
            );
        }
    }

    #[test]
    fn uses_utf16_units_and_requires_opt_in_above_compatible_limit() {
        let root_units = r"C:\Music\".encode_utf16().count();
        let file_suffix_units = ".flac".encode_utf16().count();
        let emoji_units = "😀".encode_utf16().count();
        assert_eq!(emoji_units, 2);
        let repeat =
            (WINDOWS_COMPATIBLE_PATH_LIMIT - root_units - file_suffix_units) / emoji_units + 1;
        let long_filename = format!("{}{}", "😀".repeat(repeat), ".flac");

        let error =
            SafeTargetPath::from_relative_components(root(), [&long_filename], false).unwrap_err();
        assert!(matches!(
            error,
            DomainError::PathTooLong {
                limit: WINDOWS_COMPATIBLE_PATH_LIMIT,
                ..
            }
        ));
        let allowed = SafeTargetPath::from_relative_components(root(), [&long_filename], true)
            .expect("long path opt-in");
        assert!(allowed.actual_utf16_units() > WINDOWS_COMPATIBLE_PATH_LIMIT);
        assert!(allowed.allows_long_paths());
    }

    #[test]
    fn manual_target_comparison_is_component_based_not_string_prefix_based() {
        assert!(SafeTargetPath::from_manual_absolute_target(
            root(),
            Path::new(r"c:\music\Artist\song.flac"),
            false
        )
        .is_ok());
        assert!(SafeTargetPath::from_manual_absolute_target(
            root(),
            Path::new(r"C:\Music-Other\song.flac"),
            false
        )
        .is_err());
    }

    #[cfg(windows)]
    #[test]
    fn path_key_preserves_unpaired_utf16_instead_of_using_display_replacement() {
        use std::os::windows::ffi::OsStringExt as _;

        let unpaired = PathBuf::from(OsString::from_wide(&[
            u16::from(b'C'),
            u16::from(b':'),
            u16::from(b'\\'),
            0xd800,
        ]));
        let replacement = PathBuf::from(r"C:\�");
        assert_ne!(
            WindowsPathKey::from_path(&unpaired),
            WindowsPathKey::from_path(&replacement)
        );
    }
}
