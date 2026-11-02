//! MATLAB MAT-files: Level 5 (the 128-byte header, data elements, zlib-compressed variables),
//! Level 4 (headerless matrices) and version 7.3, which is HDF5 behind a 512-byte MATLAB
//! header and is read by the HDF5 view. Variables are listed with class, dimensions and their
//! first values; cells and structs are walked with bounded nesting. Nothing is evaluated.
use super::sci_common::{self, Cur, latin1};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State, Table};
use std::ops::Range;

pub struct Mat;

/// Variables listed (including cell and struct members).
const MAX_VARS: usize = 100_000;
/// Cell/struct nesting followed.
const MAX_DEPTH: usize = 8;
/// Members of one cell or struct array listed.
const MAX_MEMBERS: usize = 1000;
/// Total bytes expanded from compressed variables.
const MAX_INFLATED: usize = 256 * 1024 * 1024;
/// Values shown per variable.
const SHOWN: usize = 10;
/// Struct fields read, and bytes of character data decoded per variable.
const MAX_FIELDS: usize = 4096;
const CHAR_BYTES: usize = 64 * 1024;

const HDF5: &[u8] = b"\x89HDF\r\n\x1a\n";

const MI_INT8: u32 = 1;
const MI_UINT8: u32 = 2;
const MI_INT16: u32 = 3;
const MI_UINT16: u32 = 4;
const MI_INT32: u32 = 5;
const MI_UINT32: u32 = 6;
const MI_SINGLE: u32 = 7;
const MI_DOUBLE: u32 = 9;
const MI_INT64: u32 = 12;
const MI_UINT64: u32 = 13;
const MI_MATRIX: u32 = 14;
const MI_COMPRESSED: u32 = 15;
const MI_UTF8: u32 = 16;
const MI_UTF16: u32 = 17;
const MI_UTF32: u32 = 18;

fn class_name(c: u32) -> &'static str {
    match c {
        1 => "cell",
        2 => "struct",
        3 => "object",
        4 => "char",
        5 => "sparse",
        6 => "double",
        7 => "single",
        8 => "int8",
        9 => "uint8",
        10 => "int16",
        11 => "uint16",
        12 => "int32",
        13 => "uint32",
        14 => "int64",
        15 => "uint64",
        16 => "function handle",
        17 => "opaque (MATLAB object)",
        _ => "unknown",
    }
}

fn size_of(t: u32) -> usize {
    match t {
        MI_INT8 | MI_UINT8 | MI_UTF8 => 1,
        MI_INT16 | MI_UINT16 | MI_UTF16 => 2,
        MI_INT32 | MI_UINT32 | MI_SINGLE | MI_UTF32 => 4,
        MI_DOUBLE | MI_INT64 | MI_UINT64 => 8,
        _ => 0,
    }
}

/// Numbers of a data element (at most `max`) and how many it holds.
fn numbers(t: u32, data: &[u8], be: bool, max: usize) -> (Vec<f64>, usize) {
    let size = size_of(t);
    if size == 0 {
        return (Vec::new(), 0);
    }
    let n = data.len() / size;
    let mut c = Cur::new(data, be);
    let vals = (0..n.min(max))
        .map_while(|_| {
            Some(match t {
                MI_INT8 => c.u8()? as i8 as f64,
                MI_UINT8 | MI_UTF8 => c.u8()? as f64,
                MI_INT16 => c.i16()? as f64,
                MI_UINT16 | MI_UTF16 => c.u16()? as f64,
                MI_INT32 => c.i32()? as f64,
                MI_UINT32 | MI_UTF32 => c.u32()? as f64,
                MI_SINGLE => c.f32()? as f64,
                MI_DOUBLE => c.f64()?,
                MI_INT64 => c.i64()? as f64,
                _ => c.u64()? as f64,
            })
        })
        .collect();
    (vals, n)
}

/// One data element: type and data. Small elements keep their data in the tag.
fn element<'a>(c: &mut Cur<'a>) -> Option<(u32, &'a [u8])> {
    let start = c.at;
    let w = c.u32()?;
    if w >> 16 != 0 {
        let (t, n) = (w & 0xffff, (w >> 16) as usize);
        let d = c.take(4)?;
        return Some((t, d.get(..n.min(4))?));
    }
    let n = c.u32()? as usize;
    let d = c.take(n)?;
    if w != MI_COMPRESSED {
        let pad = (8 - (c.at - start) % 8) % 8;
        c.at = (c.at + pad).min(c.b.len());
    }
    Some((w, d))
}

