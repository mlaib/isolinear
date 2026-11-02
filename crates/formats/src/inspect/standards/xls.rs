//! Legacy Excel workbooks (`.xls`, BIFF8 in a Compound File `Workbook` stream, [MS-XLS]):
//! the sheet list (BOUNDSHEET), shared strings (SST with CONTINUE records), and the cell
//! records LABELSST, LABEL, NUMBER, RK, MULRK, BOOLERR and FORMULA with its cached result —
//! formulas are never calculated. Dates come from the cell's format (XF → FORMAT) where a
//! format is clearly a date. Encrypted workbooks (FILEPASS) and BIFF5 are recognized with a note.
use super::cfb::{self, Cfb, Locator};
use super::doc_common::{decode_charset, push_row, table, u16le, u32le, u64le};
use crate::inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Stage, State, Table};
use crate::table::xlsx::{column_name, is_date_format, number_text};
use std::collections::HashMap;

/// BIFF8 has 256 columns; cells are kept up to this budget per workbook.
const MAX_COLUMNS: usize = 256;
const MAX_CELLS: usize = 20_000_000;
const MAX_STRINGS: usize = 10_000_000;

struct Rec<'a> {
    kind: u16,
    data: &'a [u8],
    offset: usize,
}

/// Records from `at`, until the end of the stream or a cut record.
fn records(stream: &[u8], mut at: usize) -> impl Iterator<Item = Rec<'_>> {
    std::iter::from_fn(move || {
        if at + 4 > stream.len() {
            return None;
        }
        let kind = u16le(stream, at);
        let len = u16le(stream, at + 2) as usize;
        let data = stream.get(at + 4..at + 4 + len)?;
        let r = Rec {
            kind,
            data,
            offset: at,
        };
        at += 4 + len;
        Some(r)
    })
}

/// Reads across an SST record and its CONTINUE records.
struct Segments<'a> {
    segs: Vec<&'a [u8]>,
    i: usize,
    pos: usize,
}
impl Segments<'_> {
    fn byte(&mut self) -> Option<u8> {
        while self.pos >= self.segs.get(self.i)?.len() {
            self.i += 1;
            self.pos = 0;
        }
        let b = self.segs[self.i][self.pos];
        self.pos += 1;
        Some(b)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes([self.byte()?, self.byte()?]))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes([
            self.byte()?,
            self.byte()?,
            self.byte()?,
            self.byte()?,
        ]))
    }
    fn skip(&mut self, mut n: usize) -> Option<()> {
        while n > 0 {
            let seg = self.segs.get(self.i)?;
            let avail = seg.len() - self.pos.min(seg.len());
            if avail == 0 {
                self.i += 1;
                self.pos = 0;
                continue;
            }
            let k = avail.min(n);
            self.pos += k;
            n -= k;
        }
        Some(())
    }
    /// `cch` characters; a CONTINUE boundary inside them starts with a fresh flags byte.
    fn chars(&mut self, cch: usize, mut high: bool) -> Option<String> {
        let mut units: Vec<u16> = Vec::with_capacity(cch.min(65_536));
        while units.len() < cch {
            let seg = self.segs.get(self.i)?;
            if self.pos >= seg.len() {
                self.i += 1;
                self.pos = 0;
                let flags = *self.segs.get(self.i)?.first()?;
                self.pos = 1;
                high = flags & 1 != 0;
                continue;
            }
            let width = if high { 2 } else { 1 };
            let avail = (seg.len() - self.pos) / width;
            if avail == 0 {
                return None;
            }
            let take = avail.min(cch - units.len());
            let bytes = &seg[self.pos..self.pos + take * width];
            if high {
                units.extend(
                    bytes
                        .chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]])),
                );
            } else {
                units.extend(bytes.iter().map(|&b| u16::from(b)));
            }
            self.pos += take * width;
        }
        Some(String::from_utf16_lossy(&units))
    }
}

