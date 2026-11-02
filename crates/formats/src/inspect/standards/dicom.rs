//! DICOM files (PS3.10): the 128-byte preamble and `DICM`, the file meta information, and the
//! data set in implicit VR little endian, explicit VR little endian (also deflated) or explicit
//! VR big endian. Sequences with defined and undefined lengths are read with bounded nesting.
//! Uncompressed monochrome and RGB pixel data become an image of the first frame; encapsulated
//! (compressed) pixel data is listed but not decoded.
use super::sci_common::{self, Cur, latin1};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};
use std::borrow::Cow;
use std::ops::Range;

pub struct Dicom;

/// Sequence nesting followed.
const MAX_DEPTH: u16 = 16;
/// Data elements read (in all sequences).
const MAX_ELEMENTS: usize = 200_000;
/// Rows of the element table.
const MAX_TABLE: usize = 50_000;
/// Largest deflated data set expanded.
const MAX_INFLATED: usize = 256 * 1024 * 1024;
/// Largest frame turned into an image (pixels).
const MAX_PIXELS: usize = 64 * 1024 * 1024;

const PIXEL_DATA: u32 = 0x7FE0_0010;
const ITEM: u32 = 0xFFFE_E000;
const ITEM_END: u32 = 0xFFFE_E00D;
const SEQ_END: u32 = 0xFFFE_E0DD;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Syntax {
    ImplicitLe,
    ExplicitLe,
    ExplicitBe,
}

#[derive(Clone, Debug)]
struct Elem {
    tag: u32,
    vr: [u8; 2],
    depth: u16,
    /// Element start and value range in the parsed buffer.
    start: usize,
    value: Range<usize>,
    /// Sequence items, or fragments of encapsulated pixel data.
    items: u32,
    encapsulated: bool,
    be: bool,
    /// The buffer is the file itself (not an inflated data set), so ranges are file offsets.
    in_file: bool,
}

struct Parser<'a> {
    b: &'a [u8],
    in_file: bool,
    elems: Vec<Elem>,
    problem: Option<String>,
    cancelled: &'a dyn Fn() -> bool,
}

fn long_vr(vr: &[u8]) -> bool {
    matches!(
        vr,
        b"OB"
            | b"OD"
            | b"OF"
            | b"OL"
            | b"OV"
            | b"OW"
            | b"SQ"
            | b"SV"
            | b"UC"
            | b"UN"
            | b"UR"
            | b"UT"
            | b"UV"
    )
}
fn valid_vr(vr: &[u8]) -> bool {
    long_vr(vr)
        || matches!(
            vr,
            b"AE"
                | b"AS"
                | b"AT"
                | b"CS"
                | b"DA"
                | b"DS"
                | b"DT"
                | b"FL"
                | b"FD"
                | b"IS"
                | b"LO"
                | b"LT"
                | b"PN"
                | b"SH"
                | b"SL"
                | b"SS"
                | b"ST"
                | b"TM"
                | b"UI"
                | b"UL"
                | b"US"
        )
}

impl<'a> Parser<'a> {
    fn fail(&mut self, msg: String) {
        if self.problem.is_none() {
            self.problem = Some(msg);
        }
    }
    fn tag_at(&self, pos: usize, be: bool) -> Option<u32> {
        let mut c = Cur::at(self.b, pos, be);
        let g = c.u16()? as u32;
        let e = c.u16()? as u32;
        Some(g << 16 | e)
    }

    /// Reads elements from `pos` to `end`; with `in_item`, stops after an item delimiter.
    /// Returns the position after what was read; `None` stops everything (damage).
    fn dataset(
        &mut self,
        mut pos: usize,
        end: usize,
        syntax: Syntax,
        depth: u16,
        in_item: bool,
    ) -> Option<usize> {
        let be = syntax == Syntax::ExplicitBe;
        while pos < end {
            if self.elems.len() >= MAX_ELEMENTS {
                self.fail(format!("Stopped after {MAX_ELEMENTS} data elements"));
                return None;
            }
            if (self.cancelled)() {
                self.fail("Cancelled".into());
                return None;
            }
            if end - pos < 8 {
                // Trailing padding is common; anything else is a cut element.
                if self.b[pos..end].iter().any(|&b| b != 0) {
                    self.fail(format!("The data set ends inside an element at byte {pos}"));
                }
                return Some(end);
            }
            let Some(tag) = self.tag_at(pos, be) else {
                self.fail(format!("Unreadable element at byte {pos}"));
                return None;
            };
            if tag >> 16 == 0xFFFE {
                match tag {
                    ITEM_END if in_item => return Some(pos + 8),
                    SEQ_END | ITEM_END => {
                        // Stray delimiter: skip it.
                        pos += 8;
                        continue;
                    }
                    _ => {
                        self.fail(format!("Unexpected item outside a sequence at byte {pos}"));
                        return None;
                    }
                }
            }
            let start = pos;
            let mut c = Cur::at(self.b, pos + 4, be);
            let raw_vr = self.b.get(pos + 4..pos + 6)?;
            let (vr, len, header) = if syntax != Syntax::ImplicitLe && valid_vr(raw_vr) {
                let vr = [raw_vr[0], raw_vr[1]];
                if long_vr(&vr) {
                    c.skip(4)?;
                    let Some(len) = c.u32() else {
                        self.fail(format!("The data set ends inside an element at byte {pos}"));
                        return None;
                    };
                    (vr, len, 12)
                } else {
                    c.skip(2)?;
                    (vr, c.u16()? as u32, 8)
                }
            } else {
                let len = Cur::at(self.b, pos + 4, be).u32()?;
                let vr = dict(tag).map_or(*b"UN", |d| {
                    let v = d.1.as_bytes();
                    [v[0], v[1]]
                });
                (vr, len, 8)
            };
            pos += header;
            let undefined = len == 0xFFFF_FFFF;
            let index = self.elems.len();
            self.elems.push(Elem {
                tag,
                vr,
                depth,
                start,
                value: pos..pos,
                items: 0,
                encapsulated: false,
                be,
                in_file: self.in_file,
            });
            if tag == PIXEL_DATA && undefined {
                // Encapsulated: a basic offset table item, then one item per fragment.
                self.elems[index].encapsulated = true;
                let mut n = 0u32;
                loop {
                    let Some(t) = self.tag_at(pos, be) else {
                        self.fail("The encapsulated pixel data is cut".into());
                        self.elems[index].value.end = self.b.len().min(end);
                        return None;
                    };
                    let flen = Cur::at(self.b, pos + 4, be).u32().unwrap_or(0) as usize;
                    pos += 8;
                    if t == SEQ_END {
                        break;
                    }
                    if t != ITEM || flen == 0xFFFF_FFFF {
                        self.fail(format!("Malformed pixel data fragment at byte {pos}"));
                        return None;
                    }
                    pos = match pos.checked_add(flen).filter(|&p| p <= end) {
                        Some(p) => p,
                        None => {
                            self.fail("The encapsulated pixel data is cut".into());
                            self.elems[index].value.end = end;
                            return None;
                        }
                    };
                    n += 1;
                }
                self.elems[index].items = n.saturating_sub(1);
                self.elems[index].value.end = pos;
                continue;
            }
            let implicit_sq = &vr == b"UN" && undefined;
            if &vr == b"SQ" || implicit_sq {
                self.elems[index].vr = *b"SQ";
                let inner = if implicit_sq {
                    Syntax::ImplicitLe
                } else {
                    syntax
                };
                let seq_end = if undefined {
                    end
                } else {
                    match pos.checked_add(len as usize).filter(|&p| p <= end) {
                        Some(p) => p,
                        None => {
                            self.fail(format!("A sequence at byte {start} runs past the end"));
                            end
                        }
                    }
                };
                if depth >= MAX_DEPTH {
                    self.fail(format!(
                        "Sequences nested deeper than {MAX_DEPTH} were skipped"
                    ));
                    if undefined {
                        return None;
                    }
                    pos = seq_end;
                    continue;
                }
                let ibe = inner == Syntax::ExplicitBe;
                let mut items = 0u32;
                loop {
                    if !undefined && pos >= seq_end {
                        break;
                    }
                    let Some(t) = self.tag_at(pos, ibe) else {
                        self.fail(format!("The sequence at byte {start} is cut"));
                        return None;
                    };
                    let Some(ilen) = Cur::at(self.b, pos + 4, ibe).u32() else {
                        self.fail(format!("The sequence at byte {start} is cut"));
                        return None;
                    };
                    pos += 8;
                    if t == SEQ_END {
                        break;
                    }
                    if t != ITEM {
                        self.fail(format!("Expected an item at byte {}", pos - 8));
                        return None;
                    }
                    items += 1;
                    self.elems.push(Elem {
                        tag: ITEM,
                        vr: *b"--",
                        depth: depth + 1,
                        start: pos - 8,
                        value: pos..pos,
                        items,
                        encapsulated: false,
                        be: ibe,
                        in_file: self.in_file,
                    });
                    let item_index = self.elems.len() - 1;
                    pos = if ilen == 0xFFFF_FFFF {
                        self.dataset(pos, seq_end, inner, depth + 1, true)?
                    } else {
                        let iend = match pos.checked_add(ilen as usize).filter(|&p| p <= seq_end) {
                            Some(p) => p,
                            None => {
                                self.fail(format!(
                                    "An item at byte {} runs past its sequence",
                                    pos - 8
                                ));
                                seq_end
                            }
                        };
                        self.dataset(pos, iend, inner, depth + 1, false)?;
                        iend
                    };
                    self.elems[item_index].value.end = pos;
                }
                self.elems[index].items = items;
                self.elems[index].value.end = pos;
                continue;
            }
            if undefined {
                self.fail(format!(
                    "Element ({:04X},{:04X}) has an undefined length that cannot be followed",
                    tag >> 16,
                    tag & 0xFFFF
                ));
                return None;
            }
            match pos.checked_add(len as usize).filter(|&p| p <= end) {
                Some(p) => {
                    self.elems[index].value = pos..p;
                    pos = p;
                }
                None => {
                    self.elems[index].value = pos..end.max(pos);
                    self.fail(format!(
                        "The file ends inside element ({:04X},{:04X})",
                        tag >> 16,
                        tag & 0xFFFF
                    ));
                    return None;
                }
            }
        }
        Some(pos)
    }
}

