//! What is inside an archive, without extracting it: ZIP central directories, tar headers, and a
//! bounded look at the start of gzip/Zstandard/xz/bzip2 streams. Listing writes nothing; decompressed output
//! is capped, so a decompression bomb costs at most the budget. One entry can be copied out
//! (`extract_*`) under `EXTRACT_LIMIT`, into a writer the caller chooses.
use std::io::{self, Read, Seek, SeekFrom, Write};

/// Entries listed at most; the rest is reported as truncated.
pub const MAX_ENTRIES: usize = 200_000;
/// Largest ZIP central directory read.
const MAX_DIRECTORY: u64 = 64 * 1024 * 1024;
/// Decompressed bytes scanned when listing a compressed tar stream.
pub const TAR_SCAN_LIMIT: u64 = 256 * 1024 * 1024;

/// Largest single entry (or decompressed stream) copied out to be opened.
pub const EXTRACT_LIMIT: u64 = 256 * 1024 * 1024;
/// Decoder memory allowed for xz/LZMA (the dictionary size comes from the file): `xz -9` needs
/// 65 MiB, so 256 MiB refuses only crafted or unusual streams.
const LZMA_MEMORY_KIB: u32 = 256 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    /// Stored size inside the archive, when the format records it.
    pub packed: Option<u64>,
    /// "YYYY-MM-DD HH:MM" as recorded (ZIP: local time without zone; tar: UTC).
    pub modified: String,
    pub method: String,
    pub dir: bool,
    pub special: String,
    pub encrypted: bool,
    /// The name escapes the archive root (absolute path or `..`); extracting it would be unsafe.
    pub unsafe_path: bool,
    /// A regular file whose data Isolinear can decode (not a folder, link, or encrypted entry).
    pub openable: bool,
    /// ZIP: local header offset. Tar: payload offset in the uncompressed stream.
    pub offset: u64,
    /// ZIP: the CRC-32 the directory records, checked after extraction.
    pub crc: Option<u32>,
    /// ZIP: the compression method number.
    pub method_id: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// Entries the archive declares (may exceed `entries.len()` when truncated).
    pub declared: Option<u64>,
    pub truncated: bool,
    pub comment: String,
    pub warnings: Vec<String>,
}
impl Listing {
    /// The entry named `name`, ignoring ASCII case (OOXML part names are case-insensitive).
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
    }
    pub fn total_size(&self) -> u64 {
        self.entries
            .iter()
            .map(|e| e.size)
            .fold(0, u64::saturating_add)
    }
    fn finish(mut self) -> Self {
        let unsafe_paths = self.entries.iter().filter(|e| e.unsafe_path).count();
        if unsafe_paths > 0 {
            self.warnings.push(format!(
                "{unsafe_paths} entries have absolute or \"..\" paths that would write outside a target folder"
            ));
        }
        self
    }
}

