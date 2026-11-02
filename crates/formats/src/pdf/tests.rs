use super::*;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pdf")
        .join(name)
}

struct Temp(PathBuf);
impl Temp {
    fn new(data: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "isolinear-pdf-{}-{}.pdf",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, data).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn pdf(objects: &[String], trailer: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![];
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R {trailer} >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn document(count: usize, size: (u32, u32), extra: &str, trailer: &str, content: &str) -> Vec<u8> {
    let kids = (0..count)
        .map(|i| format!("{} 0 R ", i + 5))
        .collect::<String>();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {count} >>"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    for _ in 0..count {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 3 0 R >> >> /Contents 4 0 R {extra} >>", size.0, size.1
        ));
    }
    pdf(&objects, trailer)
}

fn hello(extra: &str) -> Vec<u8> {
    document(
        1,
        (612, 792),
        extra,
        "",
        "BT /F1 48 Tf 72 700 Td (Hello) Tj ET",
    )
}

struct Picture {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

// Independently check the PNG wire format, CRCs, zlib stream and row lengths.
fn decode(bytes: &[u8]) -> Picture {
    fn word(bytes: &[u8]) -> u32 {
        u32::from_be_bytes(bytes[..4].try_into().unwrap())
    }
    fn crc(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in bytes {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb8_8320 } else { 0 };
            }
        }
        !crc
    }
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let mut at = 8;
    let mut size = None;
    let mut compressed = vec![];
    let mut ended = false;
    while at < bytes.len() {
        let len = word(&bytes[at..]) as usize;
        let kind = &bytes[at + 4..at + 8];
        let data = &bytes[at + 8..at + 8 + len];
        assert_eq!(
            word(&bytes[at + 8 + len..]),
            crc(&bytes[at + 4..at + 8 + len])
        );
        match kind {
            b"IHDR" => {
                assert_eq!(&data[8..], &[8, 6, 0, 0, 0]);
                assert!(size.is_none());
                size = Some((word(data), word(&data[4..])));
            }
            b"IDAT" => {
                assert!(size.is_some());
                compressed.extend_from_slice(data);
            }
            b"IEND" => {
                assert_eq!(len, 0);
                assert_eq!(at + 12, bytes.len());
                ended = true;
            }
            _ => panic!("Unexpected chunk"),
        }
        at += 12 + len;
    }
    assert!(ended);
    let (width, height) = size.unwrap();
    let mut filtered = vec![];
    flate2::read::ZlibDecoder::new(&compressed[..])
        .read_to_end(&mut filtered)
        .unwrap();
    let stride = width as usize * 4;
    assert_eq!(filtered.len(), (stride + 1) * height as usize);
    let mut rgba = vec![];
    for row in filtered.chunks_exact(stride + 1) {
        assert_eq!(row[0], 0);
        rgba.extend_from_slice(&row[1..]);
    }
    Picture {
        width,
        height,
        rgba,
    }
}

fn ink(p: &Picture, x: (f32, f32), y: (f32, f32)) -> f32 {
    let (w, h) = (p.width as usize, p.height as usize);
    let (x0, x1) = ((x.0 * w as f32) as usize, (x.1 * w as f32) as usize);
    let (y0, y1) = ((y.0 * h as f32) as usize, (y.1 * h as f32) as usize);
    let mut dark = 0;
    for row in y0..y1 {
        for col in x0..x1 {
            let i = (row * w + col) * 4;
            if p.rgba[i..i + 3].iter().map(|v| u32::from(*v)).sum::<u32>() < 3 * 128 {
                dark += 1;
            }
        }
    }
    dark as f32 / ((x1 - x0) * (y1 - y0)).max(1) as f32
}