/// The data set as read: meta elements (from the file) and data set elements.
struct Read<'a> {
    file: &'a [u8],
    inflated: Option<Vec<u8>>,
    elems: Vec<Elem>,
    syntax: Syntax,
    syntax_uid: Option<String>,
    preamble: bool,
    problem: Option<String>,
}
impl Read<'_> {
    fn bytes(&self, e: &Elem) -> &[u8] {
        let b: &[u8] = if e.in_file {
            self.file
        } else {
            self.inflated.as_deref().unwrap_or(&[])
        };
        b.get(e.value.clone()).unwrap_or(&[])
    }
    fn top(&self, tag: u32) -> Option<&Elem> {
        self.elems.iter().find(|e| e.depth == 0 && e.tag == tag)
    }
    fn str(&self, tag: u32) -> Option<String> {
        let e = self.top(tag)?;
        let s = text(self, e, 64);
        (!s.is_empty()).then_some(s)
    }
    fn uint(&self, tag: u32) -> Option<u32> {
        let e = self.top(tag)?;
        let b = self.bytes(e);
        let mut c = Cur::new(b, e.be);
        match &e.vr {
            b"US" | b"SS" | b"OW" => c.u16().map(u32::from),
            b"UL" | b"SL" => c.u32(),
            _ => latin1(b).trim().split('\\').next()?.trim().parse().ok(),
        }
    }
    fn decimal(&self, tag: u32) -> Option<f64> {
        let e = self.top(tag)?;
        latin1(self.bytes(e))
            .trim_matches(|c: char| c == ' ' || c == '\0')
            .split('\\')
            .next()?
            .trim()
            .parse()
            .ok()
    }
}

fn read<'a>(file: &'a [u8], cancelled: &'a dyn Fn() -> bool) -> Result<Read<'a>, String> {
    let preamble = file.get(128..132) == Some(b"DICM");
    let mut pos = if preamble { 132 } else { 0 };
    let mut p = Parser {
        b: file,
        in_file: true,
        elems: Vec::new(),
        problem: None,
        cancelled,
    };
    // File meta information: group 0002, always explicit VR little endian.
    let meta_end = {
        let mut end = pos;
        let mut c = Cur::at(file, pos, false);
        if c.u16() == Some(0x0002) {
            // Group length when present; otherwise read while the group is 0002.
            if c.u16() == Some(0x0000) && file.get(pos + 4..pos + 6) == Some(b"UL") {
                let gl = Cur::at(file, pos + 8, false).u32().unwrap_or(0) as usize;
                end = (pos + 12).saturating_add(gl).min(file.len());
            } else {
                let mut q = pos;
                while Cur::at(file, q, false).u16() == Some(0x0002) {
                    let vr = file.get(q + 4..q + 6).unwrap_or(b"  ");
                    let next = if long_vr(vr) {
                        Cur::at(file, q + 8, false)
                            .u32()
                            .and_then(|l| (q + 12).checked_add(l as usize))
                    } else {
                        Cur::at(file, q + 6, false)
                            .u16()
                            .map(|l| q + 8 + l as usize)
                    };
                    match next {
                        Some(n) if n <= file.len() => q = n,
                        _ => break,
                    }
                }
                end = q;
            }
        }
        end
    };
    if meta_end > pos {
        p.dataset(pos, meta_end, Syntax::ExplicitLe, 0, false);
        pos = meta_end;
    } else if preamble {
        p.fail("No file meta information after the DICM prefix".into());
    }
    let mut r = Read {
        file,
        inflated: None,
        elems: Vec::new(),
        syntax: Syntax::ExplicitLe,
        syntax_uid: None,
        preamble,
        problem: None,
    };
    r.elems = std::mem::take(&mut p.elems);
    r.syntax_uid = r
        .str(0x0002_0010)
        .map(|s| s.trim_end_matches('\0').trim().to_owned());
    let uid = r.syntax_uid.clone().unwrap_or_default();
    r.syntax = match uid.as_str() {
        "1.2.840.10008.1.2" => Syntax::ImplicitLe,
        "1.2.840.10008.1.2.2" => Syntax::ExplicitBe,
        "" => guess_syntax(file, pos),
        _ => Syntax::ExplicitLe,
    };
    let mut problem = p.problem.take();
    let syntax = r.syntax;
    if uid == "1.2.840.10008.1.2.1.99" {
        let inflated = sci_common::inflate(&file[pos.min(file.len())..], MAX_INFLATED, cancelled);
        if let Some(e) = &inflated.error {
            problem.get_or_insert(format!("The deflated data set is damaged: {e}"));
        }
        if inflated.limited {
            problem.get_or_insert(format!(
                "The deflated data set was expanded to its first {} MiB only",
                MAX_INFLATED >> 20
            ));
        }
        r.inflated = Some(inflated.bytes);
        let b = r.inflated.as_deref().unwrap_or(&[]);
        let mut q = Parser {
            b,
            in_file: false,
            elems: Vec::new(),
            problem: None,
            cancelled,
        };
        q.dataset(0, b.len(), syntax, 0, false);
        r.elems.append(&mut q.elems);
        problem = problem.or(q.problem);
    } else {
        let mut q = Parser {
            b: file,
            in_file: true,
            elems: Vec::new(),
            problem: None,
            cancelled,
        };
        q.dataset(pos, file.len(), syntax, 0, false);
        r.elems.append(&mut q.elems);
        problem = problem.or(q.problem);
    }
    if r.elems.is_empty() {
        return Err(problem.unwrap_or_else(|| "No DICOM data elements were found".into()));
    }
    r.problem = problem;
    Ok(r)
}

/// Without file meta information: explicit when the bytes after the first tag are a VR.
fn guess_syntax(b: &[u8], pos: usize) -> Syntax {
    match b.get(pos + 4..pos + 6) {
        Some(vr) if valid_vr(vr) => Syntax::ExplicitLe,
        _ => Syntax::ImplicitLe,
    }
}

/// A data set without preamble: the first elements are known tags in ascending order.
fn bare_dataset(h: &[u8]) -> bool {
    let first = Cur::new(h, false).u16();
    if !matches!(first, Some(0x0002 | 0x0008)) {
        return false;
    }
    let explicit = guess_syntax(h, 0) == Syntax::ExplicitLe;
    let mut pos = 0usize;
    let mut last = 0u32;
    for _ in 0..4 {
        let mut c = Cur::at(h, pos, false);
        let (Some(g), Some(e)) = (c.u16(), c.u16()) else {
            return false;
        };
        let tag = (g as u32) << 16 | e as u32;
        if tag < last || g > 0x0028 {
            return false;
        }
        last = tag;
        let len = if explicit {
            let Some(vr) = h.get(pos + 4..pos + 6) else {
                return false;
            };
            let known = dict(tag).map(|d| d.1.as_bytes());
            if !valid_vr(vr) || known.is_some_and(|k| k != vr && e != 0) {
                return false;
            }
            if long_vr(vr) {
                pos += 12;
                Cur::at(h, pos - 4, false).u32()
            } else {
                pos += 8;
                Cur::at(h, pos - 2, false).u16().map(u32::from)
            }
        } else {
            if dict(tag).is_none() && e != 0 {
                return false;
            }
            pos += 8;
            Cur::at(h, pos - 4, false).u32()
        };
        match len {
            Some(l) if l < 1024 => pos += l as usize,
            _ => return false,
        }
        if pos >= h.len() {
            return false;
        }
    }
    true
}

/// Column of the element table for values that identify the patient (masked on request).
const PERSONAL_COLUMN: &str = "Patient-identifying value";

