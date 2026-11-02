//! XML over retained source bytes. Entities, DTDs and external references are never processed.

use std::sync::atomic::{AtomicBool, Ordering};

use quick_xml::{
    events::{BytesStart, Event},
    name::ResolveResult,
    reader::NsReader,
};

use super::{Builder, Kind, Limits, NodeId, Scalar, Span, Stop, Stopped, Tree, TreeFormat};

pub const FORMAT: Option<&'static dyn TreeFormat> = Some(&Xml);

pub struct Xml;

impl TreeFormat for Xml {
    fn parse(&self, source: &str, limits: &Limits, cancel: &AtomicBool) -> Tree {
        let mut b = Builder::new(source.len(), limits, cancel);
        let mut pos = 0;
        let result = document(source, &mut b, cancel, &mut pos);
        if let Err(Halt::Syntax(span, message)) = &result {
            b.fail(span.clone(), message.clone());
        }
        let mut tree = b.finish(pos as u32);
        if matches!(result, Err(Halt::Cancelled)) {
            tree.stopped = Some(Stop::Cancelled);
        }
        tree
    }
}

enum Halt {
    Syntax(Span, String),
    Stopped,
    Cancelled,
}

impl From<Stopped> for Halt {
    fn from(_: Stopped) -> Self {
        Self::Stopped
    }
}

fn syntax(at: usize, message: impl Into<String>) -> Halt {
    Halt::Syntax(at as u32..at as u32, message.into())
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), Halt> {
    if cancel.load(Ordering::Relaxed) {
        Err(Halt::Cancelled)
    } else {
        Ok(())
    }
}