fn shared_strings(segs: Vec<&[u8]>) -> (Vec<String>, bool) {
    let mut r = Segments { segs, i: 0, pos: 0 };
    let mut out = vec![];
    let (Some(_total), Some(unique)) = (r.u32(), r.u32()) else {
        return (out, true);
    };
    for _ in 0..(unique as usize).min(MAX_STRINGS) {
        let Some(s) = (|| {
            let cch = r.u16()? as usize;
            let flags = r.byte()?;
            let runs = if flags & 8 != 0 { r.u16()? as usize } else { 0 };
            let ext = if flags & 4 != 0 { r.u32()? as usize } else { 0 };
            let s = r.chars(cch, flags & 1 != 0)?;
            r.skip(runs * 4)?;
            r.skip(ext)?;
            Some(s)
        })() else {
            return (out, true);
        };
        out.push(s);
    }
    (out, false)
}

/// An XLUnicodeString (16-bit count) or ShortXLUnicodeString (8-bit count) at the start of `b`.
fn unicode_string(b: &[u8], short: bool) -> String {
    let (cch, at) = if short {
        (b.first().copied().unwrap_or(0) as usize, 1)
    } else {
        (u16le(b, 0) as usize, 2)
    };
    let flags = b.get(at).copied().unwrap_or(0);
    let data = b.get(at + 1..).unwrap_or(&[]);
    if flags & 1 != 0 {
        let units: Vec<u16> = data
            .chunks_exact(2)
            .take(cch)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        data.iter().take(cch).map(|&c| c as char).collect()
    }
}

fn rk(v: u32) -> f64 {
    let x = if v & 2 != 0 {
        f64::from((v as i32) >> 2)
    } else {
        f64::from_bits(u64::from(v & 0xFFFF_FFFC) << 32)
    };
    if v & 1 != 0 { x / 100.0 } else { x }
}

fn error_text(code: u8) -> &'static str {
    match code {
        0x00 => "#NULL!",
        0x07 => "#DIV/0!",
        0x0F => "#VALUE!",
        0x17 => "#REF!",
        0x1D => "#NAME?",
        0x24 => "#NUM!",
        0x2A => "#N/A",
        _ => "#ERROR",
    }
}

struct Sheet {
    name: String,
    kind: &'static str,
    visibility: &'static str,
    offset: usize,
}

struct Globals {
    version: u16,
    encrypted: bool,
    date1904: bool,
    codepage: Option<u16>,
    sheets: Vec<Sheet>,
    strings: Vec<String>,
    strings_cut: bool,
    formats: HashMap<u16, String>,
    /// Format id per XF index.
    xfs: Vec<u16>,
}

