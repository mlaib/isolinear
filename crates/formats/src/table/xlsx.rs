//! Excel worksheet values. Formulas use saved caches; macros and external links stay inert.
//! Opening indexes worksheet metadata; row requests materialize only their selected cells.
mod xml;
mod zip;
use super::{Column, Schema, TableInfo, TableSource, cut};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, Read, Seek},
    path::Path,
};
use xml::{self as office, Ev, Ns};

pub const READY: bool = true;
const TABLE_BATCH_BYTES: usize = 256 * 1024 * 1024;
const MAX_CELLS: usize = 50_000_000;
const MAX_STRINGS: usize = 1_000_000;
const MAX_SHEETS: usize = 10_000;
const MAX_ROWS: u32 = 1_048_576;
const MAX_COLUMNS: u32 = 16_384;
const MAX_PAGE_CELLS: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SheetKind {
    Worksheet,
    Chart,
    Dialog,
    Macro,
}

#[derive(Clone, Debug)]
struct SheetRef {
    name: String,
    kind: SheetKind,
    hidden: bool,
    part: String,
    packed: u64,
    size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumFmt {
    General,
    Date,
    Time,
    DateTime,
    Percent,
}

struct Book {
    sheets: Vec<SheetRef>,
    strings: Vec<String>,
    /// Number format kind per cell style (`s` attribute).
    styles: Vec<NumFmt>,
    date1904: bool,
    active: usize,
    facts: Vec<(String, String)>,
    warnings: Vec<String>,
}

/// Bits of the kinds of value seen in a column.
const NUMBER: u8 = 1;
const TEXT: u8 = 2;
const DATE: u8 = 4;
const BOOL: u8 = 8;
const ERROR: u8 = 16;

#[derive(Default)]
struct Sheet {
    /// Non-empty rows in order: (row index from 0, cells as (column index, text) in order).
    rows: Vec<(u32, Vec<(u32, String)>)>,
    /// Rows and columns up to the last non-empty cell.
    height: u32,
    width: u32,
    /// Kinds of value per column below row 1, and in row 1 (often a header).
    kinds: Vec<u8>,
    first_row_kinds: Vec<u8>,
    formulas: usize,
    merged: usize,
    /// The cell budget stopped reading; later rows are not shown.
    truncated: bool,
}

fn main_part(file: &mut (impl Read + std::io::Seek), listing: &zip::Listing) -> Option<String> {
    office::main_part_by_type(
        file,
        listing,
        &[
            "spreadsheetml.sheet.main",
            "spreadsheetml.template.main",
            "ms-excel.sheet.macroEnabled.main",
            "ms-excel.template.macroEnabled.main",
        ],
        "xl/workbook.xml",
    )
}

/// The named sheet, else the active one when it is a worksheet, else the first worksheet.
fn pick(book: &Book, name: Option<&str>) -> Result<usize, String> {
    if let Some(name) = name {
        let i = book
            .sheets
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| format!("The workbook has no sheet named {name:?}"))?;
        if book.sheets[i].kind != SheetKind::Worksheet {
            return Err(format!(
                "{name:?} is a {}; it has no cells to show",
                kind_name(&book.sheets[i])
            ));
        }
        return Ok(i);
    }
    let worksheet = |i: &usize| book.sheets[*i].kind == SheetKind::Worksheet;
    Some(book.active)
        .filter(|i| *i < book.sheets.len() && worksheet(i) && !book.sheets[*i].hidden)
        .or_else(|| (0..book.sheets.len()).find(|i| worksheet(i) && !book.sheets[*i].hidden))
        .or_else(|| (0..book.sheets.len()).find(worksheet))
        .ok_or_else(|| "The workbook has no worksheets".into())
}

fn kind_name(s: &SheetRef) -> &'static str {
    match (s.kind, s.hidden) {
        (SheetKind::Worksheet, false) => "sheet",
        (SheetKind::Worksheet, true) => "hidden sheet",
        (SheetKind::Chart, _) => "chart sheet",
        (SheetKind::Dialog, _) => "dialog sheet",
        (SheetKind::Macro, _) => "macro sheet",
    }
}

