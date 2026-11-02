//! Windows PE executables and libraries (`.exe`, `.dll`, `.sys`): DOS and PE headers, the
//! optional header, sections, imports, exports, a resource summary with simple version
//! strings, the .NET (CLR) header, and the Authenticode certificate table (its signers listed
//! through the CMS reader; the signature is not verified). Never loaded or run.
use super::util::{Bytes, bytes_text, clip, flags, printable, span, us};
use super::{cms, der};
use crate::inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Row, Stage, State, Table};

pub struct Pe;

const MAX_SECTIONS: usize = 96;
const MAX_IMPORTS: usize = 100_000;

/// Offset of the "PE\0\0" signature when `h` starts a PE file.
pub fn pe_offset(h: &[u8]) -> Option<usize> {
    let b = Bytes::new(h, true);
    if !h.starts_with(b"MZ") {
        return None;
    }
    let at = b.u32(0x3c)? as usize;
    (b.get(at, 4)? == b"PE\0\0").then_some(at)
}

fn machine(v: u16) -> &'static str {
    match v {
        0x14c => "x86",
        0x8664 => "x86-64",
        0x1c0 | 0x1c4 => "ARM",
        0xaa64 => "ARM64",
        0xa641 => "ARM64EC",
        0x200 => "IA-64",
        0x5064 => "RISC-V 64",
        0xebc => "EFI byte code",
        0 => "any",
        _ => "other",
    }
}

fn subsystem(v: u16) -> &'static str {
    match v {
        1 => "native (driver)",
        2 => "Windows GUI",
        3 => "Windows console",
        5 => "OS/2 console",
        7 => "POSIX console",
        9 => "Windows CE GUI",
        10 => "EFI application",
        11 => "EFI boot service driver",
        12 => "EFI runtime driver",
        13 => "EFI ROM",
        14 => "Xbox",
        16 => "Windows boot application",
        _ => "unknown",
    }
}

const CHARACTERISTICS: &[(u64, &str)] = &[
    (0x0001, "relocations stripped"),
    (0x0002, "executable"),
    (0x0020, "large address aware"),
    (0x0100, "32-bit machine"),
    (0x0200, "debug stripped"),
    (0x1000, "system file"),
    (0x2000, "DLL"),
];
const DLL_CHARACTERISTICS: &[(u64, &str)] = &[
    (0x0020, "high-entropy ASLR"),
    (0x0040, "ASLR (dynamic base)"),
    (0x0080, "force integrity"),
    (0x0100, "DEP (NX compatible)"),
    (0x0200, "no isolation"),
    (0x0400, "no SEH"),
    (0x0800, "no bind"),
    (0x1000, "AppContainer"),
    (0x2000, "WDM driver"),
    (0x4000, "Control Flow Guard"),
    (0x8000, "terminal server aware"),
];
const SECTION_FLAGS: &[(u64, &str)] = &[
    (0x20, "code"),
    (0x40, "initialized data"),
    (0x80, "uninitialized data"),
    (0x0200_0000, "discardable"),
    (0x1000_0000, "shared"),
    (0x2000_0000, "execute"),
    (0x4000_0000, "read"),
    (0x8000_0000, "write"),
];

fn resource_type(id: u32) -> String {
    match id {
        1 => "cursor",
        2 => "bitmap",
        3 => "icon",
        4 => "menu",
        5 => "dialog",
        6 => "string table",
        7 => "font directory",
        8 => "font",
        9 => "accelerators",
        10 => "raw data",
        11 => "message table",
        12 => "cursor group",
        14 => "icon group",
        16 => "version",
        17 => "dialog include",
        19 => "plug and play",
        20 => "VxD",
        21 => "animated cursor",
        22 => "animated icon",
        23 => "HTML",
        24 => "manifest",
        _ => return format!("type {id}"),
    }
    .into()
}

struct Sec {
    va: u32,
    vsize: u32,
    raw_off: u32,
    raw_size: u32,
}

