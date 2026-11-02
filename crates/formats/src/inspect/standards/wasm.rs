//! WebAssembly binary modules (`\0asm`): sections, type signatures, imports, exports,
//! functions with their names (name section), memories, tables, data and element segment
//! counts, and producer information. The module is only read, never compiled or run.
use super::util::{bytes_text, clip, printable, span};
use crate::inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Row, Stage, State, Table};

pub struct Wasm;

/// A bounded reader over `b[pos..end]`.
struct R<'a> {
    b: &'a [u8],
    pos: usize,
    end: usize,
}
impl<'a> R<'a> {
    fn byte(&mut self) -> Option<u8> {
        if self.pos >= self.end {
            return None;
        }
        let v = *self.b.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }
    fn leb(&mut self, max_bytes: u32) -> Option<u64> {
        let mut v = 0u64;
        for i in 0..max_bytes {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    fn u32(&mut self) -> Option<u32> {
        self.leb(5).and_then(|v| u32::try_from(v).ok())
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let stop = self.pos.checked_add(n)?;
        if stop > self.end {
            return None;
        }
        let s = self.b.get(self.pos..stop)?;
        self.pos = stop;
        Some(s)
    }
    fn name(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        Some(printable(&String::from_utf8_lossy(self.bytes(n)?)))
    }
    /// A vector length, refused when it cannot fit in what is left.
    fn count(&mut self) -> Option<u32> {
        let n = self.u32()?;
        (n as usize <= self.end.saturating_sub(self.pos)).then_some(n)
    }
    fn valtype(&mut self) -> Option<String> {
        let t = self.byte()?;
        Some(match t {
            0x7f => "i32".into(),
            0x7e => "i64".into(),
            0x7d => "f32".into(),
            0x7c => "f64".into(),
            0x7b => "v128".into(),
            0x70 => "funcref".into(),
            0x6f => "externref".into(),
            0x6e => "anyref".into(),
            0x6d => "eqref".into(),
            0x6c => "i31ref".into(),
            0x6b => "structref".into(),
            0x6a => "arrayref".into(),
            0x71 => "nullref".into(),
            0x72 => "nullexternref".into(),
            0x73 => "nullfuncref".into(),
            0x69 => "exnref".into(),
            0x63 | 0x64 => {
                let heap = self.leb(5)?;
                format!("(ref{} {heap})", if t == 0x63 { " null" } else { "" })
            }
            _ => return None,
        })
    }
    fn limits(&mut self) -> Option<String> {
        let flags = self.byte()?;
        let wide = if flags & 4 != 0 { 10 } else { 5 };
        let min = self.leb(wide)?;
        let max = if flags & 1 != 0 {
            Some(self.leb(wide)?)
        } else {
            None
        };
        let mut s = match max {
            Some(m) => format!("min {min}, max {m}"),
            None => format!("min {min}, no max"),
        };
        if flags & 2 != 0 {
            s.push_str(", shared");
        }
        if flags & 4 != 0 {
            s.push_str(", 64-bit");
        }
        Some(s)
    }
}

fn section_name(id: u8) -> &'static str {
    match id {
        0 => "custom",
        1 => "type",
        2 => "import",
        3 => "function",
        4 => "table",
        5 => "memory",
        6 => "global",
        7 => "export",
        8 => "start",
        9 => "element",
        10 => "code",
        11 => "data",
        12 => "data count",
        13 => "tag",
        _ => "unknown",
    }
}

fn kind_name(k: u8) -> &'static str {
    match k {
        0 => "function",
        1 => "table",
        2 => "memory",
        3 => "global",
        4 => "tag",
        _ => "unknown",
    }
}

fn table(name: &str, cols: &[&str]) -> Table {
    Table {
        name: name.into(),
        columns: cols.iter().map(|c| (*c).to_owned()).collect(),
        ..Table::default()
    }
}

fn push(t: &mut Table, cells: Vec<String>, source: Option<std::ops::Range<u64>>) {
    if t.rows.len() < MAX_ROWS {
        t.rows.push(Row { cells, source });
    } else {
        t.total = Some(t.total.unwrap_or(t.rows.len() as u64) + 1);
    }
}