/// Patient module values, person names, and identifiers, addresses and telephone numbers.
fn identifying(e: &Elem) -> bool {
    e.tag >> 16 == 0x0010
        || &e.vr == b"PN"
        || matches!(
            e.tag,
            0x0008_0050 | 0x0008_0081 | 0x0008_0092 | 0x0008_0094 | 0x0038_0010 | 0x0038_0300
        )
}

fn tag_text(tag: u32) -> String {
    format!("({:04X},{:04X})", tag >> 16, tag & 0xFFFF)
}

fn keyword(tag: u32) -> Cow<'static, str> {
    if tag == ITEM {
        return "Item".into();
    }
    if let Some(d) = dict(tag) {
        return d.2.into();
    }
    let (g, e) = (tag >> 16, tag & 0xFFFF);
    if e == 0 {
        "GroupLength".into()
    } else if g % 2 == 1 {
        if (0x10..=0xFF).contains(&e) {
            "PrivateCreator".into()
        } else {
            "Private".into()
        }
    } else {
        "Unknown".into()
    }
}

fn is_string_vr(vr: &[u8; 2]) -> bool {
    matches!(
        vr,
        b"AE"
            | b"AS"
            | b"CS"
            | b"DA"
            | b"DS"
            | b"DT"
            | b"IS"
            | b"LO"
            | b"LT"
            | b"PN"
            | b"SH"
            | b"ST"
            | b"TM"
            | b"UC"
            | b"UI"
            | b"UR"
            | b"UT"
    )
}

/// The value as text: strings trimmed, numbers listed (at most `max` values), binary summarized.
fn text(r: &Read, e: &Elem, max: usize) -> String {
    let b = r.bytes(e);
    if e.tag == ITEM {
        return format!("item {}", e.items);
    }
    if e.encapsulated {
        return format!(
            "encapsulated pixel data, {} fragment(s), {} bytes",
            e.items,
            e.value.len()
        );
    }
    let vr = &e.vr;
    if vr == b"SQ" {
        return format!("sequence of {} item(s)", e.items);
    }
    if is_string_vr(vr) {
        let s = latin1(b);
        let s = s.trim_matches(|c: char| c == ' ' || c == '\0');
        return sci_common::clip(s, 400);
    }
    let mut c = Cur::new(b, e.be);
    let mut vals: Vec<String> = Vec::new();
    let total = match vr {
        b"US" | b"SS" => b.len() / 2,
        b"UL" | b"SL" | b"FL" | b"AT" => b.len() / 4,
        b"FD" | b"SV" | b"UV" => b.len() / 8,
        _ if b.is_empty() => return String::new(),
        _ => return format!("binary, {} bytes", b.len()),
    };
    for _ in 0..total.min(max) {
        let v = match vr {
            b"US" => c.u16().map(|v| v.to_string()),
            b"SS" => c.i16().map(|v| v.to_string()),
            b"UL" => c.u32().map(|v| v.to_string()),
            b"SL" => c.i32().map(|v| v.to_string()),
            b"FL" => c.f32().map(|v| sci_common::num(v as f64)),
            b"FD" => c.f64().map(sci_common::num),
            b"SV" => c.i64().map(|v| v.to_string()),
            b"UV" => c.u64().map(|v| v.to_string()),
            _ => {
                let g = c.u16();
                let el = c.u16();
                g.zip(el)
                    .map(|(g, el)| tag_text((g as u32) << 16 | el as u32))
            }
        };
        match v {
            Some(v) => vals.push(v),
            None => break,
        }
    }
    let mut s = vals.join("\\");
    if total > max {
        s.push_str(&format!("\\… ({total} values)"));
    }
    s
}

/// "20240115" -> "2024-01-15"; other values unchanged.
fn date(s: &str) -> String {
    let d = s.trim();
    if d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..])
    } else {
        d.to_owned()
    }
}
/// "101530.123" -> "10:15:30.123".
fn time(s: &str) -> String {
    let t = s.trim();
    let (hms, frac) = t.split_once('.').unwrap_or((t, ""));
    if hms.len() == 6 && hms.bytes().all(|b| b.is_ascii_digit()) {
        let mut out = format!("{}:{}:{}", &hms[..2], &hms[2..4], &hms[4..]);
        if !frac.is_empty() {
            out.push('.');
            out.push_str(frac);
        }
        out
    } else {
        t.to_owned()
    }
}
fn person(s: &str) -> String {
    s.split('^')
        .filter(|p| !p.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
}

fn syntax_name(uid: &str) -> Option<&'static str> {
    Some(match uid {
        "1.2.840.10008.1.2" => "Implicit VR Little Endian",
        "1.2.840.10008.1.2.1" => "Explicit VR Little Endian",
        "1.2.840.10008.1.2.1.99" => "Deflated Explicit VR Little Endian",
        "1.2.840.10008.1.2.2" => "Explicit VR Big Endian",
        "1.2.840.10008.1.2.4.50" => "JPEG Baseline (Process 1)",
        "1.2.840.10008.1.2.4.51" => "JPEG Extended (Process 2 & 4)",
        "1.2.840.10008.1.2.4.57" => "JPEG Lossless (Process 14)",
        "1.2.840.10008.1.2.4.70" => "JPEG Lossless (Process 14, SV1)",
        "1.2.840.10008.1.2.4.80" => "JPEG-LS Lossless",
        "1.2.840.10008.1.2.4.81" => "JPEG-LS Near-Lossless",
        "1.2.840.10008.1.2.4.90" => "JPEG 2000 (Lossless Only)",
        "1.2.840.10008.1.2.4.91" => "JPEG 2000",
        "1.2.840.10008.1.2.4.92" => "JPEG 2000 Part 2 Multi-component (Lossless Only)",
        "1.2.840.10008.1.2.4.93" => "JPEG 2000 Part 2 Multi-component",
        "1.2.840.10008.1.2.4.100" | "1.2.840.10008.1.2.4.100.1" => "MPEG2 Main Profile",
        "1.2.840.10008.1.2.4.101" | "1.2.840.10008.1.2.4.101.1" => "MPEG2 High Profile",
        "1.2.840.10008.1.2.4.102" | "1.2.840.10008.1.2.4.102.1" => "MPEG-4 AVC/H.264",
        "1.2.840.10008.1.2.4.103" | "1.2.840.10008.1.2.4.103.1" => "MPEG-4 AVC/H.264 BD",
        "1.2.840.10008.1.2.4.104" | "1.2.840.10008.1.2.4.105" | "1.2.840.10008.1.2.4.106" => {
            "MPEG-4 AVC/H.264 (stereo/high)"
        }
        "1.2.840.10008.1.2.4.107" | "1.2.840.10008.1.2.4.108" => "HEVC/H.265",
        "1.2.840.10008.1.2.4.201" => "High-Throughput JPEG 2000 (Lossless Only)",
        "1.2.840.10008.1.2.4.202" => "High-Throughput JPEG 2000 with RPCL (Lossless Only)",
        "1.2.840.10008.1.2.4.203" => "High-Throughput JPEG 2000",
        "1.2.840.10008.1.2.5" => "RLE Lossless",
        _ => return None,
    })
}

fn sop_class_name(uid: &str) -> Option<&'static str> {
    let s = uid.strip_prefix("1.2.840.10008.5.1.4.1.1.")?;
    Some(match s {
        "1" => "Computed Radiography Image",
        "1.1" => "Digital X-Ray Image (for presentation)",
        "1.1.1" => "Digital X-Ray Image (for processing)",
        "1.2" => "Digital Mammography X-Ray Image (for presentation)",
        "1.2.1" => "Digital Mammography X-Ray Image (for processing)",
        "1.3" => "Digital Intra-Oral X-Ray Image (for presentation)",
        "2" => "CT Image",
        "2.1" => "Enhanced CT Image",
        "3.1" => "Ultrasound Multi-frame Image",
        "4" => "MR Image",
        "4.1" => "Enhanced MR Image",
        "4.2" => "MR Spectroscopy",
        "6.1" => "Ultrasound Image",
        "6.2" => "Enhanced US Volume",
        "7" => "Secondary Capture Image",
        "7.1" => "Multi-frame Single Bit Secondary Capture Image",
        "7.2" => "Multi-frame Grayscale Byte Secondary Capture Image",
        "7.3" => "Multi-frame Grayscale Word Secondary Capture Image",
        "7.4" => "Multi-frame True Color Secondary Capture Image",
        "11.1" => "Grayscale Softcopy Presentation State",
        "12.1" => "X-Ray Angiographic Image",
        "12.2" => "X-Ray Radiofluoroscopic Image",
        "13.1.1" => "X-Ray 3D Angiographic Image",
        "13.1.3" => "Breast Tomosynthesis Image",
        "20" => "Nuclear Medicine Image",
        "66" => "Raw Data",
        "66.4" => "Segmentation",
        "67" => "Real World Value Mapping",
        "77.1.1" => "VL Endoscopic Image",
        "77.1.2" => "VL Microscopic Image",
        "77.1.4" => "VL Photographic Image",
        "77.1.6" => "VL Whole Slide Microscopy Image",
        "88.11" => "Basic Text SR",
        "88.22" => "Enhanced SR",
        "88.33" => "Comprehensive SR",
        "88.59" => "Key Object Selection Document",
        "88.67" => "X-Ray Radiation Dose SR",
        "104.1" => "Encapsulated PDF",
        "104.2" => "Encapsulated CDA",
        "128" => "Positron Emission Tomography Image",
        "130" => "Enhanced PET Image",
        "481.1" => "RT Image",
        "481.2" => "RT Dose",
        "481.3" => "RT Structure Set",
        "481.5" => "RT Plan",
        _ => return None,
    })
}

