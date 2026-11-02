//! BibTeX and BibLaTeX databases read as entries of fields, with byte spans into the source.
//! Values are kept as written; nothing is expanded (string macros are named, not substituted).
//! Syntax errors skip to the next `@` that starts a line, so one bad entry does not hide the rest.

use std::collections::HashMap;
use std::ops::Range;

use crate::tree::{Diagnostic, Severity};

#[derive(Debug, Clone)]
pub struct Entry {
    /// Lower-cased entry type (`article`, `book`, …).
    pub kind: String,
    pub span: Range<usize>,
    pub kind_span: Range<usize>,
    pub key: Option<Range<usize>>,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: Range<usize>,
    /// The whole value as written, concatenations included.
    pub value: Range<usize>,
}

#[derive(Debug, Default)]
pub struct Bib {
    pub entries: Vec<Entry>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Fields an entry type needs; each inner list is satisfied by any one of its names.
fn required(kind: &str) -> &'static [&'static [&'static str]] {
    match kind {
        "article" => &[
            &["author"],
            &["title"],
            &["journal", "journaltitle"],
            &["year", "date"],
        ],
        "book" => &[
            &["author", "editor"],
            &["title"],
            &["publisher"],
            &["year", "date"],
        ],
        "inproceedings" | "conference" => {
            &[&["author"], &["title"], &["booktitle"], &["year", "date"]]
        }
        "incollection" => &[
            &["author"],
            &["title"],
            &["booktitle"],
            &["publisher"],
            &["year", "date"],
        ],
        "phdthesis" | "mastersthesis" | "thesis" => &[
            &["author"],
            &["title"],
            &["school", "institution"],
            &["year", "date"],
        ],
        "techreport" | "report" => &[&["author"], &["title"], &["institution"], &["year", "date"]],
        "inbook" => &[
            &["author", "editor"],
            &["title"],
            &["chapter", "pages"],
            &["publisher"],
            &["year", "date"],
        ],
        "online" => &[&["title"], &["url", "doi", "eprint"]],
        _ => &[],
    }
}

pub fn parse(source: &str) -> Bib {
    let mut p = Parser {
        s: source.as_bytes(),
        text: source,
        i: 0,
        bib: Bib::default(),
    };
    while let Some(at) = p.next_at() {
        p.i = at;
        if let Err((span, message)) = p.item() {
            p.diag(Severity::Error, span, message);
            p.recover();
        }
    }
    p.check();
    p.bib
}

struct Parser<'s> {
    s: &'s [u8],
    text: &'s str,
    i: usize,
    bib: Bib,
}

type Failure = (Range<usize>, String);