impl Handler for Wasm {
    fn name(&self) -> &'static str {
        "WebAssembly"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        probe.head.starts_with(b"\0asm") && probe.head.len() >= 8
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        if !b.starts_with(b"\0asm") || b.len() < 8 {
            return Err("Not a WebAssembly module".into());
        }
        let version = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        let mut out = Inspection::new(
            "WebAssembly",
            if version == 1 {
                "WebAssembly module"
            } else {
                "WebAssembly binary (unknown version)"
            },
        );
        out.version = Some(version.to_string());
        out.fact("Module", "Binary version", version.to_string(), span(4, 4));
        let mut sections = table("Sections", &["#", "Section", "Offset", "Size", "Name"]);
        let mut types: Vec<String> = vec![];
        let mut types_t = table("Types", &["Index", "Signature"]);
        let mut imports = table("Imports", &["Module", "Name", "Kind", "Type"]);
        let mut exports = table("Exports", &["Name", "Kind", "Index"]);
        let mut mems = table(
            "Memories and tables",
            &["Kind", "Index", "Limits", "Element type"],
        );
        let mut func_types: Vec<u32> = vec![];
        let mut bodies: Vec<(usize, usize)> = vec![];
        let mut names: std::collections::HashMap<u32, String> = Default::default();
        let mut exported: std::collections::HashMap<u32, String> = Default::default();
        let mut imported_funcs = 0u32;
        let mut imported_kinds = [0u32; 5];
        let mut damaged: Option<String> = None;
        let mut pos = 8usize;
        let mut n = 0usize;
        while pos < b.len() {
            if n.is_multiple_of(1024) && cancelled() {
                return Err("Cancelled".into());
            }
            let mut r = R {
                b,
                pos,
                end: b.len(),
            };
            let (Some(id), Some(size)) = (r.byte(), r.u32()) else {
                damaged = Some(format!("section header at byte {pos} is cut"));
                break;
            };
            let body = r.pos;
            let Some(end) = body.checked_add(size as usize).filter(|&e| e <= b.len()) else {
                damaged = Some(format!(
                    "the {} section at byte {pos} claims {size} bytes, past the end of the file",
                    section_name(id)
                ));
                break;
            };
            n += 1;
            let mut s = R { b, pos: body, end };
            let mut custom = String::new();
            let ok = match id {
                0 => {
                    custom = s.name().unwrap_or_default();
                    custom_section(&custom, &mut s, &mut names, &mut out);
                    Some(())
                }
                1 => (|| {
                    for i in 0..s.count()? {
                        let form = s.byte()?;
                        if form != 0x60 {
                            out.notes.push(
                                "Types of the garbage-collection proposal are not decoded".into(),
                            );
                            return Some(());
                        }
                        let p: Option<Vec<String>> = (0..s.count()?).map(|_| s.valtype()).collect();
                        let q: Option<Vec<String>> = (0..s.count()?).map(|_| s.valtype()).collect();
                        let sig = format!("({}) -> ({})", p?.join(", "), q?.join(", "));
                        push(&mut types_t, vec![i.to_string(), sig.clone()], None);
                        types.push(sig);
                    }
                    Some(())
                })(),
                2 => (|| {
                    for _ in 0..s.count()? {
                        let at = s.pos;
                        let module = s.name()?;
                        let field = s.name()?;
                        let kind = s.byte()?;
                        let desc = match kind {
                            0 => {
                                let t = s.u32()?;
                                imported_funcs += 1;
                                types
                                    .get(t as usize)
                                    .cloned()
                                    .unwrap_or_else(|| format!("type {t}"))
                            }
                            1 => {
                                let et = s.valtype()?;
                                let l = s.limits()?;
                                format!("{et}, {l}")
                            }
                            2 => s.limits()?,
                            3 => {
                                let t = s.valtype()?;
                                let m = s.byte()?;
                                format!("{}{t}", if m == 1 { "mut " } else { "" })
                            }
                            4 => {
                                s.byte()?;
                                format!("type {}", s.u32()?)
                            }
                            _ => return None,
                        };
                        if let Some(c) = imported_kinds.get_mut(kind as usize) {
                            *c += 1;
                        }
                        push(
                            &mut imports,
                            vec![module, field, kind_name(kind).into(), desc],
                            span(at, s.pos - at),
                        );
                    }
                    Some(())
                })(),
                3 => (|| {
                    for _ in 0..s.count()? {
                        func_types.push(s.u32()?);
                    }
                    Some(())
                })(),
                4 => (|| {
                    for i in 0..s.count()? {
                        let at = s.pos;
                        let et = s.valtype()?;
                        let l = s.limits()?;
                        push(
                            &mut mems,
                            vec!["table".into(), i.to_string(), l, et],
                            span(at, s.pos - at),
                        );
                    }
                    Some(())
                })(),
                5 => (|| {
                    for i in 0..s.count()? {
                        let at = s.pos;
                        let l = s.limits()?;
                        push(
                            &mut mems,
                            vec![
                                "memory".into(),
                                i.to_string(),
                                format!("{l} (pages of 64 KiB)"),
                                String::new(),
                            ],
                            span(at, s.pos - at),
                        );
                    }
                    Some(())
                })(),
                6 => s.count().map(|c| {
                    out.fact(
                        "Module",
                        "Globals",
                        c.to_string(),
                        span(body, size as usize),
                    )
                }),
                7 => (|| {
                    for _ in 0..s.count()? {
                        let at = s.pos;
                        let name = s.name()?;
                        let kind = s.byte()?;
                        let index = s.u32()?;
                        if kind == 0 {
                            exported.insert(index, name.clone());
                        }
                        push(
                            &mut exports,
                            vec![name, kind_name(kind).into(), index.to_string()],
                            span(at, s.pos - at),
                        );
                    }
                    Some(())
                })(),
                8 => s.u32().map(|f| {
                    out.fact(
                        "Module",
                        "Start function",
                        f.to_string(),
                        span(body, size as usize),
                    )
                }),
                9 => s.count().map(|c| {
                    out.fact(
                        "Module",
                        "Element segments",
                        c.to_string(),
                        span(body, size as usize),
                    )
                }),
                10 => (|| {
                    for _ in 0..s.count()? {
                        let len = s.u32()? as usize;
                        let at = s.pos;
                        s.bytes(len)?;
                        bodies.push((at, len));
                    }
                    Some(())
                })(),
                11 => s.count().map(|c| {
                    out.fact(
                        "Module",
                        "Data segments",
                        c.to_string(),
                        span(body, size as usize),
                    )
                }),
                12 => s.u32().map(|c| {
                    out.fact(
                        "Module",
                        "Data count (declared)",
                        c.to_string(),
                        span(body, size as usize),
                    )
                }),
                13 => s
                    .count()
                    .map(|c| out.fact("Module", "Tags", c.to_string(), span(body, size as usize))),
                _ => Some(()),
            };
            if ok.is_none() && damaged.is_none() {
                damaged = Some(format!(
                    "the {} section at byte {pos} could not be read",
                    section_name(id)
                ));
            }
            push(
                &mut sections,
                vec![
                    n.to_string(),
                    format!("{} ({id})", section_name(id)),
                    pos.to_string(),
                    bytes_text(size as u64),
                    custom,
                ],
                span(pos, end - pos),
            );
            pos = end;
        }
        let mut funcs = table(
            "Functions",
            &["Index", "Name", "Signature", "Body size", "Exported as"],
        );
        for (i, t) in func_types.iter().enumerate() {
            let index = imported_funcs.saturating_add(i as u32);
            let body = bodies.get(i);
            push(
                &mut funcs,
                vec![
                    index.to_string(),
                    names.get(&index).cloned().unwrap_or_default(),
                    types
                        .get(*t as usize)
                        .cloned()
                        .unwrap_or_else(|| format!("type {t}")),
                    body.map(|b| bytes_text(b.1 as u64)).unwrap_or_default(),
                    exported.get(&index).cloned().unwrap_or_default(),
                ],
                body.and_then(|b| span(b.0, b.1)),
            );
        }
        out.fact("Module", "Sections", n.to_string(), None);
        out.fact("Module", "Types", types.len().to_string(), None);
        out.fact(
            "Module",
            "Imports",
            format!(
                "{} ({} functions, {} tables, {} memories, {} globals)",
                imports.rows.len(),
                imported_kinds[0],
                imported_kinds[1],
                imported_kinds[2],
                imported_kinds[3]
            ),
            None,
        );
        out.fact(
            "Module",
            "Functions defined",
            func_types.len().to_string(),
            None,
        );
        out.fact("Module", "Exports", exports.rows.len().to_string(), None);
        let memories = mems.rows.iter().filter(|r| r.cells[0] == "memory").count();
        out.fact("Module", "Memories", memories.to_string(), None);
        out.fact(
            "Module",
            "Tables",
            (mems.rows.len() - memories).to_string(),
            None,
        );
        if func_types.len() != bodies.len() && damaged.is_none() {
            out.check(
                Stage::Semantic,
                State::Failed,
                format!(
                    "The function section declares {} functions but the code section has {} bodies",
                    func_types.len(),
                    bodies.len()
                ),
            );
        } else if damaged.is_none() {
            out.check(
                Stage::Semantic,
                State::Passed,
                "Function declarations and code bodies agree in number",
            );
        }
        for t in [sections, types_t, imports, exports, funcs, mems] {
            if !t.rows.is_empty() {
                out.tables.push(t);
            }
        }
        match damaged {
            None => out.check(
                Stage::Parsed,
                State::Passed,
                "Every section was read (code is not disassembled)",
            ),
            Some(e) => out.check(Stage::Parsed, State::Partial, format!("Damaged: {e}")),
        }
        out.not_validated();
        out.notes.push(
            "The module is never compiled or run; function bodies are not disassembled.".into(),
        );
        Ok(out)
    }
}

