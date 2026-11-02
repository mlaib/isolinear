//! What a binary or unknown file is, from its first bytes only: a signature table with header
//! details. Nothing here decompresses,
//! executes or follows anything; every header read is bounds-checked.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Category {
    Document,
    Archive,
    Compressed,
    Image,
    Audio,
    Video,
    Database,
    Data,
    Executable,
    Font,
}
impl Category {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Document => "Document",
            Self::Archive => "Archive",
            Self::Compressed => "Compressed data",
            Self::Image => "Image",
            Self::Audio => "Audio",
            Self::Video => "Video",
            Self::Database => "Database",
            Self::Data => "Data",
            Self::Executable => "Program",
            Self::Font => "Font",
        }
    }
}

/// Result of identifying a file from its first bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    /// Human name, e.g. "PNG image" or "ZIP archive (Word document, DOCX)".
    pub(super) name: String,
    pub(super) category: Category,
    /// Facts read from the header, as (label, value).
    pub(super) details: Vec<(String, String)>,
}

fn u16le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn u16be(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        b.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn u32le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn u32be(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn at(b: &[u8], offset: usize, sig: &[u8]) -> bool {
    offset
        .checked_add(sig.len())
        .and_then(|end| b.get(offset..end))
        == Some(sig)
}
/// Printable ASCII prefix of `b`, at most `max` bytes, stopping at the first non-printable byte.
fn ascii(b: &[u8], max: usize) -> String {
    b.iter()
        .take(max)
        .take_while(|&&c| (0x20..0x7f).contains(&c))
        .map(|&c| c as char)
        .collect()
}

type Id = Identity;
impl Id {
    fn new(name: impl Into<String>, category: Category) -> Self {
        Self {
            name: name.into(),
            category,
            details: vec![],
        }
    }
    fn with(mut self, label: &str, value: impl Into<String>) -> Self {
        let value = value.into();
        if !value.is_empty() {
            self.details.push((label.to_owned(), value));
        }
        self
    }
    fn done(self) -> Option<Identity> {
        Some(self)
    }
}

/// Identify `head` (the first bytes of a file). `binary` says whether the bytes look binary;
/// short or common signatures (MZ, BM, ID3, …) are trusted only then, so text is never misnamed.
pub(super) fn identify(head: &[u8], binary: bool) -> Option<Identity> {
    use Category::*;
    let h = head;
    if h.starts_with(b"%PDF-") {
        return Id::new("PDF document", Document)
            .with("Version", ascii(&h[5..], 4))
            .done();
    }
    if h.starts_with(b"PK\x03\x04") || h.starts_with(b"PK\x05\x06") {
        return zip(h);
    }
    if h.starts_with(b"SQLite format 3\0") {
        let page = match u16be(h, 16) {
            Some(1) => 65536,
            Some(p) => p as u32,
            None => 0,
        };
        let pages = u32be(h, 28).unwrap_or(0);
        let encoding = match u32be(h, 56) {
            Some(1) => "UTF-8",
            Some(2) => "UTF-16 LE",
            Some(3) => "UTF-16 BE",
            _ => "",
        };
        let mut id = Id::new("SQLite database", Database).with("Text encoding", encoding);
        if (512..=65536).contains(&page) && page.is_power_of_two() {
            id = id.with("Page size", format!("{page} bytes"));
        }
        if pages > 0 && page > 0 {
            id = id.with("Size in header", format!("{pages} pages"));
        }
        return id.done();
    }
    if h.starts_with(b"\x7fELF") {
        return elf(h);
    }
    if let Some(id) = mach_o(h) {
        return Some(id);
    }
    if h.starts_with(b"\x89PNG\r\n\x1a\n") {
        if !at(h, 12, b"IHDR") || u32be(h, 8) != Some(13) {
            return Id::new("PNG image", Image).done();
        }
        let color = match h.get(25) {
            Some(0) => "greyscale",
            Some(2) => "RGB",
            Some(3) => "palette",
            Some(4) => "greyscale + alpha",
            Some(6) => "RGBA",
            _ => "",
        };
        let mut id = Id::new("PNG image", Image);
        if let (Some(w), Some(hh)) = (u32be(h, 16), u32be(h, 20)) {
            id = id.with("Dimensions", format!("{w} × {hh} pixels"));
        }
        if let Some(depth) = h.get(24) {
            id = id.with(
                "Colour",
                format!("{color}, {depth}-bit").trim_start_matches(", "),
            );
        }
        return id.done();
    }
    if h.starts_with(b"\xff\xd8\xff") {
        let mut id = Id::new("JPEG image", Image);
        if let Some((w, hh)) = jpeg_size(h) {
            id = id.with("Dimensions", format!("{w} × {hh} pixels"));
        }
        if at(h, 6, b"Exif") {
            id = id.with("Metadata", "Exif (camera data may include location)");
        }
        return id.done();
    }
    if h.starts_with(b"GIF87a") || h.starts_with(b"GIF89a") {
        let mut id = Id::new("GIF image", Image).with("Version", ascii(&h[3..], 3));
        if let (Some(w), Some(hh)) = (u16le(h, 6), u16le(h, 8)) {
            id = id.with("Dimensions", format!("{w} × {hh} pixels"));
        }
        return id.done();
    }
    if h.starts_with(b"8BPS") {
        return psd(h);
    }
    if h.starts_with(b"RIFF") {
        match h.get(8..12) {
            Some(b"WEBP") => return Id::new("WebP image", Image).done(),
            Some(b"WAVE") => {
                let mut id = Id::new("WAV audio", Audio);
                if at(h, 12, b"fmt ") {
                    if let Some(ch) = u16le(h, 22) {
                        id = id.with("Channels", ch.to_string());
                    }
                    if let Some(rate) = u32le(h, 24) {
                        id = id.with("Sample rate", format!("{rate} Hz"));
                    }
                }
                return id.done();
            }
            Some(b"AVI ") => return Id::new("AVI video", Video).done(),
            _ => {}
        }
    }
    if h.starts_with(b"II*\0") || h.starts_with(b"MM\0*") {
        return Id::new("TIFF image", Image)
            .with(
                "Byte order",
                if h[0] == b'I' {
                    "little-endian"
                } else {
                    "big-endian"
                },
            )
            .done();
    }
    if h.starts_with(b"\x1f\x8b") {
        return gzip(h);
    }
    if h.starts_with(b"BZh") && h.get(3).is_some_and(|b| (b'1'..=b'9').contains(b)) {
        return Id::new("bzip2 compressed data", Compressed)
            .with("Block size", format!("{}00 kB", h[3] as char))
            .done();
    }
    if h.starts_with(b"\xfd7zXZ\0") {
        return Id::new("xz compressed data", Compressed).done();
    }
    if h.starts_with(b"\x28\xb5\x2f\xfd") {
        return Id::new("Zstandard compressed data", Compressed).done();
    }
    if h.starts_with(b"\x04\x22\x4d\x18") {
        return Id::new("LZ4 compressed data", Compressed).done();
    }
    if h.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return Id::new("7-Zip archive", Archive)
            .with("Format version", version(h, 6))
            .done();
    }
    if h.starts_with(b"Rar!\x1a\x07") {
        return Id::new("RAR archive", Archive)
            .with(
                "Format",
                match h.get(6) {
                    Some(0) => "RAR 4",
                    Some(1) => "RAR 5",
                    _ => "",
                },
            )
            .done();
    }
    if at(h, 257, b"ustar") {
        return Id::new("tar archive", Archive)
            .with("First entry", ascii(h, 100))
            .done();
    }
    if h.starts_with(b"!<arch>\ndebian") {
        return Id::new("Debian package", Archive).done();
    }
    if h.starts_with(b"!<arch>\n") {
        return Id::new("ar archive (static library)", Archive).done();
    }
    if h.starts_with(b"\xed\xab\xee\xdb") {
        return Id::new("RPM package", Archive).done();
    }
    if at(h, 0x8001, b"CD001") {
        return Id::new("ISO 9660 disc image", Archive)
            .with(
                "Volume label",
                ascii(h.get(0x8028..).unwrap_or(&[]), 32)
                    .trim_end()
                    .to_owned(),
            )
            .done();
    }
    if h.starts_with(b"Obj\x01") {
        let mut id = Id::new("Avro data file", Data);
        if let Some(i) = find(h, b"avro.codec")
            && let Some(&len) = h.get(i + 10)
            && len & 1 == 0
            && len <= 24
            && let Some(bytes) = h.get(i + 11..i + 11 + usize::from(len / 2))
            && bytes.iter().all(u8::is_ascii_graphic)
        {
            // Codec names fit a one-byte, nonnegative Avro length.
            id = id.with("Compression", ascii(bytes, 12));
        }
        return id.done();
    }
    if h.starts_with(b"PAR1") {
        return Id::new("Parquet data file", Data).done();
    }
    if h.starts_with(b"ARROW1") {
        return Id::new("Arrow IPC file", Data).done();
    }
    if h.starts_with(b"ORC") {
        return Id::new("ORC data file", Data).done();
    }
    if h.starts_with(b"\x89HDF\r\n\x1a\n") {
        return Id::new("HDF5 data file", Data).done();
    }
    if h.starts_with(b"CDF\x01") || h.starts_with(b"CDF\x02") {
        return Id::new("NetCDF data file", Data).done();
    }
    if h.starts_with(b"\x93NUMPY") {
        let mut id = Id::new("NumPy array (.npy)", Data).with("Format version", version(h, 6));
        let header = match h.get(6) {
            Some(1) => u16le(h, 8).map(|n| (10usize, u32::from(n))),
            Some(2 | 3) => u32le(h, 8).map(|n| (12usize, n)),
            _ => None,
        };
        if let Some((start, len)) = header {
            let available = h.get(start..).unwrap_or_default();
            let shown = available.len().min(len as usize).min(400);
            let header = String::from_utf8_lossy(&available[..shown])
                .trim()
                .to_owned();
            id = id.with("Header", header);
            if u64::from(len) > shown as u64 {
                id = id.with(
                    "Header limit",
                    format!("showing the first {shown} of {len} header bytes"),
                );
            }
        }
        return id.done();
    }
    if h.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1") {
        // Password-protected DOCX/XLSX/PPTX files are compound files holding these streams.
        let utf16 = |s: &str| {
            s.encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<u8>>()
        };
        if find(h, &utf16("EncryptedPackage")).is_some()
            && find(h, &utf16("EncryptionInfo")).is_some()
        {
            return Id::new(
                "Password-protected Office document (DOCX, XLSX or PPTX)",
                Document,
            )
            .done();
        }
        return Id::new(
            "Microsoft Compound File (Office 97–2003 document, MSI or Outlook message)",
            Document,
        )
        .done();
    }
    if h.starts_with(b"bplist00") {
        return Id::new("Binary property list (Apple)", Data).done();
    }
    if h.starts_with(b"\0asm") {
        return Id::new("WebAssembly module", Executable)
            .with(
                "Version",
                u32le(h, 4).map(|v| v.to_string()).unwrap_or_default(),
            )
            .done();
    }
    if h.starts_with(b"\xac\xed\x00\x05") {
        return Id::new("Java serialized object", Data).done();
    }
    if h.starts_with(b"LUKS\xba\xbe") {
        return Id::new("LUKS encrypted volume", Data).done();
    }
    if h.starts_with(b"PACK") && u32be(h, 4).is_some_and(|v| v == 2 || v == 3) {
        return Id::new("Git pack file", Archive).done();
    }
    if h.starts_with(b"OTTO") {
        return Id::new("OpenType font (CFF)", Font).done();
    }
    if h.starts_with(b"wOFF") || h.starts_with(b"wOF2") {
        return Id::new("Web font (WOFF)", Font).done();
    }
    if h.starts_with(b"fLaC") {
        return Id::new("FLAC audio", Audio).done();
    }
    if h.starts_with(b"OggS") {
        return Id::new("Ogg media (Vorbis/Opus audio or Theora video)", Audio).done();
    }
    if h.starts_with(b"\x1a\x45\xdf\xa3") {
        let webm = find(&h[..h.len().min(64)], b"webm").is_some();
        return Id::new(if webm { "WebM video" } else { "Matroska video" }, Video).done();
    }
    if h.starts_with(b"MThd") {
        return Id::new("MIDI music", Audio).done();
    }
    if at(h, 4, b"ftyp") {
        let brand = ascii(h.get(8..).unwrap_or(&[]), 4);
        let (name, category) = match brand.trim() {
            "heic" | "heix" | "mif1" | "msf1" => ("HEIF/HEIC image", Image),
            "avif" | "avis" => ("AVIF image", Image),
            "qt" => ("QuickTime video", Video),
            "M4A" | "M4B" => ("MPEG-4 audio", Audio),
            "crx" => ("Canon raw image", Image),
            _ => ("MPEG-4 media", Video),
        };
        let mut id = Id::new(name, category).with("Brand", brand);
        if category == Image
            && let Some((w, hh)) = heif_size(h)
        {
            id = id.with("Dimensions", format!("{w} × {hh} pixels"));
        }
        return id.done();
    }
    if !binary {
        return None;
    }
    // Short signatures, trusted only for bytes that already look binary.
    if h.starts_with(b"MZ") {
        return pe(h);
    }
    if h.starts_with(b"BM") && u32le(h, 2).is_some() {
        let mut id = Id::new("BMP image", Image);
        if let (Some(w), Some(hh)) = (u32le(h, 18), u32le(h, 22)) {
            id = id.with(
                "Dimensions",
                format!("{w} × {} pixels", (hh as i32).unsigned_abs()),
            );
        }
        return id.done();
    }
    if h.starts_with(b"\0\0\x01\0") {
        return Id::new("Windows icon", Image)
            .with(
                "Images",
                u16le(h, 4).map(|n| n.to_string()).unwrap_or_default(),
            )
            .done();
    }
    // An MPEG audio frame header: sync, then no reserved version, layer, bit rate or sample rate.
    let frame = h.len() > 2
        && h[0] == 0xff
        && h[1] & 0xe0 == 0xe0
        && h[1] & 0x18 != 0x08
        && h[1] & 0x06 != 0
        && h[2] & 0xf0 != 0xf0
        && h[2] & 0x0c != 0x0c;
    if h.starts_with(b"ID3") || frame {
        return Id::new("MP3 audio", Audio).done();
    }
    if h.starts_with(b"\0\x01\0\0") || h.starts_with(b"true") {
        return Id::new("TrueType font", Font).done();
    }
    if h.first() == Some(&0x80) && h.get(1).is_some_and(|v| (2..=5).contains(v)) {
        return Id::new("Python pickle", Data)
            .with("Protocol", h[1].to_string())
            .done();
    }
    None
}

