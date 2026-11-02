//! Fixed-width records (`.txt`, `.dat`) read with a layout the user writes next to the file:
//! `<name>.layout.toml` or `<name>.<ext>.layout.toml`. Field boundaries are never guessed: a
//! file without a layout is plain text. A layout names record types (told apart by the text at
//! given columns) and their fields (name, start column from 1, length, type, trim):
//!
//! ```toml
//! name = "Payments v2"
//! [[record]]
//! name = "Payment"
//! match_start = 1
//! match = "D"
//! [[record.field]]
//! name = "Amount"
//! start = 2
//! length = 10
//! type = "decimal"   # text (default), integer, decimal, date
//! decimals = 2       # implied decimals when the value has no separator
//! # pattern = "YYYYMMDD" for dates; trim = false keeps spaces
//! ```
//!
//! Without `[[record]]`, top-level `[[field]]` tables describe every line.
use super::doc_common::{self, SimpleToml, TomlValue, clip, push_row, sidecar, table};
use crate::inspect::{
    Handler, Input, Inspection, OutlineNode, Probe, Stage, State, spec::decimal_sum,
};
use std::path::{Path, PathBuf};

const SUFFIX: &str = ".layout.toml";
const MAX_FIELDS: usize = 1000;
const MAX_RECORD_TYPES: usize = 200;
const MAX_ERRORS_LISTED: usize = 10_000;
const MAX_UNMATCHED_LISTED: usize = 1000;

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Text,
    Integer,
    Decimal(u32),
    Date(String),
}

#[derive(Clone, Debug)]
struct Field {
    name: String,
    start: usize,
    length: usize,
    kind: Kind,
    trim: bool,
}

#[derive(Clone, Debug, Default)]
struct RecordType {
    name: String,
    match_start: usize,
    matches: Option<String>,
    fields: Vec<Field>,
}

#[derive(Debug, Default)]
struct Layout {
    name: String,
    encoding: String,
    records: Vec<RecordType>,
}

fn layout_path(path: &Path) -> Option<PathBuf> {
    sidecar(path, SUFFIX)
}

fn field(pairs: &[(String, TomlValue)], record: &str) -> Result<Field, String> {
    let get = |k: &str| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| v);
    let name = get("name")
        .and_then(TomlValue::as_str)
        .ok_or_else(|| format!("a field of record {record:?} has no name"))?
        .to_owned();
    let start = get("start")
        .and_then(TomlValue::as_int)
        .filter(|&s| (1..=1_000_000).contains(&s))
        .ok_or_else(|| format!("field {name:?} needs a start column (from 1)"))?
        as usize;
    let length = match (
        get("length").and_then(TomlValue::as_int),
        get("end").and_then(TomlValue::as_int),
    ) {
        (Some(l), _) => l,
        (None, Some(end)) => end - start as i64 + 1,
        _ => 0,
    };
    if !(1..=100_000).contains(&length) {
        return Err(format!("field {name:?} needs a length (or an end column)"));
    }
    let kind = match get("type").and_then(TomlValue::as_str).unwrap_or("text") {
        "text" | "string" => Kind::Text,
        "integer" | "int" => Kind::Integer,
        "decimal" | "number" | "amount" => Kind::Decimal(
            get("decimals")
                .and_then(TomlValue::as_int)
                .filter(|d| (0..=18).contains(d))
                .unwrap_or(0) as u32,
        ),
        "date" => Kind::Date(
            get("pattern")
                .and_then(TomlValue::as_str)
                .unwrap_or("YYYYMMDD")
                .to_owned(),
        ),
        other => return Err(format!("field {name:?} has an unknown type {other:?}")),
    };
    Ok(Field {
        name,
        start,
        length: length as usize,
        kind,
        trim: get("trim").and_then(TomlValue::as_bool).unwrap_or(true),
    })
}