struct Out<'t> {
    table: &'t mut Table,
    be: bool,
    inflated: usize,
    unsupported: Vec<String>,
    problem: Option<String>,
    cancelled: &'t dyn Fn() -> bool,
}

fn dims_text(d: &[i64]) -> String {
    d.iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("×")
}

fn show(v: f64) -> String {
    sci_common::num(v)
}

/// A miMATRIX element's contents: one row, then its members.
fn matrix(o: &mut Out, data: &[u8], name: Option<String>, depth: usize, src: Option<Range<u64>>) {
    if !sci_common::has_room(o.table, MAX_VARS) {
        sci_common::push(o.table, MAX_VARS, Vec::new(), None);
        return;
    }
    if data.is_empty() {
        // An empty placeholder (an empty cell member).
        sci_common::push(
            o.table,
            MAX_VARS,
            vec![
                name.unwrap_or_default(),
                "double".into(),
                "0×0".into(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ],
            src,
        );
        return;
    }
    let be = o.be;
    let mut c = Cur::new(data, be);
    let Some((_, flags)) = element(&mut c) else {
        o.problem
            .get_or_insert("A variable's array flags are cut".into());
        return;
    };
    let f = Cur::new(flags, be).u32().unwrap_or(0);
    let class = f & 0xff;
    let (complex, global, logical) = (f & 0x800 != 0, f & 0x400 != 0, f & 0x200 != 0);
    let dims: Vec<i64> = match element(&mut c) {
        Some((t, d)) => numbers(t, d, be, 64).0.iter().map(|&v| v as i64).collect(),
        None => {
            o.problem
                .get_or_insert("A variable's dimensions are cut".into());
            return;
        }
    };
    let own_name = element(&mut c).map(|(_, d)| latin1(d)).unwrap_or_default();
    let name = name.unwrap_or(own_name);
    if class == 16 || class == 17 {
        o.unsupported
            .push(format!("{name} ({})", class_name(class)));
        sci_common::push(
            o.table,
            MAX_VARS,
            vec![
                name,
                class_name(class).into(),
                dims_text(&dims),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                "not supported".into(),
            ],
            src,
        );
        return;
    }
    let count: u64 = dims
        .iter()
        .fold(1u64, |a, &d| a.saturating_mul(d.max(0) as u64));
    let mut attrs = Vec::new();
    if global {
        attrs.push("global");
    }
    if logical {
        attrs.push("logical");
    }
    let class_text = if logical {
        "logical"
    } else {
        class_name(class)
    };
    let mut fields_text = String::new();
    let mut values = String::new();
    let mut members: Vec<(String, &[u8])> = Vec::new();
    match class {
        1 => {
            for k in 0..count.min(MAX_MEMBERS as u64) {
                match element(&mut c) {
                    Some((MI_MATRIX, d)) => members.push((format!("{name}{{{}}}", k + 1), d)),
                    _ => break,
                }
            }
        }
        2 | 3 => {
            if class == 3 {
                let cls = element(&mut c).map(|(_, d)| latin1(d)).unwrap_or_default();
                o.unsupported
                    .push(format!("{name} (object of class {cls})"));
                values = format!("object of class {cls}; not supported");
            }
            let len = element(&mut c)
                .and_then(|(t, d)| numbers(t, d, be, 1).0.first().copied())
                .unwrap_or(0.0) as usize;
            let names: Vec<String> = match element(&mut c) {
                Some((_, d)) if len > 0 => d
                    .chunks(len)
                    .take(MAX_FIELDS)
                    .map(|n| latin1(n.split(|&b| b == 0).next().unwrap_or(&[])))
                    .collect(),
                _ => Vec::new(),
            };
            fields_text = sci_common::clip(&names.join(", "), 300);
            'all: for k in 0..count.min(MAX_MEMBERS as u64) {
                for field in &names {
                    match element(&mut c) {
                        Some((MI_MATRIX, d)) => {
                            let n = if count == 1 {
                                format!("{name}.{field}")
                            } else {
                                format!("{name}({}).{field}", k + 1)
                            };
                            members.push((n, d));
                        }
                        _ => break 'all,
                    }
                }
            }
        }
        4 => {
            if let Some((t, d)) = element(&mut c) {
                // Only the start of long text is shown.
                let d = &d[..d.len().min(CHAR_BYTES)];
                let chars: Vec<char> = match t {
                    MI_UTF8 => String::from_utf8_lossy(d).chars().collect(),
                    MI_UTF16 | MI_UINT16 => {
                        let units: Vec<u16> = numbers(MI_UINT16, d, be, usize::MAX)
                            .0
                            .iter()
                            .map(|&v| v as u16)
                            .collect();
                        char::decode_utf16(units)
                            .map(|r| r.unwrap_or('\u{fffd}'))
                            .collect()
                    }
                    _ => numbers(t, d, be, usize::MAX)
                        .0
                        .iter()
                        .map(|&v| char::from_u32(v as u32).unwrap_or('\u{fffd}'))
                        .collect(),
                };
                // Column-major: each row of a char matrix is one string.
                let rows = dims.first().copied().unwrap_or(1).max(1) as usize;
                let cols = chars.len() / rows;
                let lines: Vec<String> = (0..rows.min(SHOWN))
                    .map(|r| (0..cols).filter_map(|j| chars.get(r + j * rows)).collect())
                    .collect();
                values = sci_common::clip(&lines.join(" | "), 200);
            }
        }
        5 => {
            let _ir = element(&mut c);
            let _jc = element(&mut c);
            if let Some((t, d)) = element(&mut c) {
                let (v, n) = numbers(t, d, be, SHOWN);
                values = format!(
                    "{n} non-zero: {}",
                    v.iter().map(|&x| show(x)).collect::<Vec<_>>().join(", ")
                );
            }
        }
        6..=15 => {
            let real = element(&mut c);
            let imag = if complex { element(&mut c) } else { None };
            if let Some((t, d)) = real {
                let (re, n) = numbers(t, d, be, SHOWN);
                let im = imag.map(|(t, d)| numbers(t, d, be, SHOWN).0);
                let parts: Vec<String> = re
                    .iter()
                    .enumerate()
                    .map(|(k, &r)| match im.as_ref().and_then(|im| im.get(k)) {
                        Some(&i) => format!(
                            "{}{}{}i",
                            show(r),
                            if i < 0.0 { "-" } else { "+" },
                            show(i.abs())
                        ),
                        None => show(r),
                    })
                    .collect();
                values = parts.join(", ");
                if n > SHOWN {
                    values.push_str(", …");
                }
            }
        }
        _ => {
            o.unsupported.push(format!("{name} (class {class})"));
            values = "not supported".into();
        }
    }
    let indent = "› ".repeat(depth);
    sci_common::push(
        o.table,
        MAX_VARS,
        vec![
            format!("{indent}{name}"),
            class_text.into(),
            dims_text(&dims),
            if complex { "yes".into() } else { String::new() },
            if class == 5 {
                "yes".into()
            } else {
                String::new()
            },
            attrs.join(", "),
            fields_text,
            values,
        ],
        src,
    );
    if depth + 1 >= MAX_DEPTH && !members.is_empty() {
        o.problem.get_or_insert(format!(
            "Members nested deeper than {MAX_DEPTH} levels are not listed"
        ));
        return;
    }
    for (n, d) in members {
        if (o.cancelled)() {
            return;
        }
        matrix(o, d, Some(n), depth + 1, None);
    }
}

