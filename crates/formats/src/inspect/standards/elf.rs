//! ELF executables, libraries and objects (System V ABI): header, program and section headers,
//! interpreter, dynamic section (needed libraries, SONAME, RPATH/RUNPATH), symbols from
//! `.symtab` and `.dynsym`, and notes (GNU build ID, ABI tag). Never loaded or run.
use super::hash::hex;
use super::util::{Bytes, bytes_text, clip, flags, printable, span, us};
use crate::inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Row, Stage, State, Table};

pub struct Elf;

/// Sections and segments read at most (their counts are 16-bit, but can be extended).
const MAX_HEADERS: usize = 65_536;

pub fn is_elf(h: &[u8]) -> bool {
    h.starts_with(b"\x7fELF")
        && matches!(h.get(4), Some(1) | Some(2))
        && matches!(h.get(5), Some(1) | Some(2))
}

fn os_abi(v: u8) -> &'static str {
    match v {
        0 => "System V",
        1 => "HP-UX",
        2 => "NetBSD",
        3 => "Linux (GNU)",
        6 => "Solaris",
        7 => "AIX",
        8 => "IRIX",
        9 => "FreeBSD",
        10 => "Tru64",
        12 => "OpenBSD",
        97 => "ARM EABI",
        255 => "standalone",
        _ => "other",
    }
}

fn file_type(v: u16) -> &'static str {
    match v {
        0 => "none",
        1 => "relocatable object",
        2 => "executable",
        3 => "shared object (library or position-independent executable)",
        4 => "core dump",
        _ => "other",
    }
}

fn machine(v: u16) -> &'static str {
    match v {
        0x02 => "SPARC",
        0x03 => "x86",
        0x08 => "MIPS",
        0x14 => "PowerPC",
        0x15 => "PowerPC 64",
        0x16 => "S/390",
        0x28 => "ARM",
        0x2b => "SPARC V9",
        0x32 => "IA-64",
        0x3e => "x86-64",
        0xb7 => "AArch64",
        0xf3 => "RISC-V",
        0xf7 => "eBPF",
        0x102 => "LoongArch",
        _ => "other",
    }
}

fn ph_type(v: u32) -> String {
    match v {
        0 => "NULL",
        1 => "LOAD",
        2 => "DYNAMIC",
        3 => "INTERP",
        4 => "NOTE",
        5 => "SHLIB",
        6 => "PHDR",
        7 => "TLS",
        0x6474_e550 => "GNU_EH_FRAME",
        0x6474_e551 => "GNU_STACK",
        0x6474_e552 => "GNU_RELRO",
        0x6474_e553 => "GNU_PROPERTY",
        _ => return format!("0x{v:x}"),
    }
    .into()
}

fn sh_type(v: u32) -> String {
    match v {
        0 => "NULL",
        1 => "PROGBITS",
        2 => "SYMTAB",
        3 => "STRTAB",
        4 => "RELA",
        5 => "HASH",
        6 => "DYNAMIC",
        7 => "NOTE",
        8 => "NOBITS",
        9 => "REL",
        11 => "DYNSYM",
        14 => "INIT_ARRAY",
        15 => "FINI_ARRAY",
        16 => "PREINIT_ARRAY",
        17 => "GROUP",
        18 => "SYMTAB_SHNDX",
        0x6fff_fff6 => "GNU_HASH",
        0x6fff_fffd => "VERDEF",
        0x6fff_fffe => "VERNEED",
        0x6fff_ffff => "VERSYM",
        _ => return format!("0x{v:x}"),
    }
    .into()
}

const SH_FLAGS: &[(u64, &str)] = &[
    (0x1, "write"),
    (0x2, "alloc"),
    (0x4, "exec"),
    (0x10, "merge"),
    (0x20, "strings"),
    (0x40, "info link"),
    (0x80, "link order"),
    (0x200, "group"),
    (0x400, "TLS"),
    (0x800, "compressed"),
];

