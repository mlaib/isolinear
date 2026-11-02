use super::reader::{
    Compression, EXTRACT_LIMIT, extract_stream, extract_tar, extract_zip, list_tar, list_zip, peek,
};
use std::io::{Cursor, Write};

const NEVER: &dyn Fn() -> bool = &|| false;

/// A minimal ZIP writer: stored entries, a central directory and the end record.
fn zip(entries: &[(&str, &[u8], bool)], comment: &str, declared_size: Option<u32>) -> Vec<u8> {
    zip_with(entries, comment, declared_size, 0)
}
/// Compress `data` as ZIP method `method` does: stored, deflate, bzip2, LZMA (with ZIP's
/// 4-byte header and the 5 property bytes) or xz.
fn compress(method: u16, data: &[u8]) -> Vec<u8> {
    match method {
        0 => data.to_vec(),
        8 => {
            let mut d = flate2::write::DeflateEncoder::new(vec![], flate2::Compression::default());
            d.write_all(data).unwrap();
            d.finish().unwrap()
        }
        12 => {
            let mut e = bzip2::write::BzEncoder::new(vec![], bzip2::Compression::default());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        }
        14 => {
            let options = lzma_rust2::LzmaOptions::with_preset(6);
            let mut out = vec![9, 20, 5, 0];
            let w = lzma_rust2::LzmaWriter::new_no_header(vec![], &options, false).unwrap();
            out.push(w.props());
            out.extend(options.dict_size.to_le_bytes());
            let mut w = w;
            w.write_all(data).unwrap();
            out.extend(w.finish().unwrap());
            out
        }
        95 => xz(data),
        93 => zstd::encode_all(data, 1).unwrap(),
        m => panic!("method {m}"),
    }
}
fn xz(data: &[u8]) -> Vec<u8> {
    let mut w = lzma_rust2::XzWriter::new(vec![], lzma_rust2::XzOptions::with_preset(6)).unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap()
}
fn bz2(data: &[u8]) -> Vec<u8> {
    compress(12, data)
}
/// The same with every entry compressed by `method`.
fn zip_with(
    entries: &[(&str, &[u8], bool)],
    comment: &str,
    declared_size: Option<u32>,
    method: u16,
) -> Vec<u8> {
    let mut out = vec![];
    let mut central = vec![];
    for (name, original, encrypted) in entries {
        let offset = out.len() as u32;
        let flags: u16 = if *encrypted { 1 } else { 0 };
        let size = declared_size.unwrap_or(original.len() as u32);
        let mut crc = flate2::Crc::new();
        crc.update(original);
        let compressed = compress(method, original);
        let data: &[u8] = &compressed;
        let header = |sig: &[u8], central: bool| {
            let mut h = sig.to_vec();
            if central {
                h.extend(20u16.to_le_bytes()); // made by
            }
            h.extend(20u16.to_le_bytes()); // needed
            h.extend(flags.to_le_bytes());
            h.extend(method.to_le_bytes());
            h.extend(((14u16 << 11) | (30 << 5)).to_le_bytes()); // 14:30
            h.extend((((2026u16 - 1980) << 9) | (10 << 5) | 30).to_le_bytes());
            h.extend(crc.sum().to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend(size.to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes()); // extra
            if central {
                h.extend([0u8; 6]); // comment len, disk, internal attrs
                h.extend(0u32.to_le_bytes()); // external attrs
                h.extend(offset.to_le_bytes());
            }
            h.extend(name.as_bytes());
            h
        };
        out.extend(header(b"PK\x03\x04", false));
        out.extend(data);
        central.extend(header(b"PK\x01\x02", true));
    }
    let cd_offset = out.len() as u32;
    out.extend(&central);
    out.extend(b"PK\x05\x06\0\0\0\0");
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(cd_offset.to_le_bytes());
    out.extend((comment.len() as u16).to_le_bytes());
    out.extend(comment.as_bytes());
    out
}

#[test]
fn zip_listing_reads_the_central_directory() {
    let bytes = zip(
        &[
            ("docs/", b"", false),
            ("docs/readme.md", b"# hi\n", false),
            ("secret.txt", b"xxxx", true),
            ("../escape.sh", b"rm", false),
        ],
        "built by a test",
        None,
    );
    let listing = list_zip(&mut Cursor::new(&bytes), NEVER).unwrap();
    assert_eq!(listing.declared, Some(4));
    assert_eq!(listing.comment, "built by a test");
    let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        ["docs/", "docs/readme.md", "secret.txt", "../escape.sh"]
    );
    assert!(listing.entries[0].dir);
    let readme = &listing.entries[1];
    assert_eq!((readme.size, readme.packed), (5, Some(5)));
    assert_eq!(readme.method, "stored");
    assert_eq!(readme.modified, "2026-10-30 14:30");
    assert!(listing.entries[2].encrypted);
    assert!(listing.entries[3].unsafe_path && !readme.unsafe_path);
    assert!(
        listing.warnings.iter().any(|w| w.contains("outside")),
        "{:?}",
        listing.warnings
    );
}

#[test]
fn zip_bombs_and_damage_are_reported() {
    let bytes = zip(&[("big.bin", b"x", false)], "", Some(4_000_000_000));
    let listing = list_zip(&mut Cursor::new(&bytes), NEVER).unwrap();
    assert!(
        listing.warnings.iter().any(|w| w.contains("fill a disk")),
        "{:?}",
        listing.warnings
    );
    let cut = &bytes[..bytes.len() - 10];
    assert!(
        list_zip(&mut Cursor::new(cut), NEVER).is_err(),
        "no end record"
    );
    assert!(list_zip(&mut Cursor::new(b"PK\x03\x04 not really"), NEVER).is_err());
}

