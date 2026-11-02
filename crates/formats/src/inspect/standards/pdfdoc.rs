//! What a PDF holds besides its pages: document information and XMP metadata, embedded files
//! (the EmbeddedFiles name tree, associated files and FileAttachment annotations), a Factur-X /
//! ZUGFeRD / XRechnung invoice embedded as CII XML, and digital signatures (PAdES / PKCS#7):
//! signer, reason, time, and whether the signed byte ranges cover the whole file.
//!
//! Objects are read with `hayro`'s parser (cross-reference streams, object streams and the
//! standard encryption with an empty user password included). Embedded streams are decompressed
//! here, bounded. Nothing is rendered, run or verified: signatures are described, not checked.
use super::doc_common::{human, inflate, push_row, table};
use super::doc_der;
use crate::inspect::{
    Handler, Input, Inspection, Probe, Stage, State,
    spec::{self, F, T, XmlSpec, compare_total, decimal_sum, select, value_at},
    xmldoc::Doc,
};
use hayro::hayro_syntax::object::{Array, Dict, Name, Object, ObjectIdentifier, Stream};
use hayro::hayro_syntax::{Filter, LoadPdfError, Pdf};
use std::ops::Range;

/// Embedded file bytes decompressed at most per file, and in all.
const MAX_EMBEDDED: u64 = 64 * 1024 * 1024;
const MAX_EMBEDDED_TOTAL: u64 = 256 * 1024 * 1024;
/// Name-tree and field nesting, entries and fields visited at most.
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 100_000;
const MAX_ANNOTATIONS: usize = 100_000;
const INVOICE_NAMES: &[&str] = &[
    "factur-x.xml",
    "zugferd-invoice.xml",
    "xrechnung.xml",
    "order-x.xml",
];

pub struct PdfDocument;

impl Handler for PdfDocument {
    fn name(&self) -> &'static str {
        "PDF document"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        memchr::memmem::find(&probe.head[..probe.head.len().min(1024)], b"%PDF-").is_some()
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            inspect_pdf(input, cancelled)
        }));
        read.unwrap_or_else(|_| Err("The PDF reader failed on this file".into()))
    }
}

/// A PDF text string: UTF-16BE with its mark, UTF-8 with its mark, else PDFDocEncoding (read as
/// Latin-1).
pub fn text_string(b: &[u8]) -> String {
    let s = match b {
        [0xFE, 0xFF, rest @ ..] => {
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => b.iter().map(|&c| c as char).collect(),
    };
    s.trim_end_matches('\0').to_owned()
}

/// `D:20261103101500+01'00'` → `2026-11-03 10:15:00 +01:00`.
pub fn pdf_date(s: &str) -> String {
    let d = s.trim().trim_start_matches("D:");
    let digits: String = d.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 4 {
        return s.to_owned();
    }
    let part = |i: usize, default: &str| digits.get(i..i + 2).unwrap_or(default).to_owned();
    let zone = &d[digits.len()..];
    let zone = match zone.chars().next() {
        Some('Z') => " UTC".to_owned(),
        Some(c @ ('+' | '-')) => {
            let z: String = zone[1..].chars().filter(char::is_ascii_digit).collect();
            format!(
                " {c}{}:{}",
                z.get(..2).unwrap_or("00"),
                z.get(2..4).unwrap_or("00")
            )
        }
        _ => String::new(),
    };
    format!(
        "{}-{}-{} {}:{}:{}{zone}",
        &digits[..4],
        part(4, "01"),
        part(6, "01"),
        part(8, "00"),
        part(10, "00"),
        part(12, "00")
    )
}

fn dict_text(d: &Dict, key: &str) -> Option<String> {
    d.get::<hayro::hayro_syntax::object::String>(key)
        .map(|s| text_string(s.as_bytes()))
        .filter(|s| !s.is_empty())
}
fn dict_name(d: &Dict, key: &str) -> Option<String> {
    d.get::<Name>(key).map(|n| n.as_str().to_owned())
}

/// A stream's data with its filter undone (none or FlateDecode), at most `limit` bytes.
fn stream_bytes(s: &Stream, limit: u64) -> Result<Vec<u8>, String> {
    let filters = s.filters();
    let raw = s.raw_data();
    match filters.as_slice() {
        [] => {
            if raw.len() as u64 > limit {
                Err(format!("it is larger than {}", human(limit)))
            } else {
                Ok(raw.to_vec())
            }
        }
        [Filter::FlateDecode] => {
            if s.dict().contains_key("DecodeParms") || s.dict().contains_key("DP") {
                return Err("its compression uses a predictor, which is not extracted".into());
            }
            inflate(&raw, limit, false).or_else(|_| inflate(&raw, limit, true))
        }
        other => Err(format!(
            "it is encoded with {:?}, which Isolinear does not extract",
            other
        )),
    }
}

/// The file range of a stream's data when it is stored as is (no filter, not encrypted).
fn plain_span(pdf: &Pdf, s: &Stream) -> Option<Range<u64>> {
    if !s.filters().is_empty() {
        return None;
    }
    let raw = s.raw_data();
    let std::borrow::Cow::Borrowed(slice) = raw else {
        return None;
    };
    let base = pdf.data().as_ref().as_ptr() as usize;
    let start = (slice.as_ptr() as usize).checked_sub(base)?;
    (start + slice.len() <= pdf.data().as_ref().len())
        .then(|| start as u64..(start + slice.len()) as u64)
}

struct Embedded {
    name: String,
    description: String,
    declared: Option<i64>,
    decoded: Result<Vec<u8>, String>,
    mime: String,
    relationship: String,
    modified: String,
    place: String,
    span: Option<Range<u64>>,
}

struct Ctx<'a> {
    pdf: &'a Pdf,
    seen: Vec<ObjectIdentifier>,
    nodes: usize,
    budget: u64,
    files: Vec<Embedded>,
}