fn zip(h: &[u8]) -> Option<Identity> {
    use Category::*;
    if h.starts_with(b"PK\x05\x06") {
        return Id::new("ZIP archive (empty)", Archive).done();
    }
    let name_len = u16le(h, 26).unwrap_or(0) as usize;
    let extra_len = u16le(h, 28).unwrap_or(0) as usize;
    let first = h
        .get(30..30 + name_len)
        .map(|n| String::from_utf8_lossy(n).into_owned());
    let has = |needle: &[u8]| find(h, needle).is_some();
    let kind = match first.as_deref() {
        Some("mimetype")
            if u16le(h, 8) == Some(0) && u16le(h, 6).is_some_and(|flags| flags & 1 == 0) =>
        {
            let data = 30 + name_len + extra_len;
            let len = u32le(h, 18).unwrap_or(0).min(80) as usize;
            let mime = ascii(h.get(data..).unwrap_or(&[]), len);
            match mime.as_str() {
                m if m.starts_with("application/vnd.oasis.opendocument.text") => {
                    "OpenDocument text, ODT"
                }
                m if m.starts_with("application/vnd.oasis.opendocument.spreadsheet") => {
                    "OpenDocument spreadsheet, ODS"
                }
                m if m.starts_with("application/vnd.oasis.opendocument.presentation") => {
                    "OpenDocument presentation, ODP"
                }
                "application/epub+zip" => "EPUB e-book",
                _ => "",
            }
        }
        _ if has(b"word/") => "Word document, DOCX",
        _ if has(b"xl/") => "Excel workbook, XLSX",
        _ if has(b"ppt/") => "PowerPoint presentation, PPTX",
        Some("META-INF/MANIFEST.MF") | Some("META-INF/") => "Java archive, JAR",
        Some("AndroidManifest.xml") => "Android package, APK",
        _ => "",
    };
    let name = if kind.is_empty() {
        "ZIP archive".to_owned()
    } else {
        format!("ZIP archive ({kind})")
    };
    let mut id = Id::new(name, if kind.is_empty() { Archive } else { Document });
    if let Some(first) = first {
        let mut shown: String = first.chars().take(120).collect();
        if shown.len() < first.len() {
            shown.push('…');
        }
        id = id.with("First entry", shown);
    }
    if u16le(h, 6).is_some_and(|flags| flags & 1 == 1) {
        id = id.with("Encryption", "entries are encrypted");
    }
    id.done()
}

