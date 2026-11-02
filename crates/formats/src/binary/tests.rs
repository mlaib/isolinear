use super::identify::{Category, Identity, identify, utc};
use super::strings::from_bytes;
use super::{entropy, facts, strings};

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend(w.to_be_bytes());
    b.extend(h.to_be_bytes());
    b.extend([8, 6, 0, 0, 0]);
    b
}
fn detail(id: &Identity, label: &str) -> String {
    id.details
        .iter()
        .find(|(l, _)| l == label)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("no {label} in {id:?}"))
}

#[test]
fn images_report_their_dimensions() {
    let id = identify(&png(640, 480), true).unwrap();
    assert_eq!(id.name, "PNG image");
    assert_eq!(id.category, Category::Image);
    assert_eq!(detail(&id, "Dimensions"), "640 × 480 pixels");
    assert_eq!(detail(&id, "Colour"), "RGBA, 8-bit");
    let mut gif = b"GIF89a".to_vec();
    gif.extend(320u16.to_le_bytes());
    gif.extend(200u16.to_le_bytes());
    assert_eq!(
        detail(&identify(&gif, true).unwrap(), "Dimensions"),
        "320 × 200 pixels"
    );
    // JPEG: SOI, an APP0 segment, then SOF0 with height 300 and width 400.
    let jpeg = [
        &b"\xff\xd8\xff\xe0\x00\x10JFIF\0\x01\x01\0\0\x01\0\x01\0\0"[..],
        &b"\xff\xc0\x00\x11\x08\x01\x2c\x01\x90\x03"[..],
        &[0u8; 12][..],
    ]
    .concat();
    assert_eq!(
        detail(&identify(&jpeg, true).unwrap(), "Dimensions"),
        "400 × 300 pixels"
    );
}

#[test]
fn photoshop_and_heif_report_their_size() {
    let mut psd = b"8BPS\x00\x01\0\0\0\0\0\0\x00\x03".to_vec();
    psd.extend(600u32.to_be_bytes()); // height
    psd.extend(800u32.to_be_bytes()); // width
    psd.extend([0, 8, 0, 3]); // 8-bit RGB
    let id = identify(&psd, true).unwrap();
    assert_eq!(id.name, "Photoshop document (PSD)");
    assert_eq!(detail(&id, "Dimensions"), "800 × 600 pixels");
    assert_eq!(detail(&id, "Colour"), "RGB, 8-bit, 3 channels");
    assert_eq!(
        identify(b"8BPS", true).unwrap().name,
        "Photoshop document (unknown version)"
    );

    let ispe = |w: u32, h: u32| {
        let mut b = 20u32.to_be_bytes().to_vec();
        b.extend(b"ispe\0\0\0\0");
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b
    };
    let mut heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic".to_vec();
    heic.extend(ispe(512, 512));
    heic.extend(ispe(4032, 3024));
    heic.extend(ispe(0, 0));
    let id = identify(&heic, true).unwrap();
    assert_eq!(id.name, "HEIF/HEIC image");
    assert_eq!(detail(&id, "Dimensions"), "4032 × 3024 pixels");
    let mut truncated = b"\0\0\0\x18ftypavif\0\0\0\0".to_vec();
    truncated.extend(b"ispe\0\0");
    let id = identify(&truncated, true).unwrap();
    assert!(id.details.iter().all(|(l, _)| l != "Dimensions"));
}

