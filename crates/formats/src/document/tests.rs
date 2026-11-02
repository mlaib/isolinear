//! DOCX and ODT content previews. Fixtures: `pandoc.docx` (pandoc 3.6.4 from Markdown),
//! `libreoffice.odt` and `libreoffice.docx` (LibreOffice 24.2 conversions of it); see TEST_REPORT.
use super::{
    html,
    model::{Block, Cell, Span},
    reader::{self as office, OfficeDocument},
    zip::list_zip,
};
use std::io::Cursor;
use std::time::Instant;

const NEVER: &dyn Fn() -> bool = &|| false;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/office")
            .join(name),
    )
    .unwrap()
}

fn read(bytes: &[u8]) -> Option<Result<OfficeDocument, String>> {
    let mut cursor = Cursor::new(bytes);
    let listing = list_zip(&mut cursor, NEVER).ok()?;
    office::read(&mut cursor, &listing, NEVER)
}

fn text(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// A compact outline of the blocks, one line each, indented by nesting.
fn outline(blocks: &[Block], depth: usize, out: &mut Vec<String>) {
    let pad = "  ".repeat(depth);
    for b in blocks {
        match b {
            Block::Heading { level, spans, .. } => {
                out.push(format!("{pad}h{level} {}", text(spans)))
            }
            Block::Paragraph { spans, .. } => out.push(format!("{pad}p {}", text(spans))),
            Block::Quote { blocks, .. } => {
                out.push(format!("{pad}quote"));
                outline(blocks, depth + 1, out);
            }
            Block::List { start, items, .. } => {
                out.push(format!(
                    "{pad}{}",
                    if start.is_some() { "ol" } else { "ul" }
                ));
                for item in items {
                    out.push(format!("{pad}- item"));
                    outline(&item.blocks, depth + 1, out);
                }
            }
            Block::Table { header, rows, .. } => {
                let row = |r: &Vec<Cell>| {
                    r.iter()
                        .map(|c| text(&c.spans))
                        .collect::<Vec<_>>()
                        .join(" | ")
                };
                out.push(format!("{pad}table {}", row(header)));
                for r in rows {
                    out.push(format!("{pad}row {}", row(r)));
                }
            }
            other => out.push(format!("{pad}{other:?}")),
        }
    }
}
fn lines(doc: &OfficeDocument) -> Vec<String> {
    let mut out = vec![];
    outline(&doc.content.blocks, 0, &mut out);
    out
}

#[test]
fn documents_from_three_producers_read_the_same() {
    let expected = [
        "h1 Quick look sample",
        "p Isolinear tests",
        "h1 Introduction",
        "p Plain text with bold, italic, struck and a link.",
        "h2 Lists",
        "ul",
        "- item",
        "  p first bullet",
        "- item",
        "  p second bullet",
        "  ul",
        "  - item",
        "    p nested bullet",
        "ol",
        "- item",
        "  p first number",
        "- item",
        "  p second number",
        "quote",
        "  p A quoted paragraph.",
        "table Name | Value",
        "row alpha | 1",
        "row beta | 2",
        "p Text with a footnote.[1]",
        "p Unicode: héllo wörld 日本語 😀 & < >.",
        "h2 Notes",
        "p [1] The footnote text.",
    ];
    for name in ["pandoc.docx", "libreoffice.docx", "libreoffice.odt"] {
        let doc = read(&fixture(name)).unwrap().unwrap();
        assert_eq!(lines(&doc), expected, "{name}");
        // Inline styles and the link target (kept for the UI, never opened by the reader).
        let Block::Paragraph { spans, .. } = &doc.content.blocks[3] else {
            panic!("{name}")
        };
        let styled = |t: &str| spans.iter().find(|s| s.text == t).unwrap().style.clone();
        assert!(styled("bold").strong, "{name}");
        assert!(styled("italic").emphasis, "{name}");
        assert!(styled("struck").strike, "{name}");
        assert_eq!(
            styled("link").link.as_deref(),
            Some("https://example.org/page"),
            "{name}"
        );
        assert!(doc.warnings.is_empty(), "{name}: {:?}", doc.warnings);
    }
}

#[test]
fn other_zip_files_are_not_documents() {
    let bytes = zip(&[
        ("readme.txt", b"hello"),
        ("xl/workbook.xml", b"<workbook/>"),
    ]);
    assert!(read(&bytes).is_none());
    // An ODF spreadsheet is not a text document.
    let bytes = zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.spreadsheet",
        ),
        ("content.xml", b"<x/>"),
    ]);
    assert!(read(&bytes).is_none());
}

