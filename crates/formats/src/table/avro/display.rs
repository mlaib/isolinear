//! Keep a bounded display prefix while counting the complete value. Skipped rows use the same
//! decoder without retaining their strings; nested values cannot allocate an unbounded cell.
use super::super::{CELL_MAX, cut};
use std::fmt;

#[derive(Default)]
pub(crate) struct Text {
    prefix: String,
    characters: usize,
    kept: usize,
    bytes: usize,
}
impl Text {
    pub fn push_str(&mut self, s: &str) {
        let remaining = (CELL_MAX + 1).saturating_sub(self.kept);
        for c in s.chars().take(remaining) {
            self.prefix.push(c);
            self.kept += 1;
        }
        self.characters = self.characters.saturating_add(if s.is_ascii() {
            s.len()
        } else {
            s.chars().count()
        });
        self.bytes = self.bytes.saturating_add(s.len());
    }
    pub fn push(&mut self, c: char) {
        if self.kept <= CELL_MAX {
            self.prefix.push(c);
            self.kept += 1;
        }
        self.characters = self.characters.saturating_add(1);
        self.bytes = self.bytes.saturating_add(c.len_utf8());
    }
    pub fn omit(&mut self, n: usize) {
        self.characters = self.characters.saturating_add(n);
        self.bytes = self.bytes.saturating_add(n);
    }
    pub fn len(&self) -> usize {
        self.bytes
    }
    pub fn finish(mut self) -> String {
        if self.characters <= CELL_MAX + 1 {
            return cut(self.prefix);
        }
        let at = self
            .prefix
            .char_indices()
            .nth(CELL_MAX)
            .map_or(self.prefix.len(), |(at, _)| at);
        self.prefix.truncate(at);
        format!("{}… ({} characters)", self.prefix, self.characters)
    }
}
impl fmt::Write for Text {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push_str(s);
        Ok(())
    }
}

pub(crate) fn quote(s: &str, out: &mut Text) {
    out.push('"');
    quote_inner(s, out);
    out.push('"');
}
fn quote_inner(s: &str, out: &mut Text) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}
/// Lossy UTF-8 display without allocating a replacement copy of a large invalid string.
pub(crate) fn utf8(mut bytes: &[u8], out: &mut Text, quoted: bool) {
    if quoted {
        out.push('"');
    }
    while !bytes.is_empty() {
        let (valid, skip) = match std::str::from_utf8(bytes) {
            Ok(s) => {
                if quoted {
                    quote_inner(s, out);
                } else {
                    out.push_str(s);
                }
                break;
            }
            Err(error) => (error.valid_up_to(), error.error_len()),
        };
        if let Some(prefix) = bytes.get(..valid).and_then(|b| std::str::from_utf8(b).ok()) {
            if quoted {
                quote_inner(prefix, out);
            } else {
                out.push_str(prefix);
            }
        }
        out.push('�');
        match skip
            .and_then(|n| valid.checked_add(n))
            .and_then(|at| bytes.get(at..))
        {
            Some(tail) => bytes = tail,
            None => break,
        }
    }
    if quoted {
        out.push('"');
    }
}

pub(crate) fn hex(bytes: &[u8], out: &mut Text, prefix: bool) {
    use fmt::Write;
    if prefix {
        out.push_str("0x");
    }
    let n = bytes.len().min(CELL_MAX / 2 + 1);
    for b in bytes.iter().take(n) {
        let _ = write!(out, "{b:02x}");
    }
    out.omit(bytes.len().saturating_sub(n).saturating_mul(2));
}