#[test]
fn real_document_and_public_file_api() {
    let path = fixture("libreoffice.pdf");
    let before = std::fs::read(&path).unwrap();
    assert!(page_count(&path).unwrap() > 0);
    let page = decode(&render_page(&path, 0, 1.0).unwrap());
    assert_eq!((page.width, page.height), (612, 792));
    assert!(ink(&page, (0.1, 0.9), (0.05, 0.5)) > 0.005);
    assert_eq!(ink(&page, (0.0, 0.02), (0.0, 1.0)), 0.0);
    assert!(page.rgba.chunks_exact(4).all(|p| p[3] == 255));
    let bigger = decode(&render_page(&path, 0, 2.0).unwrap());
    assert_eq!((bigger.width, bigger.height), (1224, 1584));
    assert!(
        render_page(&path, usize::MAX, 1.0)
            .unwrap_err()
            .contains("index")
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn standard_fonts_rotation_crop_and_multiple_pages() {
    let doc = open(hello("")).unwrap();
    let page = decode(&render(&doc, 0, 1.0).unwrap());
    assert!(ink(&page, (0.1, 0.4), (0.08, 0.14)) > 0.05);
    assert_eq!(ink(&page, (0.1, 0.9), (0.5, 0.95)), 0.0);
    let rotated = open(hello("/Rotate 90")).unwrap();
    let page = decode(&render(&rotated, 0, 1.0).unwrap());
    assert_eq!((page.width, page.height), (792, 612));
    let cropped = open(hello("/CropBox [72 400 372 700]")).unwrap();
    let page = decode(&render(&cropped, 0, 1.0).unwrap());
    assert_eq!((page.width, page.height), (300, 300));
    let file = Temp::new(&document(3, (24, 32), "", "", "0 0 0 rg 0 0 12 16 re f"));
    assert_eq!(page_count(&file.0).unwrap(), 3);
    let last = decode(&render_page(&file.0, 2, 1.0).unwrap());
    assert!(ink(&last, (0.0, 1.0), (0.0, 1.0)) > 0.2);
    assert!(render_page(&file.0, 3, 1.0).is_err());
}

#[test]
fn all_encryption_is_reported_as_protected() {
    for name in ["password.pdf", "owner-only.pdf"] {
        let path = fixture(name);
        assert!(page_count(&path).unwrap_err().contains("protected"));
        assert!(
            render_page(&path, 0, 1.0)
                .unwrap_err()
                .contains("protected")
        );
    }
    for marker in ["/Encrypt 9 0 R", "/Encr#79pt << /Filter /Standard >>"] {
        let file = Temp::new(&document(1, (16, 16), "", marker, ""));
        assert!(page_count(&file.0).unwrap_err().contains("protected"));
    }
    // Text, comments, stream bytes and longer names must not look like an encryption key.
    let data = document(
        1,
        (64, 64),
        "/EncryptionHint (not encrypted)",
        "/Info << /Title (/Encrypt) >> % /Encrypt\n",
        "BT /F1 8 Tf (/Encrypt) Tj ET",
    );
    assert!(open(data).is_ok());
}

#[test]
fn pictures_are_bounded_before_allocation() {
    let doc = open(document(1, (10_000, 10_000), "", "", "")).unwrap();
    let png = render(&doc, 0, 100.0).unwrap();
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert!((39_000_000..=MAX_PIXELS).contains(&(u64::from(width) * u64::from(height))));
    assert!(width <= u32::from(MAX_SIDE) && height <= u32::from(MAX_SIDE));
    for size in [
        (1.0, 1.0),
        (1e30, 1e30),
        (1e30, 1.0),
        (1.0, 1e30),
        (612.0, 792.0),
    ] {
        for scale in [f32::MIN_POSITIVE, 0.01, 1.0, 100.0, f32::MAX] {
            let (w, h, _) = canvas(size, scale).unwrap();
            assert!(w > 0 && h > 0 && w <= MAX_SIDE && h <= MAX_SIDE);
            assert!(u64::from(w) * u64::from(h) <= MAX_PIXELS);
        }
    }
    for scale in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(render(&doc, 0, scale).is_err());
        assert!(render_page(&fixture("libreoffice.pdf"), 0, scale).is_err());
    }
    for size in [
        (f32::INFINITY, 1.0),
        (1.0, f32::NAN),
        (-1.0, 1.0),
        (0.0, 1.0),
    ] {
        assert!(canvas(size, 1.0).is_err());
    }
}

#[test]
fn file_and_page_count_limits() {
    let file = Temp::new(b"");
    File::options()
        .write(true)
        .open(&file.0)
        .unwrap()
        .set_len(MAX_FILE + 1)
        .unwrap();
    assert!(page_count(&file.0).unwrap_err().contains("512 MiB"));
    assert!(page_count(Path::new(env!("CARGO_MANIFEST_DIR"))).is_err());
    assert!(open(document(0, (16, 16), "", "", "")).is_err());
    let at_limit = document(MAX_PAGES, (16, 16), "", "", "");
    assert_eq!(open(at_limit).unwrap().pages().len(), MAX_PAGES);
    assert!(
        open(document(MAX_PAGES + 1, (16, 16), "", "", ""))
            .err()
            .unwrap()
            .contains("20,000")
    );
    // A dishonest Count cannot bypass the actual-page check.
    let understated = String::from_utf8(document(MAX_PAGES + 1, (16, 16), "", "", ""))
        .unwrap()
        .replace("/Count 20001", "/Count 1");
    assert!(
        open(understated.into_bytes())
            .err()
            .unwrap()
            .contains("20,000")
    );
}

#[test]
fn damaged_cross_references_and_cycles() {
    assert!(open(vec![]).is_err());
    assert!(open(b"%PDF-1.7\n".to_vec()).is_err());
    let data = hello("");
    for length in [1, 8, 40, data.len() / 2] {
        let _ = guarded(|| open(data[..length].to_vec()));
    }
    // hayro recovers readable objects when the cross-reference offset is wrong.
    let mut damaged = String::from_utf8(data).unwrap();
    let at = damaged.rfind("startxref\n").unwrap() + 10;
    let end = at + damaged[at..].find('\n').unwrap();
    damaged.replace_range(at..end, "999999999999999999");
    let doc = open(damaged.into_bytes()).unwrap();
    assert_eq!(doc.pages().len(), 1);
    assert!(render(&doc, 0, 0.1).is_ok());
    let cycle = pdf(
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 16 16] >>".into(),
        ],
        "",
    );
    assert_eq!(open(cycle).unwrap().pages().len(), 1);
    let deep = format!("%PDF-1.7\n{}{}", "<< /X ".repeat(1000), ">>".repeat(1000));
    assert!(guarded(|| open(deep.into_bytes())).is_err());
    let huge = format!("%PDF-1.7\n<< /X ({}) >>", "x".repeat(1024 * 1024));
    assert!(open(huge.into_bytes()).err().unwrap().contains("1 MiB"));
}