impl Ctx<'_> {
    fn first_visit(&mut self, d: &Dict) -> bool {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return false;
        }
        match d.obj_id() {
            Some(id) if self.seen.contains(&id) => false,
            Some(id) => {
                self.seen.push(id);
                true
            }
            None => true,
        }
    }

    fn file_spec(&mut self, spec: &Dict, key_name: Option<String>, place: &str) {
        if !self.first_visit(spec) {
            return;
        }
        let name = dict_text(spec, "UF")
            .or_else(|| dict_text(spec, "F"))
            .or(key_name)
            .unwrap_or_default();
        let ef = spec.get::<Dict>("EF");
        let stream = ef
            .as_ref()
            .and_then(|e| e.get::<Stream>("F").or_else(|| e.get::<Stream>("UF")));
        let (declared, mime, modified, decoded, span) = match &stream {
            Some(s) => {
                let params = s.dict().get::<Dict>("Params");
                let declared = params.as_ref().and_then(|p| p.get::<i64>("Size"));
                let modified = params
                    .as_ref()
                    .and_then(|p| dict_text(p, "ModDate"))
                    .map(|d| pdf_date(&d))
                    .unwrap_or_default();
                let limit = MAX_EMBEDDED.min(self.budget);
                let decoded = stream_bytes(s, limit);
                if let Ok(b) = &decoded {
                    self.budget = self.budget.saturating_sub(b.len() as u64);
                }
                (
                    declared,
                    dict_name(s.dict(), "Subtype").unwrap_or_default(),
                    modified,
                    decoded,
                    plain_span(self.pdf, s),
                )
            }
            None => (
                None,
                String::new(),
                String::new(),
                Err(
                    "the file specification has no embedded data (external file, never opened)"
                        .into(),
                ),
                None,
            ),
        };
        self.files.push(Embedded {
            name,
            description: dict_text(spec, "Desc").unwrap_or_default(),
            declared,
            decoded,
            mime,
            relationship: dict_name(spec, "AFRelationship").unwrap_or_default(),
            modified,
            place: place.to_owned(),
            span,
        });
    }

    fn name_tree(&mut self, node: &Dict, depth: usize) {
        if depth > MAX_DEPTH || !self.first_visit(node) {
            return;
        }
        if let Some(names) = node.get::<Array>("Names") {
            let mut key: Option<String> = None;
            for item in names.iter::<Object>().take(MAX_NODES) {
                match item {
                    Object::String(s) => key = Some(text_string(s.as_bytes())),
                    Object::Dict(d) => {
                        self.file_spec(&d, key.take(), "EmbeddedFiles");
                    }
                    _ => key = None,
                }
            }
        }
        if let Some(kids) = node.get::<Array>("Kids") {
            for kid in kids.iter::<Object>().take(MAX_NODES) {
                if let Object::Dict(d) = kid {
                    self.name_tree(&d, depth + 1);
                }
            }
        }
    }
}

struct Signature {
    field: String,
    kind: String,
    filter: String,
    subfilter: String,
    name: String,
    reason: String,
    location: String,
    time: String,
    byte_range: Vec<i64>,
    contents: Vec<u8>,
}

