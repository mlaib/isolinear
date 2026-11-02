//! Helpers shared by the text-based financial formats: lines with their byte offsets, legacy
//! 8-bit text, exact decimal amounts, capped tables, and checks that tally many comparisons.
use crate::inspect::{Inspection, MAX_ROWS, Row, Stage, State, Table};
use std::{borrow::Cow, ops::Range};

/// Lines of a byte slice with the offset of their first byte, without the line break. LF, CRLF
/// and a lone CR (old Macintosh files) all end a line.
pub struct Lines<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for Lines<'a> {
    type Item = (usize, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        let start = self.pos;
        let rest = self.bytes.get(start..).filter(|r| !r.is_empty())?;
        match memchr::memchr2(b'\n', b'\r', rest) {
            Some(i) => {
                let mut next = start + i + 1;
                if rest.get(i) == Some(&b'\r') && rest.get(i + 1) == Some(&b'\n') {
                    next += 1;
                }
                self.pos = next;
                Some((start, rest.get(..i).unwrap_or_default()))
            }
            None => {
                self.pos = self.bytes.len();
                Some((start, rest))
            }
        }
    }
}

/// The lines of `bytes`, starting at `from` (offsets stay those of the whole slice).
pub fn lines_from(bytes: &[u8], from: usize) -> Lines<'_> {
    Lines {
        bytes,
        pos: from.min(bytes.len()),
    }
}

/// Length of a UTF-8 byte order mark at the start, if any.
pub fn bom(bytes: &[u8]) -> usize {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    }
}

/// The bytes after a byte order mark and leading white space.
pub fn trimmed_head(bytes: &[u8]) -> &[u8] {
    let b = bytes.get(bom(bytes)..).unwrap_or_default();
    let skip = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    b.get(skip..).unwrap_or_default()
}

/// 8-bit encodings for text that is not UTF-8.
#[derive(Clone, Copy)]
pub enum Legacy {
    /// Windows-1252 (Latin-1 with typographic characters in 0x80–0x9F).
    Windows1252,
    /// ISO 8859-15 (Latin-9: Latin-1 with the euro sign at 0xA4), required for the French FEC.
    Latin9,
}

/// Text of `b`: UTF-8 when it is, otherwise the legacy encoding (every byte is a character).
pub fn decode(b: &[u8], legacy: Legacy) -> Cow<'_, str> {
    match std::str::from_utf8(b) {
        Ok(s) => Cow::Borrowed(s),
        Err(_) => Cow::Owned(b.iter().map(|&c| legacy_char(c, legacy)).collect()),
    }
}

fn legacy_char(c: u8, legacy: Legacy) -> char {
    match legacy {
        Legacy::Windows1252 => match c {
            0x80 => '€',
            0x82 => '‚',
            0x84 => '„',
            0x85 => '…',
            0x8A => 'Š',
            0x8C => 'Œ',
            0x8E => 'Ž',
            0x91 => '‘',
            0x92 => '’',
            0x93 => '“',
            0x94 => '”',
            0x96 => '–',
            0x97 => '—',
            0x9A => 'š',
            0x9C => 'œ',
            0x9E => 'ž',
            0x9F => 'Ÿ',
            _ => c as char,
        },
        Legacy::Latin9 => match c {
            0xA4 => '€',
            0xA6 => 'Š',
            0xA8 => 'š',
            0xB4 => 'Ž',
            0xB8 => 'ž',
            0xBC => 'Œ',
            0xBD => 'œ',
            0xBE => 'Ÿ',
            _ => c as char,
        },
    }
}

/// Text of `b` with the Windows-1252 fallback.
pub fn text(b: &[u8]) -> Cow<'_, str> {
    decode(b, Legacy::Windows1252)
}

/// Byte range `start..start + len` as a source.
pub fn span(start: usize, len: usize) -> Option<Range<u64>> {
    Some(start as u64..(start + len) as u64)
}

/// An exact decimal number, in units of 10⁻⁹ (amounts with more decimals are refused).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dec(i128);

const SCALE: u32 = 9;
const UNIT: i128 = 1_000_000_000;