fn gzip(h: &[u8]) -> Option<Identity> {
    let flags = h.get(3).copied().unwrap_or(0);
    let mut id = Id::new("gzip compressed data", Category::Compressed);
    if let Some(mtime) = u32le(h, 4).filter(|&t| t > 0) {
        id = id.with("Compressed at", utc(mtime as i64));
    }
    if flags & 0x08 != 0 {
        let mut i = 10;
        if flags & 0x04 != 0 {
            i += 2 + u16le(h, 10).unwrap_or(0) as usize;
        }
        let name = h
            .get(i..)
            .map(|rest| {
                let end = rest
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(rest.len())
                    .min(255);
                let mut name = String::from_utf8_lossy(&rest[..end]).into_owned();
                if !rest.is_empty() && rest.get(end) != Some(&0) {
                    name.push('…');
                }
                name
            })
            .unwrap_or_default();
        id = id.with("Original name", name);
    }
    id.done()
}

fn elf(h: &[u8]) -> Option<Identity> {
    let bits = match h.get(4) {
        Some(1) => "32-bit",
        Some(2) => "64-bit",
        _ => "",
    };
    let read16 = |at| match h.get(5) {
        Some(1) => u16le(h, at),
        Some(2) => u16be(h, at),
        _ => None,
    };
    let kind = match read16(16) {
        Some(1) => "relocatable object",
        Some(2) => "executable",
        Some(3) => "shared library or position-independent executable",
        Some(4) => "core dump",
        _ => "",
    };
    let machine = match read16(18) {
        Some(0x03) => "x86",
        Some(0x3e) => "x86-64",
        Some(0x28) => "ARM",
        Some(0xb7) => "AArch64",
        Some(0xf3) => "RISC-V",
        Some(0x08) => "MIPS",
        Some(0x14) | Some(0x15) => "PowerPC",
        Some(0xf7) => "eBPF",
        _ => "",
    };
    Id::new("ELF program (Linux/Unix)", Category::Executable)
        .with("Kind", kind)
        .with(
            "Architecture",
            format!("{machine} {bits}").trim().to_owned(),
        )
        .with(
            "Byte order",
            match h.get(5) {
                Some(1) => "little-endian",
                Some(2) => "big-endian",
                _ => "",
            },
        )
        .done()
}

