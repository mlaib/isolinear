//! Namespace-aware Word and OpenDocument content reader, ported from the previous release.
use super::model::{Block, Cell, Document, Item, Span, Style};
use super::zip::{self as archive, Listing};
use quick_xml::{NsReader, events::Event, name::ResolveResult};
use std::collections::HashMap;
use std::io::{Read, Seek};
use std::sync::Arc;

/// Largest XML part read (decompressed).
pub const PART_LIMIT: u64 = 64 * 1024 * 1024;
/// Blocks produced at most, counting list items and table rows (as for Markdown).
pub const MAX_BLOCKS: usize = 50_000;
/// Text kept at most; later content is cut with a notice.
const MAX_TEXT: usize = 16 * 1024 * 1024;
/// Element nesting accepted in a part.
const MAX_DEPTH: usize = 256;
/// Repeated or spanned table cells expanded at most per cell, and columns per row.
const MAX_REPEAT: usize = 64;
const MAX_COLUMNS: usize = 1024;
const WALK_EVENTS: usize = 1_000_000;
const LIMIT: &str = "Preview limit reached";
const MODEL_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Docx,
    Odt,
}
pub struct OfficeDocument {
    pub content: Document,
    /// What the preview leaves out or changes (comments, tracked changes, macros, cut content).
    pub warnings: Vec<String>,
}

/// Which text document a ZIP holds, from its listing (and the ODT `mimetype` entry).
pub fn kind(file: &mut (impl Read + Seek), listing: &Listing) -> Option<Kind> {
    if main_part(file, listing).is_some() {
        return Some(Kind::Docx);
    }
    let mimetype = part(file, listing, "mimetype").ok().flatten()?;
    (mimetype.starts_with(b"application/vnd.oasis.opendocument.text")
        && has(listing, "content.xml"))
    .then_some(Kind::Odt)
}

/// Read the document's content. `None` when the ZIP is not a text document.
pub fn read(
    file: &mut (impl Read + Seek),
    listing: &Listing,
    cancelled: &dyn Fn() -> bool,
) -> Option<Result<OfficeDocument, String>> {
    Some(match kind(file, listing)? {
        Kind::Docx => docx(file, listing, cancelled),
        Kind::Odt => odt(file, listing, cancelled),
    })
}

fn has(listing: &Listing, name: &str) -> bool {
    listing.find(name).is_some()
}

/// One part's bytes; `Ok(None)` when the archive has no such entry. Part names are
/// case-insensitive in OOXML.
pub(crate) fn part(
    file: &mut (impl Read + Seek),
    listing: &Listing,
    name: &str,
) -> Result<Option<Vec<u8>>, String> {
    let Some(entry) = listing.find(name) else {
        return Ok(None);
    };
    if entry.size > PART_LIMIT {
        return Err(format!(
            "{name} is {} MiB when extracted; Isolinear reads parts up to {} MiB",
            entry.size >> 20,
            PART_LIMIT >> 20
        ));
    }
    let mut out = Vec::new();
    archive::extract_zip(file, entry, &mut out).map_err(|e| format!("{name}: {e}"))?;
    Ok(Some(out))
}

// ---------------------------------------------------------------------------------------------
// A small namespace-aware XML walker.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ns {
    W,
    /// SpreadsheetML (XLSX).
    S,
    R,
    Rel,
    Mc,
    Wp,
    Drawing,
    Vml,
    Text,
    Table,
    Office,
    Draw,
    Xlink,
    Style,
    Fo,
    Dc,
    Meta,
    Cp,
    Dcterms,
    Ep,
    Other,
}

fn ns_of(uri: &[u8]) -> Ns {
    match uri {
        b"http://schemas.openxmlformats.org/wordprocessingml/2006/main"
        | b"http://purl.oclc.org/ooxml/wordprocessingml/main" => Ns::W,
        b"http://schemas.openxmlformats.org/spreadsheetml/2006/main"
        | b"http://purl.oclc.org/ooxml/spreadsheetml/main" => Ns::S,
        b"http://schemas.openxmlformats.org/officeDocument/2006/relationships"
        | b"http://purl.oclc.org/ooxml/officeDocument/relationships" => Ns::R,
        b"http://schemas.openxmlformats.org/package/2006/relationships" => Ns::Rel,
        b"http://schemas.openxmlformats.org/markup-compatibility/2006" => Ns::Mc,
        b"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
        | b"http://purl.oclc.org/ooxml/drawingml/wordprocessingDrawing" => Ns::Wp,
        b"http://schemas.openxmlformats.org/drawingml/2006/main"
        | b"http://purl.oclc.org/ooxml/drawingml/main" => Ns::Drawing,
        b"urn:schemas-microsoft-com:vml" => Ns::Vml,
        b"urn:oasis:names:tc:opendocument:xmlns:text:1.0" => Ns::Text,
        b"urn:oasis:names:tc:opendocument:xmlns:table:1.0" => Ns::Table,
        b"urn:oasis:names:tc:opendocument:xmlns:office:1.0" => Ns::Office,
        b"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" => Ns::Draw,
        b"http://www.w3.org/1999/xlink" => Ns::Xlink,
        b"urn:oasis:names:tc:opendocument:xmlns:style:1.0" => Ns::Style,
        b"urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" => Ns::Fo,
        b"http://purl.org/dc/elements/1.1/" => Ns::Dc,
        b"urn:oasis:names:tc:opendocument:xmlns:meta:1.0" => Ns::Meta,
        b"http://schemas.openxmlformats.org/package/2006/metadata/core-properties" => Ns::Cp,
        b"http://purl.org/dc/terms/" => Ns::Dcterms,
        b"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"
        | b"http://purl.oclc.org/ooxml/officeDocument/extendedProperties" => Ns::Ep,
        _ => Ns::Other,
    }
}

pub(crate) struct Tag {
    pub(crate) ns: Ns,
    pub(crate) name: String,
    attrs: Vec<(Ns, String, String)>,
}
impl Tag {
    pub(crate) fn is(&self, ns: Ns, name: &str) -> bool {
        self.ns == ns && self.name == name
    }
    pub(crate) fn attr(&self, ns: Ns, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, l, _)| *n == ns && l == name)
            .map(|(_, _, v)| v.as_str())
    }
}