fn signature_fields(
    field: &Dict,
    parent: &str,
    ft: Option<String>,
    depth: usize,
    seen: &mut Vec<ObjectIdentifier>,
    out: &mut Vec<Signature>,
) {
    if depth > MAX_DEPTH || out.len() > 10_000 || seen.len() > MAX_NODES {
        return;
    }
    if let Some(id) = field.obj_id() {
        if seen.contains(&id) {
            return;
        }
        seen.push(id);
    }
    let partial = dict_text(field, "T").unwrap_or_default();
    let full = match (parent.is_empty(), partial.is_empty()) {
        (true, _) => partial.clone(),
        (false, true) => parent.to_owned(),
        (false, false) => format!("{parent}.{partial}"),
    };
    let ft = dict_name(field, "FT").or(ft);
    if ft.as_deref() == Some("Sig")
        && let Some(v) = field.get::<Dict>("V")
    {
        out.push(signature(&v, &full));
    }
    if let Some(kids) = field.get::<Array>("Kids") {
        for kid in kids.iter::<Object>().take(MAX_NODES) {
            if let Object::Dict(d) = kid {
                signature_fields(&d, &full, ft.clone(), depth + 1, seen, out);
            }
        }
    }
}

fn signature(v: &Dict, field: &str) -> Signature {
    Signature {
        field: field.to_owned(),
        kind: dict_name(v, "Type").unwrap_or_else(|| "Sig".into()),
        filter: dict_name(v, "Filter").unwrap_or_default(),
        subfilter: dict_name(v, "SubFilter").unwrap_or_default(),
        name: dict_text(v, "Name").unwrap_or_default(),
        reason: dict_text(v, "Reason").unwrap_or_default(),
        location: dict_text(v, "Location").unwrap_or_default(),
        time: dict_text(v, "M").map(|m| pdf_date(&m)).unwrap_or_default(),
        byte_range: v
            .get::<Array>("ByteRange")
            .map(|a| a.iter::<i64>().take(16).collect())
            .unwrap_or_default(),
        contents: v
            .get::<hayro::hayro_syntax::object::String>("Contents")
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default(),
    }
}

/// Whether a signature's ByteRange covers the file except its Contents string.
pub fn coverage(br: &[i64], data: &[u8], size: u64, complete: bool) -> (State, String) {
    let [a, b, c, d] = match br {
        [a, b, c, d] if *a >= 0 && *b >= 0 && *c >= 0 && *d >= 0 => {
            [*a as u64, *b as u64, *c as u64, *d as u64]
        }
        [] => return (State::Failed, "The signature has no ByteRange".into()),
        _ => {
            return (
                State::Failed,
                format!("The ByteRange {br:?} is not four non-negative numbers"),
            );
        }
    };
    if a != 0 {
        return (
            State::Failed,
            format!(
                "The signed bytes do not start at the beginning of the file (ByteRange {br:?})"
            ),
        );
    }
    if b > c || c.checked_add(d).is_none_or(|end| end > size) {
        return (
            State::Failed,
            format!(
                "The ByteRange {br:?} overlaps or runs past the end of the file ({size} bytes)"
            ),
        );
    }
    if !complete {
        return (
            State::NotDone,
            "Only part of the file was read; the coverage was not checked".into(),
        );
    }
    let hole = &data[b as usize..c as usize];
    let hex_hole = hole.len() >= 2
        && hole[0] == b'<'
        && hole[hole.len() - 1] == b'>'
        && hole[1..hole.len() - 1]
            .iter()
            .all(|x| x.is_ascii_hexdigit() || x.is_ascii_whitespace());
    if !hex_hole {
        return (
            State::Failed,
            format!(
                "The bytes left out of the signature ({b}–{c}) are not its Contents value: something else is unsigned"
            ),
        );
    }
    let end = c + d;
    if end == size {
        (
            State::Passed,
            format!(
                "The signed bytes cover the whole file except the signature value (bytes {b}–{c})"
            ),
        )
    } else {
        (
            State::Partial,
            format!(
                "The signed bytes end at {end} of {size}: {} bytes were added after this signature (a later revision)",
                size - end
            ),
        )
    }
}

/// `/ByteRange [a b c d]` arrays found in the raw bytes (for files the object reader cannot open).
fn raw_byte_ranges(bytes: &[u8]) -> Vec<Vec<i64>> {
    memchr::memmem::find_iter(bytes, b"/ByteRange")
        .take(1000)
        .filter_map(|at| {
            let rest = &bytes[at + 10..bytes.len().min(at + 200)];
            let open = memchr::memchr(b'[', rest)?;
            let close = memchr::memchr(b']', rest)?;
            (open < close).then(|| {
                String::from_utf8_lossy(&rest[open + 1..close])
                    .split_whitespace()
                    .filter_map(|n| n.parse().ok())
                    .collect()
            })
        })
        .collect()
}

fn version_text(v: hayro::hayro_syntax::PdfVersion) -> String {
    let d = format!("{v:?}");
    let digits: String = d.chars().filter(char::is_ascii_digit).collect();
    match digits.len() {
        2 => format!("{}.{}", &digits[..1], &digits[1..]),
        _ => d,
    }
}

