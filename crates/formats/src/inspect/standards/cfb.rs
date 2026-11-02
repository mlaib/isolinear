//! A small read-only reader for Compound File Binary files ([MS-CFB]), the container of Outlook
//! `.msg`, Excel `.xls`, PowerPoint `.ppt` and Word `.doc`: header, DIFAT, FAT and mini FAT,
//! the directory's red-black tree, and streams. Every chain is followed at most once per sector
//! (loops are reported, not followed forever), sizes are checked against the file, and a cut
//! file gives the parts of streams that exist.
use super::doc_common::{u16le, u32le, u64le, utf16le};
use std::ops::Range;

pub const MAGIC: &[u8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";
const END: u32 = 0xFFFF_FFFE;
const FREE: u32 = 0xFFFF_FFFF;
const NONE: u32 = 0xFFFF_FFFF;
/// Directory entries read at most.
const MAX_ENTRIES: usize = 200_000;
/// Largest stream read.
pub const MAX_STREAM: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Empty,
    Storage,
    Stream,
    Root,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    left: u32,
    right: u32,
    child: u32,
    start: u32,
    pub size: u64,
}

pub struct Cfb<'a> {
    data: &'a [u8],
    shift: u32,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    pub entries: Vec<Entry>,
    mini: Vec<u8>,
    /// File offset of each 64-byte mini sector, when it lies inside one sector of the file.
    mini_offsets: Vec<Option<u64>>,
    cutoff: u64,
    pub version: u16,
    /// Damage found while reading (loops, chains past the end, cut streams).
    pub warnings: Vec<String>,
}

/// File offsets of a stream's units (sectors or mini sectors), to map stream bytes to the file.
pub struct Locator {
    offsets: Vec<Option<u64>>,
    unit: usize,
}
impl Locator {
    /// The file range of `len` stream bytes from `start`, when they are contiguous in the file.
    pub fn range(&self, start: usize, len: usize) -> Option<Range<u64>> {
        let first = start / self.unit;
        let last = (start + len.max(1) - 1) / self.unit;
        let base = (*self.offsets.get(first)?)?;
        for i in first..last {
            let (a, b) = ((*self.offsets.get(i)?)?, (*self.offsets.get(i + 1)?)?);
            if b != a + self.unit as u64 {
                return None;
            }
        }
        let s = base + (start % self.unit) as u64;
        Some(s..s + len as u64)
    }
}

