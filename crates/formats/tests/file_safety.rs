//! Opening and viewing never change a file, and saving a text without editing it gives back
//! exactly the bytes it came from. Every fixture goes through every reader that applies to it
//! (detection, trees, tables, Overview, previews, strings, archive entries), small files and
//! files larger than the editor holds, and must be byte for byte and time for time the same.
use isolinear_core::encoding::{self, Encoding, LineEnding};
use isolinear_core::paged::{From, LineIndex, Params};
use isolinear_core::save;
use isolinear_formats::registry::Kind;
use isolinear_formats::{
    binary, decoded, detect, document, inspect, notebook, pdf, table, tiff, tree,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

fn fixtures() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures"),
        &mut out,
    );
    out.sort();
    assert!(out.len() > 300, "only {} fixtures found", out.len());
    out
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("isolinear-safety-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The bytes and modification time of a file, to compare after reading it.
fn snapshot(path: &Path) -> (Vec<u8>, SystemTime) {
    let bytes = std::fs::read(path).unwrap();
    let modified = std::fs::metadata(path).unwrap().modified().unwrap();
    (bytes, modified)
}

/// Every read-only path of the app that applies to the file. Errors are fine (damaged
/// fixtures are meant to fail); a change to the file is not.
fn read_every_way(path: &Path, scratch: &Path) {
    let bytes = std::fs::read(path).unwrap();
    let name = path.file_name().unwrap().to_string_lossy();
    let probe = &bytes[..bytes.len().min(64 * 1024)];
    let d = detect(&name, probe);
    let format = d.format.id;
    let never = AtomicBool::new(false);

    if d.format.kind == Kind::Text {
        let text = encoding::decode(&bytes).text;
        if let Some(parser) = tree::for_format(format, d.language) {
            let _ = parser.parse(&text, &tree::Limits::default(), &never);
        }
        if format == "notebook" {
            let _ = notebook::parse(&text);
        }
    }
    let _ = binary::facts(probe);
    let _ = binary::strings(path, 1 << 20);
    if let Some(Ok(mut t)) = table::open(format, path, None) {
        let rows = t.schema().rows.unwrap_or(0);
        let _ = t.rows(0, 50);
        let _ = t.rows(rows.saturating_sub(5), 10);
        if table::ARCHIVES.contains(&format) {
            let _ = table::archive::extract(format, path, 0, scratch);
        }
    }
    let _ = inspect::inspect_file(path, format, &|| false);
    match format {
        "docx" | "odt" => {
            let _ = document::to_html(format, path);
        }
        "pdf" => {
            if pdf::page_count(path).is_ok_and(|n| n > 0) {
                let _ = pdf::render_page(path, 0, 0.5);
            }
        }
        "tiff" => {
            let _ = tiff::decode(&bytes);
        }
        f if decoded::supports(f) => {
            let _ = decoded::to_json(f, &bytes, &|| false);
        }
        _ => {}
    }
}

#[test]
fn reading_and_viewing_never_change_a_fixture() {
    let scratch = TempDir::new("read");
    for path in fixtures() {
        let before = snapshot(&path);
        read_every_way(&path, &scratch.0);
        assert!(
            snapshot(&path) == before,
            "{} changed while it was read",
            path.display()
        );
    }
}

#[test]
fn large_files_are_paged_and_read_without_a_change() {
    let dir = TempDir::new("large");
    // Beyond the 16 MiB the editor holds: read a page at a time, as the window does.
    let line = "Ligne 0000000 — données 日本 \u{a0d} 1234.50;EUR;OK\r\n";
    let text = line.repeat(20 * 1024 * 1024 / line.len() + 1);
    let utf8 = dir.0.join("big.csv");
    std::fs::write(&utf8, text.as_bytes()).unwrap();
    let utf16 = dir.0.join("big-utf16.txt");
    let mut wide = vec![0xFF, 0xFE];
    wide.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&utf16, &wide).unwrap();
    // A large binary file: facts, strings and a detection pass.
    let blob = dir.0.join("big.bin");
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let random: Vec<u8> = (0..24 << 20)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();
    std::fs::write(&blob, &random).unwrap();

    for (path, encoding, bom) in [
        (&utf8, Encoding::Utf8, false),
        (&utf16, Encoding::Utf16Le, true),
    ] {
        let before = snapshot(path);
        let size = before.0.len() as u64;
        let index = LineIndex::new(path, size, encoding, bom, Params::default()).unwrap();
        index.build(|_| {}).unwrap();
        let lines = index.progress().lines;
        assert!(lines > 300_000, "{lines} lines");
        assert_eq!(index.lines(lines / 2, 3).unwrap()[0].text, line.trim_end());
        assert!(
            index
                .find("1234.50", From::Line(lines - 10), true, &|| false)
                .unwrap()
                .is_some()
        );
        read_every_way(path, &dir.0);
        assert!(snapshot(path) == before, "{} changed", path.display());
    }
    let before = snapshot(&blob);
    read_every_way(&blob, &dir.0);
    let _ = binary::strings(&blob, 64 << 20);
    assert!(snapshot(&blob) == before, "big.bin changed");
}

