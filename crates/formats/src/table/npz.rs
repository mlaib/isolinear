//! ZIP members exposed as NumPy tables. Stored arrays remain file ranges; a selected
//! deflated member is decoded once into bounded memory, never into a temporary file.
use super::{Schema, TableInfo, TableSource, cut, npy::Npy};
use std::{
    fs::File,
    io::{self, Cursor, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub const READY: bool = true;
const MAX_DIRECTORY: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 200_000;
const MEMBER_MAX: u64 = 256 * 1024 * 1024;

pub fn open(path: &Path, table: Option<&str>) -> Result<Box<dyn TableSource>, String> {
    open_with_cancel(path, table, Arc::new(AtomicBool::new(false)))
}
pub fn open_with_cancel(
    path: &Path,
    table: Option<&str>,
    cancelled: Arc<AtomicBool>,
) -> Result<Box<dyn TableSource>, String> {
    Npz::open(path, table, cancelled).map(|r| Box::new(r) as Box<dyn TableSource>)
}

struct Npz {
    array: Npy,
    schema: Schema,
    file: File,
    stamp: std::fs::Metadata,
    cancelled: Arc<AtomicBool>,
}
impl Npz {
    fn open(path: &Path, table: Option<&str>, cancelled: Arc<AtomicBool>) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let stamp = file.metadata().map_err(|e| e.to_string())?;
        let mut source = Range {
            file: file.try_clone().map_err(|e| e.to_string())?,
            start: 0,
            len: stamp.len(),
            pos: 0,
            cancelled: cancelled.clone(),
        };
        let listing = list_zip(&mut source, &|| cancelled.load(Ordering::Relaxed))
            .map_err(|e| e.to_string())?;
        let members: Vec<_> = listing
            .entries
            .into_iter()
            .filter(|m| m.name.ends_with(".npy"))
            .collect();
        let names: Vec<_> = members
            .iter()
            .map(|m| m.name.strip_suffix(".npy").unwrap_or(&m.name).to_owned())
            .collect();
        let index = match table {
            Some(name) => names
                .iter()
                .position(|n| n == name)
                .ok_or_else(|| format!("The archive has no array named {name:?}"))?,
            None => 0,
        };
        let member = members.get(index).ok_or("The archive holds no arrays")?;
        if !member.openable {
            return Err("The array is encrypted, is a link, or uses a ZIP method other than stored or deflate".into());
        }
        let start = member_start(&mut source, member).map_err(|e| e.to_string())?;
        let range = Range {
            file: source.file,
            start,
            len: member.packed.unwrap_or(0),
            pos: 0,
            cancelled: cancelled.clone(),
        };
        let array = if member.method_id == 0 {
            if member.packed != Some(member.size) {
                return Err("Stored array size differs from its packed size".into());
            }
            Npy::from_reader(names[index].clone(), range)?
        } else {
            if member.size > MEMBER_MAX {
                return Err("Compressed NumPy arrays larger than 256 MiB are not opened".into());
            }
            let mut data = Vec::new();
            flate2::read::DeflateDecoder::new(range)
                .take(member.size + 1)
                .read_to_end(&mut data)
                .map_err(|e| e.to_string())?;
            if data.len() as u64 != member.size {
                return Err(
                    "The array's decompressed size does not match its ZIP declaration".into(),
                );
            }
            let mut crc = flate2::Crc::new();
            crc.update(&data);
            if member.crc != Some(crc.sum()) {
                return Err("The array's CRC-32 checksum does not match".into());
            }
            Npy::from_reader(
                names[index].clone(),
                Memory {
                    bytes: Cursor::new(data),
                    cancelled: cancelled.clone(),
                },
            )?
        };
        let mut schema = array.schema().clone();
        schema.table = index;
        if let Some(n) = listing.declared {
            schema
                .details
                .push(("Archive entries".into(), n.to_string()));
        }
        schema.tables = names
            .into_iter()
            .enumerate()
            .map(|(i, name)| TableInfo {
                name,
                rows: if i == index { schema.rows } else { None },
            })
            .collect();
        schema
            .details
            .push(("Member".into(), cut(member.name.clone())));
        schema.details.push((
            "Compression".into(),
            if member.method_id == 0 {
                "stored"
            } else {
                "deflate"
            }
            .into(),
        ));
        if !listing.comment.is_empty() {
            schema
                .details
                .push(("Archive comment".into(), cut(listing.comment)));
        }
        schema.details.extend(
            listing
                .warnings
                .into_iter()
                .map(|s| ("Warning".into(), cut(s))),
        );
        Ok(Self {
            array,
            schema,
            file,
            stamp,
            cancelled,
        })
    }
}
impl TableSource for Npz {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn rows(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let stamp = self.file.metadata().map_err(|e| e.to_string())?;
        if stamp.len() != self.stamp.len() || stamp.modified().ok() != self.stamp.modified().ok() {
            return Err("The NumPy archive changed; reopen it before reading more rows".into());
        }
        self.array.rows(start, count)
    }
}

struct Memory {
    bytes: Cursor<Vec<u8>>,
    cancelled: Arc<AtomicBool>,
}
impl Read for Memory {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::other("cancelled"));
        }
        self.bytes.read(out)
    }
}
impl Seek for Memory {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::other("cancelled"));
        }
        self.bytes.seek(from)
    }
}

