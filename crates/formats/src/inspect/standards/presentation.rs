//! Presentations: PowerPoint OOXML (`.pptx`), OpenDocument (`.odp`) and the binary PowerPoint
//! 97–2003 format (`.ppt`). Slides in presentation order with their titles and text, speaker
//! notes, the pictures each slide uses (the first ones shown) and the document properties.
//! Layout is not reproduced: this is the text of the slides, not their rendering.
use super::cfb::{self, Cfb};
use super::doc_common::{self, Zip, clip, human, is_picture, push_row, table, u16le, u32le};
use crate::document::{self as office, Ev, Ns};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State, Visual};

const MAX_SLIDES: usize = 10_000;
const MAX_PICTURES: usize = 10;
const MAX_PICTURE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TEXT_ROWS: usize = 200_000;

#[derive(Default)]
struct Slide {
    title: String,
    paragraphs: Vec<(String, String)>,
    notes: Vec<String>,
    images: Vec<String>,
    source: Option<std::ops::Range<u64>>,
}

pub struct Presentation;

fn is_pptx(probe: &Probe) -> bool {
    probe.head.starts_with(b"PK\x03\x04")
        && (doc_common::head_entries(probe.head)
            .iter()
            .any(|(n, _, _)| n.eq_ignore_ascii_case("ppt/presentation.xml"))
            || probe.ext_is(&["pptx", "pptm", "ppsx", "ppsm", "potx", "potm"]))
}
fn is_odp(probe: &Probe) -> bool {
    probe.head.starts_with(b"PK\x03\x04")
        && probe.head.get(30..38) == Some(b"mimetype")
        && memchr::memmem::find(
            &probe.head[..probe.head.len().min(200)],
            b"application/vnd.oasis.opendocument.presentation",
        )
        .is_some()
}

impl Handler for Presentation {
    fn name(&self) -> &'static str {
        "Presentation"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        is_pptx(probe)
            || is_odp(probe)
            || cfb::head_has_name(probe.head, "PowerPoint Document")
                && !cfb::head_has_name(probe.head, "WordDocument")
                && !cfb::head_has_name(probe.head, "__substg1.0_")
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        if input.bytes.starts_with(cfb::MAGIC) {
            return ppt(input.bytes, cancelled);
        }
        let mut zip = Zip::open(input.bytes, cancelled)?;
        if zip.has("ppt/presentation.xml") || zip.has("[Content_Types].xml") {
            pptx(&mut zip, cancelled)
        } else if zip.has("content.xml") {
            odp(&mut zip, cancelled)
        } else {
            Err("Not a presentation: no ppt/presentation.xml or content.xml".into())
        }
    }
}

/// Text of every paragraph of a DrawingML part, by shape: (placeholder type, paragraphs).
fn drawing_text(
    xml: &[u8],
    cancelled: &dyn Fn() -> bool,
) -> (Vec<(String, Vec<String>)>, Vec<String>) {
    let mut shapes: Vec<(String, Vec<String>)> = vec![];
    let mut images = vec![];
    let mut stack: Vec<(String, Vec<String>)> = vec![];
    let mut para: Option<String> = None;
    let mut in_t = false;
    let _ = office::walk(xml, cancelled, |ev| {
        match ev {
            Ev::Open(tag) => match tag.name.as_str() {
                "sp" | "graphicFrame" | "cxnSp" if tag.ns == Ns::Other => {
                    stack.push((String::new(), vec![]))
                }
                "ph" if tag.ns == Ns::Other => {
                    if let Some(s) = stack.last_mut() {
                        s.0 = tag.attr(Ns::Other, "type").unwrap_or("body").to_owned();
                    }
                }
                "p" if tag.ns == Ns::Drawing => para = Some(String::new()),
                "t" if tag.ns == Ns::Drawing => in_t = true,
                "br" if tag.ns == Ns::Drawing => {
                    if let Some(p) = para.as_mut() {
                        p.push(' ');
                    }
                }
                "blip" => {
                    if let Some(id) = tag.attr(Ns::R, "embed")
                        && images.len() < 1000
                    {
                        images.push(id.to_owned());
                    }
                }
                _ => {}
            },
            Ev::Text(t) if in_t => {
                if let Some(p) = para.as_mut()
                    && p.len() < 64 * 1024
                {
                    p.push_str(t);
                }
            }
            Ev::Close(Ns::Drawing, "t") => in_t = false,
            Ev::Close(Ns::Drawing, "p") => {
                if let Some(p) = para.take() {
                    let p = p.trim().to_owned();
                    if !p.is_empty() {
                        match stack.last_mut() {
                            Some(s) => s.1.push(p),
                            None => shapes.push((String::new(), vec![p])),
                        }
                    }
                }
            }
            Ev::Close(Ns::Other, "sp" | "graphicFrame" | "cxnSp") => {
                if let Some(s) = stack.pop()
                    && !s.1.is_empty()
                {
                    shapes.push(s);
                }
            }
            _ => {}
        }
        Ok(())
    });
    (shapes, images)
}

