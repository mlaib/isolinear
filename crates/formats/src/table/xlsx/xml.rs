//! Namespace-aware XML and package relationships. No external references are followed.
use super::zip::{self as archive, Listing};
use quick_xml::{NsReader, events::Event, name::ResolveResult};
use std::{
    collections::HashMap,
    io::{Read, Seek},
};
const PART_LIMIT: u64 = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 256;
fn has(listing: &Listing, name: &str) -> bool {
    listing
        .entries
        .iter()
        .any(|e| e.name.eq_ignore_ascii_case(name))
}

/// One part's bytes; `Ok(None)` when the archive has no such entry. Part names are
/// case-insensitive in OOXML.
pub(super) fn part(
    file: &mut (impl Read + Seek),
    listing: &Listing,
    name: &str,
) -> Result<Option<Vec<u8>>, String> {
    let Some(entry) = listing
        .entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(name))
    else {
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
pub(super) enum Ns {
    S,
    R,
    Rel,
    Dc,
    Meta,
    Cp,
    Dcterms,
    Ep,
    Other,
}
fn ns_of(uri: &[u8]) -> Ns {
    match uri {
        b"http://schemas.openxmlformats.org/spreadsheetml/2006/main"
        | b"http://purl.oclc.org/ooxml/spreadsheetml/main" => Ns::S,
        b"http://schemas.openxmlformats.org/officeDocument/2006/relationships"
        | b"http://purl.oclc.org/ooxml/officeDocument/relationships" => Ns::R,
        b"http://schemas.openxmlformats.org/package/2006/relationships" => Ns::Rel,
        b"http://purl.org/dc/elements/1.1/" => Ns::Dc,
        b"urn:oasis:names:tc:opendocument:xmlns:meta:1.0" => Ns::Meta,
        b"http://schemas.openxmlformats.org/package/2006/metadata/core-properties" => Ns::Cp,
        b"http://purl.org/dc/terms/" => Ns::Dcterms,
        b"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"
        | b"http://purl.oclc.org/ooxml/officeDocument/extendedProperties" => Ns::Ep,
        _ => Ns::Other,
    }
}

pub(super) struct Tag {
    pub(super) ns: Ns,
    pub(super) name: String,
    attrs: Vec<(Ns, String, String)>,
}
impl Tag {
    pub(super) fn is(&self, ns: Ns, name: &str) -> bool {
        self.ns == ns && self.name == name
    }
    pub(super) fn attr(&self, ns: Ns, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, l, _)| *n == ns && l == name)
            .map(|(_, _, v)| v.as_str())
    }
}

pub(super) enum Ev<'a> {
    Open(&'a Tag),
    Close(Ns, &'a str),
    Text(&'a str),
}

/// Walk `xml`, calling `f` for every element start, end and piece of text (entity and character
/// references resolved; only XML's predefined entities exist, nothing is expanded or fetched).
pub(super) fn walk(
    xml: &[u8],
    cancelled: &dyn Fn() -> bool,
    mut f: impl FnMut(Ev) -> Result<(), String>,
) -> Result<(), String> {
    let mut reader = NsReader::from_reader(xml);
    reader.config_mut().expand_empty_elements = true;
    let mut open: Vec<(Ns, String)> = vec![];
    let mut events = 0u64;
    loop {
        events += 1;
        if (events == 1 || events.is_multiple_of(4096)) && cancelled() {
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
                    if attrs.len() >= 4096 {
                        return Err("An XML element exceeds 4,096 attributes".into());
                    }
                    let a = a.map_err(|e| format!("Malformed XML attribute: {e}"))?;
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
                let (ns, name) = open.pop().ok_or("Unexpected XML closing element")?;
                f(Ev::Close(ns, &name))?;
            }
            Event::Text(t) => {
                let text = t.decode().map_err(|e| e.to_string())?;
                f(Ev::Text(&text))?;
            }
            Event::CData(t) => {
                let text = t.decode().map_err(|e| e.to_string())?;
                f(Ev::Text(&text))?;
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
                f(Ev::Text(&text))?;
            }
            Event::DocType(_) => {
                return Err("XML document types and entity declarations are not supported".into());
            }
            Event::Eof if !open.is_empty() => {
                return Err("Unclosed XML element at the end of the part".into());
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// The part whose `[Content_Types].xml` override contains one of `types`, else `fallback` when
/// the archive has it.
pub(super) fn main_part_by_type(
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

pub(super) fn relationship_ids(xml: &[u8]) -> HashMap<String, (String, String, bool)> {
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
pub(super) fn rels_of(name: &str) -> String {
    match name.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{name}.rels"),
    }
}
pub(super) fn resolve_target(base: &str, target: &str) -> String {
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

/// Document properties of `docProps/core.xml`, `docProps/app.xml` or ODT `meta.xml`.
pub(super) fn properties(xml: &[u8], cancelled: &dyn Fn() -> bool) -> Vec<(String, String)> {
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
