//! Indented text for XML, JSON and JSON Lines that are hard to read as written (a minified file
//! on one line). The source stays authoritative: [`Map`] links every token of the formatted text
//! back to its source offset, so trees, search and clicks keep working on the source.
//!
//! XML: one element per line, indented by depth; an element whose content includes text stays on
//! one line exactly as written (text is never reflowed); whitespace between elements is not
//! shown. JSON: two-space indentation; every token (strings, numbers, comments) exactly as
//! written, so formatting valid JSON gives the same JSON.

/// Formatting is not attempted above this size (the text and map are held in memory).
pub const MAX_SOURCE: usize = 64 * 1024 * 1024;
/// A line longer than this makes the formatted text the default.
const LONG_LINE: usize = 1000;

pub struct Formatted {
    pub text: String,
    pub map: Map,
}

/// `(formatted offset, source offset)` at the start of each token, ascending in both.
#[derive(Default)]
pub struct Map(Vec<(usize, usize)>);

impl Map {
    /// Offset in `formatted` of source offset `at`.
    pub fn to_formatted(&self, formatted: &str, at: usize) -> usize {
        let i = self.0.partition_point(|&(_, s)| s <= at);
        match i.checked_sub(1) {
            Some(k) => {
                let (d, s) = self.0[k];
                let limit = self
                    .0
                    .get(k + 1)
                    .map(|&(d2, _)| d2)
                    .unwrap_or(formatted.len());
                floor(formatted, (d + (at - s)).min(limit))
            }
            None => 0,
        }
    }

    /// Source offset of formatted offset `at`.
    pub fn to_source(&self, at: usize) -> usize {
        let i = self.0.partition_point(|&(d, _)| d <= at);
        match i.checked_sub(1) {
            Some(k) => {
                let (d, s) = self.0[k];
                let limit = self.0.get(k + 1).map(|&(_, s2)| s2).unwrap_or(usize::MAX);
                (s + (at - d)).min(limit)
            }
            None => 0,
        }
    }
}

