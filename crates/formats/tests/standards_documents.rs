//! Documents, mail and containers recognized as standards: EML, MBOX, Outlook MSG, legacy XLS,
//! PPTX/ODP/PPT, ASiC-E, PDF (embedded files, Factur-X, signatures), fixed-width records with a
//! layout, and insurance bordereaux.
//!
//! The fixtures in tests/fixtures/standards are written by
//! `python3 -I tests/fixtures/standards/make_documents.py` (standard library only, deterministic):
//! Compound Files, BIFF8 records, PowerPoint records, PDF objects and DER structures are
//! assembled by hand there from the published specifications.
use isolinear_formats::{
    detect,
    inspect::{Inspection, Stage, State, Table, Visual, inspect_file},
};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/standards")
        .join(name)
}

fn inspect(path: &Path) -> Option<Result<Inspection, String>> {
    let head = std::fs::read(path).unwrap();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let format = detect(&name, &head[..head.len().min(65536)]).format.id;
    inspect_file(path, format, &|| false)
}

fn open(name: &str) -> Inspection {
    inspect(&fixture(name))
        .unwrap_or_else(|| panic!("{name} not recognized"))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn fact<'a>(i: &'a Inspection, label: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.label == label)
        .map(|f| f.value.as_str())
        .unwrap_or_else(|| panic!("no fact {label:?} in {:#?}", i.facts))
}

fn check(i: &Inspection, stage: Stage, contains: &str) -> State {
    i.checks
        .iter()
        .find(|c| c.stage == stage && c.detail.contains(contains))
        .unwrap_or_else(|| panic!("no {stage:?} check {contains:?} in {:#?}", i.checks))
        .state
}

fn table<'a>(i: &'a Inspection, name: &str) -> &'a Table {
    i.tables.iter().find(|t| t.name == name).unwrap_or_else(|| {
        panic!(
            "no table {name:?} in {:?}",
            i.tables.iter().map(|t| &t.name).collect::<Vec<_>>()
        )
    })
}

fn cell<'a>(t: &'a Table, row: usize, column: &str) -> &'a str {
    let c = t
        .columns
        .iter()
        .position(|c| c == column)
        .unwrap_or_else(|| panic!("no column {column:?} in {:?}", t.columns));
    &t.rows[row].cells[c]
}