/// Decodes, prepares for the editor and writes back as the window does when the text was not
/// edited.
fn round_trip(bytes: &[u8]) -> (Vec<u8>, encoding::Decoded) {
    let d = encoding::decode(bytes);
    let text = save::normalize(&d.text, d.line_ending);
    let out = save::encode(&text, d.encoding, d.bom, d.line_ending).unwrap();
    (out, d)
}

#[test]
fn saving_an_unedited_text_fixture_gives_back_its_bytes() {
    let mut checked = 0;
    for path in fixtures() {
        let bytes = std::fs::read(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy();
        let d = detect(&name, &bytes[..bytes.len().min(64 * 1024)]);
        if d.format.kind != Kind::Text || !d.editable {
            continue;
        }
        let (out, decoded) = round_trip(&bytes);
        if decoded.lossy {
            continue; // Not editable: see the edge cases below.
        }
        assert!(out == bytes, "{} would change on save", path.display());
        checked += 1;
    }
    assert!(checked > 100, "only {checked} text fixtures");
}

#[test]
fn saving_keeps_encodings_byte_order_marks_and_line_endings() {
    let long = "x".repeat(70_000);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", b"".to_vec()),
        ("no final newline", b"a\nb".to_vec()),
        ("CRLF", b"a\r\nb\r\n".to_vec()),
        ("CR", b"a\rb\r".to_vec()),
        ("mixed", b"a\r\nb\nc\rd".to_vec()),
        ("blank lines and spaces", b"\n\n  \t\n \r\n".to_vec()),
        ("UTF-8 BOM", b"\xEF\xBB\xBFcaf\xC3\xA9\r\n".to_vec()),
        ("only a BOM", b"\xEF\xBB\xBF".to_vec()),
        ("Windows-1252", b"caf\xE9 \x80 \x9F\r\n".to_vec()),
        (
            "Windows-1252, every byte",
            (1..=255u8).filter(|b| *b != b'\r').collect(),
        ),
        (
            "UTF-8 BOM then not UTF-8",
            b"\xEF\xBB\xBFcaf\xE9\n".to_vec(),
        ),
        (
            "UTF-16 LE",
            [
                &b"\xFF\xFE"[..],
                &"h\u{e9}\r\n日\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            ]
            .concat(),
        ),
        (
            "UTF-16 BE",
            [
                &b"\xFE\xFF"[..],
                &"h\u{e9}\n𝄞"
                    .encode_utf16()
                    .flat_map(u16::to_be_bytes)
                    .collect::<Vec<_>>(),
            ]
            .concat(),
        ),
        // One style for the first 64 KiB, another line further on.
        (
            "CRLF then a lone LF late",
            format!("{long}\r\n{long}\r\n{long}\nend\r\n").into_bytes(),
        ),
        (
            "LF then a CRLF late",
            format!("{long}\n{long}\n{long}\r\nend\n").into_bytes(),
        ),
    ];
    for (what, bytes) in cases {
        let (out, d) = round_trip(&bytes);
        assert!(!d.lossy, "{what}: decoded as lossy");
        assert!(
            out == bytes,
            "{what}: {:?} → {:?}",
            d.line_ending,
            String::from_utf8_lossy(&out[..out.len().min(80)])
        );
    }
    // Bytes that cannot be decoded are reported, so the window does not offer to edit them.
    let broken = [&b"\xFF\xFE"[..], &[0x00, 0xD8, b'a', 0]].concat(); // an unpaired surrogate
    assert!(encoding::decode(&broken).lossy);
    let _ = LineEnding::Lf;
}