pub(crate) enum Ev<'a> {
    Open(&'a Tag),
    Close(Ns, &'a str),
    Text(&'a str),
}

/// Walk `xml`, calling `f` for every element start, end and piece of text (entity and character
/// references resolved; only XML's predefined entities exist, nothing is expanded or fetched).
pub(crate) fn walk(
    xml: &[u8],
    cancelled: &dyn Fn() -> bool,
    mut f: impl FnMut(Ev) -> Result<(), String>,
) -> Result<(), String> {
    if cancelled() {
        return Err("Cancelled".into());
    }
    if xml.len() as u64 > PART_LIMIT {
        return Err("XML part exceeds 64 MiB".into());
    }
    let mut reader = NsReader::from_reader(xml);
    reader.config_mut().expand_empty_elements = true;
    let mut open: Vec<(Ns, String)> = vec![];
    let mut events = 0usize;
    let mut text_left = MAX_TEXT;
    let mut attr_left = MAX_TEXT;
    loop {
        events += 1;
        if events > WALK_EVENTS {
            return Err(LIMIT.into());
        }
        if events.is_multiple_of(4096) && cancelled() {
            return Err("Cancelled".into());
        }
        let at = reader.buffer_position();
        let (resolved, event) = reader
            .read_resolved_event()
            .map_err(|e| format!("Malformed XML near byte {at}: {e}"))?;
        let ns = match resolved {
            ResolveResult::Bound(n) => ns_of(n.as_ref()),
            _ => Ns::Other,
        };
        match event {
            Event::Start(e) => {
                if open.len() >= MAX_DEPTH {
                    return Err(format!("Elements nest deeper than {MAX_DEPTH} levels"));
                }
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                let mut attrs = vec![];
                for a in e.attributes() {
                    let a =
                        a.map_err(|e| format!("Malformed XML attribute near byte {at}: {e}"))?;
                    if attrs.len() >= 256 || a.value.len() > 65_536 || a.value.len() > attr_left {
                        return Err(LIMIT.into());
                    }
                    attr_left -= a.value.len();
                    let (r, local) = reader.resolver().resolve_attribute(a.key);
                    let ans = match r {
                        ResolveResult::Bound(n) => ns_of(n.as_ref()),
                        _ => Ns::Other,
                    };
                    let value = a
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map(|v| v.into_owned())
                        .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned());
                    attrs.push((
                        ans,
                        String::from_utf8_lossy(local.as_ref()).into_owned(),
                        value,
                    ));
                }
                let tag = Tag { ns, name, attrs };
                f(Ev::Open(&tag))?;
                open.push((tag.ns, tag.name));
            }
            Event::End(_) => {
                let (ns, name) = open.pop().unwrap_or((Ns::Other, String::new()));
                f(Ev::Close(ns, &name))?;
            }
            Event::Text(t) => {
                let text = t.decode().map_err(|e| e.to_string())?;
                let end = text.floor_char_boundary(text.len().min(text_left));
                f(Ev::Text(text.get(..end).unwrap_or("")))?;
                text_left -= end;
                if end < text.len() {
                    return Err(LIMIT.into());
                }
            }
            Event::CData(t) => {
                let text = t.decode().map_err(|e| e.to_string())?;
                let end = text.floor_char_boundary(text.len().min(text_left));
                f(Ev::Text(text.get(..end).unwrap_or("")))?;
                text_left -= end;
                if end < text.len() {
                    return Err(LIMIT.into());
                }
            }
            Event::GeneralRef(r) => {
                let text = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => {
                        let name = r.decode().map_err(|e| e.to_string())?;
                        quick_xml::escape::resolve_predefined_entity(&name)
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("&{name};"))
                    }
                };
                let end = text.floor_char_boundary(text.len().min(text_left));
                f(Ev::Text(text.get(..end).unwrap_or("")))?;
                text_left -= end;
                if end < text.len() {
                    return Err(LIMIT.into());
                }
            }
            Event::Eof => {
                return if open.is_empty() {
                    Ok(())
                } else {
                    Err("Malformed XML: an element was not closed".into())
                };
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Building blocks, shared by both formats.

#[derive(Clone, Copy, Debug, PartialEq)]
enum ParaKind {
    Normal,
    Heading(u8),
    Quote,
    Code,
    /// A list paragraph at `level` (0 = outermost). `new_item`: starts an item; otherwise it
    /// continues the previous item (a second paragraph in an ODT list item).
    Item {
        level: usize,
        ordered: bool,
        new_item: bool,
        /// Which list: a different list at the same level starts a new one.
        list: u64,
        /// Start a new list even when `list` is the same (numbering restarts).
        restart: bool,
    },
}

struct Full;

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    budget: usize,
    text: usize,
    truncated: bool,
    /// Open lists from the outermost: (identity, ordered, items).
    lists: Vec<(u64, bool, Vec<Item>)>,
}

impl Builder {
    fn new() -> Self {
        Self {
            budget: MAX_BLOCKS,
            ..Default::default()
        }
    }
    fn take(&mut self, n: usize) -> Result<(), Full> {
        if self.budget < n {
            self.truncated = true;
            return Err(Full);
        }
        self.budget -= n;
        Ok(())
    }
    fn paragraph(&mut self, kind: ParaKind, spans: Vec<Span>) -> Result<(), Full> {
        let len: usize = spans.iter().map(|s| s.text.len()).sum();
        self.text += len;
        if self.text > MAX_TEXT {
            self.truncated = true;
            return Err(Full);
        }
        if let ParaKind::Item {
            level,
            ordered,
            new_item,
            list,
            restart,
        } = kind
        {
            return self.item(level, list, ordered, new_item, restart, spans);
        }
        self.close_lists()?;
        self.take(1)?;
        let block = match kind {
            ParaKind::Heading(level) => Block::Heading { level, spans },
            ParaKind::Code => Block::Code { spans },
            ParaKind::Quote => Block::Quote {
                blocks: vec![Block::Paragraph { spans }],
            },
            _ => Block::Paragraph { spans },
        };
        self.blocks.push(block);
        Ok(())
    }
    fn item(
        &mut self,
        level: usize,
        list: u64,
        ordered: bool,
        new_item: bool,
        restart: bool,
        spans: Vec<Span>,
    ) -> Result<(), Full> {
        // A list can only open one level deeper than the deepest open one.
        let depth = level.saturating_add(1).min(self.lists.len() + 1).min(64);
        while self.lists.len() > depth {
            self.pop_list()?;
        }
        if self.lists.len() == depth && self.lists.last().is_some_and(|l| l.0 != list || restart) {
            self.pop_list()?;
        }
        if self.lists.len() < depth {
            self.lists.push((list, ordered, vec![]));
        }
        self.take(1)?;
        let paragraph = Block::Paragraph { spans };
        let Some((_, _, items)) = self.lists.last_mut() else {
            return Err(Full);
        };
        match items.last_mut() {
            Some(item) if !new_item => item.blocks.push(paragraph),
            _ => items.push(Item {
                blocks: vec![paragraph],
            }),
        }
        Ok(())
    }
    /// Close the innermost list: into the parent's last item, or out as a block.
    fn pop_list(&mut self) -> Result<(), Full> {
        let Some((_, ordered, items)) = self.lists.pop() else {
            return Ok(());
        };
        self.take(0)?;
        let block = Block::List {
            start: ordered.then_some(1),
            items,
        };
        match self.lists.last_mut() {
            Some((_, _, parent)) => match parent.last_mut() {
                Some(item) => item.blocks.push(block),
                None => parent.push(Item {
                    blocks: vec![block],
                }),
            },
            None => self.blocks.push(block),
        }
        Ok(())
    }
    fn close_lists(&mut self) -> Result<(), Full> {
        while !self.lists.is_empty() {
            self.pop_list()?;
        }
        Ok(())
    }
    fn table(&mut self, mut rows: Vec<Vec<Cell>>) -> Result<(), Full> {
        self.close_lists()?;
        rows.retain(|r| !r.is_empty());
        if rows.is_empty() {
            return Ok(());
        }
        self.take(rows.len())?;
        let header = rows.remove(0);
        self.blocks.push(Block::Table { header, rows });
        Ok(())
    }
    fn finish(mut self) -> Document {
        let _ = self.close_lists();
        Document {
            blocks: self.blocks,
            truncated: self.truncated,
        }
    }
}

/// Append text in `style`, merging with the previous span when the style is the same.
fn push_text(spans: &mut Vec<Span>, text: &str, style: &Style) {
    if text.is_empty() {
        return;
    }
    match spans.last_mut() {
        Some(last) if last.style == *style => last.text.push_str(text),
        _ => spans.push(Span {
            text: text.to_owned(),
            style: style.clone(),
        }),
    }
}

/// A table being read; finished paragraphs inside a cell are appended to it.
#[derive(Default)]
struct TableState {
    rows: Vec<Vec<Cell>>,
    trim_empty: bool,
    row: Option<Vec<Cell>>,
    cell: Option<Vec<Span>>,
    /// Extra empty cells after the current one (merged or repeated columns).
    extra: usize,
    rowspan: usize,
    continuation: bool,
    merge_start: bool,
    covered: bool,
}

/// Where a finished paragraph goes: the innermost open table cell or note, else the document.
enum Sink {
    Table(TableState),
    Note(Vec<Span>),
}

fn append_lines(target: &mut Vec<Span>, spans: Vec<Span>) {
    if spans.is_empty() {
        return;
    }
    if !target.is_empty() {
        push_text(target, "\n", &Style::default());
    }
    for s in spans {
        push_text(target, &s.text, &s.style);
    }
}

/// A nested table, flattened into lines of " | "-separated cells.
fn flatten(rows: Vec<Vec<Cell>>) -> Vec<Span> {
    let mut out = vec![];
    for (r, row) in rows.into_iter().enumerate() {
        if r > 0 {
            push_text(&mut out, "\n", &Style::default());
        }
        for (c, cell) in row.into_iter().enumerate() {
            if c > 0 {
                push_text(&mut out, " | ", &Style::default());
            }
            for s in cell.spans {
                push_text(&mut out, &s.text.replace('\n', " "), &s.style);
            }
        }
    }
    out
}

/// Shared state of a body walk: the builder, open tables and notes, and collected notes.
struct Body {
    builder: Builder,
    sinks: Vec<Sink>,
    notes: Vec<(String, Vec<Span>)>,
    full: bool,
    retained: usize,
    units: usize,
}
impl Body {
    fn new() -> Self {
        Self {
            builder: Builder::new(),
            sinks: vec![],
            notes: vec![],
            full: false,
            retained: 0,
            units: 0,
        }
    }
    fn charge(&mut self, bytes: usize, units: usize) -> bool {
        self.retained = self.retained.saturating_add(bytes);
        self.units = self.units.saturating_add(units);
        if self.retained > MODEL_BYTES || self.units > MAX_BLOCKS {
            self.full = true;
            self.builder.truncated = true;
        }
        !self.full
    }
    fn paragraph(&mut self, kind: ParaKind, spans: Vec<Span>) {
        if !self.charge(span_bytes(&spans), 1) {
            return;
        }
        match self.sinks.last_mut() {
            Some(Sink::Table(t)) => {
                if let Some(cell) = t.cell.as_mut() {
                    append_lines(cell, spans);
                }
            }
            Some(Sink::Note(note)) => append_lines(note, spans),
            None => {
                let empty = spans.iter().all(|s| s.text.trim().is_empty());
                if !self.full && (!empty || matches!(kind, ParaKind::Item { .. })) {
                    self.full = self.builder.paragraph(kind, spans).is_err();
                }
            }
        }
    }
    fn table_start(&mut self, trim_empty: bool) {
        self.sinks.push(Sink::Table(TableState {
            trim_empty,
            ..TableState::default()
        }));
    }
    fn row_start(&mut self) {
        if let Some(Sink::Table(t)) = self.sinks.last_mut() {
            t.row = Some(vec![]);
        }
    }
    fn cell_start(&mut self, extra: usize) {
        if extra >= MAX_REPEAT {
            self.builder.truncated = true;
        }
        if let Some(Sink::Table(t)) = self.sinks.last_mut() {
            t.cell = Some(vec![]);
            t.extra = extra.min(MAX_REPEAT - 1);
            t.rowspan = 1;
            t.continuation = false;
            t.merge_start = false;
            t.covered = false;
        }
    }
    /// End a cell; `repeat` copies of it (ODT repeated columns), empty extras for spans.
    fn cell_end(&mut self, repeat: usize) {
        let Some(Sink::Table(t)) = self.sinks.last_mut() else {
            return;
        };
        let Some(cell) = t.cell.take() else {
            return;
        };
        if repeat > MAX_REPEAT && !cell.is_empty() {
            self.builder.truncated = true;
        }
        let repeat = repeat.clamp(1, MAX_REPEAT);
        let cell = Cell {
            spans: cell,
            colspan: t.extra + 1,
            rowspan: t.rowspan,
            continuation: t.continuation,
            merge_start: t.merge_start,
            covered: t.covered,
        };
        if !self.charge(
            (span_bytes(&cell.spans) + std::mem::size_of::<Cell>()).saturating_mul(repeat),
            repeat,
        ) {
            return;
        }
        if let Some(Sink::Table(t)) = self.sinks.last_mut()
            && let Some(row) = &mut t.row
        {
            if row.len().saturating_add(repeat) > MAX_COLUMNS {
                self.builder.truncated = true;
            }
            let mut columns: usize = row.iter().filter(|c| !c.covered).map(|c| c.colspan).sum();
            for _ in 0..repeat.min(MAX_COLUMNS.saturating_sub(row.len())) {
                if !cell.covered && columns >= MAX_COLUMNS {
                    self.builder.truncated = true;
                    break;
                }
                let mut copy = cell.clone();
                if !copy.covered {
                    copy.colspan = copy.colspan.min(MAX_COLUMNS.saturating_sub(columns));
                    self.builder.truncated |= copy.colspan != cell.colspan;
                    columns += copy.colspan;
                }
                row.push(copy);
            }
        }
    }
    fn row_end(&mut self, repeat: usize) {
        let Some(Sink::Table(t)) = self.sinks.last_mut() else {
            return;
        };
        let Some(mut row) = t.row.take() else {
            return;
        };
        while t.trim_empty
            && row.last().is_some_and(|c| {
                c.spans.is_empty()
                    && c.colspan == 1
                    && c.rowspan == 1
                    && !c.continuation
                    && !c.covered
            })
        {
            row.pop();
        }
        if row.is_empty() {
            return;
        }
        if repeat > MAX_REPEAT {
            self.builder.truncated = true;
        }
        let repeat = repeat.clamp(1, MAX_REPEAT);
        let bytes: usize = row
            .iter()
            .map(|c| span_bytes(&c.spans) + std::mem::size_of::<Cell>())
            .sum();
        if !self.charge(
            bytes.saturating_mul(repeat),
            repeat.saturating_mul(row.len() + 1),
        ) {
            return;
        }
        if let Some(Sink::Table(t)) = self.sinks.last_mut() {
            for _ in 0..repeat {
                t.rows.push(row.clone());
            }
        }
    }
    fn table_end(&mut self) {
        let Some(Sink::Table(mut t)) = self.sinks.pop() else {
            return;
        };
        self.builder.truncated |= merge_rows(&mut t.rows);
        match self.sinks.last_mut() {
            Some(Sink::Table(outer)) => {
                if let Some(cell) = outer.cell.as_mut() {
                    append_lines(cell, flatten(t.rows));
                }
            }
            Some(Sink::Note(note)) => append_lines(note, flatten(t.rows)),
            None => {
                self.full |= self.builder.table(t.rows).is_err();
            }
        }
    }
    fn finish(mut self) -> Document {
        if !self.notes.is_empty() && !self.full {
            let notes = std::mem::take(&mut self.notes);
            self.paragraph(ParaKind::Heading(2), vec![plain("Notes")]);
            for (label, mut spans) in notes {
                if let Some(first) = spans.first_mut() {
                    first.text = first.text.trim_start().to_owned();
                }
                let mut line = vec![plain(&format!("[{label}] "))];
                line.extend(spans);
                for span in &mut line {
                    span.style.note = true;
                }
                self.paragraph(ParaKind::Normal, line);
            }
        }
        self.builder.finish()
    }
}

fn plain(text: &str) -> Span {
    Span {
        text: text.to_owned(),
        style: Style::default(),
    }
}
fn placeholder(text: String) -> Span {
    Span {
        text,
        style: Style {
            image: true,
            ..Style::default()
        },
    }
}

// ---------------------------------------------------------------------------------------------
// DOCX

/// The main document part, from its content type in `[Content_Types].xml` (document, template
/// or macro-enabled variants), else `word/document.xml`.
fn main_part(file: &mut (impl Read + Seek), listing: &Listing) -> Option<String> {
    main_part_by_type(
        file,
        listing,
        &[
            "wordprocessingml.document.main",
            "wordprocessingml.template.main",
            "ms-word.document.macroEnabled.main",
            "ms-word.template.macroEnabledTemplate.main",
        ],
        "word/document.xml",
    )
}

/// The part whose `[Content_Types].xml` override contains one of `types`, else `fallback` when
/// the archive has it.
pub(crate) fn main_part_by_type(
    file: &mut (impl Read + Seek),
    listing: &Listing,
    types: &[&str],
    fallback: &str,
) -> Option<String> {
    if let Ok(Some(xml)) = part(file, listing, "[Content_Types].xml") {
        let mut found = None;
        let _ = walk(&xml, &|| false, |ev| {
            if let Ev::Open(tag) = ev
                && tag.name == "Override"
                && tag
                    .attr(Ns::Other, "ContentType")
                    .is_some_and(|t| types.iter().any(|k| t.contains(k)))
            {
                found = tag
                    .attr(Ns::Other, "PartName")
                    .map(|p| p.trim_start_matches('/').to_owned());
                return Err(String::new());
            }
            Ok(())
        });
        if let Some(found) = found.filter(|f| has(listing, f)) {
            return Some(found);
        }
    }
    has(listing, fallback).then(|| fallback.to_owned())
}

pub(crate) fn relationship_ids(xml: &[u8]) -> HashMap<String, (String, String, bool)> {
    let mut out = HashMap::new();
    let _ = walk(xml, &|| false, |ev| {
        if let Ev::Open(tag) = ev
            && tag.is(Ns::Rel, "Relationship")
            && let Some(id) = tag.attr(Ns::Other, "Id")
        {
            out.insert(
                id.to_owned(),
                (
                    tag.attr(Ns::Other, "Type").unwrap_or("").to_owned(),
                    tag.attr(Ns::Other, "Target").unwrap_or("").to_owned(),
                    tag.attr(Ns::Other, "TargetMode") == Some("External"),
                ),
            );
        }
        Ok(())
    });
    out
}

/// `word/document.xml` → `word/_rels/document.xml.rels`; a target relative to `word/`.
pub(crate) fn rels_of(name: &str) -> String {
    match name.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{name}.rels"),
    }
}
pub(crate) fn resolve_target(base: &str, target: &str) -> String {
    let decoded = package_uri(target);
    let target = decoded.as_str();
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_owned();
    }
    let mut parts: Vec<&str> = base
        .rsplit_once('/')
        .map_or(vec![], |(d, _)| d.split('/').collect());
    for p in target.split('/') {
        match p {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// Bold/italic/strike set by a style or run; `None` inherits.
#[derive(Clone, Copy, Default, Debug)]
struct Toggles {
    bold: Option<bool>,
    italic: Option<bool>,
    strike: Option<bool>,
    underline: Option<bool>,
    position: Option<i8>,
}
impl Toggles {
    fn over(self, base: Toggles) -> Toggles {
        Toggles {
            bold: self.bold.or(base.bold),
            italic: self.italic.or(base.italic),
            strike: self.strike.or(base.strike),
            underline: self.underline.or(base.underline),
            position: self.position.or(base.position),
        }
    }
}

#[derive(Clone, Default, Debug)]
struct WordStyle {
    name: String,
    based_on: Option<String>,
    outline: Option<u8>,
    num: Option<(String, usize)>,
    toggles: Toggles,
}

/// A WordprocessingML on/off property: present means on unless `w:val` is false/0/off.
fn on(tag: &Tag) -> bool {
    !matches!(tag.attr(Ns::W, "val"), Some("0" | "false" | "off"))
}

fn word_styles(xml: &[u8], cancelled: &dyn Fn() -> bool) -> HashMap<String, WordStyle> {
    let mut styles = HashMap::new();
    let mut current: Option<(String, WordStyle)> = None;
    let mut in_ppr = false;
    let mut num_id: Option<String> = None;
    let mut ilvl = 0usize;
    let _ = walk(xml, cancelled, |ev| {
        match ev {
            Ev::Open(tag) if tag.ns == Ns::W => match tag.name.as_str() {
                "style" => {
                    let id = tag.attr(Ns::W, "styleId").unwrap_or("").to_owned();
                    current = Some((id, WordStyle::default()));
                    num_id = None;
                    ilvl = 0;
                }
                "name" => {
                    if let Some((_, s)) = current.as_mut() {
                        s.name = tag.attr(Ns::W, "val").unwrap_or("").to_owned();
                    }
                }
                "basedOn" => {
                    if let Some((_, s)) = current.as_mut() {
                        s.based_on = tag.attr(Ns::W, "val").map(str::to_owned);
                    }
                }
                "pPr" => in_ppr = true,
                "outlineLvl" if in_ppr => {
                    if let Some((_, s)) = current.as_mut() {
                        s.outline = tag.attr(Ns::W, "val").and_then(|v| v.parse().ok());
                    }
                }
                "numId" if in_ppr => num_id = tag.attr(Ns::W, "val").map(str::to_owned),
                "ilvl" if in_ppr => {
                    ilvl = tag
                        .attr(Ns::W, "val")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0)
                }
                "b" => {
                    if let Some((_, s)) = current.as_mut() {
                        s.toggles.bold = Some(on(tag));
                    }
                }
                "i" => {
                    if let Some((_, s)) = current.as_mut() {
                        s.toggles.italic = Some(on(tag));
                    }
                }
                "u" => {
                    if let Some((_, s)) = current.as_mut() {
                        s.toggles.underline = Some(tag.attr(Ns::W, "val") != Some("none"));
                    }
                }
                "vertAlign" => {
                    if let Some((_, s)) = current.as_mut() {
                        s.toggles.position = Some(word_position(tag));
                    }
                }
                "strike" | "dstrike" => {
                    if let Some((_, s)) = current.as_mut() {
                        s.toggles.strike = Some(on(tag));
                    }
                }
                _ => {}
            },
            Ev::Close(Ns::W, "pPr") => in_ppr = false,
            Ev::Close(Ns::W, "style") => {
                if let Some((id, mut s)) = current.take() {
                    s.num = num_id.take().filter(|n| n != "0").map(|n| (n, ilvl));
                    styles.insert(id, s);
                }
            }
            _ => {}
        }
        Ok(())
    });
    styles
}

/// Follow `basedOn` (at most 16 steps; loops stop there).
fn style_chain<'a>(styles: &'a HashMap<String, WordStyle>, id: &str) -> Vec<&'a WordStyle> {
    let mut out = vec![];
    let mut next = Some(id);
    while let Some(id) = next.take() {
        if out.len() >= 16 {
            break;
        }
        if let Some(s) = styles.get(id) {
            out.push(s);
            next = s.based_on.as_deref();
        }
    }
    out
}
fn chain_toggles(chain: &[&WordStyle]) -> Toggles {
    chain
        .iter()
        .fold(Toggles::default(), |acc, s| acc.over(s.toggles))
}