fn pptx(zip: &mut Zip, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let main = "ppt/presentation.xml";
    let xml = zip
        .part(main)?
        .ok_or("Not a PowerPoint presentation: ppt/presentation.xml is missing")?;
    let rels = zip
        .part(&office::rels_of(main))?
        .map(|r| office::relationship_ids(&r))
        .unwrap_or_default();
    let mut order: Vec<String> = vec![];
    office::walk(&xml, cancelled, |ev| {
        if let Ev::Open(tag) = ev
            && tag.name == "sldId"
            && let Some(id) = tag.attr(Ns::R, "id")
            && let Some((_, target, false)) = rels.get(id)
            && order.len() < MAX_SLIDES
        {
            order.push(office::resolve_target(main, target));
        }
        Ok(())
    })?;
    let mut out = Inspection::new("Presentation", "PowerPoint presentation (.pptx)");
    for name in ["docProps/core.xml", "docProps/app.xml"] {
        if let Some(x) = zip.part(name)? {
            for (k, v) in office::properties(&x, cancelled) {
                if k != "Pages" && k != "Words" {
                    let src = zip.span(name);
                    out.fact("Document", &k, v, src);
                }
            }
        }
    }
    let mut slides = vec![];
    let mut missing = 0usize;
    let mut picture_parts: Vec<(usize, String)> = vec![];
    for (n, part) in order.iter().enumerate() {
        if cancelled() {
            return Err("Cancelled".into());
        }
        let Some(xml) = zip.part(part)? else {
            missing += 1;
            continue;
        };
        let (shapes, images) = drawing_text(&xml, cancelled);
        let srels = zip
            .part(&office::rels_of(part))?
            .map(|r| office::relationship_ids(&r))
            .unwrap_or_default();
        let mut slide = Slide {
            source: zip.span(part),
            ..Slide::default()
        };
        for (kind, paragraphs) in shapes {
            if slide.title.is_empty() && matches!(kind.as_str(), "title" | "ctrTitle") {
                slide.title = paragraphs.join(" ");
                continue;
            }
            let shape = if kind.is_empty() { "text".into() } else { kind };
            slide
                .paragraphs
                .extend(paragraphs.into_iter().map(|p| (shape.clone(), p)));
        }
        for id in images {
            if let Some((_, target, false)) = srels.get(&id) {
                let path = office::resolve_target(part, target);
                if !slide.images.contains(&path) {
                    picture_parts.push((n + 1, path.clone()));
                    slide.images.push(path);
                }
            }
        }
        if let Some((_, target, _)) = srels
            .values()
            .find(|(kind, _, ext)| kind.ends_with("/notesSlide") && !ext)
        {
            let notes = office::resolve_target(part, target);
            if let Some(x) = zip.part(&notes)? {
                let (shapes, _) = drawing_text(&x, cancelled);
                slide.notes = shapes
                    .into_iter()
                    .filter(|(k, _)| k == "body")
                    .flat_map(|(_, p)| p)
                    .collect();
            }
        }
        slides.push(slide);
    }
    out.fact(
        "Presentation",
        "Slides",
        order.len().to_string(),
        zip.span(main),
    );
    finish(&mut out, slides, missing);
    pictures(&mut out, zip, &picture_parts);
    if zip
        .listing
        .entries
        .iter()
        .any(|e| e.name.to_ascii_lowercase().ends_with("vbaproject.bin"))
    {
        out.notes.push("Contains macros; they are never run".into());
    }
    Ok(out)
}

/// Pictures of the listed parts: a table of all, and the first decodable ones shown.
fn pictures(out: &mut Inspection, zip: &mut Zip, parts: &[(usize, String)]) {
    if parts.is_empty() {
        return;
    }
    let mut t = table("Pictures", &["Slide", "Part", "Size"]);
    let mut shown = 0usize;
    let mut bytes_shown = 0u64;
    for (slide, path) in parts {
        let entry = zip
            .listing
            .entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(path))
            .cloned();
        let size = entry.as_ref().map_or(0, |e| e.size);
        push_row(
            &mut t,
            vec![slide.to_string(), path.clone(), human(size)],
            zip.span(path),
        );
        if shown < MAX_PICTURES
            && entry.is_some()
            && bytes_shown + size <= MAX_PICTURE_BYTES
            && let Ok(Some(b)) = zip.part(path)
            && is_picture(&b)
        {
            shown += 1;
            bytes_shown += size;
            out.visuals.push(Visual::Picture {
                bytes: b,
                caption: format!("Slide {slide}: {}", path.rsplit('/').next().unwrap_or(path)),
            });
        }
    }
    out.tables.push(t);
}

