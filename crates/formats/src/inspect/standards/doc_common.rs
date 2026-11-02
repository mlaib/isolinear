//! Small shared pieces for the document, mail and container handlers: bounded decoders
//! (base64, quoted-printable, zlib), text in legacy charsets, ZIP parts from memory, sidecar
//! files, picture signatures, exact amounts and dates written in the usual ways.
use crate::inspect::{Row, Table};
use crate::table::archive::reader::{self as archive, Listing};
use std::io::{Cursor, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};

/// Largest sidecar file read (layouts, column mappings).
pub const MAX_SIDECAR: u64 = 1024 * 1024;

/// A table with these columns and no rows yet.
pub fn table(name: &str, columns: &[&str]) -> Table {
    Table {
        name: name.into(),
        columns: columns.iter().map(|c| (*c).to_owned()).collect(),
        rows: vec![],
        total: None,
    }
}

/// Add a row unless the table is full; past the limit only the total is counted.
pub fn push_row(t: &mut Table, cells: Vec<String>, source: Option<Range<u64>>) {
    if t.rows.len() < crate::inspect::MAX_ROWS {
        t.rows.push(Row { cells, source });
    } else {
        t.total = Some(t.total.unwrap_or(t.rows.len() as u64) + 1);
    }
}

/// "1.2 MiB", "512 bytes".
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut v = bytes as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

/// At most `max` characters of `s`, with an ellipsis when cut; newlines become spaces.
pub fn clip(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut out: String = flat.chars().take(max).collect();
        out.push('…');
        out
    }
}

pub fn u16le(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2)
        .map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}
pub fn u32le(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
pub fn u64le(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8).map_or(0, |s| {
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        u64::from_le_bytes(a)
    })
}

