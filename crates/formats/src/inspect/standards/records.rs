//! Helpers shared by the record standards (FHIR, CDA, SARIF, CycloneDX, SPDX, HAR, Jupyter,
//! SDMX): bounded tables and outlines, JSON path shortcuts, an XML-to-JSON view of documents
//! whose XML and JSON forms say the same thing, and a bounded base64 decoder.
use crate::inspect::{
    Inspection, MAX_ITEMS, MAX_ROWS, OutlineNode, Row, Table,
    jsondoc::{Node, Value},
    spec::{json_value, select_json},
    xmldoc::Doc,
};
use std::ops::Range;

/// A table being filled: rows past [`MAX_ROWS`] are counted, not kept.
pub struct Rows {
    table: Table,
    seen: u64,
}
impl Rows {
    pub fn new(name: &str, columns: &[&str]) -> Self {
        Self {
            table: Table {
                name: name.into(),
                columns: columns.iter().map(|c| (*c).to_owned()).collect(),
                rows: vec![],
                total: None,
            },
            seen: 0,
        }
    }
    pub fn push(&mut self, cells: Vec<String>, source: Option<Range<u64>>) {
        self.seen += 1;
        if self.table.rows.len() < MAX_ROWS {
            self.table.rows.push(Row { cells, source });
        }
    }
    pub fn seen(&self) -> u64 {
        self.seen
    }
    /// Add the table to `out` when it has rows; columns empty in every row are dropped.
    pub fn finish(mut self, out: &mut Inspection) {
        if self.table.rows.is_empty() {
            return;
        }
        let keep: Vec<bool> = (0..self.table.columns.len())
            .map(|i| {
                self.table
                    .rows
                    .iter()
                    .any(|r| r.cells.get(i).is_some_and(|c| !c.is_empty()))
            })
            .collect();
        let filter = |cells: &mut Vec<String>| {
            let mut i = 0;
            cells.retain(|_| {
                i += 1;
                keep.get(i - 1).copied().unwrap_or(false)
            });
        };
        filter(&mut self.table.columns);
        for r in &mut self.table.rows {
            filter(&mut r.cells);
        }
        if self.seen > self.table.rows.len() as u64 {
            self.table.total = Some(self.seen);
        }
        out.tables.push(self.table);
    }
}

/// Add an outline entry (bounded by [`MAX_ITEMS`]).
pub fn outline(
    out: &mut Inspection,
    depth: u16,
    label: impl Into<String>,
    value: impl Into<String>,
    source: Option<Range<u64>>,
) {
    if out.outline.len() < MAX_ITEMS {
        out.outline.push(OutlineNode {
            depth,
            label: label.into(),
            value: value.into(),
            source,
        });
    }
}

pub fn span(n: &Node) -> Option<Range<u64>> {
    Some(n.span.clone())
}

/// Nodes at `path`, with arrays reached by the last step given as their items.
pub fn nodes<'a>(from: &'a Node, path: &str) -> Vec<&'a Node> {
    let mut out = vec![];
    for n in select_json(from, path) {
        match &n.value {
            Value::Array(items) => out.extend(items.iter()),
            _ => out.push(n),
        }
        if out.len() > MAX_ROWS {
            break;
        }
    }
    out
}

/// The value at `path` as text, or empty.
pub fn s(from: &Node, path: &str) -> String {
    json_value(from, path).map(|v| v.0).unwrap_or_default()
}

/// Every scalar value at `path`, joined.
pub fn all(from: &Node, path: &str, sep: &str) -> String {
    let mut parts: Vec<String> = vec![];
    for n in nodes(from, path) {
        if n.is_scalar() {
            let t = n.text();
            if !t.is_empty() && !parts.contains(&t) {
                parts.push(t);
            }
        }
        if parts.len() >= 50 {
            parts.push("…".into());
            break;
        }
    }
    parts.join(sep)
}

/// The first `max` characters, with an ellipsis when cut.
pub fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().nth(max).is_some() {
        out.push('…');
    }
    out
}

/// The first non-blank line, clipped.
pub fn first_line(text: &str, max: usize) -> String {
    clip(
        text.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or(""),
        max,
    )
}

