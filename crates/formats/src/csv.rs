//! Delimited text (CSV, TSV) read as records of fields, with byte spans into the source so a
//! cell can be found in the text and the text in a cell.
//!
//! Records follow RFC 4180, read leniently: fields may be quoted (a doubled quote is a quote),
//! quoted fields may span lines, records end at LF or CRLF, and records may have different field
//! counts (reported as ragged). Values are never converted: a cell shows its text as written,
//! without its enclosing quotes.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

const SNIFF_RECORDS: usize = 50;
/// Characters of a cell sent for display; the rest is cut.
pub const CELL_CAP: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dialect {
    pub delimiter: u8,
    pub quote: u8,
    pub has_header: bool,
}

impl Default for Dialect {
    fn default() -> Self {
        Dialect {
            delimiter: b',',
            quote: b'"',
            has_header: false,
        }
    }
}

/// Field spans of the record starting at `start`, and where the next record starts. Spans keep
/// a field's quotes. A quote that is never closed runs to the end of the text.
pub fn record(text: &[u8], start: usize, d: &Dialect, fields: &mut Vec<Range<usize>>) -> usize {
    fields.clear();
    let mut i = start;
    loop {
        let field_start = i;
        if text.get(i) == Some(&d.quote) {
            i += 1;
            loop {
                match text[i.min(text.len())..].iter().position(|&b| b == d.quote) {
                    None => {
                        i = text.len();
                        break;
                    }
                    Some(p) => {
                        i += p + 1;
                        if text.get(i) == Some(&d.quote) {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                }
            }
        }
        // Unquoted text, or what follows a closing quote, up to the delimiter or line end.
        while i < text.len() && text[i] != d.delimiter && text[i] != b'\n' {
            i += 1;
        }
        let mut end = i;
        if i < text.len() && text[i] == b'\n' && end > field_start && text[end - 1] == b'\r' {
            end -= 1;
        }
        fields.push(field_start..end);
        match text.get(i) {
            Some(&b) if b == d.delimiter => i += 1,
            Some(b'\n') => return i + 1,
            _ => return text.len(),
        }
    }
}

/// The value of a field as shown: enclosing quotes removed, doubled quotes undone, cut at
/// [`CELL_CAP`] characters.
pub fn cell(text: &[u8], span: &Range<usize>, d: &Dialect) -> String {
    let raw = &text[span.clone()];
    let q = d.quote;
    let value: Vec<u8> = if raw.len() >= 2 && raw[0] == q && raw[raw.len() - 1] == q {
        let inner = &raw[1..raw.len() - 1];
        let mut out = Vec::with_capacity(inner.len());
        let mut i = 0;
        while i < inner.len() {
            out.push(inner[i]);
            i += if inner[i] == q && inner.get(i + 1) == Some(&q) {
                2
            } else {
                1
            };
        }
        out
    } else {
        raw.to_vec()
    };
    let s = String::from_utf8_lossy(&value);
    if s.chars().count() > CELL_CAP {
        let mut cut: String = s.chars().take(CELL_CAP).collect();
        cut.push('…');
        cut
    } else {
        s.into_owned()
    }
}

/// Suggests a dialect from the start of a file. The delimiter is the one among `,` `;` tab `|`
/// (or `hint`) whose most common field count is above one and covers the largest share of the
/// first 50 records; ties go to more fields, then list order. The header is a guess: the first
/// row has no empty and no numeric field, and either a later row has a numeric field, or the
/// first-row values are distinct and none reappears in its column.
pub fn sniff(head: &[u8], hint: Option<u8>) -> Dialect {
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let delimiter = hint.unwrap_or_else(|| {
        let mut best = (0.0f64, 0usize, b',');
        for d in [b',', b';', b'\t', b'|'] {
            let rows = sample(head, d);
            if rows.is_empty() {
                continue;
            }
            let mut counts: Vec<usize> = rows.iter().map(Vec::len).collect();
            counts.sort_unstable();
            let (mut mode, mut freq) = (0, 0);
            for chunk in counts.chunk_by(|a, b| a == b) {
                if chunk.len() > freq || (chunk.len() == freq && chunk[0] > mode) {
                    (mode, freq) = (chunk[0], chunk.len());
                }
            }
            let share = freq as f64 / rows.len() as f64;
            if mode > 1 && (share > best.0 || (share == best.0 && mode > best.1)) {
                best = (share, mode, d);
            }
        }
        best.2
    });
    let rows = sample(head, delimiter);
    Dialect {
        delimiter,
        quote: b'"',
        has_header: looks_like_header(&rows),
    }
}

/// The first records' field values with delimiter `d`; a last record cut by the end of the
/// sample is dropped when it disagrees with the one before.
fn sample(head: &[u8], delimiter: u8) -> Vec<Vec<Vec<u8>>> {
    let d = Dialect {
        delimiter,
        ..Dialect::default()
    };
    let mut rows = Vec::new();
    let mut fields = Vec::new();
    let mut at = 0;
    while at < head.len() && rows.len() < SNIFF_RECORDS {
        at = record(head, at, &d, &mut fields);
        rows.push(
            fields
                .iter()
                .map(|f| cell(head, f, &d).into_bytes())
                .collect::<Vec<_>>(),
        );
    }
    let cut = !head.ends_with(b"\n");
    if cut && rows.len() > 1 && rows[rows.len() - 1].len() != rows[rows.len() - 2].len() {
        rows.pop();
    }
    rows
}

fn is_numeric(field: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(field) else {
        return false;
    };
    let text = text.trim();
    text.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '+' | '.'))
        && text.parse::<f64>().is_ok()
}