/// A minimal stored ZIP with real CRCs.
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut out, mut central) = (vec![], vec![]);
    for (name, data) in entries {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let header = |sig: &[u8], central: bool, offset: u32| {
            let mut h = sig.to_vec();
            if central {
                h.extend(20u16.to_le_bytes());
            }
            h.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            h.extend(crc.sum().to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes());
            if central {
                h.extend([0u8; 10]);
                h.extend(offset.to_le_bytes());
            }
            h.extend(name.as_bytes());
            h
        };
        central.extend(header(b"PK\x01\x02", true, out.len() as u32));
        out.extend(header(b"PK\x03\x04", false, 0));
        out.extend(*data);
    }
    let at = out.len() as u32;
    out.extend(&central);
    out.extend(b"PK\x05\x06\0\0\0\0");
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(at.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing""#;

fn docx(body: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
    let document =
        format!(r#"<?xml version="1.0"?><w:document {W}><w:body>{body}</w:body></w:document>"#);
    let mut entries: Vec<(&str, &[u8])> = vec![("word/document.xml", document.as_bytes())];
    entries.extend_from_slice(extra);
    zip(&entries)
}

#[test]
fn tracked_changes_drawings_fields_and_entities() {
    let body = concat!(
        // Inserted text shows, deleted text does not.
        r#"<w:p><w:r><w:t xml:space="preserve">kept </w:t></w:r><w:ins><w:r><w:t>added</w:t></w:r></w:ins><w:del><w:r><w:delText>removed</w:delText></w:r></w:del></w:p>"#,
        // A drawing inside AlternateContent: one placeholder (the fallback is not repeated).
        r#"<w:p><w:r><mc:AlternateContent><mc:Choice><w:drawing><wp:inline><wp:docPr id="1" name="Picture 1" descr="A red square"/></wp:inline></w:drawing></mc:Choice><mc:Fallback><w:pict/></mc:Fallback></mc:AlternateContent></w:r></w:p>"#,
        // Field codes are not shown; their results are.
        r#"<w:p><w:r><w:instrText>PAGE \* MERGEFORMAT</w:instrText></w:r><w:r><w:t>3</w:t></w:r></w:p>"#,
        // Character and predefined entity references; tab and break.
        r#"<w:p><w:r><w:t>a &amp; b &#x263A;</w:t><w:tab/><w:t>c</w:t><w:br/><w:t>d</w:t></w:r></w:p>"#,
        // A table with a spanned cell and a nested table, flattened into its cell.
        r#"<w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>wide</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>z</w:t></w:r></w:p></w:tc></w:tr>"#,
        r#"<w:tr><w:tc><w:tbl><w:tr><w:tc><w:p><w:r><w:t>i1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>i2</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:tc><w:tc><w:p><w:r><w:t>y</w:t></w:r></w:p></w:tc><w:tc><w:p/></w:tc></w:tr></w:tbl>"#,
    );
    let doc = read(&docx(body, &[("word/vbaProject.bin", b"macro")]))
        .unwrap()
        .unwrap();
    assert_eq!(
        lines(&doc),
        [
            "p kept added",
            "p [image: A red square]",
            "p 3",
            "p a & b ☺\tc\nd",
            "table wide | z",
            "row i1 | i2 | y | ",
        ]
    );
    let warnings = doc.warnings.join("\n");
    assert!(warnings.contains("tracked changes"), "{warnings}");
    assert!(warnings.contains("placeholders"), "{warnings}");
    assert!(
        warnings.contains("macros; they are never run"),
        "{warnings}"
    );
}

#[test]
fn entity_declarations_are_never_expanded() {
    // A DOCTYPE with an internal entity (a billion-laughs seed): the reference stays literal.
    let document = format!(
        r#"<?xml version="1.0"?><!DOCTYPE w:document [<!ENTITY lol "lollollol">]><w:document {W}><w:body><w:p><w:r><w:t>&lol;</w:t></w:r></w:p></w:body></w:document>"#
    );
    let doc = read(&zip(&[("word/document.xml", document.as_bytes())]))
        .unwrap()
        .unwrap();
    assert_eq!(lines(&doc), ["p &lol;"]);
}

#[test]
fn limits_are_enforced() {
    // Nesting deeper than the limit is an error, not a stack overflow.
    let deep = format!("{}{}", "<w:sdt>".repeat(400), "</w:sdt>".repeat(400));
    let e = read(&docx(&deep, &[])).unwrap().err().unwrap();
    assert!(e.contains("nest deeper"), "{e}");

    // More paragraphs than the block budget: cut, with a warning.
    let many = "<w:p><w:r><w:t>x</w:t></w:r></w:p>".repeat(office::MAX_BLOCKS + 10);
    let started = Instant::now();
    let doc = read(&docx(&many, &[])).unwrap().unwrap();
    assert!(doc.content.truncated);
    assert_eq!(doc.content.blocks.len(), office::MAX_BLOCKS);
    assert!(doc.warnings.iter().any(|w| w.contains("stops at")));
    assert!(started.elapsed().as_secs() < 10);

    // A part declared larger than the part limit is refused before extraction.
    let mut bytes = docx("<w:p/>", &[]);
    let listing = list_zip(&mut Cursor::new(&bytes), NEVER).unwrap();
    let entry = &listing.entries[0];
    // Patch the declared size in the central directory (offset 24 of its record).
    let central = bytes.len() - 22 - (46 + entry.name.len());
    // (Not 0xFFFFFFFF: that value means the size is in a ZIP64 field.)
    bytes[central + 24..central + 28].copy_from_slice(&(u32::MAX - 15).to_le_bytes());
    let e = read(&bytes).unwrap().err().unwrap();
    assert!(e.contains("parts up to"), "{e}");

    // Cancellation stops a long walk.
    let mut cursor = Cursor::new(docx(&many, &[]));
    let listing = list_zip(&mut cursor, NEVER).unwrap();
    let r = office::read(&mut cursor, &listing, &|| true).unwrap();
    assert!(r.is_err());
}

#[test]
fn password_protected_odt_is_refused() {
    let bytes = zip(&[
        ("mimetype", b"application/vnd.oasis.opendocument.text"),
        ("content.xml", b"\x01\x02 ciphertext"),
        (
            "META-INF/manifest.xml",
            b"<manifest:file-entry><manifest:encryption-data/></manifest:file-entry>",
        ),
    ]);
    let e = read(&bytes).unwrap().err().unwrap();
    assert!(e.contains("password-protected"), "{e}");
}

#[test]
fn odt_white_space_repeats_and_annotations() {
    let content = r#"<?xml version="1.0"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0">
  <office:body>
    <office:text>
      <text:p>  two   words<text:s text:c="3"/>spaced<office:annotation><text:p>a comment</text:p></office:annotation></text:p>
      <table:table>
        <table:table-row><table:table-cell><text:p>a</text:p></table:table-cell><table:table-cell table:number-columns-repeated="16384"/></table:table-row>
        <table:table-row table:number-rows-repeated="1000000"><table:table-cell table:number-columns-repeated="16384"/></table:table-row>
        <table:table-row table:number-rows-repeated="2"><table:table-cell><text:p>b</text:p></table:table-cell></table:table-row>
      </table:table>
      <text:p>with <draw:frame draw:name="Logo"><draw:image/></draw:frame> image</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;
    let bytes = zip(&[
        ("mimetype", b"application/vnd.oasis.opendocument.text"),
        ("content.xml", content.as_bytes()),
        ("Basic/Standard/Module1.xml", b"<x/>"),
    ]);
    let doc = read(&bytes).unwrap().unwrap();
    assert_eq!(
        lines(&doc),
        [
            "p two words   spaced",
            "table a",
            "row b",
            "row b",
            "p with [image: Logo] image",
        ]
    );
    let warnings = doc.warnings.join("\n");
    assert!(warnings.contains("1 comments"), "{warnings}");
    assert!(warnings.contains("macros"), "{warnings}");
}

#[test]
fn damaged_documents_never_panic() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    // Mutate the XML parts themselves (re-zipped with valid CRCs), so damage reaches the readers.
    let mut bases = vec![];
    for name in ["pandoc.docx", "libreoffice.odt"] {
        let bytes = fixture(name);
        let mut cursor = Cursor::new(&bytes);
        let listing = list_zip(&mut cursor, NEVER).unwrap();
        let mut parts = vec![];
        for entry in listing.entries.iter().filter(|e| !e.dir) {
            let mut out = vec![];
            super::zip::extract_zip(&mut cursor, entry, &mut out).unwrap();
            parts.push((entry.name.clone(), out));
        }
        bases.push(parts);
    }
    for i in 0..600 {
        let mut parts = bases[i % bases.len()].clone();
        let xml: Vec<usize> = (0..parts.len())
            .filter(|&p| parts[p].0.ends_with(".xml"))
            .collect();
        for _ in 0..(next() % 3 + 1) {
            let (_, data) = &mut parts[xml[(next() % xml.len() as u64) as usize]];
            for _ in 0..(next() % 16 + 1) {
                let at = (next() % data.len().max(1) as u64) as usize;
                if at < data.len() {
                    data[at] = match next() % 5 {
                        0 => b'<',
                        1 => b'>',
                        2 => b'/',
                        3 => b'&',
                        _ => next() as u8,
                    };
                }
            }
            if next() % 4 == 0 {
                let cut = (next() % data.len().max(1) as u64) as usize;
                data.truncate(cut);
            }
        }
        let entries: Vec<(&str, &[u8])> = parts
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect();
        let started = Instant::now();
        let _ = read(&zip(&entries));
        assert!(started.elapsed().as_secs() < 5, "case {i}");
    }
}

fn rendered(bytes: &[u8]) -> Result<String, String> {
    let mut cursor = Cursor::new(bytes);
    let listing = list_zip(&mut cursor, NEVER).map_err(|e| e.to_string())?;
    let doc = office::read(&mut cursor, &listing, NEVER).ok_or("Not a text document")??;
    html::render(&doc, &mut cursor, &listing)
}
fn rels(items: &str) -> String {
    format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{items}</Relationships>"#
    )
}
fn relationship(id: &str, kind: &str, target: &str, external: bool) -> String {
    format!(r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}" {}/ >"#, if external { r#"TargetMode="External""# } else { "" }).replace("/ >", "/>")
}
fn odt(body: &str, extras: &[(&str, &[u8])]) -> Vec<u8> {
    let xml = format!(
        r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:text>{body}</office:text></office:body></office:document-content>"#
    );
    let mut parts = vec![
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text".as_slice(),
        ),
        ("content.xml", xml.as_bytes()),
    ];
    parts.extend_from_slice(extras);
    zip(&parts)
}

