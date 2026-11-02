//! BAM alignments (SAM in binary, BGZF-compressed): the SAM header (@HD, @SQ, @RG, @PG),
//! the reference sequences and the first alignment records. BGZF blocks are decompressed one
//! at a time, each bounded to 64 KiB, and reading stops after the records shown.
use super::sci_common::{self, Cur, latin1};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State, Table};
use std::io::Read;

pub struct Bam;

/// Alignment records read.
const MAX_RECORDS: usize = 10_000;
/// Largest SAM header text and record accepted.
const MAX_HEADER: usize = 64 * 1024 * 1024;
const MAX_RECORD: usize = 64 * 1024 * 1024;
/// Reference sequences listed (all are counted).
const MAX_REFS: usize = 1_000_000;
const EOF_BLOCK: [u8; 28] = [
    0x1f, 0x8b, 0x08, 0x04, 0, 0, 0, 0, 0, 0xff, 0x06, 0, 0x42, 0x43, 0x02, 0, 0x1b, 0, 0x03, 0, 0,
    0, 0, 0, 0, 0, 0, 0,
];

/// Reads a BGZF file block by block.
pub(crate) struct Bgzf<'a> {
    b: &'a [u8],
    pos: usize,
    buf: Vec<u8>,
    at: usize,
    pub blocks: u64,
    pub error: Option<String>,
}

/// Size of the BGZF block starting at `h` (from its `BC` extra field), if it is one.
fn block_size(h: &[u8]) -> Option<usize> {
    if h.len() < 18 || h[..4] != [0x1f, 0x8b, 0x08, 0x04] {
        return None;
    }
    let xlen = u16::from_le_bytes([h[10], h[11]]) as usize;
    let extra = h.get(12..12 + xlen)?;
    let mut c = Cur::new(extra, false);
    while c.left() >= 4 {
        let (si1, si2) = (c.u8()?, c.u8()?);
        let slen = c.u16()? as usize;
        let data = c.take(slen)?;
        if si1 == b'B' && si2 == b'C' && slen == 2 {
            return Some(u16::from_le_bytes([data[0], data[1]]) as usize + 1);
        }
    }
    None
}