impl Handler for Dicom {
    fn name(&self) -> &'static str {
        "DICOM"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        probe.head.get(128..132) == Some(b"DICM") || bare_dataset(probe.head)
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let r = read(input.bytes, cancelled)?;
        let sop = r
            .str(0x0008_0016)
            .or_else(|| r.str(0x0002_0002))
            .unwrap_or_default();
        let kind = match sop_class_name(&sop) {
            Some(n) => format!("{n} Storage"),
            None if sop == "1.2.840.10008.1.3.10" => "Media Storage Directory (DICOMDIR)".into(),
            None => "DICOM data set".into(),
        };
        let mut i = Inspection::new("DICOM", &kind);
        let uid = r.syntax_uid.clone().unwrap_or_default();
        i.version = Some(match syntax_name(&uid) {
            Some(n) => format!("{n} ({uid})"),
            None if uid.is_empty() => format!("{:?} (no file meta information; guessed)", r.syntax)
                .replace("ImplicitLe", "Implicit VR Little Endian")
                .replace("ExplicitLe", "Explicit VR Little Endian")
                .replace("ExplicitBe", "Explicit VR Big Endian"),
            None => uid.clone(),
        });
        if !r.preamble {
            i.notes.push(
                "The file has no 128-byte preamble and DICM prefix (a bare data set).".into(),
            );
        }
        facts(&mut i, &r);
        // The element table.
        // Values that identify the patient go in a column of their own, which the Overview
        // masks with the patient facts.
        let mut t = sci_common::table(
            "Data elements",
            &["Tag", "Keyword", "VR", "Value", PERSONAL_COLUMN],
        );
        for e in &r.elems {
            let indent = "› ".repeat(e.depth as usize);
            let tag = if e.tag == ITEM {
                String::new()
            } else {
                tag_text(e.tag)
            };
            let vr = if e.tag == ITEM {
                String::new()
            } else {
                String::from_utf8_lossy(&e.vr).into_owned()
            };
            let source = e.in_file.then(|| e.start as u64..e.value.end as u64);
            let value = text(&r, e, 16);
            let (value, personal) = if identifying(e) {
                (String::new(), value)
            } else {
                (value, String::new())
            };
            sci_common::push(
                &mut t,
                MAX_TABLE,
                vec![
                    tag,
                    format!("{indent}{}", keyword(e.tag)),
                    vr,
                    value,
                    personal,
                ],
                source,
            );
        }
        let n = r.elems.len();
        match &r.problem {
            None => i.check(
                Stage::Parsed,
                State::Passed,
                format!("{n} data elements read"),
            ),
            Some(p) => i.check(
                Stage::Parsed,
                State::Partial,
                format!("{n} data elements read; {p}"),
            ),
        }
        // Business rules: the meta header agrees with the data set; pixel data has its size.
        for (meta, data, what) in [
            (0x0002_0002, 0x0008_0016, "SOP class"),
            (0x0002_0003, 0x0008_0018, "SOP instance"),
        ] {
            if let (Some(a), Some(b)) = (r.str(meta), r.str(data)) {
                if a == b {
                    i.check(
                        Stage::Semantic,
                        State::Passed,
                        format!("Media storage {what} UID matches the data set"),
                    );
                } else {
                    i.check(
                        Stage::Semantic,
                        State::Failed,
                        format!("Media storage {what} UID {a} differs from the data set's {b}"),
                    );
                }
            }
        }
        pixels(&mut i, &r);
        i.check(
            Stage::Schema,
            State::NotDone,
            "Not checked against the information object definition (required attributes)",
        );
        if r.elems.iter().any(|e| e.tag == 0xFFFA_FFFA) {
            i.check(
                Stage::Signature,
                State::NotDone,
                "The file carries digital signatures; they were not verified",
            );
        }
        i.tables.push(t);
        i.notes.push(
            "Images are shown for inspection only, not for diagnosis; the display is not calibrated."
                .into(),
        );
        Ok(i)
    }
}

fn facts(i: &mut Inspection, r: &Read) {
    let src = |tag: u32| {
        r.top(tag)
            .filter(|e| e.in_file)
            .map(|e| e.start as u64..e.value.end as u64)
    };
    type Fmt = fn(&str) -> String;
    let plain: Fmt = |s| s.to_owned();
    let list: &[(&str, &str, u32, Fmt)] = &[
        ("Patient", "Patient name", 0x0010_0010, person),
        ("Patient", "Patient ID", 0x0010_0020, plain),
        ("Patient", "Patient birth date", 0x0010_0030, date),
        ("Patient", "Patient sex", 0x0010_0040, plain),
        ("Patient", "Patient age", 0x0010_1010, plain),
        ("Patient", "Patient size (m)", 0x0010_1020, plain),
        ("Patient", "Patient weight (kg)", 0x0010_1030, plain),
        ("Patient", "Other patient IDs", 0x0010_1000, plain),
        ("Patient", "Patient comments", 0x0010_4000, plain),
        ("Study", "Study date", 0x0008_0020, date),
        ("Study", "Study time", 0x0008_0030, time),
        ("Study", "Study description", 0x0008_1030, plain),
        ("Study", "Study ID", 0x0020_0010, plain),
        ("Study", "Accession number", 0x0008_0050, plain),
        ("Study", "Referring physician name", 0x0008_0090, person),
        ("Study", "Study instance UID", 0x0020_000D, plain),
        ("Series", "Modality", 0x0008_0060, plain),
        ("Series", "Series description", 0x0008_103E, plain),
        ("Series", "Series number", 0x0020_0011, plain),
        ("Series", "Series date", 0x0008_0021, date),
        ("Series", "Body part examined", 0x0018_0015, plain),
        ("Series", "Protocol name", 0x0018_1030, plain),
        ("Series", "Patient position", 0x0018_5100, plain),
        ("Series", "Series instance UID", 0x0020_000E, plain),
        (
            "Frame of reference",
            "Frame of reference UID",
            0x0020_0052,
            plain,
        ),
        (
            "Frame of reference",
            "Position reference indicator",
            0x0020_1040,
            plain,
        ),
        ("Equipment", "Manufacturer", 0x0008_0070, plain),
        ("Equipment", "Model", 0x0008_1090, plain),
        ("Equipment", "Station name", 0x0008_1010, plain),
        ("Equipment", "Institution name", 0x0008_0080, plain),
        ("Equipment", "Department name", 0x0008_1040, plain),
        ("Equipment", "Device serial number", 0x0018_1000, plain),
        ("Equipment", "Software versions", 0x0018_1020, plain),
        ("Acquisition", "Acquisition date", 0x0008_0022, date),
        ("Acquisition", "Acquisition time", 0x0008_0032, time),
        ("Acquisition", "Slice thickness (mm)", 0x0018_0050, plain),
        ("Acquisition", "KVP", 0x0018_0060, plain),
        ("Acquisition", "Exposure time (ms)", 0x0018_1150, plain),
        ("Acquisition", "Tube current (mA)", 0x0018_1151, plain),
        ("Acquisition", "Repetition time (ms)", 0x0018_0080, plain),
        ("Acquisition", "Echo time (ms)", 0x0018_0081, plain),
        (
            "Acquisition",
            "Magnetic field strength (T)",
            0x0018_0087,
            plain,
        ),
        ("Acquisition", "Convolution kernel", 0x0018_1210, plain),
        ("Image", "SOP class UID", 0x0008_0016, plain),
        ("Image", "SOP instance UID", 0x0008_0018, plain),
        ("Image", "Image type", 0x0008_0008, plain),
        ("Image", "Instance number", 0x0020_0013, plain),
        ("Image", "Content date", 0x0008_0023, date),
        ("Image", "Rows", 0x0028_0010, plain),
        ("Image", "Columns", 0x0028_0011, plain),
        ("Image", "Number of frames", 0x0028_0008, plain),
        ("Image", "Samples per pixel", 0x0028_0002, plain),
        ("Image", "Photometric interpretation", 0x0028_0004, plain),
        ("Image", "Bits allocated", 0x0028_0100, plain),
        ("Image", "Bits stored", 0x0028_0101, plain),
        ("Image", "High bit", 0x0028_0102, plain),
        ("Image", "Pixel spacing (mm)", 0x0028_0030, plain),
        ("Image", "Image position (patient)", 0x0020_0032, plain),
        ("Image", "Image orientation (patient)", 0x0020_0037, plain),
        ("Image", "Slice location", 0x0020_1041, plain),
        ("Image", "Window centre", 0x0028_1050, plain),
        ("Image", "Window width", 0x0028_1051, plain),
        ("Image", "Rescale slope", 0x0028_1053, plain),
        ("Image", "Rescale intercept", 0x0028_1052, plain),
        ("Image", "Rescale type", 0x0028_1054, plain),
        ("Image", "VOI LUT function", 0x0028_1056, plain),
        ("Image", "Lossy compression", 0x0028_2110, plain),
        ("File", "Transfer syntax UID", 0x0002_0010, plain),
        ("File", "Implementation class UID", 0x0002_0012, plain),
        ("File", "Implementation version", 0x0002_0013, plain),
        ("File", "Source application entity", 0x0002_0016, plain),
        ("File", "Character set", 0x0008_0005, plain),
    ];
    for &(group, label, tag, f) in list {
        if let Some(v) = r.str(tag) {
            i.fact(group, label, f(&v), src(tag));
        }
    }
    if let Some(p) = r.uint(0x0028_0103) {
        let v = if p == 1 { "signed" } else { "unsigned" };
        i.fact("Image", "Pixel representation", v, src(0x0028_0103));
    }
    if let Some(n) = r.str(0x0008_0016).as_deref().and_then(sop_class_name) {
        i.fact("Image", "SOP class", n, src(0x0008_0016));
    }
    for (tag, label) in [
        (0x0028_3000u32, "Modality LUT sequence"),
        (0x0028_3010, "VOI LUT sequence"),
    ] {
        if let Some(e) = r.top(tag) {
            i.fact("Image", label, format!("{} item(s)", e.items), src(tag));
        }
    }
}