/// A numbering instance: its abstract definition (lists sharing one continue each other's
/// count), per-level "is it a bullet", and whether it restarts the count.
#[derive(Clone, Default)]
struct Numbering {
    abstract_id: u64,
    bullets: Vec<bool>,
    restarts: bool,
}

/// numId → its numbering.
fn word_numbering(xml: &[u8], cancelled: &dyn Fn() -> bool) -> HashMap<String, Numbering> {
    let mut abstract_levels: HashMap<String, Vec<bool>> = HashMap::new();
    let mut nums: HashMap<String, (String, bool)> = HashMap::new();
    let mut current_abstract: Option<String> = None;
    let mut level: Option<usize> = None;
    let mut current_num: Option<String> = None;
    let _ = walk(xml, cancelled, |ev| {
        match ev {
            Ev::Open(tag) if tag.ns == Ns::W => match tag.name.as_str() {
                "abstractNum" => {
                    current_abstract = tag.attr(Ns::W, "abstractNumId").map(str::to_owned)
                }
                "lvl" if current_abstract.is_some() => {
                    level = tag
                        .attr(Ns::W, "ilvl")
                        .and_then(|v| v.parse().ok())
                        .filter(|&l: &usize| l < 16)
                }
                "numFmt" => {
                    if let (Some(a), Some(l)) = (&current_abstract, level) {
                        let bullet = matches!(tag.attr(Ns::W, "val"), Some("bullet" | "none"));
                        let levels = abstract_levels.entry(a.clone()).or_default();
                        if levels.len() <= l {
                            levels.resize(l + 1, true);
                        }
                        if let Some(value) = levels.get_mut(l) {
                            *value = bullet;
                        }
                    }
                }
                "num" => current_num = tag.attr(Ns::W, "numId").map(str::to_owned),
                "abstractNumId" => {
                    if let (Some(n), Some(a)) = (&current_num, tag.attr(Ns::W, "val")) {
                        nums.entry(n.clone()).or_default().0 = a.to_owned();
                    }
                }
                "startOverride" => {
                    if let Some(n) = &current_num {
                        nums.entry(n.clone()).or_default().1 = true;
                    }
                }
                _ => {}
            },
            Ev::Close(Ns::W, "abstractNum") => current_abstract = None,
            Ev::Close(Ns::W, "lvl") => level = None,
            Ev::Close(Ns::W, "num") => current_num = None,
            _ => {}
        }
        Ok(())
    });
    nums.into_iter()
        .map(|(n, (a, restarts))| {
            let numbering = Numbering {
                abstract_id: a.parse().unwrap_or(u64::MAX),
                bullets: abstract_levels.get(&a).cloned().unwrap_or_default(),
                restarts,
            };
            (n, numbering)
        })
        .collect()
}