fn tar_header(name: &str, size: u64, kind: u8, link: &str) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..107].copy_from_slice(b"0000644");
    h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
    h[136..147].copy_from_slice(format!("{:011o}", 1_793_364_516u64).as_bytes());
    h[156] = kind;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[148..156].copy_from_slice(b"        ");
    let sum: u64 = h.iter().map(|&b| b as u64).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    h
}
fn tar(entries: &[(&str, &[u8], u8, &str)]) -> Vec<u8> {
    let mut out = vec![];
    for (name, data, kind, link) in entries {
        if name.len() > 100 {
            out.extend(tar_header("././@LongLink", name.len() as u64 + 1, b'L', ""));
            let mut long = name.as_bytes().to_vec();
            long.push(0);
            long.resize(long.len().div_ceil(512) * 512, 0);
            out.extend(long);
            out.extend(tar_header(&name[..99], data.len() as u64, *kind, link));
        } else {
            out.extend(tar_header(name, data.len() as u64, *kind, link));
        }
        let mut d = data.to_vec();
        d.resize(d.len().div_ceil(512) * 512, 0);
        out.extend(d);
    }
    out.extend([0u8; 1024]);
    out
}

#[test]
fn tar_listing_walks_headers_without_keeping_data() {
    let long = format!("deep/{}/file.txt", "d".repeat(120));
    let bytes = tar(&[
        ("src/", b"", b'5', ""),
        ("src/main.rs", b"fn main() {}\n", b'0', ""),
        ("latest", b"", b'2', "/etc/passwd"),
        (&long, b"", b'0', ""),
    ]);
    let listing = list_tar(&mut Cursor::new(&bytes), u64::MAX, NEVER).unwrap();
    let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names[..3], ["src/", "src/main.rs", "latest"]);
    assert_eq!(names[3], long, "GNU long names");
    assert!(listing.entries[0].dir);
    assert_eq!(listing.entries[1].size, 13);
    assert_eq!(listing.entries[1].modified, "2026-10-30 12:48");
    assert!(listing.entries[2].unsafe_path, "absolute symlink target");
    assert!(listing.warnings.iter().any(|w| w.contains("outside")));
    assert!(!listing.truncated);
}

#[test]
fn tar_damage_and_limits_stop_the_listing() {
    let bytes = tar(&[("a.txt", b"hello", b'0', ""), ("b.txt", b"world", b'0', "")]);
    let mut bad = bytes.clone();
    bad[1024] ^= 0xff; // second header's name: checksum no longer matches
    let listing = list_tar(&mut Cursor::new(&bad), u64::MAX, NEVER).unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert!(listing.warnings.iter().any(|w| w.contains("checksum")));
    let listing = list_tar(&mut Cursor::new(&bytes[..700]), u64::MAX, NEVER).unwrap();
    assert!(
        listing.warnings.iter().any(|w| w.contains("cut short")),
        "{:?}",
        listing.warnings
    );
    let listing = list_tar(&mut Cursor::new(&bytes), 1024, NEVER).unwrap();
    assert!(listing.truncated);
}

#[test]
fn compressed_streams_are_peeked_within_a_budget() {
    let text = "line of text\n".repeat(10_000);
    let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    gz.write_all(text.as_bytes()).unwrap();
    let gz = gz.finish().unwrap();
    assert_eq!(Compression::of(&gz), Some(Compression::Gzip));
    let p = peek(Compression::Gzip, Cursor::new(&gz), 1000);
    assert_eq!(p.bytes, text.as_bytes()[..1000]);
    assert!(p.more && p.error.is_none());
    let all = peek(Compression::Gzip, Cursor::new(&gz), 1 << 20);
    assert_eq!(all.bytes.len(), text.len());
    assert!(!all.more);

    let zst = zstd::encode_all(text.as_bytes(), 3).unwrap();
    assert_eq!(Compression::of(&zst), Some(Compression::Zstd));
    assert_eq!(
        peek(Compression::Zstd, Cursor::new(&zst), 13).bytes,
        b"line of text\n"
    );

    // A bomb: 1 GiB of zeros compresses to about 1 MiB; the peek stops at its budget.
    let mut bomb = flate2::write::GzEncoder::new(vec![], flate2::Compression::best());
    let zeros = vec![0u8; 1 << 20];
    for _ in 0..64 {
        bomb.write_all(&zeros).unwrap();
    }
    let bomb = bomb.finish().unwrap();
    let p = peek(Compression::Gzip, Cursor::new(&bomb), 4096);
    assert_eq!(p.bytes.len(), 4096);
    assert!(p.more);

    let mut damaged = gz.clone();
    let mid = damaged.len() / 2;
    damaged.truncate(mid);
    let p = peek(Compression::Gzip, Cursor::new(&damaged), 1 << 20);
    assert!(!p.bytes.is_empty(), "keeps what decoded");
    assert!(p.error.is_some());
}

#[test]
fn tar_inside_gzip_lists_through_the_decoder() {
    let bytes = tar(&[("a.txt", b"hello", b'0', "")]);
    let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    gz.write_all(&bytes).unwrap();
    let gz = gz.finish().unwrap();
    let mut reader = Compression::Gzip.reader(Cursor::new(gz)).unwrap();
    let listing = list_tar(&mut reader, u64::MAX, NEVER).unwrap();
    assert_eq!(listing.entries[0].name, "a.txt");
}