struct Image<'a> {
    b: Bytes<'a>,
    secs: Vec<Sec>,
}
impl Image<'_> {
    /// File offset of a relative virtual address.
    fn off(&self, rva: u32) -> Option<usize> {
        for s in &self.secs {
            let size = s.vsize.max(s.raw_size);
            if rva >= s.va && rva - s.va < size {
                let delta = rva - s.va;
                if delta >= s.raw_size {
                    return None;
                }
                return us(u64::from(s.raw_off) + u64::from(delta));
            }
        }
        None
    }
    fn cstr(&self, rva: u32, max: usize) -> Option<String> {
        self.off(rva)
            .and_then(|o| self.b.cstr(o, max))
            .map(|s| printable(&s))
    }
}

fn table(name: &str, cols: &[&str]) -> Table {
    Table {
        name: name.into(),
        columns: cols.iter().map(|c| (*c).to_owned()).collect(),
        ..Table::default()
    }
}

impl Handler for Pe {
    fn name(&self) -> &'static str {
        "PE"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        pe_offset(probe.head).is_some()
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let raw = input.bytes;
        let pe = pe_offset(raw).ok_or("Not a PE file")?;
        let b = Bytes::new(raw, true);
        let cut = "The PE header is cut";
        let mach = b.u16(pe + 4).ok_or(cut)?;
        let nsec = b.u16(pe + 6).ok_or(cut)? as usize;
        let stamp = b.u32(pe + 8).ok_or(cut)?;
        let opt_size = b.u16(pe + 20).ok_or(cut)? as usize;
        let chars = b.u16(pe + 22).ok_or(cut)?;
        let opt = pe + 24;
        let magic = b.u16(opt).unwrap_or(0);
        let plus = magic == 0x20b;
        let dll = chars & 0x2000 != 0;
        let mut out = Inspection::new(
            "PE",
            if dll {
                "Windows library (DLL)"
            } else {
                "Windows program (PE)"
            },
        );
        let coff = span(pe, 24);
        out.fact(
            "Headers",
            "DOS header",
            format!("MZ, PE header at byte {pe}"),
            span(0, 64),
        );
        out.fact(
            "Headers",
            "Machine",
            format!("{} (0x{mach:x})", machine(mach)),
            coff.clone(),
        );
        out.fact(
            "Headers",
            "Timestamp",
            format!(
                "{} (0x{stamp:08x}; may be a hash in reproducible builds)",
                crate::binary::utc(i64::from(stamp))
            ),
            coff.clone(),
        );
        out.fact(
            "Headers",
            "Characteristics",
            flags(u64::from(chars), CHARACTERISTICS),
            coff,
        );
        let ohdr = span(opt, opt_size);
        let mut dirs: Vec<(u32, u32)> = vec![];
        if opt_size >= 2 && (magic == 0x10b || plus) {
            out.version = Some(if plus { "PE32+".into() } else { "PE32".into() });
            out.fact(
                "Optional header",
                "Format",
                if plus {
                    "PE32+ (64-bit)"
                } else {
                    "PE32 (32-bit)"
                },
                ohdr.clone(),
            );
            if let (Some(maj), Some(min)) = (b.u8(opt + 2), b.u8(opt + 3)) {
                out.fact(
                    "Optional header",
                    "Linker version",
                    format!("{maj}.{min}"),
                    ohdr.clone(),
                );
            }
            out.fact(
                "Optional header",
                "Entry point (RVA)",
                b.u32(opt + 16)
                    .map(|v| format!("0x{v:x}"))
                    .unwrap_or_default(),
                ohdr.clone(),
            );
            let base = if plus {
                b.u64(opt + 24)
            } else {
                b.u32(opt + 28).map(u64::from)
            };
            out.fact(
                "Optional header",
                "Image base",
                base.map(|v| format!("0x{v:x}")).unwrap_or_default(),
                ohdr.clone(),
            );
            if let (Some(maj), Some(min)) = (b.u16(opt + 40), b.u16(opt + 42)) {
                out.fact(
                    "Optional header",
                    "Minimum OS version",
                    format!("{maj}.{min}"),
                    ohdr.clone(),
                );
            }
            if let Some(s) = b.u16(opt + 68) {
                out.fact("Optional header", "Subsystem", subsystem(s), ohdr.clone());
            }
            if let Some(d) = b.u16(opt + 70) {
                out.fact(
                    "Optional header",
                    "DLL characteristics",
                    flags(u64::from(d), DLL_CHARACTERISTICS),
                    ohdr.clone(),
                );
            }
            let nd_at = opt + if plus { 108 } else { 92 };
            let nd = (b.u32(nd_at).unwrap_or(0) as usize).min(16);
            for i in 0..nd {
                let at = nd_at + 4 + i * 8;
                if at + 8 > opt + opt_size {
                    break;
                }
                dirs.push((b.u32(at).unwrap_or(0), b.u32(at + 4).unwrap_or(0)));
            }
        }
        let dir = |i: usize| dirs.get(i).copied().filter(|d| d.0 != 0 && d.1 != 0);
        // Sections.
        let mut problems: Vec<String> = vec![];
        let mut secs_t = table(
            "Sections",
            &[
                "Name",
                "Virtual address",
                "Virtual size",
                "File offset",
                "File size",
                "Flags",
            ],
        );
        let mut secs = vec![];
        let table_at = opt + opt_size;
        for i in 0..nsec.min(MAX_SECTIONS) {
            let at = table_at + i * 40;
            let Some(name) = b.get(at, 8) else {
                problems.push("the section table is cut".into());
                break;
            };
            let name = printable(String::from_utf8_lossy(name).trim_end_matches('\0'));
            let (vsize, va, raw_size, raw_off, ch) = (
                b.u32(at + 8).unwrap_or(0),
                b.u32(at + 12).unwrap_or(0),
                b.u32(at + 16).unwrap_or(0),
                b.u32(at + 20).unwrap_or(0),
                b.u32(at + 36).unwrap_or(0),
            );
            secs_t.rows.push(Row {
                cells: vec![
                    name,
                    format!("0x{va:x}"),
                    bytes_text(u64::from(vsize)),
                    format!("0x{raw_off:x}"),
                    bytes_text(u64::from(raw_size)),
                    flags(u64::from(ch), SECTION_FLAGS),
                ],
                source: span(raw_off as usize, raw_size as usize),
            });
            secs.push(Sec {
                va,
                vsize,
                raw_off,
                raw_size,
            });
        }
        if nsec > MAX_SECTIONS {
            problems.push(format!(
                "{nsec} sections declared; the first {MAX_SECTIONS} are read"
            ));
        }
        out.fact("Headers", "Sections", nsec.to_string(), None);
        let img = Image { b, secs };
        // Imports.
        let mut imports = table("Imports", &["Library", "Function", "Hint or ordinal"]);
        let mut libs = vec![];
        if let Some((rva, _)) = dir(1) {
            let mut d = img.off(rva);
            let mut guard = 0;
            while let Some(at) = d {
                if guard > 4096 {
                    break;
                }
                guard += 1;
                let (Some(oft), Some(name), Some(ft)) = (b.u32(at), b.u32(at + 12), b.u32(at + 16))
                else {
                    problems.push("the import directory is cut".into());
                    break;
                };
                if oft == 0 && name == 0 && ft == 0 {
                    break;
                }
                let lib = img.cstr(name, 256).unwrap_or_default();
                libs.push(lib.clone());
                let thunks = if oft != 0 { oft } else { ft };
                let step = if plus { 8 } else { 4 };
                for k in 0..MAX_IMPORTS {
                    if k % 4096 == 0 && cancelled() {
                        return Err("Cancelled".into());
                    }
                    let Some(t_at) = img.off(thunks.wrapping_add((k * step) as u32)) else {
                        break;
                    };
                    let Some(v) = (if plus {
                        b.u64(t_at)
                    } else {
                        b.u32(t_at).map(u64::from)
                    }) else {
                        break;
                    };
                    if v == 0 {
                        break;
                    }
                    let ordinal_flag = if plus { 1u64 << 63 } else { 1u64 << 31 };
                    let (func, hint) = if v & ordinal_flag != 0 {
                        (String::new(), format!("ordinal {}", v & 0xffff))
                    } else {
                        let r = (v & 0x7fff_ffff) as u32;
                        let h = img.off(r).and_then(|o| b.u16(o)).unwrap_or(0);
                        (
                            img.cstr(r.wrapping_add(2), 512).unwrap_or_default(),
                            h.to_string(),
                        )
                    };
                    if imports.rows.len() < MAX_ROWS {
                        imports.rows.push(Row {
                            cells: vec![lib.clone(), func, hint],
                            source: span(t_at, step),
                        });
                    } else {
                        imports.total = Some(imports.total.unwrap_or(MAX_ROWS as u64) + 1);
                    }
                }
                d = at.checked_add(20);
            }
        }
        out.fact("Imports", "Libraries", libs.join(", "), None);
        out.fact(
            "Imports",
            "Functions",
            imports
                .total
                .unwrap_or(imports.rows.len() as u64)
                .to_string(),
            None,
        );
        // Exports.
        let mut exports = table(
            "Exports",
            &["Ordinal", "Name", "Address (RVA) or forwarder"],
        );
        if let Some((rva, size)) = dir(0)
            && let Some(at) = img.off(rva)
        {
            let name = b
                .u32(at + 12)
                .and_then(|r| img.cstr(r, 256))
                .unwrap_or_default();
            out.fact("Exports", "Library name", name, span(at, 40));
            let base = b.u32(at + 16).unwrap_or(0);
            let nfun = b.u32(at + 20).unwrap_or(0) as usize;
            let nnames = b.u32(at + 24).unwrap_or(0) as usize;
            let (funcs, names, ords) = (
                b.u32(at + 28).unwrap_or(0),
                b.u32(at + 32).unwrap_or(0),
                b.u32(at + 36).unwrap_or(0),
            );
            out.fact("Exports", "Functions", nfun.to_string(), None);
            let mut named: std::collections::HashMap<u32, String> = Default::default();
            for i in 0..nnames.min(MAX_ROWS) {
                let n = img
                    .off(names.wrapping_add((i * 4) as u32))
                    .and_then(|o| b.u32(o));
                let o = img
                    .off(ords.wrapping_add((i * 2) as u32))
                    .and_then(|o| b.u16(o));
                let (Some(n), Some(o)) = (n, o) else {
                    break;
                };
                named.insert(u32::from(o), img.cstr(n, 512).unwrap_or_default());
            }
            for i in 0..nfun.min(MAX_ROWS) {
                let Some(f_at) = img.off(funcs.wrapping_add((i * 4) as u32)) else {
                    break;
                };
                let Some(addr) = b.u32(f_at) else {
                    break;
                };
                if addr == 0 {
                    continue;
                }
                let target = if addr >= rva && addr - rva < size {
                    format!("→ {}", img.cstr(addr, 512).unwrap_or_default())
                } else {
                    format!("0x{addr:x}")
                };
                exports.rows.push(Row {
                    cells: vec![
                        base.wrapping_add(i as u32).to_string(),
                        named.get(&(i as u32)).cloned().unwrap_or_default(),
                        target,
                    ],
                    source: span(f_at, 4),
                });
            }
        }
        // Resources.
        let mut res_t = table("Resources", &["Type", "Entries"]);
        if let Some((rva, _)) = dir(2)
            && let Some(root) = img.off(rva)
        {
            let entries = |dir_at: usize| -> Vec<(u32, u32)> {
                let n = usize::from(b.u16(dir_at + 12).unwrap_or(0))
                    + usize::from(b.u16(dir_at + 14).unwrap_or(0));
                (0..n.min(4096))
                    .map_while(|i| Some((b.u32(dir_at + 16 + i * 8)?, b.u32(dir_at + 20 + i * 8)?)))
                    .collect()
            };
            let name_of = |id: u32| -> String {
                if id & 0x8000_0000 != 0 {
                    let at = root + (id & 0x7fff_ffff) as usize;
                    let len = b.u16(at).unwrap_or(0) as usize;
                    let units: Vec<u16> = (0..len.min(256))
                        .filter_map(|i| b.u16(at + 2 + i * 2))
                        .collect();
                    printable(&String::from_utf16_lossy(&units))
                } else {
                    resource_type(id)
                }
            };
            // Counts the data leaves under a directory (levels: type, name, language).
            fn leaves(b: &Bytes, root: usize, at: usize, depth: u8, out: &mut Vec<usize>) {
                if depth > 3 || out.len() > 65_536 {
                    return;
                }
                let n = usize::from(b.u16(at + 12).unwrap_or(0))
                    + usize::from(b.u16(at + 14).unwrap_or(0));
                for i in 0..n.min(4096) {
                    let Some(off) = b.u32(at + 20 + i * 8) else {
                        return;
                    };
                    let target = root + (off & 0x7fff_ffff) as usize;
                    if off & 0x8000_0000 != 0 {
                        if target > at {
                            leaves(b, root, target, depth + 1, out);
                        }
                    } else {
                        out.push(target);
                    }
                }
            }
            for (id, off) in entries(root).into_iter().take(MAX_ROWS) {
                let mut found = vec![];
                if off & 0x8000_0000 != 0 {
                    let target = root + (off & 0x7fff_ffff) as usize;
                    if target > root {
                        leaves(&b, root, target, 1, &mut found);
                    }
                } else {
                    found.push(root + off as usize);
                }
                res_t.rows.push(Row {
                    cells: vec![name_of(id), found.len().to_string()],
                    source: None,
                });
                if id == 16
                    && let Some(&leaf) = found.first()
                    && let (Some(data_rva), Some(size)) = (b.u32(leaf), b.u32(leaf + 4))
                    && let Some(at) = img.off(data_rva)
                {
                    version_info(&b, at, size as usize, &mut out);
                }
            }
        }
        // .NET header.
        if let Some((rva, _)) = dir(14) {
            let at = img.off(rva);
            let (maj, min) = at
                .map(|a| (b.u16(a + 4).unwrap_or(0), b.u16(a + 6).unwrap_or(0)))
                .unwrap_or((0, 0));
            out.fact(
                ".NET",
                "CLR header",
                format!("present (runtime header {maj}.{min})"),
                at.and_then(|a| span(a, 72)),
            );
            let meta = at.and_then(|a| b.u32(a + 8)).and_then(|r| img.off(r));
            if let Some(m) = meta
                && b.u32(m) == Some(0x424a_5342)
            {
                let len = b.u32(m + 12).unwrap_or(0) as usize;
                let v = b.cstr(m + 16, len.min(256)).unwrap_or_default();
                out.fact(
                    ".NET",
                    "Runtime version",
                    printable(&v),
                    span(m, 16 + len.min(256)),
                );
            }
            if let Some(f) = at.and_then(|a| b.u32(a + 16)) {
                out.fact(
                    ".NET",
                    "Flags",
                    flags(
                        u64::from(f),
                        &[
                            (1, "IL only"),
                            (2, "32-bit required"),
                            (8, "strong-name signed"),
                            (0x10, "native entry point"),
                            (0x20000, "32-bit preferred"),
                        ],
                    ),
                    None,
                );
            }
        }
        // Authenticode: the certificate table holds a file offset, not an RVA.
        if let Some((off, size)) = dir(4) {
            let at = off as usize;
            out.fact(
                "Signature",
                "Certificate table",
                format!("present ({}) at byte {at}", bytes_text(u64::from(size))),
                span(at, size as usize),
            );
            let kind = b.u16(at + 6);
            let len = b.u32(at).unwrap_or(0) as usize;
            out.fact(
                "Signature",
                "Kind",
                match kind {
                    Some(2) => "Authenticode (PKCS#7 signed data)",
                    Some(1) => "X.509 certificate",
                    _ => "other",
                },
                None,
            );
            if kind == Some(2)
                && let Some(data) = b.get(at + 8, len.saturating_sub(8))
                && let Ok(top) = der::parse(data, 0, data.len())
            {
                let mut sig = Inspection::default();
                if cms::content_info(&top, &mut sig, true).is_ok() {
                    // Signer facts, with sources moved to the file.
                    for mut f in sig.facts.into_iter().filter(|f| {
                        f.group.starts_with("Signer") || f.group.starts_with("Certificate ")
                    }) {
                        f.source = f
                            .source
                            .map(|r| r.start + (at as u64 + 8)..r.end + (at as u64 + 8));
                        f.group = format!("Authenticode {}", f.group.to_lowercase());
                        out.facts.push(f);
                    }
                    for mut t in sig.tables {
                        for r in &mut t.rows {
                            r.source = r
                                .source
                                .take()
                                .map(|s| s.start + (at as u64 + 8)..s.end + (at as u64 + 8));
                        }
                        t.name = format!("Authenticode {}", t.name.to_lowercase());
                        out.tables.push(t);
                    }
                }
            }
            out.check(
                Stage::Signature,
                State::NotDone,
                "The Authenticode signature was not verified (nor the file's hash)",
            );
        } else {
            out.fact("Signature", "Certificate table", "none (not signed)", None);
        }
        out.fact(
            "Headers",
            "Debug directory",
            if dir(6).is_some() { "present" } else { "" },
            None,
        );
        for t in [secs_t, imports, exports, res_t] {
            if !t.rows.is_empty() {
                out.tables.push(t);
            }
        }
        if problems.is_empty() {
            out.check(
                Stage::Parsed,
                State::Passed,
                "Headers, sections, imports, exports and resources read (code is not disassembled)",
            );
        } else {
            out.check(
                Stage::Parsed,
                State::Partial,
                format!("Damaged: {}", problems.join("; ")),
            );
        }
        out.not_validated();
        out.notes
            .push("Programs are never loaded or run; code is not disassembled.".into());
        Ok(out)
    }
}