fn globals(stream: &[u8]) -> Result<Globals, String> {
    let mut g = Globals {
        version: 0,
        encrypted: false,
        date1904: false,
        codepage: None,
        sheets: vec![],
        strings: vec![],
        strings_cut: false,
        formats: HashMap::new(),
        xfs: vec![],
    };
    let mut iter = records(stream, 0).peekable();
    let first = iter.next().ok_or("The workbook stream is empty")?;
    if !matches!(first.kind, 0x0809 | 0x0409 | 0x0209 | 0x0009) {
        return Err("The workbook stream does not start with a BOF record".into());
    }
    g.version = u16le(first.data, 0);
    while let Some(r) = iter.next() {
        match r.kind {
            0x000A => break,
            0x002F => g.encrypted = true,
            0x0022 => g.date1904 = u16le(r.data, 0) == 1,
            0x0042 => g.codepage = Some(u16le(r.data, 0)),
            0x041E if g.formats.len() < 100_000 => {
                let s = if g.version >= 0x0600 {
                    unicode_string(&r.data[2.min(r.data.len())..], false)
                } else {
                    let n = r.data.get(2).copied().unwrap_or(0) as usize;
                    r.data
                        .get(3..3 + n)
                        .unwrap_or(&[])
                        .iter()
                        .map(|&c| c as char)
                        .collect()
                };
                g.formats.insert(u16le(r.data, 0), s);
            }
            0x00E0 if g.xfs.len() < 1_000_000 => g.xfs.push(u16le(r.data, 2)),
            0x0085 => {
                let name = if g.version >= 0x0600 {
                    unicode_string(r.data.get(6..).unwrap_or(&[]), true)
                } else {
                    let n = r.data.get(6).copied().unwrap_or(0) as usize;
                    decode_charset(r.data.get(7..7 + n).unwrap_or(&[]), "windows-1252")
                };
                g.sheets.push(Sheet {
                    name,
                    offset: u32le(r.data, 0) as usize,
                    visibility: match r.data.get(4).copied().unwrap_or(0) & 3 {
                        0 => "visible",
                        1 => "hidden",
                        _ => "very hidden",
                    },
                    kind: match r.data.get(5).copied().unwrap_or(0) {
                        0 => "worksheet",
                        1 => "macro sheet",
                        2 => "chart",
                        6 => "VBA module",
                        _ => "other",
                    },
                });
            }
            0x00FC if !g.encrypted => {
                let mut segs = vec![r.data];
                while let Some(next) = iter.peek() {
                    if next.kind != 0x003C {
                        break;
                    }
                    segs.push(next.data);
                    iter.next();
                }
                let (strings, cut) = shared_strings(segs);
                g.strings = strings;
                g.strings_cut = cut;
            }
            _ => {}
        }
    }
    Ok(g)
}

struct Cells {
    /// (row, column, text, record offset, record length)
    cells: Vec<(u32, u16, String, usize, usize)>,
    formulas: usize,
    dates: usize,
    cut: bool,
}

fn sheet_cells(stream: &[u8], g: &Globals, offset: usize, budget: &mut usize) -> Cells {
    let mut out = Cells {
        cells: vec![],
        formulas: 0,
        dates: 0,
        cut: false,
    };
    let mut iter = records(stream, offset).peekable();
    match iter.next() {
        Some(r) if r.kind == 0x0809 => {}
        _ => {
            out.cut = true;
            return out;
        }
    }
    let number = |x: f64, xf: u16, dates: &mut usize| -> String {
        let id = g.xfs.get(xf as usize).copied().unwrap_or(0);
        let code = g.formats.get(&id).map(String::as_str);
        if is_date_format(u32::from(id), code) {
            *dates += 1;
        }
        number_text(x, u32::from(id), code, g.date1904)
    };
    let mut depth = 0usize;
    while let Some(r) = iter.next() {
        if *budget == 0 {
            out.cut = true;
            break;
        }
        let d = r.data;
        let (row, col, xf) = (u32::from(u16le(d, 0)), u16le(d, 2), u16le(d, 4));
        let mut push = |out: &mut Cells, col: u16, text: String| {
            if (col as usize) < MAX_COLUMNS && !text.is_empty() && *budget > 0 {
                *budget -= 1;
                out.cells.push((row, col, text, r.offset, d.len() + 4));
            }
        };
        match r.kind {
            0x0809 => depth += 1, // an embedded substream (chart)
            0x000A if depth > 0 => depth -= 1,
            0x000A => break,
            _ if depth > 0 => {}
            0x00FD => {
                let i = u32le(d, 6) as usize;
                let text = g
                    .strings
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("[missing shared string {i}]"));
                push(&mut out, col, text);
            }
            0x0204 => push(
                &mut out,
                col,
                unicode_string(d.get(6..).unwrap_or(&[]), false),
            ),
            0x0203 => {
                let t = number(f64::from_bits(u64le(d, 6)), xf, &mut out.dates);
                push(&mut out, col, t);
            }
            0x027E => {
                let t = number(rk(u32le(d, 6)), xf, &mut out.dates);
                push(&mut out, col, t);
            }
            0x00BD => {
                let last = u16le(d, d.len().saturating_sub(2));
                let mut at = 4;
                let mut c = col;
                while at + 6 <= d.len().saturating_sub(2) && c <= last {
                    let t = number(rk(u32le(d, at + 2)), u16le(d, at), &mut out.dates);
                    push(&mut out, c, t);
                    at += 6;
                    c += 1;
                }
            }
            0x0205 => {
                let v = d.get(6).copied().unwrap_or(0);
                let text = if d.get(7) == Some(&1) {
                    error_text(v).to_owned()
                } else if v != 0 {
                    "TRUE".into()
                } else {
                    "FALSE".into()
                };
                push(&mut out, col, text);
            }
            0x0006 => {
                out.formulas += 1;
                let res = d.get(6..14).unwrap_or(&[0; 8]);
                let text = if res[6] == 0xFF && res[7] == 0xFF {
                    match res[0] {
                        0 => match iter.peek() {
                            Some(s) if s.kind == 0x0207 => unicode_string(s.data, false),
                            _ => String::new(),
                        },
                        1 => if res[2] != 0 { "TRUE" } else { "FALSE" }.into(),
                        2 => error_text(res[2]).into(),
                        _ => String::new(),
                    }
                } else {
                    let mut b = [0u8; 8];
                    b.copy_from_slice(res);
                    number(f64::from_le_bytes(b), xf, &mut out.dates)
                };
                push(&mut out, col, text);
            }
            _ => {}
        }
    }
    out
}