/// Join non-empty parts.
pub fn join(parts: &[String], sep: &str) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(sep)
}

/// The string value of the first `"key": "value"` in `head` (for cheap recognition).
pub fn str_after<'a>(head: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{key}\"");
    let mut from = 0;
    while let Some(i) = head[from..].find(&pattern) {
        let rest = head[from + i + pattern.len()..].trim_start();
        if let Some(rest) = rest.strip_prefix(':') {
            let rest = rest.trim_start();
            let rest = rest.strip_prefix('"')?;
            let end = rest.find('"').unwrap_or(rest.len());
            return Some(&rest[..end]);
        }
        from += i + pattern.len();
    }
    None
}

/// Decode base64 (standard or URL alphabet; whitespace ignored). `None` when it is not base64
/// or would decode to more than `max` bytes.
pub fn base64(text: &str, max: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity((text.len() / 4 * 3).min(max));
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if out.len() >= max {
                return None;
            }
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// How an XML document is seen as JSON.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum XmlView {
    /// FHIR XML: `value` attributes are the values, a resource container holds one element
    /// named after the resource type, XHTML narrative is text.
    Fhir,
    /// Attributes and child elements are members; a plural wrapper of like elements
    /// (`<components><component/>…</components>`) is an array; text beside attributes is `#text`.
    Plain,
}

/// The element `e` of `doc` as a JSON node (bounded by the document's own limits).
pub fn xml_to_node(doc: &Doc, e: u32, view: XmlView) -> Node {
    convert(doc, e, view, 0)
}

/// Whether `item` is the singular of `plural` ("components"/"component",
/// "dependencies"/"dependency", "hashes"/"hash").
fn is_singular_of(item: &str, plural: &str) -> bool {
    !item.is_empty()
        && (plural
            .strip_suffix("ies")
            .is_some_and(|stem| item.strip_suffix('y') == Some(stem))
            || plural.strip_suffix("es") == Some(item)
            || plural.strip_suffix('s') == Some(item))
}