#[test]
fn programs_are_described() {
    let mut elf = b"\x7fELF\x02\x01\x01".to_vec();
    elf.resize(16, 0);
    elf.extend(2u16.to_le_bytes());
    elf.extend(0x3eu16.to_le_bytes());
    let id = identify(&elf, true).unwrap();
    assert_eq!(id.category, Category::Executable);
    assert_eq!(detail(&id, "Kind"), "executable");
    assert_eq!(detail(&id, "Architecture"), "x86-64 64-bit");

    let mut pe = vec![0u8; 0x100];
    pe[..2].copy_from_slice(b"MZ");
    pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    pe[0x80..0x84].copy_from_slice(b"PE\0\0");
    pe[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
    pe[0x96..0x98].copy_from_slice(&0x2022u16.to_le_bytes());
    let id = identify(&pe, true).unwrap();
    assert_eq!(id.name, "Windows library (DLL)");
    assert_eq!(detail(&id, "Architecture"), "x86-64");
}

#[test]
fn short_signatures_are_ignored_for_text() {
    assert!(identify(b"MZ is a plain text line\n", false).is_none());
    assert!(identify(b"BM: notes\n", false).is_none());
    assert!(identify(b"ID3 tags are described here\n", false).is_none());
    assert!(identify(b"MZ\x90\0\x03\0\0\0", true).is_some());
}

#[test]
fn containers_and_compressed_data_name_their_content() {
    // A DOCX starts with [Content_Types].xml and mentions word/ early.
    let mut docx = b"PK\x03\x04\x14\0\0\0\x08\0".to_vec();
    docx.resize(26, 0);
    let name = b"[Content_Types].xml";
    docx.extend((name.len() as u16).to_le_bytes());
    docx.extend(0u16.to_le_bytes());
    docx.extend(name);
    docx.extend(b"....word/document.xml");
    let id = identify(&docx, true).unwrap();
    assert_eq!(id.name, "ZIP archive (Word document, DOCX)");
    assert_eq!(detail(&id, "First entry"), "[Content_Types].xml");

    let mut gz = b"\x1f\x8b\x08\x08".to_vec();
    gz.extend(1_700_000_000u32.to_le_bytes());
    gz.extend([0, 3]);
    gz.extend(b"report.csv\0");
    let id = identify(&gz, true).unwrap();
    assert_eq!(detail(&id, "Original name"), "report.csv");

    let mut sqlite = b"SQLite format 3\0".to_vec();
    sqlite.extend(4096u16.to_be_bytes());
    sqlite.resize(28, 0);
    sqlite.extend(12u32.to_be_bytes());
    sqlite.resize(56, 0);
    sqlite.extend(1u32.to_be_bytes());
    let id = identify(&sqlite, true).unwrap();
    assert_eq!(detail(&id, "Page size"), "4096 bytes");
    assert_eq!(detail(&id, "Text encoding"), "UTF-8");
}

#[test]
fn entropy_separates_text_from_random_data() {
    assert!(entropy(b"aaaaaaaa") < 0.01);
    let mut seed = 7u64;
    let random: Vec<u8> = (0..65536)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();
    assert!(entropy(&random) > 7.9);
}

#[test]
fn random_headers_never_panic() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let prefixes: [&[u8]; 16] = [
        b"PK\x03\x04",
        b"\x7fELF",
        b"MZ",
        b"\x89PNG\r\n\x1a\n",
        b"\xff\xd8\xff",
        b"GIF89a",
        b"RIFF",
        b"\x1f\x8b",
        b"SQLite format 3\0",
        b"\xca\xfe\xba\xbe",
        b"\xcf\xfa\xed\xfe",
        b"BM",
        b"\x93NUMPY",
        b"Obj\x01",
        b"\0\0\x01\0",
        b"",
    ];
    for _ in 0..20_000 {
        let prefix = prefixes[(next() % prefixes.len() as u64) as usize];
        let len = (next() % 80) as usize;
        let mut bytes = prefix.to_vec();
        bytes.extend((0..len).map(|_| next() as u8));
        for binary in [false, true] {
            let _ = identify(&bytes, binary);
        }
        let found = from_bytes(&bytes);
        assert!(found.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(
            found
                .iter()
                .all(|(o, s)| *o < bytes.len() as u64 && s.chars().count() <= 200)
        );
        let _ = facts(&bytes);
        let _ = entropy(&bytes);
    }
}

#[test]
fn utc_dates_are_calendar_correct() {
    assert_eq!(utc(0), "1970-01-01 00:00 UTC");
    assert_eq!(utc(951_782_400), "2000-02-29 00:00 UTC");
    assert_eq!(utc(1_793_364_516), "2026-10-30 12:48 UTC"); // date -u -d @1793364516
    assert_eq!(utc(-86_400), "1969-12-31 00:00 UTC");
}

fn value(bytes: &[u8], label: &str) -> Option<String> {
    facts(bytes)
        .into_iter()
        .find(|(l, _)| l == label)
        .map(|(_, v)| v)
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[test]
fn password_protected_office_files_are_named() {
    let mut bytes = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1".to_vec();
    bytes.extend(utf16("EncryptedPackage"));
    bytes.extend(utf16("EncryptionInfo"));
    assert!(
        value(&bytes, "Document")
            .unwrap()
            .starts_with("Password-protected")
    );
    bytes.truncate(8);
    assert!(
        value(&bytes, "Document")
            .unwrap()
            .starts_with("Microsoft Compound File")
    );
}

#[test]
fn public_facts_gate_short_signatures_for_text() {
    for text in [
        "MZ is a plain text line\n",
        "BM: notes\n",
        "ID3 tags\n",
        "true is a value\n",
        "true café crème\n",
    ] {
        let f = facts(text.as_bytes());
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].0, "First bytes");
    }
    assert!(value(b"MZ\x90\0\x03\0\0\0", "Program").is_some());
    assert_eq!(
        value(b"\x80\x05\0", "Data").as_deref(),
        Some("Python pickle")
    );
}