pub struct Xls;
impl Handler for Xls {
    fn name(&self) -> &'static str {
        "Excel 97–2003 workbook"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let has = |name: &str| cfb::head_has_name(probe.head, name);
        // Other Office files may embed a workbook in a sub-storage: they are not workbooks.
        probe.head.starts_with(cfb::MAGIC)
            && !["__substg1.0_", "PowerPoint Document", "WordDocument"]
                .iter()
                .any(|n| has(n))
            && (has("Workbook") || has("Book") || probe.ext_is(&["xls", "xlt"]))
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let cfb = Cfb::open(input.bytes)?;
        let index = cfb
            .child(0, "Workbook")
            .or_else(|| cfb.child(0, "Book"))
            .ok_or("The Compound File has no Workbook stream: not an Excel workbook")?;
        let (stream, stream_cut) = cfb.read(index)?;
        let locator: Locator = cfb.locator(index);
        let g = globals(&stream)?;
        let mut out = Inspection::new("Microsoft Excel", "Excel 97–2003 workbook (.xls)");
        let biff = match g.version {
            0x0600 => "BIFF8 (Excel 97–2003)",
            0x0500 => "BIFF5 (Excel 5.0/95)",
            _ => "older BIFF",
        };
        out.version = Some(biff.into());
        out.fact("Workbook", "Format", biff, None);
        out.fact("Workbook", "Sheets", g.sheets.len().to_string(), None);
        out.fact(
            "Workbook",
            "Date system",
            if g.date1904 { "1904" } else { "1900" },
            None,
        );
        if let Some(cp) = g.codepage {
            out.fact(
                "Workbook",
                "Code page",
                if cp == 1200 {
                    "1200 (UTF-16)".into()
                } else {
                    cp.to_string()
                },
                None,
            );
        }
        if g.version >= 0x0600 {
            out.fact(
                "Workbook",
                "Shared strings",
                g.strings.len().to_string(),
                None,
            );
        }
        if cfb.child(0, "_VBA_PROJECT_CUR").is_some() {
            out.notes.push("Contains macros; they are never run".into());
        }
        let mut list = table("Sheets", &["Name", "Kind", "Visibility", "Stream offset"]);
        for s in &g.sheets {
            push_row(
                &mut list,
                vec![
                    s.name.clone(),
                    s.kind.into(),
                    s.visibility.into(),
                    s.offset.to_string(),
                ],
                locator.range(s.offset, 4),
            );
        }
        out.tables.push(list);
        if g.encrypted {
            out.check(
                Stage::Parsed,
                State::Partial,
                "The workbook is encrypted (FILEPASS): only the sheet list is shown",
            );
            out.notes.push("Encrypted workbooks are not decrypted; Isolinear never asks for or tries passwords".into());
            out.not_validated();
            return Ok(out);
        }
        if g.version < 0x0600 {
            out.check(
                Stage::Parsed,
                State::Partial,
                "BIFF5 and older workbooks: only the sheet list is read",
            );
            out.not_validated();
            return Ok(out);
        }
        let mut budget = MAX_CELLS;
        let mut formulas = 0;
        let mut dates = 0;
        let mut cut = stream_cut || g.strings_cut;
        for s in g.sheets.iter().filter(|s| s.kind == "worksheet") {
            if cancelled() {
                return Err("Cancelled".into());
            }
            let cells = sheet_cells(&stream, &g, s.offset, &mut budget);
            formulas += cells.formulas;
            dates += cells.dates;
            cut |= cells.cut;
            out.tables.push(sheet_table(&s.name, cells, &locator));
        }
        if formulas > 0 {
            out.fact(
                "Workbook",
                "Formulas",
                format!(
                    "{formulas} (their last calculated values are shown; nothing is recalculated)"
                ),
                None,
            );
        }
        if dates > 0 {
            out.fact("Workbook", "Date cells", dates.to_string(), None);
        }
        let mut damage = cfb.warnings.clone();
        if cut {
            damage.push("some records or strings are cut short".into());
        }
        if damage.is_empty() {
            out.check(
                Stage::Parsed,
                State::Passed,
                "BIFF8 records read: sheets, shared strings and cell values",
            );
        } else {
            out.check(
                Stage::Parsed,
                State::Partial,
                format!("The workbook is damaged or cut: {}", damage.join("; ")),
            );
        }
        out.not_validated();
        out.notes
            .push("Formatting, charts, pictures, comments and merged ranges are not shown".into());
        Ok(out)
    }
}