fn pictures(i: &Inspection) -> Vec<&str> {
    i.visuals
        .iter()
        .filter_map(|v| match v {
            Visual::Picture { caption, bytes } => {
                assert!(bytes.starts_with(b"\x89PNG"), "{caption} is a PNG");
                Some(caption.as_str())
            }
            _ => None,
        })
        .collect()
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("isolinear-docs-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn eml_headers_mime_body_attachments_and_pictures() {
    let i = open("message.eml");
    assert_eq!(i.family, "Internet mail");
    assert_eq!(fact(&i, "Subject"), "Rapport mensuel et facture €");
    assert_eq!(fact(&i, "From address"), "José García <jose@example.org>");
    assert_eq!(
        fact(&i, "To addresses"),
        "\"Doe, Ann\" <ann@example.net>, bob@example.net"
    );
    assert_eq!(fact(&i, "Date"), "2026-11-01 10:14:55 +0100");
    assert_eq!(fact(&i, "Received hops"), "2");
    assert_eq!(
        fact(&i, "Authentication results"),
        "spf=pass, dkim=pass, dmarc=pass"
    );
    assert_eq!(fact(&i, "DKIM signing domain"), "example.org");
    assert_eq!(fact(&i, "References"), "2 earlier messages");
    // The plain text part is preferred, quoted-printable decoded (soft line break joined).
    assert_eq!(fact(&i, "Body"), "text/plain");
    let body = table(&i, "Body text");
    let text: Vec<&str> = body.rows.iter().map(|r| r.cells[0].as_str()).collect();
    assert_eq!(text[0], "Hello Ann,");
    assert!(
        text[2].contains("1 234,50 €") && text[2].ends_with("long and soft-wrapped."),
        "{text:?}"
    );
    let a = table(&i, "Attachments");
    assert_eq!(a.rows.len(), 2);
    assert_eq!(cell(a, 0, "Filename"), "logo.png");
    assert_eq!(cell(a, 0, "Content ID"), "logo@example.org");
    assert_eq!(cell(a, 1, "Filename"), "facture n°7.pdf");
    assert_eq!(cell(a, 1, "Decoded size"), "37");
    assert_eq!(pictures(&i), ["logo.png"]);
    // MIME outline: mixed > alternative > plain, html; png; pdf.
    let outline: Vec<(u16, &str)> = i
        .outline
        .iter()
        .map(|o| (o.depth, o.label.as_str()))
        .collect();
    assert_eq!(
        outline,
        [
            (0, "multipart/mixed"),
            (1, "multipart/alternative"),
            (2, "text/plain"),
            (2, "text/html"),
            (1, "image/png"),
            (1, "application/pdf")
        ]
    );
    // Sources point at the bytes.
    let text = std::fs::read(fixture("message.eml")).unwrap();
    let subject = i.facts.iter().find(|f| f.label == "Subject").unwrap();
    let s = subject.source.clone().unwrap();
    assert!(text[s.start as usize..s.end as usize].starts_with(b"Subject:"));
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "From and Date"), State::Passed);
    assert_eq!(check(&i, Stage::Signature, "not verified"), State::NotDone);
    assert!(
        i.notes.iter().any(|n| n.contains("1 remote resource ")),
        "{:?}",
        i.notes
    );

    // The HTML body is used when there is no plain text, with tags and scripts removed.
    let dir = temp_dir("eml");
    let html = dir.join("html.eml");
    std::fs::write(
        &html,
        &b"From: a@example.org\r\nDate: Sun, 1 Nov 2026 10:00:00 +0000\r\nSubject: x\r\nContent-Type: text/html; charset=iso-8859-1\r\n\r\n<p>Caf\xE9 <b>ouvert</b></p><script>alert(1)</script>"[..],
    )
    .unwrap();
    let i = inspect(&html).unwrap().unwrap();
    assert!(fact(&i, "Body").starts_with("text/html"));
    assert_eq!(table(&i, "Body text").rows[0].cells[0], "Café ouvert");
    // A message without Date fails the required-fields rule; a cut multipart is partial.
    let cut = dir.join("cut.eml");
    let full = std::fs::read(fixture("message.eml")).unwrap();
    let mut b = full.clone();
    let date = b.windows(5).position(|w| w == b"Date:").unwrap();
    b[date..date + 4].copy_from_slice(b"Xate");
    b.truncate(b.len() - 30);
    std::fs::write(&cut, &b).unwrap();
    let i = inspect(&cut).unwrap().unwrap();
    assert_eq!(
        check(&i, Stage::Semantic, "Missing required"),
        State::Failed
    );
    assert_eq!(check(&i, Stage::Parsed, "closing boundary"), State::Partial);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mbox_lists_messages_with_ranges_and_facts() {
    let i = open("mailbox.mbox");
    assert_eq!(i.kind, "Mailbox (mbox)");
    assert_eq!(fact(&i, "Messages"), "3");
    assert_eq!(fact(&i, "First date"), "2026-10-31 09:00:00 +0000");
    assert_eq!(fact(&i, "Last date"), "2026-11-02 08:30:00 +0000");
    assert_eq!(fact(&i, "Distinct senders"), "2");
    assert_eq!(
        fact(&i, "Most frequent sender addresses"),
        "ann@example.net (2), jose@example.org (1)"
    );
    assert!(fact(&i, "Escaped \">From\" lines").starts_with('1'));
    let t = table(&i, "Messages");
    assert_eq!(t.rows.len(), 3);
    assert_eq!(cell(t, 1, "From address"), "José <jose@example.org>");
    assert_eq!(cell(t, 1, "Subject"), "Re: Planning");
    assert_eq!(cell(t, 1, "Attachments"), "1");
    assert_eq!(cell(t, 0, "Attachments"), "0");
    let bytes = std::fs::read(fixture("mailbox.mbox")).unwrap();
    let s = t.rows[2].source.clone().unwrap();
    assert!(bytes[s.start as usize..].starts_with(b"From ann@example.net Mon"));
    assert_eq!(s.end as usize, bytes.len());
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
}

