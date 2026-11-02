//! Writing a document back with the bytes it had, except where the user typed.
//!
//! The editor holds LF-separated text. A file whose line endings were uniformly CRLF or CR is
//! normalised to LF on the way in and restored on the way out; a file with mixed endings is
//! kept as written in both directions. The encoding and BOM are restored exactly. The write
//! is atomic: a temporary file next to the target, synced, then renamed over it.

use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::encoding::{Encoding, LineEnding};

/// What a file looked like on disk at some moment: enough to notice a change by another program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stamp {
    pub size: u64,
    pub modified_secs: u64,
    pub modified_nanos: u32,
}

pub fn stamp(path: &Path) -> io::Result<Stamp> {
    let meta = fs::metadata(path)?;
    let modified = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Ok(Stamp {
        size: meta.len(),
        modified_secs: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
    })
}

/// The text as the editor holds it.
pub fn normalize(text: &str, line_ending: LineEnding) -> Cow<'_, str> {
    match line_ending {
        LineEnding::CrLf => Cow::Owned(text.replace("\r\n", "\n")),
        LineEnding::Cr => Cow::Owned(text.replace('\r', "\n")),
        LineEnding::Lf | LineEnding::Mixed | LineEnding::None => Cow::Borrowed(text),
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EncodeError {
    /// The character at `line` (1-based) cannot be written in the file's encoding.
    Unencodable { line: usize, character: char },
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::Unencodable { line, character } => write!(
                f,
                "line {line}: {character:?} cannot be written in this encoding"
            ),
        }
    }
}

impl std::error::Error for EncodeError {}

/// The bytes to write for editor text, in the file's original encoding, BOM and line endings.
pub fn encode(
    text: &str,
    encoding: Encoding,
    bom: bool,
    line_ending: LineEnding,
) -> Result<Vec<u8>, EncodeError> {
    let text: Cow<str> = match line_ending {
        LineEnding::CrLf => Cow::Owned(text.replace('\n', "\r\n")),
        LineEnding::Cr => Cow::Owned(text.replace('\n', "\r")),
        _ => Cow::Borrowed(text),
    };
    let mut out = Vec::with_capacity(text.len() + 4);
    match encoding {
        Encoding::Utf8 => {
            if bom {
                out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            }
            out.extend_from_slice(text.as_bytes());
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            // UTF-16 is only ever detected through its BOM, so one is always written back.
            let big = encoding == Encoding::Utf16Be;
            out.extend_from_slice(if big { &[0xFE, 0xFF] } else { &[0xFF, 0xFE] });
            for unit in text.encode_utf16() {
                out.extend_from_slice(&if big {
                    unit.to_be_bytes()
                } else {
                    unit.to_le_bytes()
                });
            }
        }
        Encoding::Windows1252 => {
            let (bytes, _, had_errors) = encoding_rs::WINDOWS_1252.encode(&text);
            if had_errors {
                return Err(first_unencodable(&text));
            }
            out.extend_from_slice(&bytes);
        }
    }
    Ok(out)
}

fn first_unencodable(text: &str) -> EncodeError {
    let mut line = 1;
    for character in text.chars() {
        if character == '\n' {
            line += 1;
        }
        let mut buf = [0u8; 4];
        let (_, _, bad) = encoding_rs::WINDOWS_1252.encode(character.encode_utf8(&mut buf));
        if bad {
            return EncodeError::Unencodable { line, character };
        }
    }
    unreachable!("the whole text failed to encode but every character passed")
}

/// Writes `bytes` to `path` through a temporary file in the same directory. A symbolic link
/// is followed, so the link's target is replaced, not the link. Existing permissions are kept.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<Stamp> {
    let target: PathBuf = match fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) if e.kind() == io::ErrorKind::NotFound => path.to_owned(),
        Err(e) => return Err(e),
    };
    let dir = target.parent().filter(|d| !d.as_os_str().is_empty());
    let dir = dir
        .map(Path::to_owned)
        .unwrap_or_else(|| PathBuf::from("."));
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let tmp = dir.join(format!(
        ".{}.isolinear-{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| {
        let mut file = File::create(&tmp)?;
        if let Ok(meta) = fs::metadata(&target) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    stamp(&target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::decode;

    /// Decoding, normalising and encoding again gives back the exact bytes.
    fn round_trips(bytes: &[u8]) {
        let d = decode(bytes);
        assert!(!d.lossy);
        let text = normalize(&d.text, d.line_ending);
        let back = encode(&text, d.encoding, d.bom, d.line_ending).unwrap();
        assert_eq!(back, bytes);
    }

    #[test]
    fn round_trip_every_encoding_and_line_ending() {
        round_trips(b"plain\nlf\n");
        round_trips(b"\xEF\xBB\xBFbom\r\ncrlf\r\n");
        round_trips(b"old\rmac\r");
        round_trips(b"mixed\r\nendings\nhere\r");
        round_trips(b"\xFF\xFEh\0i\0\r\0\n\0");
        round_trips(b"\xFE\xFF\0h\0i\0\n");
        round_trips(b"caf\xE9 \x80 latin\r\n");
        round_trips(b"");
        round_trips(b"no newline");
    }

    #[test]
    fn normalised_text_has_only_lf_for_uniform_files() {
        assert_eq!(normalize("a\r\nb\r\n", LineEnding::CrLf), "a\nb\n");
        assert_eq!(normalize("a\rb", LineEnding::Cr), "a\nb");
        assert_eq!(normalize("a\r\nb\n", LineEnding::Mixed), "a\r\nb\n");
    }

    #[test]
    fn edits_take_the_file_line_ending() {
        let out = encode("new\nline\n", Encoding::Utf8, false, LineEnding::CrLf).unwrap();
        assert_eq!(out, b"new\r\nline\r\n");
    }

    #[test]
    fn unencodable_character_is_reported_with_its_line() {
        let err = encode("ok\nsnow ☃\n", Encoding::Windows1252, false, LineEnding::Lf).unwrap_err();
        assert_eq!(
            err,
            EncodeError::Unencodable {
                line: 2,
                character: '☃'
            }
        );
    }

    #[test]
    fn atomic_write_replaces_content_and_keeps_permissions() {
        let dir = std::env::temp_dir().join(format!("isolinear-save-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.txt");
        fs::write(&p, b"old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let s = write_atomic(&p, b"new content").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new content");
        assert_eq!(s.size, 11);
        assert_eq!(s, stamp(&p).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
        assert!(
            fs::read_dir(&dir).unwrap().count() == 1,
            "no temporary file left behind"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn atomic_write_creates_a_missing_file() {
        let dir = std::env::temp_dir().join(format!("isolinear-save-new-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("fresh.txt");
        write_atomic(&p, b"x").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"x");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_follows_a_symlink() {
        let dir = std::env::temp_dir().join(format!("isolinear-save-link-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let real = dir.join("real.txt");
        let link = dir.join("link.txt");
        fs::write(&real, b"old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_atomic(&link, b"through the link").unwrap();
        assert_eq!(fs::read(&real).unwrap(), b"through the link");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stamp_changes_with_content() {
        let dir = std::env::temp_dir().join(format!("isolinear-stamp-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.txt");
        fs::write(&p, b"1").unwrap();
        let a = stamp(&p).unwrap();
        fs::write(&p, b"22").unwrap();
        let b = stamp(&p).unwrap();
        assert_ne!(a, b);
        assert_eq!(b.size, 2);
        fs::remove_dir_all(&dir).unwrap();
    }
}