pub(super) fn unsafe_path(name: &str) -> bool {
    name.starts_with('/')
        || name.starts_with('\\')
        || name.get(1..3) == Some(":\\")
        || name.get(1..3) == Some(":/")
        || name.split(['/', '\\']).any(|part| part == "..")
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

fn dos_time(date: u16, time: u16) -> String {
    if date == 0 {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        1980 + (date >> 9),
        (date >> 5) & 0xf,
        date & 0x1f,
        time >> 11,
        (time >> 5) & 0x3f
    )
}

fn zip_method(method: u16) -> String {
    match method {
        0 => "stored".into(),
        8 => "deflate".into(),
        9 => "deflate64".into(),
        12 => "bzip2".into(),
        14 => "LZMA".into(),
        93 => "Zstandard".into(),
        95 => "xz".into(),
        99 => "AES".into(),
        m => format!("method {m}"),
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}
fn too_large(size: u64) -> io::Error {
    invalid(&format!(
        "it is {} when extracted; Isolinear opens entries up to {}",
        human(size),
        human(EXTRACT_LIMIT)
    ))
}

/// Counts and checksums what passes through, and refuses to write past `limit`.
struct Sink<'a> {
    out: &'a mut dyn Write,
    crc: flate2::Crc,
    written: u64,
    limit: u64,
    overflow: bool,
}
impl Write for Sink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() as u64 > self.limit.saturating_sub(self.written) {
            self.overflow = true;
            return Err(invalid("it expands past its declared size"));
        }
        let n = self.out.write(buf)?;
        self.crc.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Copy the data of one ZIP entry (from `list_zip`) into `out`: stored, deflate, bzip2, LZMA,
/// Zstandard or xz, at most its declared size and `EXTRACT_LIMIT`, CRC-32 checked. Returns the bytes written.
pub fn extract_zip(
    file: &mut (impl Read + Seek),
    entry: &Entry,
    out: &mut dyn Write,
) -> io::Result<u64> {
    if entry.dir {
        return Err(invalid("it is a folder"));
    }
    if entry.encrypted {
        return Err(invalid("it is encrypted"));
    }
    if !entry.special.is_empty() {
        return Err(invalid("only regular files can be opened"));
    }
    if !entry.openable {
        return Err(invalid(&format!(
            "its compression ({}) is not supported",
            entry.method
        )));
    }
    if entry.size > EXTRACT_LIMIT {
        return Err(too_large(entry.size));
    }
    let len = file.seek(SeekFrom::End(0))?;
    let packed = entry.packed.unwrap_or(0);
    let mut header = [0u8; 30];
    file.seek(SeekFrom::Start(entry.offset))?;
    file.read_exact(&mut header)
        .map_err(|_| invalid("the entry's header is outside the file"))?;
    if !header.starts_with(b"PK\x03\x04") {
        return Err(invalid("the entry's local header is missing"));
    }
    if u16le(&header, 8) != entry.method_id || u16le(&header, 6) & 0x41 != 0 {
        return Err(invalid("the local header disagrees with the ZIP directory"));
    }
    let start = entry
        .offset
        .checked_add(30 + u16le(&header, 26) as u64 + u16le(&header, 28) as u64)
        .ok_or_else(|| invalid("the entry's offset overflows"))?;
    if start.saturating_add(packed) > len {
        return Err(invalid("the entry's data runs past the end of the file"));
    }
    file.seek(SeekFrom::Start(start))?;
    let data = file.take(packed);
    let mut sink = Sink {
        out,
        crc: flate2::Crc::new(),
        written: 0,
        limit: entry.size,
        overflow: false,
    };
    match entry.method_id {
        0 => io::copy(&mut { data }, &mut sink)?,
        8 => io::copy(&mut flate2::read::DeflateDecoder::new(data), &mut sink)?,
        12 => io::copy(&mut bzip2::read::BzDecoder::new(data), &mut sink)?,
        14 => io::copy(&mut zip_lzma(data, entry.size)?, &mut sink)?,
        95 => io::copy(&mut Compression::Xz.reader(data)?, &mut sink)?,
        93 => io::copy(&mut Compression::Zstd.reader(data)?, &mut sink)?,
        _ => return Err(invalid("the ZIP compression method is not supported")),
    };
    if sink.written != entry.size {
        return Err(invalid("the entry is shorter than its declared size"));
    }
    if entry.crc.is_some_and(|crc| crc != sink.crc.sum()) {
        return Err(invalid(
            "the entry's checksum does not match (damaged data)",
        ));
    }
    Ok(sink.written)
}

/// ZIP's LZMA (method 14): a 4-byte version/length header, the 5 property bytes, then raw LZMA.
/// The entry's size bounds the output, so an end marker after it is never needed.
fn zip_lzma<R: Read>(mut data: R, size: u64) -> io::Result<impl Read> {
    let mut header = [0u8; 9];
    data.read_exact(&mut header)
        .map_err(|_| invalid("the LZMA header is cut short"))?;
    if u16le(&header, 2) != 5 {
        return Err(invalid("the LZMA header is not understood"));
    }
    let dict = u32le(&header, 5);
    let memory = lzma_rust2::lzma_get_memory_usage_by_props(dict, header[4])
        .map_err(|_| invalid("the LZMA properties are invalid"))?;
    if memory > LZMA_MEMORY_KIB {
        return Err(invalid(
            "its LZMA dictionary needs more memory than Isolinear allows",
        ));
    }
    lzma_rust2::LzmaReader::new_with_props(data, size, header[4], dict, None)
}

/// List a ZIP archive from its central directory (ZIP64 included). Only the directory is read.
pub fn list_zip(
    file: &mut (impl Read + Seek),
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Listing> {
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
        let openable =
            !dir && !encrypted && special.is_empty() && matches!(method, 0 | 8 | 12 | 14 | 93 | 95);
        listing.entries.push(Entry {
            dir,
            special,
            unsafe_path: unsafe_path(&name),
            name,
            size,
            packed: Some(packed),
            modified: dos_time(u16le(h, 14), u16le(h, 12)),
            method: zip_method(method),
            encrypted,
            openable,
            offset: entry_offset,
            crc: Some(u32le(h, 16)),
            method_id: method,
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
            "Possible decompression bomb: expands about {}× ({} declared); extracting it could fill a disk",
            total / len.max(1), human(total)
        ));
    }
    Ok(listing.finish())
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

fn octal(field: &[u8]) -> Option<u64> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        if field[0] & 0x40 != 0 {
            return None;
        }
        return field.iter().enumerate().try_fold(0u64, |v, (i, &b)| {
            v.checked_mul(256)?
                .checked_add(if i == 0 { (b & 0x7f) as u64 } else { b as u64 })
        });
    }
    let text = std::str::from_utf8(field).ok()?;
    let digits = text.trim_matches(|c: char| c == '\0' || c == ' ');
    if digits.is_empty() {
        Some(0)
    } else {
        u64::from_str_radix(digits, 8).ok()
    }
}