#[test]
fn xz_and_bzip2_streams_are_peeked_listed_and_extracted() {
    let text = "line of text\n".repeat(10_000);
    for (compression, bytes) in [
        (Compression::Xz, xz(text.as_bytes())),
        (Compression::Bzip2, bz2(text.as_bytes())),
    ] {
        assert_eq!(Compression::of(&bytes), Some(compression));
        let p = peek(compression, Cursor::new(&bytes), 13);
        assert_eq!((p.bytes.as_slice(), p.more), (&b"line of text\n"[..], true));
        let mut out = vec![];
        extract_stream(compression, Cursor::new(&bytes), &mut out).unwrap();
        assert_eq!(out, text.as_bytes(), "{compression:?}");

        // Cut in half: what decoded before the damage is kept, with the error.
        let p = peek(compression, Cursor::new(&bytes[..bytes.len() / 2]), 1 << 20);
        assert!(p.error.is_some(), "{compression:?}");
        assert!(text.as_bytes().starts_with(&p.bytes));
        // A flipped byte is an error, never silently different text.
        let mut damaged = bytes.clone();
        let mid = damaged.len() / 2;
        damaged[mid] ^= 0x55;
        let mut out = vec![];
        let result = extract_stream(compression, Cursor::new(&damaged), &mut out);
        assert!(result.is_err() || out == text.as_bytes(), "{compression:?}");

        let tarred = tar(&[("a.txt", b"hello", b'0', "")]);
        let packed = if compression == Compression::Xz {
            xz(&tarred)
        } else {
            bz2(&tarred)
        };
        let mut reader = compression.reader(Cursor::new(&packed)).unwrap();
        let listing = list_tar(&mut reader, u64::MAX, NEVER).unwrap();
        assert_eq!(listing.entries[0].name, "a.txt");
        let mut out = vec![];
        let mut reader = compression.reader(Cursor::new(&packed)).unwrap();
        extract_tar(&mut reader, 0, u64::MAX, &mut out, NEVER).unwrap();
        assert_eq!(out, b"hello");
    }
    assert_eq!(Compression::of(b"BZh0"), None, "not a bzip2 block size");

    // Bombs stop at the budget: 64 MiB of zeros, a peek of 4 KiB and an extraction capped.
    let zeros = vec![0u8; 64 << 20];
    for (compression, bomb) in [
        (Compression::Xz, xz(&zeros)),
        (Compression::Bzip2, bz2(&zeros)),
    ] {
        let p = peek(compression, Cursor::new(&bomb), 4096);
        assert_eq!((p.bytes.len(), p.more), (4096, true));
    }
}

/// An xz stream whose LZMA2 dictionary would need 4 GiB of memory is refused, not allocated.
#[test]
fn xz_dictionaries_over_the_memory_limit_are_refused() {
    let mut bytes = xz(b"tiny");
    // Stream header (12 bytes), then the block header: size byte, flags, filter id 0x21,
    // property size 1, the dictionary byte, padding, CRC-32 of the header.
    let header_len = (bytes[12] as usize + 1) * 4;
    let block = 12..12 + header_len;
    assert_eq!(&bytes[14..16], &[0x21, 0x01], "one LZMA2 filter");
    bytes[16] = 40; // 4 GiB - 1
    let mut crc = flate2::Crc::new();
    crc.update(&bytes[block.start..block.end - 4]);
    bytes[block.end - 4..block.end].copy_from_slice(&crc.sum().to_le_bytes());
    let p = peek(Compression::Xz, Cursor::new(&bytes), 4096);
    let e = p.error.expect("refused");
    assert!(e.contains("mem"), "{e}");
    assert!(p.bytes.is_empty());

    // The same for a ZIP LZMA entry: dictionary size bytes after the property byte.
    let mut zipped = zip_with(&[("a.txt", b"tiny", false)], "", None, 14);
    let entry = list_zip(&mut Cursor::new(&zipped), NEVER).unwrap().entries[0].clone();
    let data = entry.offset as usize + 30 + "a.txt".len();
    zipped[data + 5..data + 9].copy_from_slice(&0x6000_0000u32.to_le_bytes()); // 1.5 GiB
    let e = extract_zip(&mut Cursor::new(&zipped), &entry, &mut vec![]).unwrap_err();
    assert!(e.to_string().contains("memory"), "{e}");
}

/// Files made by the reference tools: `xz`, `bzip2` and Python's zipfile (bzip2 and LZMA
/// entries); see TEST_REPORT for the commands.
#[test]
fn files_from_the_reference_tools_decode() {
    let fixture = |name: &str| {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/archives")
                .join(name),
        )
        .unwrap()
    };
    let expected = fixture("compressed.txt");
    for (name, compression) in [
        ("compressed.txt.xz", Compression::Xz),
        ("compressed.txt.bz2", Compression::Bzip2),
    ] {
        let bytes = fixture(name);
        assert_eq!(Compression::of(&bytes), Some(compression));
        let mut out = vec![];
        extract_stream(compression, Cursor::new(&bytes), &mut out).unwrap();
        assert_eq!(out, expected, "{name}");
    }
    let bytes = fixture("methods.zip");
    let listing = list_zip(&mut Cursor::new(&bytes), NEVER).unwrap();
    let methods: Vec<&str> = listing.entries.iter().map(|e| e.method.as_str()).collect();
    assert_eq!(methods, ["stored", "deflate", "bzip2", "LZMA"]);
    for entry in &listing.entries {
        let mut out = vec![];
        extract_zip(&mut Cursor::new(&bytes), entry, &mut out).unwrap();
        assert_eq!(out, expected, "{}", entry.name);
    }
}