fn custom_section(
    name: &str,
    s: &mut R,
    names: &mut std::collections::HashMap<u32, String>,
    out: &mut Inspection,
) {
    match name {
        "name" => {
            while s.pos < s.end {
                let (Some(id), Some(size)) = (s.byte(), s.u32()) else {
                    return;
                };
                let Some(end) = s.pos.checked_add(size as usize).filter(|&e| e <= s.end) else {
                    return;
                };
                let mut sub = R {
                    b: s.b,
                    pos: s.pos,
                    end,
                };
                match id {
                    0 => {
                        if let Some(m) = sub.name() {
                            out.fact("Module", "Module name", m, None);
                        }
                    }
                    1 => {
                        if let Some(c) = sub.count() {
                            for _ in 0..c {
                                let (Some(i), Some(n)) = (sub.u32(), sub.name()) else {
                                    break;
                                };
                                if names.len() < MAX_ROWS {
                                    names.insert(i, clip(&n, 500));
                                }
                            }
                        }
                    }
                    _ => {}
                }
                s.pos = end;
            }
        }
        "producers" => {
            let Some(fields) = s.count() else {
                return;
            };
            for _ in 0..fields.min(32) {
                let Some(field) = s.name() else {
                    return;
                };
                let Some(c) = s.count() else {
                    return;
                };
                let mut values = vec![];
                for _ in 0..c.min(64) {
                    let (Some(n), Some(v)) = (s.name(), s.name()) else {
                        return;
                    };
                    values.push(format!("{n} {v}").trim().to_owned());
                }
                out.fact("Producers", &field, values.join(", "), None);
            }
        }
        "sourceMappingURL" => {
            if let Some(u) = s.name() {
                out.fact("Module", "Source map URL (not fetched)", u, None);
            }
        }
        "target_features" => {
            let Some(c) = s.count() else {
                return;
            };
            let mut f = vec![];
            for _ in 0..c.min(128) {
                let (Some(prefix), Some(n)) = (s.byte(), s.name()) else {
                    break;
                };
                f.push(format!("{}{n}", prefix as char));
            }
            out.fact("Module", "Target features", f.join(", "), None);
        }
        _ => {}
    }
}