fn cstr(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Skips payloads on disk, but consumes them through a decoder for compressed tar files.
trait TarInput: Read {
    fn skip(&mut self, bytes: u64) -> io::Result<u64>;
}
struct Stream<R>(R);
impl<R: Read> Read for Stream<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.0.read(out)
    }
}
impl<R: Read> TarInput for Stream<R> {
    fn skip(&mut self, bytes: u64) -> io::Result<u64> {
        io::copy(&mut self.0.by_ref().take(bytes), &mut io::sink())
    }
}
struct Disk<'a, R> {
    reader: &'a mut R,
    len: u64,
}
impl<R: Read> Read for Disk<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.reader.read(out)
    }
}
impl<R: Read + Seek> TarInput for Disk<'_, R> {
    fn skip(&mut self, bytes: u64) -> io::Result<u64> {
        let at = self.reader.stream_position()?;
        let skip = bytes.min(self.len.saturating_sub(at));
        self.reader.seek(SeekFrom::Start(
            at.checked_add(skip)
                .ok_or_else(|| invalid("tar offset overflows"))?,
        ))?;
        Ok(skip)
    }
}

pub fn list_tar_file(
    reader: &mut (impl Read + Seek),
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Listing> {
    let len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    walk_tar(&mut Disk { reader, len }, u64::MAX, cancelled, None).map(|(listing, _)| listing)
}

/// List a tar stream within a decompressed-byte budget.
pub fn list_tar(
    reader: &mut impl Read,
    limit: u64,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Listing> {
    walk_tar(&mut Stream(reader), limit, cancelled, None).map(|(listing, _)| listing)
}

pub fn extract_tar(
    reader: &mut impl Read,
    index: usize,
    limit: u64,
    out: &mut dyn Write,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<u64> {
    walk_tar(&mut Stream(reader), limit, cancelled, Some((index, out)))?
        .1
        .ok_or_else(|| invalid("the entry was not found within the archive scan limit"))
}

#[derive(Default, Clone)]
struct Pax {
    path: Option<String>,
    link: Option<String>,
    size: Option<u64>,
    sparse: bool,
}
impl Pax {
    fn parse(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let space = bytes
                .iter()
                .position(|&b| b == b' ')
                .ok_or_else(|| invalid("pax record has no length"))?;
            let len = std::str::from_utf8(&bytes[..space])
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or_else(|| invalid("pax record length is invalid"))?;
            if len <= space + 1 || len > bytes.len() || bytes[len - 1] != b'\n' {
                return Err(invalid("pax record length runs outside its header"));
            }
            let field = &bytes[space + 1..len - 1];
            let eq = field
                .iter()
                .position(|&b| b == b'=')
                .ok_or_else(|| invalid("pax record has no value"))?;
            let value = String::from_utf8_lossy(&field[eq + 1..]).into_owned();
            match &field[..eq] {
                b"path" => self.path = Some(value),
                b"linkpath" => self.link = Some(value),
                b"size" => {
                    self.size = Some(value.parse().map_err(|_| invalid("pax size is invalid"))?)
                }
                key if key.starts_with(b"GNU.sparse") || key == b"SCHILY.realsize" => {
                    self.sparse = true
                }
                _ => {}
            }
            bytes = &bytes[len..];
        }
        Ok(())
    }
}

fn walk_tar(
    reader: &mut impl TarInput,
    limit: u64,
    cancelled: &dyn Fn() -> bool,
    mut target: Option<(usize, &mut dyn Write)>,
) -> io::Result<(Listing, Option<u64>)> {
    let mut listing = Listing::default();
    let mut read = 0u64;
    let mut metadata = 0usize;
    let mut block = [0u8; 512];
    let mut pending = Pax::default();
    let mut global = Pax::default();
    loop {
        if cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        if limit.saturating_sub(read) < 512 {
            listing.truncated = true;
            listing
                .warnings
                .push(format!("Listing stopped after {} of content", human(limit)));
            break;
        }
        match reader.read_exact(&mut block) {
            Ok(()) => {
                read = read
                    .checked_add(512)
                    .ok_or_else(|| invalid("tar offset overflows"))?
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                listing
                    .warnings
                    .push("The archive ends without its end marker".into());
                break;
            }
            Err(e) => return Err(e),
        }
        if block.iter().all(|&b| b == 0) {
            break;
        }
        let checksum = octal(&block[148..156]);
        let sum: u64 = block
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    b as u64
                }
            })
            .sum();
        if checksum != Some(sum) {
            listing.warnings.push(format!(
                "Header checksum mismatch after {} entries; listing stopped",
                listing.entries.len()
            ));
            break;
        }
        if listing.entries.len() >= MAX_ENTRIES {
            listing.truncated = true;
            listing
                .warnings
                .push(format!("Listing stopped at {MAX_ENTRIES} entries"));
            break;
        }
        let kind = block[156];
        let extension = matches!(kind, b'L' | b'K' | b'x' | b'g');
        let header_size = octal(&block[124..136])
            .ok_or_else(|| invalid("tar entry size is invalid or overflows"))?;
        let size = if extension {
            header_size
        } else {
            pending.size.or(global.size).unwrap_or(header_size)
        };
        let padded = size
            .checked_add(511)
            .map(|n| n / 512 * 512)
            .ok_or_else(|| invalid("tar entry size overflows"))?;
        if extension {
            // Extensions must obey the same scan budget as ordinary payloads.
            if padded > limit.saturating_sub(read) {
                listing.truncated = true;
                listing.warnings.push(format!(
                    "Tar extension exceeds the {} scan budget; listing stopped",
                    human(limit)
                ));
                break;
            }
            if size > 64 * 1024 {
                listing.truncated = true;
                listing
                    .warnings
                    .push("Tar extension exceeds 64 KiB; listing stopped".into());
                break;
            }
            let mut data = vec![0; size as usize];
            reader.read_exact(&mut data)?;
            if reader.skip(padded - size)? != padded - size {
                return Err(invalid("tar extension is cut short"));
            }
            read = read
                .checked_add(padded)
                .ok_or_else(|| invalid("tar offset overflows"))?;
            match kind {
                b'L' => pending.path = Some(cstr(&data)),
                b'K' => pending.link = Some(cstr(&data)),
                b'x' => pending.parse(&data)?,
                b'g' => global.parse(&data)?,
                _ => {}
            }
            continue;
        }
        let name = pending
            .path
            .take()
            .or_else(|| global.path.clone())
            .unwrap_or_else(|| {
                let prefix = cstr(&block[345..500]);
                let name = cstr(&block[..100]);
                if &block[257..263] == b"ustar\0" && !prefix.is_empty() {
                    format!("{prefix}/{name}")
                } else {
                    name
                }
            });
        let link = pending
            .link
            .take()
            .or_else(|| global.link.clone())
            .unwrap_or_else(|| cstr(&block[157..257]));
        let sparse = pending.sparse || global.sparse || kind == b'S';
        pending = Pax::default();
        let method = match kind {
            _ if sparse => "sparse file (not supported)",
            b'0' | 0 | b'7' => "file",
            b'1' => "hard link",
            b'2' => "symlink",
            b'3' | b'4' => "device",
            b'5' => "folder",
            b'6' => "pipe",
            _ => "other",
        };
        let openable = matches!(kind, b'0' | 0 | b'7') && !sparse;
        if let Some((index, out)) = target.as_mut()
            && *index == listing.entries.len()
        {
            if !openable {
                return Err(invalid("only regular files can be opened"));
            }
            if size > EXTRACT_LIMIT {
                return Err(too_large(size));
            }
            if size > limit.saturating_sub(read) {
                return Err(invalid("entry data exceeds the tar scan budget"));
            }
            let copied = io::copy(&mut reader.by_ref().take(size), out)?;
            if copied != size {
                return Err(invalid("the archive is cut short inside this entry"));
            }
            return Ok((listing, Some(copied)));
        }
        metadata += name.len() + link.len();
        if metadata > MAX_DIRECTORY as usize {
            listing.truncated = true;
            listing
                .warnings
                .push("Tar names exceed 64 MiB; listing stopped".into());
            break;
        }
        let mtime = octal(&block[136..148])
            .and_then(|n| i64::try_from(n).ok())
            .unwrap_or(0);
        let special = if openable || kind == b'5' {
            String::new()
        } else if matches!(kind, b'1' | b'2') {
            format!("{method} → {link}")
        } else {
            method.into()
        };
        listing.entries.push(Entry {
            dir: kind == b'5',
            special,
            unsafe_path: unsafe_path(&name) || (matches!(kind, b'1' | b'2') && unsafe_path(&link)),
            name,
            size,
            packed: None,
            modified: if mtime > 0 { utc(mtime) } else { String::new() },
            method: method.into(),
            openable,
            offset: read,
            ..Default::default()
        });
        if kind == b'S' {
            listing.truncated = true;
            listing
                .warnings
                .push("GNU sparse tar layout is not supported; listing stopped".into());
            break;
        }
        let skipped = reader.skip(padded.min(limit.saturating_sub(read)))?;
        read = read
            .checked_add(skipped)
            .ok_or_else(|| invalid("tar offset overflows"))?;
        if skipped < padded {
            if read >= limit {
                continue;
            }
            listing
                .warnings
                .push("The archive is cut short inside an entry".into());
            break;
        }
    }
    Ok((listing.finish(), None))
}