impl<'a> Cfb<'a> {
    pub fn open(data: &'a [u8]) -> Result<Self, String> {
        if !data.starts_with(MAGIC) {
            return Err("Not a Compound File (OLE2) container".into());
        }
        if data.len() < 512 {
            return Err("The Compound File header is cut short".into());
        }
        let version = u16le(data, 0x1A);
        let shift = u32::from(u16le(data, 0x1E));
        if shift != 9 && shift != 12 {
            return Err(format!("Unsupported sector size 2^{shift}"));
        }
        if u16le(data, 0x20) != 6 {
            return Err("Unsupported mini sector size".into());
        }
        let mut cfb = Cfb {
            data,
            shift,
            fat: vec![],
            minifat: vec![],
            entries: vec![],
            mini: vec![],
            mini_offsets: vec![],
            cutoff: u64::from(u32le(data, 0x38)).max(1),
            version,
            warnings: vec![],
        };
        let sector = cfb.sector_size();
        let max_sectors = data.len() / sector + 1;
        // DIFAT: 109 entries in the header, then a chain of DIFAT sectors.
        let mut fat_sectors: Vec<u32> = (0..109)
            .map(|i| u32le(data, 0x4C + i * 4))
            .take_while(|&s| s != FREE)
            .collect();
        let mut difat = u32le(data, 0x44);
        let mut seen = vec![false; max_sectors];
        let mut steps = 0usize;
        while difat != END && difat != FREE {
            steps += 1;
            let Some(slot) = seen.get_mut(difat as usize) else {
                cfb.warnings
                    .push("The DIFAT chain points past the end of the file".into());
                break;
            };
            if *slot || steps > max_sectors {
                cfb.warnings
                    .push("The DIFAT chain loops; it was cut".into());
                break;
            }
            *slot = true;
            let Some(s) = cfb.sector(difat) else {
                cfb.warnings
                    .push("A DIFAT sector is missing (cut file)".into());
                break;
            };
            let per = sector / 4 - 1;
            fat_sectors.extend((0..per).map(|i| u32le(s, i * 4)).filter(|&v| v != FREE));
            difat = u32le(s, per * 4);
        }
        fat_sectors.truncate(max_sectors);
        for &fs in &fat_sectors {
            match cfb.sector(fs) {
                Some(s) => cfb.fat.extend((0..sector / 4).map(|i| u32le(s, i * 4))),
                None => {
                    cfb.warnings
                        .push("A FAT sector lies past the end of the file (cut file)".into());
                    break;
                }
            }
        }
        if cfb.fat.is_empty() {
            return Err("The Compound File has no allocation table".into());
        }
        // Directory.
        let (dir, _) = cfb.chain_bytes(u32le(data, 0x30), u64::MAX, "directory");
        for e in dir.chunks_exact(128).take(MAX_ENTRIES) {
            let len = (u16le(e, 0x40) as usize).min(64);
            let kind = match e[0x42] {
                1 => Kind::Storage,
                2 => Kind::Stream,
                5 => Kind::Root,
                _ => Kind::Empty,
            };
            let mut size = u64le(e, 0x78);
            if version == 3 {
                size &= 0xFFFF_FFFF;
            }
            cfb.entries.push(Entry {
                name: utf16le(&e[..len.saturating_sub(2)]),
                kind,
                left: u32le(e, 0x44),
                right: u32le(e, 0x48),
                child: u32le(e, 0x4C),
                start: u32le(e, 0x74),
                size,
            });
        }
        if cfb.entries.first().map(|e| e.kind) != Some(Kind::Root) {
            return Err("The Compound File directory has no root entry".into());
        }
        // Mini FAT and the mini stream (kept by the root entry).
        let (minifat, _) = cfb.chain_bytes(u32le(data, 0x3C), u64::MAX, "mini FAT");
        cfb.minifat = minifat.chunks_exact(4).map(|c| u32le(c, 0)).collect();
        let root = cfb.entries[0].clone();
        if root.size > 0 {
            let (mini, cut) = cfb.chain_bytes(root.start, root.size.min(MAX_STREAM), "mini stream");
            if cut {
                cfb.warnings.push("The mini stream is cut short".into());
            }
            let locator = cfb.fat_locator(root.start);
            cfb.mini_offsets = (0..mini.len().div_ceil(64))
                .map(|m| locator.range(m * 64, 64).map(|r| r.start))
                .collect();
            cfb.mini = mini;
        }
        Ok(cfb)
    }

    fn sector_size(&self) -> usize {
        1 << self.shift
    }