fn dyn_tag(v: u64) -> String {
    match v {
        1 => "NEEDED",
        2 => "PLTRELSZ",
        3 => "PLTGOT",
        4 => "HASH",
        5 => "STRTAB",
        6 => "SYMTAB",
        7 => "RELA",
        8 => "RELASZ",
        10 => "STRSZ",
        12 => "INIT",
        13 => "FINI",
        14 => "SONAME",
        15 => "RPATH",
        17 => "REL",
        20 => "PLTREL",
        21 => "DEBUG",
        23 => "JMPREL",
        24 => "BIND_NOW",
        25 => "INIT_ARRAY",
        26 => "FINI_ARRAY",
        29 => "RUNPATH",
        30 => "FLAGS",
        0x6fff_fef5 => "GNU_HASH",
        0x6fff_fff0 => "VERSYM",
        0x6fff_fffb => "FLAGS_1",
        0x6fff_fffe => "VERNEED",
        0x6fff_ffff => "VERNEEDNUM",
        _ => return format!("0x{v:x}"),
    }
    .into()
}

#[derive(Clone, Debug, Default)]
struct Section {
    name: String,
    kind: u32,
    flags: u64,
    addr: u64,
    offset: u64,
    size: u64,
    link: u32,
    entsize: u64,
}

fn table(name: &str, cols: &[&str]) -> Table {
    Table {
        name: name.into(),
        columns: cols.iter().map(|c| (*c).to_owned()).collect(),
        ..Table::default()
    }
}

