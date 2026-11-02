//! Bounds-checked reading shared by the binary handlers: integers of either byte order,
//! NUL-terminated strings, base64, and small formatting helpers. Every read returns `None`
//! past the end instead of panicking.
use std::ops::Range;

/// A view of the file's bytes with a byte order.
#[derive(Clone, Copy)]
pub struct Bytes<'a> {
    pub b: &'a [u8],
    pub le: bool,
}
impl<'a> Bytes<'a> {
    pub fn new(b: &'a [u8], le: bool) -> Self {
        Self { b, le }
    }
    pub fn get(&self, at: usize, len: usize) -> Option<&'a [u8]> {
        self.b.get(at..at.checked_add(len)?)
    }
    pub fn u8(&self, at: usize) -> Option<u8> {
        self.b.get(at).copied()
    }
    pub fn u16(&self, at: usize) -> Option<u16> {
        let s: [u8; 2] = self.get(at, 2)?.try_into().ok()?;
        Some(if self.le {
            u16::from_le_bytes(s)
        } else {
            u16::from_be_bytes(s)
        })
    }
    pub fn u32(&self, at: usize) -> Option<u32> {
        let s: [u8; 4] = self.get(at, 4)?.try_into().ok()?;
        Some(if self.le {
            u32::from_le_bytes(s)
        } else {
            u32::from_be_bytes(s)
        })
    }
    pub fn u64(&self, at: usize) -> Option<u64> {
        let s: [u8; 8] = self.get(at, 8)?.try_into().ok()?;
        Some(if self.le {
            u64::from_le_bytes(s)
        } else {
            u64::from_be_bytes(s)
        })
    }
    /// A 32- or 64-bit word (ELF addresses and sizes).
    pub fn word(&self, at: usize, wide: bool) -> Option<u64> {
        if wide {
            self.u64(at)
        } else {
            self.u32(at).map(u64::from)
        }
    }
    /// The NUL-terminated string at `at`, at most `max` bytes, lossy UTF-8.
    pub fn cstr(&self, at: usize, max: usize) -> Option<String> {
        let rest = self.b.get(at..)?;
        let rest = &rest[..rest.len().min(max)];
        let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
        Some(String::from_utf8_lossy(&rest[..end]).into_owned())
    }
}

/// `start..start+len` as a file range, when it does not overflow.
pub fn span(start: usize, len: usize) -> Option<Range<u64>> {
    let end = start.checked_add(len)?;
    Some(start as u64..end as u64)
}

/// Offset as usize, when it fits.
pub fn us(v: u64) -> Option<usize> {
    usize::try_from(v).ok()
}

fn b64_value(c: u8, url: bool) -> Option<u8> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' if !url => 62,
        b'/' if !url => 63,
        b'-' if url => 62,
        b'_' if url => 63,
        _ => return None,
    })
}

/// Base64 (standard alphabet, whitespace ignored, padding optional) or base64url.
pub fn base64(text: &[u8], url: bool) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3 + 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut ended = false;
    for &c in text {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            ended = true;
            continue;
        }
        if ended {
            return None;
        }
        acc = (acc << 6) | u32::from(b64_value(c, url)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Readable byte count: "1,234 bytes".
pub fn bytes_text(n: u64) -> String {
    format!("{} bytes", thousands(n))
}

pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Names of the set bits of `value` from a table of (bit, name).
pub fn flags(value: u64, names: &[(u64, &str)]) -> String {
    names
        .iter()
        .filter(|(bit, _)| value & bit != 0)
        .map(|(_, n)| *n)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Text cut to `max` characters, with an ellipsis when cut.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Control characters replaced, so file text cannot disturb the display.
pub fn printable(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}