/// Paragraph being read in a DOCX body.
#[derive(Default)]
struct WordPara {
    style: Option<String>,
    num: Option<(String, usize)>,
    outline: Option<u8>,
    spans: Vec<Span>,
}

struct WordContext<'a> {
    part: &'a str,
    styles: &'a HashMap<String, WordStyle>,
    numbering: &'a HashMap<String, Numbering>,
    links: &'a HashMap<String, (String, String, bool)>,
}

#[derive(Default)]
struct WordCounters {
    tracked: usize,
    images: usize,
    footnote_refs: Vec<(String, String)>,
}

/// Walk a WordprocessingML part (the document body, or footnotes/endnotes when `notes` is
/// set: then each `w:footnote`/`w:endnote` becomes one note keyed by its id).
fn word_body(
    xml: &[u8],
    ctx: &WordContext,
    body: &mut Body,
    counters: &mut WordCounters,
    notes: Option<&mut HashMap<String, Vec<Span>>>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    let mut paras: Vec<WordPara> = vec![];
    let mut in_ppr = 0usize;
    let mut in_rpr = false;
    let mut run = Toggles::default();
    let mut run_style: Option<String> = None;
    let mut links: Vec<Option<Arc<str>>> = vec![];
    // Depth inside content that is not shown (fallbacks, deleted text, field codes, drawings).
    let mut skip = 0usize;
    let mut note_id: Option<String> = None;
    let mut collected: HashMap<String, Vec<Span>> = HashMap::new();
    let collecting = notes.is_some();
    let mut drawing_name: Option<String> = None;
    let mut drawing_part: Option<String> = None;
    let mut in_drawing = false;
    // Text counts only inside `w:t` (whitespace between elements is layout of the XML).
    let mut in_t = false;
    let mut last_num: Option<String> = None;

    let style_of =
        |p: &WordPara, run: Toggles, run_style: &Option<String>, link: Option<Arc<str>>| {
            let para = p
                .style
                .as_deref()
                .map(|s| chain_toggles(&style_chain(ctx.styles, s)))
                .unwrap_or_default();
            let chr = run_style
                .as_deref()
                .map(|s| chain_toggles(&style_chain(ctx.styles, s)))
                .unwrap_or_default();
            let t = run.over(chr).over(para);
            Style {
                strong: t.bold.unwrap_or(false),
                emphasis: t.italic.unwrap_or(false),
                strike: t.strike.unwrap_or(false),
                underline: t.underline.unwrap_or(false),
                position: t.position.unwrap_or(0),
                link,
                ..Style::default()
            }
        };
    let current_link = |links: &Vec<Option<Arc<str>>>| links.iter().rev().flatten().next().cloned();

    let result = walk(xml, cancelled, |ev| {
        match ev {
            Ev::Open(tag) => {
                if skip > 0 {
                    skip += 1;
                    if in_drawing && (tag.is(Ns::Drawing, "blip") || tag.is(Ns::Vml, "imagedata")) {
                        drawing_part = tag
                            .attr(Ns::R, "embed")
                            .or(tag.attr(Ns::R, "id"))
                            .and_then(|id| ctx.links.get(id))
                            .filter(|(kind, _, external)| !external && kind.ends_with("/image"))
                            .map(|(_, target, _)| resolve_target(ctx.part, target));
                    }
                    if in_drawing
                        && tag.ns == Ns::Wp
                        && tag.name == "docPr"
                        && drawing_name.is_none()
                    {
                        drawing_name = tag
                            .attr(Ns::Other, "descr")
                            .filter(|d| !d.is_empty())
                            .or(tag.attr(Ns::Other, "name"))
                            .map(str::to_owned);
                    }
                    return Ok(());
                }
                match (tag.ns, tag.name.as_str()) {
                    (Ns::Mc, "Fallback") | (Ns::W, "delText" | "instrText" | "delInstrText") => {
                        skip = 1
                    }
                    (Ns::W, "del" | "moveFrom") => {
                        counters.tracked += 1;
                        skip = 1;
                    }
                    (Ns::W, "ins" | "moveTo") => counters.tracked += 1,
                    (Ns::W, "drawing" | "pict" | "object") => {
                        counters.images += 1;
                        drawing_name = None;
                        drawing_part = None;
                        in_drawing = true;
                        skip = 1;
                    }
                    (Ns::W, "footnote" | "endnote") if collecting => {
                        note_id = tag.attr(Ns::W, "id").map(str::to_owned);
                        body.sinks.push(Sink::Note(vec![]));
                    }
                    (Ns::W, "p") => paras.push(WordPara::default()),
                    (Ns::W, "t") => in_t = true,
                    (Ns::W, "pPr") => in_ppr += 1,
                    (Ns::W, "pStyle") if in_ppr > 0 => {
                        if let Some(p) = paras.last_mut() {
                            p.style = tag.attr(Ns::W, "val").map(str::to_owned);
                        }
                    }
                    (Ns::W, "numId") if in_ppr > 0 => {
                        if let Some(p) = paras.last_mut() {
                            let id = tag.attr(Ns::W, "val").unwrap_or("0").to_owned();
                            let level = p.num.as_ref().map_or(0, |n| n.1);
                            p.num = Some((id, level));
                        }
                    }
                    (Ns::W, "ilvl") if in_ppr > 0 => {
                        if let Some(p) = paras.last_mut() {
                            let level = tag
                                .attr(Ns::W, "val")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0usize)
                                .min(15);
                            let id = p.num.take().map_or(String::new(), |n| n.0);
                            p.num = Some((id, level));
                        }
                    }
                    (Ns::W, "outlineLvl") if in_ppr > 0 => {
                        if let Some(p) = paras.last_mut() {
                            p.outline = tag.attr(Ns::W, "val").and_then(|v| v.parse().ok());
                        }
                    }
                    // Run properties: only directly inside a run, not the paragraph mark's.
                    (Ns::W, "r") => {
                        run = Toggles::default();
                        run_style = None;
                    }
                    (Ns::W, "rPr") if in_ppr == 0 => in_rpr = true,
                    (Ns::W, "rStyle") if in_rpr => {
                        run_style = tag.attr(Ns::W, "val").map(str::to_owned)
                    }
                    (Ns::W, "b") if in_rpr => run.bold = Some(on(tag)),
                    (Ns::W, "i") if in_rpr => run.italic = Some(on(tag)),
                    (Ns::W, "u") if in_rpr => {
                        run.underline = Some(tag.attr(Ns::W, "val") != Some("none"))
                    }
                    (Ns::W, "vertAlign") if in_rpr => run.position = Some(word_position(tag)),
                    (Ns::W, "strike" | "dstrike") if in_rpr => run.strike = Some(on(tag)),
                    (Ns::W, "hyperlink") => {
                        let target = tag
                            .attr(Ns::R, "id")
                            .and_then(|id| ctx.links.get(id))
                            .map(|(_, t, _)| t.clone())
                            .or_else(|| tag.attr(Ns::W, "anchor").map(|a| format!("#{a}")));
                        links.push(target.map(Arc::from));
                    }
                    (Ns::W, "tab") if in_ppr == 0 => {
                        if let Some(p) = paras.last_mut() {
                            let s = style_of(p, run, &run_style, current_link(&links));
                            push_text(&mut p.spans, "\t", &s);
                        }
                    }
                    (Ns::W, "br" | "cr") => {
                        if let Some(p) = paras.last_mut() {
                            push_text(&mut p.spans, "\n", &Style::default());
                        }
                    }
                    (Ns::W, "noBreakHyphen") => {
                        if let Some(p) = paras.last_mut() {
                            let s = style_of(p, run, &run_style, current_link(&links));
                            push_text(&mut p.spans, "-", &s);
                        }
                    }
                    (Ns::W, "footnoteReference" | "endnoteReference") => {
                        let kind = if tag.name.starts_with("foot") {
                            "footnote"
                        } else {
                            "endnote"
                        };
                        if let (Some(p), Some(id)) = (paras.last_mut(), tag.attr(Ns::W, "id")) {
                            counters
                                .footnote_refs
                                .push((kind.to_owned(), id.to_owned()));
                            let label = counters.footnote_refs.len().to_string();
                            push_text(
                                &mut p.spans,
                                &format!("[{label}]"),
                                &Style {
                                    note: true,
                                    ..Style::default()
                                },
                            );
                        }
                    }
                    (Ns::W, "tbl") => body.table_start(false),
                    (Ns::W, "tr") => body.row_start(),
                    (Ns::W, "tc") => body.cell_start(0),
                    (Ns::W, "vMerge") => {
                        if let Some(Sink::Table(t)) = body.sinks.last_mut() {
                            t.continuation = tag.attr(Ns::W, "val") != Some("restart");
                            t.merge_start = !t.continuation;
                        }
                    }
                    (Ns::W, "gridSpan") => {
                        if let Some(Sink::Table(t)) = body.sinks.last_mut() {
                            t.extra = tag
                                .attr(Ns::W, "val")
                                .and_then(|v| v.parse::<usize>().ok())
                                .unwrap_or(1)
                                .saturating_sub(1)
                                .min(MAX_REPEAT - 1);
                        }
                    }
                    _ => {}
                }
            }
            Ev::Close(ns, name) => {
                if skip > 0 {
                    skip -= 1;
                    if skip == 0
                        && ns == Ns::W
                        && matches!(name, "drawing" | "pict" | "object")
                        && let Some(p) = paras.last_mut()
                    {
                        let label = match drawing_name.take() {
                            Some(n) => format!("[image: {n}]"),
                            None => "[image]".to_owned(),
                        };
                        let mut span = placeholder(label);
                        span.style.image_part = drawing_part.take().map(Arc::from);
                        p.spans.push(span);
                        in_drawing = false;
                    }
                    return Ok(());
                }
                match (ns, name) {
                    (Ns::W, "pPr") => in_ppr = in_ppr.saturating_sub(1),
                    (Ns::W, "t") => in_t = false,
                    (Ns::W, "rPr") => in_rpr = false,
                    (Ns::W, "hyperlink") => {
                        links.pop();
                    }
                    (Ns::W, "p") => {
                        if let Some(p) = paras.pop() {
                            let kind = word_kind(&p, ctx, &mut last_num);
                            body.paragraph(kind, p.spans);
                        }
                    }
                    (Ns::W, "tc") => body.cell_end(1),
                    (Ns::W, "tr") => body.row_end(1),
                    (Ns::W, "tbl") => body.table_end(),
                    (Ns::W, "footnote" | "endnote") if collecting => {
                        if let Some(Sink::Note(spans)) = body.sinks.pop()
                            && let Some(id) = note_id.take()
                        {
                            collected.insert(id, spans);
                        }
                    }
                    _ => {}
                }
            }
            Ev::Text(text) => {
                if skip == 0
                    && in_t
                    && let Some(p) = paras.last_mut()
                {
                    let s = style_of(p, run, &run_style, current_link(&links));
                    push_text(&mut p.spans, text, &s);
                }
            }
        }
        if body.full { Err(LIMIT.into()) } else { Ok(()) }
    });
    finish_walk(result, body)?;
    for p in paras {
        body.paragraph(ParaKind::Normal, p.spans);
    }
    while matches!(body.sinks.last(), Some(Sink::Table(_))) {
        body.table_end();
    }
    if let Some(notes) = notes {
        *notes = collected;
    }
    Ok(())
}