fn columns() -> Table {
    sci_common::table(
        "Variables",
        &[
            "Variable",
            "Class",
            "Dimensions",
            "Complex",
            "Sparse",
            "Attributes",
            "Fields",
            "First values",
        ],
    )
}

fn level5(b: &[u8], cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let be = match b.get(126..128) {
        Some(b"IM") => false,
        Some(b"MI") => true,
        _ => return Err("The MAT-file header has no byte-order mark".into()),
    };
    let mut i = Inspection::new("MATLAB", "MAT-file (Level 5) — MATLAB variables");
    let text = sci_common::field(&b[..116]);
    let version = Cur::at(b, 124, be).u16().unwrap_or(0);
    i.version = Some(format!("Level 5, version 0x{version:04X}"));
    describe_header(&mut i, &text);
    i.fact(
        "File",
        "Byte order",
        if be { "big-endian" } else { "little-endian" },
        Some(126..128),
    );
    let mut table = columns();
    let mut o = Out {
        table: &mut table,
        be,
        inflated: 0,
        unsupported: Vec::new(),
        problem: None,
        cancelled,
    };
    let mut c = Cur::at(b, 128, be);
    let (mut vars, mut compressed) = (0usize, 0usize);
    while c.left() >= 8 {
        if cancelled() {
            return Err("Cancelled".into());
        }
        let start = c.at;
        let Some((t, d)) = element(&mut c) else {
            o.problem
                .get_or_insert(format!("The data element at byte {start} is cut"));
            break;
        };
        let src = Some(start as u64..c.at as u64);
        match t {
            MI_MATRIX => {
                vars += 1;
                matrix(&mut o, d, None, 0, src);
            }
            MI_COMPRESSED => {
                compressed += 1;
                vars += 1;
                let room = MAX_INFLATED.saturating_sub(o.inflated);
                let z = sci_common::unzlib(d, room, cancelled);
                o.inflated += z.bytes.len();
                if let Some(e) = &z.error {
                    o.problem
                        .get_or_insert(format!("A compressed variable is damaged ({e})"));
                }
                if z.limited {
                    o.problem.get_or_insert(format!(
                        "Compressed variables were expanded up to {} MiB only",
                        MAX_INFLATED >> 20
                    ));
                }
                let mut inner = Cur::new(&z.bytes, be);
                while inner.left() >= 8 {
                    match element(&mut inner) {
                        Some((MI_MATRIX, m)) => matrix(&mut o, m, None, 0, src.clone()),
                        Some(_) => {}
                        None => break,
                    }
                }
            }
            _ => {}
        }
    }
    let Out {
        unsupported,
        problem,
        ..
    } = o;
    i.fact("File", "Variables", vars.to_string(), None);
    if compressed > 0 {
        i.fact("File", "Compressed variables", compressed.to_string(), None);
    }
    if !unsupported.is_empty() {
        i.fact(
            "File",
            "Unsupported values",
            sci_common::clip(&unsupported.join(", "), 500),
            None,
        );
        i.notes.push(
            "Objects, function handles and other opaque MATLAB values are listed by name but not read."
                .into(),
        );
    }
    match problem {
        None => i.check(
            Stage::Parsed,
            State::Passed,
            format!("{vars} variable(s) read"),
        ),
        Some(p) => i.check(
            Stage::Parsed,
            State::Partial,
            format!("{vars} variable(s) read; {p}"),
        ),
    }
    i.not_validated();
    i.tables.push(table);
    Ok(i)
}