#[test]
fn outlook_msg_properties_recipients_and_attachments() {
    let i = open("message.msg");
    assert_eq!(i.family, "Microsoft Outlook");
    assert_eq!(i.kind, "Outlook item (IPM.Note)");
    assert_eq!(fact(&i, "Subject"), "Quarterly figures – draft");
    assert_eq!(fact(&i, "Sender name"), "José García");
    assert_eq!(fact(&i, "Sender address"), "jose@example.org");
    assert_eq!(fact(&i, "Display To names"), "Ann Doe");
    assert_eq!(
        fact(&i, "Sent (client submit time)"),
        "2026-11-01 09:14:15 UTC"
    );
    assert_eq!(fact(&i, "Importance"), "high");
    assert_eq!(fact(&i, "Received hops"), "2");
    assert!(fact(&i, "Authentication results").contains("spf=pass"));
    assert_eq!(table(&i, "Body text").rows[0].cells[0], "Hello Ann,");
    let r = table(&i, "Recipients");
    assert_eq!(r.rows.len(), 2);
    assert_eq!(
        r.rows[0].cells,
        ["To", "Ann Doe", "ann@example.net", "SMTP"]
    );
    assert_eq!(cell(r, 1, "Type"), "Cc");
    assert_eq!(cell(r, 1, "Email address"), "bob@example.net");
    let a = table(&i, "Attachments");
    assert_eq!(cell(a, 0, "Filename"), "chart.png");
    assert_eq!(cell(a, 0, "MIME type"), "image/png");
    assert_eq!(cell(a, 0, "Method"), "by value");
    // The second attachment is larger than the mini stream cutoff: read from regular sectors.
    assert_eq!(cell(a, 1, "Size"), "5120");
    assert_eq!(pictures(&i), ["chart.png"]);
    let p = table(&i, "Properties");
    let subject = p.rows.iter().find(|r| r.cells[1] == "0x0037001F").unwrap();
    assert_eq!(subject.cells[2], "Subject");
    assert_eq!(subject.cells[3], "String");
    // Stream sources point at the UTF-16 text in the file.
    let bytes = std::fs::read(fixture("message.msg")).unwrap();
    let s = subject.source.clone().unwrap();
    assert!(bytes[s.start as usize..s.end as usize].starts_with(&[b'Q', 0, b'u', 0]));
    assert!(
        p.rows
            .iter()
            .any(|r| r.cells[0] == "Attachment 2" && r.cells[2] == "AttachDataBinary")
    );
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
}

#[test]
fn xls_sheets_shared_strings_cells_and_dates() {
    let i = open("workbook.xls");
    assert_eq!(i.family, "Microsoft Excel");
    assert_eq!(i.version.as_deref(), Some("BIFF8 (Excel 97–2003)"));
    assert_eq!(fact(&i, "Sheets"), "2");
    assert_eq!(fact(&i, "Shared strings"), "8");
    assert_eq!(fact(&i, "Code page"), "1200 (UTF-16)");
    assert!(fact(&i, "Formulas").starts_with("2 "));
    let sheets = table(&i, "Sheets");
    assert_eq!(sheets.rows[0].cells[..3], ["Data", "worksheet", "visible"]);
    let t = table(&i, "Sheet Data");
    assert_eq!(t.columns, ["Row", "A", "B", "C", "D", "E"]);
    assert_eq!(
        t.rows[0].cells,
        ["1", "Name", "Amount", "Date", "Flag", "Formula"]
    );
    // NUMBER, a custom date format, a boolean and a formula's cached number.
    assert_eq!(
        t.rows[1].cells,
        ["2", "Café", "1234.5", "2026-11-01", "TRUE", "2469"]
    );
    // A string split by CONTINUE, RK integer, built-in date format 14, an error, a cached string.
    assert_eq!(
        t.rows[2].cells,
        [
            "3",
            "Zoë Ünal",
            "42",
            "2026-11-02",
            "#DIV/0!",
            "cached text"
        ]
    );
    // LABEL and MULRK (12.34 stored ×100, 7).
    assert_eq!(t.rows[3].cells[..4], ["4", "inline label", "12.34", "7"]);
    let notes = table(&i, "Sheet Notes");
    assert_eq!(notes.rows[1].cells, ["2", "3.25"]);
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);

    let i = open("encrypted.xls");
    assert_eq!(check(&i, Stage::Parsed, "encrypted"), State::Partial);
    assert_eq!(table(&i, "Sheets").rows.len(), 2);
    assert!(i.tables.iter().all(|t| !t.name.starts_with("Sheet ")));
}