/// UTF-16LE text, up to the first NUL.
pub fn utf16le(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

// ----- decoders -----

/// Base64 (standard or URL alphabet, whitespace and junk ignored), at most `limit` bytes out.
/// Returns the bytes and whether the output was cut.
pub fn base64(input: &[u8], limit: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::with_capacity((input.len() / 4 * 3).min(limit));
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in input {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if out.len() >= limit {
                return (out, true);
            }
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (out, false)
}

/// Quoted-printable (RFC 2045): `=XX` escapes and soft line breaks; `underscore` turns `_` into
/// a space (RFC 2047 "Q" words). At most `limit` bytes out.
pub fn quoted_printable(input: &[u8], limit: usize, underscore: bool) -> (Vec<u8>, bool) {
    let mut out = Vec::with_capacity(input.len().min(limit));
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut i = 0;
    while i < input.len() {
        if out.len() >= limit {
            return (out, true);
        }
        let c = input[i];
        if c == b'=' {
            match (input.get(i + 1), input.get(i + 2)) {
                (Some(b'\r'), Some(b'\n')) => i += 3,
                (Some(b'\n'), _) => i += 2,
                (Some(&a), Some(&b)) if hex(a).is_some() && hex(b).is_some() => {
                    out.push(hex(a).unwrap_or(0) << 4 | hex(b).unwrap_or(0));
                    i += 3;
                }
                _ => {
                    out.push(c);
                    i += 1;
                }
            }
        } else if c == b'_' && underscore {
            out.push(b' ');
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    (out, false)
}

/// Decompress zlib (or raw deflate when `raw`) data, at most `limit` bytes; an error when the
/// output would be larger or the data is damaged.
pub fn inflate(data: &[u8], limit: u64, raw: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let reader: Box<dyn Read> = if raw {
        Box::new(flate2::read::DeflateDecoder::new(data))
    } else {
        Box::new(flate2::read::ZlibDecoder::new(data))
    };
    reader
        .take(limit + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("damaged compressed data ({e})"))?;
    if out.len() as u64 > limit {
        return Err(format!(
            "it expands past {} (decompression limit)",
            human(limit)
        ));
    }
    Ok(out)
}

/// Text in a named charset: UTF-8, ASCII, ISO-8859-1/15, Windows-1252 and UTF-16 are decoded;
/// anything else is read as UTF-8 with replacement characters.
pub fn decode_charset(bytes: &[u8], charset: &str) -> String {
    let cs = charset.trim().trim_matches('"').to_ascii_lowercase();
    match cs.as_str() {
        "iso-8859-1" | "latin1" | "latin-1" | "iso8859-1" | "l1" | "iso_8859-1" => {
            bytes.iter().map(|&b| b as char).collect()
        }
        "windows-1252" | "cp1252" | "iso-8859-15" | "latin9" | "iso8859-15" => {
            bytes.iter().map(|&b| cp1252(b)).collect()
        }
        "utf-16le" => utf16le_all(bytes, false),
        "utf-16be" => utf16le_all(bytes, true),
        "utf-16" => match bytes {
            [0xFE, 0xFF, rest @ ..] => utf16le_all(rest, true),
            [0xFF, 0xFE, rest @ ..] => utf16le_all(rest, false),
            _ => utf16le_all(bytes, true),
        },
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}
fn utf16le_all(b: &[u8], big: bool) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| {
            if big {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}
/// A Windows-1252 byte as a character.
pub fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

// ----- pictures -----

/// Whether `bytes` start like a picture the app can decode (PNG, JPEG, GIF, WebP, BMP, TIFF).
pub fn is_picture(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xFF\xD8\xFF")
        || bytes.starts_with(b"GIF8")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
        || bytes.starts_with(b"BM")
        || bytes.starts_with(b"II*\0")
        || bytes.starts_with(b"MM\0*")
}

// ----- ZIP parts from memory -----

/// A ZIP held in memory, listed from its central directory.
pub struct Zip<'a> {
    pub file: Cursor<&'a [u8]>,
    pub listing: Listing,
}
impl<'a> Zip<'a> {
    pub fn open(bytes: &'a [u8], cancelled: &dyn Fn() -> bool) -> Result<Self, String> {
        let mut file = Cursor::new(bytes);
        let listing = archive::list_zip(&mut file, cancelled).map_err(|e| e.to_string())?;
        Ok(Self { file, listing })
    }
    /// A part's bytes (case-insensitive name), `Ok(None)` when missing.
    pub fn part(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        part(&mut self.file, &self.listing, name)
    }
    pub fn has(&self, name: &str) -> bool {
        self.listing
            .entries
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case(name))
    }
    /// Byte range of an entry's stored data in the file (local header included).
    pub fn span(&self, name: &str) -> Option<Range<u64>> {
        let e = self
            .listing
            .entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))?;
        let end = e.offset.checked_add(30)?.checked_add(e.packed?)?;
        Some(e.offset..end)
    }
}

/// The ZIP local entries that lie inside `head` (the start of a file), as (name, method,
/// compressed data). Used to recognize a ZIP's content from the probe alone; stops at the
/// first entry it cannot step over.
pub fn head_entries(head: &[u8]) -> Vec<(String, u16, &[u8])> {
    let mut out = vec![];
    let mut at = 0usize;
    while out.len() < 64 && head.get(at..at + 4) == Some(b"PK\x03\x04") {
        let flags = u16le(head, at + 6);
        let method = u16le(head, at + 8);
        let packed = u32le(head, at + 18) as usize;
        let name_len = u16le(head, at + 26) as usize;
        let extra_len = u16le(head, at + 28) as usize;
        let name_at = at + 30;
        let Some(name) = head.get(name_at..name_at + name_len) else {
            break;
        };
        let data_at = name_at + name_len + extra_len;
        let name = String::from_utf8_lossy(name).into_owned();
        let data = head.get(data_at..data_at.saturating_add(packed));
        if flags & 8 != 0 && packed == 0 {
            out.push((name, method, &[][..]));
            break;
        }
        match data {
            Some(d) => out.push((name, method, d)),
            None => {
                out.push((name, method, &[][..]));
                break;
            }
        }
        at = data_at + packed;
    }
    out
}

// ----- sidecars -----

/// The first existing sidecar of `path` among `suffixes` appended to its full name and to its
/// name without the extension (`data.dat` → `data.dat.layout.toml`, `data.layout.toml`).
pub fn sidecar(path: &Path, suffix: &str) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let mut candidates = vec![path.with_file_name(format!("{name}{suffix}"))];
    if let Some((stem, _)) = name.rsplit_once('.')
        && !stem.is_empty()
    {
        candidates.push(path.with_file_name(format!("{stem}{suffix}")));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// A sidecar's text, at most [`MAX_SIDECAR`] bytes.
pub fn read_sidecar(path: &Path) -> Result<String, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_SIDECAR {
        return Err(format!(
            "{} is {}; sidecar files are read up to {}",
            path.display(),
            human(size),
            human(MAX_SIDECAR)
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8 text", path.display()))
}

/// A tiny reader for the TOML subset of sidecars: `[table]` and `[[array.of.tables]]` headers,
/// `key = value` with strings, integers, booleans; comments. The file is first checked with the
/// full TOML parser, so syntax errors are reported with their position.
/// A header path, whether it is an array element, and its pairs.
pub type Section = (String, bool, Vec<(String, TomlValue)>);

#[derive(Debug, Default)]
pub struct SimpleToml {
    /// (header path, is an array element, pairs) in source order; the top level has an empty path.
    pub sections: Vec<Section>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum TomlValue {
    Str(String),
    Int(i64),
    Bool(bool),
    Other(String),
}
impl TomlValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }
}
impl SimpleToml {
    pub fn parse(source: &str) -> Result<Self, String> {
        use crate::tree::{Limits, Severity, TreeFormat, toml::Toml};
        let never = std::sync::atomic::AtomicBool::new(false);
        let tree = Toml.parse(source, &Limits::default(), &never);
        if let Some(d) = tree
            .diagnostics
            .iter()
            .find(|d| d.severity == Severity::Error)
        {
            let at = (d.span.start as usize).min(source.len());
            let line = source.as_bytes()[..at]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
                + 1;
            return Err(format!("line {line}: {}", d.message));
        }
        let mut out = SimpleToml {
            sections: vec![(String::new(), false, vec![])],
        };
        for raw in source.lines().take(100_000) {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(h) = line.strip_prefix("[[") {
                let name = h.split("]]").next().unwrap_or("").trim().to_owned();
                out.sections.push((name, true, vec![]));
            } else if let Some(h) = line.strip_prefix('[') {
                let name = h.split(']').next().unwrap_or("").trim().to_owned();
                out.sections.push((name, false, vec![]));
            } else if let Some((k, v)) = split_pair(line) {
                let key = unquote(k.trim());
                let value = toml_value(v.trim());
                if let Some(s) = out.sections.last_mut() {
                    s.2.push((key, value));
                }
            }
        }
        Ok(out)
    }
}
/// `key = value`, where the key may be quoted and contain `=`.
fn split_pair(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut quote = None;
    for (i, &c) in bytes.iter().enumerate() {
        match (quote, c) {
            (None, b'"' | b'\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, b'=') => return Some((&line[..i], &line[i + 1..])),
            _ => {}
        }
    }
    None
}
fn unquote(s: &str) -> String {
    if s.len() >= 2 && (s.starts_with('"') && s.ends_with('"')) {
        crate::tree::toml::decode_basic(&s[1..s.len() - 1])
    } else if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        s[1..s.len() - 1].to_owned()
    } else {
        s.to_owned()
    }
}
fn toml_value(v: &str) -> TomlValue {
    // Strip a trailing comment outside quotes.
    let mut end = v.len();
    let mut quote = None;
    for (i, c) in v.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '#') => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    let v = v[..end].trim();
    if v.starts_with('"') || v.starts_with('\'') {
        return TomlValue::Str(unquote(v));
    }
    match v {
        "true" => return TomlValue::Bool(true),
        "false" => return TomlValue::Bool(false),
        _ => {}
    }
    match v.replace('_', "").parse::<i64>() {
        Ok(i) => TomlValue::Int(i),
        Err(_) => TomlValue::Other(v.to_owned()),
    }
}

// ----- amounts and dates -----

/// An amount as written in a spreadsheet or bordereau ("1,234.50", "1.234,50", "(100.00)",
/// "EUR 12", "-7", "12-") as an exact decimal string; `None` when it is not a number.
pub fn parse_amount(text: &str) -> Option<String> {
    let mut s = text.trim().to_owned();
    if s.is_empty() {
        return None;
    }
    let mut negative = false;
    if s.starts_with('(') && s.ends_with(')') {
        negative = true;
        s = s[1..s.len() - 1].trim().to_owned();
    }
    // Currency codes and symbols around the number.
    let s = s.trim_matches(|c: char| c.is_ascii_alphabetic() || "€$£¥ \u{a0}".contains(c));
    let mut s = s.replace([' ', '\u{a0}', '\''], "");
    if let Some(rest) = s.strip_suffix('-') {
        negative = !negative;
        s = rest.to_owned();
    }
    if let Some(rest) = s.strip_prefix('-') {
        negative = !negative;
        s = rest.to_owned();
    } else if let Some(rest) = s.strip_prefix('+') {
        s = rest.to_owned();
    }
    if s.is_empty()
        || !s
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
    {
        return None;
    }
    let (dots, commas) = (s.matches('.').count(), s.matches(',').count());
    let normalized = match (dots, commas) {
        (0, 0) => s.clone(),
        (_, 0) if dots == 1 => s.clone(),
        (_, 0) => grouped(&s, '.')?,
        (0, 1) => {
            let after = s.rsplit(',').next().unwrap_or("");
            if after.len() == 3 && !s.starts_with("0,") {
                s.replace(',', "")
            } else {
                s.replace(',', ".")
            }
        }
        (0, _) => grouped(&s, ',')?,
        _ => {
            let (last_dot, last_comma) = (s.rfind('.')?, s.rfind(',')?);
            if last_dot > last_comma {
                let int = s[..last_dot].replace(',', "");
                format!("{int}{}", &s[last_dot..])
            } else {
                let int = s[..last_comma].replace('.', "");
                format!("{int}.{}", &s[last_comma + 1..])
            }
        }
    };
    let (int, frac) = normalized.split_once('.').unwrap_or((&normalized, ""));
    if int.is_empty() && frac.is_empty() || frac.contains('.') || int.len() + frac.len() > 30 {
        return None;
    }
    let int = if int.is_empty() { "0" } else { int };
    let body = if frac.is_empty() {
        int.to_owned()
    } else {
        format!("{int}.{frac}")
    };
    Some(
        if negative && body.chars().any(|c| ('1'..='9').contains(&c)) {
            format!("-{body}")
        } else {
            body
        },
    )
}
/// "1.234.567" with `sep` as thousands separator only.
fn grouped(s: &str, sep: char) -> Option<String> {
    let parts: Vec<&str> = s.split(sep).collect();
    if parts[1..].iter().all(|p| p.len() == 3) && !parts[0].is_empty() && parts[0].len() <= 3 {
        Some(parts.concat())
    } else {
        None
    }
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// A month name or abbreviation (English), 1-based.
pub fn month_number(name: &str) -> Option<u32> {
    let n = name.to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|m| n.starts_with(m))
        .map(|i| i as u32 + 1)
}

pub fn valid_date(y: i64, m: u32, d: u32) -> bool {
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&d) && (1..=9999).contains(&y)
}

/// A calendar date written in a common way, as ISO `YYYY-MM-DD`: `2026-01-31`, `31/01/2026`,
/// `31.01.2026`, `20260131`, `31-Jan-2026`, `31 January 2026`, `2026-01-31T10:00:00`. Dates
/// with numbers only are read day first, unless only month first makes sense (`12/31/2026`).
pub fn parse_date(text: &str) -> Option<String> {
    let t = text.trim();
    let t = t
        .split(['T', ' '])
        .next()
        .filter(|d| d.len() >= 6)
        .unwrap_or(t);
    let t = t.trim();
    let iso = |y: i64, m: u32, d: u32| valid_date(y, m, d).then(|| format!("{y:04}-{m:02}-{d:02}"));
    if t.len() == 8 && t.chars().all(|c| c.is_ascii_digit()) {
        return iso(
            t[..4].parse().ok()?,
            t[4..6].parse().ok()?,
            t[6..].parse().ok()?,
        );
    }
    let parts: Vec<&str> = t
        .split(['-', '/', '.', ' '])
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() != 3 {
        // "31 January 2026" splits on spaces too; anything else is not a date.
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.len() == 3 {
            let d: u32 = words[0].trim_end_matches(',').parse().ok()?;
            let m = month_number(words[1])?;
            return iso(words[2].parse().ok()?, m, d);
        }
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let year = |s: &str| -> Option<i64> {
        let y = num(s)?;
        match s.len() {
            4 => Some(y),
            2 => Some(if y < 70 { 2000 + y } else { 1900 + y }),
            _ => None,
        }
    };
    if parts[0].len() == 4 {
        let y = num(parts[0])?;
        let m = num(parts[1])
            .map(|m| m as u32)
            .or_else(|| month_number(parts[1]))?;
        return iso(y, m, num(parts[2])? as u32);
    }
    let y = year(parts[2])?;
    if let Some(m) = month_number(parts[1]).filter(|_| parts[1].chars().any(char::is_alphabetic)) {
        return iso(y, m, num(parts[0])? as u32);
    }
    let (a, b) = (num(parts[0])? as u32, num(parts[1])? as u32);
    if b > 12 && a <= 12 && t.contains('/') {
        iso(y, a, b)
    } else {
        iso(y, b, a)
    }
}

/// Days since 1970-01-01 → (year, month, day).
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Days from 1970-01-01 to a civil date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Seconds since 1970 as `YYYY-MM-DD HH:MM:SS UTC`.
pub fn unix_time(secs: i64) -> String {
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// A Windows FILETIME (100 ns since 1601) as UTC text; `None` for zero or absurd values.
pub fn filetime(ft: u64) -> Option<String> {
    if ft == 0 || ft == u64::MAX {
        return None;
    }
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
    (-62_135_596_800..253_402_300_800)
        .contains(&secs)
        .then(|| unix_time(secs))
}

/// Largest part of a package read into memory.
const PART_LIMIT: u64 = 64 * 1024 * 1024;

/// One part's bytes; `Ok(None)` when the package has no such entry. Names are case-insensitive.
fn part(
    file: &mut (impl std::io::Read + std::io::Seek),
    listing: &Listing,
    name: &str,
) -> Result<Option<Vec<u8>>, String> {
    let Some(entry) = listing
        .entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(name))
    else {
        return Ok(None);
    };
    if entry.size > PART_LIMIT {
        return Err(format!(
            "{name} is {} MiB when extracted; Isolinear reads parts up to {} MiB",
            entry.size >> 20,
            PART_LIMIT >> 20
        ));
    }
    let mut out = Vec::new();
    archive::extract_zip(file, entry, &mut out).map_err(|e| format!("{name}: {e}"))?;
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decoders() {
        assert_eq!(base64(b"SGVs\nbG8=", 100).0, b"Hello");
        assert_eq!(base64(b"SGVsbG8=", 3), (b"Hel".to_vec(), true));
        assert_eq!(
            quoted_printable(b"caf=C3=A9 =\r\nau lait", 100, false).0,
            "café au lait".as_bytes()
        );
        assert_eq!(quoted_printable(b"a_b", 10, true).0, b"a b");
        assert_eq!(decode_charset(&[0x80, 0xE9], "windows-1252"), "€é");
        assert_eq!(decode_charset(&[0xE9], "ISO-8859-1"), "é");
        let packed = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::default());
            e.write_all(&[b'a'; 10_000]).unwrap();
            e.finish().unwrap()
        };
        assert_eq!(inflate(&packed, 10_000, false).unwrap().len(), 10_000);
        assert!(inflate(&packed, 9_999, false).is_err());
        assert!(inflate(b"junk", 100, false).is_err());
    }
    #[test]
    fn amounts_and_dates() {
        for (text, want) in [
            ("1,234.50", Some("1234.50")),
            ("1.234,50", Some("1234.50")),
            ("(100.00)", Some("-100.00")),
            ("EUR 12", Some("12")),
            ("12-", Some("-12")),
            ("1,234", Some("1234")),
            ("12,5", Some("12.5")),
            ("1.234.567", Some("1234567")),
            ("£ 1 000,00", Some("1000.00")),
            ("abc", None),
            ("1.2.3", None),
            ("", None),
        ] {
            assert_eq!(parse_amount(text).as_deref(), want, "{text}");
        }
        for (text, want) in [
            ("2026-01-31", Some("2026-01-31")),
            ("31/01/2026", Some("2026-01-31")),
            ("12/31/2026", Some("2026-12-31")),
            ("31.01.2026", Some("2026-01-31")),
            ("20260131", Some("2026-01-31")),
            ("31-Jan-2026", Some("2026-01-31")),
            ("31 January 2026", Some("2026-01-31")),
            ("2026-01-31T10:00:00Z", Some("2026-01-31")),
            ("2026-02-30", None),
            ("soon", None),
        ] {
            assert_eq!(parse_date(text).as_deref(), want, "{text}");
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(
            filetime(116_444_736_000_000_000).unwrap(),
            "1970-01-01 00:00:00 UTC"
        );
    }
    #[test]
    fn simple_toml() {
        let t = SimpleToml::parse(
            "name = \"x\" # c\n[[record]]\ntype = 'H'\nstart = 1\n[[record.field]]\nname = \"A = B\"\ntrim = false\n[columns]\n\"Policy Ref\" = \"policy_number\"\n",
        )
        .unwrap();
        assert_eq!(
            t.sections[0].2[0],
            ("name".into(), TomlValue::Str("x".into()))
        );
        assert_eq!(t.sections[1].0, "record");
        assert_eq!(t.sections[2].2[0].1, TomlValue::Str("A = B".into()));
        assert_eq!(t.sections[2].2[1].1, TomlValue::Bool(false));
        assert_eq!(t.sections[3].2[0].0, "Policy Ref");
        assert!(SimpleToml::parse("a = = 1").is_err());
    }
}