impl<'a> Bgzf<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self {
            b,
            pos: 0,
            buf: Vec::new(),
            at: 0,
            blocks: 0,
            error: None,
        }
    }
    /// Decompresses the next block; false at the end or on damage (see `error`).
    fn next_block(&mut self) -> bool {
        if self.error.is_some() {
            return false;
        }
        let h = &self.b[self.pos.min(self.b.len())..];
        if h.is_empty() {
            return false;
        }
        let Some(total) = block_size(h) else {
            self.error = Some(format!("No BGZF block at byte {}", self.pos));
            return false;
        };
        let xlen = u16::from_le_bytes([h[10], h[11]]) as usize;
        if total > h.len() || total < 12 + xlen + 8 {
            self.error = Some(format!("The BGZF block at byte {} is cut", self.pos));
            return false;
        }
        let cdata = &h[12 + xlen..total - 8];
        let isize =
            u32::from_le_bytes([h[total - 4], h[total - 3], h[total - 2], h[total - 1]]) as usize;
        self.buf.clear();
        self.at = 0;
        let read = flate2::read::DeflateDecoder::new(cdata)
            .take(65_537)
            .read_to_end(&mut self.buf);
        if read.is_err() || self.buf.len() != isize || isize > 65_536 {
            self.error = Some(format!("The BGZF block at byte {} is damaged", self.pos));
            return false;
        }
        self.pos += total;
        self.blocks += 1;
        true
    }
    /// Exactly `n` bytes of decompressed data, or `None` at the end (or on damage).
    pub fn take(&mut self, n: usize) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(n.min(1 << 20));
        while out.len() < n {
            if self.at >= self.buf.len() && !self.next_block() {
                return None;
            }
            let k = (n - out.len()).min(self.buf.len() - self.at);
            out.extend_from_slice(&self.buf[self.at..self.at + k]);
            self.at += k;
        }
        Some(out)
    }
    pub fn i32(&mut self) -> Option<i32> {
        self.take(4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// Whether decompressed data remains.
    pub fn more(&mut self) -> bool {
        while self.at >= self.buf.len() {
            if !self.next_block() {
                return false;
            }
        }
        true
    }
}

pub(crate) const FLAGS: [(u16, &str); 12] = [
    (0x1, "paired"),
    (0x2, "proper pair"),
    (0x4, "unmapped"),
    (0x8, "mate unmapped"),
    (0x10, "reverse"),
    (0x20, "mate reverse"),
    (0x40, "first"),
    (0x80, "second"),
    (0x100, "secondary"),
    (0x200, "QC fail"),
    (0x400, "duplicate"),
    (0x800, "supplementary"),
];
pub(crate) fn flag_words(flag: u16) -> String {
    FLAGS
        .iter()
        .filter(|(b, _)| flag & b != 0)
        .map(|(_, w)| *w)
        .collect::<Vec<_>>()
        .join(", ")
}

/// SAM header lines as one table per record type (@HD, @SQ, @RG, @PG, @CO, others).
pub(crate) fn header_tables(text: &str) -> Vec<Table> {
    let mut order: Vec<String> = Vec::new();
    // Per record type: the tags seen (columns) and each line's tag/value pairs.
    type Lines = Vec<Vec<(String, String)>>;
    let mut tables: Vec<(Vec<String>, Lines)> = Vec::new();
    for line in text.lines().take(crate::inspect::MAX_ROWS) {
        let Some(rest) = line.strip_prefix('@') else {
            continue;
        };
        let mut fields = rest.split('\t');
        let kind = fields.next().unwrap_or("").to_owned();
        let pairs: Vec<(String, String)> = if kind == "CO" {
            vec![("Comment".into(), fields.collect::<Vec<_>>().join("\t"))]
        } else {
            fields
                .map(|f| match f.split_once(':') {
                    Some((k, v)) => (k.to_owned(), v.to_owned()),
                    None => (String::new(), f.to_owned()),
                })
                .collect()
        };
        let k = match order.iter().position(|o| *o == kind) {
            Some(k) => k,
            None => {
                order.push(kind.clone());
                tables.push((Vec::new(), Vec::new()));
                order.len() - 1
            }
        };
        let (cols, rows) = &mut tables[k];
        for (key, _) in &pairs {
            if !cols.contains(key) && cols.len() < 32 {
                cols.push(key.clone());
            }
        }
        rows.push(pairs);
    }
    order
        .iter()
        .zip(tables)
        .map(|(kind, (cols, rows))| {
            let name = match kind.as_str() {
                "HD" => "@HD header".to_owned(),
                "SQ" => "@SQ reference sequences".to_owned(),
                "RG" => "@RG read groups".to_owned(),
                "PG" => "@PG programs".to_owned(),
                "CO" => "@CO comments".to_owned(),
                k => format!("@{k} lines"),
            };
            let col_refs: Vec<&str> = cols.iter().map(String::as_str).collect();
            let mut t = sci_common::table(&name, &col_refs);
            for r in rows {
                let cells = cols
                    .iter()
                    .map(|c| {
                        r.iter()
                            .find(|(k, _)| k == c)
                            .map(|(_, v)| sci_common::clip(v, 500))
                            .unwrap_or_default()
                    })
                    .collect();
                sci_common::push(&mut t, crate::inspect::MAX_ROWS, cells, None);
            }
            t
        })
        .collect()
}

/// A header tag of the first line of a type: ("HD", "VN").
pub(crate) fn header_value(text: &str, kind: &str, tag: &str) -> Option<String> {
    let line = text
        .lines()
        .find(|l| l.strip_prefix('@').is_some_and(|r| r.starts_with(kind)))?;
    line.split('\t')
        .skip(1)
        .find_map(|f| f.strip_prefix(tag)?.strip_prefix(':'))
        .map(str::to_owned)
}

fn cigar(ops: &[u8]) -> String {
    let mut s = String::new();
    for c in ops.chunks_exact(4) {
        let v = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        let op = b"MIDNSHP=X"
            .get((v & 0xf) as usize)
            .copied()
            .unwrap_or(b'?');
        s.push_str(&(v >> 4).to_string());
        s.push(op as char);
        if s.len() > 300 {
            s.push('…');
            break;
        }
    }
    if s.is_empty() { "*".into() } else { s }
}

impl Handler for Bam {
    fn name(&self) -> &'static str {
        "BAM"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        block_size(probe.head).is_some()
            && Bgzf::new(probe.head).take(4).as_deref() == Some(&b"BAM\x01"[..])
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let mut z = Bgzf::new(input.bytes);
        if z.take(4).as_deref() != Some(&b"BAM\x01"[..]) {
            return Err(z.error.unwrap_or_else(|| "No BAM signature".into()));
        }
        let mut i = Inspection::new("SAM/BAM", "BAM — sequence alignments");
        let l_text = z.i32().ok_or("The BAM header is cut")?;
        if !(0..=MAX_HEADER as i32).contains(&l_text) {
            return Err(format!("Implausible header length {l_text}"));
        }
        let text_bytes = z
            .take(l_text as usize)
            .ok_or("The SAM header text is cut")?;
        let text = latin1(&text_bytes).trim_end_matches('\0').to_owned();
        let n_ref = z.i32().ok_or("The reference list is cut")?;
        if n_ref < 0 {
            return Err(format!("Implausible reference count {n_ref}"));
        }
        let mut refs: Vec<(String, i64)> = Vec::new();
        let mut refs_table = sci_common::table("Reference sequences", &["Reference", "Length"]);
        let mut ref_problem = None;
        for _ in 0..n_ref {
            if refs.len() >= MAX_REFS {
                ref_problem = Some(format!("only the first {MAX_REFS} references were read"));
                break;
            }
            let Some(l_name) = z.i32().filter(|&l| (1..=65_536).contains(&l)) else {
                ref_problem = Some("the reference list is cut or damaged".into());
                break;
            };
            let (Some(name), Some(len)) = (z.take(l_name as usize), z.i32()) else {
                ref_problem = Some("the reference list is cut".into());
                break;
            };
            let name = latin1(&name).trim_end_matches('\0').to_owned();
            sci_common::push(
                &mut refs_table,
                MAX_REFS,
                vec![name.clone(), len.to_string()],
                None,
            );
            refs.push((name, len as i64));
        }
        let ref_name = |id: i32| -> String {
            if id < 0 {
                "*".into()
            } else {
                refs.get(id as usize)
                    .map(|r| r.0.clone())
                    .unwrap_or_else(|| format!("#{id}"))
            }
        };

        // Alignment records.
        let mut t = sci_common::table(
            "Alignments",
            &[
                "Read",
                "Flag",
                "Flag meaning",
                "Reference",
                "Position",
                "MAPQ",
                "CIGAR",
                "Mate reference",
                "Mate position",
                "Template length",
                "Sequence length",
            ],
        );
        let (mut mapped, mut unmapped, mut dup, mut secondary, mut bad_ref) = (0u64, 0, 0, 0, 0);
        let mut problem: Option<String> = None;
        let mut n = 0usize;
        while n < MAX_RECORDS {
            if n.is_multiple_of(1024) && cancelled() {
                return Err("Cancelled".into());
            }
            if !z.more() {
                break;
            }
            let Some(size) = z.i32() else {
                problem = Some("the last record is cut".into());
                break;
            };
            if !(32..=MAX_RECORD as i32).contains(&size) {
                problem = Some(format!("record {} has an implausible size {size}", n + 1));
                break;
            }
            let Some(rec) = z.take(size as usize) else {
                problem = Some(format!("record {} is cut", n + 1));
                break;
            };
            n += 1;
            let mut c = Cur::new(&rec, false);
            let (ref_id, pos) = (c.i32().unwrap_or(-1), c.i32().unwrap_or(-1));
            let l_name = c.u8().unwrap_or(0) as usize;
            let mapq = c.u8().unwrap_or(255);
            let _bin = c.u16();
            let n_cigar = c.u16().unwrap_or(0) as usize;
            let flag = c.u16().unwrap_or(0);
            let l_seq = c.i32().unwrap_or(0);
            let (next_ref, next_pos, tlen) = (
                c.i32().unwrap_or(-1),
                c.i32().unwrap_or(-1),
                c.i32().unwrap_or(0),
            );
            let name = c
                .take(l_name)
                .map(|s| latin1(s).trim_end_matches('\0').to_owned())
                .unwrap_or_default();
            let cig = c.take(n_cigar * 4).map(cigar).unwrap_or_else(|| "?".into());
            if ref_id >= refs.len() as i32 || next_ref >= refs.len() as i32 {
                bad_ref += 1;
            }
            if flag & 0x4 != 0 {
                unmapped += 1;
            } else {
                mapped += 1;
            }
            if flag & 0x400 != 0 {
                dup += 1;
            }
            if flag & 0x900 != 0 {
                secondary += 1;
            }
            let mate = if next_ref < 0 {
                "*".into()
            } else if next_ref == ref_id {
                "=".into()
            } else {
                ref_name(next_ref)
            };
            sci_common::push(
                &mut t,
                MAX_RECORDS,
                vec![
                    name,
                    flag.to_string(),
                    flag_words(flag),
                    ref_name(ref_id),
                    (pos as i64 + 1).to_string(),
                    mapq.to_string(),
                    cig,
                    mate,
                    (next_pos as i64 + 1).to_string(),
                    tlen.to_string(),
                    l_seq.to_string(),
                ],
                None,
            );
        }
        let more = problem.is_none() && n == MAX_RECORDS && z.more();
        if problem.is_none() {
            problem = z.error.take();
        }

        // Facts.
        let g = "Header";
        i.version = header_value(&text, "HD", "VN").map(|v| format!("SAM {v}"));
        i.fact(
            g,
            "Format version",
            header_value(&text, "HD", "VN").unwrap_or_default(),
            None,
        );
        i.fact(
            g,
            "Sort order",
            header_value(&text, "HD", "SO").unwrap_or_default(),
            None,
        );
        i.fact(
            g,
            "Grouping",
            header_value(&text, "HD", "GO").unwrap_or_default(),
            None,
        );
        i.fact(g, "Reference sequences", n_ref.to_string(), None);
        let total_len: i64 = refs.iter().map(|r| r.1.max(0)).sum();
        if total_len > 0 {
            i.fact(g, "Total reference length", total_len.to_string(), None);
        }
        let count = |k: &str| text.lines().filter(|l| l.starts_with(k)).count();
        if count("@RG") > 0 {
            i.fact(g, "Read groups", count("@RG").to_string(), None);
        }
        if count("@PG") > 0 {
            i.fact(g, "Programs", count("@PG").to_string(), None);
        }
        let g = "Records";
        i.fact(
            g,
            "Records read",
            if more {
                format!("{n} (the file has more)")
            } else {
                n.to_string()
            },
            None,
        );
        i.fact(g, "Mapped (of records read)", mapped.to_string(), None);
        i.fact(g, "Unmapped (of records read)", unmapped.to_string(), None);
        i.fact(g, "Duplicates (of records read)", dup.to_string(), None);
        i.fact(g, "Secondary or supplementary", secondary.to_string(), None);
        i.fact(g, "BGZF blocks decompressed", z.blocks.to_string(), None);

        // Checks.
        match (&problem, &ref_problem, more) {
            (None, None, false) => i.check(
                Stage::Parsed,
                State::Passed,
                format!("Header, {} references and all {n} records read", refs.len()),
            ),
            (None, None, true) => i.check(
                Stage::Parsed,
                State::Partial,
                format!("Header, references and the first {n} records read; the rest of the file was not read"),
            ),
            (p, r, _) => i.check(
                Stage::Parsed,
                State::Partial,
                format!(
                    "{n} records read; {}",
                    p.clone().or_else(|| r.clone()).unwrap_or_default()
                ),
            ),
        }
        let sq: Vec<(String, String)> = text
            .lines()
            .filter(|l| l.starts_with("@SQ"))
            .map(|l| {
                let get = |tag: &str| {
                    l.split('\t')
                        .find_map(|f| f.strip_prefix(tag))
                        .unwrap_or("")
                        .to_owned()
                };
                (get("SN:"), get("LN:"))
            })
            .collect();
        if !sq.is_empty() && ref_problem.is_none() {
            let same = sq.len() == refs.len()
                && sq
                    .iter()
                    .zip(&refs)
                    .all(|((sn, ln), (name, len))| sn == name && *ln == len.to_string());
            if same {
                i.check(
                    Stage::Semantic,
                    State::Passed,
                    "The @SQ header lines match the binary reference list",
                );
            } else {
                i.check(
                    Stage::Semantic,
                    State::Failed,
                    format!(
                        "The @SQ header lines ({}) differ from the binary reference list ({})",
                        sq.len(),
                        refs.len()
                    ),
                );
            }
        }
        if bad_ref > 0 {
            i.check(
                Stage::Semantic,
                State::Failed,
                format!("{bad_ref} record(s) point at a reference that is not listed"),
            );
        } else if n > 0 {
            i.check(
                Stage::Semantic,
                State::Passed,
                "Every record read points at a listed reference",
            );
        }
        if input.complete {
            if input.bytes.ends_with(&EOF_BLOCK) {
                i.check(
                    Stage::Semantic,
                    State::Passed,
                    "BGZF end-of-file marker present",
                );
            } else {
                i.check(
                    Stage::Semantic,
                    State::Failed,
                    "BGZF end-of-file marker missing: the file may be truncated",
                );
            }
        }
        i.not_validated();
        i.notes.push(format!(
            "Only the first {MAX_RECORDS} alignment records are read; sequences, qualities and tags are not shown."
        ));
        i.tables.push(t);
        i.tables.push(refs_table);
        i.tables.extend(header_tables(&text));
        Ok(i)
    }
}