/// Uncompressed pixel data of the first frame as an image; encapsulated data explained.
fn pixels(i: &mut Inspection, r: &Read) {
    let Some(px) = r.top(PIXEL_DATA) else {
        return;
    };
    let uid = r.syntax_uid.clone().unwrap_or_default();
    if px.encapsulated {
        let name = syntax_name(&uid).unwrap_or("an unknown compressed transfer syntax");
        i.notes.push(format!(
            "The image is compressed ({name}) and is not decoded yet; the {} fragment(s) are listed.",
            px.items
        ));
        return;
    }
    let (Some(rows), Some(cols)) = (r.uint(0x0028_0010), r.uint(0x0028_0011)) else {
        i.notes
            .push("Pixel data without Rows and Columns cannot be shown.".into());
        return;
    };
    let bits = r.uint(0x0028_0100).unwrap_or(16);
    let stored = r.uint(0x0028_0101).unwrap_or(bits).clamp(1, bits.max(1));
    let spp = r.uint(0x0028_0002).unwrap_or(1);
    let signed = r.uint(0x0028_0103) == Some(1);
    let planar = r.uint(0x0028_0006).unwrap_or(0);
    let frames = r.uint(0x0028_0008).unwrap_or(1).max(1);
    let photometric = r
        .str(0x0028_0004)
        .unwrap_or_else(|| "MONOCHROME2".into())
        .to_uppercase();
    let slope = r.decimal(0x0028_1053).unwrap_or(1.0);
    let intercept = r.decimal(0x0028_1052).unwrap_or(0.0);
    let data = r.bytes(px);
    let (w, h) = (cols as usize, rows as usize);
    let Some(npx) = w.checked_mul(h).filter(|&n| n > 0) else {
        i.notes
            .push("The image has no pixels (Rows or Columns is 0).".into());
        return;
    };
    // Expected size of all frames.
    let frame_bits = (npx as u128) * spp as u128 * bits as u128;
    let expected = frame_bits * frames as u128 / 8;
    let actual = data.len() as u128;
    if expected == actual || expected + 1 == actual {
        i.check(
            Stage::Semantic,
            State::Passed,
            format!("Pixel data holds {frames} frame(s) of {cols} × {rows} as declared"),
        );
    } else {
        i.check(
            Stage::Semantic,
            State::Failed,
            format!(
                "Pixel data has {actual} bytes; {frames} frame(s) of {cols} × {rows} × {spp} × {bits} bits need {expected}"
            ),
        );
    }
    if npx > MAX_PIXELS {
        i.notes.push(format!(
            "The frame has {npx} pixels, more than Isolinear shows ({MAX_PIXELS})."
        ));
        return;
    }
    let caption = if frames > 1 {
        format!("Frame 1 of {frames} — {cols} × {rows}")
    } else {
        format!("{cols} × {rows}")
    };
    let be = px.be && &px.vr == b"OW";
    match (photometric.as_str(), spp) {
        ("MONOCHROME1" | "MONOCHROME2", 1) if matches!(bits, 8 | 16 | 32) => {
            let bytes = bits as usize / 8;
            if data.len() < npx * bytes {
                i.notes
                    .push("The pixel data is shorter than one frame; no image is shown.".into());
                return;
            }
            let mask: u64 = if stored >= 32 {
                u32::MAX as u64
            } else {
                (1u64 << stored) - 1
            };
            let mut c = Cur::new(data, be);
            let mut grey = Vec::with_capacity(npx);
            for _ in 0..npx {
                let raw = match bytes {
                    1 => c.u8().map(u64::from),
                    2 => c.u16().map(u64::from),
                    _ => c.u32().map(u64::from),
                }
                .unwrap_or(0)
                    & mask;
                let v = if signed && stored < 64 && raw >> (stored - 1) & 1 == 1 {
                    raw as i64 - (1i64 << stored)
                } else {
                    raw as i64
                };
                grey.push((v as f64 * slope + intercept) as f32);
            }
            if photometric == "MONOCHROME1" {
                let (lo, hi) = grey
                    .iter()
                    .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
                for v in &mut grey {
                    *v = lo + hi - *v;
                }
                i.notes.push(
                    "MONOCHROME1: low values are bright; the image is shown inverted so that bright means high."
                        .into(),
                );
            }
            i.visuals.push(sci_common::grey_image(w, h, grey, caption));
        }
        ("RGB" | "YBR_FULL", 3) if bits == 8 => {
            if data.len() < npx * 3 {
                i.notes
                    .push("The pixel data is shorter than one frame; no image is shown.".into());
                return;
            }
            let mut rgba = Vec::with_capacity(npx * 4);
            for p in 0..npx {
                let (a, b, c) = if planar == 1 {
                    (data[p], data[npx + p], data[2 * npx + p])
                } else {
                    (data[3 * p], data[3 * p + 1], data[3 * p + 2])
                };
                let (red, green, blue) = if photometric == "YBR_FULL" {
                    let (y, cb, cr) = (a as f32, b as f32 - 128.0, c as f32 - 128.0);
                    (
                        (y + 1.402 * cr).clamp(0.0, 255.0) as u8,
                        (y - 0.344_136 * cb - 0.714_136 * cr).clamp(0.0, 255.0) as u8,
                        (y + 1.772 * cb).clamp(0.0, 255.0) as u8,
                    )
                } else {
                    (a, b, c)
                };
                rgba.extend_from_slice(&[red, green, blue, 255]);
            }
            i.visuals.push(sci_common::rgba_image(w, h, rgba, caption));
        }
        _ => i.notes.push(format!(
            "Pixel data with photometric interpretation {photometric}, {spp} sample(s) per pixel and {bits} bits allocated is not shown yet."
        )),
    }
}

/// Tag, VR and keyword of common data elements (sorted by tag).
fn dict(tag: u32) -> Option<&'static (u32, &'static str, &'static str)> {
    // Repeating overlay groups 60xx share the 6000 entries.
    let key = if tag >> 24 == 0x60 && (tag >> 16).is_multiple_of(2) {
        0x6000_0000 | (tag & 0xFFFF)
    } else {
        tag
    };
    DICT.binary_search_by_key(&key, |d| d.0)
        .ok()
        .map(|i| &DICT[i])
}