fn parse_layout(source: &str) -> Result<Layout, String> {
    let toml = SimpleToml::parse(source)?;
    let mut layout = Layout::default();
    let mut default = RecordType {
        name: "Record".into(),
        match_start: 1,
        ..RecordType::default()
    };
    for (header, _, pairs) in &toml.sections {
        let get = |k: &str| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| v);
        match header.as_str() {
            "" => {
                layout.name = get("name")
                    .and_then(TomlValue::as_str)
                    .unwrap_or("")
                    .to_owned();
                layout.encoding = get("encoding")
                    .and_then(TomlValue::as_str)
                    .unwrap_or("utf-8")
                    .to_owned();
            }
            "record" => {
                if layout.records.len() >= MAX_RECORD_TYPES {
                    return Err(format!("more than {MAX_RECORD_TYPES} record types"));
                }
                let name = get("name")
                    .and_then(TomlValue::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Record {}", layout.records.len() + 1));
                layout.records.push(RecordType {
                    name,
                    match_start: get("match_start")
                        .and_then(TomlValue::as_int)
                        .filter(|&s| s >= 1)
                        .unwrap_or(1) as usize,
                    matches: get("match").and_then(TomlValue::as_str).map(str::to_owned),
                    fields: vec![],
                });
            }
            "record.field" => {
                let Some(r) = layout.records.last_mut() else {
                    return Err("[[record.field]] comes before any [[record]]".into());
                };
                if r.fields.len() >= MAX_FIELDS {
                    return Err(format!(
                        "record {:?} has more than {MAX_FIELDS} fields",
                        r.name
                    ));
                }
                let f = field(pairs, &r.name)?;
                r.fields.push(f);
            }
            "field" => {
                if default.fields.len() >= MAX_FIELDS {
                    return Err(format!("more than {MAX_FIELDS} fields"));
                }
                default.fields.push(field(pairs, "Record")?);
            }
            other => return Err(format!("unknown table [{other}] in the layout")),
        }
    }
    if !default.fields.is_empty() {
        layout.records.push(default);
    }
    layout.records.retain(|r| !r.fields.is_empty());
    if layout.records.is_empty() {
        return Err("the layout defines no fields".into());
    }
    Ok(layout)
}

/// Characters `start..start+len` (from 1) of `line`; shorter lines give what exists.
fn columns(line: &str, start: usize, len: usize) -> (&str, bool) {
    let begin = start - 1;
    if line.is_ascii() {
        let s = begin.min(line.len());
        let e = (begin + len).min(line.len());
        return (&line[s..e], begin + len <= line.len());
    }
    let mut idx = line
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(line.len()));
    let s = idx.nth(begin).unwrap_or(line.len());
    let mut idx = line[s..]
        .char_indices()
        .map(|(i, _)| i + s)
        .chain(std::iter::once(line.len()));
    let e = idx.nth(len);
    (&line[s..e.unwrap_or(line.len())], e.is_some())
}

/// The value of a field converted by its type, or why it cannot be.
fn convert(raw: &str, f: &Field) -> Result<String, String> {
    let v = if f.trim { raw.trim() } else { raw };
    if v.trim().is_empty() {
        return Ok(String::new());
    }
    match &f.kind {
        Kind::Text => Ok(v.to_owned()),
        Kind::Integer => {
            let t = v.trim();
            let (neg, digits) = sign(t);
            if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
                return Err("not a whole number".into());
            }
            let d = digits.trim_start_matches('0');
            let d = if d.is_empty() { "0" } else { d };
            Ok(if neg && d != "0" {
                format!("-{d}")
            } else {
                d.to_owned()
            })
        }
        Kind::Decimal(places) => {
            let t = v.trim();
            if t.contains(['.', ',']) {
                return doc_common::parse_amount(t).ok_or_else(|| "not a decimal number".into());
            }
            let (neg, digits) = sign(t);
            if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
                return Err("not a decimal number".into());
            }
            let places = *places as usize;
            let padded = format!("{digits:0>width$}", width = places + 1);
            let (int, frac) = padded.split_at(padded.len() - places);
            let int = int.trim_start_matches('0');
            let int = if int.is_empty() { "0" } else { int };
            let body = if places == 0 {
                int.to_owned()
            } else {
                format!("{int}.{frac}")
            };
            let zero = body.chars().all(|c| c == '0' || c == '.');
            Ok(if neg && !zero {
                format!("-{body}")
            } else {
                body
            })
        }
        Kind::Date(pattern) => {
            date(v.trim(), pattern).ok_or_else(|| format!("not a date in the form {pattern}"))
        }
    }
}

