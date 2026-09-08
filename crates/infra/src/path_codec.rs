use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub const WINDOWS_UTF16LE_V1: &str = "windows_utf16le_v1";
pub const UNIX_BYTES_V1: &str = "unix_bytes_v1";
pub const UTF8_LEGACY_V1: &str = "utf8_legacy_v1";

pub fn encode_path(path: &Path) -> (&'static str, Vec<u8>) {
    encode_os_string(path.as_os_str())
}

#[cfg(windows)]
fn encode_os_string(value: &std::ffi::OsStr) -> (&'static str, Vec<u8>) {
    use std::os::windows::ffi::OsStrExt;
    (
        WINDOWS_UTF16LE_V1,
        value.encode_wide().flat_map(u16::to_le_bytes).collect(),
    )
}

#[cfg(unix)]
fn encode_os_string(value: &std::ffi::OsStr) -> (&'static str, Vec<u8>) {
    use std::os::unix::ffi::OsStrExt;
    (UNIX_BYTES_V1, value.as_bytes().to_vec())
}

#[cfg(not(any(windows, unix)))]
fn encode_os_string(value: &std::ffi::OsStr) -> (&'static str, Vec<u8>) {
    (UTF8_LEGACY_V1, value.to_string_lossy().as_bytes().to_vec())
}

pub fn decode_path(encoding: &str, raw: &[u8]) -> Result<PathBuf, String> {
    match encoding {
        WINDOWS_UTF16LE_V1 => decode_windows_path(raw),
        UNIX_BYTES_V1 => decode_unix_path(raw),
        UTF8_LEGACY_V1 => String::from_utf8(raw.to_vec())
            .map(PathBuf::from)
            .map_err(|_| "legacy_path_invalid_utf8".into()),
        value => Err(format!("path_encoding_unknown:{value}")),
    }
}

#[cfg(windows)]
fn decode_windows_path(raw: &[u8]) -> Result<PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;
    if !raw.len().is_multiple_of(2) {
        return Err("windows_path_odd_byte_length".into());
    }
    let units = raw
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    Ok(PathBuf::from(OsString::from_wide(&units)))
}

#[cfg(not(windows))]
fn decode_windows_path(raw: &[u8]) -> Result<PathBuf, String> {
    if !raw.len().is_multiple_of(2) {
        return Err("windows_path_odd_byte_length".into());
    }
    let units = raw
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    String::from_utf16(&units)
        .map(PathBuf::from)
        .map_err(|_| "windows_path_not_representable_on_host".into())
}

#[cfg(unix)]
fn decode_unix_path(raw: &[u8]) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(OsString::from_vec(raw.to_vec())))
}

#[cfg(not(unix))]
fn decode_unix_path(raw: &[u8]) -> Result<PathBuf, String> {
    String::from_utf8(raw.to_vec())
        .map(PathBuf::from)
        .map_err(|_| "unix_path_not_representable_on_host".into())
}

pub fn legacy_path_blob(display: &str) -> (&'static str, Vec<u8>) {
    (UTF8_LEGACY_V1, display.as_bytes().to_vec())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LosslessPathEnvelope {
    pub schema_version: u32,
    pub role: String,
    pub display: String,
    pub display_lossy: bool,
    pub encoding: String,
    pub raw_base64: String,
}

pub fn path_envelope(path: &Path, role: &str) -> LosslessPathEnvelope {
    let (encoding, raw) = encode_path(path);
    LosslessPathEnvelope {
        schema_version: 1,
        role: role.to_owned(),
        display: path.to_string_lossy().into_owned(),
        display_lossy: path.to_str().is_none(),
        encoding: encoding.to_owned(),
        raw_base64: base64(&raw),
    }
}

pub fn serialize_archive_path<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serde::Serialize::serialize(&path_envelope(path, "history_archive"), serializer)
}

pub fn serialize_diagnostic_export_path<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serde::Serialize::serialize(&path_envelope(path, "diagnostic_export"), serializer)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            TABLE[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_utf16_codec_accepts_valid_non_ascii_units_on_every_host() {
        let text = r"C:\音楽\演奏😀.flac";
        let raw = text
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            decode_path(WINDOWS_UTF16LE_V1, &raw).unwrap(),
            PathBuf::from(text)
        );
        assert_eq!(
            decode_path(WINDOWS_UTF16LE_V1, &[0x43, 0x00, 0x3a]).unwrap_err(),
            "windows_path_odd_byte_length"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_envelope_keeps_non_utf8_bytes_outside_the_display_field() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let path = PathBuf::from(OsString::from_vec(b"archive-\xff.jsonl".to_vec()));
        let envelope = path_envelope(&path, "history_archive");
        assert_eq!(envelope.encoding, UNIX_BYTES_V1);
        assert_eq!(envelope.raw_base64, "YXJjaGl2ZS3/Lmpzb25s");
        assert!(envelope.display_lossy);
        assert_eq!(path.as_os_str().as_bytes(), b"archive-\xff.jsonl");
    }

    #[cfg(windows)]
    #[test]
    fn windows_codec_round_trips_unpaired_surrogate_losslessly() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let units = [b'C' as u16, b':' as u16, b'\\' as u16, 0xd800, b'x' as u16];
        let path = PathBuf::from(OsString::from_wide(&units));
        let (encoding, raw) = encode_path(&path);
        assert_eq!(encoding, WINDOWS_UTF16LE_V1);
        let decoded = decode_path(encoding, &raw).unwrap();
        assert_eq!(decoded.as_os_str().encode_wide().collect::<Vec<_>>(), units);
        assert!(path_envelope(&path, "test").display_lossy);
    }
}