/// Heading, list item or plain paragraph, from direct properties and the style chain.
/// `last_num`: the numbering instance of the previous list paragraph.
fn word_kind(p: &WordPara, ctx: &WordContext, last_num: &mut Option<String>) -> ParaKind {
    let chain = p
        .style
        .as_deref()
        .map(|s| style_chain(ctx.styles, s))
        .unwrap_or_default();
    let num = p
        .num
        .clone()
        .or_else(|| chain.iter().find_map(|s| s.num.clone()))
        .filter(|(id, _)| !id.is_empty() && id != "0");
    if let Some((id, level)) = num {
        let numbering = ctx.numbering.get(&id).cloned().unwrap_or_default();
        let bullet = numbering.bullets.get(level).copied().unwrap_or(true);
        let restart = numbering.restarts && last_num.as_ref() != Some(&id);
        *last_num = Some(id);
        return ParaKind::Item {
            level,
            ordered: !bullet,
            new_item: true,
            list: numbering.abstract_id,
            restart,
        };
    }
    // Headings through the style chain (a custom style based on "heading 2" is one); a title or
    // quote only by the paragraph's own style ("Author" based on "Title" is neither).
    if let Some(own) = chain.first() {
        let name = own.name.to_ascii_lowercase();
        if name.contains("source code") || name == "preformatted text" {
            return ParaKind::Code;
        }
        if name == "title" {
            return ParaKind::Heading(1);
        }
        if name.contains("quote") || name == "block text" {
            return ParaKind::Quote;
        }
    }
    for s in &chain {
        let name = s.name.to_ascii_lowercase();
        if let Some(n) = name
            .strip_prefix("heading")
            .map(str::trim)
            .and_then(|n| n.parse::<u8>().ok())
        {
            return ParaKind::Heading(n.clamp(1, 6));
        }
    }
    if chain.is_empty()
        && let Some(id) = &p.style
    {
        let name = id.to_ascii_lowercase();
        if name == "title" {
            return ParaKind::Heading(1);
        }
        if let Some(n) = name
            .strip_prefix("heading")
            .and_then(|n| n.trim().parse::<u8>().ok())
        {
            return ParaKind::Heading(n.clamp(1, 6));
        }
    }
    let outline = p.outline.or_else(|| chain.iter().find_map(|s| s.outline));
    match outline {
        Some(level) if level < 9 => ParaKind::Heading((level + 1).min(6)),
        _ => ParaKind::Normal,
    }
}

