//! A small read-only XML document for standards: elements with local names, resolved
//! namespaces, attributes, text and byte spans. Built with quick-xml under limits; DTDs and
//! entities other than the predefined and character references are never expanded.
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
use std::ops::Range;

/// Elements kept; larger documents are refused.
pub const MAX_ELEMENTS: usize = 4_000_000;
pub const MAX_DEPTH: usize = 256;

#[derive(Clone, Debug, Default)]
pub struct Element {
    pub name: String,
    pub ns: String,
    /// Attributes by local name (namespace dropped), values unescaped.
    pub attrs: Vec<(String, String)>,
    /// Text directly inside this element (not its children's), unescaped.
    pub text: String,
    pub children: Vec<u32>,
    pub parent: Option<u32>,
    /// From the start tag's `<` to the end tag's `>`.
    pub span: Range<u64>,
}

#[derive(Debug, Default)]
pub struct Doc {
    pub elements: Vec<Element>,
    /// Elements still open at the end of the text (a cut file); empty when complete.
    pub unclosed: Vec<String>,
}

impl Doc {
    pub fn parse(source: &str, cancelled: &dyn Fn() -> bool) -> Result<Doc, String> {
        let mut reader = NsReader::from_str(source);
        reader.config_mut().check_end_names = true;
        let mut doc = Doc::default();
        let mut stack: Vec<u32> = vec![];
        loop {
            if doc.elements.len() % 65536 == 0 && cancelled() {
                return Err("Cancelled".into());
            }
            let start = reader.buffer_position();
            let (ns, event) = match reader.read_resolved_event() {
                Ok((resolved, event)) => (
                    match resolved {
                        ResolveResult::Bound(ns) => {
                            String::from_utf8_lossy(ns.as_ref()).into_owned()
                        }
                        _ => String::new(),
                    },
                    event,
                ),
                Err(e) => return Err(format!("XML error near byte {start}: {e}")),
            };
            let end = reader.buffer_position();
            match event {
                Event::Start(tag) | Event::Empty(tag) => {
                    let empty = end > 1 && source.as_bytes().get(end as usize - 2) == Some(&b'/');
                    if doc.elements.len() >= MAX_ELEMENTS {
                        return Err(format!("More than {MAX_ELEMENTS} elements"));
                    }
                    if stack.len() >= MAX_DEPTH {
                        return Err(format!("Elements nest deeper than {MAX_DEPTH}"));
                    }
                    let name = String::from_utf8_lossy(tag.local_name().as_ref()).into_owned();
                    let attrs = tag
                        .attributes()
                        .with_checks(false)
                        .flatten()
                        .map(|a| {
                            let key =
                                String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
                            let value = a
                                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                .map(|v| v.into_owned())
                                .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned());
                            (key, value)
                        })
                        .collect();
                    let index = doc.elements.len() as u32;
                    let parent = stack.last().copied();
                    if let Some(p) = parent {
                        doc.elements[p as usize].children.push(index);
                    }
                    doc.elements.push(Element {
                        name,
                        ns,
                        attrs,
                        text: String::new(),
                        children: vec![],
                        parent,
                        span: start..end,
                    });
                    if !empty {
                        stack.push(index);
                    }
                }
                Event::End(_) => {
                    if let Some(i) = stack.pop() {
                        doc.elements[i as usize].span.end = end;
                    }
                }
                Event::Text(t) => {
                    if let Some(&i) = stack.last() {
                        let text = t.decode().map_err(|e| e.to_string())?;
                        doc.elements[i as usize].text.push_str(&text);
                    }
                }
                Event::CData(t) => {
                    if let Some(&i) = stack.last() {
                        doc.elements[i as usize]
                            .text
                            .push_str(&String::from_utf8_lossy(&t));
                    }
                }
                Event::GeneralRef(r) => {
                    if let Some(&i) = stack.last() {
                        let name = String::from_utf8_lossy(&r).into_owned();
                        doc.elements[i as usize].text.push_str(&entity(&name));
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        if doc.elements.is_empty() {
            return Err("No XML elements".into());
        }
        doc.unclosed = stack
            .iter()
            .map(|&i| doc.elements[i as usize].name.clone())
            .collect();
        for &i in &stack {
            doc.elements[i as usize].span.end = source.len() as u64;
        }
        Ok(doc)
    }

    pub fn root(&self) -> &Element {
        &self.elements[0]
    }
    pub fn get(&self, index: u32) -> &Element {
        &self.elements[index as usize]
    }
    /// Text of the element: its own text, trimmed; when it has none, its descendants' text.
    pub fn text(&self, index: u32) -> String {
        let e = self.get(index);
        let own = e.text.trim();
        if !own.is_empty() || e.children.is_empty() {
            return collapse(own);
        }
        let mut parts = vec![];
        self.collect_text(index, &mut parts, 0);
        collapse(&parts.join(" "))
    }
    fn collect_text(&self, index: u32, parts: &mut Vec<String>, depth: usize) {
        if depth > 64 || parts.len() > 1000 {
            return;
        }
        let e = self.get(index);
        if !e.text.trim().is_empty() {
            parts.push(e.text.trim().to_owned());
        }
        for &c in &e.children {
            self.collect_text(c, parts, depth + 1);
        }
    }
    pub fn attr(&self, index: u32, name: &str) -> Option<&str> {
        self.get(index)
            .attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    /// Descendants of `index` in document order.
    pub fn descendants(&self, index: u32) -> Vec<u32> {
        let mut out = vec![];
        let mut stack: Vec<u32> = self.get(index).children.iter().rev().copied().collect();
        while let Some(i) = stack.pop() {
            out.push(i);
            stack.extend(self.get(i).children.iter().rev().copied());
        }
        out
    }
    /// Namespace URIs declared or used, root first.
    pub fn namespaces(&self) -> Vec<&str> {
        let mut seen: Vec<&str> = vec![];
        for e in &self.elements {
            if !e.ns.is_empty() && !seen.contains(&e.ns.as_str()) {
                seen.push(&e.ns);
                if seen.len() > 64 {
                    break;
                }
            }
        }
        seen
    }
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The predefined entities and character references; anything else stays as written.
fn entity(name: &str) -> String {
    match name {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        n if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16)
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_else(|| format!("&{n};")),
        n if n.starts_with('#') => n[1..]
            .parse()
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_else(|| format!("&{n};")),
        n => format!("&{n};"),
    }
}

/// Local name and namespace of the root element, read from the start of the file only.
pub fn root_of(head: &str) -> Option<(String, String)> {
    let mut reader = NsReader::from_str(head);
    for _ in 0..10_000 {
        match reader.read_resolved_event() {
            Ok((resolved, Event::Start(tag) | Event::Empty(tag))) => {
                let ns = match resolved {
                    ResolveResult::Bound(ns) => String::from_utf8_lossy(ns.as_ref()).into_owned(),
                    _ => String::new(),
                };
                return Some((
                    String::from_utf8_lossy(tag.local_name().as_ref()).into_owned(),
                    ns,
                ));
            }
            Ok((_, Event::Eof)) | Err(_) => return None,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn elements_namespaces_text_and_spans() {
        let src = r#"<?xml version="1.0"?><d:Doc xmlns:d="urn:x" xmlns="urn:y"><A n="1 &amp; 2">x &lt; y<B/>z</A><![CDATA[c]]></d:Doc>"#;
        let doc = Doc::parse(src, &|| false).unwrap();
        assert_eq!(doc.root().name, "Doc");
        assert_eq!(doc.root().ns, "urn:x");
        let a = doc.root().children[0];
        assert_eq!(doc.get(a).ns, "urn:y");
        assert_eq!(doc.attr(a, "n"), Some("1 & 2"));
        assert_eq!(doc.text(a), "x < yz");
        let span = doc.get(a).span.clone();
        assert!(src[span.start as usize..span.end as usize].starts_with("<A n="));
        assert!(src[span.start as usize..span.end as usize].ends_with("</A>"));
        let b = doc.get(a).children[0];
        assert_eq!(
            &src[doc.get(b).span.start as usize..doc.get(b).span.end as usize],
            "<B/>"
        );
        assert_eq!(doc.text(0), "c");
        assert_eq!(root_of(src), Some(("Doc".into(), "urn:x".into())));
        assert!(Doc::parse("<a><b></a>", &|| false).is_err());
        assert!(Doc::parse("", &|| false).is_err());
        let deep = "<a>".repeat(300) + &"</a>".repeat(300);
        assert!(Doc::parse(&deep, &|| false).is_err());
    }
}
