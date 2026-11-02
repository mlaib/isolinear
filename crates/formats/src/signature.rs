//! File signatures. A signature wins over the extension: a PNG named `.txt` is a PNG.

/// The spec id for a recognised signature at the start of `probe`.
pub fn identify(probe: &[u8]) -> Option<&'static str> {
    const SIGNATURES: &[(&[u8], &str)] = &[
        (b"\x89PNG\r\n\x1a\n", "png"),
        (b"\xFF\xD8\xFF", "jpeg"),
        (b"GIF87a", "gif"),
        (b"GIF89a", "gif"),
        (b"%PDF-", "pdf"),
        (b"PK\x03\x04", "zip"),
        (b"PK\x05\x06", "zip"),
        (b"\x1F\x8B", "gzip"),
        (b"\x28\xB5\x2F\xFD", "zstd"),
        (b"\x7FELF", "elf"),
        (b"PAR1", "parquet"),
        (b"SQLite format 3\0", "sqlite"),
        (b"\x93NUMPY", "npy"),
        (b"ARROW1", "arrow"),
        (b"Obj\x01", "avro"),
        (b"\x89HDF\r\n\x1a\n", "hdf5"),
        (b"\xFD7zXZ\0", "xz"),
        (b"BZh", "bzip2"),
        (b"8BPS", "psd"),
        (b"II*\0", "tiff"),
        (b"MM\0*", "tiff"),
        (b"II+\0", "tiff"),
        (b"MM\0+", "tiff"),
    ];
    if probe.len() >= 12 && &probe[..4] == b"RIFF" && &probe[8..12] == b"WEBP" {
        return Some("webp");
    }
    if probe.get(4..8) == Some(b"ftyp") {
        match probe.get(8..12) {
            Some(b"heic" | b"heix" | b"mif1" | b"msf1") => return Some("heif"),
            Some(b"avif" | b"avis") => return Some("avif"),
            _ => {}
        }
    }
    if probe.get(257..262) == Some(b"ustar") {
        return Some("tar");
    }
    SIGNATURES
        .iter()
        .find(|(sig, _)| probe.starts_with(sig))
        .map(|(_, id)| *id)
}

/// For a probe starting with a UTF-16 byte-order mark (not UTF-32's), whether its characters are
/// text: hardly any control characters other than line breaks and tabs.
fn utf16_text(probe: &[u8]) -> Option<bool> {
    let little = match probe {
        [0xFF, 0xFE, 0, 0, ..] => return None,
        [0xFF, 0xFE, ..] => true,
        [0xFE, 0xFF, ..] => false,
        _ => return None,
    };
    let units = probe[2..].chunks_exact(2).map(|b| {
        if little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        }
    });
    let (mut chars, mut odd) = (0usize, 0usize);
    for c in char::decode_utf16(units) {
        chars += 1;
        match c {
            Ok(c) if !c.is_control() || matches!(c, '\t' | '\n' | '\r' | '\u{c}' | '\u{1b}') => {}
            _ => odd += 1,
        }
    }
    // A unit cut at the end of the probe is not a fault.
    Some(odd.saturating_sub(1) * 100 <= chars)
}

/// Whether the probe looks like binary data rather than text: a NUL byte, or more than a
/// small share of bytes that are neither printable ASCII, whitespace nor valid UTF-8.
pub fn looks_binary(probe: &[u8]) -> bool {
    if probe.is_empty() {
        return false;
    }
    if let Some(text) = utf16_text(probe) {
        return !text;
    }
    if probe.contains(&0) {
        return true;
    }
    match std::str::from_utf8(probe) {
        Ok(_) => false,
        Err(e) => {
            // A multi-byte sequence cut by the end of the probe is not an error.
            if e.error_len().is_none() && e.valid_up_to() + 4 >= probe.len() {
                return false;
            }
            let suspicious = probe
                .iter()
                .filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B))
                .count();
            suspicious * 100 > probe.len()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_signatures() {
        assert_eq!(identify(b"\x89PNG\r\n\x1a\n...."), Some("png"));
        assert_eq!(identify(b"%PDF-1.7"), Some("pdf"));
        assert_eq!(identify(b"PAR1"), Some("parquet"));
        assert_eq!(identify(b"RIFF\x10\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(identify(b"RIFF\x10\0\0\0WAVEfmt "), None);
        assert_eq!(identify(b"hello"), None);
        assert_eq!(identify(b""), None);
    }

    #[test]
    fn text_versus_binary() {
        assert!(!looks_binary(b"plain text\n"));
        assert!(!looks_binary("café, naïve".as_bytes()));
        assert!(!looks_binary(b"caf\xE9 latin-1"));
        assert!(looks_binary(b"ab\0cd"));
        assert!(looks_binary(&[
            0x01, 0x02, 0x03, 0x04, 0xFF, 0xFE, 0x05, 0x06
        ]));
        let mut cut = "é".repeat(100).into_bytes();
        cut.pop();
        assert!(
            !looks_binary(&cut),
            "a UTF-8 sequence cut at the probe end is text"
        );
    }
}