fn inspect_pdf(input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let mut out = Inspection::new("PDF", "PDF document");
    let pdf = match Pdf::new(input.bytes.to_vec()) {
        Ok(p) => p,
        Err(LoadPdfError::Decryption(_)) => {
            out.kind = "Encrypted PDF document".into();
            out.check(
                Stage::Parsed,
                State::Failed,
                "The PDF is encrypted with a password: its objects cannot be read",
            );
            let ranges = raw_byte_ranges(input.bytes);
            if !ranges.is_empty() {
                out.fact(
                    "Signatures",
                    "Signatures (ByteRange entries)",
                    ranges.len().to_string(),
                    None,
                );
                for br in &ranges {
                    let (state, detail) = coverage(br, input.bytes, input.size, input.complete);
                    out.check(Stage::Semantic, state, detail);
                }
                out.check(
                    Stage::Signature,
                    State::NotDone,
                    "Signatures were not verified",
                );
            }
            out.notes.push("Password-protected PDFs are not decrypted; Isolinear never asks for or tries passwords".into());
            out.not_validated();
            return Ok(out);
        }
        Err(LoadPdfError::Invalid) => return Err("The PDF's structure could not be read".into()),
    };
    let version = version_text(pdf.version());
    out.version = Some(format!("PDF {version}"));
    out.fact("Document", "PDF version", version, None);
    out.fact("Document", "Pages", pdf.pages().len().to_string(), None);
    let meta = pdf.metadata();
    let info: [(&str, &Option<Vec<u8>>); 6] = [
        ("Title", &meta.title),
        ("Author", &meta.author),
        ("Subject", &meta.subject),
        ("Keywords", &meta.keywords),
        ("Creator", &meta.creator),
        ("Producer", &meta.producer),
    ];
    for (label, v) in info {
        if let Some(b) = v {
            out.fact("Document information", label, text_string(b), None);
        }
    }
    let date = |d: &Option<hayro::hayro_syntax::object::DateTime>| {
        d.map(|d| {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}{:02}:{:02}",
                d.year,
                d.month,
                d.day,
                d.hour,
                d.minute,
                d.second,
                if d.utc_offset_hour < 0 { '-' } else { '+' },
                d.utc_offset_hour.unsigned_abs(),
                d.utc_offset_minute
            )
        })
    };
    if let Some(d) = date(&meta.creation_date) {
        out.fact("Document information", "Created", d, None);
    }
    if let Some(d) = date(&meta.modification_date) {
        out.fact("Document information", "Modified", d, None);
    }
    let encrypted = memchr::memmem::find_iter(input.bytes, b"/Encrypt")
        .any(|i| !input.bytes.get(i + 8).is_some_and(u8::is_ascii_alphabetic));
    if encrypted {
        out.notes.push(
            "The PDF is encrypted with an empty user password; it was read as a viewer would"
                .into(),
        );
    }
    let root = pdf
        .xref()
        .get::<Dict>(pdf.xref().root_id())
        .ok_or("The PDF has no document catalog")?;

    // XMP metadata.
    let mut pdfa = None;
    if let Some(m) = root.get::<Stream>("Metadata") {
        match stream_bytes(&m, 16 * 1024 * 1024) {
            Ok(xmp) => {
                let text = String::from_utf8_lossy(&xmp).into_owned();
                if let Ok(doc) = Doc::parse(&text, cancelled) {
                    let span = plain_span(&pdf, &m);
                    for (label, path) in [
                        ("Title", "**/title/Alt/li | **/title"),
                        ("Author", "**/creator/Seq/li | **/creator"),
                        ("Created", "**/CreateDate | **/Description/@CreateDate"),
                        ("Modified", "**/ModifyDate | **/Description/@ModifyDate"),
                        ("Producer", "**/Producer | **/Description/@Producer"),
                        (
                            "Creator tool",
                            "**/CreatorTool | **/Description/@CreatorTool",
                        ),
                        (
                            "Factur-X conformance level",
                            "**/ConformanceLevel | **/Description/@ConformanceLevel",
                        ),
                        (
                            "Factur-X document",
                            "**/DocumentFileName | **/Description/@DocumentFileName",
                        ),
                    ] {
                        if let Some((v, s)) = value_at(&doc, 0, path) {
                            let src = span
                                .clone()
                                .map(|base| base.start + s.start..base.start + s.end);
                            out.fact("XMP metadata", label, v, src);
                        }
                    }
                    let part = value_at(&doc, 0, "**/part | **/Description/@part").map(|v| v.0);
                    let conf = value_at(&doc, 0, "**/conformance | **/Description/@conformance")
                        .map(|v| v.0);
                    if let Some(p) = part {
                        let level =
                            format!("PDF/A-{p}{}", conf.unwrap_or_default().to_ascii_lowercase());
                        out.fact("Document", "PDF/A claim", level.clone(), span);
                        pdfa = Some(level);
                    }
                }
            }
            Err(e) => out
                .notes
                .push(format!("The XMP metadata could not be read: {e}")),
        }
    }
    if let Some(level) = &pdfa {
        out.version = Some(format!(
            "PDF {} ({level} claimed)",
            out.version
                .clone()
                .unwrap_or_default()
                .trim_start_matches("PDF ")
        ));
    }

    // Embedded files.
    let mut ctx = Ctx {
        pdf: &pdf,
        seen: vec![],
        nodes: 0,
        budget: MAX_EMBEDDED_TOTAL,
        files: vec![],
    };
    if let Some(tree) = root
        .get::<Dict>("Names")
        .and_then(|n| n.get::<Dict>("EmbeddedFiles"))
    {
        ctx.name_tree(&tree, 0);
    }
    if let Some(af) = root.get::<Array>("AF") {
        for item in af.iter::<Object>().take(10_000) {
            if let Object::Dict(d) = item {
                ctx.file_spec(&d, None, "Associated file (document)");
            }
        }
    }
    let mut annotations = 0usize;
    for (n, page) in pdf.pages().iter().enumerate() {
        if cancelled() {
            return Err("Cancelled".into());
        }
        let Some(annots) = page.raw().get::<Array>("Annots") else {
            continue;
        };
        for a in annots.iter::<Object>() {
            annotations += 1;
            if annotations > MAX_ANNOTATIONS {
                break;
            }
            if let Object::Dict(d) = a
                && dict_name(&d, "Subtype").as_deref() == Some("FileAttachment")
                && let Some(fs) = d.get::<Dict>("FS")
            {
                ctx.file_spec(&fs, None, &format!("Attachment annotation, page {}", n + 1));
            }
        }
    }
    let files = std::mem::take(&mut ctx.files);
    out.fact(
        "Embedded files",
        "Embedded files",
        files.len().to_string(),
        None,
    );
    if !files.is_empty() {
        let mut t = table(
            "Embedded files",
            &[
                "Name",
                "Description",
                "MIME type",
                "Size (declared)",
                "Size (decoded)",
                "Relationship",
                "Modified",
                "Where",
            ],
        );
        for f in &files {
            push_row(
                &mut t,
                vec![
                    f.name.clone(),
                    f.description.clone(),
                    f.mime.clone(),
                    f.declared.map(|s| s.to_string()).unwrap_or_default(),
                    match &f.decoded {
                        Ok(b) => b.len().to_string(),
                        Err(e) => format!("not extracted: {e}"),
                    },
                    f.relationship.clone(),
                    f.modified.clone(),
                    f.place.clone(),
                ],
                f.span.clone(),
            );
        }
        out.tables.push(t);
    }

    // Factur-X / ZUGFeRD / XRechnung.
    let invoice = files.iter().find(|f| {
        let n = f.name.to_ascii_lowercase();
        INVOICE_NAMES.contains(&n.as_str())
    });
    if let Some(f) = invoice {
        match &f.decoded {
            Ok(xml) => {
                let text = String::from_utf8_lossy(xml).into_owned();
                match Doc::parse(&text, cancelled) {
                    Ok(doc) if doc.root().name == "CrossIndustryInvoice" => {
                        let sub = spec::run_xml(&CII, &doc);
                        out.kind = format!(
                            "PDF document with an embedded {} invoice",
                            invoice_family(&f.name)
                        );
                        let base = f.span.clone().map(|s| s.start);
                        let shift = |r: Option<Range<u64>>| match (r, base) {
                            (Some(r), Some(b)) => Some(b + r.start..b + r.end),
                            _ => None,
                        };
                        for fact in sub.facts {
                            out.fact(
                                &format!("Invoice: {}", fact.group),
                                &fact.label,
                                fact.value,
                                shift(fact.source),
                            );
                        }
                        for mut t in sub.tables {
                            for r in &mut t.rows {
                                r.source = shift(r.source.take());
                            }
                            out.tables.push(t);
                        }
                        for c in sub
                            .checks
                            .into_iter()
                            .filter(|c| c.stage == Stage::Semantic)
                        {
                            out.check(c.stage, c.state, format!("Invoice: {}", c.detail));
                        }
                        out.notes.push(format!(
                            "The invoice data is the embedded {} (CII XML); it was not validated against the Factur-X/EN 16931 schemas or Schematron rules",
                            f.name
                        ));
                    }
                    Ok(doc) => out.notes.push(format!(
                        "{} is XML with root <{}>, not a CII invoice",
                        f.name,
                        doc.root().name
                    )),
                    Err(e) => out
                        .notes
                        .push(format!("{} could not be read as XML: {e}", f.name)),
                }
            }
            Err(e) => out
                .notes
                .push(format!("{} could not be extracted: {e}", f.name)),
        }
    }

    // Signatures.
    let mut sigs = vec![];
    if let Some(fields) = root
        .get::<Dict>("AcroForm")
        .and_then(|a| a.get::<Array>("Fields"))
    {
        let mut seen = vec![];
        for f in fields.iter::<Object>().take(MAX_NODES) {
            if let Object::Dict(d) = f {
                signature_fields(&d, "", None, 0, &mut seen, &mut sigs);
            }
        }
    }
    if sigs.is_empty() && !raw_byte_ranges(input.bytes).is_empty() {
        out.notes.push(
            "The file has signature byte ranges that are not reachable from its form fields".into(),
        );
    }
    if !sigs.is_empty() {
        out.kind = format!("Signed {}", out.kind);
        let mut t = table(
            "Signatures",
            &[
                "Field",
                "Type",
                "SubFilter",
                "Signer name",
                "Certificate subject",
                "Certificate issuer",
                "Reason",
                "Location",
                "Signing time (/M)",
                "Signing time (CMS)",
                "Digest",
                "ByteRange",
                "Coverage",
                "CMS size",
            ],
        );
        for s in &sigs {
            let (state, detail) = coverage(&s.byte_range, input.bytes, input.size, input.complete);
            let cms = doc_der::cms(&s.contents);
            let used = doc_der::read(&s.contents).map_or(0, |(t, _)| t.raw.len());
            let profile = match s.subfilter.as_str() {
                "ETSI.CAdES.detached" => "PAdES (CAdES detached)",
                "ETSI.RFC3161" => "document timestamp (RFC 3161)",
                "adbe.pkcs7.detached" => "PKCS#7 detached",
                "adbe.pkcs7.sha1" => "PKCS#7 with SHA-1",
                "adbe.x509.rsa_sha1" => "X.509 RSA-SHA1",
                _ => "",
            };
            let signer = cms.as_ref().and_then(|c| c.signer.clone());
            push_row(
                &mut t,
                vec![
                    s.field.clone(),
                    if profile.is_empty() {
                        s.kind.clone()
                    } else {
                        profile.into()
                    },
                    s.subfilter.clone(),
                    s.name.clone(),
                    signer
                        .as_ref()
                        .map(|c| c.subject.clone())
                        .unwrap_or_default(),
                    signer
                        .as_ref()
                        .map(|c| c.issuer.clone())
                        .unwrap_or_default(),
                    s.reason.clone(),
                    s.location.clone(),
                    s.time.clone(),
                    cms.as_ref()
                        .and_then(|c| c.signing_time.clone())
                        .unwrap_or_default(),
                    cms.as_ref()
                        .and_then(|c| c.digest.clone())
                        .unwrap_or_default(),
                    format!("{:?}", s.byte_range),
                    detail.clone(),
                    if s.contents.is_empty() {
                        String::new()
                    } else {
                        format!("{used} bytes used of {} reserved", s.contents.len())
                    },
                ],
                s.byte_range
                    .get(1..3)
                    .filter(|r| r[0] >= 0 && r[1] >= r[0] && (r[1] as u64) <= input.size)
                    .map(|r| r[0] as u64..r[1] as u64),
            );
            out.check(
                Stage::Semantic,
                state,
                format!("Signature “{}”: {detail}", s.field),
            );
            if s.filter.is_empty() && s.subfilter.is_empty() {
                out.notes.push(format!(
                    "Signature “{}” names no handler (/Filter)",
                    s.field
                ));
            }
            if cms.as_ref().is_some_and(|c| c.timestamp) {
                out.notes.push(format!(
                    "Signature “{}” carries a signature timestamp (not verified)",
                    s.field
                ));
            }
        }
        out.fact("Signatures", "Signatures", sigs.len().to_string(), None);
        if let Some(first) = t.rows.first() {
            out.fact(
                "Signatures",
                "First signer name",
                if first.cells[3].is_empty() {
                    first.cells[4].clone()
                } else {
                    first.cells[3].clone()
                },
                first.source.clone(),
            );
            out.fact(
                "Signatures",
                "First signing time",
                first.cells[8].clone(),
                first.source.clone(),
            );
        }
        out.tables.push(t);
        if let Some(dss) = root.get::<Dict>("DSS") {
            let count = |k: &str| {
                dss.get::<Array>(k)
                    .map_or(0, |a| a.iter::<Object>().take(100_000).count())
            };
            out.fact(
                "Signatures",
                "Validation data (DSS)",
                format!(
                    "{} certificates, {} OCSP responses, {} CRLs",
                    count("Certs"),
                    count("OCSPs"),
                    count("CRLs")
                ),
                None,
            );
        }
        if root
            .get::<Dict>("Perms")
            .is_some_and(|p| p.contains_key("DocMDP"))
        {
            out.fact(
                "Signatures",
                "Certification",
                "The document is certified (DocMDP): later changes are restricted",
                None,
            );
        }
        out.check(
            Stage::Signature,
            State::NotDone,
            "Signatures were not verified (the CMS signature, the certificate chain and revocation are not checked)",
        );
    }
    out.check(
        Stage::Parsed,
        State::Passed,
        "Document catalog, metadata, embedded files and signature fields read",
    );
    if out.checks.iter().all(|c| c.stage != Stage::Schema) {
        out.check(
            Stage::Schema,
            State::NotDone,
            "Not checked against the PDF or PDF/A specification",
        );
    }
    out.notes.push(
        "Pages are shown in the Pages view; links, forms, JavaScript and attachments never act"
            .into(),
    );
    Ok(out)
}