fn sign(t: &str) -> (bool, &str) {
    if let Some(r) = t.strip_prefix('-') {
        (true, r.trim())
    } else if let Some(r) = t.strip_suffix('-') {
        (true, r.trim())
    } else {
        (
            false,
            t.strip_prefix('+')
                .or_else(|| t.strip_suffix('+'))
                .unwrap_or(t)
                .trim(),
        )
    }
}

/// A date by a pattern of `YYYY`, `YY`, `MM`, `DD`, `hh`, `mm`, `ss` and literal characters.
fn date(v: &str, pattern: &str) -> Option<String> {
    let (vb, pb) = (v.as_bytes(), pattern.as_bytes());
    if vb.len() != pb.len() {
        return None;
    }
    // Year, month, day, hour, minute, second.
    let mut vals: [Option<u32>; 6] = [None; 6];
    let mut i = 0;
    let num = |a: usize, n: usize| -> Option<u32> {
        let t = v.get(a..a + n)?;
        t.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| t.parse().ok())?
    };
    while i < pb.len() {
        let rest = &pattern[i..];
        let (slot, n) = if rest.starts_with("YYYY") {
            (0, 4)
        } else if rest.starts_with("YY") {
            (0, 2)
        } else if rest.starts_with("MM") {
            (1, 2)
        } else if rest.starts_with("DD") {
            (2, 2)
        } else if rest.starts_with("hh") {
            (3, 2)
        } else if rest.starts_with("mm") {
            (4, 2)
        } else if rest.starts_with("ss") {
            (5, 2)
        } else {
            if vb[i] != pb[i] {
                return None;
            }
            i += 1;
            continue;
        };
        let mut value = num(i, n)?;
        if slot == 0 && n == 2 {
            value += if value < 70 { 2000 } else { 1900 };
        }
        vals[slot] = Some(value);
        i += n;
    }
    let [y, mo, d, h, mi, s] = vals;
    let (y, mo, d) = (y?, mo.unwrap_or(1), d.unwrap_or(1));
    if !doc_common::valid_date(i64::from(y), mo, d) {
        return None;
    }
    let mut out = format!("{y:04}-{mo:02}-{d:02}");
    if let Some(h) = h {
        let (mi, s) = (mi.unwrap_or(0), s.unwrap_or(0));
        if h > 23 || mi > 59 || s > 59 {
            return None;
        }
        out.push_str(&format!(" {h:02}:{mi:02}:{s:02}"));
    }
    Some(out)
}

fn type_label(k: &Kind) -> String {
    match k {
        Kind::Text => "text".into(),
        Kind::Integer => "integer".into(),
        Kind::Decimal(0) => "decimal".into(),
        Kind::Decimal(p) => format!("decimal, {p} implied decimals"),
        Kind::Date(p) => format!("date {p}"),
    }
}