/// The tables and checks shared by the three formats.
fn finish(out: &mut Inspection, slides: Vec<Slide>, missing: usize) {
    let mut list = table("Slides", &["No.", "Title", "Text", "Notes", "Pictures"]);
    let mut text = table("Slide text", &["Slide", "Shape", "Text"]);
    let mut notes = table("Speaker notes", &["Slide", "Text"]);
    for (i, s) in slides.iter().enumerate() {
        let n = (i + 1).to_string();
        let body: Vec<&str> = s.paragraphs.iter().map(|(_, p)| p.as_str()).collect();
        push_row(
            &mut list,
            vec![
                n.clone(),
                s.title.clone(),
                clip(&body.join(" · "), 500),
                clip(&s.notes.join(" "), 300),
                s.images.len().to_string(),
            ],
            s.source.clone(),
        );
        if !s.title.is_empty() && text.rows.len() < MAX_TEXT_ROWS {
            push_row(
                &mut text,
                vec![n.clone(), "title".into(), s.title.clone()],
                s.source.clone(),
            );
        }
        for (shape, p) in &s.paragraphs {
            if text.rows.len() < MAX_TEXT_ROWS {
                push_row(
                    &mut text,
                    vec![n.clone(), shape.clone(), p.clone()],
                    s.source.clone(),
                );
            }
        }
        for p in &s.notes {
            push_row(&mut notes, vec![n.clone(), p.clone()], s.source.clone());
        }
    }
    out.fact(
        "Presentation",
        "Slides with notes",
        slides
            .iter()
            .filter(|s| !s.notes.is_empty())
            .count()
            .to_string(),
        None,
    );
    out.fact(
        "Presentation",
        "Pictures used",
        slides
            .iter()
            .map(|s| s.images.len())
            .sum::<usize>()
            .to_string(),
        None,
    );
    out.tables.push(list);
    if !text.rows.is_empty() {
        out.tables.push(text);
    }
    if !notes.rows.is_empty() {
        out.tables.push(notes);
    }
    if missing > 0 {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("{missing} slides listed by the presentation are missing from the file"),
        );
    } else {
        out.check(
            Stage::Parsed,
            State::Passed,
            "Slides, notes and properties read",
        );
    }
    out.not_validated();
    out.notes.push("Slide layout, fonts, animations and charts are not reproduced: the text and pictures of each slide are listed".into());
}