/// Compression of a stream Isolinear can peek into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    Gzip,
    Zstd,
    Xz,
    Bzip2,
}
impl Compression {
    #[cfg(test)]
    pub fn of(head: &[u8]) -> Option<Self> {
        if head.starts_with(b"\x1f\x8b") {
            Some(Self::Gzip)
        } else if head.starts_with(b"\x28\xb5\x2f\xfd") {
            Some(Self::Zstd)
        } else if head.starts_with(b"\xfd7zXZ\0") {
            Some(Self::Xz)
        } else if head.starts_with(b"BZh") && head.get(3).is_some_and(|b| (b'1'..=b'9').contains(b))
        {
            Some(Self::Bzip2)
        } else {
            None
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Zstd => "Zstandard",
            Self::Xz => "xz",
            Self::Bzip2 => "bzip2",
        }
    }
    /// A decompressing reader over `file` (concatenated gzip, xz and bzip2 streams included).
    /// xz streams whose dictionary needs more than 256 MiB are refused when they start.
    pub fn reader<'a>(self, file: impl Read + 'a) -> io::Result<Box<dyn Read + 'a>> {
        Ok(match self {
            Self::Gzip => Box::new(flate2::read::MultiGzDecoder::new(file)),
            Self::Zstd => {
                let mut decoder = zstd::stream::read::Decoder::new(file)?;
                decoder.window_log_max(28)?;
                Box::new(decoder)
            }
            Self::Xz => Box::new(lzma_rust2::XzReader::new_mem_limit(
                file,
                true,
                LZMA_MEMORY_KIB,
            )),
            Self::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(file)),
        })
    }
}