fn convert(doc: &Doc, e: u32, view: XmlView, depth: usize) -> Node {
    let el = doc.get(e);
    let span = el.span.clone();
    if depth > 200 {
        return Node {
            value: Value::Null,
            span,
        };
    }
    let starts_upper = |i: u32| {
        doc.get(i)
            .name
            .starts_with(|c: char| c.is_ascii_uppercase())
    };
    if view == XmlView::Fhir {
        if el.name == "div" && el.ns.contains("xhtml") {
            return Node {
                value: Value::String(flatten(doc, e, 2000)),
                span,
            };
        }
        // A resource container (`resource`, `contained`, …) or the root: the resource's
        // members with its type.
        if el.children.len() == 1 && starts_upper(el.children[0]) && el.attrs.is_empty() {
            return convert(doc, el.children[0], view, depth + 1);
        }
    }
    if view == XmlView::Plain
        && !el.children.is_empty()
        && el.attrs.is_empty()
        && el
            .children
            .iter()
            .all(|&c| is_singular_of(&doc.get(c).name, &el.name))
    {
        return Node {
            value: Value::Array(
                el.children
                    .iter()
                    .map(|&c| convert(doc, c, view, depth + 1))
                    .collect(),
            ),
            span,
        };
    }
    let mut fields: Vec<(String, Node)> = vec![];
    if view == XmlView::Fhir && starts_upper(e) {
        fields.push((
            "resourceType".into(),
            Node {
                value: Value::String(el.name.clone()),
                span: span.clone(),
            },
        ));
    }
    for (k, v) in &el.attrs {
        if k == "xmlns" || k == "schemaLocation" || (view == XmlView::Fhir && k == "value") {
            continue;
        }
        fields.push((
            k.clone(),
            Node {
                value: Value::String(v.clone()),
                span: span.clone(),
            },
        ));
    }
    let mut seen: Vec<String> = vec![];
    for &c in &el.children {
        let name = &doc.get(c).name;
        if seen.contains(name) {
            continue;
        }
        seen.push(name.clone());
        let same: Vec<u32> = el
            .children
            .iter()
            .copied()
            .filter(|&x| &doc.get(x).name == name)
            .collect();
        let as_value = |x: u32| -> Node {
            let child = doc.get(x);
            if view == XmlView::Fhir
                && let Some(v) = doc.attr(x, "value")
            {
                return Node {
                    value: Value::String(v.to_owned()),
                    span: child.span.clone(),
                };
            }
            convert(doc, x, view, depth + 1)
        };
        let node = if same.len() == 1 {
            as_value(same[0])
        } else {
            Node {
                value: Value::Array(same.iter().map(|&x| as_value(x)).collect()),
                span: span.clone(),
            }
        };
        fields.push((name.clone(), node));
        // FHIR primitives with extensions keep them under `_name`, as in JSON.
        if view == XmlView::Fhir
            && same.len() == 1
            && doc.attr(same[0], "value").is_some()
            && !doc.get(same[0]).children.is_empty()
        {
            let mut ext = convert(doc, same[0], view, depth + 1);
            if let Value::Object(f) = &mut ext.value {
                f.retain(|(k, _)| k != "value");
            }
            fields.push((format!("_{name}"), ext));
        }
    }
    let text = el.text.trim();
    if fields.is_empty() && el.children.is_empty() {
        return Node {
            value: if text.is_empty() && view == XmlView::Fhir {
                Value::Object(vec![])
            } else {
                Value::String(collapse(text))
            },
            span,
        };
    }
    if !text.is_empty() {
        fields.push((
            "#text".into(),
            Node {
                value: Value::String(collapse(text)),
                span: span.clone(),
            },
        ));
    }
    Node {
        value: Value::Object(fields),
        span,
    }
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// All text under `e` in document order (own text before children), collapsed, at most
/// `max` characters.
pub fn flatten(doc: &Doc, e: u32, max: usize) -> String {
    let mut out = String::new();
    let mut stack = vec![e];
    let mut visited = 0usize;
    while let Some(i) = stack.pop() {
        visited += 1;
        if visited > 100_000 || out.len() > max * 4 + 16 {
            break;
        }
        let el = doc.get(i);
        let t = el.text.trim();
        if !t.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(t);
        }
        stack.extend(el.children.iter().rev().copied());
    }
    clip(&collapse(&out), max)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_and_helpers() {
        assert_eq!(base64("aGVs\nbG8=", 100).unwrap(), b"hello");
        assert_eq!(base64("aGVsbG8", 100).unwrap(), b"hello");
        assert!(base64("aGVsbG8=", 3).is_none());
        assert!(base64("a$b", 10).is_none());
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(first_line("\n  x y \nz", 10), "x y");
        assert_eq!(
            str_after(r#"{"a": {"bomFormat" : "CycloneDX"}}"#, "bomFormat"),
            Some("CycloneDX")
        );
    }
    #[test]
    fn xml_as_json() {
        let src = r#"<bom xmlns="urn:x"><components><component type="library"><name>a</name><hashes><hash alg="SHA-256">00</hash></hashes></component></components></bom>"#;
        let doc = Doc::parse(src, &|| false).unwrap();
        let n = xml_to_node(&doc, 0, XmlView::Plain);
        assert_eq!(s(&n, "components/name"), "a");
        assert_eq!(s(&n, "components/type"), "library");
        assert_eq!(nodes(&n, "components").len(), 1);
        assert_eq!(s(&n, "components/hashes/#text"), "00");
        assert!(is_singular_of("dependency", "dependencies") && is_singular_of("hash", "hashes"));
        assert!(!is_singular_of("target", "affects") && !is_singular_of("", "s"));
        let src = r#"<Bundle xmlns="http://hl7.org/fhir"><type value="collection"/><entry><resource><Patient><id value="p1"/><name><given value="A"/><given value="B"/></name></Patient></resource></entry></Bundle>"#;
        let doc = Doc::parse(src, &|| false).unwrap();
        let n = xml_to_node(&doc, 0, XmlView::Fhir);
        assert_eq!(s(&n, "resourceType"), "Bundle");
        assert_eq!(s(&n, "type"), "collection");
        assert_eq!(s(&n, "entry/resource/resourceType"), "Patient");
        assert_eq!(s(&n, "entry/resource/name/given"), "A, B");
    }
}