fn odp(zip: &mut Zip, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let mimetype = zip.part("mimetype")?.unwrap_or_default();
    if !mimetype.starts_with(b"application/vnd.oasis.opendocument.presentation") {
        return Err("Not an OpenDocument presentation".into());
    }
    let xml = zip.part("content.xml")?.ok_or("content.xml is missing")?;
    let mut out = Inspection::new("Presentation", "OpenDocument presentation (.odp)");
    if let Some(x) = zip.part("meta.xml")? {
        for (k, v) in office::properties(&x, cancelled) {
            let src = zip.span("meta.xml");
            out.fact("Document", &k, v, src);
        }
    }
    let src = zip.span("content.xml");
    let mut slides: Vec<Slide> = vec![];
    let mut names: Vec<String> = vec![];
    let mut in_notes = 0usize;
    let mut frame_class: Vec<String> = vec![];
    let mut para: Option<String> = None;
    let mut picture_parts = vec![];
    office::walk(&xml, cancelled, |ev| {
        match ev {
            Ev::Open(tag) => match (tag.ns, tag.name.as_str()) {
                (Ns::Draw, "page") if slides.len() < MAX_SLIDES => {
                    names.push(tag.attr(Ns::Draw, "name").unwrap_or("").to_owned());
                    slides.push(Slide {
                        source: src.clone(),
                        ..Slide::default()
                    });
                }
                (Ns::Other, "notes") => in_notes += 1,
                (Ns::Draw, "frame") => {
                    frame_class.push(tag.attr(Ns::Other, "class").unwrap_or("").to_owned())
                }
                (Ns::Text, "p" | "h") => para = Some(String::new()),
                (Ns::Text, "s") | (Ns::Text, "tab") | (Ns::Text, "line-break") => {
                    if let Some(p) = para.as_mut() {
                        p.push(' ');
                    }
                }
                (Ns::Draw, "image") => {
                    if let (Some(href), Some(s)) = (tag.attr(Ns::Xlink, "href"), slides.last_mut())
                        && in_notes == 0
                        && !href.contains(':')
                    {
                        let path = href.trim_start_matches("./").to_owned();
                        if !s.images.contains(&path) {
                            s.images.push(path);
                        }
                    }
                }
                _ => {}
            },
            Ev::Text(t) => {
                if let Some(p) = para.as_mut()
                    && p.len() < 64 * 1024
                {
                    p.push_str(t);
                }
            }
            Ev::Close(Ns::Text, "p" | "h") => {
                if let (Some(p), Some(s)) = (para.take(), slides.last_mut()) {
                    let p = p.trim().to_owned();
                    if p.is_empty() {
                    } else if in_notes > 0 {
                        s.notes.push(p);
                    } else if s.title.is_empty() && frame_class.last().is_some_and(|c| c == "title")
                    {
                        s.title = p;
                    } else {
                        let shape = frame_class
                            .last()
                            .filter(|c| !c.is_empty())
                            .cloned()
                            .unwrap_or_else(|| "text".into());
                        s.paragraphs.push((shape, p));
                    }
                }
            }
            Ev::Close(Ns::Draw, "frame") => {
                frame_class.pop();
            }
            Ev::Close(Ns::Other, "notes") => in_notes = in_notes.saturating_sub(1),
            _ => {}
        }
        Ok(())
    })?;
    for (i, s) in slides.iter_mut().enumerate() {
        if s.title.is_empty() {
            s.title = names.get(i).cloned().unwrap_or_default();
        }
        for p in &s.images {
            picture_parts.push((i + 1, p.clone()));
        }
    }
    out.fact("Presentation", "Slides", slides.len().to_string(), src);
    finish(&mut out, slides, 0);
    pictures(&mut out, zip, &picture_parts);
    Ok(out)
}

// ----- PowerPoint 97–2003 -----

const DOCUMENT: u16 = 0x03E8;
const SLIDE: u16 = 0x03EE;
const NOTES: u16 = 0x03F0;
const SLIDE_LIST: u16 = 0x0FF0;
const SLIDE_PERSIST: u16 = 0x03F3;
const TEXT_HEADER: u16 = 0x0F9F;
const TEXT_CHARS: u16 = 0x0FA0;
const TEXT_BYTES: u16 = 0x0FA8;
const MAX_RECORDS: usize = 2_000_000;
const MAX_DEPTH: usize = 32;

struct PptText {
    /// Slide list entries: (instance: 0 slides, 2 notes; slide number; kind; text).
    list: Vec<(u16, usize, &'static str, String)>,
    /// Text inside Slide / Notes records, by record number in file order.
    drawings: Vec<(u16, usize, String)>,
    records: usize,
    damaged: bool,
}

fn text_kind(t: u32) -> &'static str {
    match t {
        0 | 6 => "title",
        1 | 5 | 7 | 8 => "body",
        2 => "notes",
        _ => "text",
    }
}

fn atom_text(kind: u16, data: &[u8]) -> String {
    let data = &data[..data.len().min(128 * 1024)];
    let s = if kind == TEXT_CHARS {
        let units: Vec<u16> = data
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        data.iter().map(|&b| doc_common::cp1252(b)).collect()
    };
    s.replace(['\r', '\u{b}'], "\n").trim().to_owned()
}