pub struct FixedWidth;
impl Handler for FixedWidth {
    fn name(&self) -> &'static str {
        "Fixed-width records with a layout"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        probe.ext_is(&["txt", "dat"]) && layout_path(probe.path).is_some()
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let path = layout_path(input.path).ok_or("The layout file is gone")?;
        let source = doc_common::read_sidecar(&path)?;
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let layout = parse_layout(&source)
            .map_err(|e| format!("The layout {file_name} cannot be used: {e}"))?;
        let text = match layout.encoding.to_ascii_lowercase().as_str() {
            "utf-8" | "utf8" | "ascii" | "us-ascii" => input.text()?.into_owned(),
            other => doc_common::decode_charset(input.bytes, other),
        };
        let reencoded = text.len() != input.bytes.len() || text.as_bytes() != input.bytes;
        let mut out = Inspection::new(
            "Fixed-width records",
            if layout.name.is_empty() {
                "Fixed-width records (user layout)"
            } else {
                &layout.name
            },
        );
        out.fact("Layout", "Layout file", file_name.clone(), None);
        if !layout.name.is_empty() {
            out.fact("Layout", "Layout name", layout.name.clone(), None);
        }
        out.fact(
            "Layout",
            "Record types",
            layout.records.len().to_string(),
            None,
        );
        out.fact("Layout", "Encoding", layout.encoding.clone(), None);
        for r in &layout.records {
            out.outline.push(OutlineNode {
                depth: 0,
                label: r.name.clone(),
                value: match &r.matches {
                    Some(m) => format!("lines with {m:?} at column {}", r.match_start),
                    None => "every other line".into(),
                },
                source: None,
            });
            for f in &r.fields {
                out.outline.push(OutlineNode {
                    depth: 1,
                    label: f.name.clone(),
                    value: format!(
                        "columns {}–{}, {}",
                        f.start,
                        f.start + f.length - 1,
                        type_label(&f.kind)
                    ),
                    source: None,
                });
            }
        }
        let mut tables: Vec<crate::inspect::Table> = layout
            .records
            .iter()
            .map(|r| {
                let mut cols = vec!["Line"];
                cols.extend(r.fields.iter().map(|f| f.name.as_str()));
                table(&r.name, &cols)
            })
            .collect();
        let mut counts = vec![0usize; layout.records.len()];
        let mut decimals: Vec<Vec<Vec<String>>> = layout
            .records
            .iter()
            .map(|r| vec![vec![]; r.fields.len()])
            .collect();
        let mut errors = table(
            "Conversion errors",
            &["Line", "Record", "Field", "Value", "Problem"],
        );
        let mut error_count = 0usize;
        let mut unmatched = table("Unrecognized lines", &["Line", "Text"]);
        let mut unmatched_count = 0usize;
        let mut short = 0usize;
        let mut offset = 0usize;
        for (n, raw) in text.split_inclusive('\n').enumerate() {
            if n % 65_536 == 0 && cancelled() {
                return Err("Cancelled".into());
            }
            let start = offset;
            offset += raw.len();
            let line = raw.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                continue;
            }
            let source = (!reencoded).then_some(start as u64..(start + line.len()) as u64);
            let which = layout
                .records
                .iter()
                .position(|r| {
                    r.matches
                        .as_ref()
                        .is_some_and(|m| columns(line, r.match_start, m.chars().count()).0 == m)
                })
                .or_else(|| layout.records.iter().position(|r| r.matches.is_none()));
            let Some(which) = which else {
                unmatched_count += 1;
                if unmatched.rows.len() < MAX_UNMATCHED_LISTED {
                    push_row(
                        &mut unmatched,
                        vec![(n + 1).to_string(), clip(line, 200)],
                        source,
                    );
                }
                continue;
            };
            let r = &layout.records[which];
            counts[which] += 1;
            let mut cells = vec![(n + 1).to_string()];
            let mut cut = false;
            for (fi, f) in r.fields.iter().enumerate() {
                let (raw, complete) = columns(line, f.start, f.length);
                cut |= !complete;
                match convert(raw, f) {
                    Ok(v) => {
                        if matches!(f.kind, Kind::Decimal(_))
                            && !v.is_empty()
                            && decimals[which][fi].len() < 10_000_000
                        {
                            decimals[which][fi].push(v.clone());
                        }
                        cells.push(v);
                    }
                    Err(problem) => {
                        error_count += 1;
                        if errors.rows.len() < MAX_ERRORS_LISTED {
                            push_row(
                                &mut errors,
                                vec![
                                    (n + 1).to_string(),
                                    r.name.clone(),
                                    f.name.clone(),
                                    raw.to_owned(),
                                    problem,
                                ],
                                source.clone(),
                            );
                        }
                        cells.push(raw.trim().to_owned());
                    }
                }
            }
            if cut {
                short += 1;
            }
            push_row(&mut tables[which], cells, source);
        }
        for (i, r) in layout.records.iter().enumerate() {
            out.fact("Records", &r.name, counts[i].to_string(), None);
            for (fi, f) in r.fields.iter().enumerate() {
                if matches!(f.kind, Kind::Decimal(_))
                    && !decimals[i][fi].is_empty()
                    && let Some(sum) = decimal_sum(decimals[i][fi].iter().map(String::as_str))
                {
                    out.fact("Totals", &format!("{} — {}", r.name, f.name), sum, None);
                }
            }
        }
        if unmatched_count > 0 {
            out.fact(
                "Records",
                "Unrecognized lines",
                unmatched_count.to_string(),
                None,
            );
        }
        for t in tables {
            if !t.rows.is_empty() {
                out.tables.push(t);
            }
        }
        out.check(
            Stage::Parsed,
            if unmatched_count == 0 {
                State::Passed
            } else {
                State::Partial
            },
            if unmatched_count == 0 {
                format!("Every line matched a record type of {file_name}")
            } else {
                format!("{unmatched_count} lines match no record type of the layout")
            },
        );
        out.check(
            Stage::Semantic,
            if error_count == 0 {
                State::Passed
            } else {
                State::Failed
            },
            if error_count == 0 {
                "Every integer, decimal and date field converted".to_owned()
            } else {
                format!("{error_count} field values could not be converted to their type")
            },
        );
        out.not_validated();
        if short > 0 {
            out.notes.push(format!(
                "{short} lines are shorter than their record layout; the missing fields are empty"
            ));
        }
        if reencoded {
            out.notes.push(
                "The text was decoded from its encoding: rows do not point at byte positions"
                    .into(),
            );
        }
        if error_count > 0 {
            out.tables.push(errors);
        }
        if unmatched_count > 0 {
            out.tables.push(unmatched);
        }
        out.notes.push(format!("Fields are cut at the columns given by {file_name}; Isolinear never guesses field boundaries"));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_and_conversions() {
        let l = parse_layout(
            "name = \"T\"\n[[record]]\nname = \"H\"\nmatch = \"H\"\n[[record.field]]\nname = \"Date\"\nstart = 2\nlength = 8\ntype = \"date\"\n[[record]]\nname = \"D\"\nmatch = \"D\"\n[[record.field]]\nname = \"Amount\"\nstart = 2\nend = 7\ntype = \"decimal\"\ndecimals = 2\n",
        )
        .unwrap();
        assert_eq!(l.records.len(), 2);
        assert_eq!(l.records[1].fields[0].length, 6);
        let amount = &l.records[1].fields[0];
        assert_eq!(convert("001234", amount).unwrap(), "12.34");
        assert_eq!(convert("-00050", amount).unwrap(), "-0.50");
        assert_eq!(convert("  12.5", amount).unwrap(), "12.5");
        assert!(convert("12x456", amount).is_err());
        assert_eq!(
            convert("20261103", &l.records[0].fields[0]).unwrap(),
            "2026-11-03"
        );
        assert!(convert("20261332", &l.records[0].fields[0]).is_err());
        assert_eq!(
            date("03.11.26 10:15", "DD.MM.YY hh:mm").unwrap(),
            "2026-11-03 10:15:00"
        );
        assert_eq!(columns("abcdef", 3, 2), ("cd", true));
        assert_eq!(columns("abc", 3, 2), ("c", false));
        assert_eq!(columns("çàbcd", 2, 2), ("àb", true));
        assert!(parse_layout("[[field]]\nname = \"x\"\n").is_err());
        assert!(parse_layout("[[record.field]]\nname = \"x\"\nstart = 1\nlength = 1\n").is_err());
        assert!(parse_layout("[other]\n").is_err());
        assert!(parse_layout("x = \n").is_err());
    }
}