impl Parser<'_> {
    fn diag(&mut self, severity: Severity, span: Range<usize>, message: impl Into<String>) {
        if self.bib.diagnostics.len() < 1000 {
            self.bib.diagnostics.push(Diagnostic {
                severity,
                span: span.start as u32..span.end as u32,
                message: message.into(),
            });
        }
    }

    /// The next `@` from the current position; text between entries is a comment.
    fn next_at(&self) -> Option<usize> {
        self.s[self.i.min(self.s.len())..]
            .iter()
            .position(|&c| c == b'@')
            .map(|p| self.i + p)
    }

    /// Skips to the next `@` at the start of a line.
    fn recover(&mut self) {
        for k in self.i + 1..self.s.len() {
            if self.s[k] != b'@' {
                continue;
            }
            let line = self.s[..k]
                .iter()
                .rposition(|&c| c == b'\n')
                .map_or(0, |p| p + 1);
            if self.s[line..k].iter().all(|c| matches!(c, b' ' | b'\t')) {
                self.i = k;
                return;
            }
        }
        self.i = self.s.len();
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn ident(&mut self) -> Range<usize> {
        let start = self.i;
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_alphanumeric() || b"_-:.+/'".contains(&self.s[self.i]))
        {
            self.i += 1;
        }
        start..self.i
    }

    fn expect_close(&self, close: u8) -> Failure {
        let at = self.i.min(self.s.len());
        let found = self.text[at..]
            .chars()
            .next()
            .map_or("the end".to_owned(), |c| format!("'{c}'"));
        (
            at..(at + 1).min(self.s.len()),
            format!("expected ',' or '{}', found {found}", close as char),
        )
    }

    fn item(&mut self) -> Result<(), Failure> {
        let start = self.i;
        self.i += 1;
        self.ws();
        let kind_span = self.ident();
        if kind_span.is_empty() {
            return Err((start..start + 1, "expected an entry type after '@'".into()));
        }
        let kind = self.text[kind_span.clone()].to_ascii_lowercase();
        self.ws();
        let close = match self.s.get(self.i) {
            Some(b'{') => b'}',
            Some(b'(') => b')',
            _ => return Err((kind_span, "expected '{' after the entry type".into())),
        };
        let open = self.i;
        if kind == "comment" || kind == "preamble" {
            self.i = self.balanced(open, close)?;
            return Ok(());
        }
        self.i += 1;
        self.ws();
        let mut entry = Entry {
            kind: kind.clone(),
            span: start..start,
            kind_span,
            key: None,
            fields: Vec::new(),
        };
        if kind != "string" {
            let key_start = self.i;
            while self.i < self.s.len() && !b",}) \t\r\n".contains(&self.s[self.i]) {
                self.i += 1;
            }
            if self.i == key_start {
                return Err((key_start..key_start + 1, "expected a citation key".into()));
            }
            entry.key = Some(key_start..self.i);
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(&c) if c == close => {}
                _ => return Err(self.expect_close(close)),
            }
        }
        loop {
            self.ws();
            match self.s.get(self.i) {
                Some(&c) if c == close => {
                    self.i += 1;
                    break;
                }
                None => return Err((start..start + 1, "entry is never closed".into())),
                _ => {}
            }
            let name = self.ident();
            if name.is_empty() {
                let at = self.i;
                return Err((at..at + 1, "expected a field name".into()));
            }
            self.ws();
            if self.s.get(self.i) != Some(&b'=') {
                let at = self.i;
                return Err((
                    at..(at + 1).min(self.s.len()),
                    "expected '=' after the field name".into(),
                ));
            }
            self.i += 1;
            let value = self.value()?;
            entry.fields.push(Field { name, value });
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(&c) if c == close => {}
                _ => return Err(self.expect_close(close)),
            }
        }
        entry.span = start..self.i;
        if kind != "string" {
            self.bib.entries.push(entry);
        }
        Ok(())
    }

    /// A value: parts joined by `#`, each a braced group, a quoted string, a number or a macro.
    fn value(&mut self) -> Result<Range<usize>, Failure> {
        self.ws();
        let start = self.i;
        loop {
            self.ws();
            match self.s.get(self.i) {
                Some(b'{') => self.i = self.balanced(self.i, b'}')?,
                Some(b'"') => {
                    let at = self.i;
                    let mut depth = 0i32;
                    self.i += 1;
                    loop {
                        match self.s.get(self.i) {
                            None => return Err((at..at + 1, "unterminated string".into())),
                            Some(b'"') if depth == 0 => break,
                            Some(b'{') => depth += 1,
                            Some(b'}') => depth -= 1,
                            Some(b'\\') => self.i += 1,
                            _ => {}
                        }
                        self.i += 1;
                    }
                    self.i += 1;
                }
                Some(c) if c.is_ascii_alphanumeric() => {
                    self.ident();
                }
                _ => {
                    let at = self.i.min(self.s.len());
                    return Err((at..(at + 1).min(self.s.len()), "expected a value".into()));
                }
            }
            let end = self.i;
            self.ws();
            if self.s.get(self.i) == Some(&b'#') {
                self.i += 1;
                continue;
            }
            return Ok(start..end);
        }
    }

    /// Past the group opened at `open`, braces balanced and backslash escapes skipped.
    fn balanced(&self, open: usize, close: u8) -> Result<usize, Failure> {
        let opener = self.s[open];
        let mut depth = 0usize;
        let mut k = open;
        while k < self.s.len() {
            match self.s[k] {
                b'\\' => k += 1,
                c if c == opener => depth += 1,
                c if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(k + 1);
                    }
                }
                _ => {}
            }
            k += 1;
        }
        Err((open..open + 1, "group is never closed".into()))
    }

    /// Duplicate keys and fields, and fields an entry type requires.
    fn check(&mut self) {
        let mut keys: HashMap<String, usize> = HashMap::new();
        let mut found = Vec::new();
        for (i, e) in self.bib.entries.iter().enumerate() {
            if let Some(k) = &e.key {
                let key = self.text[k.clone()].to_lowercase();
                if keys.insert(key, i).is_some() {
                    found.push((
                        Severity::Warning,
                        k.clone(),
                        format!("duplicate key {}", &self.text[k.clone()]),
                    ));
                }
            }
            let mut names: HashMap<String, ()> = HashMap::new();
            for f in &e.fields {
                let name = self.text[f.name.clone()].to_ascii_lowercase();
                if names.insert(name.clone(), ()).is_some() {
                    found.push((
                        Severity::Warning,
                        f.name.clone(),
                        format!("field {name} appears twice"),
                    ));
                }
            }
            let missing: Vec<&str> = required(&e.kind)
                .iter()
                .filter(|any| !any.iter().any(|n| names.contains_key(*n)))
                .map(|any| any[0])
                .collect();
            if !missing.is_empty() {
                let at = e.key.clone().unwrap_or(e.kind_span.clone());
                found.push((
                    Severity::Warning,
                    at,
                    format!("@{} without {}", e.kind, missing.join(", ")),
                ));
            }
        }
        for (severity, span, message) in found {
            self.diag(severity, span, message);
        }
    }
}

