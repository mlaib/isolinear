//! MessagePack shown as JSON: every value decoded into pretty JSON text that the JSON tree and
//! source views display read-only. Binary data and extension types become labelled strings, the
//! timestamp extension becomes ISO text, and map keys that are not strings are written as their
//! JSON text. A file holding several values shows them as one array.
use super::json_out::{JsonOut, bytes_label};
use super::{MAX_NESTING, MAX_NODES};

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
    nodes: usize,
    odd_keys: bool,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| format!("The data ends inside a value at byte {}", self.at))?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn uint(&mut self, n: usize) -> Result<u64, String> {
        Ok(self
            .take(n)?
            .iter()
            .fold(0u64, |a, &x| (a << 8) | u64::from(x)))
    }
    /// A count of items that each take at least one byte: more than the bytes left is damage.
    fn count(&mut self, n: usize) -> Result<usize, String> {
        let c = self.uint(n)? as usize;
        if c > self.b.len() - self.at {
            return Err(format!(
                "A collection at byte {} claims {c} items, more than the bytes left",
                self.at
            ));
        }
        Ok(c)
    }

    fn value(&mut self, out: &mut JsonOut, depth: usize) -> Result<(), String> {
        if depth > MAX_NESTING {
            return Err(format!("Values nest deeper than {MAX_NESTING} levels"));
        }
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(format!("More than {MAX_NODES} values"));
        }
        out.check()?;
        let at = self.at;
        let tag = self.take(1)?[0];
        match tag {
            0x00..=0x7f => out.raw(&tag.to_string()),
            0xe0..=0xff => out.raw(&(tag as i8).to_string()),
            0x80..=0x8f => self.map((tag & 0x0f) as usize, out, depth)?,
            0x90..=0x9f => self.array((tag & 0x0f) as usize, out, depth)?,
            0xa0..=0xbf => self.str((tag & 0x1f) as usize, out)?,
            0xc0 => out.raw("null"),
            0xc2 => out.raw("false"),
            0xc3 => out.raw("true"),
            0xc4..=0xc6 => {
                let n = self.uint(1 << (tag - 0xc4))? as usize;
                let b = self.take(n)?;
                out.string(&bytes_label(b));
            }
            0xc7..=0xc9 => {
                let n = self.uint(1 << (tag - 0xc7))? as usize;
                let kind = self.take(1)?[0] as i8;
                let b = self.take(n)?;
                self.ext(kind, b, out);
            }
            0xca => {
                let b = self.take(4)?;
                out.float(f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64)
            }
            0xcb => {
                let v = self.uint(8)?;
                out.float(f64::from_bits(v))
            }
            0xcc..=0xcf => {
                let v = self.uint(1 << (tag - 0xcc))?;
                out.raw(&v.to_string())
            }
            0xd0..=0xd3 => {
                let n = 1usize << (tag - 0xd0);
                let v = self.uint(n)?;
                // Sign-extend from n bytes.
                let shift = 64 - 8 * n as u32;
                out.raw(&(((v << shift) as i64) >> shift).to_string())
            }
            0xd4..=0xd8 => {
                let kind = self.take(1)?[0] as i8;
                let b = self.take(1 << (tag - 0xd4))?;
                self.ext(kind, b, out);
            }
            0xd9..=0xdb => {
                let n = self.uint(1 << (tag - 0xd9))? as usize;
                self.str(n, out)?
            }
            0xdc | 0xdd => {
                let n = self.count(if tag == 0xdc { 2 } else { 4 })?;
                self.array(n, out, depth)?
            }
            0xde | 0xdf => {
                let n = self.count(if tag == 0xde { 2 } else { 4 })?;
                self.map(n, out, depth)?
            }
            0xc1 => {
                return Err(format!(
                    "Byte {at} holds 0xc1, which MessagePack never uses"
                ));
            }
        }
        Ok(())
    }

    fn str(&mut self, n: usize, out: &mut JsonOut) -> Result<(), String> {
        let b = self.take(n)?;
        out.string(&String::from_utf8_lossy(b));
        Ok(())
    }

    fn array(&mut self, n: usize, out: &mut JsonOut, depth: usize) -> Result<(), String> {
        if n == 0 {
            out.raw("[]");
            return Ok(());
        }
        out.raw("[");
        for i in 0..n {
            if i > 0 {
                out.raw(",");
            }
            out.newline(depth + 1);
            self.value(out, depth + 1)?;
        }
        out.newline(depth);
        out.raw("]");
        Ok(())
    }

    fn map(&mut self, n: usize, out: &mut JsonOut, depth: usize) -> Result<(), String> {
        if n == 0 {
            out.raw("{}");
            return Ok(());
        }
        out.raw("{");
        for i in 0..n {
            if i > 0 {
                out.raw(",");
            }
            out.newline(depth + 1);
            // A string key is written as is; any other key as its JSON text.
            let key_at = self.at;
            let tag = *self.b.get(key_at).ok_or("The data ends inside a map")?;
            if matches!(tag, 0xa0..=0xbf | 0xd9..=0xdb) {
                self.value(out, depth + 1)?;
            } else {
                let mut key = JsonOut::new();
                self.value(&mut key, depth + 1)?;
                self.odd_keys = true;
                let flat: String = key.text.split_whitespace().collect::<Vec<_>>().join(" ");
                out.string(&flat);
            }
            out.raw(": ");
            self.value(out, depth + 1)?;
        }
        out.newline(depth);
        out.raw("}");
        Ok(())
    }

    fn ext(&mut self, kind: i8, b: &[u8], out: &mut JsonOut) {
        if kind == -1
            && let Some(text) = timestamp(b)
        {
            out.string(&text);
            return;
        }
        out.string(&format!("<extension {kind}, {}", &bytes_label(b)[1..]));
    }
}