fn sheet_table(name: &str, cells: Cells, locator: &Locator) -> Table {
    let width = cells
        .cells
        .iter()
        .map(|c| c.1 as usize + 1)
        .max()
        .unwrap_or(0);
    let mut columns: Vec<String> = vec!["Row".into()];
    columns.extend((0..width as u32).map(column_name));
    let mut rows: Vec<(u32, Vec<String>, Option<std::ops::Range<u64>>)> = vec![];
    let mut sorted = cells.cells;
    sorted.sort_by_key(|c| (c.0, c.1));
    for (row, col, text, offset, len) in sorted {
        if rows.last().is_none_or(|r| r.0 != row) {
            if rows.len() >= MAX_ROWS {
                break;
            }
            rows.push((row, vec![String::new(); width], locator.range(offset, len)));
        }
        if let Some(r) = rows.last_mut() {
            r.1[col as usize] = text;
            if let (Some(src), Some(this)) = (r.2.as_mut(), locator.range(offset, len)) {
                src.start = src.start.min(this.start);
                src.end = src.end.max(this.end);
            }
        }
    }
    let mut t = Table {
        name: format!("Sheet {name}"),
        columns,
        rows: vec![],
        total: None,
    };
    for (row, cells, src) in rows {
        let mut c = vec![(row + 1).to_string()];
        c.extend(cells);
        push_row(&mut t, c, src);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rk_and_strings() {
        assert_eq!(rk((42 << 2) | 2), 42.0);
        assert_eq!(rk((1234 << 2) | 3), 12.34);
        assert_eq!(rk(0x3FF0_0000), 1.0);
        // A string split by a CONTINUE record that switches to 16-bit characters.
        let first = [2, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, b'a', b'b'];
        let second = [1, b'c', 0, 2, 0, 0, b'x', b'y'];
        let (s, cut) = shared_strings(vec![&first, &second]);
        assert_eq!(s, ["abc", "xy"]);
        assert!(!cut);
        let (s, cut) = shared_strings(vec![&first]);
        assert!(s.is_empty() && cut);
    }
}