fn pe(h: &[u8]) -> Option<Identity> {
    let mut id = Id::new("Windows program (MZ)", Category::Executable);
    if let Some(pe) = u32le(h, 0x3c).map(|v| v as usize)
        && let Some(header) = h.get(pe..)
        && header.starts_with(b"PE\0\0")
    {
        let machine = match u16le(header, 4) {
            Some(0x8664) => "x86-64",
            Some(0x14c) => "x86",
            Some(0xaa64) => "ARM64",
            Some(0x1c4) => "ARM",
            _ => "",
        };
        let dll = u16le(header, 22).is_some_and(|c| c & 0x2000 != 0);
        id.name = if dll {
            "Windows library (DLL)".into()
        } else {
            "Windows program (PE)".into()
        };
        id = id.with("Architecture", machine);
    }
    id.done()
}

fn mach_o(h: &[u8]) -> Option<Identity> {
    let magic = u32be(h, 0)?;
    let (bits, little) = match magic {
        0xfeedface => ("32-bit", false),
        0xfeedfacf => ("64-bit", false),
        0xcefaedfe => ("32-bit", true),
        0xcffaedfe => ("64-bit", true),
        0xcafebabe => {
            // Shared by universal Mach-O binaries and Java classes; the next word disambiguates.
            let next = u32be(h, 4)?;
            if next < 30 {
                return Id::new("Universal macOS program (Mach-O)", Category::Executable)
                    .with("Architectures", next.to_string())
                    .done();
            }
            return Id::new("Java class file", Category::Executable)
                .with(
                    "Class version",
                    u16be(h, 6).map(|v| v.to_string()).unwrap_or_default(),
                )
                .done();
        }
        _ => return None,
    };
    let cpu = if little { u32le(h, 4) } else { u32be(h, 4) };
    let machine = match cpu {
        Some(0x0100_0007) => "x86-64",
        Some(7) => "x86",
        Some(0x0100_000c) => "ARM64",
        Some(12) => "ARM",
        _ => "",
    };
    let kind = match if little { u32le(h, 12) } else { u32be(h, 12) } {
        Some(1) => "relocatable object",
        Some(2) => "executable",
        Some(4) => "core dump",
        Some(6) => "shared library",
        Some(8) => "bundle",
        _ => "",
    };
    Id::new("macOS program (Mach-O)", Category::Executable)
        .with("Kind", kind)
        .with(
            "Architecture",
            format!("{machine} {bits}").trim().to_owned(),
        )
        .done()
}