/// The timestamp extension (type -1) as ISO 8601 UTC.
fn timestamp(b: &[u8]) -> Option<String> {
    let (secs, nanos): (i64, u32) = match b.len() {
        4 => (u32::from_be_bytes(b.try_into().ok()?) as i64, 0),
        8 => {
            let v = u64::from_be_bytes(b.try_into().ok()?);
            ((v & 0x3_ffff_ffff) as i64, (v >> 34) as u32)
        }
        12 => (
            i64::from_be_bytes(b[4..12].try_into().ok()?),
            u32::from_be_bytes(b[..4].try_into().ok()?),
        ),
        _ => return None,
    };
    if nanos >= 1_000_000_000 || !(-62_135_596_800..=253_402_300_799).contains(&secs) {
        return None;
    }
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let mut s = format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    );
    if nanos > 0 {
        s.push_str(&format!(".{nanos:09}"));
    }
    s.push('Z');
    Some(s)
}

/// Decode a MessagePack file to JSON text and a note on how it was shown. Values before a
/// damaged one are kept (with the damage in the note); damage in the first value is an error.
pub fn to_json(bytes: &[u8], cancelled: &dyn Fn() -> bool) -> Result<(String, String), String> {
    if bytes.is_empty() {
        return Err("The file is empty".into());
    }
    let mut r = Reader {
        b: bytes,
        at: 0,
        nodes: 0,
        odd_keys: false,
    };
    let mut values = vec![];
    let mut damage = None;
    while r.at < bytes.len() {
        if cancelled() {
            return Err("Cancelled".into());
        }
        let mut one = JsonOut::new();
        match r.value(&mut one, 1) {
            Ok(()) => values.push(one.text),
            Err(e) => {
                damage = Some(e);
                break;
            }
        }
    }
    let mut note = "Decoded from MessagePack and shown as JSON, read-only".to_owned();
    let text = match values.len() {
        0 => return Err(damage.unwrap_or_else(|| "No values".into())),
        1 => values.pop().unwrap_or_default().replace("\n  ", "\n"),
        n => {
            note.push_str(&format!(
                "; the file holds {n} values in sequence, shown as one list"
            ));
            let mut out = String::from("[");
            for (i, v) in values.iter().enumerate() {
                out.push_str(if i == 0 { "\n  " } else { ",\n  " });
                out.push_str(v);
            }
            out.push_str("\n]");
            out
        }
    };
    if r.odd_keys {
        note.push_str("; map keys that are not strings are written as their JSON text");
    }
    if let Some(e) = damage {
        note.push_str(&format!("; decoding stopped: {e}"));
    }
    Ok((text + "\n", note))
}