/// The start of a compressed stream's content.
#[cfg(test)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Peek {
    pub bytes: Vec<u8>,
    /// The content continues past `limit`.
    pub more: bool,
    /// Decoding stopped with this error after `bytes`.
    pub error: Option<String>,
}

/// The first `limit` decompressed bytes. A damaged stream keeps what decoded before the damage.
#[cfg(test)]
pub fn peek(compression: Compression, file: impl Read, limit: u64) -> Peek {
    let mut peek = Peek::default();
    let mut reader = match compression.reader(file) {
        Ok(r) => r.take(limit + 1),
        Err(e) => {
            peek.error = Some(e.to_string());
            return peek;
        }
    };
    if let Err(e) = reader.read_to_end(&mut peek.bytes) {
        peek.error = Some(e.to_string());
    }
    peek.more = peek.bytes.len() as u64 > limit;
    peek.bytes.truncate(limit as usize);
    peek
}

/// The whole content of a compressed stream into `out`, refusing more than `EXTRACT_LIMIT`.
pub fn extract_stream(
    compression: Compression,
    file: impl Read,
    out: &mut dyn Write,
) -> io::Result<u64> {
    let mut reader = compression.reader(file)?;
    let mut sink = Sink {
        out,
        crc: flate2::Crc::new(),
        written: 0,
        limit: EXTRACT_LIMIT,
        overflow: false,
    };
    io::copy(&mut reader, &mut sink).map_err(|e| {
        if sink.overflow {
            invalid(&format!(
                "it is larger than {} when decompressed; Isolinear opens up to that size",
                human(EXTRACT_LIMIT)
            ))
        } else {
            e
        }
    })
}

pub(super) fn utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (H. Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60
    )
}

/// Recognize legacy tar headers too; a filename alone is not required for compressed tar.
pub(super) fn is_tar_header(block: &[u8]) -> bool {
    if block.len() != 512 || block.iter().all(|&b| b == 0) {
        return false;
    }
    let sum: u64 = block
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                b as u64
            }
        })
        .sum();
    octal(&block[148..156]) == Some(sum)
}