#[test]
fn actions_external_references_and_javascript_do_not_run() {
    let extra = "/AA << /O << /S /JavaScript /JS (throw 'must not run') >> >> /Annots [<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /URI /URI (https://127.0.0.1:9/not-read) >> >>]";
    let plain = open(document(1, (32, 32), "", "", "0 0 0 rg 0 0 10 10 re f")).unwrap();
    let active = open(document(1, (32, 32), extra, "", "0 0 0 rg 0 0 10 10 re f")).unwrap();
    assert_eq!(
        render(&plain, 0, 1.0).unwrap(),
        render(&active, 0, 1.0).unwrap()
    );
    let external = pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /OpenAction << /S /Launch /F (/nonexistent/program) >> >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 16 16] /Contents 4 0 R >>".into(),
        "<< /Length 0 /F (/nonexistent/external.pdf) >>\nstream\n\nendstream".into(),
    ], "");
    assert!(render(&open(external).unwrap(), 0, 1.0).is_ok());
}

#[test]
fn png_round_trip_and_invalid_rasters() {
    let rgba = [
        255, 0, 0, 255, 0, 128, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255,
    ];
    let picture = decode(&png::encode(2, 2, &rgba).unwrap());
    assert_eq!((picture.width, picture.height), (2, 2));
    assert_eq!(picture.rgba, rgba);
    assert!(png::encode(2, 2, &rgba[..12]).is_err());
    assert!(png::encode(0, 1, &[]).is_err());
    assert!(png::encode(u16::MAX, u16::MAX, &[]).is_err());
    let mut random = vec![0; 256 * 256 * 4];
    let mut seed = 1u64;
    for byte in &mut random {
        *byte = next(&mut seed) as u8;
    }
    let bytes = png::encode(256, 256, &random).unwrap();
    assert!(bytes.windows(4).filter(|w| *w == b"IDAT").count() > 1);
    assert_eq!(decode(&bytes).rgba, random);
}

fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn twenty_thousand_mutated_documents() {
    let bases = [
        document(1, (32, 32), "", "", "0 0 0 rg 0 0 10 10 re f"),
        document(3, (16, 24), "/Rotate 90", "", "BT /F1 8 Tf (Fuzz) Tj ET"),
        document(1, (16, 16), "", "/Encrypt 9 0 R", ""),
    ];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rendered = 0;
    for i in 0..20_000 {
        let mut data = bases[i % bases.len()].clone();
        for _ in 0..next(&mut seed) % 10 + 1 {
            let at = next(&mut seed) as usize % data.len();
            data[at] = match next(&mut seed) % 4 {
                0 => b'(',
                1 => b'<',
                2 => b'0' + (next(&mut seed) % 10) as u8,
                _ => next(&mut seed) as u8,
            };
        }
        if next(&mut seed).is_multiple_of(5) {
            data.truncate(next(&mut seed) as usize % data.len());
        }
        let _ = guarded(|| {
            if let Ok(doc) = open(data) {
                let (w, h) = doc.pages()[0].render_dimensions();
                // Keep this parser/render stress test cheap even when a mutation changes the box.
                if w <= 256.0 && h <= 256.0 {
                    let _ = render(&doc, 0, 0.125);
                    rendered += 1;
                }
            }
            Ok(())
        });
    }
    assert!(
        rendered > 1000,
        "only {rendered} mutations reached rendering"
    );
}