/// A seekable view of one stored member. No read can cross its ZIP data range.
struct Range {
    file: File,
    start: u64,
    len: u64,
    pos: u64,
    cancelled: Arc<AtomicBool>,
}
impl Read for Range {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::other("cancelled"));
        }
        let n = (out.len() as u64)
            .min(self.len.saturating_sub(self.pos))
            .min(64 * 1024) as usize;
        if n == 0 {
            return Ok(0);
        }
        self.file.seek(SeekFrom::Start(
            self.start
                .checked_add(self.pos)
                .ok_or_else(|| invalid("member offset overflows"))?,
        ))?;
        let n = self.file.read(&mut out[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
}
impl Seek for Range {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::other("cancelled"));
        }
        let at = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.pos) + i128::from(n),
            SeekFrom::End(n) => i128::from(self.len) + i128::from(n),
        };
        self.pos =
            u64::try_from(at).map_err(|_| invalid("member seek is outside its address range"))?;
        Ok(self.pos)
    }
}

fn member_start(file: &mut (impl Read + Seek), member: &Member) -> io::Result<u64> {
    let size = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(member.offset))?;
    let mut header = [0; 30];
    file.read_exact(&mut header)?;
    if !header.starts_with(b"PK\x03\x04")
        || u16le(&header, 8) != member.method_id
        || u16le(&header, 6) & 0x41 != 0
    {
        return Err(invalid(
            "The array's ZIP local header is missing or disagrees with the directory",
        ));
    }
    let start = member
        .offset
        .checked_add(30 + u16le(&header, 26) as u64 + u16le(&header, 28) as u64)
        .ok_or_else(|| invalid("member offset overflows"))?;
    if start
        .checked_add(member.packed.unwrap_or(0))
        .is_none_or(|end| end > size)
    {
        return Err(invalid("The array runs past the end of the archive"));
    }
    Ok(start)
}

struct Member {
    name: String,
    size: u64,
    packed: Option<u64>,
    offset: u64,
    crc: Option<u32>,
    method_id: u16,
    openable: bool,
}
#[derive(Default)]
struct Listing {
    entries: Vec<Member>,
    declared: Option<u64>,
    truncated: bool,
    comment: String,
    warnings: Vec<String>,
}
impl Listing {
    fn total_size(&self) -> u64 {
        self.entries
            .iter()
            .map(|e| e.size)
            .fold(0, u64::saturating_add)
    }
}
fn invalid(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s.to_owned())
}

fn u16le(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .unwrap_or(0)
}
fn u32le(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .unwrap_or(0)
}
fn u64le(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8)
        .and_then(|s| s.try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0)
}