/// A value as shown in a table: outer braces or quotes removed, inner braces dropped,
/// whitespace collapsed.
pub fn display(source: &str, value: &Range<usize>) -> String {
    let raw = source.get(value.clone()).unwrap_or("").trim();
    let inner = raw
        .strip_prefix('{')
        .and_then(|r| r.strip_suffix('}'))
        .or_else(|| raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')))
        .unwrap_or(raw);
    inner
        .chars()
        .filter(|c| *c != '{' && *c != '}')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl Entry {
    /// The value of the first field named `name` (any letter case).
    pub fn field(&self, source: &str, name: &str) -> Option<&Field> {
        self.fields
            .iter()
            .find(|f| source[f.name.clone()].eq_ignore_ascii_case(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"Some text before.
@string{tug = "TeX Users Group"}
@article{knuth84,
  author = {Donald E. Knuth},
  title = {Literate {Programming}},
  journal = "The Computer Journal",
  year = 1984,
}
@Book(lamport94, author = "Leslie Lamport", title = {\LaTeX: A Document Preparation System},
  publisher = tug # " Press", date = {1994})
@comment{ anything @here }
@misc{knuth84, note = {duplicate key}}
"#;

    #[test]
    fn entries_fields_and_spans() {
        let b = parse(SAMPLE);
        assert_eq!(b.entries.len(), 3);
        let k = &b.entries[0];
        assert_eq!(k.kind, "article");
        assert_eq!(&SAMPLE[k.key.clone().unwrap()], "knuth84");
        assert_eq!(
            display(SAMPLE, &k.field(SAMPLE, "title").unwrap().value),
            "Literate Programming"
        );
        assert_eq!(
            display(SAMPLE, &k.field(SAMPLE, "YEAR").unwrap().value),
            "1984"
        );
        let l = &b.entries[1];
        assert_eq!(l.kind, "book");
        assert_eq!(
            &SAMPLE[l.field(SAMPLE, "publisher").unwrap().value.clone()],
            "tug # \" Press\""
        );
        assert!(SAMPLE[l.span.clone()].ends_with("{1994})"));
    }

    #[test]
    fn duplicates_and_missing_fields_warn() {
        let b = parse(SAMPLE);
        let messages: Vec<_> = b.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages, ["duplicate key knuth84"]);
        let b = parse("@article{a, title={T}, title={U}}\n@inproceedings{b, author={A}}");
        let messages: Vec<_> = b.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "field title appears twice",
                "@article without author, journal, year",
                "@inproceedings without title, booktitle, year",
            ]
        );
    }

    #[test]
    fn a_bad_entry_does_not_hide_the_next() {
        let b = parse(
            "@article{a, title = }\n@book{b, title={x}, author={y}, publisher={p}, year=1}\n",
        );
        assert_eq!(b.entries.len(), 1);
        assert_eq!(b.diagnostics[0].severity, Severity::Error);
        assert!(b.diagnostics[0].message.contains("expected a value"));
    }

    #[test]
    fn odd_input_never_panics() {
        for s in [
            "@",
            "@{",
            "@a{",
            "@a{k",
            "@a{k,",
            "@a{k,f",
            "@a{k,f=",
            "@a{k,f={",
            "@a{k,f=\"",
            "@a(k,f=x#",
            "\n@\n@x{}",
        ] {
            let _ = parse(s);
        }
        let alphabet: Vec<char> = "@{}()\",=#\\ \nabk1é".chars().collect();
        let mut seed = 0xDEAD_BEEF_1234_5678u64;
        for _ in 0..20_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let s: String = (0..(seed % 40) as usize)
                .map(|i| alphabet[((seed >> (i % 50)) as usize + i * 7) % alphabet.len()])
                .collect();
            let b = parse(&s);
            for e in &b.entries {
                assert!(e.span.end <= s.len());
            }
        }
    }
}