fn read_book(
    file: &mut (impl Read + std::io::Seek),
    listing: &zip::Listing,
    cancelled: &dyn Fn() -> bool,
) -> Result<Book, String> {
    let main =
        main_part(file, listing).ok_or("Not an Excel workbook: the workbook part is missing")?;
    let xml = office::part(file, listing, &main)?.ok_or("The workbook part is missing")?;
    let rels = office::part(file, listing, &office::rels_of(&main))?
        .map(|r| office::relationship_ids(&r))
        .unwrap_or_default();
    let target = |suffix: &str| {
        rels.values()
            .find(|(kind, _, external)| kind.ends_with(suffix) && !external)
            .map(|(_, t, _)| office::resolve_target(&main, t))
    };

    let mut sheets = vec![];
    let mut date1904 = false;
    let mut active = 0usize;
    office::walk(&xml, cancelled, |ev| {
        if let Ev::Open(tag) = ev {
            match (tag.ns, tag.name.as_str()) {
                (Ns::S, "workbookPr") => {
                    date1904 = matches!(tag.attr(Ns::Other, "date1904"), Some("1" | "true"))
                }
                (Ns::S, "workbookView") => {
                    active = tag
                        .attr(Ns::Other, "activeTab")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0)
                }
                (Ns::S, "sheet") => {
                    if sheets.len() >= MAX_SHEETS {
                        return Err("The workbook exceeds 10,000 sheets".into());
                    }
                    let name = tag.attr(Ns::Other, "name").unwrap_or("").to_owned();
                    let hidden =
                        matches!(tag.attr(Ns::Other, "state"), Some("hidden" | "veryHidden"));
                    let rel = tag.attr(Ns::R, "id").and_then(|id| rels.get(id));
                    if let Some((kind, t, false)) = rel {
                        let kind = if kind.ends_with("/worksheet") {
                            SheetKind::Worksheet
                        } else if kind.ends_with("/chartsheet") {
                            SheetKind::Chart
                        } else if kind.ends_with("/dialogsheet") {
                            SheetKind::Dialog
                        } else {
                            SheetKind::Macro
                        };
                        let part = office::resolve_target(&main, t);
                        let entry = listing
                            .entries
                            .iter()
                            .find(|e| e.name.eq_ignore_ascii_case(&part));
                        sheets.push(SheetRef {
                            name,
                            kind,
                            hidden,
                            packed: entry.and_then(|e| e.packed).unwrap_or(0),
                            size: entry.map_or(0, |e| e.size),
                            part,
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(())
    })?;
    if sheets.is_empty() {
        return Err("The workbook lists no sheets".into());
    }

    let strings = match target("/sharedStrings") {
        Some(p) => office::part(file, listing, &p)?
            .map(|x| shared_strings(&x, cancelled))
            .transpose()?
            .unwrap_or_default(),
        None => vec![],
    };
    let styles = match target("/styles") {
        Some(p) => office::part(file, listing, &p)?
            .map(|x| number_formats(&x, cancelled))
            .transpose()?
            .unwrap_or_default(),
        None => vec![],
    };
    let mut facts = vec![];
    for name in ["docProps/core.xml", "docProps/app.xml"] {
        if let Some(x) = office::part(file, listing, name)? {
            facts.extend(office::properties(&x, cancelled));
        }
    }
    let mut warnings = vec![];
    if listing
        .entries
        .iter()
        .any(|e| e.name.to_ascii_lowercase().ends_with("vbaproject.bin"))
    {
        warnings.push("Contains macros; they are never run".to_owned());
    }
    if listing
        .entries
        .iter()
        .any(|e| e.name.to_ascii_lowercase().starts_with("xl/externallinks/"))
    {
        warnings.push("Links to other workbooks; they are never opened".to_owned());
    }
    Ok(Book {
        sheets,
        strings,
        styles,
        date1904,
        active,
        facts,
        warnings,
    })
}

/// The shared string table: each `si` as plain text (rich-text runs joined, phonetic hints left
/// out).
fn shared_strings(xml: &[u8], cancelled: &dyn Fn() -> bool) -> Result<Vec<String>, String> {
    shared_strings_with_limit(xml, cancelled, MAX_STRINGS)
}

fn shared_strings_with_limit(
    xml: &[u8],
    cancelled: &dyn Fn() -> bool,
    max_strings: usize,
) -> Result<Vec<String>, String> {
    let mut out = vec![];
    let mut current: Option<String> = None;
    let mut in_t = false;
    let mut phonetic = 0usize;
    let mut bytes = 0usize;
    office::walk(xml, cancelled, |ev| {
        match ev {
            Ev::Open(tag) => match (tag.ns, tag.name.as_str()) {
                (Ns::S, "si") => current = Some(String::new()),
                (Ns::S, "rPh") => phonetic += 1,
                (Ns::S, "t") => in_t = true,
                _ => {}
            },
            Ev::Close(Ns::S, "si") => {
                if out.len() >= max_strings {
                    return Err("The shared strings exceed 1,000,000 entries".into());
                }
                out.push(current.take().unwrap_or_default());
            }
            Ev::Close(Ns::S, "rPh") => phonetic = phonetic.saturating_sub(1),
            Ev::Close(Ns::S, "t") => in_t = false,
            Ev::Text(t) => {
                if in_t
                    && phonetic == 0
                    && let Some(c) = current.as_mut()
                {
                    bytes += t.len();
                    if bytes > TABLE_BATCH_BYTES {
                        return Err("The shared strings exceed 256 MiB".into());
                    }
                    c.push_str(t);
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(out)
}

/// Number-format kind of each cell style (`cellXfs`), from built-in ids and custom codes.
fn number_formats(xml: &[u8], cancelled: &dyn Fn() -> bool) -> Result<Vec<NumFmt>, String> {
    let mut custom: HashMap<u32, NumFmt> = HashMap::new();
    let mut xfs = vec![];
    let mut in_xfs = false;
    office::walk(xml, cancelled, |ev| {
        match ev {
            Ev::Open(tag) => match (tag.ns, tag.name.as_str()) {
                (Ns::S, "numFmt") => {
                    if let (Some(id), Some(code)) = (
                        tag.attr(Ns::Other, "numFmtId").and_then(|v| v.parse().ok()),
                        tag.attr(Ns::Other, "formatCode"),
                    ) {
                        custom.insert(id, classify(code));
                    }
                }
                (Ns::S, "cellXfs") => in_xfs = true,
                (Ns::S, "xf") if in_xfs => {
                    if xfs.len() >= 1_000_000 {
                        return Err("The workbook exceeds 1,000,000 styles".into());
                    }
                    let id: u32 = tag
                        .attr(Ns::Other, "numFmtId")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    xfs.push(custom.get(&id).copied().unwrap_or_else(|| builtin(id)));
                }
                _ => {}
            },
            Ev::Close(Ns::S, "cellXfs") => in_xfs = false,
            _ => {}
        }
        Ok(())
    })?;
    Ok(xfs)
}

fn builtin(id: u32) -> NumFmt {
    match id {
        9 | 10 => NumFmt::Percent,
        14..=17 | 27..=31 | 34..=36 | 50..=58 => NumFmt::Date,
        18..=21 | 32 | 33 | 45..=47 => NumFmt::Time,
        22 => NumFmt::DateTime,
        _ => NumFmt::General,
    }
}

/// Classify a custom format code by its first section, ignoring quoted text, escaped
/// characters and bracketed colours/locales (but not elapsed-time brackets like `[h]`).
fn classify(code: &str) -> NumFmt {
    let mut kept = String::new();
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        match c {
            ';' => break,
            '"' => {
                for c in chars.by_ref() {
                    if c == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                chars.next();
            }
            '[' => {
                let inner: String = chars.by_ref().take_while(|&c| c != ']').collect();
                let l = inner.to_ascii_lowercase();
                if !l.is_empty() && l.chars().all(|c| matches!(c, 'h' | 'm' | 's')) {
                    kept.push('h');
                }
            }
            c => kept.push(c.to_ascii_lowercase()),
        }
    }
    let date = kept.contains('y') || kept.contains('d');
    let time = kept.contains('h') || kept.contains('s');
    match (date, time) {
        (true, true) => NumFmt::DateTime,
        (true, false) => NumFmt::Date,
        (false, true) => NumFmt::Time,
        (false, false) if kept.contains('m') && !kept.contains("general") => NumFmt::Date,
        _ if kept.contains('%') => NumFmt::Percent,
        _ => NumFmt::General,
    }
}

/// "B12" → (column 1, row 11), both from 0.
fn cell_ref(r: &str) -> Option<(u32, u32)> {
    let letters = r.bytes().take_while(u8::is_ascii_alphabetic).count();
    if letters == 0 || letters > 3 {
        return None;
    }
    let mut col = 0u32;
    for b in r[..letters].bytes() {
        col = col * 26 + (b.to_ascii_uppercase() - b'A' + 1) as u32;
    }
    let row: u32 = r[letters..].parse().ok()?;
    ((1..=MAX_COLUMNS).contains(&col) && (1..=MAX_ROWS).contains(&row)).then(|| (col - 1, row - 1))
}

/// Column letters of a column index from 0: 0 → "A", 27 → "AB".
pub fn column_name(mut index: u32) -> String {
    let mut out = vec![];
    loop {
        out.push(b'A' + (index % 26) as u8);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn read_sheet(
    xml: &[u8],
    book: &Book,
    requested: Option<std::ops::Range<u32>>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Sheet, String> {
    read_sheet_with_budget(
        xml,
        book,
        requested,
        cancelled,
        MAX_CELLS,
        TABLE_BATCH_BYTES,
    )
}

fn read_sheet_with_budget(
    xml: &[u8],
    book: &Book,
    requested: Option<std::ops::Range<u32>>,
    cancelled: &dyn Fn() -> bool,
    max_cells: usize,
    max_bytes: usize,
) -> Result<Sheet, String> {
    let mut out = Sheet::default();
    let mut row: Option<u32> = None;
    let mut next_row = 0u32;
    let mut cells: Vec<(u32, String)> = vec![];
    // The cell being read: column, type, style, value text, inline string text.
    let mut cell: Option<(u32, String, usize)> = None;
    let mut next_col = 0u32;
    let mut value = String::new();
    let mut inline = String::new();
    let (mut in_v, mut in_is, mut in_t, mut phonetic) = (false, false, false, 0usize);
    let mut total_cells = 0usize;
    let mut bytes = 0usize;
    let finish_row = |out: &mut Sheet, row: u32, mut cells: Vec<(u32, String)>| {
        if cells.is_empty() {
            return;
        }
        cells.sort_by_key(|c| c.0);
        cells.dedup_by_key(|c| c.0);
        out.height = out.height.max(row + 1);
        if requested.as_ref().is_some_and(|range| range.contains(&row)) {
            out.rows.push((row, cells));
        }
    };
    let result = office::walk(xml, cancelled, |ev| {
        match ev {
            Ev::Open(tag) => match (tag.ns, tag.name.as_str()) {
                (Ns::S, "row") => {
                    let r = tag
                        .attr(Ns::Other, "r")
                        .and_then(|v| v.parse::<u32>().ok())
                        .filter(|&r| (1..=MAX_ROWS).contains(&r))
                        .map_or(next_row, |r| r - 1);
                    if r >= MAX_ROWS {
                        return Err("Worksheet exceeds Excel's row limit".into());
                    }
                    row = Some(r);
                    next_row = r + 1;
                    next_col = 0;
                }
                (Ns::S, "c") => {
                    // A reference outside Excel's grid drops the cell; none means "next".
                    let reference = tag.attr(Ns::Other, "r");
                    let at = reference.and_then(cell_ref);
                    if reference.is_some() && at.is_none() {
                        cell = None;
                        return Ok(());
                    }
                    let col = at.map_or(next_col, |(c, _)| c);
                    if row.is_none() {
                        row = Some(at.map_or(next_row, |(_, r)| r));
                    }
                    next_col = col + 1;
                    let t = tag.attr(Ns::Other, "t").unwrap_or("n").to_owned();
                    let s = tag
                        .attr(Ns::Other, "s")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    cell = Some((col, t, s));
                    value.clear();
                    inline.clear();
                }
                (Ns::S, "v") => in_v = true,
                (Ns::S, "is") => in_is = true,
                (Ns::S, "rPh") => phonetic += 1,
                (Ns::S, "t") => in_t = true,
                (Ns::S, "f") => out.formulas += 1,
                (Ns::S, "mergeCell") => out.merged += 1,
                _ => {}
            },
            Ev::Close(ns, name) => match (ns, name) {
                (Ns::S, "v") => in_v = false,
                (Ns::S, "is") => in_is = false,
                (Ns::S, "rPh") => phonetic = phonetic.saturating_sub(1),
                (Ns::S, "t") => in_t = false,
                (Ns::S, "c") => {
                    if let Some((col, t, s)) = cell.take()
                        && col < MAX_COLUMNS
                    {
                        let (nonempty, kind, len) = cell_metadata(book, &t, s, &value, &inline);
                        if nonempty {
                            total_cells += 1;
                            bytes = bytes.checked_add(len).ok_or("budget")?;
                            if total_cells > max_cells || bytes > max_bytes {
                                return Err("budget".into());
                            }
                            out.width = out.width.max(col + 1);
                            let kinds = if row == Some(0) {
                                &mut out.first_row_kinds
                            } else {
                                &mut out.kinds
                            };
                            if kinds.len() <= col as usize {
                                kinds.resize(col as usize + 1, 0);
                            }
                            kinds[col as usize] |= kind;
                            let text = if requested
                                .as_ref()
                                .is_some_and(|range| range.contains(&row.unwrap_or(0)))
                            {
                                cut(display(book, &t, s, &value, &inline).0)
                            } else {
                                String::new()
                            };
                            cells.push((col, text));
                        }
                    }
                }
                (Ns::S, "row") => {
                    if let Some(r) = row.take() {
                        finish_row(&mut out, r, std::mem::take(&mut cells));
                    }
                }
                _ => {}
            },
            Ev::Text(text) => {
                if value
                    .len()
                    .saturating_add(inline.len())
                    .saturating_add(text.len())
                    > TABLE_BATCH_BYTES
                {
                    return Err("budget".into());
                }
                if in_v {
                    value.push_str(text);
                } else if in_is && in_t && phonetic == 0 {
                    inline.push_str(text);
                }
            }
        }
        Ok(())
    });
    match result {
        Ok(()) => {}
        Err(e) if e == "budget" => out.truncated = true,
        Err(e) => {
            // A damaged sheet keeps the rows before the damage.
            if out.height == 0 && cells.is_empty() {
                return Err(e);
            }
            out.truncated = true;
        }
    }
    if let Some(r) = row {
        finish_row(&mut out, r, cells);
    }
    out.rows.sort_by_key(|r| r.0);
    out.rows.dedup_by_key(|r| r.0);
    Ok(out)
}

/// A cell's text and kind, by its type (`t`) and number format (style `s`).
fn display(book: &Book, t: &str, s: usize, value: &str, inline: &str) -> (String, u8) {
    match t {
        "s" => match value
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|i| book.strings.get(i))
        {
            Some(text) => (text.clone(), TEXT),
            None if value.is_empty() => (String::new(), 0),
            None => (format!("[missing shared string {value}]"), ERROR),
        },
        "inlineStr" => (inline.to_owned(), TEXT),
        "str" => (value.to_owned(), TEXT),
        "b" => (
            if value.trim() == "1" { "TRUE" } else { "FALSE" }.to_owned(),
            BOOL,
        ),
        "e" => (value.to_owned(), ERROR),
        "d" => (value.to_owned(), DATE),
        _ if value.is_empty() => (String::new(), 0),
        _ => {
            let fmt = book.styles.get(s).copied().unwrap_or(NumFmt::General);
            match value.trim().parse::<f64>() {
                Ok(x) if x.is_finite() => match fmt {
                    NumFmt::Date | NumFmt::Time | NumFmt::DateTime
                        if (0.0..2_958_466.0).contains(&x) =>
                    {
                        (serial(x, fmt, book.date1904), DATE)
                    }
                    NumFmt::Percent => (format!("{}%", general(x * 100.0)), NUMBER),
                    _ => (general(x), NUMBER),
                },
                _ => (value.to_owned(), TEXT),
            }
        }
    }
}

/// Excel's General display: whole numbers as integers, others rounded to 15 significant digits;
/// very large or small magnitudes in scientific notation (`1.23456789012346E+19`).
pub fn general(x: f64) -> String {
    if x == x.trunc() && x.abs() < 1e15 {
        return format!("{}", x as i64);
    }
    if x.abs() >= 1e15 || x.abs() < 1e-9 {
        let s = format!("{x:.14E}");
        let (mantissa, exponent) = s.split_once('E').unwrap_or((&s, "0"));
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        let exponent: i32 = exponent.parse().unwrap_or(0);
        let sign = if exponent < 0 { '-' } else { '+' };
        return format!("{mantissa}E{sign}{:02}", exponent.abs());
    }
    let rounded: f64 = format!("{x:.14e}").parse().unwrap_or(x);
    format!("{rounded}")
}

/// Days since 1970-01-01 → (year, month, day) (proleptic Gregorian).
fn civil(days: i64) -> (i64, u32, u32) {
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

/// An Excel serial date/time as ISO text. The 1900 system keeps Lotus's 29 February 1900
/// (serial 60), so serials before it count from 31 December 1899.
fn serial(x: f64, fmt: NumFmt, date1904: bool) -> String {
    let mut days = x.floor() as i64;
    let mut secs = ((x - x.floor()) * 86_400.0).round() as i64;
    if secs >= 86_400 {
        days += 1;
        secs -= 86_400;
    }
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if fmt == NumFmt::Time {
        return format!("{:02}:{m:02}:{s:02}", h + days * 24);
    }
    let date = if !date1904 && days == 60 {
        "1900-02-29".to_owned()
    } else {
        // Days from 1970-01-01 of the epoch: 1899-12-30 (or -31 before serial 60), 1904-01-01.
        let epoch = if date1904 {
            -24_107
        } else if days < 60 {
            -25_568
        } else {
            -25_569
        };
        let (y, mo, d) = civil(epoch + days);
        format!("{y:04}-{mo:02}-{d:02}")
    };
    if fmt == NumFmt::Date && secs == 0 {
        date
    } else {
        format!("{date} {h:02}:{m:02}:{s:02}")
    }
}

fn kind_label(bits: u8) -> &'static str {
    match bits {
        0 => "empty",
        NUMBER => "number",
        TEXT => "text",
        DATE => "date",
        BOOL => "boolean",
        ERROR => "error",
        _ => "mixed",
    }
}

/// Classify cells while indexing without allocating their formatted display values.
fn cell_metadata(book: &Book, t: &str, s: usize, value: &str, inline: &str) -> (bool, u8, usize) {
    let (text, kind) = match t {
        "s" => match value
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|i| book.strings.get(i))
        {
            Some(text) => (text.as_str(), TEXT),
            None => (value, ERROR),
        },
        "inlineStr" => (inline, TEXT),
        "str" => (value, TEXT),
        "b" => return (true, BOOL, 5),
        "e" => (value, ERROR),
        "d" => (value, DATE),
        _ => (
            value,
            match value.trim().parse::<f64>() {
                Ok(x) if x.is_finite() => {
                    match book.styles.get(s).copied().unwrap_or(NumFmt::General) {
                        NumFmt::Date | NumFmt::Time | NumFmt::DateTime
                            if (0.0..2_958_466.0).contains(&x) =>
                        {
                            DATE
                        }
                        _ => NUMBER,
                    }
                }
                _ => TEXT,
            },
        ),
    };
    (!text.is_empty(), kind, text.len())
}

/// Rows of a sheet as (row, [(column, text)]), empty cells left out.
pub(crate) type SheetRows = Vec<(u32, Vec<(u32, String)>)>;

/// The first sheet of a workbook in memory: its name, its rows, and whether they stop at the
/// cell budget. For standards carried in spreadsheets.
pub(crate) fn sheet_rows(
    bytes: &[u8],
    cancelled: &dyn Fn() -> bool,
) -> Result<(String, SheetRows, bool), String> {
    let mut file = std::io::Cursor::new(bytes);
    let listing = zip::list_zip(&mut file, cancelled).map_err(|e| e.to_string())?;
    let book = read_book(&mut file, &listing, cancelled)?;
    let index = pick(&book, None)?;
    let chosen = &book.sheets[index];
    let xml = office::part(&mut file, &listing, &chosen.part)?
        .ok_or_else(|| format!("The part of sheet {:?} is missing", chosen.name))?;
    // Every row, within the cell and byte budgets (`truncated` says when they stop it).
    let sheet = read_sheet(&xml, &book, Some(0..u32::MAX), cancelled)?;
    Ok((chosen.name.clone(), sheet.rows, sheet.truncated))
}

/// A number as Excel shows it with the number format `id` (or its custom `code`).
pub(crate) fn number_text(x: f64, id: u32, code: Option<&str>, date1904: bool) -> String {
    let fmt = code.map(classify).unwrap_or_else(|| builtin(id));
    match fmt {
        NumFmt::Date | NumFmt::Time | NumFmt::DateTime if (0.0..2_958_466.0).contains(&x) => {
            serial(x, fmt, date1904)
        }
        NumFmt::Percent => format!("{}%", general(x * 100.0)),
        _ => general(x),
    }
}

/// Whether the number format `id` (or its custom `code`) shows a date or a time.
pub(crate) fn is_date_format(id: u32, code: Option<&str>) -> bool {
    matches!(
        code.map(classify).unwrap_or_else(|| builtin(id)),
        NumFmt::Date | NumFmt::Time | NumFmt::DateTime
    )
}

pub fn open(path: &Path, table: Option<&str>) -> Result<Box<dyn TableSource>, String> {
    let file = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    Ok(Box::new(Xlsx::new(file, table)?))
}

struct Xlsx {
    book: Book,
    xml: Vec<u8>,
    schema: Schema,
}

impl Xlsx {
    fn new(mut file: impl Read + Seek, table: Option<&str>) -> Result<Self, String> {
        let listing = zip::list_zip(&mut file, &|| false).map_err(|e| e.to_string())?;
        let book = read_book(&mut file, &listing, &|| false)?;
        let index = pick(&book, table)?;
        let chosen = &book.sheets[index];
        let xml = office::part(&mut file, &listing, &chosen.part)?
            .ok_or_else(|| format!("The part of sheet {:?} is missing", chosen.name))?;
        let sheet = read_sheet(&xml, &book, None, &|| false)?;
        let columns: Vec<Column> = (0..sheet.width)
            .map(|c| {
                let below = sheet.kinds.get(c as usize).copied().unwrap_or(0);
                let first = sheet.first_row_kinds.get(c as usize).copied().unwrap_or(0);
                Column {
                    name: column_name(c),
                    type_name: kind_label(if below == 0 { first } else { below }).into(),
                }
            })
            .collect();
        let mut details = book.facts.clone();
        details.extend([
            ("Sheet part".into(), chosen.part.clone()),
            ("Sheet kind".into(), kind_name(chosen).into()),
            (
                "Date system".into(),
                if book.date1904 { "1904" } else { "1900" }.into(),
            ),
            (
                "Sheet bytes".into(),
                format!("{} stored, {} unpacked", chosen.packed, chosen.size),
            ),
            (
                "Formulas".into(),
                format!(
                    "{} (saved values only; nothing is recalculated; formula text is omitted)",
                    sheet.formulas
                ),
            ),
        ]);
        if sheet.merged > 0 {
            details.push((
                "Merged cell ranges".into(),
                format!("{} (the value shows in the first cell)", sheet.merged),
            ));
        }
        for warning in &book.warnings {
            details.push(("Warning".into(), warning.clone()));
        }
        if sheet.truncated {
            details.push(("Warning".into(),format!("Only the first {} rows of this sheet are shown (cell budget, or damage further on)",sheet.height)));
        }
        for s in &book.sheets {
            details.push((s.name.clone(), kind_name(s).into()));
        }
        let schema = Schema {
            tables: book
                .sheets
                .iter()
                .enumerate()
                .map(|(i, s)| TableInfo {
                    name: s.name.clone(),
                    rows: (i == index).then_some(sheet.height as u64),
                })
                .collect(),
            table: index,
            rows: Some(sheet.height as u64),
            text: Some(format!(
                "Sheet {:?} ({})\n{}",
                chosen.name,
                kind_name(chosen),
                columns
                    .iter()
                    .map(|c| format!("{}: {}\n", c.name, c.type_name))
                    .collect::<String>()
            )),
            columns,
            details,
        };
        Ok(Self { book, xml, schema })
    }
}

impl TableSource for Xlsx {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn rows(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        let end = start
            .saturating_add(count as u64)
            .min(self.schema.rows.unwrap_or(0));
        if start >= end {
            return Ok(Vec::new());
        }
        let width = self.schema.columns.len();
        if (end - start).saturating_mul(width.max(1) as u64) > MAX_PAGE_CELLS as u64 {
            return Err("A page request exceeds 1,000,000 cells; request fewer rows".into());
        }
        let sheet = read_sheet(
            &self.xml,
            &self.book,
            Some(start as u32..end as u32),
            &|| false,
        )?;
        let mut at = 0;
        let mut rows = Vec::with_capacity((end - start) as usize);
        for r in start as u32..end as u32 {
            let mut row = vec![Some(String::new()); width];
            if let Some((number, cells)) = sheet.rows.get(at)
                && *number == r
            {
                for (c, value) in cells {
                    if let Some(cell) = row.get_mut(*c as usize) {
                        *cell = Some(value.clone());
                    }
                }
                at += 1;
            }
            rows.push(row);
        }
        Ok(rows)
    }
}

#[cfg(test)]
mod tests;