fn list_zip(file: &mut (impl Read + Seek), cancelled: &dyn Fn() -> bool) -> io::Result<Listing> {
    let len = file.seek(SeekFrom::End(0))?;
    let tail_len = len.min(65_557);
    file.seek(SeekFrom::Start(len - tail_len))?;
    let mut tail = vec![0; tail_len as usize];
    file.read_exact(&mut tail)?;
    let eocd = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| {
            tail[i..].starts_with(b"PK\x05\x06")
                && i + 22 + u16le(&tail, i + 20) as usize == tail.len()
        })
        .ok_or_else(|| invalid("no complete ZIP end-of-directory record"))?;
    let e = &tail[eocd..];
    if u16le(e, 4) != 0 || u16le(e, 6) != 0 || u16le(e, 8) != u16le(e, 10) {
        return Err(invalid("multi-disk ZIP archives are not supported"));
    }
    let mut declared = u16le(e, 10) as u64;
    let mut size = u32le(e, 12) as u64;
    let mut offset = u32le(e, 16) as u64;
    let comment = String::from_utf8_lossy(&e[22..]).into_owned();
    let end_offset = len - tail_len + eocd as u64;
    let mut directory_end = end_offset;
    if declared == 0xffff || size == 0xffff_ffff || offset == 0xffff_ffff {
        let locator_offset = end_offset.saturating_sub(20);
        file.seek(SeekFrom::Start(locator_offset))?;
        let mut locator = [0; 20];
        file.read_exact(&mut locator)?;
        // Exactly 65,535 entries can still use the classic end record.
        if !locator.starts_with(b"PK\x06\x07") {
            if size == 0xffff_ffff || offset == 0xffff_ffff {
                return Err(invalid("the ZIP64 locator is missing"));
            }
        } else {
            if u32le(&locator, 4) != 0 || u32le(&locator, 16) != 1 {
                return Err(invalid("expected a single-disk ZIP64 locator"));
            }
            let record = u64le(&locator, 8);
            if record
                .checked_add(56)
                .is_none_or(|end| end > locator_offset)
            {
                return Err(invalid("the ZIP64 record points outside the file"));
            }
            let mut z = [0u8; 56];
            file.seek(SeekFrom::Start(record))?;
            file.read_exact(&mut z)?;
            if !z.starts_with(b"PK\x06\x06")
                || u64le(&z, 4) < 44
                || record
                    .checked_add(12)
                    .and_then(|n| n.checked_add(u64le(&z, 4)))
                    .is_none_or(|end| end > locator_offset)
            {
                return Err(invalid("broken ZIP64 end record"));
            }
            if u32le(&z, 16) != 0 || u32le(&z, 20) != 0 || u64le(&z, 24) != u64le(&z, 32) {
                return Err(invalid("multi-disk ZIP64 archives are not supported"));
            }
            declared = u64le(&z, 32);
            size = u64le(&z, 40);
            offset = u64le(&z, 48);
            directory_end = record;
        }
    }
    if offset
        .checked_add(size)
        .is_none_or(|end| end > directory_end)
    {
        return Err(invalid(
            "the ZIP directory points past the end of the archive data",
        ));
    }
    let mut listing = Listing {
        declared: Some(declared),
        comment,
        ..Default::default()
    };
    if size > MAX_DIRECTORY {
        listing.truncated = true;
        listing
            .warnings
            .push("The ZIP directory exceeds 64 MiB; only the first 64 MiB were read".into());
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut directory = vec![0; size.min(MAX_DIRECTORY) as usize];
    file.read_exact(&mut directory)?;
    let mut at = 0usize;
    while (listing.entries.len() as u64) < declared {
        if cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        if listing.entries.len() >= MAX_ENTRIES {
            listing.truncated = true;
            break;
        }
        let h = directory.get(at..).unwrap_or_default();
        if h.len() < 46 {
            if listing.truncated {
                break;
            }
            return Err(invalid(
                "the ZIP directory ends before its declared entries",
            ));
        }
        if !h.starts_with(b"PK\x01\x02") {
            return Err(invalid("expected a ZIP central-directory entry"));
        }
        let flags = u16le(h, 8);
        let name_len = u16le(h, 28) as usize;
        let extra_len = u16le(h, 30) as usize;
        let comment_len = u16le(h, 32) as usize;
        let record_len = 46 + name_len + extra_len + comment_len;
        if record_len > h.len() {
            if listing.truncated {
                break;
            }
            return Err(invalid("a ZIP directory entry is cut short"));
        }
        let name = String::from_utf8_lossy(&h[46..46 + name_len]).into_owned();
        let (mut packed, mut size) = (u32le(h, 20) as u64, u32le(h, 24) as u64);
        let mut entry_offset = u32le(h, 42) as u64;
        let mut disk = u16le(h, 34) as u32;
        let needs_zip64 = size == u32::MAX as u64
            || packed == u32::MAX as u64
            || entry_offset == u32::MAX as u64
            || disk == u16::MAX as u32;
        let mut found_zip64 = false;
        let mut extra = &h[46 + name_len..46 + name_len + extra_len];
        while !extra.is_empty() {
            if extra.len() < 4 {
                return Err(invalid("the ZIP extra-field header is cut short"));
            }
            let (id, n) = (u16le(extra, 0), u16le(extra, 2) as usize);
            let mut data = extra
                .get(4..4 + n)
                .ok_or_else(|| invalid("the ZIP extra field is cut short"))?;
            if id == 1 && !found_zip64 {
                found_zip64 = true;
                for value in [&mut size, &mut packed, &mut entry_offset] {
                    if *value == u32::MAX as u64 {
                        if data.len() < 8 {
                            return Err(invalid("a required ZIP64 value is missing"));
                        }
                        *value = u64le(data, 0);
                        data = &data[8..];
                    }
                }
                if disk == u16::MAX as u32 {
                    if data.len() < 4 {
                        return Err(invalid("the ZIP64 disk number is missing"));
                    }
                    disk = u32le(data, 0);
                }
            }
            extra = &extra[4 + n..];
        }
        if needs_zip64 && !found_zip64 {
            return Err(invalid("the ZIP64 extra field is missing"));
        }
        if disk != 0 {
            return Err(invalid("multi-disk ZIP entries are not supported"));
        }
        let method = u16le(h, 10);
        let attrs = u32le(h, 38);
        let unix_kind = if matches!(h[5], 3 | 19) {
            (attrs >> 16) & 0xf000
        } else {
            0
        };
        let dir = name.ends_with('/') || attrs & 0x10 != 0 || unix_kind == 0x4000;
        let special = match unix_kind {
            0xa000 => "symlink (target not read; may point outside the archive)",
            0 | 0x8000 | 0x4000 => "",
            _ => "special file",
        }
        .to_owned();
        let encrypted = flags & 0x41 != 0;
        let openable = !dir && !encrypted && special.is_empty() && matches!(method, 0 | 8);
        listing.entries.push(Member {
            name,
            size,
            packed: Some(packed),
            offset: entry_offset,
            crc: Some(u32le(h, 16)),
            method_id: method,
            openable,
        });
        at += record_len;
    }
    if listing.truncated {
        listing.warnings.push(format!(
            "Listing stopped at {} of {declared} entries (limit {MAX_ENTRIES})",
            listing.entries.len()
        ));
    }
    let total = listing.total_size();
    if total as u128 > len.max(1) as u128 * 1000 {
        listing.warnings.push(format!(
            "Declared uncompressed data is about {}× the archive size ({}); compressed arrays are limited to 256 MiB",
            total / len.max(1), human(total)
        ));
    }
    Ok(listing)
}
fn human(bytes: u64) -> String {
    const U: [&str; 6] = ["bytes", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < U.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{v:.1} {}", U[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "isolinear-npz-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&dir).unwrap();
            Self(dir)
        }
        fn write(&self, bytes: &[u8]) -> PathBuf {
            let path = self.0.join("arrays.npz");
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/npz")
            .join(name)
    }
    fn npy(descr: &str, shape: &str, data: &[u8]) -> Vec<u8> {
        let header =
            format!("{{'descr': '{descr}', 'fortran_order': False, 'shape': {shape}, }}\n");
        let mut out = b"\x93NUMPY\x01\x00".to_vec();
        out.extend((header.len() as u16).to_le_bytes());
        out.extend(header.as_bytes());
        out.extend(data);
        out
    }
    fn zip(entries: &[(&str, &[u8])], method: u16) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let start = out.len() as u32;
            let mut crc = flate2::Crc::new();
            crc.update(data);
            let packed = if method == 8 {
                let mut e =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            } else {
                data.to_vec()
            };
            let header = |central: bool| {
                let mut h = if central {
                    b"PK\x01\x02".to_vec()
                } else {
                    b"PK\x03\x04".to_vec()
                };
                if central {
                    h.extend(20u16.to_le_bytes());
                }
                h.extend(20u16.to_le_bytes());
                h.extend(0u16.to_le_bytes());
                h.extend(method.to_le_bytes());
                h.extend([0; 4]);
                h.extend(crc.sum().to_le_bytes());
                h.extend((packed.len() as u32).to_le_bytes());
                h.extend((data.len() as u32).to_le_bytes());
                h.extend((name.len() as u16).to_le_bytes());
                h.extend([0; 2]);
                if central {
                    h.extend([0; 10]);
                    h.extend(start.to_le_bytes());
                }
                h.extend(name.as_bytes());
                h
            };
            out.extend(header(false));
            central.extend(header(true));
            out.extend(packed);
        }
        let offset = out.len() as u32;
        out.extend(&central);
        out.extend(b"PK\x05\x06\0\0\0\0");
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(offset.to_le_bytes());
        out.extend([0; 2]);
        out
    }
    fn strings(values: &[&str]) -> Vec<Option<String>> {
        values.iter().map(|s| Some((*s).into())).collect()
    }

    #[test]
    fn reference_numpy_archives_list_and_page_their_arrays() {
        let path = fixture("np-arrays.npz");
        let mut table = open(&path, None).unwrap();
        assert_eq!(
            table
                .schema()
                .tables
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(table.schema().rows, Some(10));
        assert_eq!(
            table.rows(8, 5).unwrap(),
            [strings(&["8"]), strings(&["9"])]
        );
        let mut second = open(&path, Some("second")).unwrap();
        assert_eq!(second.schema().table, 1);
        assert_eq!(
            second.rows(0, 5).unwrap(),
            [strings(&["x"]), strings(&["yz"])]
        );
        assert!(open(&path, Some("missing")).is_err());
        let path = fixture("np-compressed.npz");
        let mut dates = open(&path, None).unwrap();
        assert_eq!(
            dates
                .schema()
                .tables
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["dates", "grid"]
        );
        assert_eq!(dates.rows(9, 5).unwrap(), [strings(&["2026-01-10"])]);
        let mut grid = open(&path, Some("grid")).unwrap();
        // Retain the current NPY reader's float formatting.
        assert_eq!(grid.rows(49, 5).unwrap(), [strings(&["1", "1", "1", "1"])]);
        assert!(open(&path, Some("missing")).is_err());
    }

    #[test]
    fn stored_and_deflated_members_use_the_same_npy_reader() {
        let data = npy("<i2", "(2, 2)", &[1, 0, 2, 0, 3, 0, 4, 0]);
        for method in [0, 8] {
            let temp = Temp::new();
            let path = temp.write(&zip(
                &[("readme.txt", b"not an array"), ("dir/a.npy", &data)],
                method,
            ));
            let mut table = open(&path, None).unwrap();
            assert_eq!(table.schema().tables[0].name, "dir/a");
            assert_eq!(table.rows(1, 20).unwrap(), [strings(&["3", "4"])]);
            assert!(table.rows(u64::MAX, usize::MAX).unwrap().is_empty());
            assert!(table.rows(0, 0).unwrap().is_empty());
            assert_eq!(
                std::fs::read_dir(&temp.0).unwrap().count(),
                1,
                "opening writes no files"
            );
        }
    }

    #[test]
    fn stored_member_reads_stay_inside_their_range() {
        let temp = Temp::new();
        let data = npy("|u1", "(1,)", &[7]);
        let bytes = zip(&[("a.npy", &data)], 0);
        let path = temp.write(&bytes);
        let mut source = Npz::open(&path, None, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(source.rows(0, 1).unwrap(), [strings(&["7"])]);
        // Range reads cannot reach a neighbouring member or the central directory.
        let file = File::open(&path).unwrap();
        let mut range = Range {
            file,
            start: 35,
            len: data.len() as u64,
            pos: data.len() as u64 - 1,
            cancelled: source.cancelled.clone(),
        };
        let mut buf = [0; 16];
        assert_eq!(range.read(&mut buf).unwrap(), 1);
        assert_eq!(range.read(&mut buf).unwrap(), 0);
        assert!(range.seek(SeekFrom::End(-1_000_000)).is_err());
    }

    #[test]
    fn large_stored_arrays_are_paged_without_decompression() {
        use std::io::Write;
        let temp = Temp::new();
        let count = MEMBER_MAX + 1;
        let header = npy("|u1", &format!("({count},)"), &[]);
        let mut bytes = zip(&[("a.npy", &header)], 0);
        let old_cd = 35 + header.len();
        let member_size = header.len() as u32 + count as u32;
        for at in [18, 22, old_cd + 20, old_cd + 24] {
            bytes[at..at + 4].copy_from_slice(&member_size.to_le_bytes());
        }
        let new_cd = 35 + u64::from(member_size);
        let eocd = bytes.len() - 22;
        bytes[eocd + 16..eocd + 20].copy_from_slice(&(new_cd as u32).to_le_bytes());
        let path = temp.0.join("large.npz");
        let mut file = File::create(&path).unwrap();
        file.write_all(&bytes[..old_cd]).unwrap();
        file.seek(SeekFrom::Start(new_cd - 1)).unwrap();
        file.write_all(&[42]).unwrap();
        file.write_all(&bytes[old_cd..]).unwrap();
        drop(file);
        let mut table = open(&path, None).unwrap();
        assert_eq!(table.schema().rows, Some(count));
        assert_eq!(table.rows(count - 1, 1).unwrap(), [strings(&["42"])]);
    }

    #[test]
    fn crc_size_encryption_and_missing_headers_are_rejected() {
        let data = npy("|u1", "(3,)", &[1, 2, 3]);
        let good = zip(&[("a.npy", &data)], 8);
        let cd = good.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
        let temp = Temp::new();
        for (at, replacement) in [
            (cd + 16, 0u32.to_le_bytes().to_vec()),
            (cd + 24, (data.len() as u32 - 1).to_le_bytes().to_vec()),
            (cd + 24, (data.len() as u32 + 1).to_le_bytes().to_vec()),
            (cd + 24, (MEMBER_MAX as u32 + 1).to_le_bytes().to_vec()),
            (cd + 8, vec![1, 0]),
            (cd + 42, u32::MAX.to_le_bytes().to_vec()),
            (0, vec![0, 0, 0, 0]),
        ] {
            let mut bytes = good.clone();
            bytes[at..at + replacement.len()].copy_from_slice(&replacement);
            assert!(open(&temp.write(&bytes), None).is_err(), "offset {at}");
        }
        assert!(open(&temp.write(&zip(&[("text", b"hello")], 0)), None).is_err());
    }

    #[test]
    fn pickles_and_malformed_array_headers_are_never_loaded() {
        let temp = Temp::new();
        for method in [0, 8] {
            for data in [
                npy("|O8", "(1,)", &[0; 8]),
                npy("<i4", "(18446744073709551615, 2, 2)", &[]),
                npy("<é4", "(1,)", &[0; 4]),
            ] {
                assert!(open(&temp.write(&zip(&[("a.npy", &data)], method)), None).is_err());
            }
        }
    }

    #[test]
    fn cancellation_and_file_changes_are_reported() {
        let temp = Temp::new();
        let path = temp.write(&zip(&[("a.npy", &npy("|u1", "(3,)", &[1, 2, 3]))], 8));
        let token = Arc::new(AtomicBool::new(true));
        assert!(
            open_with_cancel(&path, None, token.clone())
                .err()
                .unwrap()
                .contains("cancelled")
        );
        token.store(false, Ordering::Relaxed);
        let mut source = open_with_cancel(&path, None, token.clone()).unwrap();
        token.store(true, Ordering::Relaxed);
        assert!(source.rows(0, 1).unwrap_err().contains("cancelled"));
        let mut source = open(&path, None).unwrap();
        std::fs::write(&path, b"changed").unwrap();
        assert!(source.rows(0, 1).unwrap_err().contains("changed"));
    }

    #[test]
    fn twenty_thousand_mutated_archives_never_panic() {
        let temp = Temp::new();
        let bases = [
            std::fs::read(fixture("np-arrays.npz")).unwrap(),
            std::fs::read(fixture("np-compressed.npz")).unwrap(),
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for i in 0..20_000 {
            let mut bytes = bases[i % bases.len()].clone();
            for _ in 0..next() % 8 + 1 {
                let at = next() as usize % bytes.len();
                bytes[at] = next() as u8;
            }
            if next() % 4 == 0 {
                bytes.truncate(next() as usize % bytes.len());
            }
            if let Ok(mut table) = open(&temp.write(&bytes), None) {
                let _ = table.rows(0, 4);
            }
        }
    }
}
