//! Text decoding that remembers enough to write the file back unchanged.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// Windows-1252 as a fallback for bytes that are not UTF-8.
    Windows1252,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
    Mixed,
    /// A single line, nothing to tell.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decoded {
    pub text: String,
    pub encoding: Encoding,
    pub bom: bool,
    pub line_ending: LineEnding,
    /// Some bytes could not be decoded and became U+FFFD.
    pub lossy: bool,
}

/// Decodes a whole file. UTF-8 (with or without BOM) and UTF-16 with a BOM are recognised;
/// anything else is read as Windows-1252 so that every byte stays visible.
pub fn decode(bytes: &[u8]) -> Decoded {
    let (encoding, bom, body) = match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => (Encoding::Utf8, true, rest),
        [0xFF, 0xFE, rest @ ..] => (Encoding::Utf16Le, true, rest),
        [0xFE, 0xFF, rest @ ..] => (Encoding::Utf16Be, true, rest),
        _ => (Encoding::Utf8, false, bytes),
    };
    let (text, lossy) = match encoding {
        Encoding::Utf8 => match std::str::from_utf8(body) {
            Ok(s) => (s.to_owned(), false),
            Err(_) => {
                // Not UTF-8 after all: every byte as Windows-1252, a UTF-8 BOM included (it shows
                // as "ï»¿"), so that saving writes back exactly what was read.
                let (cow, had_errors) =
                    encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes);
                return finish(cow.into_owned(), Encoding::Windows1252, false, had_errors);
            }
        },
        Encoding::Utf16Le => {
            let (cow, had_errors) = encoding_rs::UTF_16LE.decode_without_bom_handling(body);
            (cow.into_owned(), had_errors)
        }
        Encoding::Utf16Be => {
            let (cow, had_errors) = encoding_rs::UTF_16BE.decode_without_bom_handling(body);
            (cow.into_owned(), had_errors)
        }
        Encoding::Windows1252 => unreachable!("never chosen before decoding"),
    };
    finish(text, encoding, bom, lossy)
}

fn finish(text: String, encoding: Encoding, bom: bool, lossy: bool) -> Decoded {
    let line_ending = line_ending(&text);
    Decoded {
        text,
        encoding,
        bom,
        line_ending,
        lossy,
    }
}

/// The line ending style of a text, from all of it: a single line ending differently makes it
/// mixed, so that saving never rewrites a line break the user did not touch.
pub fn line_ending(text: &str) -> LineEnding {
    let b = text.as_bytes();
    let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
    for i in memchr::memchr2_iter(b'\r', b'\n', b) {
        if b[i] == b'\n' {
            if i > 0 && b[i - 1] == b'\r' {
                crlf += 1;
            } else {
                lf += 1;
            }
        } else if b.get(i + 1) != Some(&b'\n') {
            cr += 1;
        }
    }
    match ((lf > 0) as u8, (crlf > 0) as u8, (cr > 0) as u8) {
        (0, 0, 0) => LineEnding::None,
        (1, 0, 0) => LineEnding::Lf,
        (0, 1, 0) => LineEnding::CrLf,
        (0, 0, 1) => LineEnding::Cr,
        _ => LineEnding::Mixed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_without_bom() {
        let d = decode("a\nb\n".as_bytes());
        assert_eq!(d.encoding, Encoding::Utf8);
        assert!(!d.bom);
        assert_eq!(d.line_ending, LineEnding::Lf);
        assert!(!d.lossy);
    }

    #[test]
    fn utf8_bom_is_stripped_and_remembered() {
        let d = decode(b"\xEF\xBB\xBFx\r\ny\r\n");
        assert_eq!(d.text, "x\r\ny\r\n");
        assert!(d.bom);
        assert_eq!(d.line_ending, LineEnding::CrLf);
    }

    #[test]
    fn utf16_little_endian() {
        let d = decode(b"\xFF\xFEh\0i\0\r\0");
        assert_eq!(d.text, "hi\r");
        assert_eq!(d.encoding, Encoding::Utf16Le);
        assert_eq!(d.line_ending, LineEnding::Cr);
    }

    #[test]
    fn invalid_utf8_falls_back_to_windows_1252() {
        let d = decode(b"caf\xE9");
        assert_eq!(d.text, "café");
        assert_eq!(d.encoding, Encoding::Windows1252);
    }

    #[test]
    fn mixed_line_endings() {
        assert_eq!(line_ending("a\nb\r\nc"), LineEnding::Mixed);
        assert_eq!(line_ending("one line"), LineEnding::None);
    }
}