#[test]
fn random_archives_never_panic() {
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let good_zip = zip(&[("a", b"abc", false), ("b/", b"", false)], "c", None);
    let good_tar = tar(&[("a.txt", b"hello", b'0', "")]);
    for _ in 0..10_000 {
        for base in [&good_zip, &good_tar] {
            let mut b = base.clone();
            for _ in 0..(next() % 8) {
                let i = (next() % b.len() as u64) as usize;
                b[i] = next() as u8;
            }
            b.truncate((next() % (b.len() as u64 + 1)) as usize);
            if let Ok(listing) = list_zip(&mut Cursor::new(&b), NEVER) {
                for entry in listing.entries.iter().take(4) {
                    let _ = extract_zip(&mut Cursor::new(&b), entry, &mut vec![]);
                }
            }
            let _ = extract_tar(&mut Cursor::new(&b), 0, 1 << 20, &mut vec![], NEVER);
            let _ = list_tar(&mut Cursor::new(&b), 1 << 20, NEVER);
            let _ = peek(Compression::Gzip, Cursor::new(&b), 4096);
            let _ = peek(Compression::Zstd, Cursor::new(&b), 4096);
            let _ = peek(Compression::Xz, Cursor::new(&b), 4096);
            let _ = peek(Compression::Bzip2, Cursor::new(&b), 4096);
        }
    }
}

#[test]
fn zip_entries_are_extracted_checked_and_bounded() {
    let text = "readme line\n".repeat(500);
    for method in [0, 8, 12, 14, 93, 95] {
        let bytes = zip_with(
            &[
                ("docs/", b"", false),
                ("docs/readme.md", text.as_bytes(), false),
                ("secret.txt", b"xxxx", true),
            ],
            "",
            None,
            method,
        );
        let listing = list_zip(&mut Cursor::new(&bytes), NEVER).unwrap();
        let [dir, readme, secret] = &listing.entries[..] else {
            panic!("{listing:?}")
        };
        assert!(
            readme.openable && !dir.openable && !secret.openable,
            "{method}"
        );
        let mut out = vec![];
        let n = extract_zip(&mut Cursor::new(&bytes), readme, &mut out).unwrap();
        assert_eq!(
            (n, out.as_slice()),
            (text.len() as u64, text.as_bytes()),
            "method {method}"
        );
        assert!(extract_zip(&mut Cursor::new(&bytes), dir, &mut vec![]).is_err());
        let e = extract_zip(&mut Cursor::new(&bytes), secret, &mut vec![]).unwrap_err();
        assert!(e.to_string().contains("encrypted"), "{e}");

        // A flipped data byte fails the checksum (or the decoder); never silently wrong text.
        let mut damaged = bytes.clone();
        let at = readme.offset as usize
            + 30
            + "docs/readme.md".len()
            + readme.packed.unwrap() as usize / 2;
        damaged[at] ^= 0x55;
        assert!(
            extract_zip(&mut Cursor::new(&damaged), readme, &mut vec![]).is_err(),
            "method {method}"
        );
    }
    // An entry that expands past its declared size is cut off at the declaration.
    for method in [8, 12, 14, 93, 95] {
        let bytes = zip_with(&[("a.txt", b"0123456789", false)], "", None, method);
        let mut entry = list_zip(&mut Cursor::new(&bytes), NEVER).unwrap().entries[0].clone();
        entry.size = 4;
        let mut out = vec![];
        assert!(extract_zip(&mut Cursor::new(&bytes), &entry, &mut out).is_err());
        assert!(out.len() <= 4, "method {method}");
    }
    let bytes = zip_with(&[("a.txt", b"0123456789", false)], "", None, 8);
    let mut entry = list_zip(&mut Cursor::new(&bytes), NEVER).unwrap().entries[0].clone();
    entry.size = 4;
    let mut out = vec![];
    let e = extract_zip(&mut Cursor::new(&bytes), &entry, &mut out).unwrap_err();
    assert!(e.to_string().contains("declared"), "{e}");
    assert!(out.len() <= 4);
    // A declared size over the limit is refused before reading.
    let bytes = zip(&[("big.bin", b"x", false)], "", Some(4_000_000_000));
    let entry = &list_zip(&mut Cursor::new(&bytes), NEVER).unwrap().entries[0];
    assert!(entry.size > EXTRACT_LIMIT);
    let e = extract_zip(&mut Cursor::new(&bytes), entry, &mut vec![]).unwrap_err();
    assert!(e.to_string().contains("opens entries up to"), "{e}");
}

#[test]
fn tar_entries_are_extracted_by_index() {
    let bytes = tar(&[
        ("src/", b"", b'5', ""),
        ("src/main.rs", b"fn main() {}\n", b'0', ""),
        ("latest", b"", b'2', "/etc/passwd"),
        ("b.txt", b"second", b'0', ""),
    ]);
    let mut out = vec![];
    extract_tar(&mut Cursor::new(&bytes), 1, u64::MAX, &mut out, NEVER).unwrap();
    assert_eq!(out, b"fn main() {}\n");
    let mut out = vec![];
    extract_tar(&mut Cursor::new(&bytes), 3, u64::MAX, &mut out, NEVER).unwrap();
    assert_eq!(out, b"second");
    for index in [0, 2] {
        let e = extract_tar(
            &mut Cursor::new(&bytes),
            index,
            u64::MAX,
            &mut vec![],
            NEVER,
        );
        assert!(e.is_err(), "folders and links are not opened");
    }
    assert!(extract_tar(&mut Cursor::new(&bytes), 9, u64::MAX, &mut vec![], NEVER).is_err());
    let e = extract_tar(
        &mut Cursor::new(&bytes[..1030]),
        1,
        u64::MAX,
        &mut vec![],
        NEVER,
    );
    assert!(e.is_err(), "cut short");
    let listing = list_tar(&mut Cursor::new(&bytes), u64::MAX, NEVER).unwrap();
    assert_eq!(
        listing
            .entries
            .iter()
            .map(|e| e.openable)
            .collect::<Vec<_>>(),
        [false, true, false, true]
    );
}