#[test]
fn compound_file_loops_are_detected() {
    let dir = temp_dir("cfb");
    let mut bytes = std::fs::read(fixture("workbook.xls")).unwrap();
    // The Workbook stream starts at sector 0; point sector 1 back at sector 0.
    let fat_sector = u32::from_le_bytes(bytes[0x4C..0x50].try_into().unwrap()) as usize;
    let fat = (fat_sector + 1) * 512;
    bytes[fat + 4..fat + 8].copy_from_slice(&0u32.to_le_bytes());
    let p = dir.join("loop.xls");
    std::fs::write(&p, &bytes).unwrap();
    let started = std::time::Instant::now();
    let i = inspect(&p).unwrap().unwrap();
    assert!(started.elapsed().as_secs() < 5);
    assert_eq!(check(&i, Stage::Parsed, "damaged or cut"), State::Partial);
    // A directory chain that loops.
    let mut bytes = std::fs::read(fixture("message.msg")).unwrap();
    let dir_start = u32::from_le_bytes(bytes[0x30..0x34].try_into().unwrap()) as usize;
    let fat_sector = u32::from_le_bytes(bytes[0x4C..0x50].try_into().unwrap()) as usize;
    let fat = (fat_sector + 1) * 512;
    let at = fat + dir_start * 4;
    let next = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    if next < 0xFFFF_FFF0 {
        let at2 = fat + next as usize * 4;
        bytes[at2..at2 + 4].copy_from_slice(&(dir_start as u32).to_le_bytes());
    } else {
        bytes[at..at + 4].copy_from_slice(&(dir_start as u32).to_le_bytes());
    }
    let p = dir.join("loop.msg");
    std::fs::write(&p, &bytes).unwrap();
    let started = std::time::Instant::now();
    match inspect(&p).unwrap() {
        Ok(i) => assert_eq!(check(&i, Stage::Parsed, "loops"), State::Partial),
        Err(e) => assert!(!e.is_empty()),
    }
    assert!(started.elapsed().as_secs() < 5);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn presentations_pptx_odp_and_ppt() {
    let i = open("deck.pptx");
    assert_eq!(i.kind, "PowerPoint presentation (.pptx)");
    assert_eq!(fact(&i, "Title"), "Q3 review");
    assert_eq!(fact(&i, "Author"), "Ann Doe");
    assert_eq!(fact(&i, "Slides"), "2");
    let s = table(&i, "Slides");
    // Presentation order (slide2.xml comes first in sldIdLst).
    assert_eq!(cell(s, 0, "Title"), "Quarterly results");
    assert_eq!(cell(s, 1, "Title"), "Revenue & costs");
    assert_eq!(
        cell(s, 1, "Text"),
        "Revenue up 12 % · Costs flat · Footnote box"
    );
    assert_eq!(cell(s, 1, "Notes"), "Mention the new contract.");
    assert_eq!(cell(s, 1, "Pictures"), "1");
    let notes = table(&i, "Speaker notes");
    assert_eq!(notes.rows[0].cells, ["2", "Mention the new contract."]);
    assert_eq!(
        cell(table(&i, "Pictures"), 0, "Part"),
        "ppt/media/image1.png"
    );
    assert_eq!(pictures(&i), ["Slide 2: image1.png"]);
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);

    let i = open("deck.odp");
    assert_eq!(fact(&i, "Title"), "Kick-off");
    assert_eq!(fact(&i, "Author"), "Bob Smith");
    let s = table(&i, "Slides");
    assert_eq!(cell(s, 0, "Title"), "Welcome");
    assert_eq!(cell(s, 0, "Text"), "First point · Second point");
    assert_eq!(cell(s, 0, "Notes"), "Say hello.");
    assert_eq!(cell(s, 1, "Title"), "Closing");
    assert_eq!(cell(s, 1, "Text"), "Thank you");
    assert_eq!(pictures(&i), ["Slide 1: blue.png"]);

    let i = open("deck.ppt");
    assert_eq!(i.kind, "PowerPoint 97–2003 presentation (.ppt)");
    assert_eq!(fact(&i, "Slides"), "2");
    let s = table(&i, "Slides");
    assert_eq!(cell(s, 0, "Title"), "Annual review");
    assert_eq!(
        cell(s, 0, "Text"),
        "Revenue grew · Costs fell · Draft – confidential"
    );
    assert_eq!(cell(s, 0, "Notes"), "Thank the team");
    assert_eq!(cell(s, 1, "Title"), "Next steps");
    assert_eq!(cell(s, 1, "Text"), "Hire two people");
    // The master's template text is not slide text.
    assert!(
        !table(&i, "Slide text")
            .rows
            .iter()
            .any(|r| r.cells[2].contains("Click to edit"))
    );
    assert!(i.notes.iter().any(|n| n.contains("placement")));
}