fn describe_header(i: &mut Inspection, text: &str) {
    i.fact("File", "Header text", text, Some(0..116));
    if let Some(p) = text.split("Platform: ").nth(1) {
        i.fact(
            "File",
            "Platform",
            p.split(',').next().unwrap_or("").trim(),
            None,
        );
    }
    if let Some(p) = text.split("Created on: ").nth(1) {
        i.fact("File", "Created", p.trim(), None);
    }
}

/// A Level 4 matrix header: type (MOPT), rows, columns, imaginary flag, name length.
fn level4_header(b: &[u8], at: usize) -> Option<(bool, [i32; 5])> {
    for be in [false, true] {
        let mut c = Cur::at(b, at, be);
        let h = [c.i32()?, c.i32()?, c.i32()?, c.i32()?, c.i32()?];
        let (m, o, p, t) = (h[0] / 1000, h[0] / 100 % 10, h[0] / 10 % 10, h[0] % 10);
        let ok = (0..=4999).contains(&h[0])
            && m == i32::from(be)
            && o == 0
            && (0..=5).contains(&p)
            && (0..=2).contains(&t)
            && h[1] >= 0
            && h[2] >= 0
            && (0..=1).contains(&h[3])
            && (1..=4096).contains(&h[4]);
        if ok {
            return Some((be, h));
        }
    }
    None
}

fn level4_plausible(b: &[u8]) -> bool {
    let Some((_, h)) = level4_header(b, 0) else {
        return false;
    };
    let name = b.get(20..20 + h[4] as usize);
    name.is_some_and(|n| {
        n.last() == Some(&0)
            && n[..n.len() - 1]
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || *c == b'_')
            && n.len() > 1
    })
}