fn docx(
    file: &mut (impl Read + Seek),
    listing: &Listing,
    cancelled: &dyn Fn() -> bool,
) -> Result<OfficeDocument, String> {
    let main = main_part(file, listing).ok_or("The Word document part is missing")?;
    let xml = part(file, listing, &main)?.ok_or("The Word document part is missing")?;
    let rels = part(file, listing, &rels_of(&main))?
        .map(|r| relationship_ids(&r))
        .unwrap_or_default();
    let target = |suffix: &str| {
        rels.values()
            .find(|(kind, _, external)| kind.ends_with(suffix) && !external)
            .map(|(_, t, _)| resolve_target(&main, t))
    };
    let read = |file: &mut _, name: Option<String>| -> Result<Option<Vec<u8>>, String> {
        match name {
            Some(n) => part(file, listing, &n),
            None => Ok(None),
        }
    };
    let styles = read(file, target("/styles"))?
        .map(|x| word_styles(&x, cancelled))
        .unwrap_or_default();
    let numbering = read(file, target("/numbering"))?
        .map(|x| word_numbering(&x, cancelled))
        .unwrap_or_default();
    let links: HashMap<String, (String, String, bool)> =
        rels.iter().map(|(id, v)| (id.clone(), v.clone())).collect();
    let ctx = WordContext {
        part: &main,
        styles: &styles,
        numbering: &numbering,
        links: &links,
    };
    let mut body = Body::new();
    let mut counters = WordCounters::default();
    word_body(&xml, &ctx, &mut body, &mut counters, None, cancelled)?;

    // Notes, in the order they are referenced.
    let mut warnings = vec![];
    if !counters.footnote_refs.is_empty() {
        let mut notes: HashMap<(&str, String), Vec<Span>> = HashMap::new();
        for (kind, suffix) in [("footnote", "/footnotes"), ("endnote", "/endnotes")] {
            if let Some(note_part) = target(suffix)
                && let Some(x) = part(file, listing, &note_part)?
            {
                let note_links = part(file, listing, &rels_of(&note_part))?
                    .map(|r| relationship_ids(&r))
                    .unwrap_or_default();
                let note_ctx = WordContext {
                    part: &note_part,
                    styles: &styles,
                    numbering: &numbering,
                    links: &note_links,
                };
                let mut found = HashMap::new();
                let mut scratch = Body::new();
                let mut c = WordCounters::default();
                if let Err(e) = word_body(
                    &x,
                    &note_ctx,
                    &mut scratch,
                    &mut c,
                    Some(&mut found),
                    cancelled,
                ) {
                    warnings.push(format!("The {kind}s could not be read: {e}"));
                }
                notes.extend(found.into_iter().map(|(id, s)| ((kind, id), s)));
            }
        }
        for (n, (kind, id)) in counters.footnote_refs.iter().enumerate() {
            let spans = notes
                .remove(&(kind.as_str(), id.clone()))
                .unwrap_or_default();
            body.notes.push(((n + 1).to_string(), spans));
        }
    }
    let content = body.finish();

    if counters.tracked > 0 {
        warnings
            .push("Has tracked changes: shown as if accepted (deleted text is hidden)".to_owned());
    }
    if let Some(x) = read(file, target("/comments"))? {
        let n = count(&x, Ns::W, "comment", cancelled);
        if n > 0 {
            warnings.push(format!("Has {n} comments (not shown)"));
        }
    }
    if counters.images > 0 {
        warnings.push(format!(
            "{} images, drawings and text boxes; unsupported or external images are shown as placeholders",
            counters.images
        ));
    }
    let external = rels
        .values()
        .filter(|(kind, _, external)| *external && !kind.ends_with("/hyperlink"))
        .count();
    if external > 0 {
        warnings.push(format!(
            "Refers to {external} external resources (linked images or templates); they are never loaded"
        ));
    }
    if listing
        .entries
        .iter()
        .any(|e| e.name.to_ascii_lowercase().ends_with("vbaproject.bin"))
    {
        warnings.push("Contains macros; they are never run".to_owned());
    }
    finish(content, warnings)
}

fn count(xml: &[u8], ns: Ns, name: &str, cancelled: &dyn Fn() -> bool) -> usize {
    let mut n = 0;
    let _ = walk(xml, cancelled, |ev| {
        if let Ev::Open(tag) = ev
            && tag.is(ns, name)
        {
            n += 1;
        }
        Ok(())
    });
    n
}

fn finish(content: Document, mut warnings: Vec<String>) -> Result<OfficeDocument, String> {
    if content.truncated {
        warnings.push(format!(
            "The preview stops at a content limit: {MAX_BLOCKS} blocks/cells, 16 MiB text, 32 MiB retained content, or {WALK_EVENTS} XML events; repeated rows/cells are capped at {MAX_REPEAT} columns at {MAX_COLUMNS}, and explicit spaces at 1,000 per element"
        ));
    }
    Ok(OfficeDocument { content, warnings })
}

// ---------------------------------------------------------------------------------------------
// ODT

#[derive(Clone, Default, Debug)]
struct OdfStyle {
    parent: Option<String>,
    display: String,
    toggles: Toggles,
}

/// Text and paragraph styles (bold/italic/strike, parent, display name) and list styles
/// (per-level "numbered") from `office:styles` and `office:automatic-styles`.
fn odf_styles(
    xml: &[u8],
    styles: &mut HashMap<String, OdfStyle>,
    lists: &mut HashMap<String, Vec<bool>>,
    cancelled: &dyn Fn() -> bool,
) {
    let mut current: Option<(String, OdfStyle)> = None;
    let mut list: Option<String> = None;
    let _ = walk(xml, cancelled, |ev| {
        match ev {
            Ev::Open(tag) => match (tag.ns, tag.name.as_str()) {
                (Ns::Style, "style") => {
                    let name = tag.attr(Ns::Style, "name").unwrap_or("").to_owned();
                    current = Some((
                        name.clone(),
                        OdfStyle {
                            parent: tag.attr(Ns::Style, "parent-style-name").map(str::to_owned),
                            display: tag
                                .attr(Ns::Style, "display-name")
                                .map(str::to_owned)
                                .unwrap_or_else(|| name.replace("_20_", " ")),
                            toggles: Toggles::default(),
                        },
                    ));
                }
                (Ns::Style, "text-properties") => {
                    if let Some((_, s)) = current.as_mut() {
                        if let Some(w) = tag.attr(Ns::Fo, "font-weight") {
                            s.toggles.bold =
                                Some(w == "bold" || w.parse::<u32>().is_ok_and(|n| n >= 600));
                        }
                        if let Some(f) = tag.attr(Ns::Fo, "font-style") {
                            s.toggles.italic = Some(f == "italic" || f == "oblique");
                        }
                        if let Some(l) = tag.attr(Ns::Style, "text-underline-style") {
                            s.toggles.underline = Some(l != "none");
                        }
                        if let Some(l) = tag.attr(Ns::Style, "text-position") {
                            s.toggles.position = Some(if l.starts_with("super") {
                                1
                            } else if l.starts_with("sub") || l.starts_with('-') {
                                -1
                            } else if l
                                .split_whitespace()
                                .next()
                                .and_then(|v| v.trim_end_matches('%').parse::<f32>().ok())
                                .is_some_and(|v| v > 0.)
                            {
                                1
                            } else {
                                0
                            });
                        }
                        if let Some(l) = tag.attr(Ns::Style, "text-line-through-style") {
                            s.toggles.strike = Some(l != "none");
                        }
                    }
                }
                (Ns::Text, "list-style") => {
                    list = tag.attr(Ns::Style, "name").map(str::to_owned);
                }
                (Ns::Text, name) if name.starts_with("list-level-style-") => {
                    if let Some(l) = &list {
                        let level = tag
                            .attr(Ns::Text, "level")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, 16);
                        let levels = lists.entry(l.clone()).or_default();
                        if levels.len() < level {
                            levels.resize(level, false);
                        }
                        if let Some(value) = levels.get_mut(level - 1) {
                            *value = name == "list-level-style-number";
                        }
                    }
                }
                _ => {}
            },
            Ev::Close(Ns::Style, "style") => {
                if let Some((name, s)) = current.take() {
                    styles.insert(name, s);
                }
            }
            Ev::Close(Ns::Text, "list-style") => list = None,
            _ => {}
        }
        Ok(())
    });
}

fn odf_chain<'a>(styles: &'a HashMap<String, OdfStyle>, name: &str) -> Vec<&'a OdfStyle> {
    let mut out = vec![];
    let mut next = Some(name);
    while let Some(n) = next.take() {
        if out.len() >= 16 {
            break;
        }
        if let Some(s) = styles.get(n) {
            out.push(s);
            next = s.parent.as_deref();
        }
    }
    out
}
fn odf_toggles(styles: &HashMap<String, OdfStyle>, name: Option<&str>) -> Toggles {
    name.map(|n| {
        odf_chain(styles, n)
            .iter()
            .fold(Toggles::default(), |acc, s| acc.over(s.toggles))
    })
    .unwrap_or_default()
}

struct OdfPara {
    kind: ParaKind,
    style: Option<String>,
    spans: Vec<Span>,
}