/// Width and height from the first JPEG start-of-frame marker within `h`.
fn jpeg_size(h: &[u8]) -> Option<(u16, u16)> {
    let mut rest = h.get(2..)?;
    while let Some((&0xff, tail)) = rest.split_first() {
        let (&marker, _) = tail.split_first()?;
        if marker == 0xff {
            rest = tail;
            continue;
        }
        if matches!(marker, 0xda | 0xd9) {
            return None;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            rest = rest.get(2..)?;
            continue;
        }
        let len = usize::from(u16be(rest, 2)?);
        if len < 2 {
            return None;
        }
        let segment = rest.get(2..2 + len)?;
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            return Some((u16be(segment, 5)?, u16be(segment, 3)?));
        }
        rest = rest.get(2 + len..)?;
    }
    None
}

/// Photoshop document header: version, size, depth and colour mode. Layers are not read.
fn psd(h: &[u8]) -> Option<Identity> {
    let name = match u16be(h, 4) {
        Some(1) => "Photoshop document (PSD)",
        Some(2) => "Photoshop large document (PSB)",
        _ => return Id::new("Photoshop document (unknown version)", Category::Image).done(),
    };
    let mut id = Id::new(name, Category::Image);
    if let (Some(ch), Some(hh), Some(w), Some(depth), Some(mode)) = (
        u16be(h, 12),
        u32be(h, 14),
        u32be(h, 18),
        u16be(h, 22),
        u16be(h, 24),
    ) {
        let mode = match mode {
            0 => "bitmap",
            1 => "greyscale",
            2 => "indexed",
            3 => "RGB",
            4 => "CMYK",
            7 => "multichannel",
            8 => "duotone",
            9 => "Lab",
            _ => "unknown mode",
        };
        id = id
            .with("Dimensions", format!("{w} × {hh} pixels"))
            .with("Colour", format!("{mode}, {depth}-bit, {ch} channels"));
    }
    id.done()
}