impl Dec {
    pub const ZERO: Dec = Dec(0);
    /// A plain decimal: optional sign, digits, optional `.` and up to nine decimals.
    pub fn parse(s: &str) -> Option<Dec> {
        let s = s.trim();
        let (neg, digits) = match s.strip_prefix('-') {
            Some(d) => (true, d),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
        if (int.is_empty() && frac.is_empty())
            || int.len() > 24
            || frac.len() > SCALE as usize
            || !int.bytes().chain(frac.bytes()).all(|c| c.is_ascii_digit())
        {
            return None;
        }
        let i: i128 = if int.is_empty() { 0 } else { int.parse().ok()? };
        let f: i128 = if frac.is_empty() {
            0
        } else {
            frac.parse::<i128>().ok()? * 10i128.pow(SCALE - frac.len() as u32)
        };
        let v = i.checked_mul(UNIT)?.checked_add(f)?;
        Some(Dec(if neg { -v } else { v }))
    }
    /// A whole number of minor units (cents for two decimals).
    pub fn from_minor(units: i128, decimals: u32) -> Option<Dec> {
        let decimals = decimals.min(SCALE);
        units.checked_mul(10i128.pow(SCALE - decimals)).map(Dec)
    }
    pub fn checked_add(self, other: Dec) -> Option<Dec> {
        self.0.checked_add(other.0).map(Dec)
    }
    pub fn checked_sub(self, other: Dec) -> Option<Dec> {
        self.0.checked_sub(other.0).map(Dec)
    }
    pub fn negated(self) -> Dec {
        Dec(-self.0)
    }
    pub fn is_negative(self) -> bool {
        self.0 < 0
    }
    /// Written with at least `min_decimals` decimals (more when the value has them).
    pub fn format(self, min_decimals: usize) -> String {
        let neg = self.0 < 0;
        let abs = self.0.unsigned_abs();
        let int = abs / UNIT as u128;
        let frac = format!("{:09}", abs % UNIT as u128);
        let keep = frac.trim_end_matches('0').len().max(min_decimals.min(9));
        let frac = &frac[..keep];
        format!(
            "{}{int}{}{frac}",
            if neg { "-" } else { "" },
            if keep > 0 { "." } else { "" }
        )
    }
}

/// A decimal written with a comma, SWIFT style ("1234,56", "1500,"): `Dec`, or `None`.
pub fn comma_decimal(s: &str) -> Option<Dec> {
    let s = s.trim();
    if s.is_empty() || s.matches(',').count() > 1 || s.contains('.') {
        return None;
    }
    Dec::parse(&s.replace(',', "."))
}

/// A decimal written loosely: optional sign, `.` or `,` as the decimal separator, and spaces,
/// non-breaking spaces or the other separator as thousands separators. With both `.` and `,`,
/// the last one is the decimal separator. A single `,` alone is a decimal separator when
/// `lone_comma_decimal` is set (French files) and a thousands separator otherwise when exactly
/// three digits follow it.
pub fn loose_decimal(s: &str, lone_comma_decimal: bool) -> Option<Dec> {
    let s: String = s
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '\u{a0}' | '\u{202f}' | '\''))
        .collect();
    if s.is_empty() || s.len() > 64 {
        return None;
    }
    let (dot, comma) = (s.rfind('.'), s.rfind(','));
    let normalized = match (dot, comma) {
        (Some(d), Some(c)) if d > c => s.replace(',', ""),
        (Some(_), Some(_)) => s.replace('.', "").replace(',', "."),
        (None, Some(c)) => {
            let after = s.len() - c - 1;
            if s.matches(',').count() > 1 || (!lone_comma_decimal && after == 3) {
                s.replace(',', "")
            } else {
                s.replace(',', ".")
            }
        }
        (Some(_), None) if s.matches('.').count() > 1 => s.replace('.', ""),
        _ => s,
    };
    Dec::parse(&normalized)
}

/// "261031" (YYMMDD) as "2026-10-31". Years 80–99 are 1980–1999, the others 2000–2079.
pub fn yymmdd(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() != 6 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let yy: u32 = s.get(0..2)?.parse().ok()?;
    let (mm, dd) = (s.get(2..4)?, s.get(4..6)?);
    valid_md(mm, dd)?;
    let year = if yy >= 80 { 1900 + yy } else { 2000 + yy };
    Some(format!("{year}-{mm}-{dd}"))
}

/// "20261031" (YYYYMMDD) as "2026-10-31".
pub fn yyyymmdd(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() != 8 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let (y, m, d) = (s.get(0..4)?, s.get(4..6)?, s.get(6..8)?);
    valid_md(m, d)?;
    Some(format!("{y}-{m}-{d}"))
}

fn valid_md(mm: &str, dd: &str) -> Option<()> {
    let m: u32 = mm.parse().ok()?;
    let d: u32 = dd.parse().ok()?;
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then_some(())
}

/// "1230" (HHMM) as "12:30".
pub fn hhmm(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() != 4 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(format!("{}:{}", s.get(0..2)?, s.get(2..4)?))
}

/// A table that keeps at most [`MAX_ROWS`] rows and counts the others.
pub struct Capped {
    table: Table,
    count: u64,
}