fn looks_like_header(rows: &[Vec<Vec<u8>>]) -> bool {
    let Some((first, rest)) = rows.split_first() else {
        return false;
    };
    if rest.is_empty()
        || first
            .iter()
            .any(|f| f.trim_ascii().is_empty() || is_numeric(f))
    {
        return false;
    }
    if rest
        .iter()
        .any(|row| row.iter().take(first.len()).any(|f| is_numeric(f)))
    {
        return true;
    }
    let distinct = first
        .iter()
        .enumerate()
        .all(|(i, f)| !first[..i].contains(f));
    let repeated = rest
        .iter()
        .any(|row| row.iter().zip(first).any(|(a, b)| a == b));
    distinct && !repeated
}

/// Where every record starts, and what the records look like.
#[derive(Debug, Clone)]
pub struct Table {
    pub dialect: Dialect,
    /// Start offset of each record, the header included.
    starts: Vec<u32>,
    pub max_fields: usize,
    /// Records have different field counts.
    pub ragged: bool,
    /// Offset of a quote that is never closed.
    pub unterminated: Option<u32>,
    /// False when indexing was cancelled.
    pub complete: bool,
}

impl Table {
    /// Indexes `text` (at most 4 GiB) with dialect `d`.
    pub fn index(text: &[u8], d: Dialect, cancel: &AtomicBool) -> Table {
        let mut t = Table {
            dialect: d,
            starts: Vec::new(),
            max_fields: 0,
            ragged: false,
            unterminated: None,
            complete: false,
        };
        let mut fields = Vec::new();
        let mut at = if text.starts_with(b"\xEF\xBB\xBF") {
            3
        } else {
            0
        };
        let mut first_len = None;
        while at < text.len() {
            if t.starts.len().is_multiple_of(65_536) && cancel.load(Ordering::Relaxed) {
                return t;
            }
            t.starts.push(at as u32);
            let next = record(text, at, &d, &mut fields);
            let n = fields.len();
            t.ragged |= *first_len.get_or_insert(n) != n;
            t.max_fields = t.max_fields.max(n);
            if t.unterminated.is_none()
                && let Some(last) = fields.last()
                && text.get(last.start) == Some(&d.quote)
                && (last.len() < 2 || text[last.end - 1] != d.quote)
                && next == text.len()
                && !text.ends_with(b"\n")
            {
                t.unterminated = Some(last.start as u32);
            }
            at = next;
        }
        t.complete = true;
        t
    }

    /// Data rows (the header excluded).
    pub fn rows(&self) -> usize {
        self.starts
            .len()
            .saturating_sub(usize::from(self.dialect.has_header))
    }

    /// Field spans of data row `row`, or of the header when `row` is `None`.
    pub fn fields(&self, text: &[u8], row: Option<usize>) -> Option<Vec<Range<usize>>> {
        let i = match row {
            None if self.dialect.has_header => 0,
            None => return None,
            Some(r) => r + usize::from(self.dialect.has_header),
        };
        let start = *self.starts.get(i)? as usize;
        let mut fields = Vec::new();
        record(text, start, &self.dialect, &mut fields);
        Some(fields)
    }