/// VS_VERSIONINFO: the fixed file version and the strings (CompanyName, FileVersion, …).
fn version_info(b: &Bytes, at: usize, size: usize, out: &mut Inspection) {
    // A block: wLength, wValueLength, wType, szKey (UTF-16, NUL), padding, value, children.
    fn key(b: &Bytes, at: usize, end: usize) -> (String, usize) {
        let mut units = vec![];
        let mut p = at;
        while p + 2 <= end && units.len() < 128 {
            let u = b.u16(p).unwrap_or(0);
            p += 2;
            if u == 0 {
                break;
            }
            units.push(u);
        }
        (String::from_utf16_lossy(&units), p)
    }
    fn walk(b: &Bytes, at: usize, end: usize, depth: u8, out: &mut Inspection) {
        let mut p = at;
        let mut guard = 0;
        while p + 6 <= end && depth <= 4 && guard < 512 {
            guard += 1;
            let len = b.u16(p).unwrap_or(0) as usize;
            if len < 6 {
                return;
            }
            let block_end = (p + len).min(end);
            let vlen = b.u16(p + 2).unwrap_or(0) as usize;
            let text = b.u16(p + 4) == Some(1);
            let (k, after) = key(b, p + 6, block_end);
            let value_at = after.div_ceil(4) * 4;
            let value_bytes = if text { vlen * 2 } else { vlen };
            if depth == 0 && b.u32(value_at) == Some(0xfeef_04bd) {
                let (ms, ls) = (
                    b.u32(value_at + 8).unwrap_or(0),
                    b.u32(value_at + 12).unwrap_or(0),
                );
                out.fact(
                    "Version information",
                    "File version (fixed)",
                    format!("{}.{}.{}.{}", ms >> 16, ms & 0xffff, ls >> 16, ls & 0xffff),
                    span(value_at, 52),
                );
            }
            if depth == 3 && text {
                let units: Vec<u16> = (0..vlen.min(1024))
                    .filter_map(|i| b.u16(value_at + i * 2))
                    .take_while(|&u| u != 0)
                    .collect();
                let v = printable(&String::from_utf16_lossy(&units));
                out.fact(
                    "Version information",
                    &printable(&k),
                    clip(&v, 500),
                    span(p, len),
                );
            }
            let children = (value_at + value_bytes).div_ceil(4) * 4;
            if children < block_end && (depth == 0 || k == "StringFileInfo" || depth == 2) {
                walk(b, children, block_end, depth + 1, out);
            }
            let next = (p + len).div_ceil(4) * 4;
            if next <= p {
                return;
            }
            p = next;
        }
    }
    walk(b, at, at.saturating_add(size).min(b.b.len()), 0, out);
}