fn odt(
    file: &mut (impl Read + Seek),
    listing: &Listing,
    cancelled: &dyn Fn() -> bool,
) -> Result<OfficeDocument, String> {
    if let Some(manifest) = part(file, listing, "META-INF/manifest.xml")?
        && manifest
            .windows(b"encryption-data".len())
            .any(|w| w == b"encryption-data")
    {
        return Err("The document is password-protected; its content cannot be shown".into());
    }
    let content = part(file, listing, "content.xml")?.ok_or("content.xml is missing")?;
    let mut styles = HashMap::new();
    let mut lists = HashMap::new();
    if let Some(x) = part(file, listing, "styles.xml")? {
        odf_styles(&x, &mut styles, &mut lists, cancelled);
    }
    odf_styles(&content, &mut styles, &mut lists, cancelled);

    let mut body = Body::new();
    let mut in_text = false;
    let mut paras: Vec<OdfPara> = vec![];
    let mut spans_style: Vec<Option<String>> = vec![];
    let mut links: Vec<Arc<str>> = vec![];
    // (list style, first paragraph of the current item still to come, identity)
    let mut list_stack: Vec<(Option<String>, bool, u64)> = vec![];
    let mut lists_seen = 0u64;
    let mut skip = 0usize;
    let mut comments = 0usize;
    let mut images = 0usize;
    let mut generated_text = 0usize;
    let mut tracked = false;
    let mut note_label: Vec<String> = vec![];
    let mut in_citation = false;
    let mut frame_name: Option<String> = None;
    let mut frame_part: Option<String> = None;
    let mut in_frame = false;
    let mut cell_repeat: Vec<usize> = vec![];
    let mut row_repeat: Vec<usize> = vec![];

    let style_now = |paras: &[OdfPara], spans_style: &[Option<String>], links: &[Arc<str>]| {
        let para = odf_toggles(&styles, paras.last().and_then(|p| p.style.as_deref()));
        let t = spans_style
            .iter()
            .rev()
            .fold(Toggles::default(), |acc, s| {
                acc.over(odf_toggles(&styles, s.as_deref()))
            })
            .over(para);
        Style {
            strong: t.bold.unwrap_or(false),
            emphasis: t.italic.unwrap_or(false),
            strike: t.strike.unwrap_or(false),
            underline: t.underline.unwrap_or(false),
            position: t.position.unwrap_or(0),
            link: links.last().cloned(),
            ..Style::default()
        }
    };

    let result = walk(&content, cancelled, |ev| {
        match ev {
            Ev::Open(tag) => {
                if !in_text {
                    in_text = tag.is(Ns::Office, "text");
                    return Ok(());
                }
                if skip > 0 {
                    skip += 1;
                    if in_frame && tag.is(Ns::Draw, "image") {
                        frame_part = tag
                            .attr(Ns::Xlink, "href")
                            .filter(|p| !p.contains(':') && !p.starts_with('/'))
                            .map(|p| resolve_target("content.xml", p));
                    }
                    return Ok(());
                }
                match (tag.ns, tag.name.as_str()) {
                    (Ns::Office, "annotation") => {
                        comments += 1;
                        skip = 1;
                    }
                    (Ns::Text, "tracked-changes") => {
                        tracked = true;
                        skip = 1;
                    }
                    (Ns::Draw, "frame") => {
                        images += 1;
                        in_frame = true;
                        frame_part = None;
                        frame_name = tag.attr(Ns::Draw, "name").map(str::to_owned);
                        skip = 1;
                    }
                    // Other shapes, and the templates of generated indexes (their bodies show).
                    (Ns::Draw, _) => skip = 1,
                    (Ns::Text, name) if name.ends_with("-source") => skip = 1,
                    (Ns::Text, "h") => {
                        let level = tag
                            .attr(Ns::Text, "outline-level")
                            .and_then(|v| v.parse::<u8>().ok())
                            .unwrap_or(1)
                            .clamp(1, 6);
                        paras.push(OdfPara {
                            kind: ParaKind::Heading(level),
                            style: tag.attr(Ns::Text, "style-name").map(str::to_owned),
                            spans: vec![],
                        });
                    }
                    (Ns::Text, "p") => {
                        let style = tag.attr(Ns::Text, "style-name").map(str::to_owned);
                        let kind = if let Some(depth) = list_stack.len().checked_sub(1) {
                            let ordered = list_stack
                                .iter()
                                .rev()
                                .find_map(|(s, _, _)| s.as_ref())
                                .and_then(|s| lists.get(s))
                                .and_then(|l| l.get(depth))
                                .copied()
                                .unwrap_or(false);
                            let (first, list) = list_stack
                                .last_mut()
                                .map_or((true, 0), |l| (std::mem::take(&mut l.1), l.2));
                            ParaKind::Item {
                                level: depth,
                                ordered,
                                new_item: first,
                                list,
                                restart: false,
                            }
                        } else {
                            let chain = style
                                .as_deref()
                                .map(|s| odf_chain(&styles, s))
                                .unwrap_or_default();
                            let own = chain.first().map(|s| s.display.to_ascii_lowercase());
                            if own.as_deref().is_some_and(|n| {
                                n == "preformatted text" || n.contains("source code")
                            }) {
                                ParaKind::Code
                            } else if own.as_deref() == Some("title") {
                                ParaKind::Heading(1)
                            } else if own.is_some_and(|n| n.contains("quot") || n == "block text") {
                                ParaKind::Quote
                            } else {
                                chain
                                    .iter()
                                    .find_map(|s| {
                                        s.display
                                            .to_ascii_lowercase()
                                            .strip_prefix("heading ")
                                            .and_then(|n| n.parse::<u8>().ok())
                                    })
                                    .map_or(ParaKind::Normal, |n| ParaKind::Heading(n.clamp(1, 6)))
                            }
                        };
                        paras.push(OdfPara {
                            kind,
                            style,
                            spans: vec![],
                        });
                    }
                    (Ns::Text, "span") => {
                        spans_style.push(tag.attr(Ns::Text, "style-name").map(str::to_owned))
                    }
                    (Ns::Text, "a") => {
                        links.push(Arc::from(tag.attr(Ns::Xlink, "href").unwrap_or("")))
                    }
                    (Ns::Text, "s") => {
                        let n = tag
                            .attr(Ns::Text, "c")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(1);
                        if n > 1000 {
                            body.builder.truncated = true;
                        }
                        let n = n.min(1000);
                        generated_text = generated_text.saturating_add(n);
                        if generated_text > MAX_TEXT {
                            return Err(LIMIT.into());
                        }
                        let style = style_now(&paras, &spans_style, &links);
                        if let Some(p) = paras.last_mut() {
                            push_text(&mut p.spans, &" ".repeat(n), &style);
                        }
                    }
                    (Ns::Text, "tab") => {
                        let style = style_now(&paras, &spans_style, &links);
                        if let Some(p) = paras.last_mut() {
                            push_text(&mut p.spans, "\t", &style);
                        }
                    }
                    (Ns::Text, "line-break") => {
                        if let Some(p) = paras.last_mut() {
                            push_text(&mut p.spans, "\n", &Style::default());
                        }
                    }
                    (Ns::Text, "list") => {
                        let style = tag.attr(Ns::Text, "style-name").map(str::to_owned);
                        lists_seen += 1;
                        list_stack.push((style, false, lists_seen));
                    }
                    (Ns::Text, "list-item" | "list-header") => {
                        if let Some(l) = list_stack.last_mut() {
                            l.1 = true;
                        }
                    }
                    (Ns::Text, "note") => note_label.push(String::new()),
                    (Ns::Text, "note-citation") => in_citation = true,
                    (Ns::Text, "note-body") => body.sinks.push(Sink::Note(vec![])),
                    (Ns::Table, "table") => body.table_start(true),
                    (Ns::Table, "table-row") => {
                        row_repeat.push(
                            tag.attr(Ns::Table, "number-rows-repeated")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(1),
                        );
                        body.row_start();
                    }
                    (Ns::Table, "table-cell" | "covered-table-cell") => {
                        cell_repeat.push(
                            tag.attr(Ns::Table, "number-columns-repeated")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(1),
                        );
                        let span = tag
                            .attr(Ns::Table, "number-columns-spanned")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(1);
                        body.cell_start(span.saturating_sub(1));
                        if let Some(Sink::Table(t)) = body.sinks.last_mut() {
                            t.rowspan = tag
                                .attr(Ns::Table, "number-rows-spanned")
                                .and_then(|v| v.parse::<usize>().ok())
                                .unwrap_or(1)
                                .clamp(1, MAX_REPEAT);
                            t.covered = tag.name == "covered-table-cell";
                        }
                    }
                    _ => {}
                }
            }
            Ev::Close(ns, name) => {
                if !in_text {
                    return Ok(());
                }
                if ns == Ns::Office && name == "text" && skip == 0 {
                    in_text = false;
                    return Ok(());
                }
                if skip > 0 {
                    skip -= 1;
                    if skip == 0
                        && (ns, name) == (Ns::Draw, "frame")
                        && let Some(p) = paras.last_mut()
                    {
                        let mut span = placeholder(match frame_name.take() {
                            Some(n) => format!("[image: {n}]"),
                            None => "[image]".to_owned(),
                        });
                        span.style.image_part = frame_part.take().map(Arc::from);
                        p.spans.push(span);
                        in_frame = false;
                    }
                    return Ok(());
                }
                match (ns, name) {
                    (Ns::Text, "h" | "p") => {
                        if let Some(p) = paras.pop() {
                            body.paragraph(p.kind, p.spans);
                        }
                    }
                    (Ns::Text, "span") => {
                        spans_style.pop();
                    }
                    (Ns::Text, "a") => {
                        links.pop();
                    }
                    (Ns::Text, "list") => {
                        list_stack.pop();
                    }
                    (Ns::Text, "note-citation") => in_citation = false,
                    (Ns::Text, "note-body") => {
                        if let Some(Sink::Note(spans)) = body.sinks.pop() {
                            let label = note_label.last().cloned().unwrap_or_default();
                            body.notes.push((label, spans));
                        }
                    }
                    (Ns::Text, "note") => {
                        note_label.pop();
                    }
                    (Ns::Table, "table-cell" | "covered-table-cell") => {
                        body.cell_end(cell_repeat.pop().unwrap_or(1))
                    }
                    (Ns::Table, "table-row") => body.row_end(row_repeat.pop().unwrap_or(1)),
                    (Ns::Table, "table") => body.table_end(),
                    _ => {}
                }
            }
            Ev::Text(text) => {
                if !in_text || skip > 0 {
                    return Ok(());
                }
                if in_citation {
                    if let Some(l) = note_label.last_mut() {
                        l.push_str(text);
                    }
                    if let Some(p) = paras.last_mut() {
                        push_text(
                            &mut p.spans,
                            &format!("[{text}]"),
                            &Style {
                                note: true,
                                ..Style::default()
                            },
                        );
                    }
                    return Ok(());
                }
                // ODF collapses white space: runs become one space, none at a paragraph start
                // (explicit spaces, tabs and breaks are elements).
                let style = style_now(&paras, &spans_style, &links);
                if let Some(p) = paras.last_mut() {
                    let mut collapsed = String::with_capacity(text.len());
                    let mut space = p.spans.last().is_none_or(|s| s.text.ends_with([' ', '\n']));
                    for c in text.chars() {
                        if matches!(c, ' ' | '\t' | '\n' | '\r') {
                            if !space {
                                collapsed.push(' ');
                            }
                            space = true;
                        } else {
                            collapsed.push(c);
                            space = false;
                        }
                    }
                    push_text(&mut p.spans, &collapsed, &style);
                }
            }
        }
        if body.full { Err(LIMIT.into()) } else { Ok(()) }
    });
    finish_walk(result, &mut body)?;
    for p in paras {
        body.paragraph(p.kind, p.spans);
    }
    while matches!(body.sinks.last(), Some(Sink::Table(_))) {
        body.table_end();
    }
    let content = body.finish();

    let mut warnings = vec![];
    if tracked {
        warnings.push("Has tracked changes: the current text is shown".to_owned());
    }
    if comments > 0 {
        warnings.push(format!("Has {comments} comments (not shown)"));
    }
    if images > 0 {
        warnings.push(format!(
            "{images} images, drawings or text frames; unsupported or external images are shown as placeholders"
        ));
    }
    if listing.entries.iter().any(|e| {
        let n = e.name.to_ascii_lowercase();
        n.starts_with("basic/") || n.starts_with("scripts/")
    }) {
        warnings.push("Contains macros; they are never run".to_owned());
    }
    finish(content, warnings)
}