fn document(
    source: &str,
    b: &mut Builder<'_>,
    cancel: &AtomicBool,
    pos: &mut usize,
) -> Result<(), Halt> {
    // Account for the BOM ourselves: reader offsets are relative to its input.
    let input = source.strip_prefix('\u{feff}').unwrap_or(source);
    let base = source.len() - input.len();
    if input.starts_with('\u{feff}') {
        return Err(syntax(base, "Text outside the root element"));
    }
    let mut reader = NsReader::from_str(input);
    reader.config_mut().check_comments = true;
    let mut roots = 0;
    let mut skipped_blank = None;
    let mut last_text: Option<NodeId> = None;
    loop {
        check_cancel(cancel)?;
        let start = base + reader.buffer_position() as usize;
        let event = reader.read_event().map_err(|e| {
            // Namespace resolution can fail after tokenization without setting error_position.
            *pos = (base + reader.error_position() as usize).max(start);
            syntax(*pos, e.to_string())
        })?;
        let end = base + reader.buffer_position() as usize;
        *pos = end;
        let span = start as u32..end as u32;
        let (kind, name, value) = match &event {
            Event::Eof => {
                if b.depth() > 0 {
                    let name = b
                        .node(b.current())
                        .name
                        .as_ref()
                        .and_then(|s| source.get(s.start as usize..s.end as usize))
                        .unwrap_or("element");
                    return Err(syntax(
                        end,
                        format!("Unclosed element <{name}> at end of file"),
                    ));
                }
                if roots == 0 {
                    return Err(syntax(end, "No root element"));
                }
                return Ok(());
            }
            Event::End(_) => {
                b.close(end as u32);
                last_text = None;
                skipped_blank = None;
                continue;
            }
            Event::Start(tag) | Event::Empty(tag) => {
                last_text = None;
                skipped_blank = None;
                if b.depth() == 0 {
                    roots += 1;
                    if roots > 1 {
                        return Err(syntax(start, "Multiple root elements"));
                    }
                }
                let name_len = tag.name().as_ref().len();
                let token = source
                    .get(start..end)
                    .ok_or_else(|| syntax(start, "Invalid element span"))?;
                let tag = checked_tag(token, name_len)
                    .ok_or_else(|| syntax(start, "Invalid element name span"))?;
                // Validate the whole tag before emitting it, retaining the old parser's prefix.
                for attr in tag.attributes() {
                    check_cancel(cancel)?;
                    attr.map_err(|e| syntax(start, format!("Attribute error: {e}")))?;
                }
                if let ResolveResult::Unknown(prefix) =
                    reader.resolver().resolve_element(tag.name()).0
                {
                    return Err(syntax(
                        start,
                        format!(
                            "Undeclared namespace prefix {:?}",
                            String::from_utf8_lossy(&prefix)
                        ),
                    ));
                }
                b.open(
                    Kind::Element,
                    start as u32,
                    Some((start + 1) as u32..(start + 1 + name_len) as u32),
                )?;
                attributes(source, start + 1 + name_len, end, b, cancel)?;
                if matches!(event, Event::Empty(_)) {
                    b.close(end as u32);
                }
                continue;
            }
            Event::Text(text) => {
                let blank = text.as_ref().iter().all(u8::is_ascii_whitespace);
                if b.depth() == 0 && !blank {
                    return Err(syntax(start, "Text outside the root element"));
                }
                if blank {
                    skipped_blank = Some(span);
                    continue;
                }
                (Kind::Text, None, None)
            }
            Event::GeneralRef(_) => {
                if b.depth() == 0 {
                    return Err(syntax(start, "Entity reference outside the root element"));
                }
                (Kind::Text, None, None)
            }
            Event::CData(_) => (
                Kind::CData,
                None,
                Some((start + 9) as u32..(end - 3) as u32),
            ),
            Event::Comment(_) => (
                Kind::Comment,
                None,
                Some((start + 4) as u32..(end - 3) as u32),
            ),
            Event::PI(pi) => {
                let name_end = start + 2 + pi.target().len();
                (
                    Kind::ProcessingInstruction,
                    Some((start + 2) as u32..name_end as u32),
                    Some(name_end as u32..(end - 2) as u32),
                )
            }
            Event::Decl(decl) => {
                if let Some(Ok(encoding)) = decl.encoding() {
                    let enc = String::from_utf8_lossy(&encoding).to_ascii_lowercase();
                    if !matches!(
                        enc.as_str(),
                        "utf-8"
                            | "utf8"
                            | "utf-16"
                            | "utf-16le"
                            | "utf-16be"
                            | "iso-8859-1"
                            | "latin1"
                            | "windows-1252"
                            | "us-ascii"
                            | "ascii"
                    ) {
                        b.warning(span.clone(), format!("Declared encoding {enc:?} is not supported; the file was decoded as shown in File info"));
                    }
                }
                (
                    Kind::ProcessingInstruction,
                    Some((start + 2) as u32..(start + 5) as u32),
                    Some((start + 5) as u32..(end - 2) as u32),
                )
            }
            Event::DocType(_) => {
                b.warning(
                    span.clone(),
                    "DOCTYPE shown as source only: DTDs and entities are not processed or fetched",
                );
                (Kind::Doctype, None, Some(span.clone()))
            }
        };
        if kind == Kind::Text {
            let start = match skipped_blank.take() {
                Some(blank) if blank.end == span.start => blank.start,
                _ => span.start,
            };
            if let Some(id) = last_text
                && b.node(id).span.end == start
            {
                let value = b.node(id).span.start..span.end;
                b.set_end(id, span.end);
                b.set_value(id, value, None);
            } else {
                let span = start..span.end;
                last_text = Some(b.leaf(kind, None, span.clone(), None, Some(span))?);
            }
        } else {
            last_text = None;
            skipped_blank = None;
            b.leaf(kind, None, span, name, value)?;
        }
    }
}

/// Recovered markup must not pass an out-of-range or split UTF-8 name to quick-xml.
fn checked_tag(token: &str, name_len: usize) -> Option<BytesStart<'_>> {
    let inner = token.strip_prefix('<')?.strip_suffix('>')?;
    let inner = inner.strip_suffix('/').unwrap_or(inner);
    // quick-xml slices by this length without checking.
    if name_len > inner.len() || !inner.is_char_boundary(name_len) {
        return None;
    }
    Some(BytesStart::from_content(inner, name_len))
}