    fn sector(&self, s: u32) -> Option<&'a [u8]> {
        if s > 0xFFFF_FFFA {
            return None;
        }
        let size = self.sector_size();
        let start = (s as usize).checked_add(1)?.checked_mul(size)?;
        self.data.get(start..start.checked_add(size)?)
    }

    /// The sectors of a FAT chain, stopping at a loop or a pointer out of the table.
    fn chain(&self, start: u32, table: &[u32], what: &str) -> (Vec<u32>, Option<String>) {
        let mut out = vec![];
        let mut seen = vec![false; table.len()];
        let mut s = start;
        while s != END {
            if s == FREE && out.is_empty() {
                return (out, None);
            }
            let Some(flag) = seen.get_mut(s as usize) else {
                return (
                    out,
                    Some(format!(
                        "The {what} chain points outside the allocation table"
                    )),
                );
            };
            if *flag {
                return (
                    out,
                    Some(format!("The {what} chain loops back on itself; it was cut")),
                );
            }
            *flag = true;
            out.push(s);
            s = table[s as usize];
        }
        (out, None)
    }

    /// Bytes of a FAT chain, at most `size`; `true` when the chain ended before `size`.
    fn chain_bytes(&mut self, start: u32, size: u64, what: &str) -> (Vec<u8>, bool) {
        let (sectors, problem) = self.chain(start, &self.fat, what);
        if let Some(p) = problem {
            self.warnings.push(p);
        }
        let mut out = vec![];
        for s in sectors {
            if out.len() as u64 >= size {
                break;
            }
            match self.sector(s) {
                Some(bytes) => out.extend_from_slice(bytes),
                None => {
                    self.warnings
                        .push(format!("The {what} runs past the end of the file"));
                    break;
                }
            }
        }
        let cut = size != u64::MAX && (out.len() as u64) < size;
        if (out.len() as u64) > size {
            out.truncate(size as usize);
        }
        (out, cut)
    }

    fn fat_locator(&self, start: u32) -> Locator {
        let (sectors, _) = self.chain(start, &self.fat, "stream");
        let size = self.sector_size();
        Locator {
            offsets: sectors
                .iter()
                .map(|&s| self.sector(s).map(|_| ((s as u64) + 1) * size as u64))
                .collect(),
            unit: size,
        }
    }

    fn in_mini(&self, e: &Entry) -> bool {
        e.kind != Kind::Root && e.size < self.cutoff
    }

    /// Where a stream's bytes are in the file.
    pub fn locator(&self, index: usize) -> Locator {
        let Some(e) = self.entries.get(index) else {
            return Locator {
                offsets: vec![],
                unit: 64,
            };
        };
        if self.in_mini(e) {
            let (chain, _) = self.chain(e.start, &self.minifat, "mini");
            Locator {
                offsets: chain
                    .iter()
                    .map(|&m| self.mini_offsets.get(m as usize).copied().flatten())
                    .collect(),
                unit: 64,
            }
        } else {
            self.fat_locator(e.start)
        }
    }

    /// A stream's bytes, and whether it is cut short (damaged or truncated file).
    pub fn read(&self, index: usize) -> Result<(Vec<u8>, bool), String> {
        let e = self.entries.get(index).ok_or("No such stream")?;
        if e.kind != Kind::Stream {
            return Err(format!("{} is not a stream", e.name));
        }
        if e.size > MAX_STREAM {
            return Err(format!(
                "The stream {} is {} MiB; streams are read up to {} MiB",
                e.name,
                e.size >> 20,
                MAX_STREAM >> 20
            ));
        }
        let size = e.size as usize;
        let mut out = Vec::with_capacity(size.min(self.data.len()));
        if self.in_mini(e) {
            let (chain, _) = self.chain(e.start, &self.minifat, "mini");
            for m in chain {
                if out.len() >= size {
                    break;
                }
                let at = m as usize * 64;
                match self.mini.get(at..at + 64) {
                    Some(b) => out.extend_from_slice(b),
                    None => break,
                }
            }
        } else {
            let (chain, _) = self.chain(e.start, &self.fat, "stream");
            for s in chain {
                if out.len() >= size {
                    break;
                }
                match self.sector(s) {
                    Some(b) => out.extend_from_slice(b),
                    None => break,
                }
            }
        }
        let cut = out.len() < size;
        out.truncate(size);
        Ok((out, cut))
    }

    /// Children of a storage, in name order, each entry visited once (loops are skipped).
    pub fn children(&self, index: usize) -> Vec<usize> {
        let Some(e) = self.entries.get(index) else {
            return vec![];
        };
        let mut out = vec![];
        let mut seen = vec![false; self.entries.len()];
        let mut stack: Vec<u32> = vec![];
        let mut node = e.child;
        loop {
            while node != NONE && (node as usize) < self.entries.len() && !seen[node as usize] {
                seen[node as usize] = true;
                stack.push(node);
                node = self.entries[node as usize].left;
            }
            let Some(n) = stack.pop() else {
                break;
            };
            out.push(n as usize);
            node = self.entries[n as usize].right;
        }
        out
    }

    /// The child of `parent` called `name` (case-insensitive).
    pub fn child(&self, parent: usize, name: &str) -> Option<usize> {
        self.children(parent)
            .into_iter()
            .find(|&i| self.entries[i].name.eq_ignore_ascii_case(name))
    }
}