fn floor(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Whether `text` reads badly as written: a line longer than about a screen width.
pub fn hard_to_read(text: &str) -> bool {
    text.split('\n').any(|l| l.len() > LONG_LINE)
}

/// Format ids that can be shown formatted.
pub fn supports(format: &str) -> bool {
    matches!(format, "xml" | "json" | "jsonl")
}

/// Formatted text for `text` in `format`; `None` when the format is not supported, the text is
/// too large, or `cancelled` says so.
pub fn format(format: &str, text: &str, cancelled: &dyn Fn() -> bool) -> Option<Formatted> {
    if text.len() > MAX_SOURCE {
        return None;
    }
    let mut out = Out {
        text: String::with_capacity(text.len() + text.len() / 4),
        map: vec![],
    };
    match format {
        "xml" => xml(text, &mut out, cancelled)?,
        "json" => json(text, 0, &mut out, cancelled)?,
        "jsonl" => {
            let mut at = 0;
            for line in text.split_inclusive('\n') {
                if cancelled() {
                    return None;
                }
                if !line.trim().is_empty() {
                    if !out.text.is_empty() {
                        out.text.push('\n');
                    }
                    json(line.trim_end_matches(['\n', '\r']), at, &mut out, cancelled)?;
                }
                at += line.len();
            }
        }
        _ => return None,
    }
    if !out.text.ends_with('\n') && !out.text.is_empty() {
        out.text.push('\n');
    }
    Some(Formatted {
        text: out.text,
        map: Map(out.map),
    })
}

struct Out {
    text: String,
    map: Vec<(usize, usize)>,
}

impl Out {
    /// Appends `token`, which starts at source offset `at`.
    fn token(&mut self, token: &str, at: usize) {
        self.map.push((self.text.len(), at));
        self.text.push_str(token);
    }

    fn line(&mut self, depth: usize) {
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        for _ in 0..depth.min(64) {
            self.text.push_str("  ");
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Xml {
    Start,
    End,
    Empty,
    Text,
    /// Comments, processing instructions, declarations.
    Other,
}

/// XML tokens: (kind, byte range). Malformed markup ends as text.
fn xml_tokens(text: &str) -> Vec<(Xml, std::ops::Range<usize>)> {
    let b = text.as_bytes();
    let mut tokens = vec![];
    let mut at = 0;
    let find = |from: usize, pat: &str| text[from..].find(pat).map(|i| from + i + pat.len());
    while at < b.len() {
        if b[at] != b'<' {
            let end = text[at..].find('<').map_or(b.len(), |i| at + i);
            tokens.push((Xml::Text, at..end));
            at = end;
            continue;
        }
        let rest = &text[at..];
        let (kind, end) = if rest.starts_with("<!--") {
            (Xml::Other, find(at + 4, "-->"))
        } else if rest.starts_with("<![CDATA[") {
            (Xml::Text, find(at + 9, "]]>"))
        } else if rest.starts_with("<?") {
            (Xml::Other, find(at + 2, "?>"))
        } else if rest.starts_with("<!") {
            // A declaration, possibly with an internal subset in brackets.
            let mut depth = 0i32;
            let mut end = None;
            for (i, c) in rest.char_indices().skip(2) {
                match c {
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    '>' if depth <= 0 => {
                        end = Some(at + i + 1);
                        break;
                    }
                    _ => {}
                }
            }
            (Xml::Other, end)
        } else {
            // A tag: quoted attribute values may contain '>'.
            let mut quote = None;
            let mut end = None;
            for (i, c) in rest.char_indices().skip(1) {
                match (quote, c) {
                    (Some(q), c) if c == q => quote = None,
                    (Some(_), _) => {}
                    (None, '"' | '\'') => quote = Some(c),
                    (None, '>') => {
                        end = Some(at + i + 1);
                        break;
                    }
                    (None, '<') => break,
                    _ => {}
                }
            }
            let kind = match end {
                Some(_) if rest.starts_with("</") => Xml::End,
                Some(e) if text[..e].ends_with("/>") => Xml::Empty,
                _ => Xml::Start,
            };
            (kind, end)
        };
        match end {
            Some(end) => {
                tokens.push((kind, at..end));
                at = end;
            }
            None => {
                tokens.push((Xml::Text, at..b.len()));
                at = b.len();
            }
        }
    }
    tokens
}

fn xml(text: &str, out: &mut Out, cancelled: &dyn Fn() -> bool) -> Option<()> {
    let tokens = xml_tokens(text);
    // Elements whose direct content includes text (or CDATA) stay on one line as written.
    let mut mixed = vec![false; tokens.len()];
    let mut stack: Vec<usize> = vec![];
    for (i, (kind, range)) in tokens.iter().enumerate() {
        match kind {
            Xml::Start => stack.push(i),
            Xml::End => {
                stack.pop();
            }
            Xml::Text if !text[range.clone()].trim().is_empty() => {
                if let Some(&open) = stack.last() {
                    mixed[open] = true;
                }
            }
            _ => {}
        }
    }
    let mut depth = 0usize;
    // Inside a one-line element: how deep, so its end is found.
    let mut inline = 0usize;
    let mut last = None;
    for (i, (kind, range)) in tokens.iter().enumerate() {
        if i.is_multiple_of(4096) && cancelled() {
            return None;
        }
        let token = &text[range.clone()];
        if inline > 0 {
            out.token(token, range.start);
            match kind {
                Xml::Start => inline += 1,
                Xml::End => inline -= 1,
                _ => {}
            }
            if inline == 0 {
                last = Some(Xml::End);
            }
            continue;
        }
        match kind {
            Xml::Text => {
                let trimmed = token.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let lead = token.len() - token.trim_start().len();
                out.line(depth);
                out.token(trimmed, range.start + lead);
            }
            Xml::Start => {
                out.line(depth);
                out.token(token, range.start);
                if mixed[i] {
                    inline = 1;
                } else {
                    depth += 1;
                }
            }
            Xml::End => {
                depth = depth.saturating_sub(1);
                if last != Some(Xml::Start) {
                    out.line(depth);
                }
                out.token(token, range.start);
            }
            Xml::Empty | Xml::Other => {
                out.line(depth);
                out.token(token, range.start);
            }
        }
        last = Some(*kind);
    }
    Some(())
}

/// JSON (or anything JSON-like: unknown runs are kept as they are) re-indented; `base` is the
/// source offset of `text`. Comments keep their own lines.
fn json(text: &str, base: usize, out: &mut Out, cancelled: &dyn Fn() -> bool) -> Option<()> {
    let b = text.as_bytes();
    // Tokens: punctuation, strings, comments and other runs, with their ranges.
    let mut tokens: Vec<std::ops::Range<usize>> = vec![];
    let mut at = 0;
    while at < b.len() {
        let c = b[at];
        if c.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        let end = match c {
            b'{' | b'}' | b'[' | b']' | b':' | b',' => at + 1,
            b'"' => {
                let mut i = at + 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                (i + 1).min(b.len())
            }
            b'/' if b.get(at + 1) == Some(&b'/') => {
                text[at..].find('\n').map_or(b.len(), |i| at + i)
            }
            b'/' if b.get(at + 1) == Some(&b'*') => text[at + 2..]
                .find("*/")
                .map_or(b.len(), |i| at + 2 + i + 2),
            _ => {
                let mut i = at;
                while i < b.len()
                    && !b[i].is_ascii_whitespace()
                    && !matches!(b[i], b'{' | b'}' | b'[' | b']' | b':' | b',' | b'"')
                {
                    i += 1;
                }
                i
            }
        };
        let end = floor(text, end.max(at + 1).min(b.len()));
        let end = if end <= at {
            at + text[at..].chars().next().map_or(1, char::len_utf8)
        } else {
            end
        };
        tokens.push(at..end);
        at = end;
    }
    let is_comment = |t: &str| t.starts_with("//") || t.starts_with("/*");
    let mut depth = 0usize;
    let mut fresh_line = out.text.is_empty() || out.text.ends_with('\n');
    // After a comment, the next token starts a new line.
    let mut after_comment = false;
    for (i, range) in tokens.iter().enumerate() {
        if i.is_multiple_of(4096) && cancelled() {
            return None;
        }
        let token = &text[range.clone()];
        let next = tokens.get(i + 1).map(|r| &text[r.clone()]);
        if after_comment && !matches!(token, "}" | "]") {
            out.line(depth);
            fresh_line = true;
        }
        after_comment = false;
        if is_comment(token) {
            // A comment after a token on the same line stays there.
            if !fresh_line && !out.text.ends_with(' ') {
                out.text.push(' ');
            }
            out.token(token, base + range.start);
            after_comment = true;
            fresh_line = false;
            continue;
        }
        match token {
            "{" | "[" => {
                out.token(token, base + range.start);
                let close = if token == "{" { "}" } else { "]" };
                if next != Some(close) {
                    depth += 1;
                    out.line(depth);
                    fresh_line = true;
                    continue;
                }
            }
            "}" | "]" => {
                let prev = i.checked_sub(1).map(|k| &text[tokens[k].clone()]);
                let empty = matches!((prev, token), (Some("{"), "}") | (Some("["), "]"));
                if !empty {
                    depth = depth.saturating_sub(1);
                    out.text.truncate(out.text.trim_end_matches(' ').len());
                    if !out.text.ends_with('\n') || out.text.is_empty() {
                        out.line(depth);
                    } else {
                        out.text.push_str(&"  ".repeat(depth.min(64)));
                    }
                }
                out.token(token, base + range.start);
            }
            ":" => {
                out.token(":", base + range.start);
                out.text.push(' ');
            }
            "," => {
                out.token(",", base + range.start);
                if !next.is_some_and(|n| n.starts_with("//")) {
                    out.line(depth);
                    fresh_line = true;
                    continue;
                }
            }
            _ => {
                if !fresh_line && out.text.ends_with(['}', ']', '"']) && !out.text.ends_with(": ") {
                    // Two values in a row (invalid JSON): keep them apart.
                    out.line(depth);
                }
                out.token(token, base + range.start);
            }
        }
        fresh_line = false;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(format: &str, text: &str) -> Formatted {
        super::format(format, text, &|| false).unwrap()
    }

    fn squeeze(s: &str) -> String {
        s.split_whitespace().collect()
    }

    #[test]
    fn xml_one_line_becomes_indented_and_text_stays_inline() {
        let src = "<?xml version='1.0'?><t><p><span m='1'>INT:</span> <span m='2'>Well, a &lt; b</span> </p><p/><!-- note --><q>a <b>bold</b> word</q></t>";
        let out = f("xml", src);
        assert_eq!(
            out.text,
            "<?xml version='1.0'?>\n<t>\n  <p>\n    <span m='1'>INT:</span>\n    <span m='2'>Well, a &lt; b</span>\n  </p>\n  <p/>\n  <!-- note -->\n  <q>a <b>bold</b> word</q>\n</t>\n"
        );
        let at = out.text.find("Well").unwrap();
        let s = out.map.to_source(at);
        assert_eq!(&src[s..s + 4], "Well");
        assert_eq!(
            out.map.to_formatted(&out.text, src.find("bold").unwrap()),
            out.text.find("bold").unwrap()
        );
        assert_eq!(
            out.map.to_formatted(&out.text, src.find("<p/>").unwrap()),
            out.text.find("<p/>").unwrap()
        );
    }

    #[test]
    fn xml_edge_cases_never_lose_text() {
        for src in [
            "<a x='>' y=\"<\">t</a>",
            "<a><![CDATA[ <not> ]]></a>",
            "<!DOCTYPE r [<!ENTITY e 'v'>]><r>&e;</r>",
            "<a><b>unclosed",
            "plain text",
            "<a></a>",
            "",
            "<日本 属性='値'>テキスト</日本>",
        ] {
            let out = f("xml", src);
            assert_eq!(squeeze(&out.text), squeeze(src), "{src}");
        }
        assert_eq!(f("xml", "<a></a>").text, "<a></a>\n");
    }

    #[test]
    fn json_is_indented_with_tokens_as_written() {
        let src =
            r#"{"a":[1,2.50,{"b":null}],"c":{},"d":[],"e":"x,y:{z}","big":12345678901234567890}"#;
        let out = f("json", src);
        assert_eq!(
            out.text,
            "{\n  \"a\": [\n    1,\n    2.50,\n    {\n      \"b\": null\n    }\n  ],\n  \"c\": {},\n  \"d\": [],\n  \"e\": \"x,y:{z}\",\n  \"big\": 12345678901234567890\n}\n"
        );
        let at = out.text.find("2.50").unwrap();
        assert_eq!(
            &src[out.map.to_source(at)..out.map.to_source(at) + 4],
            "2.50"
        );
        let bad = r#"{"a": tru e, "s": "unterminated"#;
        assert_eq!(squeeze(&f("json", bad).text), squeeze(bad));
    }

    #[test]
    fn formatting_is_stable() {
        let src = r#"{"a":[1,{"b":[]}],"c":"d"}"#;
        let once = f("json", src).text;
        assert_eq!(f("json", &once).text, once);
    }

    #[test]
    fn json_comments_keep_their_text_and_lines() {
        let src = "{\n  // build options\n  \"a\": 1, // trailing\n  /* block */ \"b\": [2]\n}";
        let out = f("json", src);
        assert_eq!(
            out.text,
            "{\n  // build options\n  \"a\": 1, // trailing\n  /* block */\n  \"b\": [\n    2\n  ]\n}\n"
        );
        assert_eq!(f("json", &out.text).text, out.text, "stable with comments");
        assert_eq!(squeeze(&out.text), squeeze(src));
    }

    #[test]
    fn json_lines_are_formatted_record_by_record() {
        let src = "{\"a\":1}\n\n{\"b\":[2]}\n";
        let out = f("jsonl", src);
        assert_eq!(out.text, "{\n  \"a\": 1\n}\n{\n  \"b\": [\n    2\n  ]\n}\n");
        let at = out.text.find("\"b\"").unwrap();
        assert_eq!(out.map.to_source(at), src.find("\"b\"").unwrap());
    }

    #[test]
    fn heuristics_and_limits() {
        assert!(hard_to_read(&"x".repeat(2000)));
        assert!(!hard_to_read(&"short\n".repeat(1000)));
        assert!(super::format("csv", "a,b", &|| false).is_none());
        assert!(
            super::format("json", "[1]", &|| true).is_none(),
            "cancelled"
        );
    }

    #[test]
    fn random_input_never_panics_or_loses_text() {
        let alphabet: &[u8] = b"{}[],:\"\\/* \n<>!?-=x'a";
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..20_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let len = (seed % 40) as usize;
            let s: String = (0..len)
                .map(|i| alphabet[((seed >> (i % 50)) as usize + i * 7) % alphabet.len()] as char)
                .collect();
            for format in ["json", "xml", "jsonl"] {
                let out = f(format, &s);
                assert_eq!(squeeze(&out.text), squeeze(&s), "{format}: {s:?}");
                for at in [0, s.len() / 2, s.len()] {
                    let back = out.map.to_source(out.map.to_formatted(&out.text, at));
                    assert!(back <= s.len());
                }
            }
        }
    }
}