#[test]
fn asice_entries_manifest_and_signatures_not_verified() {
    let i = open("container.asice");
    assert_eq!(i.family, "ETSI ASiC");
    assert!(i.kind.starts_with("ASiC-E"));
    assert_eq!(fact(&i, "Media type"), "application/vnd.etsi.asic-e+zip");
    let e = table(&i, "Entries");
    let signed = |name: &str| {
        e.rows
            .iter()
            .find(|r| r.cells[0] == name)
            .map(|r| r.cells[5].clone())
            .unwrap()
    };
    assert_eq!(signed("invoice.pdf"), "yes");
    assert_eq!(signed("data file.xml"), "yes");
    assert_eq!(signed("META-INF/manifest.xml"), "—");
    assert_eq!(table(&i, "Manifest").rows.len(), 3);
    let s = table(&i, "Signatures");
    assert_eq!(cell(s, 0, "Signing time"), "2026-11-01T09:00:00Z");
    assert_eq!(
        cell(s, 0, "Signer certificate subject"),
        "C=LU, GN=Ann, SN=Example, CN=Ann Example"
    );
    assert_eq!(
        cell(s, 0, "Certificate issuer"),
        "C=LU, O=Example Trust Services, CN=Example Qualified CA"
    );
    assert_eq!(
        cell(s, 0, "Certificate valid"),
        "2026-01-01 00:00:00 UTC to 2028-12-31 23:59:59 UTC"
    );
    assert_eq!(cell(s, 0, "Method"), "RSA with SHA-256");
    assert_eq!(cell(s, 0, "References"), "2");
    let r = table(&i, "Signed references");
    assert_eq!(cell(r, 1, "Digest algorithm"), "SHA-512");
    assert_eq!(check(&i, Stage::Semantic, "mimetype"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "Every data file"), State::Passed);
    assert_eq!(
        check(&i, Stage::Signature, "Signatures were not verified"),
        State::NotDone
    );

    let i = open("unsigned-entry.asice");
    assert_eq!(check(&i, Stage::Semantic, "not covered"), State::Failed);
}

#[test]
fn pdf_embedded_factur_x_invoice() {
    let i = open("facturx.pdf");
    assert_eq!(i.family, "PDF");
    assert_eq!(i.kind, "PDF document with an embedded Factur-X invoice");
    assert_eq!(i.version.as_deref(), Some("PDF 1.7 (PDF/A-3b claimed)"));
    assert_eq!(fact(&i, "Pages"), "1");
    assert_eq!(fact(&i, "Author"), "Example Research S.A.");
    assert_eq!(fact(&i, "Producer"), "Example PDF Writer 2.0");
    assert_eq!(fact(&i, "Factur-X conformance level"), "EN 16931");
    assert_eq!(fact(&i, "Embedded files"), "2");
    let e = table(&i, "Embedded files");
    assert_eq!(cell(e, 0, "Name"), "factur-x.xml");
    assert_eq!(cell(e, 0, "MIME type"), "text/xml");
    assert_eq!(cell(e, 0, "Relationship"), "Data");
    assert_eq!(cell(e, 0, "Size (declared)"), cell(e, 0, "Size (decoded)"));
    assert_eq!(cell(e, 1, "Name"), "note.txt");
    assert_eq!(cell(e, 1, "Where"), "Attachment annotation, page 1");
    // The uncompressed attachment points at its bytes.
    let bytes = std::fs::read(fixture("facturx.pdf")).unwrap();
    let s = e.rows[1].source.clone().unwrap();
    assert_eq!(
        &bytes[s.start as usize..s.end as usize],
        b"Delivery note 17"
    );
    // The CII invoice.
    assert_eq!(fact(&i, "Invoice number"), "INV-2026-0042");
    assert_eq!(fact(&i, "Profile"), "EN 16931 (COMFORT)");
    assert_eq!(fact(&i, "Seller name"), "Example Research S.A.");
    assert_eq!(fact(&i, "Buyer name"), "Client & Partners GmbH");
    assert_eq!(fact(&i, "Grand total"), "2047.50");
    let lines = table(&i, "Invoice lines");
    assert_eq!(lines.rows.len(), 2);
    assert_eq!(cell(lines, 0, "Unit"), "DAY");
    assert_eq!(
        check(&i, Stage::Semantic, "sum of the invoice lines"),
        State::Passed
    );
    assert_eq!(check(&i, Stage::Semantic, "Grand total"), State::Passed);
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
}