fn word_position(tag: &Tag) -> i8 {
    match tag.attr(Ns::W, "val") {
        Some("superscript") => 1,
        Some("subscript") => -1,
        _ => 0,
    }
}

/// Resolve Word vertical continuations using column positions. ODF already supplies row spans.
/// Covered cells consume a physical column but emit no HTML cell.
fn merge_rows(rows: &mut [Vec<Cell>]) -> bool {
    let mut truncated = false;
    let mut previous: HashMap<usize, (usize, usize)> = HashMap::new();
    for r in 0..rows.len() {
        let mut next = HashMap::new();
        let mut column = 0usize;
        let count = rows.get(r).map_or(0, Vec::len);
        for c in 0..count {
            let Some(cell) = rows.get(r).and_then(|row| row.get(c)) else {
                continue;
            };
            let width = cell.colspan;
            let continuation = cell.continuation;
            let merge_start = cell.merge_start;
            if continuation && let Some(&(origin_r, origin_c)) = previous.get(&column) {
                let origin = rows.get_mut(origin_r).and_then(|row| row.get_mut(origin_c));
                if let Some(origin) = origin.filter(|o| o.colspan == width) {
                    if origin.rowspan >= MAX_REPEAT {
                        truncated = true;
                        column += width;
                        continue;
                    }
                    origin.rowspan += 1;
                    if let Some(cell) = rows.get_mut(r).and_then(|row| row.get_mut(c)) {
                        cell.covered = true;
                    }
                    next.insert(column, (origin_r, origin_c));
                }
            } else if merge_start {
                next.insert(column, (r, c));
            }
            // ODF covered cells are explicit; their preceding colspan must not advance twice.
            let odf_covered_follows = rows
                .get(r)
                .and_then(|row| row.get(c + 1))
                .is_some_and(|c| c.covered && !c.continuation);
            column += if odf_covered_follows { 1 } else { width };
        }
        previous = next;
    }
    truncated
}

fn span_bytes(spans: &[Span]) -> usize {
    spans.iter().fold(0usize, |n, s| {
        n.saturating_add(std::mem::size_of::<Span>())
            .saturating_add(s.text.len())
            .saturating_add(s.style.link.as_deref().map_or(0, str::len))
            .saturating_add(s.style.image_part.as_deref().map_or(0, str::len))
    })
}
fn finish_walk(result: Result<(), String>, body: &mut Body) -> Result<(), String> {
    match result {
        Err(e) if e == LIMIT => {
            body.builder.truncated = true;
            Ok(())
        }
        other => other,
    }
}

/// Package targets are URIs. Decoding only selects a ZIP member; it never forms a file path.
fn package_uri(uri: &str) -> String {
    let mut bytes = uri.as_bytes().iter().copied().peekable();
    let mut out = Vec::with_capacity(uri.len());
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let mut probe = bytes.clone();
            if let (Some(a), Some(b)) = (probe.next(), probe.next())
                && let (Some(a), Some(b)) = ((a as char).to_digit(16), (b as char).to_digit(16))
            {
                out.push((a * 16 + b) as u8);
                bytes = probe;
                continue;
            }
        }
        out.push(b);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Document properties of `docProps/core.xml`, `docProps/app.xml` or ODT `meta.xml`.
pub(crate) fn properties(xml: &[u8], cancelled: &dyn Fn() -> bool) -> Vec<(String, String)> {
    let label = |ns: Ns, name: &str, core: bool| -> Option<&'static str> {
        Some(match (ns, name) {
            // The author in OOXML core properties; the last editor in ODF metadata.
            (Ns::Dc, "creator") if core => "Author",
            (Ns::Dc, "title") => "Title",
            (Ns::Dc, "subject") => "Subject",
            (Ns::Dc, "description") => "Description",
            (Ns::Cp, "keywords") | (Ns::Meta, "keyword") => "Keywords",
            (Ns::Dc, "creator") => "Last modified by",
            (Ns::Cp, "lastModifiedBy") => "Last modified by",
            (Ns::Meta, "initial-creator") => "Author",
            (Ns::Dcterms, "created") | (Ns::Meta, "creation-date") => "Created",
            (Ns::Dcterms, "modified") | (Ns::Dc, "date") => "Modified",
            (Ns::Cp, "revision") | (Ns::Meta, "editing-cycles") => "Revision",
            (Ns::Ep, "Application") | (Ns::Meta, "generator") => "Application",
            (Ns::Ep, "Pages") => "Pages",
            (Ns::Ep, "Words") => "Words",
            (Ns::Ep, "Company") => "Company",
            (Ns::Ep, "Template") => "Template",
            _ => return None,
        })
    };
    let mut out: Vec<(String, String)> = vec![];
    let mut current: Option<(&'static str, String)> = None;
    let mut core = false;
    let _ = walk(xml, cancelled, |ev| {
        match ev {
            Ev::Open(tag) => {
                if tag.is(Ns::Cp, "coreProperties") {
                    core = true;
                }
                if tag.is(Ns::Meta, "document-statistic") {
                    for (attr, name) in [
                        ("page-count", "Pages"),
                        ("word-count", "Words"),
                        ("table-count", "Tables"),
                        ("image-count", "Images"),
                    ] {
                        if let Some(v) = tag.attr(Ns::Meta, attr) {
                            out.push((name.into(), v.to_owned()));
                        }
                    }
                }
                current = label(tag.ns, &tag.name, core).map(|l| (l, String::new()));
            }
            Ev::Text(t) => {
                if let Some((_, v)) = current.as_mut()
                    && v.len() < 4096
                {
                    v.push_str(t);
                }
            }
            Ev::Close(..) => {
                if let Some((label, value)) = current.take() {
                    let value = value.trim();
                    if !value.is_empty() && out.iter().all(|(k, _)| k != label) {
                        out.push((label.to_owned(), value.to_owned()));
                    }
                }
            }
        }
        Ok(())
    });
    out
}
