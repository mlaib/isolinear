//! Streaming string extraction. Input buffers and each pending run have fixed bounds.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Take};
use std::path::Path;

/// Strings returned at most.
pub const MAX_STRINGS: usize = 200_000;
const MAX_CHARS: usize = 200;
const BLOCK: usize = 64 * 1024;

// Prefer UTF-8 when both decoders find a run at the same offset.
type Found = BTreeMap<u64, (bool, String)>;

/// Read printable UTF-8 and UTF-16 LE runs of at least four characters, in byte order; UTF-16
/// runs hold Latin, Greek and Cyrillic text only.
/// Only the first `limit_bytes` are read. At most 200,000 strings are returned; long runs
/// end with an ellipsis within the 200-character limit. Whitespace-only runs are omitted.
/// UTF-16 is checked at both byte alignments. This synchronous I/O belongs on a worker.
pub fn strings(path: &Path, limit_bytes: u64) -> Result<Vec<(u64, String)>, String> {
    let file = File::open(path).map_err(|e| format!("Could not open {}: {e}", path.display()))?;
    scan(file.take(limit_bytes))
        .map_err(|e| format!("Could not read strings from {}: {e}", path.display()))
}

#[derive(Default)]
struct Run {
    offset: u64,
    text: String,
    count: usize,
    non_space: bool,
    wide: bool,
}

/// Characters a UTF-16 run may hold: Latin, Greek, Cyrillic and general punctuation. Almost any
/// byte pair decodes to some character (mostly CJK), so a wider set turns data into "text".
fn wide_plausible(c: char) -> bool {
    matches!(c, '\u{20}'..='\u{24f}' | '\u{370}'..='\u{52f}' | '\u{2000}'..='\u{206f}' | '\t')
}

impl Run {
    fn character(&mut self, c: char, offset: u64, out: &mut Found) {
        if c.is_control() && c != '\t'
            || c == '\u{feff}'
            || c == '\u{fffe}'
            || c == '\u{ffff}'
            || self.wide && !wide_plausible(c)
        {
            self.finish(out);
            return;
        }

        if self.count == 0 {
            self.offset = offset;
        }
        self.non_space |= !c.is_whitespace();
        if self.count < MAX_CHARS {
            self.text.push(c);
        } else if self.count == MAX_CHARS {
            self.text.pop();
            self.text.push('…');
        }
        self.count = (self.count + 1).min(MAX_CHARS + 1);
    }

    fn finish(&mut self, out: &mut Found) {
        if self.count >= 4 && self.non_space {
            let eligible = out.len() < MAX_STRINGS
                || out
                    .last_key_value()
                    .is_some_and(|(&last, _)| self.offset <= last);
            if eligible {
                match out.entry(self.offset) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert((self.wide, std::mem::take(&mut self.text)));
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        if !self.wide && entry.get().0 {
                            entry.insert((false, std::mem::take(&mut self.text)));
                        }
                    }
                }
                if out.len() > MAX_STRINGS {
                    out.pop_last();
                }
            }
        }
        self.text.clear();
        self.count = 0;
        self.non_space = false;
    }
}

#[derive(Default)]
struct Utf8 {
    run: Run,
    bytes: [u8; 4],
    used: usize,
    needed: usize,
    offset: u64,
}

impl Utf8 {
    fn byte(&mut self, b: u8, offset: u64, out: &mut Found) {
        if self.used != 0 {
            if (0x80..=0xbf).contains(&b) {
                self.bytes[self.used] = b;
                self.used += 1;
                if self.used == self.needed {
                    match std::str::from_utf8(&self.bytes[..self.used])
                        .ok()
                        .and_then(|s| s.chars().next())
                    {
                        Some(c) => self.run.character(c, self.offset, out),
                        None => self.run.finish(out),
                    }
                    self.used = 0;
                }
                return;
            }
            self.run.finish(out);
            self.used = 0;
        }
        if b < 0x80 {
            self.run.character(b as char, offset, out);
        } else {
            self.needed = match b {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => {
                    self.run.finish(out);
                    return;
                }
            };
            self.bytes[0] = b;
            self.used = 1;
            self.offset = offset;
        }
    }
}

struct Utf16 {
    run: Run,
    high: Option<(u16, u64)>,
}

impl Default for Utf16 {
    fn default() -> Self {
        Self {
            run: Run {
                wide: true,
                ..Run::default()
            },
            high: None,
        }
    }
}

impl Utf16 {
    fn unit(&mut self, unit: u16, offset: u64, out: &mut Found) {
        if let Some((high, start)) = self.high.take() {
            if (0xdc00..=0xdfff).contains(&unit) {
                let scalar =
                    0x10000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(unit) - 0xdc00);
                if let Some(c) = char::from_u32(scalar) {
                    self.run.character(c, start, out);
                }
                return;
            }
            self.run.finish(out);
        }
        if (0xd800..=0xdbff).contains(&unit) {
            self.high = Some((unit, offset));
        } else if let Some(c) = char::from_u32(u32::from(unit)) {
            self.run.character(c, offset, out);
        } else {
            self.run.finish(out);
        }
    }
}

fn scan(reader: Take<impl Read>) -> std::io::Result<Vec<(u64, String)>> {
    let mut reader = reader;
    let mut out = Found::new();
    let mut utf8 = Utf8::default();
    let mut utf16 = [Utf16::default(), Utf16::default()];
    let mut block = [0; BLOCK];
    let mut previous = 0;
    let mut offset = 0u64;
    loop {
        let n = match reader.read(&mut block) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if n == 0 {
            break;
        }
        for &b in &block[..n] {
            utf8.byte(b, offset, &mut out);
            if offset > 0 {
                let start = offset - 1;
                utf16[(start % 2) as usize].unit(
                    u16::from_le_bytes([previous, b]),
                    start,
                    &mut out,
                );
            }
            previous = b;
            // `Take` permits at most u64::MAX bytes; the last byte's index is MAX - 1.
            offset += 1;
        }
    }
    utf8.run.finish(&mut out);
    for state in &mut utf16 {
        state.run.finish(&mut out);
    }
    Ok(out
        .into_iter()
        .map(|(offset, (_, text))| (offset, text))
        .collect())
}

#[cfg(test)]
pub(super) fn from_bytes(bytes: &[u8]) -> Vec<(u64, String)> {
    scan(bytes.take(bytes.len() as u64)).expect("in-memory read")
}