#[test]
fn pdf_signatures_and_byte_range_coverage() {
    let i = open("signed.pdf");
    assert_eq!(i.kind, "Signed PDF document");
    let s = table(&i, "Signatures");
    assert_eq!(cell(s, 0, "Field"), "Signature1");
    assert_eq!(cell(s, 0, "SubFilter"), "ETSI.CAdES.detached");
    assert_eq!(cell(s, 0, "Type"), "PAdES (CAdES detached)");
    assert_eq!(cell(s, 0, "Signer name"), "Ann Example");
    assert_eq!(cell(s, 0, "Reason"), "Approval");
    assert_eq!(
        cell(s, 0, "Signing time (/M)"),
        "2026-11-01 10:00:00 +01:00"
    );
    assert_eq!(cell(s, 0, "Signing time (CMS)"), "2026-11-01 09:00:00 UTC");
    assert_eq!(
        cell(s, 0, "Certificate subject"),
        "C=LU, GN=Ann, SN=Example, CN=Ann Example"
    );
    assert_eq!(cell(s, 0, "Digest"), "SHA-256");
    assert!(cell(s, 0, "CMS size").ends_with("of 2048 reserved"));
    assert_eq!(check(&i, Stage::Semantic, "whole file"), State::Passed);
    assert_eq!(check(&i, Stage::Signature, "not verified"), State::NotDone);

    let dir = temp_dir("pdf");
    let bytes = std::fs::read(fixture("signed.pdf")).unwrap();
    // Bytes appended after signing: a later revision, not covered.
    let mut later = bytes.clone();
    later.extend_from_slice(b"\n% a later revision\n");
    let p = dir.join("later.pdf");
    std::fs::write(&p, &later).unwrap();
    let i = inspect(&p).unwrap().unwrap();
    assert_eq!(
        check(&i, Stage::Semantic, "added after this signature"),
        State::Partial
    );
    // A ByteRange whose hole is not the Contents string.
    let mut moved = bytes.clone();
    let at = moved
        .windows(12)
        .position(|w| w == b"/ByteRange [")
        .unwrap()
        + 12;
    let text = String::from_utf8_lossy(&moved[at..at + 40]).into_owned();
    let nums: Vec<u64> = text
        .split(']')
        .next()
        .unwrap()
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    let replaced = format!("0 {:010} {:010} {:010}", nums[1] - 5, nums[2], nums[3]);
    moved[at..at + replaced.len()].copy_from_slice(replaced.as_bytes());
    let p = dir.join("moved.pdf");
    std::fs::write(&p, &moved).unwrap();
    let i = inspect(&p).unwrap().unwrap();
    assert_eq!(
        check(&i, Stage::Semantic, "not its Contents"),
        State::Failed
    );
    let _ = std::fs::remove_dir_all(&dir);

    // Existing PDFs: an ordinary one, and a password-protected one.
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pdf");
    let i = inspect(&fixtures.join("libreoffice.pdf")).unwrap().unwrap();
    assert_eq!(i.kind, "PDF document");
    assert!(fact(&i, "Pages").parse::<u32>().unwrap() >= 1);
    assert_eq!(fact(&i, "Embedded files"), "0");
    let i = inspect(&fixtures.join("password.pdf")).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Parsed, "password"), State::Failed);
}

