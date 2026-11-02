//! CRAM alignments (reference-based compression): the file definition, the SAM header from
//! the first container (raw or gzip-compressed) and the container headers. Records are not
//! decoded: that needs the reference sequences, which Isolinear never fetches.
use super::bam::{header_tables, header_value};
use super::sci_common::{self, Cur, latin1};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};

pub struct Cram;

const MAX_HEADER: usize = 64 * 1024 * 1024;
/// Container headers listed (all are counted).
const MAX_CONTAINERS: usize = 100_000;

/// CRAM's variable-length 32-bit integer.
fn itf8(c: &mut Cur) -> Option<i32> {
    let b0 = c.u8()? as u32;
    let v = if b0 & 0x80 == 0 {
        b0
    } else if b0 & 0x40 == 0 {
        (b0 & 0x7f) << 8 | c.u8()? as u32
    } else if b0 & 0x20 == 0 {
        (b0 & 0x3f) << 16 | (c.u8()? as u32) << 8 | c.u8()? as u32
    } else if b0 & 0x10 == 0 {
        (b0 & 0x1f) << 24 | (c.u8()? as u32) << 16 | (c.u8()? as u32) << 8 | c.u8()? as u32
    } else {
        (b0 & 0x0f) << 28
            | (c.u8()? as u32) << 20
            | (c.u8()? as u32) << 12
            | (c.u8()? as u32) << 4
            | (c.u8()? as u32 & 0x0f)
    };
    Some(v as i32)
}

/// CRAM's variable-length 64-bit integer.
fn ltf8(c: &mut Cur) -> Option<i64> {
    let b0 = c.u8()?;
    let n = b0.leading_ones() as usize;
    // The bits of the first byte after its leading ones (none for 7 or 8 extra bytes).
    let mut v: u64 = if n >= 7 { 0 } else { (b0 & (0x7f >> n)) as u64 };
    for _ in 0..n {
        v = v << 8 | c.u8()? as u64;
    }
    Some(v as i64)
}

struct Container {
    offset: usize,
    length: usize,
    ref_id: i32,
    start: i32,
    span: i32,
    records: i32,
    bases: i64,
    blocks: i32,
    /// First byte after the header.
    data: usize,
}

fn container(b: &[u8], at: usize, major: u8, minor: u8) -> Option<Container> {
    let mut c = Cur::at(b, at, false);
    let length = c.i32()?;
    if length < 0 {
        return None;
    }
    let ref_id = itf8(&mut c)?;
    let start = itf8(&mut c)?;
    let span = itf8(&mut c)?;
    let records = itf8(&mut c)?;
    let modern = major >= 3 || (major == 2 && minor >= 1);
    let _counter = if modern {
        ltf8(&mut c)?
    } else {
        itf8(&mut c)? as i64
    };
    let bases = if modern { ltf8(&mut c)? } else { 0 };
    let blocks = itf8(&mut c)?;
    let landmarks = itf8(&mut c)?;
    if !(0..=1_000_000).contains(&landmarks) {
        return None;
    }
    for _ in 0..landmarks {
        itf8(&mut c)?;
    }
    if major >= 3 {
        c.u32()?;
    }
    Some(Container {
        offset: at,
        length: length as usize,
        ref_id,
        start,
        span,
        records,
        bases,
        blocks,
        data: c.at,
    })
}