/// Largest image size given by an `ispe` property in the probe: a HEIF or AVIF image made of
/// tiles lists the tiles' size too, the whole picture is the largest.
fn heif_size(h: &[u8]) -> Option<(u32, u32)> {
    let mut best: Option<(u32, u32)> = None;
    let mut from = 0;
    while let Some(i) = h.get(from..).and_then(|rest| find(rest, b"ispe")) {
        let at = from + i;
        if let (Some(w), Some(hh)) = (u32be(h, at + 8), u32be(h, at + 12))
            && w > 0
            && hh > 0
            && best
                .is_none_or(|(bw, bh)| u64::from(w) * u64::from(hh) > u64::from(bw) * u64::from(bh))
        {
            best = Some((w, hh));
        }
        from = at + 4;
    }
    best
}

fn find(h: &[u8], needle: &[u8]) -> Option<usize> {
    h.windows(needle.len()).position(|w| w == needle)
}

/// "2026-10-30 12:01 UTC" for Unix seconds (proleptic Gregorian, no time zone data needed).
pub fn utc(secs: i64) -> String {
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
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
}

fn version(h: &[u8], offset: usize) -> String {
    match h.get(offset..).and_then(|tail| tail.get(..2)) {
        Some([major, minor]) => format!("{major}.{minor}"),
        _ => String::new(),
    }
}