#[test]
fn producer_documents_render_safe_html_through_public_api() {
    for (file, format) in [
        ("pandoc.docx", "docx"),
        ("libreoffice.docx", "docx"),
        ("libreoffice.odt", "odt"),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/office")
            .join(file);
        let html = super::to_html(format, &path).unwrap();
        for expected in [
            "<h1>Introduction</h1>",
            "<h2>Lists</h2>",
            "<strong>bold</strong>",
            "<em>italic</em>",
            "<s>struck</s>",
            "<ul>",
            "<ol>",
            "<blockquote>",
            "<table>",
            "The footnote text.",
            "héllo wörld 日本語 😀 &amp; &lt; &gt;",
            "<a href=\"https://example.org/page\">",
        ] {
            assert!(
                html.contains(expected),
                "{file}: missing {expected}: {html}"
            );
        }
        assert!(!html.contains("<script"));
    }
}

#[test]
fn escapes_text_and_attributes_and_rejects_unsafe_link_schemes() {
    let links = rels(
        &[
            relationship(
                "ok",
                "hyperlink",
                "https://example.org/?x=&quot;&amp;y=&lt;x&gt;",
                true,
            ),
            relationship("bad", "hyperlink", "javascript:alert(1)", true),
            relationship("local", "hyperlink", "file:///etc/passwd", true),
        ]
        .join(""),
    );
    let bytes = docx(
        r#"<w:p><w:hyperlink r:id="ok"><w:r><w:t>&lt;script&gt;&amp;&quot;'</w:t></w:r></w:hyperlink><w:hyperlink r:id="bad"><w:r><w:t>bad</w:t></w:r></w:hyperlink><w:hyperlink r:id="local"><w:r><w:t>local</w:t></w:r></w:hyperlink></w:p>"#,
        &[("word/_rels/document.xml.rels", links.as_bytes())],
    );
    let html = rendered(&bytes).unwrap();
    assert!(html.contains("&lt;script&gt;&amp;&quot;&#39;"), "{html}");
    assert!(
        html.contains("href=\"https://example.org/?x=&quot;&amp;y=&lt;x&gt;\""),
        "{html}"
    );
    assert!(!html.contains("javascript:"));
    assert!(!html.contains("file:"));
}