/// The SAM header text in the first block of the header container.
fn header_text(
    b: &[u8],
    at: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<(String, &'static str), String> {
    let mut c = Cur::at(b, at, false);
    let method = c.u8().ok_or("The header block is cut")?;
    let _content_type = c.u8().ok_or("The header block is cut")?;
    let _content_id = itf8(&mut c).ok_or("The header block is cut")?;
    let size = itf8(&mut c).ok_or("The header block is cut")?;
    let raw = itf8(&mut c).ok_or("The header block is cut")?;
    if size < 0 || raw < 0 || raw as usize > MAX_HEADER {
        return Err("The header block sizes are implausible".into());
    }
    let data = c.take(size as usize).ok_or("The header block is cut")?;
    let (plain, how) = match method {
        0 => (data.to_vec(), "uncompressed"),
        1 => {
            let out = sci_common::gunzip(data, MAX_HEADER, cancelled);
            if let Some(e) = out.error {
                return Err(format!("The gzip-compressed header is damaged: {e}"));
            }
            (out.bytes, "gzip")
        }
        2 => return Err("The header is bzip2-compressed, which is not read here".into()),
        3 => return Err("The header is LZMA-compressed, which is not read here".into()),
        m => {
            return Err(format!(
                "The header uses compression method {m}, which is not read here"
            ));
        }
    };
    let mut h = Cur::new(&plain, false);
    let len = h.i32().ok_or("The header text is cut")?;
    let text = h
        .take(len.max(0) as usize)
        .map(latin1)
        .unwrap_or_else(|| latin1(&plain[4.min(plain.len())..]));
    Ok((text.trim_end_matches('\0').to_owned(), how))
}

impl Handler for Cram {
    fn name(&self) -> &'static str {
        "CRAM"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        probe.head.starts_with(b"CRAM")
            && probe.head.len() >= 26
            && (1..=4).contains(&probe.head[4])
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        if b.len() < 26 {
            return Err("The file definition is cut".into());
        }
        let (major, minor) = (b[4], b[5]);
        let mut i = Inspection::new(
            "SAM/BAM/CRAM",
            "CRAM — reference-compressed sequence alignments",
        );
        i.version = Some(format!("{major}.{minor}"));
        i.fact(
            "File",
            "CRAM version",
            format!("{major}.{minor}"),
            Some(4..6),
        );
        i.fact("File", "File ID", sci_common::field(&b[6..26]), Some(6..26));
        i.notes.push(
            "Decoding CRAM records needs the reference sequences they were compressed against (identified by the M5 checksums and UR locations in @SQ). Isolinear never downloads them, and record decoding is not implemented: only the header and the container layout are shown."
                .into(),
        );
        if major >= 4 {
            i.check(
                Stage::Parsed,
                State::Partial,
                "Only the file definition was read; the CRAM 4 container layout is not read",
            );
            i.not_validated();
            return Ok(i);
        }
        let first = container(b, 26, major, minor).ok_or("The first container header is cut")?;
        let mut problem = None;
        match header_text(b, first.data, cancelled) {
            Ok((text, how)) => {
                i.fact("Header", "Header compression", how, None);
                i.fact(
                    "Header",
                    "Format version",
                    header_value(&text, "HD", "VN").unwrap_or_default(),
                    None,
                );
                i.fact(
                    "Header",
                    "Sort order",
                    header_value(&text, "HD", "SO").unwrap_or_default(),
                    None,
                );
                let sq = text.lines().filter(|l| l.starts_with("@SQ")).count();
                i.fact("Header", "Reference sequences", sq.to_string(), None);
                let md5 = text
                    .lines()
                    .filter(|l| l.starts_with("@SQ") && l.contains("\tM5:"))
                    .count();
                if sq > 0 {
                    i.fact(
                        "Header",
                        "References with an MD5 checksum",
                        md5.to_string(),
                        None,
                    );
                }
                i.tables.extend(header_tables(&text));
            }
            Err(e) => problem = Some(e),
        }
        // Container headers, without decoding their blocks.
        let mut t = sci_common::table(
            "Containers",
            &[
                "Offset",
                "Reference ID",
                "Start",
                "Span",
                "Records",
                "Bases",
                "Blocks",
                "Bytes",
            ],
        );
        let mut at = first.data.saturating_add(first.length);
        let (mut containers, mut records, mut eof) = (0u64, 0i64, false);
        while at < b.len() {
            if containers.is_multiple_of(4096) && cancelled() {
                return Err("Cancelled".into());
            }
            let Some(c) = container(b, at, major, minor) else {
                problem.get_or_insert(format!(
                    "The container header at byte {at} is cut or damaged"
                ));
                break;
            };
            let end = c.data.checked_add(c.length);
            containers += 1;
            records += c.records.max(0) as i64;
            let is_eof = c.records == 0 && c.ref_id == -1 && c.start == 4_542_278;
            eof |= is_eof;
            let ref_text = match c.ref_id {
                -1 => "unmapped".to_owned(),
                -2 => "multiple".to_owned(),
                r => r.to_string(),
            };
            sci_common::push(
                &mut t,
                MAX_CONTAINERS,
                vec![
                    c.offset.to_string(),
                    if is_eof {
                        "end of file".into()
                    } else {
                        ref_text
                    },
                    c.start.to_string(),
                    c.span.to_string(),
                    c.records.to_string(),
                    c.bases.to_string(),
                    c.blocks.to_string(),
                    c.length.to_string(),
                ],
                Some(c.offset as u64..end.unwrap_or(b.len()).min(b.len()) as u64),
            );
            match end {
                Some(e) if e <= b.len() => at = e,
                _ => {
                    problem.get_or_insert(format!("The container at byte {} is cut", c.offset));
                    break;
                }
            }
        }
        i.fact(
            "Containers",
            "Data containers",
            containers.saturating_sub(u64::from(eof)).to_string(),
            None,
        );
        i.fact(
            "Containers",
            "Records (from container headers)",
            records.to_string(),
            None,
        );
        i.tables.push(t);
        match problem {
            None => i.check(
                Stage::Parsed,
                State::Partial,
                "File definition, SAM header and container headers read; records not decoded",
            ),
            Some(p) => i.check(
                Stage::Parsed,
                State::Partial,
                format!("Records not decoded; {p}"),
            ),
        }
        if major >= 3 && input.complete {
            if eof {
                i.check(
                    Stage::Semantic,
                    State::Passed,
                    "End-of-file container present",
                );
            } else {
                i.check(
                    Stage::Semantic,
                    State::Failed,
                    "End-of-file container missing: the file may be truncated",
                );
            }
        }
        i.not_validated();
        Ok(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn variable_integers() {
        let mut c = Cur::new(&[0x05, 0x81, 0x00, 0xC1, 0x02, 0x03], false);
        assert_eq!(itf8(&mut c), Some(5));
        assert_eq!(itf8(&mut c), Some(0x100));
        assert_eq!(itf8(&mut c), Some(0x010203));
        let mut c = Cur::new(&[0x7f, 0x80, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 1], false);
        assert_eq!(ltf8(&mut c), Some(0x7f));
        assert_eq!(ltf8(&mut c), Some(0xff));
        assert_eq!(ltf8(&mut c), Some(1));
    }
}