#[test]
fn facts_keep_first_bytes_and_entropy_after_identification() {
    let mut bytes = png(1920, 1080);
    bytes.resize(1024, 0);
    let f = facts(&bytes);
    assert_eq!(f[0], ("Image".into(), "PNG image".into()));
    assert!(
        f.iter()
            .any(|(l, v)| l == "Dimensions" && v == "1920 × 1080 pixels")
    );
    assert!(f.iter().any(|(l, _)| l == "First bytes"));
    assert_eq!(f.last().unwrap().0, "Entropy");
}

#[test]
fn facts_never_use_bytes_past_the_probe() {
    let mut bytes = vec![0; 64 * 1024];
    bytes[..8].copy_from_slice(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1");
    let before = facts(&bytes);
    bytes.extend(utf16("EncryptedPackage"));
    bytes.extend(utf16("EncryptionInfo"));
    assert_eq!(facts(&bytes), before);
}

#[test]
fn additional_signatures_are_ported() {
    let cases: &[(&[u8], &str)] = &[
        (b"%PDF-1.7", "PDF"),
        (b"PK\x05\x06", "ZIP"),
        (b"RIFF\0\0\0\0WEBP", "WebP"),
        (b"RIFF\0\0\0\0WAVE", "WAV"),
        (b"RIFF\0\0\0\0AVI ", "AVI"),
        (b"II*\0", "TIFF"),
        (b"MM\0*", "TIFF"),
        (b"BZh9", "bzip2"),
        (b"\xfd7zXZ\0", "xz"),
        (b"\x28\xb5\x2f\xfd", "Zstandard"),
        (b"\x04\x22\x4d\x18", "LZ4"),
        (b"7z\xbc\xaf\x27\x1c\0\x04", "7-Zip"),
        (b"Rar!\x1a\x07\x01", "RAR"),
        (b"!<arch>\ndebian", "Debian"),
        (b"!<arch>\n", "ar archive"),
        (b"\xed\xab\xee\xdb", "RPM"),
        (b"Obj\x01", "Avro"),
        (b"ARROW1", "Arrow"),
        (b"PAR1", "Parquet"),
        (b"ORC", "ORC"),
        (b"\x89HDF\r\n\x1a\n", "HDF5"),
        (b"CDF\x01", "NetCDF"),
        (b"CDF\x02", "NetCDF"),
        (b"bplist00", "Binary property list"),
        (b"\0asm\x01\0\0\0", "WebAssembly"),
        (b"\xac\xed\0\x05", "Java serialized"),
        (b"LUKS\xba\xbe", "LUKS"),
        (b"PACK\0\0\0\x02", "Git pack"),
        (b"OTTO", "OpenType"),
        (b"wOFF", "Web font"),
        (b"wOF2", "Web font"),
        (b"fLaC", "FLAC"),
        (b"OggS", "Ogg"),
        (b"\x1a\x45\xdf\xa3webm", "WebM"),
        (b"\x1a\x45\xdf\xa3", "Matroska"),
        (b"MThd", "MIDI"),
        (b"\0\0\x01\0\x02\0", "Windows icon"),
        (b"ID3\0", "MP3"),
        (b"\0\x01\0\0", "TrueType"),
        (b"true\0", "TrueType"),
        (b"\x80\x04\0", "Python pickle"),
    ];
    for (bytes, name) in cases {
        assert!(
            facts(bytes)[0].1.starts_with(name),
            "{name}: {:?}",
            facts(bytes)
        );
        for end in 0..bytes.len() {
            let _ = facts(&bytes[..end]);
        }
    }
}

#[test]
fn offset_signatures_and_container_brands() {
    let mut tar = vec![0; 262];
    tar[..8].copy_from_slice(b"data.csv");
    tar[257..].copy_from_slice(b"ustar");
    assert_eq!(value(&tar, "First entry").as_deref(), Some("data.csv"));
    let mut iso = vec![0; 0x8048];
    iso[0x8001..0x8006].copy_from_slice(b"CD001");
    iso[0x8028..0x802c].copy_from_slice(b"DISC");
    assert_eq!(value(&iso, "Volume label").as_deref(), Some("DISC"));
    for (brand, name) in [
        ("heic", "HEIF/HEIC"),
        ("avif", "AVIF"),
        ("qt  ", "QuickTime"),
        ("M4A ", "MPEG-4 audio"),
        ("crx ", "Canon"),
        ("isom", "MPEG-4 media"),
    ] {
        let mut bytes = b"\0\0\0\x18ftyp".to_vec();
        bytes.extend(brand.as_bytes());
        assert!(facts(&bytes)[0].1.starts_with(name));
        assert_eq!(value(&bytes, "Brand").as_deref(), Some(brand));
    }
}

#[test]
fn mach_o_architecture_type_and_java_disambiguation() {
    for little in [true, false] {
        let mut bytes = Vec::new();
        for n in [0xfeedfacfu32, 0x0100_000c, 0, 6] {
            bytes.extend(if little {
                n.to_le_bytes()
            } else {
                n.to_be_bytes()
            });
        }
        assert_eq!(
            value(&bytes, "Architecture").as_deref(),
            Some("ARM64 64-bit")
        );
        assert_eq!(value(&bytes, "Kind").as_deref(), Some("shared library"));
    }
    assert_eq!(
        value(b"\xca\xfe\xba\xbe\0\0\0\x02", "Architectures").as_deref(),
        Some("2")
    );
    assert_eq!(
        value(b"\xca\xfe\xba\xbe\0\0\0\x3d", "Class version").as_deref(),
        Some("61")
    );
}

fn zip_entry(name: &str, content: &[u8]) -> Vec<u8> {
    let mut bytes = b"PK\x03\x04".to_vec();
    bytes.resize(18, 0);
    bytes.extend((content.len() as u32).to_le_bytes());
    bytes.extend((content.len() as u32).to_le_bytes());
    bytes.extend((name.len() as u16).to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(name.as_bytes());
    bytes.extend(content);
    bytes
}

#[test]
fn zip_subtypes_and_encryption() {
    for (name, content, kind) in [
        ("mimetype", "application/epub+zip", "EPUB"),
        ("mimetype", "application/vnd.oasis.opendocument.text", "ODT"),
        (
            "mimetype",
            "application/vnd.oasis.opendocument.spreadsheet",
            "ODS",
        ),
        (
            "mimetype",
            "application/vnd.oasis.opendocument.presentation",
            "ODP",
        ),
        ("META-INF/MANIFEST.MF", "", "JAR"),
        ("AndroidManifest.xml", "", "APK"),
        ("xl/workbook.xml", "", "XLSX"),
        ("ppt/presentation.xml", "", "PPTX"),
    ] {
        let bytes = zip_entry(name, content.as_bytes());
        assert!(
            facts(&bytes)[0].1.contains(kind),
            "{kind}: {:?}",
            facts(&bytes)
        );
    }
    let mut bytes = zip_entry("secret", b"data");
    bytes[6] = 1;
    assert!(value(&bytes, "Encryption").is_some());
    bytes[26..30].fill(0xff);
    assert_eq!(value(&bytes, "First entry"), None);
}

#[test]
fn gzip_extra_field_name_and_time() {
    let mut bytes = b"\x1f\x8b\x08\x0c".to_vec();
    bytes.extend(1_700_000_000u32.to_le_bytes());
    bytes.extend([0, 3, 3, 0, 4, 5, 6]);
    bytes.extend(b"report.csv\0");
    assert_eq!(
        value(&bytes, "Original name").as_deref(),
        Some("report.csv")
    );
    assert_eq!(
        value(&bytes, "Compressed at").as_deref(),
        Some("2023-11-14 22:13 UTC")
    );
    bytes[10..12].fill(0xff);
    assert_eq!(value(&bytes, "Original name"), None);
}

#[test]
fn numpy_header_versions_and_limits() {
    let header = "{'descr': '<f8', 'fortran_order': False, 'shape': (2, 3)}\n";
    for version in [1, 2, 3] {
        let mut bytes = b"\x93NUMPY".to_vec();
        bytes.extend([version, 0]);
        if version == 1 {
            bytes.extend((header.len() as u16).to_le_bytes());
        } else {
            bytes.extend((header.len() as u32).to_le_bytes());
        }
        bytes.extend(header.as_bytes());
        assert_eq!(value(&bytes, "Header").as_deref(), Some(header.trim()));
        bytes.pop();
        assert!(value(&bytes, "Header limit").is_some());
    }
}

#[test]
fn truncated_headers_and_corrupt_offsets_do_not_invent_details() {
    let mut pe = vec![0; 64];
    pe[..2].copy_from_slice(b"MZ");
    pe[60..].fill(0xff);
    assert_eq!(value(&pe, "Architecture"), None);
    assert_eq!(
        value(
            b"\xff\xd8\xff\xc0\xff\xff\x08\x01\x2c\x01\x90\x03",
            "Dimensions"
        ),
        None
    );
    for n in 0..png(640, 480).len() {
        let _ = facts(&png(640, 480)[..n]);
    }
}

#[test]
fn strings_finds_ascii_utf8_and_utf16_at_byte_offsets() {
    let mut bytes = vec![0, 1, 2];
    bytes.extend(b"Hello world");
    bytes.extend([0, 0xff, 0xfe]);
    let cafe = bytes.len() as u64;
    bytes.extend("café crème".as_bytes());
    bytes.extend([0, 0]);
    let windows = bytes.len() as u64;
    bytes.extend(utf16("Windows"));
    bytes.extend([0, 0, 0, 0]);
    let found = from_bytes(&bytes);
    for (offset, text) in [
        (3, "Hello world"),
        (cafe, "café crème"),
        (windows, "Windows"),
    ] {
        assert!(found.contains(&(offset, text.into())), "{found:?}");
    }
}

#[test]
fn utf16_strings_keep_latin_greek_and_cyrillic_at_both_alignments() {
    for padding in [0, 1] {
        let mut bytes = vec![0; padding];
        bytes.extend(utf16("Grüße Ωμέγα Привет"));
        bytes.extend([0, 0]);
        assert!(from_bytes(&bytes).contains(&(padding as u64, "Grüße Ωμέγα Привет".into())));
    }
}

#[test]
fn utf16_runs_stop_at_characters_random_bytes_decode_to() {
    // Any byte pair is a UTF-16 unit, mostly CJK: such runs would turn data into "text".
    let mut bytes = utf16("Hello 世界🌍");
    bytes.extend([0, 0]);
    let found = from_bytes(&bytes);
    assert!(found.contains(&(0, "Hello ".into())), "{found:?}");
    assert!(!found.iter().any(|(_, s)| s.contains('世')), "{found:?}");
}

#[test]
fn long_strings_are_cut_within_two_hundred_characters() {
    for text in ["x".repeat(1000), "é".repeat(1000)] {
        let found = from_bytes(text.as_bytes());
        let shown = &found.iter().find(|(o, _)| *o == 0).unwrap().1;
        assert_eq!(shown.chars().count(), 200);
        assert!(shown.ends_with('…'));
    }
    assert!(
        from_bytes(b"abc\0\0\0   \n\t\0")
            .iter()
            .all(|(_, s)| s != "abc" && !s.trim().is_empty())
    );
}

#[test]
fn invalid_utf8_and_control_characters_split_runs() {
    let found = from_bytes(b"first\xffsecond\xc0\xafthird\xc3fourth\x7ffifth");
    for (offset, text) in [
        (0, "first"),
        (6, "second"),
        (14, "third"),
        (20, "fourth"),
        (27, "fifth"),
    ] {
        assert!(found.contains(&(offset, text.into())), "{found:?}");
    }
}

struct TempFile(std::path::PathBuf);
impl TempFile {
    fn new(bytes: &[u8]) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "isolinear-strings-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&p, bytes).unwrap();
        Self(p)
    }
}
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn strings_obeys_file_prefix_and_handles_missing_files() {
    let f = TempFile::new(b"first\0second");
    assert_eq!(strings(&f.0, 0).unwrap(), vec![]);
    assert_eq!(strings(&f.0, 5).unwrap(), vec![(0, "first".into())]);
    assert!(strings(&f.0, 6).unwrap().iter().all(|(o, _)| *o < 6));
    assert!(
        strings(&f.0, u64::MAX)
            .unwrap()
            .contains(&(6, "second".into()))
    );
    let path = f.0.clone();
    drop(f);
    assert!(strings(&path, 16).unwrap_err().contains("Could not open"));
}

#[test]
fn strings_span_read_blocks_without_losing_utf8_or_utf16() {
    for wide in [false, true] {
        let mut bytes = vec![0; 65533];
        let text = if wide {
            "abéΩЖ—hello"
        } else {
            "abé世界🌍hello"
        };
        bytes.extend(if wide {
            utf16(text)
        } else {
            text.as_bytes().to_vec()
        });
        bytes.extend([0; 8]);
        let f = TempFile::new(&bytes);
        let found = strings(&f.0, u64::MAX).unwrap();
        assert!(found.contains(&(65533, text.into())), "{found:?}");
    }
}

#[test]
fn string_count_is_bounded_and_results_are_in_byte_order() {
    let bytes = b"hello\0\0\0".repeat(200_010);
    let found = from_bytes(&bytes);
    assert_eq!(found.len(), 200_000);
    assert!(found.windows(2).all(|w| w[0].0 < w[1].0));
    assert_eq!(found[0], (0, "hello".into()));
}

#[test]
fn utf8_wins_over_a_spurious_utf16_run_at_the_same_offset() {
    let text = "🌍🌍🌍🌍";
    assert!(from_bytes(text.as_bytes()).contains(&(0, text.into())));
}

#[test]
fn strings_count_characters_and_discard_incomplete_sequences() {
    assert!(!from_bytes("ééé".as_bytes()).iter().any(|(_, s)| s == "ééé"));
    assert!(from_bytes("éééé".as_bytes()).contains(&(0, "éééé".into())));
    let mut bytes = b"hello".to_vec();
    bytes.extend([0xf0, 0x9f]);
    assert!(from_bytes(&bytes).contains(&(0, "hello".into())));
    let exact = "é".repeat(200);
    assert!(from_bytes(exact.as_bytes()).contains(&(0, exact)));
}

#[test]
fn invalid_utf16_surrogates_split_runs_without_losing_following_text() {
    let mut bytes = utf16("first");
    bytes.extend(0xd800u16.to_le_bytes());
    bytes.extend(utf16("second"));
    bytes.extend(0xdc00u16.to_le_bytes());
    bytes.extend(utf16("third"));
    bytes.extend(0xd800u16.to_le_bytes());
    let found = from_bytes(&bytes);
    for (offset, text) in [(0, "first"), (12, "second"), (26, "third")] {
        assert!(found.contains(&(offset, text.into())), "{found:?}");
    }
}

#[test]
fn truncated_fixed_headers_omit_unavailable_facts() {
    for (bytes, label) in [
        (&b"SQLite format 3\0"[..], "Page size"),
        (&b"\x7fELF"[..], "Byte order"),
        (&b"7z\xbc\xaf\x27\x1c"[..], "Format version"),
        (&b"\x93NUMPY"[..], "Format version"),
        (&b"Rar!\x1a\x07"[..], "Format"),
        (&b"\x1f\x8b\x08\x08\0\0\0\0\0\0"[..], "Original name"),
    ] {
        assert_eq!(value(bytes, label), None);
    }
}

#[test]
fn duplicate_decodings_do_not_consume_the_string_limit() {
    let found = from_bytes(&b"abcdefghij\0\0".repeat(200_010));
    assert_eq!(found.len(), 200_000);
    assert!(found.windows(2).all(|w| w[0].0 < w[1].0));
    assert_eq!(found[0], (0, "abcdefghij".into()));
}