#[test]
fn word_underlines_superscripts_subscripts_and_heading_ids() {
    let bytes = docx(
        r#"<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:rPr><w:u/><w:vertAlign w:val="superscript"/></w:rPr><w:t>up</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="subscript"/></w:rPr><w:t>down</w:t></w:r></w:p>"#,
        &[],
    );
    assert_eq!(
        rendered(&bytes).unwrap(),
        "<h2><u><sup>up</sup></u><sub>down</sub></h2>"
    );
}

#[test]
fn odf_inherited_inline_styles_and_code_paragraphs() {
    let styles = br#"<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"><office:styles><style:style style:name="Base"><style:text-properties style:text-underline-style="solid" style:text-position="super 58%"/></style:style><style:style style:name="Child" style:parent-style-name="Base"/><style:style style:name="Code" style:display-name="Preformatted Text"/></office:styles></office:document-styles>"#;
    let bytes = odt(
        r#"<text:p><text:span text:style-name="Child">x</text:span></text:p><text:p text:style-name="Code">a &lt; b</text:p>"#,
        &[("styles.xml", styles)],
    );
    let html = rendered(&bytes).unwrap();
    assert!(html.contains("<u><sup>x</sup></u>"), "{html}");
    assert!(html.contains("<pre><code>a &lt; b</code></pre>"), "{html}");
}