#[test]
fn compressed_streams_are_extracted_up_to_the_limit() {
    let text = "line of text\n".repeat(10_000);
    let zst = zstd::encode_all(text.as_bytes(), 3).unwrap();
    let mut out = vec![];
    extract_stream(Compression::Zstd, Cursor::new(&zst), &mut out).unwrap();
    assert_eq!(out, text.as_bytes());
    // Larger than the limit: refused once the limit is reached, never more written.
    let mut bomb = flate2::write::GzEncoder::new(vec![], flate2::Compression::best());
    let zeros = vec![0u8; 1 << 20];
    for _ in 0..(EXTRACT_LIMIT >> 20) + 1 {
        bomb.write_all(&zeros).unwrap();
    }
    let bomb = bomb.finish().unwrap();
    let mut counted = Counter(0);
    let e = extract_stream(Compression::Gzip, Cursor::new(&bomb), &mut counted).unwrap_err();
    assert!(e.to_string().contains("larger than"), "{e}");
    assert!(counted.0 <= EXTRACT_LIMIT);
}
struct Counter(u64);
impl Write for Counter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "isolinear-archives-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
    fn output(&self) -> std::path::PathBuf {
        let path = self.0.join("output");
        std::fs::create_dir(&path).unwrap();
        path
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut w = flate2::write::GzEncoder::new(vec![], flate2::Compression::fast());
    w.write_all(bytes).unwrap();
    w.finish().unwrap()
}

#[test]
fn table_schema_pages_notes_and_bomb_fact() {
    let temp = Temp::new();
    let path = temp.write(
        "a.zip",
        &zip(
            &[
                ("folder/", b"", false),
                ("../a.txt", b"abc", false),
                ("secret", b"", true),
            ],
            "comment",
            Some(4_000_000_000),
        ),
    );
    let mut table = super::open("zip", &path, None).unwrap();
    assert_eq!(table.schema().rows, Some(3));
    assert_eq!(
        table
            .schema()
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.type_name.as_str()))
            .collect::<Vec<_>>(),
        [
            ("Name", "text"),
            ("Size", "text"),
            ("Packed", "text"),
            ("Modified", "text"),
            ("Method", "text"),
            ("Notes", "text")
        ]
    );
    assert!(
        table
            .schema()
            .details
            .iter()
            .any(|(_, value)| value.contains("Possible decompression bomb"))
    );
    assert!(
        table
            .schema()
            .details
            .iter()
            .any(|(key, value)| key == "Comment" && value == "comment")
    );
    let rows = table.rows(0, usize::MAX).unwrap();
    assert!(rows[0][5].as_deref().unwrap().contains("folder"));
    assert!(rows[1][5].as_deref().unwrap().contains("unsafe path"));
    assert!(rows[2][5].as_deref().unwrap().contains("encrypted"));
    assert_eq!(table.rows(1, 1).unwrap(), rows[1..2]);
    assert!(table.rows(u64::MAX, usize::MAX).unwrap().is_empty());
    assert!(table.rows(0, 0).unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(&temp.0).unwrap().count(),
        1,
        "listing writes nothing"
    );
}

#[test]
fn table_cells_are_cut_but_extraction_keeps_the_full_name_component() {
    let temp = Temp::new();
    let long = format!("{}/a.txt", "d".repeat(1500));
    let path = temp.write("a.tar", &tar(&[(&long, b"abc", b'0', "")]));
    let mut table = super::open("tar", &path, None).unwrap();
    assert!(
        table.rows(0, 1).unwrap()[0][0]
            .as_deref()
            .unwrap()
            .contains("characters")
    );
    let extracted = super::extract("tar", &path, 0, &temp.output()).unwrap();
    assert_eq!(extracted.file_name().unwrap(), "a.txt");
    assert_eq!(std::fs::read(extracted).unwrap(), b"abc");
}

#[test]
fn every_compression_lists_tar_and_extracts_a_member() {
    let bytes = tar(&[("dir/", b"", b'5', ""), ("dir/a.txt", b"hello", b'0', "")]);
    for (format, filename, packed) in [
        ("gzip", "a.tar.gz", gzip(&bytes)),
        (
            "zstd",
            "a.tar.zst",
            zstd::encode_all(&bytes[..], 1).unwrap(),
        ),
        ("xz", "a.tar.xz", xz(&bytes)),
        ("bzip2", "a.tar.bz2", bz2(&bytes)),
    ] {
        let temp = Temp::new();
        let path = temp.write(filename, &packed);
        let mut table = super::open(format, &path, None).unwrap();
        assert_eq!(table.schema().rows, Some(2));
        assert_eq!(
            table.rows(1, 1).unwrap()[0][0].as_deref(),
            Some("dir/a.txt")
        );
        let output = temp.output();
        assert!(super::extract(format, &path, 0, &output).is_err());
        let extracted = super::extract(format, &path, 1, &output).unwrap();
        assert_eq!(std::fs::read(extracted).unwrap(), b"hello");
        let renamed = temp.write("no-tar-name", &packed);
        assert_eq!(
            super::open(format, &renamed, None).unwrap().schema().rows,
            Some(2),
            "tar detected from header"
        );
    }
}