    /// Data row and column at byte `offset`; the row is `None` on the header.
    pub fn cell_at(&self, text: &[u8], offset: usize) -> Option<(Option<usize>, usize)> {
        let i = self
            .starts
            .partition_point(|&s| s as usize <= offset)
            .checked_sub(1)?;
        let mut fields = Vec::new();
        record(text, self.starts[i] as usize, &self.dialect, &mut fields);
        let col = fields
            .iter()
            .position(|f| offset <= f.end)
            .unwrap_or(fields.len().saturating_sub(1));
        let header = usize::from(self.dialect.has_header);
        let row = if i < header { None } else { Some(i - header) };
        Some((row, col))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn rows(text: &str, d: Dialect) -> Vec<Vec<String>> {
        let t = Table::index(text.as_bytes(), d, &never());
        (0..t.rows())
            .map(|r| {
                t.fields(text.as_bytes(), Some(r))
                    .unwrap()
                    .iter()
                    .map(|f| cell(text.as_bytes(), f, &d))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn fields_quotes_and_line_ends() {
        let d = Dialect::default();
        let text = "a,\"b,c\",\"say \"\"hi\"\"\"\r\n1,\"multi\nline\",\n\n";
        assert_eq!(
            rows(text, d),
            [
                vec!["a", "b,c", "say \"hi\""],
                vec!["1", "multi\nline", ""],
                vec![""],
            ]
        );
        let t = Table::index(text.as_bytes(), d, &never());
        assert!(t.ragged);
        assert_eq!(t.max_fields, 3);
        assert_eq!(t.unterminated, None);
    }

    #[test]
    fn spans_point_into_the_source() {
        let d = Dialect::default();
        let text = b"x,\"y\"\n10,20\n";
        let t = Table::index(text, d, &never());
        let f = t.fields(text, Some(0)).unwrap();
        assert_eq!(&text[f[1].clone()], b"\"y\"");
        assert_eq!(t.cell_at(text, 7), Some((Some(1), 0)));
        assert_eq!(t.cell_at(text, 10), Some((Some(1), 1)));
        assert_eq!(t.cell_at(text, 0), Some((Some(0), 0)));
    }

    #[test]
    fn header_rows_are_kept_apart() {
        let d = Dialect {
            has_header: true,
            ..Dialect::default()
        };
        let text = b"name,age\nann,31\nbob,42\n";
        let t = Table::index(text, d, &never());
        assert_eq!(t.rows(), 2);
        let h = t.fields(text, None).unwrap();
        assert_eq!(&text[h[1].clone()], b"age");
        assert_eq!(t.cell_at(text, 2), Some((None, 0)));
        assert_eq!(t.cell_at(text, 17), Some((Some(1), 0)));
    }

    #[test]
    fn unterminated_quote_is_reported() {
        let t = Table::index(b"a,b\n1,\"open\n2,3", Dialect::default(), &never());
        assert_eq!(t.unterminated, Some(6));
        assert_eq!(t.rows(), 2);
    }

    #[test]
    fn sniffing_delimiters_and_headers() {
        assert_eq!(sniff(b"a;b;c\n1;2;3\n", None).delimiter, b';');
        assert_eq!(sniff(b"a\tb\n1\t2\n", None).delimiter, b'\t');
        assert_eq!(sniff(b"a|b|c\nx|y|z\n", None).delimiter, b'|');
        assert_eq!(sniff(b"a,b;c\n1,2;3\n", None).delimiter, b',');
        assert!(sniff(b"name,age\nann,31\n", None).has_header);
        assert!(!sniff(b"1,2\n3,4\n", None).has_header);
        assert!(
            !sniff(b"x,y\nx,z\n", None).has_header,
            "first value repeats below"
        );
        assert!(sniff(b"\xEF\xBB\xBFid,name\n1,a\n", None).has_header);
        assert_eq!(sniff(b"a,b\n", Some(b'\t')).delimiter, b'\t');
    }

    #[test]
    fn long_cells_are_cut() {
        let text = format!("{}\n", "x".repeat(CELL_CAP + 10));
        let r = rows(&text, Dialect::default());
        assert_eq!(r[0][0].chars().count(), CELL_CAP + 1);
    }

    #[test]
    fn bom_is_skipped_and_empty_text_has_no_rows() {
        let r = rows("\u{FEFF}a,b\n", Dialect::default());
        assert_eq!(r, [vec!["a", "b"]]);
        assert!(rows("", Dialect::default()).is_empty());
    }

    #[test]
    fn random_input_never_panics() {
        let alphabet: &[u8] = b"ab,;\"\r\n\t|1 ";
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..20_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let len = (seed % 30) as usize;
            let text: Vec<u8> = (0..len)
                .map(|i| alphabet[((seed >> (i % 40)) as usize + i * 3) % alphabet.len()])
                .collect();
            let d = sniff(&text, None);
            let t = Table::index(&text, d, &never());
            for r in 0..t.rows() {
                for f in t.fields(&text, Some(r)).unwrap() {
                    assert!(f.start <= f.end && f.end <= text.len());
                    let _ = cell(&text, &f, &d);
                }
            }
            for at in 0..=text.len() {
                let _ = t.cell_at(&text, at);
            }
        }
    }
}