/// The tag has been validated; scan its raw spelling to retain spaces and both quote styles.
fn attributes(
    source: &str,
    mut pos: usize,
    end: usize,
    b: &mut Builder<'_>,
    cancel: &AtomicBool,
) -> Result<(), Halt> {
    let bytes = source.as_bytes();
    while pos < end {
        check_cancel(cancel)?;
        while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        if matches!(bytes.get(pos), Some(b'/' | b'>') | None) {
            break;
        }
        let start = pos;
        while bytes
            .get(pos)
            .is_some_and(|c| !c.is_ascii_whitespace() && *c != b'=')
        {
            pos += 1;
        }
        let name = start as u32..pos as u32;
        while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        if bytes.get(pos) != Some(&b'=') {
            return Err(syntax(pos, "Expected '=' after attribute name"));
        }
        pos += 1;
        while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        let value_start = pos;
        let quote = match bytes.get(pos) {
            Some(q @ (b'\'' | b'"')) => *q,
            _ => return Err(syntax(pos, "Expected quoted attribute value")),
        };
        pos += 1;
        while pos < end && bytes.get(pos) != Some(&quote) {
            pos += 1;
        }
        if pos >= end {
            return Err(syntax(pos, "Expected closing attribute quote"));
        }
        pos += 1;
        b.leaf(
            Kind::Attribute,
            Some(Scalar::String),
            start as u32..pos as u32,
            Some(name),
            Some(value_start as u32..pos as u32),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{ROOT, Severity, dump};
    use super::*;

    const FIXTURES: &[(&str, &str, bool)] = &[
        (
            "example.xml",
            include_str!("../../../../tests/fixtures/tree/xml/example.xml"),
            true,
        ),
        (
            "namespaces.xml",
            include_str!("../../../../tests/fixtures/tree/xml/namespaces.xml"),
            true,
        ),
        (
            "mixed.xml",
            include_str!("../../../../tests/fixtures/tree/xml/mixed.xml"),
            true,
        ),
        (
            "doctype.xml",
            include_str!("../../../../tests/fixtures/tree/xml/doctype.xml"),
            true,
        ),
        (
            "drawing.svg",
            include_str!("../../../../tests/fixtures/tree/xml/drawing.svg"),
            true,
        ),
        (
            "mismatched.xml",
            include_str!("../../../../tests/fixtures/tree/xml/mismatched.xml"),
            false,
        ),
        (
            "unclosed.xml",
            include_str!("../../../../tests/fixtures/tree/xml/unclosed.xml"),
            false,
        ),
        (
            "two-roots.xml",
            include_str!("../../../../tests/fixtures/tree/xml/two-roots.xml"),
            false,
        ),
    ];

    fn parse(s: &str) -> Tree {
        Xml.parse(s, &Limits::default(), &AtomicBool::new(false))
    }

    fn slice<'a>(s: &'a str, span: &Span) -> &'a str {
        &s[span.start as usize..span.end as usize]
    }

    fn named(t: &Tree, s: &str, name: &str) -> NodeId {
        (0..t.len() as u32)
            .find(|&id| {
                t.node(id)
                    .name
                    .as_ref()
                    .is_some_and(|span| slice(s, span) == name)
            })
            .unwrap()
    }

    fn assert_spans(t: &Tree, s: &str) {
        let mut previous = 0;
        for id in 0..t.len() as u32 {
            let n = t.node(id);
            assert!(n.span.start >= previous, "{s:?}");
            previous = n.span.start;
            for span in [Some(&n.span), n.name.as_ref(), n.value.as_ref()]
                .into_iter()
                .flatten()
            {
                assert!(
                    s.get(span.start as usize..span.end as usize).is_some(),
                    "{s:?}: {span:?}"
                );
                assert!(
                    span.start >= n.span.start && span.end <= n.span.end,
                    "{s:?}: {n:?}"
                );
            }
            if let Some(parent) = n.parent() {
                let p = t.node(parent);
                assert!(
                    p.span.start <= n.span.start && p.span.end >= n.span.end,
                    "{s:?}"
                );
                assert!(t.children(parent).any(|child| child == id));
            }
        }
        for d in &t.diagnostics {
            assert!(
                s.get(d.span.start as usize..d.span.end as usize).is_some(),
                "{s:?}: {d:?}"
            );
        }
    }

    #[test]
    fn element_attribute_and_text_snapshot() {
        let s = "<r a='1' b = \"é\"><x/>Hi &amp; bye</r>";
        let t = parse(s);
        assert_eq!(
            dump(&t, s),
            "\
Document [1] @0..38
  Element r [4] @0..38
    Attribute(String) a = '1' @3..8
    Attribute(String) b = \"é\" @9..17
    Element x [0] @18..22
    Text = Hi &amp; bye @22..34
"
        );
        assert_spans(&t, s);
    }

    #[test]
    fn fixtures_have_expected_status_and_valid_spans() {
        for &(name, s, valid) in FIXTURES {
            let t = parse(s);
            assert_eq!(t.stopped.is_none(), valid, "{name}: {}", dump(&t, s));
            assert_eq!(t.errors() == 0, valid, "{name}");
            assert_spans(&t, s);
        }
    }

    #[test]
    fn namespaces_and_full_element_spans_are_retained() {
        let s = "<r xmlns='urn:test'><x>café</x></r>";
        let t = parse(s);
        let x = named(&t, s, "x");
        assert_eq!(slice(s, &t.node(x).span), "<x>café</x>");
        assert_eq!(t.node(x).parent(), Some(named(&t, s, "r")));
        assert_eq!(
            t.node(1).child_count(),
            2,
            "namespace attribute and child element"
        );
        assert_eq!(t.stopped, None);
    }

    #[test]
    fn namespace_scope_is_restored_after_empty_and_nested_elements() {
        for s in [
            "<r><p:a xmlns:p='urn:p'/><p:b/></r>",
            "<r><p:a xmlns:p='urn:p'></p:a><p:b/></r>",
        ] {
            let t = parse(s);
            assert_eq!(t.stopped, Some(Stop::Error));
            assert!(
                t.diagnostics[0]
                    .message
                    .contains("Undeclared namespace prefix")
            );
            assert_eq!(t.node(1).child_count(), 1);
        }
    }

    #[test]
    fn malformed_inputs_keep_prefix_and_report_position() {
        for (s, message) in [
            ("", "No root element"),
            ("<r>", "Unclosed element <r>"),
            ("<r></x>", "expected `</r>`"),
            ("<r/><x/>", "Multiple root elements"),
            ("<p:r/>", "Undeclared namespace prefix"),
            ("text", "Text outside the root element"),
            ("&amp;<r/>", "Entity reference outside the root element"),
            ("<r a='1' a='2'/>", "Attribute error"),
            ("<r a=1/>", "Attribute error"),
            ("<r a/>", "Attribute error"),
            ("<r a='1' b/>", "Attribute error"),
            ("<r><!-- a -- b --></r>", "comment"),
            ("</r>", "close"),
            ("<r a='", "closed"),
        ] {
            let t = parse(s);
            assert_eq!(t.stopped, Some(Stop::Error), "{s}");
            assert!(
                t.diagnostics
                    .iter()
                    .any(|d| d.message.to_lowercase().contains(&message.to_lowercase())),
                "{s}: {:?}",
                t.diagnostics
            );
            assert_spans(&t, s);
        }
        let s = "<r><a>ok</a><b></c></r>";
        let t = parse(s);
        assert_eq!(t.len(), 5, "prefix retained");
        assert!(t.diagnostics[0].span.start > 10);
        assert_eq!(slice(s, &t.node(2).span), "<a>ok</a>");
        assert_spans(&t, s);
        assert_eq!(parse("<r a='1' b/>").len(), 1, "bad tag is not emitted");
    }

    #[test]
    fn invalid_namespace_bindings_keep_the_prefix_spans() {
        for s in [
            "<r><x/><a xmlns:xml='wrong'/></r>",
            "<r><x/><a xmlns:xmlns='urn:p'/></r>",
        ] {
            let t = parse(s);
            assert_eq!(t.stopped, Some(Stop::Error));
            assert_eq!(t.len(), 3);
            assert_eq!(t.diagnostics[0].span, 7..7);
            assert_eq!(t.node(1).span, 0..7);
            assert_spans(&t, s);
        }
    }

    #[test]
    fn depth_and_node_limits_are_enforced() {
        let deep = format!("{}{}", "<r>".repeat(300), "</r>".repeat(300));
        let t = parse(&deep);
        assert_eq!(t.stopped, Some(Stop::Depth));
        assert_spans(&t, &deep);
        for s in [
            format!("<r>{}</r>", "<a/>".repeat(100)),
            format!(
                "<r {}/>",
                (0..100).map(|i| format!("a{i}='v' ")).collect::<String>()
            ),
        ] {
            let t = Xml.parse(
                &s,
                &Limits {
                    max_nodes: 50,
                    ..Limits::default()
                },
                &AtomicBool::new(false),
            );
            assert_eq!(t.stopped, Some(Stop::Nodes));
            assert_eq!(t.len(), 50);
            assert_eq!(t.node(1).child_count(), 48);
            assert_spans(&t, &s);
        }
        let t = Xml.parse(
            "<r><a/></r>",
            &Limits {
                max_depth: 1,
                ..Limits::default()
            },
            &AtomicBool::new(false),
        );
        assert_eq!(
            t.stopped,
            Some(Stop::Depth),
            "self-closing elements also count towards depth"
        );
    }

    #[test]
    fn diagnostic_limit_counts_warnings() {
        let s = "<?xml version='1.0' encoding='Shift_JIS'?><!DOCTYPE r><r/>";
        let t = Xml.parse(
            s,
            &Limits {
                max_diagnostics: 1,
                ..Limits::default()
            },
            &AtomicBool::new(false),
        );
        assert_eq!(t.diagnostics.len(), 1);
        assert_eq!(t.more_diagnostics, 1);
        assert_eq!(t.stopped, None);
    }

    #[test]
    fn cancellation_stops_before_reading_input() {
        for s in ["<r/>", " ", "<r>"] {
            let t = Xml.parse(s, &Limits::default(), &AtomicBool::new(true));
            assert_eq!(t.stopped, Some(Stop::Cancelled));
            assert_eq!(t.len(), 1);
            assert_eq!(t.errors(), 0);
        }
    }

    #[test]
    fn doctype_entities_and_xinclude_are_shown_not_processed() {
        let s = FIXTURES[3].1;
        let t = parse(s);
        assert_eq!(t.stopped, None);
        assert_eq!(t.node(1).kind, Kind::Doctype);
        assert_eq!(t.diagnostics[0].severity, Severity::Warning);
        let text = (0..t.len() as u32)
            .map(|i| t.node(i))
            .find(|n| n.kind == Kind::Text)
            .unwrap();
        assert_eq!(slice(s, &text.span), "&x;");
        assert_eq!(t.node(named(&t, s, "xi:include")).kind, Kind::Element);
    }

    #[test]
    fn mixed_content_spans_preserve_original_tokens() {
        let s = "<r>A<!--c--><![CDATA[x<y]]><?pi z?><x/>B &amp; C</r>";
        let t = parse(s);
        let children: Vec<_> = t.children(1).map(|id| slice(s, &t.node(id).span)).collect();
        assert_eq!(
            children,
            [
                "A",
                "<!--c-->",
                "<![CDATA[x<y]]>",
                "<?pi z?>",
                "<x/>",
                "B &amp; C"
            ]
        );
        assert_eq!(slice(s, t.node(3).value.as_ref().unwrap()), "c");
        assert_eq!(slice(s, t.node(4).value.as_ref().unwrap()), "x<y");
        assert_eq!(slice(s, t.node(5).name.as_ref().unwrap()), "pi");
        assert_eq!(slice(s, t.node(5).value.as_ref().unwrap()), " z");
        assert_spans(&t, s);
    }

    #[test]
    fn declaration_and_pi_snapshot() {
        let s = "<?xml version='1.0'?><?pi?><r/>";
        assert_eq!(
            dump(&parse(s), s),
            "\
Document [3] @0..31
  ProcessingInstruction xml = version='1.0' @0..21
  ProcessingInstruction pi =  @21..27
  Element r [0] @27..31
"
        );
    }

    #[test]
    fn raw_attributes_and_navigation() {
        let s = "<?xml version='1.0'?>\n<r xmlns:p='urn:p'><p:a id='1' name='x &amp; y'>t</p:a><b/></r>";
        let t = parse(s);
        let a = named(&t, s, "p:a");
        let attrs: Vec<_> = t
            .children(a)
            .filter(|&i| t.node(i).kind == Kind::Attribute)
            .map(|i| {
                let n = t.node(i);
                (
                    slice(s, n.name.as_ref().unwrap()),
                    slice(s, n.value.as_ref().unwrap()),
                )
            })
            .collect();
        assert_eq!(attrs, [("id", "'1'"), ("name", "'x &amp; y'")]);
        let path = t.path_at(s.find("t</p:a>").unwrap() as u32);
        assert_eq!(path, [ROOT, named(&t, s, "r"), a, a + 3]);
        assert_eq!(
            t.index_in_parent(a),
            1,
            "namespace attribute precedes element"
        );
        assert_eq!(t.children(ROOT).count(), 2, "declaration and root");
    }

    #[test]
    fn unsupported_declared_encoding_warns() {
        let t = parse("<?xml version='1.0' encoding='Shift_JIS'?><r/>");
        assert_eq!(t.stopped, None);
        assert!(t.diagnostics[0].message.contains("shift_jis"));
        assert_eq!(t.diagnostics[0].severity, Severity::Warning);
        for enc in [
            "UTF-8",
            "utf8",
            "UTF-16",
            "utf-16le",
            "utf-16be",
            "iso-8859-1",
            "latin1",
            "windows-1252",
            "us-ascii",
            "ascii",
        ] {
            assert!(
                parse(&format!("<?xml version='1.0' encoding='{enc}'?><r/>"))
                    .diagnostics
                    .is_empty()
            );
        }
    }

    #[test]
    fn indentation_is_skipped_and_entity_text_is_merged() {
        let s = "<r>\n  <a>x</a>\n  <b> &amp; y</b>\n</r>";
        let t = parse(s);
        let text: Vec<_> = (0..t.len() as u32)
            .map(|i| t.node(i))
            .filter(|n| n.kind == Kind::Text)
            .map(|n| slice(s, &n.span))
            .collect();
        assert_eq!(text, ["x", " &amp; y"]);
        assert_eq!(t.node(1).child_count(), 2);
        let s = "<r> &amp; &#32; &#x1f642; &custom; tail </r>";
        let t = parse(s);
        assert_eq!(t.node(1).child_count(), 1);
        assert_eq!(
            slice(s, &t.node(2).span),
            " &amp; &#32; &#x1f642; &custom; tail "
        );
    }

    #[test]
    fn bom_and_unicode_spans_are_byte_offsets() {
        for s in [
            "\u{feff}<é 名 = '你好'>🙂&amp;é</é>",
            "\u{feff}<?xml version='1.0'?><r/>",
        ] {
            let t = parse(s);
            assert_eq!(t.stopped, None);
            assert_spans(&t, s);
            assert_eq!(t.node(1).span.start, 3);
        }
        let s = "\u{feff}\t<r/>";
        let t = parse(s);
        assert_eq!(t.stopped, None);
        assert_eq!(t.node(1).span, 4..8);
        assert_spans(&t, s);
        for s in ["\u{feff}", "\u{feff}\t<b/=>", "\u{feff}\u{feff}<r/>"] {
            assert_spans(&parse(s), s);
        }
    }

    #[test]
    fn recovered_tag_name_lengths_are_checked() {
        assert!(checked_tag("<é/>", 1).is_none());
        assert!(checked_tag("<a/>", 20).is_none());
        assert!(checked_tag("<a>", usize::MAX).is_none());
        assert!(checked_tag("<é/>", 2).is_some());
    }

    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    #[test]
    fn random_and_mutated_markup_never_panics() {
        let alphabet: Vec<_> = "<>/ab =\"'!?-:\n\t&;[]é🙂\0".chars().collect();
        let seeds = [
            FIXTURES[0].1,
            FIXTURES[1].1,
            FIXTURES[2].1,
            FIXTURES[3].1,
            "<é 名='é'/>",
            "<a x='1'",
            "<a><b/></a>",
            "<<é a='x'//>>",
            "\u{feff}<r/>",
            "<r><x/><a xmlns:xml='wrong'/></r>",
        ];
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for round in 0..20_000 {
            let mut chars: Vec<_> = if round % 2 == 0 {
                seeds[rng.below(seeds.len())].chars().collect()
            } else {
                (0..rng.below(40))
                    .map(|_| alphabet[rng.below(alphabet.len())])
                    .collect()
            };
            for _ in 0..rng.below(5) {
                if chars.is_empty() {
                    break;
                }
                let i = rng.below(chars.len());
                match rng.below(3) {
                    0 => chars[i] = alphabet[rng.below(alphabet.len())],
                    1 => {
                        chars.remove(i);
                    }
                    _ => chars.insert(i, alphabet[rng.below(alphabet.len())]),
                }
            }
            let s: String = chars.into_iter().collect();
            let t = parse(&s);
            assert_spans(&t, &s);
            let _ = dump(&t, &s);
            let _ = t.path_at(rng.below(s.len() + 1) as u32);
        }
    }
}