#[test]
fn single_stream_names_unknown_sizes_and_concatenated_members() {
    let data = b"hello\n";
    let mut gz = flate2::GzBuilder::new()
        .filename("../recorded.txt")
        .mtime(1_793_364_516)
        .write(vec![], flate2::Compression::fast());
    gz.write_all(data).unwrap();
    for (format, filename, inner, mut packed) in [
        ("gzip", "wrong.gz", "../recorded.txt", gz.finish().unwrap()),
        (
            "zstd",
            "a.txt.zst",
            "a.txt",
            zstd::encode_all(&data[..], 1).unwrap(),
        ),
        ("xz", "a.txt.xz", "a.txt", xz(data)),
        ("bzip2", "a.txt.bz2", "a.txt", bz2(data)),
    ] {
        packed.extend(packed.clone());
        let temp = Temp::new();
        let path = temp.write(filename, &packed);
        let mut table = super::open(format, &path, None).unwrap();
        assert_eq!(table.schema().rows, Some(1));
        let rows = table.rows(0, 1).unwrap();
        assert_eq!(rows[0][0].as_deref(), Some(inner));
        assert_eq!(rows[0][1], None);
        if format == "gzip" {
            assert_eq!(rows[0][3].as_deref(), Some("2026-10-30 12:48"));
        }
        assert_eq!(rows[0][2], Some(packed.len().to_string()));
        let extracted = super::extract(format, &path, 0, &temp.output()).unwrap();
        assert_eq!(std::fs::read(extracted).unwrap(), data.repeat(2));
    }
}

#[test]
fn extraction_flattens_paths_and_never_overwrites() {
    let temp = Temp::new();
    let path = temp.write(
        "a.zip",
        &zip(&[("../../outside\\a\n.txt", b"hello", false)], "", None),
    );
    let output = temp.output();
    let result = super::extract("zip", &path, 0, &output).unwrap();
    assert_eq!(result, output.join("a.txt"));
    assert_eq!(std::fs::read(&result).unwrap(), b"hello");
    assert!(super::extract("zip", &path, 0, &output).is_err());
    assert_eq!(std::fs::read(&result).unwrap(), b"hello");
    assert!(super::extract("zip", &path, u64::MAX, &output).is_err());
    assert_eq!(super::safe_name("C:"), "C");
    for name in ["..", ".", "NUL", "COM1.txt", "\\"] {
        let safe = super::safe_name(name);
        assert_eq!(safe, "entry", "{name}");
    }
}

#[cfg(unix)]
#[test]
fn extraction_refuses_existing_symlinks() {
    let temp = Temp::new();
    let path = temp.write("a.zip", &zip(&[("a.txt", b"new", false)], "", None));
    let outside = temp.write("outside", b"original");
    let output = temp.output();
    std::os::unix::fs::symlink(&outside, output.join("a.txt")).unwrap();
    assert!(super::extract("zip", &path, 0, &output).is_err());
    assert_eq!(std::fs::read(outside).unwrap(), b"original");
    assert!(output.join("a.txt").is_symlink());
}

#[test]
fn failed_crc_and_size_checks_delete_partial_files() {
    let temp = Temp::new();
    let output = temp.output();
    for declared in [None, Some(2), Some(100)] {
        let mut bytes = zip(&[("a.txt", b"hello", false)], "", declared);
        if declared.is_none() {
            bytes[35] ^= 1;
        }
        let path = temp.write("a.zip", &bytes);
        assert!(super::extract("zip", &path, 0, &output).is_err());
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 0);
    }
}

#[test]
fn cancelled_listings_and_extractions_stop_and_clean_up() {
    let temp = Temp::new();
    let path = temp.write(
        "a.zip",
        &zip(&[("a.txt", &vec![b'x'; 128 * 1024], false)], "", None),
    );
    assert!(
        super::open_with_cancel("zip", &path, None, &|| true)
            .err()
            .unwrap()
            .contains("cancelled")
    );
    let output = temp.output();
    let result = super::extract_with_cancel("zip", &path, 0, &output, &|| {
        output.join("a.txt").metadata().is_ok_and(|m| m.len() > 0)
    });
    assert!(result.unwrap_err().contains("cancelled"));
    assert_eq!(std::fs::read_dir(output).unwrap().count(), 0);
    assert!(list_tar(&mut Cursor::new(tar(&[])), 1024, &|| true).is_err());
}

#[test]
fn unix_zip_links_and_devices_are_rows_never_files() {
    for mode in [0xa000u32, 0x2000, 0x6000, 0x1000, 0xc000, 0x4000] {
        let temp = Temp::new();
        let mut bytes = zip(&[("link", b"/etc/passwd", false)], "", None);
        let cd = bytes.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
        bytes[cd + 5] = 3;
        bytes[cd + 38..cd + 42].copy_from_slice(&(mode << 16).to_le_bytes());
        let path = temp.write("a.zip", &bytes);
        let mut table = super::open("zip", &path, None).unwrap();
        assert!(
            !table.rows(0, 1).unwrap()[0][5]
                .as_deref()
                .unwrap()
                .is_empty()
        );
        assert!(super::extract("zip", &path, 0, &temp.output()).is_err());
    }
}