impl Handler for Elf {
    fn name(&self) -> &'static str {
        "ELF"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        is_elf(probe.head)
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let raw = input.bytes;
        if !is_elf(raw) {
            return Err("Not an ELF file".into());
        }
        let wide = raw[4] == 2;
        let b = Bytes::new(raw, raw[5] == 1);
        let w = |at: usize| b.word(at, wide);
        let cut = "The ELF header is cut";
        let kind = b.u16(16).ok_or(cut)?;
        let mach = b.u16(18).ok_or(cut)?;
        let (entry, phoff, shoff) = if wide {
            (w(24), w(32), w(40))
        } else {
            (w(24), w(28), w(32))
        };
        let (entry, phoff, shoff) = (entry.ok_or(cut)?, phoff.ok_or(cut)?, shoff.ok_or(cut)?);
        let base = if wide { 48 } else { 36 };
        let phentsize = b.u16(base + 6).ok_or(cut)? as usize;
        let mut phnum = b.u16(base + 8).ok_or(cut)? as usize;
        let shentsize = b.u16(base + 10).ok_or(cut)? as usize;
        let mut shnum = b.u16(base + 12).ok_or(cut)? as usize;
        let mut shstrndx = b.u16(base + 14).ok_or(cut)? as usize;
        let mut out = Inspection::new("ELF", file_type(kind));
        out.kind = format!("ELF {}", file_type(kind));
        let hdr = span(0, if wide { 64 } else { 52 });
        out.version = Some(raw.get(6).copied().unwrap_or(0).to_string());
        out.fact(
            "Header",
            "Class",
            if wide { "64-bit" } else { "32-bit" },
            hdr.clone(),
        );
        out.fact(
            "Header",
            "Byte order",
            if b.le { "little-endian" } else { "big-endian" },
            hdr.clone(),
        );
        out.fact(
            "Header",
            "OS/ABI",
            os_abi(raw.get(7).copied().unwrap_or(0)),
            hdr.clone(),
        );
        out.fact("Header", "Type", file_type(kind), hdr.clone());
        out.fact(
            "Header",
            "Machine",
            format!("{} (0x{mach:x})", machine(mach)),
            hdr.clone(),
        );
        out.fact("Header", "Entry point", format!("0x{entry:x}"), hdr.clone());
        let mut problems: Vec<String> = vec![];
        let shoff_us = us(shoff).unwrap_or(usize::MAX);
        // Extended numbering: the real counts live in section 0.
        let sec0 = |field: usize| -> Option<u64> {
            let at = shoff_us.checked_add(field)?;
            if field == 32 || field == 20 {
                b.word(at, wide)
            } else {
                b.u32(at).map(u64::from)
            }
        };
        if shnum == 0 && shoff != 0 {
            shnum = sec0(if wide { 32 } else { 20 }).and_then(us).unwrap_or(0);
        }
        if shstrndx == 0xffff {
            shstrndx = sec0(if wide { 40 } else { 24 }).and_then(us).unwrap_or(0);
        }
        if phnum == 0xffff {
            phnum = sec0(if wide { 44 } else { 28 }).and_then(us).unwrap_or(0);
        }
        // Sections.
        let mut sections: Vec<Section> = vec![];
        if shoff != 0 && shentsize >= if wide { 64 } else { 40 } {
            for i in 0..shnum.min(MAX_HEADERS) {
                let Some(at) = i
                    .checked_mul(shentsize)
                    .and_then(|o| o.checked_add(shoff_us))
                else {
                    break;
                };
                let s = if wide {
                    (|| {
                        Some(Section {
                            name: b.u32(at)?.to_string(),
                            kind: b.u32(at + 4)?,
                            flags: b.u64(at + 8)?,
                            addr: b.u64(at + 16)?,
                            offset: b.u64(at + 24)?,
                            size: b.u64(at + 32)?,
                            link: b.u32(at + 40)?,
                            entsize: b.u64(at + 56)?,
                        })
                    })()
                } else {
                    (|| {
                        Some(Section {
                            name: b.u32(at)?.to_string(),
                            kind: b.u32(at + 4)?,
                            flags: u64::from(b.u32(at + 8)?),
                            addr: u64::from(b.u32(at + 12)?),
                            offset: u64::from(b.u32(at + 16)?),
                            size: u64::from(b.u32(at + 20)?),
                            link: b.u32(at + 24)?,
                            entsize: u64::from(b.u32(at + 36)?),
                        })
                    })()
                };
                match s {
                    Some(s) => sections.push(s),
                    None => {
                        problems.push(format!("section header {i} is past the end of the file"));
                        break;
                    }
                }
            }
        }
        // Section names from the section-name string table.
        let names_off = sections.get(shstrndx).map(|s| s.offset);
        for s in &mut sections {
            let idx: usize = s.name.parse().unwrap_or(0);
            s.name = names_off
                .and_then(|o| us(o)?.checked_add(idx))
                .and_then(|at| b.cstr(at, 256))
                .map(|n| printable(&n))
                .unwrap_or_default();
        }
        let data_of = |s: &Section| -> Option<&[u8]> {
            if s.kind == 8 {
                return None;
            }
            b.get(us(s.offset)?, us(s.size)?)
        };
        let str_in = |table: Option<&Section>, idx: u64| -> String {
            table
                .and_then(|t| {
                    let at = us(t.offset)?.checked_add(us(idx)?)?;
                    (idx < t.size).then_some(())?;
                    b.cstr(at, 4096)
                })
                .map(|s| printable(&s))
                .unwrap_or_default()
        };
        // Program headers.
        let mut segs = table(
            "Program headers",
            &[
                "Type",
                "Offset",
                "Virtual address",
                "File size",
                "Memory size",
                "Flags",
                "Align",
            ],
        );
        let mut loads: Vec<(u64, u64, u64)> = vec![];
        let mut interp = String::new();
        let mut notes: Vec<(usize, usize)> = vec![];
        if phoff != 0 && phentsize >= if wide { 56 } else { 32 } {
            for i in 0..phnum.min(MAX_HEADERS) {
                let Some(at) = i
                    .checked_mul(phentsize)
                    .and_then(|o| o.checked_add(us(phoff)?))
                else {
                    break;
                };
                let p = if wide {
                    (|| {
                        Some((
                            b.u32(at)?,
                            b.u32(at + 4)?,
                            b.u64(at + 8)?,
                            b.u64(at + 16)?,
                            b.u64(at + 32)?,
                            b.u64(at + 40)?,
                            b.u64(at + 48)?,
                        ))
                    })()
                } else {
                    (|| {
                        Some((
                            b.u32(at)?,
                            b.u32(at + 24)?,
                            u64::from(b.u32(at + 4)?),
                            u64::from(b.u32(at + 8)?),
                            u64::from(b.u32(at + 16)?),
                            u64::from(b.u32(at + 20)?),
                            u64::from(b.u32(at + 28)?),
                        ))
                    })()
                };
                let Some((ty, fl, off, vaddr, filesz, memsz, align)) = p else {
                    problems.push(format!("program header {i} is past the end of the file"));
                    break;
                };
                if ty == 1 {
                    loads.push((vaddr, off, filesz));
                }
                if ty == 3 {
                    interp = us(off)
                        .and_then(|o| b.cstr(o, 4096))
                        .map(|s| printable(&s))
                        .unwrap_or_default();
                }
                if ty == 4
                    && let (Some(o), Some(l)) = (us(off), us(filesz))
                {
                    notes.push((o, l));
                }
                let f = format!(
                    "{}{}{}",
                    if fl & 4 != 0 { "R" } else { "-" },
                    if fl & 2 != 0 { "W" } else { "-" },
                    if fl & 1 != 0 { "X" } else { "-" }
                );
                segs.rows.push(Row {
                    cells: vec![
                        ph_type(ty),
                        format!("0x{off:x}"),
                        format!("0x{vaddr:x}"),
                        bytes_text(filesz),
                        bytes_text(memsz),
                        f,
                        format!("0x{align:x}"),
                    ],
                    source: us(off).zip(us(filesz)).and_then(|(o, l)| span(o, l)),
                });
            }
        }
        out.fact(
            "Header",
            "Program headers",
            segs.rows.len().to_string(),
            None,
        );
        out.fact("Header", "Sections", sections.len().to_string(), None);
        out.fact("Program", "Interpreter", interp, None);
        // Section table.
        let mut secs = table(
            "Sections",
            &["#", "Name", "Type", "Flags", "Address", "Offset", "Size"],
        );
        for (i, s) in sections.iter().enumerate() {
            secs.rows.push(Row {
                cells: vec![
                    i.to_string(),
                    s.name.clone(),
                    sh_type(s.kind),
                    flags(s.flags, SH_FLAGS),
                    format!("0x{:x}", s.addr),
                    format!("0x{:x}", s.offset),
                    bytes_text(s.size),
                ],
                source: if s.kind == 8 {
                    None
                } else {
                    us(s.offset).zip(us(s.size)).and_then(|(o, l)| span(o, l))
                },
            });
            if s.kind == 7 && !loads.is_empty() {
                continue;
            }
            if s.kind == 7
                && let (Some(o), Some(l)) = (us(s.offset), us(s.size))
            {
                notes.push((o, l));
            }
        }
        // Dynamic section: from the section (with its string table) or the segment.
        let mut dyn_t = table("Dynamic section", &["Tag", "Value"]);
        let dyn_sec = sections.iter().find(|s| s.kind == 6);
        if let Some(ds) = dyn_sec
            && let Some(data) = data_of(ds)
        {
            let strtab = sections.get(ds.link as usize);
            let esz = if wide { 16 } else { 8 };
            let d = Bytes::new(data, b.le);
            let mut needed = vec![];
            for i in 0..(data.len() / esz).min(MAX_ROWS) {
                let (Some(tag), Some(val)) =
                    (d.word(i * esz, wide), d.word(i * esz + esz / 2, wide))
                else {
                    break;
                };
                if tag == 0 {
                    break;
                }
                let text = match tag {
                    1 | 14 | 15 | 29 => str_in(strtab, val),
                    _ => format!("0x{val:x}"),
                };
                match tag {
                    1 => needed.push(text.clone()),
                    14 => out.fact("Dynamic linking", "SONAME", text.clone(), None),
                    15 => out.fact("Dynamic linking", "RPATH", text.clone(), None),
                    29 => out.fact("Dynamic linking", "RUNPATH", text.clone(), None),
                    _ => {}
                }
                dyn_t.rows.push(Row {
                    cells: vec![dyn_tag(tag), text],
                    source: us(ds.offset).and_then(|o| span(o + i * esz, esz)),
                });
            }
            out.fact(
                "Dynamic linking",
                "Needed libraries",
                needed.join(", "),
                us(ds.offset).zip(us(ds.size)).and_then(|(o, l)| span(o, l)),
            );
        }
        // Notes: build ID and ABI tag.
        let mut seen = std::collections::HashSet::new();
        for (o, l) in notes {
            if !seen.insert(o) {
                continue;
            }
            let mut p = o;
            let end = o.saturating_add(l).min(raw.len());
            let mut guard = 0;
            while p + 12 <= end && guard < 64 {
                guard += 1;
                let (Some(namesz), Some(descsz), Some(ty)) = (b.u32(p), b.u32(p + 4), b.u32(p + 8))
                else {
                    break;
                };
                let name_at = p + 12;
                let desc_at = name_at + (namesz as usize).div_ceil(4) * 4;
                let next = desc_at.saturating_add((descsz as usize).div_ceil(4) * 4);
                let name = b
                    .cstr(name_at, (namesz as usize).min(64))
                    .unwrap_or_default();
                let desc = b.get(desc_at, descsz as usize);
                if name == "GNU"
                    && let Some(desc) = desc
                {
                    match ty {
                        3 => out.fact("Program", "Build ID", hex(desc), span(desc_at, desc.len())),
                        1 if desc.len() >= 16 => {
                            let d = Bytes::new(desc, b.le);
                            let os = match d.u32(0) {
                                Some(0) => "Linux",
                                Some(1) => "GNU Hurd",
                                Some(2) => "Solaris",
                                Some(3) => "FreeBSD",
                                _ => "other",
                            };
                            out.fact(
                                "Program",
                                "ABI tag",
                                format!(
                                    "{os} {}.{}.{}",
                                    d.u32(4).unwrap_or(0),
                                    d.u32(8).unwrap_or(0),
                                    d.u32(12).unwrap_or(0)
                                ),
                                span(desc_at, desc.len()),
                            );
                        }
                        _ => {}
                    }
                }
                if next <= p {
                    break;
                }
                p = next;
            }
        }
        // Symbols.
        let mut syms = table(
            "Symbols",
            &[
                "Table", "Name", "Value", "Size", "Type", "Binding", "Section",
            ],
        );
        let mut total: u64 = 0;
        for s in sections.iter().filter(|s| s.kind == 2 || s.kind == 11) {
            let Some(data) = data_of(s) else {
                continue;
            };
            let esz = if wide { 24 } else { 16 };
            if s.entsize != 0 && s.entsize as usize != esz {
                problems.push(format!("{} has an unexpected entry size", s.name));
                continue;
            }
            let strtab = sections.get(s.link as usize);
            let d = Bytes::new(data, b.le);
            let n = data.len() / esz;
            total += n as u64;
            for i in 0..n {
                if i % 4096 == 0 && cancelled() {
                    return Err("Cancelled".into());
                }
                if syms.rows.len() >= MAX_ROWS {
                    break;
                }
                let at = i * esz;
                let parsed = if wide {
                    (|| {
                        Some((
                            d.u32(at)?,
                            d.u8(at + 4)?,
                            d.u16(at + 6)?,
                            d.u64(at + 8)?,
                            d.u64(at + 16)?,
                        ))
                    })()
                } else {
                    (|| {
                        Some((
                            d.u32(at)?,
                            d.u8(at + 12)?,
                            d.u16(at + 14)?,
                            u64::from(d.u32(at + 4)?),
                            u64::from(d.u32(at + 8)?),
                        ))
                    })()
                };
                let Some((name, info, shndx, value, size)) = parsed else {
                    break;
                };
                let ty = match info & 0xf {
                    0 => "NOTYPE",
                    1 => "OBJECT",
                    2 => "FUNC",
                    3 => "SECTION",
                    4 => "FILE",
                    5 => "COMMON",
                    6 => "TLS",
                    10 => "IFUNC",
                    _ => "other",
                };
                let bind = match info >> 4 {
                    0 => "LOCAL",
                    1 => "GLOBAL",
                    2 => "WEAK",
                    10 => "UNIQUE",
                    _ => "other",
                };
                let sec = match shndx {
                    0 => "undefined".to_owned(),
                    0xfff1 => "absolute".into(),
                    0xfff2 => "common".into(),
                    n => sections
                        .get(n as usize)
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| n.to_string()),
                };
                syms.rows.push(Row {
                    cells: vec![
                        s.name.clone(),
                        clip(&str_in(strtab, u64::from(name)), 500),
                        format!("0x{value:x}"),
                        size.to_string(),
                        ty.into(),
                        bind.into(),
                        sec,
                    ],
                    source: us(s.offset).and_then(|o| span(o + at, esz)),
                });
            }
        }
        if total > syms.rows.len() as u64 {
            syms.total = Some(total);
        }
        out.fact("Program", "Symbols", total.to_string(), None);
        if !sections.iter().any(|s| s.kind == 2) && kind != 4 {
            out.fact("Program", "Debug symbols", "stripped (no .symtab)", None);
        }
        for t in [segs, secs, dyn_t, syms] {
            if !t.rows.is_empty() {
                out.tables.push(t);
            }
        }
        if problems.is_empty() {
            out.check(
                Stage::Parsed,
                State::Passed,
                "Headers, dynamic section, notes and symbols read (code is not disassembled)",
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