/// Whether the head of a file shows a Compound File stream named `name` in its directory
/// (names are stored as UTF-16LE).
pub fn head_has_name(head: &[u8], name: &str) -> bool {
    let needle: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    head.starts_with(MAGIC) && memchr::memmem::find(head, &needle).is_some()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A version 3 Compound File with the given streams (all in the root storage, each stored
    /// in regular sectors when `regular` or in the mini stream otherwise).
    pub fn build(streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let sec = 512usize;
        let mut sectors: Vec<Vec<u8>> = vec![];
        let mut fat: Vec<u32> = vec![];
        let mut entries: Vec<(String, u8, u32, u64)> = vec![];
        let alloc = |data: &[u8], sectors: &mut Vec<Vec<u8>>, fat: &mut Vec<u32>| -> u32 {
            let start = sectors.len() as u32;
            let n = data.len().div_ceil(sec).max(1);
            for i in 0..n {
                let mut s = data
                    .get(i * sec..((i + 1) * sec).min(data.len()))
                    .unwrap_or(&[])
                    .to_vec();
                s.resize(sec, 0);
                sectors.push(s);
                fat.push(if i + 1 == n {
                    END
                } else {
                    start + i as u32 + 1
                });
            }
            start
        };
        for (name, data) in streams {
            let start = alloc(data, &mut sectors, &mut fat);
            entries.push((name.to_string(), 2, start, data.len() as u64));
        }
        // Directory: root + streams as a right-leaning chain.
        let mut dir = vec![];
        let n = entries.len();
        let mut entry =
            |name: &str, kind: u8, left: u32, right: u32, child: u32, start: u32, size: u64| {
                let mut e = vec![0u8; 128];
                let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
                e[..units.len()].copy_from_slice(&units);
                e[0x40..0x42].copy_from_slice(&((units.len() + 2) as u16).to_le_bytes());
                e[0x42] = kind;
                e[0x44..0x48].copy_from_slice(&left.to_le_bytes());
                e[0x48..0x4C].copy_from_slice(&right.to_le_bytes());
                e[0x4C..0x50].copy_from_slice(&child.to_le_bytes());
                e[0x74..0x78].copy_from_slice(&start.to_le_bytes());
                e[0x78..0x80].copy_from_slice(&size.to_le_bytes());
                dir.extend(e);
            };
        entry(
            "Root Entry",
            5,
            NONE,
            NONE,
            if n > 0 { 1 } else { NONE },
            END,
            0,
        );
        for (i, (name, kind, start, size)) in entries.iter().enumerate() {
            let right = if i + 1 < n { i as u32 + 2 } else { NONE };
            entry(name, *kind, NONE, right, NONE, *start, *size);
        }
        let dir_start = alloc(&dir, &mut sectors, &mut fat);
        let fat_index = sectors.len() as u32;
        fat.push(0xFFFF_FFFD);
        let mut fat_bytes: Vec<u8> = fat.iter().flat_map(|v| v.to_le_bytes()).collect();
        fat_bytes.resize(sec, 0xFF);
        sectors.push(fat_bytes);
        let mut header = vec![0u8; 512];
        header[..8].copy_from_slice(MAGIC);
        header[0x18..0x1A].copy_from_slice(&0x3Eu16.to_le_bytes());
        header[0x1A..0x1C].copy_from_slice(&3u16.to_le_bytes());
        header[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
        header[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes());
        header[0x20..0x22].copy_from_slice(&6u16.to_le_bytes());
        header[0x2C..0x30].copy_from_slice(&1u32.to_le_bytes());
        header[0x30..0x34].copy_from_slice(&dir_start.to_le_bytes());
        // Everything in regular sectors: a cutoff of 0 keeps the mini stream out.
        header[0x38..0x3C].copy_from_slice(&0u32.to_le_bytes());
        header[0x3C..0x40].copy_from_slice(&END.to_le_bytes());
        header[0x44..0x48].copy_from_slice(&END.to_le_bytes());
        for i in 0..109 {
            let v = if i == 0 { fat_index } else { FREE };
            header[0x4C + i * 4..0x50 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        let mut out = header;
        for s in sectors {
            out.extend(s);
        }
        out
    }

    #[test]
    fn streams_children_and_loops() {
        let big = vec![7u8; 1500];
        let file = build(&[("Alpha", b"hello".to_vec()), ("Beta", big.clone())]);
        let cfb = Cfb::open(&file).unwrap();
        let kids = cfb.children(0);
        assert_eq!(kids.len(), 2);
        let beta = cfb.child(0, "beta").unwrap();
        assert_eq!(cfb.read(beta).unwrap(), (big, false));
        let alpha = cfb.child(0, "ALPHA").unwrap();
        let span = cfb.locator(alpha).range(0, 5).unwrap();
        assert_eq!(&file[span.start as usize..span.end as usize], b"hello");
        // A FAT whose chain loops: reading stops, nothing spins.
        let mut looped = file.clone();
        // Beta starts at sector 1 and has 3 sectors (1, 2, 3); make sector 2 point back to 1.
        let fat_at = 512 + 512 * 5;
        looped[fat_at + 2 * 4..fat_at + 3 * 4].copy_from_slice(&1u32.to_le_bytes());
        let cfb = Cfb::open(&looped).unwrap();
        let (data, cut) = cfb.read(cfb.child(0, "Beta").unwrap()).unwrap();
        assert!(cut && data.len() == 1024);
        // Directory entries whose tree points back at itself are listed once.
        let mut cyclic = file.clone();
        let dir_at = 512 + 512 * 4;
        cyclic[dir_at + 256 + 0x48..dir_at + 256 + 0x4C].copy_from_slice(&1u32.to_le_bytes());
        let cfb = Cfb::open(&cyclic).unwrap();
        assert_eq!(cfb.children(0).len(), 2);
        assert!(Cfb::open(&file[..300]).is_err());
        assert!(Cfb::open(b"not a cfb").is_err());
    }
}