#[test]
fn word_merged_cells_have_column_and_row_spans() {
    let bytes = docx(
        r#"<w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>wide</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>next</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vMerge/></w:tcPr><w:p/></w:tc><w:tc><w:p><w:r><w:t>last</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        &[],
    );
    let html = rendered(&bytes).unwrap();
    assert!(
        html.contains("<th colspan=\"2\" rowspan=\"2\">wide</th>"),
        "{html}"
    );
    assert_eq!(html.matches("<td ").count(), 1);
}

#[test]
fn odt_merged_cells_skip_covered_cells() {
    let bytes = odt(
        r#"<table:table><table:table-row><table:table-cell table:number-columns-spanned="2" table:number-rows-spanned="2"><text:p>wide</text:p></table:table-cell><table:covered-table-cell/><table:table-cell><text:p>next</text:p></table:table-cell></table:table-row><table:table-row><table:covered-table-cell/><table:covered-table-cell/><table:table-cell><text:p>last</text:p></table:table-cell></table:table-row></table:table>"#,
        &[],
    );
    let html = rendered(&bytes).unwrap();
    assert!(
        html.contains("<th colspan=\"2\" rowspan=\"2\">wide</th>"),
        "{html}"
    );
    assert_eq!(html.matches("<th ").count(), 2);
    assert_eq!(html.matches("<td ").count(), 1);
}

#[test]
fn embedded_raster_images_only_and_no_external_resources() {
    let links = rels(
        &[
            relationship("png", "image", "media/p.png", false),
            relationship("svg", "image", "media/x.svg", false),
            relationship("remote", "image", "https://example.org/p.png", true),
        ]
        .join(""),
    );
    let drawing = |id: &str| {
        format!(
            r#"<w:p><w:r><w:drawing><wp:inline><wp:docPr descr="&quot;&gt;&lt;script&gt;"/><a:blip xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" r:embed="{id}"/></wp:inline></w:drawing></w:r></w:p>"#
        )
    };
    let bytes = docx(
        &[drawing("png"), drawing("svg"), drawing("remote")].join(""),
        &[
            ("word/_rels/document.xml.rels", links.as_bytes()),
            ("word/media/p.png", b"\x89PNG\r\n\x1a\n"),
            ("word/media/x.svg", b"<svg><script>alert(1)</script></svg>"),
        ],
    );
    let html = rendered(&bytes).unwrap();
    assert_eq!(html.matches("<img ").count(), 1, "{html}");
    assert!(html.contains("data:image/png;base64,iVBORw0KGgo="));
    assert!(!html.contains("<script>"));
    assert!(!html.contains("image/svg"));
    assert!(!html.contains("src=\"https:"));
    for (data, mime) in [
        (b"GIF89a".as_slice(), "gif"),
        (b"\xff\xd8\xff".as_slice(), "jpeg"),
    ] {
        let bytes = odt(
            r#"<text:p><draw:frame draw:name="Logo"><draw:image xlink:href="Pictures/p"/></draw:frame></text:p>"#,
            &[("Pictures/p", data)],
        );
        assert!(
            rendered(&bytes)
                .unwrap()
                .contains(&format!("data:image/{mime};base64,"))
        );
    }
}