fn level4(b: &[u8], cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let mut i = Inspection::new("MATLAB", "MAT-file (Level 4) — MATLAB matrices");
    i.version = Some("Level 4".into());
    let mut table = columns();
    let mut at = 0usize;
    let mut problem = None;
    let mut n = 0usize;
    while at + 20 <= b.len() {
        if cancelled() {
            return Err("Cancelled".into());
        }
        let Some((be, h)) = level4_header(b, at) else {
            problem = Some(format!("No matrix header at byte {at}"));
            break;
        };
        let p = h[0] / 10 % 10;
        let t = h[0] % 10;
        let (mi, size) = match p {
            0 => (MI_DOUBLE, 8),
            1 => (MI_SINGLE, 4),
            2 => (MI_INT32, 4),
            3 => (MI_INT16, 2),
            4 => (MI_UINT16, 2),
            _ => (MI_UINT8, 1),
        };
        let name_at = at + 20;
        let Some(name) = b.get(name_at..name_at + h[4] as usize) else {
            problem = Some("A matrix name is cut".into());
            break;
        };
        let name = latin1(name.split(|&c| c == 0).next().unwrap_or(&[]));
        let count = (h[1] as u128) * (h[2] as u128);
        let part = count * size as u128;
        let data_at = name_at + h[4] as usize;
        let end = data_at as u128 + part * (1 + h[3] as u128);
        let real = b
            .get(data_at..(data_at as u128 + part).min(b.len() as u128) as usize)
            .unwrap_or(&[]);
        let (vals, _) = numbers(mi, real, be, if t == 1 { 200 } else { SHOWN });
        let values = if t == 1 {
            vals.iter()
                .map(|&v| char::from_u32(v as u32).unwrap_or('\u{fffd}'))
                .collect()
        } else {
            let mut s = vals.iter().map(|&v| show(v)).collect::<Vec<_>>().join(", ");
            if count > SHOWN as u128 {
                s.push_str(", …");
            }
            s
        };
        let class = match (t, mi) {
            (1, _) => "char",
            (2, _) => "sparse",
            (_, MI_DOUBLE) => "double",
            (_, MI_SINGLE) => "single",
            (_, MI_INT32) => "int32",
            (_, MI_INT16) => "int16",
            (_, MI_UINT16) => "uint16",
            _ => "uint8",
        };
        sci_common::push(
            &mut table,
            MAX_VARS,
            vec![
                name,
                class.into(),
                format!("{}×{}", h[1], h[2]),
                if h[3] == 1 {
                    "yes".into()
                } else {
                    String::new()
                },
                if t == 2 { "yes".into() } else { String::new() },
                String::new(),
                String::new(),
                values,
            ],
            Some(at as u64..end.min(b.len() as u128) as u64),
        );
        n += 1;
        if end > b.len() as u128 {
            problem = Some(format!("The data of matrix {n} is cut"));
            break;
        }
        at = end as usize;
    }
    if n == 0 {
        return Err(problem.unwrap_or_else(|| "No Level 4 matrices".into()));
    }
    i.fact("File", "Variables", n.to_string(), None);
    match problem {
        None => i.check(Stage::Parsed, State::Passed, format!("{n} matrices read")),
        Some(p) => i.check(
            Stage::Parsed,
            State::Partial,
            format!("{n} matrices read; {p}"),
        ),
    }
    i.not_validated();
    i.tables.push(table);
    Ok(i)
}

fn v73(b: &[u8]) -> Inspection {
    let mut i = Inspection::new("MATLAB", "MAT-file version 7.3 (HDF5)");
    i.version = Some("7.3 (HDF5)".into());
    describe_header(&mut i, &sci_common::field(&b[..116.min(b.len())]));
    i.fact("File", "HDF5 data starts at byte", "512", Some(512..520));
    i.check(
        Stage::Parsed,
        State::Partial,
        "Only the 512-byte MATLAB header was read here; the variables are HDF5 datasets",
    );
    i.notes.push(
        "This MAT-file (version 7.3) is an HDF5 file behind a 512-byte MATLAB header: its variables open in the HDF5 view (tree and tables), not here."
            .into(),
    );
    i.not_validated();
    i
}

fn is_v73(h: &[u8]) -> bool {
    h.starts_with(b"MATLAB") && h.get(512..520) == Some(HDF5)
}
fn is_level5(h: &[u8]) -> bool {
    h.len() >= 128
        && matches!(&h[126..128], b"IM" | b"MI")
        && (h.starts_with(b"MATLAB 5.0 MAT-file") || h.starts_with(b"MATLAB"))
}

impl Handler for Mat {
    fn name(&self) -> &'static str {
        "MATLAB MAT-file"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let h = probe.head;
        is_v73(h) || is_level5(h) || (probe.ext_is(&["mat"]) && level4_plausible(h))
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        if is_v73(b) {
            Ok(v73(b))
        } else if is_level5(b) {
            level5(b, cancelled)
        } else {
            level4(b, cancelled)
        }
    }
}