fn ppt_walk(
    stream: &[u8],
    range: std::ops::Range<usize>,
    depth: usize,
    ctx: (u16, usize),
    out: &mut PptText,
) {
    let mut at = range.start;
    while at + 8 <= range.end {
        out.records += 1;
        if out.records > MAX_RECORDS {
            out.damaged = true;
            return;
        }
        let ver = u16le(stream, at) & 0xF;
        let instance = u16le(stream, at) >> 4;
        let kind = u16le(stream, at + 2);
        let len = u32le(stream, at + 4) as usize;
        let start = at + 8;
        let Some(end) = start.checked_add(len).filter(|&e| e <= range.end) else {
            out.damaged = true;
            return;
        };
        match kind {
            SLIDE_LIST => {
                let mut slide = 0usize;
                let mut text_type = 4u32;
                let mut a = start;
                while a + 8 <= end {
                    let k = u16le(stream, a + 2);
                    let l = u32le(stream, a + 4) as usize;
                    let Some(e) = (a + 8).checked_add(l).filter(|&e| e <= end) else {
                        out.damaged = true;
                        break;
                    };
                    match k {
                        SLIDE_PERSIST => {
                            slide += 1;
                            text_type = 4;
                        }
                        TEXT_HEADER => text_type = u32le(stream, a + 8),
                        TEXT_CHARS | TEXT_BYTES if out.list.len() < MAX_TEXT_ROWS => {
                            let t = atom_text(k, &stream[a + 8..e]);
                            if !t.is_empty() {
                                out.list
                                    .push((instance, slide.max(1), text_kind(text_type), t));
                            }
                        }
                        _ => {}
                    }
                    a = e;
                }
            }
            SLIDE | NOTES if ver == 0xF && depth < MAX_DEPTH => {
                let count = out
                    .drawings
                    .iter()
                    .filter(|d| d.0 == kind)
                    .map(|d| d.1)
                    .max()
                    .unwrap_or(0)
                    + 1;
                ppt_walk(stream, start..end, depth + 1, (kind, count), out);
            }
            TEXT_CHARS | TEXT_BYTES if ctx.0 != 0 && out.drawings.len() < MAX_TEXT_ROWS => {
                let t = atom_text(kind, &stream[start..end]);
                if !t.is_empty() {
                    out.drawings.push((ctx.0, ctx.1, t));
                }
            }
            _ if ver == 0xF && depth < MAX_DEPTH && kind != 0x03F8 => {
                // Containers (the document, drawings); master slides' template text is skipped.
                ppt_walk(stream, start..end, depth + 1, ctx, out);
            }
            _ => {}
        }
        at = end;
    }
}

fn ppt(bytes: &[u8], cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let cfb = Cfb::open(bytes)?;
    let index = cfb
        .child(0, "PowerPoint Document")
        .ok_or("The Compound File has no PowerPoint Document stream")?;
    let (stream, cut) = cfb.read(index)?;
    if cancelled() {
        return Err("Cancelled".into());
    }
    let mut text = PptText {
        list: vec![],
        drawings: vec![],
        records: 0,
        damaged: false,
    };
    ppt_walk(&stream, 0..stream.len(), 0, (0, 0), &mut text);
    let mut out = Inspection::new("Presentation", "PowerPoint 97–2003 presentation (.ppt)");
    let slide_count = text
        .list
        .iter()
        .filter(|l| l.0 == 0)
        .map(|l| l.1)
        .max()
        .unwrap_or(0)
        .max(
            text.drawings
                .iter()
                .filter(|d| d.0 == SLIDE)
                .map(|d| d.1)
                .max()
                .unwrap_or(0),
        );
    let mut slides: Vec<Slide> = (0..slide_count.min(MAX_SLIDES))
        .map(|_| Slide::default())
        .collect();
    for (instance, n, kind, t) in &text.list {
        let Some(s) = slides.get_mut(n - 1) else {
            continue;
        };
        match (*instance, *kind) {
            (0, "title") if s.title.is_empty() => s.title = t.replace('\n', " "),
            (0, _) => s.paragraphs.extend(
                t.lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| (kind.to_string(), l.trim().to_owned())),
            ),
            (2, _) => s.notes.push(t.clone()),
            _ => {}
        }
    }
    for (kind, n, t) in &text.drawings {
        let Some(s) = slides.get_mut(n - 1) else {
            continue;
        };
        if *kind == SLIDE {
            s.paragraphs.extend(
                t.lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| ("text box".to_string(), l.trim().to_owned())),
            );
        } else {
            s.notes.push(t.clone());
        }
    }
    out.fact("Presentation", "Slides", slide_count.to_string(), None);
    out.fact(
        "Presentation",
        "Records read",
        text.records.to_string(),
        None,
    );
    let has_document = memchr::memmem::find(
        &stream[..stream.len().min(1 << 20)],
        &DOCUMENT.to_le_bytes(),
    )
    .is_some();
    if cfb.child(0, "Pictures").is_some() {
        out.notes.push(
            "Pictures are stored in the Pictures stream; they are not extracted from .ppt files"
                .into(),
        );
    }
    let damaged = text.damaged || cut || !cfb.warnings.is_empty();
    finish(&mut out, slides, 0);
    if damaged || !has_document {
        out.checks.retain(|c| c.stage != Stage::Parsed);
        out.check(
            Stage::Parsed,
            State::Partial,
            "The PowerPoint Document stream is damaged or cut: the text found before the damage is shown",
        );
    }
    out.notes.push("Text is read from the slide list and from text boxes in the slide records (in file order); placement, fonts and the slide masters' template text are not shown".into());
    Ok(out)
}
