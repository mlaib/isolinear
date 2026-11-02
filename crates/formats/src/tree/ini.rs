//! INI sections and keys, in source order. Values keep quotes and inline comments.
//! Properties-style whitespace separators and `!` comments are accepted. Continuation
//! lines remain separate, unnamed string leaves: no lines are joined or escapes decoded.
//! One informational diagnostic explains those leaves, without flooding the diagnostics.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Builder, Kind, Limits, Scalar, Stop, Tree, TreeFormat};

pub const FORMAT: Option<&'static dyn TreeFormat> = Some(&Ini);

pub struct Ini;

impl TreeFormat for Ini {
    fn parse(&self, source: &str, limits: &Limits, cancel: &AtomicBool) -> Tree {
        let mut b = Builder::new(source.len(), limits, cancel);
        let bytes = source.as_bytes();
        let mut start = if source.starts_with('\u{feff}') { 3 } else { 0 };
        let mut section = false;
        let mut section_name = String::new();
        let mut last_end = start;
        let mut keys: HashMap<String, HashSet<String>> = HashMap::new();
        let mut sections = HashSet::new();
        let mut continuation = false;
        let mut explained_continuation = false;
        loop {
            if cancel.load(Ordering::Relaxed) {
                let mut tree = b.finish(last_end as u32);
                tree.stopped = Some(Stop::Cancelled);
                return tree;
            }
            if start == bytes.len() {
                break;
            }
            // Scan in chunks so one enormous comment or value can be cancelled too.
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'\n' {
                if end.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
                    let mut tree = b.finish(last_end as u32);
                    tree.stopped = Some(Stop::Cancelled);
                    return tree;
                }
                end += 1;
            }
            let next = (end < bytes.len()).then_some(end + 1);
            let content = source[start..end].trim_end_matches('\r');
            let content = content.trim_matches([' ', '\t']);
            let cs = start + source[start..end].len()
                - source[start..end].trim_start_matches([' ', '\t']).len();
            let ce = cs + content.len();
            let span = cs as u32..ce as u32;
            let continued = content
                .as_bytes()
                .iter()
                .rev()
                .take_while(|&&c| c == b'\\')
                .count()
                % 2
                == 1;
            if continuation {
                if !explained_continuation {
                    b.info(
                        span.clone(),
                        "continuation lines are shown separately, as written; lines are not joined",
                    );
                    explained_continuation = true;
                }
                if b.leaf(
                    Kind::Scalar,
                    Some(Scalar::String),
                    span.clone(),
                    None,
                    (!content.is_empty()).then_some(span),
                )
                .is_err()
                {
                    return b.finish(last_end as u32);
                }
                last_end = ce;
                continuation = continued;
            } else if content.is_empty() || content.starts_with([';', '#', '!']) {
                // Comments are not nodes.
            } else if content.starts_with('[') {
                match content.find(']') {
                    None => b.error(
                        span,
                        "expected ']' to close the section header, found the end of the line",
                    ),
                    Some(close) => {
                        let header_end = cs + close + 1;
                        let after = content[close + 1..].trim_start();
                        if !after.is_empty() && !after.starts_with([';', '#', '!']) {
                            b.warning(
                                header_end as u32..ce as u32,
                                "text after the section header",
                            );
                        }
                        if section {
                            b.close(last_end as u32);
                        }
                        if b.open(Kind::Table, cs as u32, Some(cs as u32..header_end as u32))
                            .is_err()
                        {
                            return b.finish(last_end as u32);
                        }
                        section = true;
                        last_end = header_end;
                        section_name = content[1..close].trim().to_owned();
                        if !sections.insert(section_name.clone()) {
                            b.warning(cs as u32..header_end as u32, "section appears more than once; most readers merge it with the earlier one");
                        }
                    }
                }
            } else {
                // Explicit separators retain INI keys containing spaces. Without one,
                // accept a properties-style whitespace separator. Escaped separators
                // stay in the key; names themselves are never decoded.
                let mut escaped = false;
                let mut sep = None;
                let mut space = None;
                for (i, c) in content.bytes().enumerate() {
                    if escaped {
                        escaped = false;
                        continue;
                    }
                    if c == b'\\' {
                        escaped = true;
                        continue;
                    }
                    if matches!(c, b'=' | b':') {
                        sep = Some(i);
                        break;
                    }
                    if matches!(c, b' ' | b'\t') {
                        space.get_or_insert(i);
                    }
                }
                let separator = sep.or(space);
                let key_end = separator.map_or(ce, |i| {
                    cs + content[..i].trim_end_matches([' ', '\t']).len()
                });
                let value_start = separator.map_or(ce, |i| {
                    let after = &content[i + 1..];
                    cs + i + 1 + after.len() - after.trim_start_matches([' ', '\t']).len()
                });
                if key_end == cs {
                    b.error(span, "expected a key before the separator");
                } else {
                    if separator.is_none() {
                        b.warning(
                            span.clone(),
                            "line has no separator; shown as a key without a value",
                        );
                    }
                    let key = source[cs..key_end].to_ascii_lowercase();
                    if !keys.entry(section_name.clone()).or_default().insert(key) {
                        b.warning(
                            cs as u32..key_end as u32,
                            "duplicate key (ignoring ASCII case)",
                        );
                    }
                    if b.leaf(
                        Kind::Scalar,
                        Some(Scalar::String),
                        span,
                        Some(cs as u32..key_end as u32),
                        (value_start < ce).then_some(value_start as u32..ce as u32),
                    )
                    .is_err()
                    {
                        return b.finish(last_end as u32);
                    }
                    last_end = ce;
                    continuation = continued;
                }
            }
            match next {
                Some(at) => start = at,
                None => break,
            }
        }
        if section {
            b.close(last_end as u32);
        }
        b.finish(source.len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::super::json::tests::Rng;
    use super::super::{ROOT, Severity, dump};
    use super::*;

    fn parse(s: &str) -> Tree {
        Ini.parse(s, &Limits::default(), &AtomicBool::new(false))
    }

    fn check_spans(s: &str, t: &Tree) {
        let mut last_start = 0;
        for id in 0..t.len() as u32 {
            let n = t.node(id);
            assert!(n.span.start >= last_start);
            last_start = n.span.start;
            for r in std::iter::once(&n.span)
                .chain(n.name.iter())
                .chain(n.value.iter())
            {
                assert!(
                    r.start <= r.end && r.end as usize <= s.len(),
                    "{s:?}: {n:?}"
                );
                assert!(
                    s.is_char_boundary(r.start as usize) && s.is_char_boundary(r.end as usize),
                    "{s:?}: {n:?}"
                );
                assert!(
                    n.span.start <= r.start && r.end <= n.span.end,
                    "{s:?}: {n:?}"
                );
            }
            if let Some(p) = n.parent() {
                assert!(
                    t.node(p).span.start <= n.span.start && n.span.end <= t.node(p).span.end,
                    "{s:?}: {n:?}"
                );
            }
        }
    }

    #[test]
    fn sections_and_keys_snapshot() {
        let s = "root = top\n[s]\na: \"x\" ; kept\nempty=\n# tail\n\n[t]\n";
        let t = parse(s);
        assert!(t.diagnostics.is_empty());
        check_spans(s, &t);
        assert_eq!(
            dump(&t, s),
            "\
Document [3] @0..48
  Scalar(String) root = top @0..10
  Table [s] [2] @11..35
    Scalar(String) a = \"x\" ; kept @15..28
    Scalar(String) empty @29..35
  Table [t] [0] @44..47
"
        );
    }

    #[test]
    fn duplicates_and_bad_lines_recover() {
        let s = "a=1\nA=2\n[s]\nk=1\n[s]\nK=2\nflag\n[broken\n=nokey\nafter=ok\n";
        let t = parse(s);
        check_spans(s, &t);
        assert_eq!(t.stopped, None);
        let diags: Vec<_> = t
            .diagnostics
            .iter()
            .map(|d| (d.severity, d.span.start as usize))
            .collect();
        assert_eq!(
            diags,
            [
                (Severity::Warning, s.find("A=2").unwrap()),
                (Severity::Warning, s.rfind("[s]").unwrap()),
                (Severity::Warning, s.find("K=2").unwrap()),
                (Severity::Warning, s.find("flag").unwrap()),
                (Severity::Error, s.find("[broken").unwrap()),
                (Severity::Error, s.find("=nokey").unwrap()),
            ]
        );
        let last = t.node(t.len() as u32 - 1);
        assert_eq!(
            &s[last.span.start as usize..last.span.end as usize],
            "after=ok"
        );
        assert_eq!(last.parent(), Some(5));
    }

    #[test]
    fn values_are_source_faithful_and_names_include_brackets() {
        let s = "[ paths ] ; comment\nhome=C:\\Users\\Ann\nnote = \"quoted = kept\"\nempty=\n";
        let t = parse(s);
        assert!(t.diagnostics.is_empty());
        check_spans(s, &t);
        let texts: Vec<_> = (1..t.len() as u32)
            .map(|id| {
                let n = t.node(id);
                (
                    n.name
                        .as_ref()
                        .map(|r| &s[r.start as usize..r.end as usize]),
                    n.value
                        .as_ref()
                        .map(|r| &s[r.start as usize..r.end as usize]),
                )
            })
            .collect();
        assert_eq!(
            texts,
            [
                (Some("[ paths ]"), None),
                (Some("home"), Some("C:\\Users\\Ann")),
                (Some("note"), Some("\"quoted = kept\"")),
                (Some("empty"), None)
            ]
        );
    }

    #[test]
    fn properties_whitespace_and_continuations_remain_physical_lines() {
        let s = include_str!("../../../../tests/fixtures/tree/ini/application.properties");
        let t = parse(s);
        check_spans(s, &t);
        assert_eq!(t.errors(), 0);
        assert_eq!(t.diagnostics.len(), 1);
        assert_eq!(t.diagnostics[0].severity, Severity::Info);
        let values: Vec<_> = t
            .children(ROOT)
            .map(|id| {
                let r = t.node(id).value.clone().unwrap();
                &s[r.start as usize..r.end as usize]
            })
            .collect();
        assert_eq!(
            values,
            [
                "Example",
                "8080",
                "Hello world",
                "first \\",
                "second \\",
                "third",
                "two backslashes \\\\",
                "still a separate key",
                "value"
            ]
        );
        assert_eq!(t.node(5).name, None);
        assert_eq!(t.node(6).name, None);
    }

    #[test]
    fn limits_and_cancellation() {
        for s in ["k=v\n".repeat(100), "[s]\nk=v\n".repeat(100)] {
            let t = Ini.parse(
                &s,
                &Limits {
                    max_nodes: 50,
                    ..Limits::default()
                },
                &AtomicBool::new(false),
            );
            assert_eq!(t.len(), 50);
            assert_eq!(t.stopped, Some(Stop::Nodes));
            check_spans(&s, &t);
        }
        let t = Ini.parse(
            "[s]\na=1",
            &Limits {
                max_depth: 0,
                ..Limits::default()
            },
            &AtomicBool::new(false),
        );
        assert_eq!(t.stopped, Some(Stop::Depth));
        let t = Ini.parse(
            "a=1\nA=2\n[bad\n=bad",
            &Limits {
                max_diagnostics: 1,
                ..Limits::default()
            },
            &AtomicBool::new(false),
        );
        assert_eq!(t.diagnostics.len(), 1);
        assert_eq!(t.more_diagnostics, 2);
        for s in ["", "#comment", "a=1", "[s]\nk=v\n"] {
            let t = Ini.parse(s, &Limits::default(), &AtomicBool::new(true));
            assert_eq!(t.stopped, Some(Stop::Cancelled));
            check_spans(s, &t);
        }
    }

    #[test]
    fn crlf_bom_empty_inputs_and_unicode() {
        let s = "\u{feff}[é]\r\n日 = 😀\r\n";
        let t = parse(s);
        assert!(t.diagnostics.is_empty());
        check_spans(s, &t);
        assert_eq!(t.len(), 3);
        for s in [
            "",
            "\n",
            "; only a comment",
            "# comment\r\n",
            "! comment",
            " \t\r\n",
        ] {
            let t = parse(s);
            assert!(t.is_empty());
            assert_eq!(t.stopped, None);
            assert!(t.diagnostics.is_empty());
        }
    }

    #[test]
    fn fixture_diagnostics_and_recovery() {
        for (s, errors, warnings) in [
            (
                include_str!("../../../../tests/fixtures/tree/ini/settings.ini"),
                0,
                3,
            ),
            (
                include_str!("../../../../tests/fixtures/tree/ini/malformed-header.ini"),
                1,
                1,
            ),
            (
                include_str!("../../../../tests/fixtures/tree/ini/malformed-key.ini"),
                2,
                1,
            ),
        ] {
            let t = parse(s);
            check_spans(s, &t);
            assert_eq!(t.stopped, None);
            assert_eq!(t.errors(), errors);
            assert_eq!(
                t.diagnostics
                    .iter()
                    .filter(|d| d.severity == Severity::Warning)
                    .count(),
                warnings
            );
        }
    }

    #[test]
    fn random_input_never_panics() {
        let alphabet = [
            "[", "]", "=", ":", ";", "#", "!", "\\", " ", "\t", "\n", "\r\n", "\r", "k", "v", "\"",
            "é", "日", "😀", "\u{feff}",
        ];
        let mut rng = Rng(0x0F1E_2D3C_4B5A_6978);
        for _ in 0..30_000 {
            let text: String = (0..rng.below(32))
                .map(|_| alphabet[rng.below(alphabet.len())])
                .collect();
            let t = parse(&text);
            check_spans(&text, &t);
            let _ = dump(&t, &text);
            let _ = t.path_at(rng.below(text.len() + 1) as u32);
        }
    }
    #[test]
    fn cancellation_with_a_long_comment() {
        // Deterministic: a cancel that arrives while a line is being scanned is checked every
        // 4 KiB, but whether the scan is still running then depends on machine speed.
        let s = format!("#{}\n[s]\nkey=value\n", "é".repeat(1_000_000));
        let cancel = AtomicBool::new(true);
        let t = Ini.parse(&s, &Limits::default(), &cancel);
        assert_eq!(t.stopped, Some(Stop::Cancelled));
        check_spans(&s, &t);
    }
}