#[test]
fn footnotes_endnotes_and_comments_are_notes_at_the_end() {
    let links = rels(
        &[
            relationship("f", "footnotes", "footnotes.xml", false),
            relationship("e", "endnotes", "endnotes.xml", false),
            relationship("c", "comments", "comments.xml", false),
        ]
        .join(""),
    );
    let f = format!(
        r#"<w:footnotes {W}><w:footnote w:id="7"><w:p><w:r><w:t>foot &lt;x&gt;</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let e = format!(
        r#"<w:endnotes {W}><w:endnote w:id="8"><w:p><w:r><w:t>end</w:t></w:r></w:p></w:endnote></w:endnotes>"#
    );
    let c = format!(r#"<w:comments {W}><w:comment w:id="1"/></w:comments>"#);
    let bytes = docx(
        r#"<w:p><w:r><w:t>body</w:t><w:endnoteReference w:id="8"/><w:footnoteReference w:id="7"/></w:r></w:p>"#,
        &[
            ("word/_rels/document.xml.rels", links.as_bytes()),
            ("word/footnotes.xml", f.as_bytes()),
            ("word/endnotes.xml", e.as_bytes()),
            ("word/comments.xml", c.as_bytes()),
        ],
    );
    let html = rendered(&bytes).unwrap();
    assert!(html.contains("<span class=\"note\">Has 1 comments (not shown)</span>"));
    assert!(
        html.find(">end</span>")
            .zip(html.find("foot &lt;x&gt;"))
            .is_some_and(|(e, f)| e < f),
        "{html}"
    );
}

#[test]
fn arbitrary_zip_offsets_sizes_checksums_and_truncation_are_rejected() {
    let bytes = docx("<w:p><w:r><w:t>x</w:t></w:r></w:p>", &[]);
    for n in 0..bytes.len() {
        assert!(rendered(&bytes[..n]).is_err(), "truncation {n}");
    }
    let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    for field in [16, 20, 24, 42] {
        let mut broken = bytes.clone();
        broken[central + field..central + field + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(rendered(&broken).is_err(), "field {field}");
    }
}

#[test]
fn repeated_cells_rows_and_large_text_stop_with_a_note() {
    let bytes = odt(
        r#"<table:table><table:table-row table:number-rows-repeated="18446744073709551615"><table:table-cell table:number-columns-repeated="18446744073709551615"><text:p>x</text:p></table:table-cell></table:table-row></table:table>"#,
        &[],
    );
    let html = rendered(&bytes).unwrap();
    assert!(html.contains("preview stops"));
    assert!(html.len() < 400_000);
    let bytes = docx(
        &format!(
            "<w:p><w:r><w:t><![CDATA[{}]]></w:t></w:r></w:p>",
            "&".repeat(4_000_000)
        ),
        &[],
    );
    let html = rendered(&bytes).unwrap();
    assert!(html.len() <= html::TEXT_LIMIT);
    assert!(html.contains("16 MiB HTML text limit"));
    assert!(html.contains("</p><p><span class=\"note\">The preview stops"));
}

#[test]
fn twenty_thousand_mutations_reach_xml_and_zip_and_html() {
    let mut state = 0x8b34_d782_491f_15e7u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let word = format!(
        r#"<w:document {W}><w:body><w:p><w:pPr><w:numPr><w:ilvl w:val="18446744073709551615"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>a &amp; b</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#
    );
    let odf = odt("<text:p>sample</text:p>", &[]);
    for n in 0..20_000 {
        let mut data = if n % 3 == 0 {
            odf.clone()
        } else {
            word.as_bytes().to_vec()
        };
        for _ in 0..(next() % 12 + 1) {
            let at = next() as usize % data.len();
            data[at] = next() as u8;
        }
        if n % 7 == 0 {
            data.truncate(next() as usize % data.len());
        }
        let bytes = if n % 3 == 0 {
            data
        } else {
            zip(&[("word/document.xml", &data)])
        };
        if let Ok(html) = rendered(&bytes) {
            assert!(html.len() <= html::TEXT_LIMIT);
            assert!(!html.contains("<script"));
        }
    }
}

#[test]
fn image_budget_counts_repeated_and_unsupported_images() {
    let mut image = vec![0; 1024 * 1024];
    image[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let frame =
        r#"<text:p><draw:frame><draw:image xlink:href="Pictures/p"/></draw:frame></text:p>"#;
    let bytes = odt(&frame.repeat(33), &[("Pictures/p", &image)]);
    let html = rendered(&bytes).unwrap();
    assert_eq!(html.matches("<img ").count(), 32);
    assert_eq!(html.matches("32 MiB image limit").count(), 1);
    image[..8].fill(0);
    let bytes = odt(&frame.repeat(33), &[("Pictures/p", &image)]);
    let html = rendered(&bytes).unwrap();
    assert!(!html.contains("<img "));
    assert!(html.contains("32 MiB image limit"));
}

#[test]
fn templates_and_macro_aliases_use_the_public_reader() {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/office");
    for format in ["docx", "docm", "dotx"] {
        let html = super::to_html(format, &base.join("pandoc.docx")).unwrap();
        assert!(html.contains("Quick look sample"));
        if format == "docm" {
            assert!(
                html.starts_with("<p><span class=\"note\">Contains macros; they are never run")
            );
        }
    }
    assert!(
        super::to_html("ott", &base.join("libreoffice.odt"))
            .unwrap()
            .contains("Quick look sample")
    );
    assert!(super::to_html("docx", &base.join("libreoffice.odt")).is_err());
    assert!(super::to_html("odt", &base.join("pandoc.docx")).is_err());
    assert!(super::to_html("html", &base.join("pandoc.docx")).is_err());
}

#[test]
fn odt_template_mime_type_is_supported() {
    let bytes = odt("<text:p>template</text:p>", &[]);
    let mut cursor = Cursor::new(&bytes);
    let listing = list_zip(&mut cursor, NEVER).unwrap();
    let content = office::part(&mut cursor, &listing, "content.xml")
        .unwrap()
        .unwrap();
    let bytes = zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text-template",
        ),
        ("content.xml", &content),
    ]);
    assert_eq!(rendered(&bytes).unwrap(), "<p>template</p>");
}