fn end64(bytes: &mut Vec<u8>, directory: u64, size: u64, count: u64, comment: &[u8]) {
    let record = bytes.len() as u64;
    bytes.extend(b"PK\x06\x06");
    bytes.extend(44u64.to_le_bytes());
    bytes.extend([45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend(count.to_le_bytes());
    bytes.extend(count.to_le_bytes());
    bytes.extend(size.to_le_bytes());
    bytes.extend(directory.to_le_bytes());
    bytes.extend(b"PK\x06\x07\0\0\0\0");
    bytes.extend(record.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(b"PK\x05\x06\0\0\0\0");
    bytes.extend([0xff; 12]);
    bytes.extend((comment.len() as u16).to_le_bytes());
    bytes.extend(comment);
}

fn zip64(comment: &[u8]) -> Vec<u8> {
    let mut bytes = zip(&[("a.txt", b"hello", false)], "", None);
    bytes.truncate(bytes.len() - 22);
    let cd = bytes.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
    bytes[cd + 20..cd + 28].fill(0xff);
    bytes[cd + 42..cd + 46].fill(0xff);
    bytes[cd + 30..cd + 32].copy_from_slice(&28u16.to_le_bytes());
    bytes.extend([1, 0, 24, 0]);
    bytes.extend(5u64.to_le_bytes());
    bytes.extend(5u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    let size = (bytes.len() - cd) as u64;
    end64(&mut bytes, cd as u64, size, 1, comment);
    bytes
}

#[test]
fn zip64_sizes_offsets_and_maximum_comment() {
    for comment in [vec![], vec![b'c'; 65535]] {
        let bytes = zip64(&comment);
        let listing = list_zip(&mut Cursor::new(&bytes), NEVER).unwrap();
        assert_eq!(listing.entries[0].size, 5);
        assert_eq!(listing.entries[0].packed, Some(5));
        assert_eq!(listing.comment.len(), comment.len());
        let mut out = vec![];
        extract_zip(&mut Cursor::new(bytes), &listing.entries[0], &mut out).unwrap();
        assert_eq!(out, b"hello");
    }
}

#[test]
fn malformed_zip64_lengths_and_offsets_are_refused() {
    let good = zip64(&[]);
    let cd = good.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
    for (at, replacement) in [
        (cd + 30, vec![0, 0]),
        (cd + 53, vec![8, 0]),
        (good.len() - 34, u64::MAX.to_le_bytes().to_vec()),
    ] {
        let mut bytes = good.clone();
        bytes[at..at + replacement.len()].copy_from_slice(&replacement);
        assert!(list_zip(&mut Cursor::new(bytes), NEVER).is_err());
    }
    let listing = list_zip(&mut Cursor::new(&good), NEVER).unwrap();
    let mut entry = listing.entries[0].clone();
    entry.offset = u64::MAX;
    assert!(extract_zip(&mut Cursor::new(&good), &entry, &mut vec![]).is_err());
    let mut bad = good.clone();
    let end = bad.len() - 22;
    bad[end + 4] = 1;
    assert!(list_zip(&mut Cursor::new(bad), NEVER).is_err());
}

#[test]
fn zip_entry_limit_is_reported() {
    let one = zip(&[("a", b"", false)], "", None);
    let cd = one.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
    let record = &one[cd..one.len() - 22];
    let count = super::reader::MAX_ENTRIES as u64 + 1;
    let mut bytes = record.repeat(count as usize);
    let size = bytes.len() as u64;
    end64(&mut bytes, 0, size, count, &[]);
    let listing = list_zip(&mut Cursor::new(bytes), NEVER).unwrap();
    assert_eq!(listing.entries.len(), super::reader::MAX_ENTRIES);
    assert!(listing.truncated);
    assert!(listing.warnings.iter().any(|w| w.contains("200000")));
}

fn pax(key: &str, value: &str) -> Vec<u8> {
    let mut size = key.len() + value.len() + 4;
    loop {
        let text = format!("{size} {key}={value}\n");
        if text.len() == size {
            return text.into_bytes();
        }
        size = text.len();
    }
}

#[test]
fn pax_paths_newlines_links_and_sizes() {
    let mut fields = pax("path", "deep/name\nwith newline.txt");
    fields.extend(pax("size", "5"));
    let mut bytes = tar(&[("pax", &fields, b'x', ""), ("ignored", b"hello", b'0', "")]);
    // The PAX size overrides the next regular header's value.
    bytes[1024..1536].copy_from_slice(&tar_header("ignored", 0, b'0', ""));
    let listing = list_tar(&mut Cursor::new(&bytes), u64::MAX, NEVER).unwrap();
    assert_eq!(listing.entries[0].name, "deep/name\nwith newline.txt");
    assert_eq!(listing.entries[0].size, 5);
    let mut out = vec![];
    extract_tar(&mut Cursor::new(&bytes), 0, u64::MAX, &mut out, NEVER).unwrap();
    assert_eq!(out, b"hello");
    let fields = pax("linkpath", "../../outside");
    for kind in [b'1', b'2'] {
        let bytes = tar(&[("pax", &fields, b'x', ""), ("link", b"", kind, "")]);
        let listing = list_tar(&mut Cursor::new(bytes), u64::MAX, NEVER).unwrap();
        assert!(listing.entries[0].unsafe_path);
        assert!(listing.entries[0].special.contains("../../outside"));
    }
}

#[test]
fn tar_extensions_cannot_bypass_scan_budget() {
    for kind in [b'L', b'K', b'x', b'g'] {
        let bytes = tar(&[("extension", &[b'x'; 2048], kind, "")]);
        let mut cursor = Cursor::new(bytes);
        let listing = list_tar(&mut cursor, 1024, NEVER).unwrap();
        assert!(listing.truncated);
        assert!(listing.warnings.iter().any(|w| w.contains("scan budget")));
        assert_eq!(cursor.position(), 512);
    }
    let bytes = tar(&[("long", &[b'x'; 65537], b'L', "")]);
    let listing = list_tar(&mut Cursor::new(bytes), u64::MAX, NEVER).unwrap();
    assert!(listing.warnings.iter().any(|w| w.contains("64 KiB")));
}

#[test]
fn malformed_tar_numbers_and_pax_lengths_are_refused() {
    for field in [
        [0xff; 12],
        [
            0x80, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ],
        [b'9'; 12],
    ] {
        let mut header = tar_header("a", 0, b'0', "");
        header[124..136].copy_from_slice(&field);
        header[148..156].fill(b' ');
        let sum: u64 = header.iter().map(|&b| b as u64).sum();
        header[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        assert!(list_tar(&mut Cursor::new(header), u64::MAX, NEVER).is_err());
    }
    for data in [
        b"0 path=a\n".as_slice(),
        b"999 path=a\n",
        b"11 path=a",
        b"x path=a\n",
    ] {
        let bytes = tar(&[("pax", data, b'x', "")]);
        assert!(list_tar(&mut Cursor::new(bytes), u64::MAX, NEVER).is_err());
    }
}

#[test]
fn tar_entry_limit_is_reported() {
    let mut bytes = tar_header("a", 0, b'0', "").repeat(super::reader::MAX_ENTRIES + 1);
    bytes.extend([0; 1024]);
    let listing = list_tar(&mut Cursor::new(bytes), u64::MAX, NEVER).unwrap();
    assert_eq!(listing.entries.len(), super::reader::MAX_ENTRIES);
    assert!(listing.truncated);
    assert!(listing.warnings.iter().any(|w| w.contains("200000")));
}

#[test]
fn plain_tar_listing_seeks_past_payloads() {
    struct Count {
        cursor: Cursor<Vec<u8>>,
        read: usize,
    }
    impl std::io::Read for Count {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = self.cursor.read(out)?;
            self.read += n;
            Ok(n)
        }
    }
    impl std::io::Seek for Count {
        fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
            self.cursor.seek(pos)
        }
    }
    let bytes = tar(&[
        ("a.bin", &vec![42; 1 << 20], b'0', ""),
        ("b", b"hi", b'0', ""),
    ]);
    let mut counted = Count {
        cursor: Cursor::new(bytes),
        read: 0,
    };
    let listing = super::reader::list_tar_file(&mut counted, NEVER).unwrap();
    assert_eq!(listing.entries.len(), 2);
    assert_eq!(counted.read, 3 * 512);
}

#[test]
fn directory_budget_and_corrupted_offsets() {
    let temp = Temp::new();
    let path = temp.0.join("large.zip");
    let mut file = std::fs::File::create(&path).unwrap();
    let size = 64 * 1024 * 1024 + 1;
    file.set_len(size).unwrap();
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(size)).unwrap();
    let mut end = Vec::new();
    end64(&mut end, 0, size, 0, &[]);
    // This suffix is written after the sparse directory, not at offset zero.
    end[64..72].copy_from_slice(&size.to_le_bytes());
    file.write_all(&end).unwrap();
    drop(file);
    let table = super::open("zip", &path, None).unwrap();
    assert!(
        table
            .schema()
            .details
            .iter()
            .any(|(_, text)| text.contains("64 MiB"))
    );
    let mut bytes = zip(&[("a", b"hello", false)], "", None);
    let end = bytes.len() - 22;
    bytes[end + 16..end + 20].copy_from_slice(&0xffff_fffeu32.to_le_bytes());
    assert!(list_zip(&mut Cursor::new(bytes), NEVER).is_err());
}

#[test]
fn tar_extraction_obeys_scan_budget_for_the_target_payload() {
    let bytes = tar(&[("a", &[b'x'; 1024], b'0', "")]);
    let mut out = vec![];
    let error = extract_tar(&mut Cursor::new(bytes), 0, 1024, &mut out, NEVER).unwrap_err();
    assert!(error.to_string().contains("scan budget"));
    assert!(out.is_empty());
}

#[test]
fn damaged_stream_extraction_removes_the_partial_copy() {
    let temp = Temp::new();
    let mut bytes = gzip(&vec![b'x'; 128 * 1024]);
    let trailer = bytes.len() - 8;
    bytes[trailer] ^= 1;
    let path = temp.write("a.gz", &bytes);
    assert_eq!(
        super::open("gzip", &path, None).unwrap().schema().rows,
        Some(1)
    );
    let output = temp.output();
    assert!(super::extract("gzip", &path, 0, &output).is_err());
    assert_eq!(std::fs::read_dir(output).unwrap().count(), 0);
}

#[test]
fn unsupported_and_truncated_inputs_return_errors() {
    let temp = Temp::new();
    let path = temp.write("a", b"not an archive");
    assert!(super::open("unknown", &path, None).is_err());
    for format in ["zip", "gzip", "zstd", "xz", "bzip2"] {
        assert!(super::open(format, &path, None).is_err(), "{format}");
    }
    let good = zip(&[("a", b"abc", false)], "", None);
    let cd = good.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
    let mut bad = good.clone();
    bad[cd + 28..cd + 30].copy_from_slice(&65535u16.to_le_bytes());
    assert!(list_zip(&mut Cursor::new(bad), NEVER).is_err());
    let mut bad = good.clone();
    bad[cd + 10..cd + 12].copy_from_slice(&99u16.to_le_bytes());
    let path = temp.write("a.zip", &bad);
    let mut table = super::open("zip", &path, None).unwrap();
    assert!(
        table.rows(0, 1).unwrap()[0][5]
            .as_deref()
            .unwrap()
            .contains("not supported")
    );
    assert!(super::extract("zip", &path, 0, &temp.output()).is_err());
}

#[test]
fn classic_zip_can_have_exactly_65535_entries() {
    let one = zip(&[("a", b"", false)], "", None);
    let cd = one.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
    let mut bytes = one[cd..one.len() - 22].repeat(65535);
    let size = bytes.len() as u32;
    bytes.extend(b"PK\x05\x06\0\0\0\0");
    bytes.extend([0xff; 4]);
    bytes.extend(size.to_le_bytes());
    bytes.extend([0; 6]);
    let listing = list_zip(&mut Cursor::new(bytes), NEVER).unwrap();
    assert_eq!(listing.entries.len(), 65535);
    assert!(!listing.truncated);
}