impl Capped {
    pub fn new(name: &str, columns: &[&str]) -> Self {
        Self {
            table: Table {
                name: name.into(),
                columns: columns.iter().map(|c| (*c).to_owned()).collect(),
                rows: vec![],
                total: None,
            },
            count: 0,
        }
    }
    /// Whether further rows are only counted (build cells only when this is false).
    pub fn full(&self) -> bool {
        self.table.rows.len() >= MAX_ROWS
    }
    pub fn push(&mut self, cells: Vec<String>, source: Option<Range<u64>>) {
        self.count += 1;
        if !self.full() {
            self.table.rows.push(Row { cells, source });
        }
    }
    /// Count a row without building it (when [`full`](Self::full)).
    pub fn skip(&mut self) {
        self.count += 1;
    }
    pub fn len(&self) -> u64 {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    /// Add the table to `out` when it has rows.
    pub fn finish(mut self, out: &mut Inspection) {
        if self.count == 0 {
            return;
        }
        if self.count > self.table.rows.len() as u64 {
            self.table.total = Some(self.count);
        }
        out.tables.push(self.table);
    }
}

/// One rule compared many times (each statement, each entry), reported as one check: how many
/// agree and the first ones that do not.
pub struct Tally {
    what: String,
    total: usize,
    failed: usize,
    first: Option<String>,
    failures: Vec<String>,
}

const SHOWN_FAILURES: usize = 5;

impl Tally {
    pub fn new(what: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            total: 0,
            failed: 0,
            first: None,
            failures: vec![],
        }
    }
    /// One comparison; `label` describes it ("statement 2: declared 10.00, computed 9.00").
    pub fn add(&mut self, ok: bool, label: impl FnOnce() -> String) {
        self.total += 1;
        let need = self.total == 1 || (!ok && self.failures.len() < SHOWN_FAILURES);
        if !need {
            if !ok {
                self.failed += 1;
            }
            return;
        }
        let l = label();
        if self.total == 1 {
            self.first = Some(l.clone());
        }
        if !ok {
            self.failed += 1;
            if self.failures.len() < SHOWN_FAILURES {
                self.failures.push(l);
            }
        }
    }
    pub fn failed(&self) -> usize {
        self.failed
    }
    /// Add the check (nothing when no comparison was made).
    pub fn report(self, out: &mut Inspection) {
        if self.total == 0 {
            return;
        }
        let state = if self.failed == 0 {
            State::Passed
        } else {
            State::Failed
        };
        let detail = if self.total == 1 {
            format!("{}: {}", self.what, self.first.unwrap_or_default())
        } else {
            let mut d = format!(
                "{}: {} of {} agree",
                self.what,
                self.total - self.failed,
                self.total
            );
            if !self.failures.is_empty() {
                d.push_str(" — ");
                d.push_str(&self.failures.join("; "));
                if self.failed > self.failures.len() {
                    d.push_str(&format!("; and {} more", self.failed - self.failures.len()));
                }
            }
            d
        };
        out.check(Stage::Semantic, state, detail);
    }
}

/// The Parsed check: passed, or partial with the reasons.
pub fn parsed(out: &mut Inspection, what: &str, problems: &[String]) {
    if problems.is_empty() {
        out.check(Stage::Parsed, State::Passed, what);
    } else {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("{what}; {}", problems.join("; ")),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lines_keep_offsets_for_every_line_break() {
        let v: Vec<(usize, &[u8])> = lines_from(b"a\r\nbb\ncc\rd", 0).collect();
        assert_eq!(
            v,
            vec![
                (0, &b"a"[..]),
                (3, &b"bb"[..]),
                (6, &b"cc"[..]),
                (9, &b"d"[..])
            ]
        );
    }
    #[test]
    fn decimals_are_exact() {
        let a = Dec::parse("0.1").unwrap();
        let b = Dec::parse("0.2").unwrap();
        assert_eq!(a.checked_add(b).unwrap(), Dec::parse("0.3").unwrap());
        assert_eq!(comma_decimal("1234,5").unwrap().format(2), "1234.50");
        assert_eq!(comma_decimal("1500,").unwrap().format(2), "1500.00");
        assert!(comma_decimal("1.5").is_none());
        assert_eq!(
            loose_decimal("1 234,56", true).unwrap().format(2),
            "1234.56"
        );
        assert_eq!(
            loose_decimal("1,234.56", false).unwrap().format(2),
            "1234.56"
        );
        assert_eq!(loose_decimal("1,234", false).unwrap().format(0), "1234");
        assert_eq!(loose_decimal("-12,5", false).unwrap().format(2), "-12.50");
        assert_eq!(Dec::from_minor(-12345, 2).unwrap().format(2), "-123.45");
        assert!(Dec::parse("1e5").is_none());
        assert!(Dec::parse("9".repeat(30).as_str()).is_none());
        assert_eq!(yymmdd("261031").as_deref(), Some("2026-10-31"));
        assert_eq!(yymmdd("991231").as_deref(), Some("1999-12-31"));
        assert!(yymmdd("261331").is_none());
    }
}