#[test]
fn fixed_width_records_need_their_layout() {
    let i = open("payments.dat");
    assert_eq!(i.family, "Fixed-width records");
    assert_eq!(i.kind, "Example payment file v2");
    assert_eq!(fact(&i, "Layout file"), "payments.dat.layout.toml");
    assert_eq!(fact(&i, "Record types"), "3");
    assert_eq!(fact(&i, "Payment"), "3");
    assert_eq!(fact(&i, "Unrecognized lines"), "1");
    let h = table(&i, "Header");
    assert_eq!(
        h.rows[0].cells,
        ["1", "H", "2026-11-01", "Example Research SA"]
    );
    let p = table(&i, "Payment");
    assert_eq!(
        p.rows[0].cells,
        ["2", "0012345678", "1250.50", "2026-11-02", "Café Müller"]
    );
    assert_eq!(p.rows[1].cells[2], "99.99");
    assert_eq!(table(&i, "Trailer").rows[0].cells, ["6", "3"]);
    // 1250.50 + 99.99 (the broken amount is not added).
    assert_eq!(fact(&i, "Payment — Amount"), "1350.49");
    let e = table(&i, "Conversion errors");
    assert_eq!(e.rows.len(), 2);
    assert_eq!(
        e.rows[0].cells[2..4],
        ["Amount".to_string(), "000012A456".to_string()]
    );
    assert_eq!(e.rows[1].cells[2], "Value date");
    assert_eq!(
        check(&i, Stage::Semantic, "could not be converted"),
        State::Failed
    );
    assert_eq!(
        check(&i, Stage::Parsed, "match no record type"),
        State::Partial
    );
    // Rows point at their lines.
    let bytes = std::fs::read(fixture("payments.dat")).unwrap();
    let s = p.rows[1].source.clone().unwrap();
    assert!(bytes[s.start as usize..s.end as usize].starts_with(b"D0098765432"));
    assert_eq!(i.outline.iter().filter(|o| o.depth == 0).count(), 3);

    // Without a layout the same file is plain text; a broken layout is reported.
    let dir = temp_dir("fixed");
    let data = dir.join("payments.dat");
    std::fs::copy(fixture("payments.dat"), &data).unwrap();
    assert!(inspect(&data).is_none());
    std::fs::write(
        dir.join("payments.layout.toml"),
        "[[field]]\nname = \"x\"\n",
    )
    .unwrap();
    let e = inspect(&data).unwrap().unwrap_err();
    assert!(
        e.contains("payments.layout.toml") && e.contains("start column"),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bordereaux_csv_xlsx_and_mapping_file() {
    let i = open("premium-bordereau.csv");
    assert_eq!(i.family, "Insurance bordereau");
    assert_eq!(i.kind, "Premium bordereau");
    assert_eq!(fact(&i, "Header row"), "2");
    assert_eq!(
        fact(&i, "Gross Premium"),
        "Gross premium (detected from the header)"
    );
    assert_eq!(
        fact(&i, "UMR"),
        "Unique market reference (UMR) (detected from the header)"
    );
    assert_eq!(fact(&i, "Rows"), "4");
    assert_eq!(fact(&i, "Gross premium total (EUR)"), "2230.50");
    assert_eq!(fact(&i, "Gross premium total (GBP)"), "500");
    assert_eq!(fact(&i, "Net premium total (EUR)"), "1784.40");
    assert_eq!(fact(&i, "Inception date"), "2026-11-01 to 2026-12-01");
    let p = table(&i, "Problems");
    assert_eq!(p.rows.len(), 2);
    assert_eq!(p.rows[0].cells[..3], ["6", "Inception Date", "32/13/2026"]);
    assert_eq!(p.rows[1].cells[..3], ["6", "Gross Premium", "n/a"]);
    assert_eq!(check(&i, Stage::Semantic, "not readable"), State::Failed);
    let rows = table(&i, "Rows");
    assert_eq!(cell(rows, 0, "Insured Name"), "Café Müller SARL");
    let bytes = std::fs::read(fixture("premium-bordereau.csv")).unwrap();
    let s = rows.rows[1].source.clone().unwrap();
    assert!(bytes[s.start as usize..].starts_with(b"B0999XYZ2026,POL-002"));

    let i = open("claims-bordereau.xlsx");
    assert_eq!(i.kind, "Claims bordereau");
    assert_eq!(fact(&i, "Sheet"), "Claims");
    assert_eq!(fact(&i, "Paid total (EUR)"), "1200.5");
    assert_eq!(fact(&i, "Outstanding reserve total (GBP)"), "0");
    assert_eq!(fact(&i, "Date of loss"), "2026-11-04 to 2026-11-14");
    assert_eq!(check(&i, Stage::Semantic, "could be read"), State::Passed);

    let i = open("mapped.csv");
    assert_eq!(
        fact(&i, "GWP Orig"),
        "Gross premium (from mapped.bordereau.toml)"
    );
    assert_eq!(fact(&i, "Gross premium total (EUR)"), "200.00");
    assert_eq!(fact(&i, "Inception date"), "2026-11-01 to 2026-11-02");

    // Ordinary tables are not bordereaux.
    for name in ["plain-sample.csv", "plain-sample.tsv", "plain-sample.txt"] {
        assert!(inspect(&fixture(name)).is_none(), "{name} was claimed");
    }
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    for name in [
        "sqlite-xlsx/libreoffice.xlsx",
        "sqlite-xlsx/openpyxl.xlsx",
        "office/libreoffice.docx",
        "archives/methods.zip",
    ] {
        assert!(
            inspect(&fixtures.join(name)).is_none(),
            "{name} was claimed"
        );
    }
}

/// Truncated and randomly damaged copies of every fixture: recognized or not, no panic, quick.
#[test]
fn damaged_documents_never_panic_and_finish_quickly() {
    let dir = temp_dir("damaged");
    let names = [
        "message.eml",
        "mailbox.mbox",
        "message.msg",
        "workbook.xls",
        "deck.pptx",
        "deck.odp",
        "deck.ppt",
        "container.asice",
        "facturx.pdf",
        "signed.pdf",
        "premium-bordereau.csv",
        "claims-bordereau.xlsx",
    ];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for name in names {
        let bytes = std::fs::read(fixture(name)).unwrap();
        let ext = name.rsplit('.').next().unwrap();
        let run = |b: &[u8], tag: &str| {
            let p = dir.join(format!("{tag}.{ext}"));
            std::fs::write(&p, b).unwrap();
            let started = std::time::Instant::now();
            // A handler panic becomes an "internal error"; none may happen.
            if let Some(Err(e)) = inspect(&p) {
                assert!(!e.contains("internal error"), "{name} ({tag}): {e}");
            }
            assert!(
                started.elapsed().as_secs() < 5,
                "{name} ({tag}) took {:?}",
                started.elapsed()
            );
        };
        for cut in [
            1,
            7,
            64,
            512,
            bytes.len() / 3,
            bytes.len() / 2,
            bytes.len() - 1,
        ] {
            run(&bytes[..cut.min(bytes.len())], &format!("cut{cut}"));
        }
        let rounds: usize = std::env::var("ISOLINEAR_FUZZ_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(40);
        for round in 0..rounds {
            let mut b = bytes.clone();
            for _ in 0..1 + round % 8 {
                let r = next();
                let at = (r % b.len() as u64) as usize;
                b[at] = (r >> 32) as u8;
            }
            run(&b, &format!("m{round}"));
        }
    }
    // The fixed-width layout and the bordereau mapping are damaged too.
    let data = dir.join("records.dat");
    std::fs::copy(fixture("payments.dat"), &data).unwrap();
    let layout = std::fs::read(fixture("payments.dat.layout.toml")).unwrap();
    for round in 0..40 {
        let mut b = layout.clone();
        let r = next();
        let at = (r % b.len() as u64) as usize;
        b[at] = (r >> 32) as u8;
        std::fs::write(dir.join("records.dat.layout.toml"), &b).unwrap();
        if let Some(Err(e)) = inspect(&data) {
            assert!(!e.contains("internal error"), "layout round {round}: {e}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