#[test]
fn merged_cell_expansion_stays_within_table_limits() {
    let cell = r#"<w:tc><w:tcPr><w:gridSpan w:val="64"/></w:tcPr><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc>"#;
    let bytes = docx(
        &format!("<w:tbl><w:tr>{}</w:tr></w:tbl>", cell.repeat(20)),
        &[],
    );
    let html = rendered(&bytes).unwrap();
    assert_eq!(html.matches("colspan=\"64\"").count(), 16);
    assert!(html.contains("preview stops"));
    let start = r#"<w:tr><w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>start</w:t></w:r></w:p></w:tc></w:tr>"#;
    let continuation = r#"<w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc></w:tr>"#;
    let bytes = docx(
        &format!("<w:tbl>{start}{}</w:tbl>", continuation.repeat(65)),
        &[],
    );
    let html = rendered(&bytes).unwrap();
    assert!(html.contains("rowspan=\"64\""));
    assert!(!html.contains("rowspan=\"65\""));
    assert!(html.contains("preview stops"));
}

#[test]
fn odt_explicit_space_repeats_report_their_limit() {
    let bytes = odt(
        r#"<text:p>a<text:s text:c="18446744073709551615"/>b</text:p>"#,
        &[],
    );
    let html = rendered(&bytes).unwrap();
    assert!(html.contains("preview stops"));
    assert!(html.contains(&format!("a{}b", " ".repeat(1000))));
}