fn invoice_family(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "factur-x.xml" => "Factur-X",
        "xrechnung.xml" => "XRechnung",
        "order-x.xml" => "Order-X",
        _ => "ZUGFeRD",
    }
}

/// The profile named by a CII guideline identifier.
pub fn cii_profile(id: &str) -> &'static str {
    let l = id.to_ascii_lowercase();
    if l.contains("xrechnung") {
        "XRechnung"
    } else if l.contains("extended") {
        "EXTENDED"
    } else if l.contains("basicwl") {
        "BASIC WL"
    } else if l.contains("basic") {
        "BASIC"
    } else if l.contains("minimum") {
        "MINIMUM"
    } else if l.contains("en16931") {
        "EN 16931 (COMFORT)"
    } else {
        "unknown profile"
    }
}

/// UN/CEFACT Cross Industry Invoice (the XML of Factur-X, ZUGFeRD 2 and XRechnung CII).
pub static CII: XmlSpec = XmlSpec {
    family: "UN/CEFACT CII",
    kind: "Cross Industry Invoice",
    matches: |root, ns| root == "CrossIndustryInvoice" && ns.contains("CrossIndustryInvoice"),
    version: |doc| {
        value_at(
            doc,
            0,
            "ExchangedDocumentContext/GuidelineSpecifiedDocumentContextParameter/ID",
        )
        .map(|(v, _)| cii_profile(&v).to_owned())
    },
    facts: &[
        F(
            "Invoice",
            "Profile (guideline)",
            "ExchangedDocumentContext/GuidelineSpecifiedDocumentContextParameter/ID",
        ),
        F("Invoice", "Invoice number", "ExchangedDocument/ID"),
        F("Invoice", "Type code", "ExchangedDocument/TypeCode"),
        F(
            "Invoice",
            "Issue date",
            "ExchangedDocument/IssueDateTime/DateTimeString",
        ),
        F(
            "Invoice",
            "Currency",
            "**/ApplicableHeaderTradeSettlement/InvoiceCurrencyCode",
        ),
        F(
            "Invoice",
            "Buyer reference",
            "**/ApplicableHeaderTradeAgreement/BuyerReference",
        ),
        F(
            "Invoice",
            "Order reference",
            "**/BuyerOrderReferencedDocument/IssuerAssignedID",
        ),
        F("Seller", "Seller name", "**/SellerTradeParty/Name"),
        F(
            "Seller",
            "Seller VAT ID",
            "**/SellerTradeParty/SpecifiedTaxRegistration/ID",
        ),
        F(
            "Seller",
            "Seller address (city)",
            "**/SellerTradeParty/PostalTradeAddress/CityName",
        ),
        F("Buyer", "Buyer name", "**/BuyerTradeParty/Name"),
        F(
            "Buyer",
            "Buyer VAT ID",
            "**/BuyerTradeParty/SpecifiedTaxRegistration/ID",
        ),
        F(
            "Payment",
            "Payee account (IBAN)",
            "**/PayeePartyCreditorFinancialAccount/IBANID",
        ),
        F(
            "Payment",
            "Due date",
            "**/SpecifiedTradePaymentTerms/DueDateDateTime/DateTimeString",
        ),
        F(
            "Totals",
            "Line total",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/LineTotalAmount",
        ),
        F(
            "Totals",
            "Tax basis total",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/TaxBasisTotalAmount",
        ),
        F(
            "Totals",
            "Tax total",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/TaxTotalAmount",
        ),
        F(
            "Totals",
            "Grand total",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/GrandTotalAmount",
        ),
        F(
            "Totals",
            "Due payable",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/DuePayableAmount",
        ),
    ],
    tables: &[
        T {
            name: "Invoice lines",
            rows: "**/IncludedSupplyChainTradeLineItem",
            columns: &[
                ("Line", "AssociatedDocumentLineDocument/LineID"),
                ("Product", "SpecifiedTradeProduct/Name"),
                ("Quantity", "SpecifiedLineTradeDelivery/BilledQuantity"),
                (
                    "Unit",
                    "SpecifiedLineTradeDelivery/BilledQuantity/@unitCode",
                ),
                (
                    "Net price",
                    "SpecifiedLineTradeAgreement/NetPriceProductTradePrice/ChargeAmount",
                ),
                (
                    "VAT %",
                    "SpecifiedLineTradeSettlement/ApplicableTradeTax/RateApplicablePercent",
                ),
                (
                    "Line total",
                    "SpecifiedLineTradeSettlement/SpecifiedTradeSettlementLineMonetarySummation/LineTotalAmount",
                ),
            ],
        },
        T {
            name: "VAT breakdown",
            rows: "**/ApplicableHeaderTradeSettlement/ApplicableTradeTax",
            columns: &[
                ("Category", "CategoryCode"),
                ("Rate %", "RateApplicablePercent"),
                ("Basis", "BasisAmount"),
                ("Tax", "CalculatedAmount"),
            ],
        },
    ],
    custom: Some(|doc, out| {
        if let Some((id, span)) = value_at(
            doc,
            0,
            "ExchangedDocumentContext/GuidelineSpecifiedDocumentContextParameter/ID",
        ) {
            out.fact("Invoice", "Profile", cii_profile(&id), Some(span));
        }
        let sums = "**/SpecifiedTradeSettlementHeaderMonetarySummation";
        let lines: Vec<String> = select(doc, 0, "**/IncludedSupplyChainTradeLineItem")
            .iter()
            .filter_map(|&l| value_at(doc, l, "SpecifiedLineTradeSettlement/SpecifiedTradeSettlementLineMonetarySummation/LineTotalAmount").map(|v| v.0))
            .collect();
        if !lines.is_empty() {
            compare_total(
                out,
                "Line total = sum of the invoice lines",
                value_at(doc, 0, &format!("{sums}/LineTotalAmount")).map(|v| v.0),
                decimal_sum(lines.iter().map(String::as_str)),
            );
        }
        let basis = value_at(doc, 0, &format!("{sums}/TaxBasisTotalAmount")).map(|v| v.0);
        let tax = value_at(doc, 0, &format!("{sums}/TaxTotalAmount")).map(|v| v.0);
        if let (Some(b), Some(t)) = (basis, tax) {
            compare_total(
                out,
                "Grand total = tax basis total + tax total",
                value_at(doc, 0, &format!("{sums}/GrandTotalAmount")).map(|v| v.0),
                decimal_sum([b.as_str(), t.as_str()].into_iter()),
            );
        }
    }),
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strings_dates_and_coverage() {
        assert_eq!(text_string(b"\xFE\xFF\x00A\x00\xE9"), "Aé");
        assert_eq!(text_string(b"caf\xE9"), "café");
        assert_eq!(
            pdf_date("D:20261103101500+01'00'"),
            "2026-11-03 10:15:00 +01:00"
        );
        assert_eq!(pdf_date("D:2026"), "2026-01-01 00:00:00");
        let data = b"0123<abcd>6789";
        let (s, _) = coverage(&[0, 4, 10, 4], data, 14, true);
        assert_eq!(s, State::Passed);
        let (s, _) = coverage(&[0, 4, 10, 2], data, 14, true);
        assert_eq!(s, State::Partial);
        let (s, _) = coverage(&[0, 3, 10, 4], data, 14, true);
        assert_eq!(s, State::Failed);
        let (s, _) = coverage(&[0, 4, 10, 40], data, 14, true);
        assert_eq!(s, State::Failed);
        let (s, _) = coverage(&[0, 4, 10], data, 14, true);
        assert_eq!(s, State::Failed);
        assert_eq!(
            raw_byte_ranges(b"/ByteRange [0 4 10 4]/x"),
            [vec![0, 4, 10, 4]]
        );
        assert_eq!(
            cii_profile("urn:cen.eu:en16931:2017#conformant#urn:factur-x.eu:1p0:basic"),
            "BASIC"
        );
        assert_eq!(cii_profile("urn:cen.eu:en16931:2017"), "EN 16931 (COMFORT)");
    }
}