#[rustfmt::skip]
static DICT: &[(u32, &str, &str)] = &[
    (0x0002_0000, "UL", "FileMetaInformationGroupLength"),
    (0x0002_0001, "OB", "FileMetaInformationVersion"),
    (0x0002_0002, "UI", "MediaStorageSOPClassUID"),
    (0x0002_0003, "UI", "MediaStorageSOPInstanceUID"),
    (0x0002_0010, "UI", "TransferSyntaxUID"),
    (0x0002_0012, "UI", "ImplementationClassUID"),
    (0x0002_0013, "SH", "ImplementationVersionName"),
    (0x0002_0016, "AE", "SourceApplicationEntityTitle"),
    (0x0002_0017, "AE", "SendingApplicationEntityTitle"),
    (0x0002_0018, "AE", "ReceivingApplicationEntityTitle"),
    (0x0002_0100, "UI", "PrivateInformationCreatorUID"),
    (0x0002_0102, "OB", "PrivateInformation"),
    (0x0004_1130, "CS", "FileSetID"),
    (0x0004_1200, "UL", "OffsetOfTheFirstDirectoryRecordOfTheRootDirectoryEntity"),
    (0x0004_1202, "UL", "OffsetOfTheLastDirectoryRecordOfTheRootDirectoryEntity"),
    (0x0004_1212, "US", "FileSetConsistencyFlag"),
    (0x0004_1220, "SQ", "DirectoryRecordSequence"),
    (0x0004_1400, "UL", "OffsetOfTheNextDirectoryRecord"),
    (0x0004_1410, "US", "RecordInUseFlag"),
    (0x0004_1420, "UL", "OffsetOfReferencedLowerLevelDirectoryEntity"),
    (0x0004_1430, "CS", "DirectoryRecordType"),
    (0x0004_1500, "CS", "ReferencedFileID"),
    (0x0004_1510, "UI", "ReferencedSOPClassUIDInFile"),
    (0x0004_1511, "UI", "ReferencedSOPInstanceUIDInFile"),
    (0x0004_1512, "UI", "ReferencedTransferSyntaxUIDInFile"),
    (0x0008_0005, "CS", "SpecificCharacterSet"),
    (0x0008_0006, "SQ", "LanguageCodeSequence"),
    (0x0008_0008, "CS", "ImageType"),
    (0x0008_0012, "DA", "InstanceCreationDate"),
    (0x0008_0013, "TM", "InstanceCreationTime"),
    (0x0008_0014, "UI", "InstanceCreatorUID"),
    (0x0008_0016, "UI", "SOPClassUID"),
    (0x0008_0018, "UI", "SOPInstanceUID"),
    (0x0008_001A, "UI", "RelatedGeneralSOPClassUID"),
    (0x0008_0020, "DA", "StudyDate"),
    (0x0008_0021, "DA", "SeriesDate"),
    (0x0008_0022, "DA", "AcquisitionDate"),
    (0x0008_0023, "DA", "ContentDate"),
    (0x0008_002A, "DT", "AcquisitionDateTime"),
    (0x0008_0030, "TM", "StudyTime"),
    (0x0008_0031, "TM", "SeriesTime"),
    (0x0008_0032, "TM", "AcquisitionTime"),
    (0x0008_0033, "TM", "ContentTime"),
    (0x0008_0050, "SH", "AccessionNumber"),
    (0x0008_0051, "SQ", "IssuerOfAccessionNumberSequence"),
    (0x0008_0052, "CS", "QueryRetrieveLevel"),
    (0x0008_0054, "AE", "RetrieveAETitle"),
    (0x0008_0056, "CS", "InstanceAvailability"),
    (0x0008_0058, "UI", "FailedSOPInstanceUIDList"),
    (0x0008_0060, "CS", "Modality"),
    (0x0008_0061, "CS", "ModalitiesInStudy"),
    (0x0008_0062, "UI", "SOPClassesInStudy"),
    (0x0008_0064, "CS", "ConversionType"),
    (0x0008_0068, "CS", "PresentationIntentType"),
    (0x0008_0070, "LO", "Manufacturer"),
    (0x0008_0080, "LO", "InstitutionName"),
    (0x0008_0081, "ST", "InstitutionAddress"),
    (0x0008_0082, "SQ", "InstitutionCodeSequence"),
    (0x0008_0090, "PN", "ReferringPhysicianName"),
    (0x0008_0092, "ST", "ReferringPhysicianAddress"),
    (0x0008_0094, "SH", "ReferringPhysicianTelephoneNumbers"),
    (0x0008_0096, "SQ", "ReferringPhysicianIdentificationSequence"),
    (0x0008_0100, "SH", "CodeValue"),
    (0x0008_0102, "SH", "CodingSchemeDesignator"),
    (0x0008_0103, "SH", "CodingSchemeVersion"),
    (0x0008_0104, "LO", "CodeMeaning"),
    (0x0008_0105, "CS", "MappingResource"),
    (0x0008_0201, "SH", "TimezoneOffsetFromUTC"),
    (0x0008_1010, "SH", "StationName"),
    (0x0008_1030, "LO", "StudyDescription"),
    (0x0008_1032, "SQ", "ProcedureCodeSequence"),
    (0x0008_103E, "LO", "SeriesDescription"),
    (0x0008_1040, "LO", "InstitutionalDepartmentName"),
    (0x0008_1048, "PN", "PhysiciansOfRecord"),
    (0x0008_1050, "PN", "PerformingPhysicianName"),
    (0x0008_1060, "PN", "NameOfPhysiciansReadingStudy"),
    (0x0008_1070, "PN", "OperatorsName"),
    (0x0008_1080, "LO", "AdmittingDiagnosesDescription"),
    (0x0008_1084, "SQ", "AdmittingDiagnosesCodeSequence"),
    (0x0008_1090, "LO", "ManufacturerModelName"),
    (0x0008_1110, "SQ", "ReferencedStudySequence"),
    (0x0008_1111, "SQ", "ReferencedPerformedProcedureStepSequence"),
    (0x0008_1115, "SQ", "ReferencedSeriesSequence"),
    (0x0008_1120, "SQ", "ReferencedPatientSequence"),
    (0x0008_1140, "SQ", "ReferencedImageSequence"),
    (0x0008_1150, "UI", "ReferencedSOPClassUID"),
    (0x0008_1155, "UI", "ReferencedSOPInstanceUID"),
    (0x0008_1160, "IS", "ReferencedFrameNumber"),
    (0x0008_1199, "SQ", "ReferencedSOPSequence"),
    (0x0008_2111, "ST", "DerivationDescription"),
    (0x0008_2112, "SQ", "SourceImageSequence"),
    (0x0008_2218, "SQ", "AnatomicRegionSequence"),
    (0x0008_3010, "UI", "IrradiationEventUID"),
    (0x0008_9007, "CS", "FrameType"),
    (0x0008_9092, "SQ", "ReferencedImageEvidenceSequence"),
    (0x0008_9205, "CS", "PixelPresentation"),
    (0x0008_9206, "CS", "VolumetricProperties"),
    (0x0008_9207, "CS", "VolumeBasedCalculationTechnique"),
    (0x0008_9215, "SQ", "DerivationCodeSequence"),
    (0x0010_0010, "PN", "PatientName"),
    (0x0010_0020, "LO", "PatientID"),
    (0x0010_0021, "LO", "IssuerOfPatientID"),
    (0x0010_0022, "CS", "TypeOfPatientID"),
    (0x0010_0030, "DA", "PatientBirthDate"),
    (0x0010_0032, "TM", "PatientBirthTime"),
    (0x0010_0040, "CS", "PatientSex"),
    (0x0010_0050, "SQ", "PatientInsurancePlanCodeSequence"),
    (0x0010_0200, "CS", "QualityControlSubject"),
    (0x0010_1000, "LO", "OtherPatientIDs"),
    (0x0010_1001, "PN", "OtherPatientNames"),
    (0x0010_1002, "SQ", "OtherPatientIDsSequence"),
    (0x0010_1005, "PN", "PatientBirthName"),
    (0x0010_1010, "AS", "PatientAge"),
    (0x0010_1020, "DS", "PatientSize"),
    (0x0010_1030, "DS", "PatientWeight"),
    (0x0010_1040, "LO", "PatientAddress"),
    (0x0010_1060, "PN", "PatientMotherBirthName"),
    (0x0010_1080, "LO", "MilitaryRank"),
    (0x0010_1090, "LO", "MedicalRecordLocator"),
    (0x0010_2000, "LO", "MedicalAlerts"),
    (0x0010_2110, "LO", "Allergies"),
    (0x0010_2154, "SH", "PatientTelephoneNumbers"),
    (0x0010_2160, "SH", "EthnicGroup"),
    (0x0010_2180, "SH", "Occupation"),
    (0x0010_21A0, "CS", "SmokingStatus"),
    (0x0010_21B0, "LT", "AdditionalPatientHistory"),
    (0x0010_21C0, "US", "PregnancyStatus"),
    (0x0010_2201, "LO", "PatientSpeciesDescription"),
    (0x0010_2203, "CS", "PatientSexNeutered"),
    (0x0010_2292, "LO", "PatientBreedDescription"),
    (0x0010_4000, "LT", "PatientComments"),
    (0x0018_0010, "LO", "ContrastBolusAgent"),
    (0x0018_0015, "CS", "BodyPartExamined"),
    (0x0018_0020, "CS", "ScanningSequence"),
    (0x0018_0021, "CS", "SequenceVariant"),
    (0x0018_0022, "CS", "ScanOptions"),
    (0x0018_0023, "CS", "MRAcquisitionType"),
    (0x0018_0024, "SH", "SequenceName"),
    (0x0018_0025, "CS", "AngioFlag"),
    (0x0018_0031, "LO", "Radiopharmaceutical"),
    (0x0018_0040, "IS", "CineRate"),
    (0x0018_0050, "DS", "SliceThickness"),
    (0x0018_0060, "DS", "KVP"),
    (0x0018_0070, "IS", "CountsAccumulated"),
    (0x0018_0071, "CS", "AcquisitionTerminationCondition"),
    (0x0018_0080, "DS", "RepetitionTime"),
    (0x0018_0081, "DS", "EchoTime"),
    (0x0018_0082, "DS", "InversionTime"),
    (0x0018_0083, "DS", "NumberOfAverages"),
    (0x0018_0084, "DS", "ImagingFrequency"),
    (0x0018_0085, "SH", "ImagedNucleus"),
    (0x0018_0086, "IS", "EchoNumbers"),
    (0x0018_0087, "DS", "MagneticFieldStrength"),
    (0x0018_0088, "DS", "SpacingBetweenSlices"),
    (0x0018_0089, "IS", "NumberOfPhaseEncodingSteps"),
    (0x0018_0090, "DS", "DataCollectionDiameter"),
    (0x0018_0091, "IS", "EchoTrainLength"),
    (0x0018_0093, "DS", "PercentSampling"),
    (0x0018_0094, "DS", "PercentPhaseFieldOfView"),
    (0x0018_0095, "DS", "PixelBandwidth"),
    (0x0018_1000, "LO", "DeviceSerialNumber"),
    (0x0018_1004, "LO", "PlateID"),
    (0x0018_1010, "LO", "SecondaryCaptureDeviceID"),
    (0x0018_1012, "DA", "DateOfSecondaryCapture"),
    (0x0018_1014, "TM", "TimeOfSecondaryCapture"),
    (0x0018_1016, "LO", "SecondaryCaptureDeviceManufacturer"),
    (0x0018_1018, "LO", "SecondaryCaptureDeviceManufacturerModelName"),
    (0x0018_1019, "LO", "SecondaryCaptureDeviceSoftwareVersions"),
    (0x0018_1020, "LO", "SoftwareVersions"),
    (0x0018_1030, "LO", "ProtocolName"),
    (0x0018_1040, "LO", "ContrastBolusRoute"),
    (0x0018_1041, "DS", "ContrastBolusVolume"),
    (0x0018_1044, "DS", "ContrastBolusTotalDose"),
    (0x0018_1046, "DS", "ContrastFlowRate"),
    (0x0018_1049, "DS", "ContrastBolusIngredientConcentration"),
    (0x0018_1050, "DS", "SpatialResolution"),
    (0x0018_1060, "DS", "TriggerTime"),
    (0x0018_1063, "DS", "FrameTime"),
    (0x0018_1072, "TM", "RadiopharmaceuticalStartTime"),
    (0x0018_1074, "DS", "RadionuclideTotalDose"),
    (0x0018_1075, "DS", "RadionuclideHalfLife"),
    (0x0018_1088, "IS", "HeartRate"),
    (0x0018_1100, "DS", "ReconstructionDiameter"),
    (0x0018_1110, "DS", "DistanceSourceToDetector"),
    (0x0018_1111, "DS", "DistanceSourceToPatient"),
    (0x0018_1114, "DS", "EstimatedRadiographicMagnificationFactor"),
    (0x0018_1120, "DS", "GantryDetectorTilt"),
    (0x0018_1130, "DS", "TableHeight"),
    (0x0018_1140, "CS", "RotationDirection"),
    (0x0018_1147, "CS", "FieldOfViewShape"),
    (0x0018_1149, "IS", "FieldOfViewDimensions"),
    (0x0018_1150, "IS", "ExposureTime"),
    (0x0018_1151, "IS", "XRayTubeCurrent"),
    (0x0018_1152, "IS", "Exposure"),
    (0x0018_1153, "IS", "ExposureInuAs"),
    (0x0018_1160, "SH", "FilterType"),
    (0x0018_1164, "DS", "ImagerPixelSpacing"),
    (0x0018_1166, "CS", "Grid"),
    (0x0018_1170, "IS", "GeneratorPower"),
    (0x0018_1180, "SH", "CollimatorGridName"),
    (0x0018_1181, "CS", "CollimatorType"),
    (0x0018_1190, "DS", "FocalSpots"),
    (0x0018_1200, "DA", "DateOfLastCalibration"),
    (0x0018_1201, "TM", "TimeOfLastCalibration"),
    (0x0018_1210, "SH", "ConvolutionKernel"),
    (0x0018_1242, "IS", "ActualFrameDuration"),
    (0x0018_1250, "SH", "ReceiveCoilName"),
    (0x0018_1251, "SH", "TransmitCoilName"),
    (0x0018_1310, "US", "AcquisitionMatrix"),
    (0x0018_1312, "CS", "InPlanePhaseEncodingDirection"),
    (0x0018_1314, "DS", "FlipAngle"),
    (0x0018_1316, "DS", "SAR"),
    (0x0018_1318, "DS", "dBdt"),
    (0x0018_1400, "LO", "AcquisitionDeviceProcessingDescription"),
    (0x0018_1401, "LO", "AcquisitionDeviceProcessingCode"),
    (0x0018_1405, "IS", "RelativeXRayExposure"),
    (0x0018_1411, "DS", "ExposureIndex"),
    (0x0018_1412, "DS", "TargetExposureIndex"),
    (0x0018_1413, "DS", "DeviationIndex"),
    (0x0018_1508, "CS", "PositionerType"),
    (0x0018_1510, "DS", "PositionerPrimaryAngle"),
    (0x0018_1511, "DS", "PositionerSecondaryAngle"),
    (0x0018_1800, "CS", "AcquisitionTimeSynchronized"),
    (0x0018_1801, "SH", "TimeSource"),
    (0x0018_1802, "CS", "TimeDistributionProtocol"),
    (0x0018_5020, "LO", "ProcessingFunction"),
    (0x0018_5100, "CS", "PatientPosition"),
    (0x0018_5101, "CS", "ViewPosition"),
    (0x0018_6000, "DS", "Sensitivity"),
    (0x0018_6011, "SQ", "SequenceOfUltrasoundRegions"),
    (0x0018_7004, "CS", "DetectorType"),
    (0x0018_700A, "SH", "DetectorID"),
    (0x0018_9004, "CS", "ContentQualification"),
    (0x0018_9073, "FD", "AcquisitionDuration"),
    (0x0018_9087, "FD", "DiffusionBValue"),
    (0x0018_9089, "FD", "DiffusionGradientOrientation"),
    (0x0018_9305, "FD", "RevolutionTime"),
    (0x0018_9306, "FD", "SingleCollimationWidth"),
    (0x0018_9307, "FD", "TotalCollimationWidth"),
    (0x0018_9309, "FD", "TableSpeed"),
    (0x0018_9310, "FD", "TableFeedPerRotation"),
    (0x0018_9311, "FD", "SpiralPitchFactor"),
    (0x0018_9345, "FD", "CTDIvol"),
    (0x0020_000D, "UI", "StudyInstanceUID"),
    (0x0020_000E, "UI", "SeriesInstanceUID"),
    (0x0020_0010, "SH", "StudyID"),
    (0x0020_0011, "IS", "SeriesNumber"),
    (0x0020_0012, "IS", "AcquisitionNumber"),
    (0x0020_0013, "IS", "InstanceNumber"),
    (0x0020_0019, "IS", "ItemNumber"),
    (0x0020_0020, "CS", "PatientOrientation"),
    (0x0020_0032, "DS", "ImagePositionPatient"),
    (0x0020_0037, "DS", "ImageOrientationPatient"),
    (0x0020_0052, "UI", "FrameOfReferenceUID"),
    (0x0020_0060, "CS", "Laterality"),
    (0x0020_0062, "CS", "ImageLaterality"),
    (0x0020_0100, "IS", "TemporalPositionIdentifier"),
    (0x0020_0105, "IS", "NumberOfTemporalPositions"),
    (0x0020_0110, "DS", "TemporalResolution"),
    (0x0020_0200, "UI", "SynchronizationFrameOfReferenceUID"),
    (0x0020_1002, "IS", "ImagesInAcquisition"),
    (0x0020_1040, "LO", "PositionReferenceIndicator"),
    (0x0020_1041, "DS", "SliceLocation"),
    (0x0020_1206, "IS", "NumberOfStudyRelatedSeries"),
    (0x0020_1208, "IS", "NumberOfStudyRelatedInstances"),
    (0x0020_1209, "IS", "NumberOfSeriesRelatedInstances"),
    (0x0020_4000, "LT", "ImageComments"),
    (0x0020_9056, "SH", "StackID"),
    (0x0020_9057, "UL", "InStackPositionNumber"),
    (0x0020_9128, "UL", "TemporalPositionIndex"),
    (0x0020_9157, "UL", "DimensionIndexValues"),
    (0x0020_9221, "SQ", "DimensionOrganizationSequence"),
    (0x0020_9222, "SQ", "DimensionIndexSequence"),
    (0x0028_0002, "US", "SamplesPerPixel"),
    (0x0028_0004, "CS", "PhotometricInterpretation"),
    (0x0028_0006, "US", "PlanarConfiguration"),
    (0x0028_0008, "IS", "NumberOfFrames"),
    (0x0028_0009, "AT", "FrameIncrementPointer"),
    (0x0028_0010, "US", "Rows"),
    (0x0028_0011, "US", "Columns"),
    (0x0028_0014, "US", "UltrasoundColorDataPresent"),
    (0x0028_0030, "DS", "PixelSpacing"),
    (0x0028_0034, "IS", "PixelAspectRatio"),
    (0x0028_0051, "CS", "CorrectedImage"),
    (0x0028_0100, "US", "BitsAllocated"),
    (0x0028_0101, "US", "BitsStored"),
    (0x0028_0102, "US", "HighBit"),
    (0x0028_0103, "US", "PixelRepresentation"),
    (0x0028_0106, "US", "SmallestImagePixelValue"),
    (0x0028_0107, "US", "LargestImagePixelValue"),
    (0x0028_0108, "US", "SmallestPixelValueInSeries"),
    (0x0028_0109, "US", "LargestPixelValueInSeries"),
    (0x0028_0120, "US", "PixelPaddingValue"),
    (0x0028_0121, "US", "PixelPaddingRangeLimit"),
    (0x0028_0300, "CS", "QualityControlImage"),
    (0x0028_0301, "CS", "BurnedInAnnotation"),
    (0x0028_0302, "CS", "RecognizableVisualFeatures"),
    (0x0028_0A02, "CS", "PixelSpacingCalibrationType"),
    (0x0028_0A04, "LO", "PixelSpacingCalibrationDescription"),
    (0x0028_1040, "CS", "PixelIntensityRelationship"),
    (0x0028_1041, "SS", "PixelIntensityRelationshipSign"),
    (0x0028_1050, "DS", "WindowCenter"),
    (0x0028_1051, "DS", "WindowWidth"),
    (0x0028_1052, "DS", "RescaleIntercept"),
    (0x0028_1053, "DS", "RescaleSlope"),
    (0x0028_1054, "LO", "RescaleType"),
    (0x0028_1055, "LO", "WindowCenterWidthExplanation"),
    (0x0028_1056, "CS", "VOILUTFunction"),
    (0x0028_1090, "CS", "RecommendedViewingMode"),
    (0x0028_1101, "US", "RedPaletteColorLookupTableDescriptor"),
    (0x0028_1102, "US", "GreenPaletteColorLookupTableDescriptor"),
    (0x0028_1103, "US", "BluePaletteColorLookupTableDescriptor"),
    (0x0028_1199, "UI", "PaletteColorLookupTableUID"),
    (0x0028_1201, "OW", "RedPaletteColorLookupTableData"),
    (0x0028_1202, "OW", "GreenPaletteColorLookupTableData"),
    (0x0028_1203, "OW", "BluePaletteColorLookupTableData"),
    (0x0028_2000, "OB", "ICCProfile"),
    (0x0028_2110, "CS", "LossyImageCompression"),
    (0x0028_2112, "DS", "LossyImageCompressionRatio"),
    (0x0028_2114, "CS", "LossyImageCompressionMethod"),
    (0x0028_3000, "SQ", "ModalityLUTSequence"),
    (0x0028_3002, "US", "LUTDescriptor"),
    (0x0028_3003, "LO", "LUTExplanation"),
    (0x0028_3004, "LO", "ModalityLUTType"),
    (0x0028_3006, "US", "LUTData"),
    (0x0028_3010, "SQ", "VOILUTSequence"),
    (0x0028_3110, "SQ", "SoftcopyVOILUTSequence"),
    (0x0028_9001, "UL", "DataPointRows"),
    (0x0028_9002, "UL", "DataPointColumns"),
    (0x0028_9110, "SQ", "PixelMeasuresSequence"),
    (0x0028_9132, "SQ", "FrameVOILUTSequence"),
    (0x0028_9145, "SQ", "PixelValueTransformationSequence"),
    (0x0032_000A, "CS", "StudyStatusID"),
    (0x0032_1032, "PN", "RequestingPhysician"),
    (0x0032_1033, "LO", "RequestingService"),
    (0x0032_1060, "LO", "RequestedProcedureDescription"),
    (0x0032_1064, "SQ", "RequestedProcedureCodeSequence"),
    (0x0032_4000, "LT", "StudyComments"),
    (0x0038_0010, "LO", "AdmissionID"),
    (0x0038_0300, "LO", "CurrentPatientLocation"),
    (0x0038_0500, "LO", "PatientState"),
    (0x0040_0002, "DA", "ScheduledProcedureStepStartDate"),
    (0x0040_0003, "TM", "ScheduledProcedureStepStartTime"),
    (0x0040_0006, "PN", "ScheduledPerformingPhysicianName"),
    (0x0040_0007, "LO", "ScheduledProcedureStepDescription"),
    (0x0040_0009, "SH", "ScheduledProcedureStepID"),
    (0x0040_0100, "SQ", "ScheduledProcedureStepSequence"),
    (0x0040_0241, "AE", "PerformedStationAETitle"),
    (0x0040_0244, "DA", "PerformedProcedureStepStartDate"),
    (0x0040_0245, "TM", "PerformedProcedureStepStartTime"),
    (0x0040_0250, "DA", "PerformedProcedureStepEndDate"),
    (0x0040_0251, "TM", "PerformedProcedureStepEndTime"),
    (0x0040_0253, "SH", "PerformedProcedureStepID"),
    (0x0040_0254, "LO", "PerformedProcedureStepDescription"),
    (0x0040_0260, "SQ", "PerformedProtocolCodeSequence"),
    (0x0040_0275, "SQ", "RequestAttributesSequence"),
    (0x0040_08EA, "SQ", "MeasurementUnitsCodeSequence"),
    (0x0040_1001, "SH", "RequestedProcedureID"),
    (0x0040_1002, "LO", "ReasonForTheRequestedProcedure"),
    (0x0040_2016, "LO", "PlacerOrderNumberImagingServiceRequest"),
    (0x0040_2017, "LO", "FillerOrderNumberImagingServiceRequest"),
    (0x0040_9096, "SQ", "RealWorldValueMappingSequence"),
    (0x0040_9210, "SH", "LUTLabel"),
    (0x0040_9211, "US", "RealWorldValueLastValueMapped"),
    (0x0040_9216, "US", "RealWorldValueFirstValueMapped"),
    (0x0040_9224, "FD", "RealWorldValueIntercept"),
    (0x0040_9225, "FD", "RealWorldValueSlope"),
    (0x0040_A010, "CS", "RelationshipType"),
    (0x0040_A040, "CS", "ValueType"),
    (0x0040_A043, "SQ", "ConceptNameCodeSequence"),
    (0x0040_A124, "UI", "UID"),
    (0x0040_A160, "UT", "TextValue"),
    (0x0040_A168, "SQ", "ConceptCodeSequence"),
    (0x0040_A491, "CS", "CompletionFlag"),
    (0x0040_A493, "CS", "VerificationFlag"),
    (0x0040_A730, "SQ", "ContentSequence"),
    (0x0054_0011, "US", "NumberOfEnergyWindows"),
    (0x0054_0016, "SQ", "RadiopharmaceuticalInformationSequence"),
    (0x0054_0081, "US", "NumberOfSlices"),
    (0x0054_1000, "CS", "SeriesType"),
    (0x0054_1001, "CS", "Units"),
    (0x0054_1002, "CS", "CountsSource"),
    (0x0054_1101, "LO", "AttenuationCorrectionMethod"),
    (0x0054_1102, "CS", "DecayCorrection"),
    (0x0054_1300, "DS", "FrameReferenceTime"),
    (0x0054_1321, "DS", "DecayFactor"),
    (0x0054_1330, "US", "ImageIndex"),
    (0x0088_0140, "UI", "StorageMediaFileSetUID"),
    (0x2050_0020, "CS", "PresentationLUTShape"),
    (0x6000_0010, "US", "OverlayRows"),
    (0x6000_0011, "US", "OverlayColumns"),
    (0x6000_0040, "CS", "OverlayType"),
    (0x6000_0050, "SS", "OverlayOrigin"),
    (0x6000_0100, "US", "OverlayBitsAllocated"),
    (0x6000_0102, "US", "OverlayBitPosition"),
    (0x6000_3000, "OW", "OverlayData"),
    (0x7FE0_0008, "OF", "FloatPixelData"),
    (0x7FE0_0009, "OD", "DoubleFloatPixelData"),
    (0x7FE0_0010, "OW", "PixelData"),
    (0xFFFA_FFFA, "SQ", "DigitalSignaturesSequence"),
    (0xFFFE_E000, "--", "Item"),
    (0xFFFE_E00D, "--", "ItemDelimitationItem"),
    (0xFFFE_E0DD, "--", "SequenceDelimitationItem"),
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dictionary_is_sorted_and_large() {
        assert!(DICT.len() >= 300, "{}", DICT.len());
        assert!(DICT.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(dict(0x6002_0010).unwrap().2, "OverlayRows");
        assert_eq!(date("20240115"), "2024-01-15");
        assert_eq!(time("101530.5"), "10:15:30.5");
        assert_eq!(person("Doe^Jane^^Dr"), "Doe Jane Dr");
    }
}
