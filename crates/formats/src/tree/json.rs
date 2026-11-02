//! JSON (RFC 8259) as a tree: the reference [`TreeFormat`].
//!
//! Values are kept as written: numbers are not converted (integers beyond 2^53 are flagged,
//! because JavaScript readers lose their precision), strings keep their escapes. Duplicate
//! keys are reported, never hidden. Comments and trailing commas are accepted with a warning:
//! they are not JSON, but configuration files use them. The parser is iterative, so nesting
//! is bounded only by [`Limits::max_depth`].

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;

use super::{Builder, Kind, Limits, Scalar, Span, Tree, TreeFormat};

pub const FORMAT: Option<&'static dyn TreeFormat> = Some(&Json);

pub struct Json;

impl TreeFormat for Json {
    fn parse(&self, source: &str, limits: &Limits, cancel: &AtomicBool) -> Tree {
        let len = source.len() as u32;
        let mut b = Builder::new(source.len(), limits, cancel);
        let mut p = Parser::new(source, 0, source.len(), &mut b);
        let result = p.document();
        let pos = p.pos as u32;
        match result {
            Ok(()) => b.finish(len),
            Err(Halt::Syntax(span, message)) => {
                let at = span.start;
                b.fail(span, message);
                b.finish(at)
            }
            Err(Halt::Stopped) => b.finish(pos),
        }
    }
}

/// Why a value could not be read to its end.
pub(super) enum Halt {
    Syntax(Span, String),
    /// The builder stopped (limit or cancellation).
    Stopped,
}

enum Frame<'s> {
    Object {
        keys: HashSet<Cow<'s, str>>,
        state: State,
    },
    Array {
        state: State,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Just after `{` or `[`.
    Open,
    /// After a comma: a member or element must follow (or, leniently, the closing bracket).
    Comma,
    /// After a member or element.
    Value,
}

/// Reads JSON values from `text[pos..end]` into a builder.
pub(super) struct Parser<'s, 'b, 'c> {
    text: &'s str,
    src: &'s [u8],
    pub(super) pos: usize,
    end: usize,
    b: &'b mut Builder<'c>,
    warned_comment: bool,
    warned_comma: bool,
}

impl<'s, 'b, 'c> Parser<'s, 'b, 'c> {
    pub(super) fn new(text: &'s str, start: usize, end: usize, b: &'b mut Builder<'c>) -> Self {
        Parser {
            text,
            src: text.as_bytes(),
            pos: start,
            end,
            b,
            warned_comment: false,
            warned_comma: false,
        }
    }

    /// Exactly one value, with only whitespace (and comments) around it.
    pub(super) fn document(&mut self) -> Result<(), Halt> {
        self.ws()?;
        if self.pos >= self.end {
            return Err(Halt::Syntax(
                self.pos as u32..self.pos as u32,
                "no JSON value".into(),
            ));
        }
        self.value(None)?;
        self.ws()?;
        if self.pos < self.end {
            return Err(self.unexpected("the end after the JSON value"));
        }
        Ok(())
    }

    /// One value and everything nested in it.
    pub(super) fn value(&mut self, name: Option<Span>) -> Result<(), Halt> {
        let mut frames: Vec<Frame<'s>> = Vec::new();
        let mut name = name;
        loop {
            // A value starts here; a member's span starts at its key.
            self.ws()?;
            let start = name.as_ref().map_or(self.pos as u32, |n| n.start);
            match self.peek() {
                None => return Err(self.unexpected("a value")),
                Some(b'{') => {
                    self.b
                        .open(Kind::Object, start, name.take())
                        .map_err(|_| Halt::Stopped)?;
                    self.pos += 1;
                    frames.push(Frame::Object {
                        keys: HashSet::new(),
                        state: State::Open,
                    });
                }
                Some(b'[') => {
                    self.b
                        .open(Kind::Array, start, name.take())
                        .map_err(|_| Halt::Stopped)?;
                    self.pos += 1;
                    frames.push(Frame::Array { state: State::Open });
                }
                Some(b'"') => {
                    let span = self.string()?;
                    self.leaf(Scalar::String, span, name.take())?;
                }
                Some(b'-' | b'0'..=b'9') => {
                    let span = self.number()?;
                    self.leaf(Scalar::Number, span, name.take())?;
                }
                Some(b't') => self.literal("true", Scalar::Boolean, name.take())?,
                Some(b'f') => self.literal("false", Scalar::Boolean, name.take())?,
                Some(b'n') => self.literal("null", Scalar::Null, name.take())?,
                Some(_) => return Err(self.unexpected("a value")),
            }
            // Close what is complete; stop where the next value goes.
            loop {
                let Some(frame) = frames.last_mut() else {
                    return Ok(());
                };
                self.ws()?;
                match frame {
                    Frame::Array { state } => match (self.peek(), *state) {
                        (Some(b']'), s) => {
                            if s == State::Comma {
                                self.trailing_comma();
                            }
                            self.pos += 1;
                            self.b.close(self.pos as u32);
                            frames.pop();
                        }
                        (Some(b','), State::Value) => {
                            self.pos += 1;
                            *state = State::Comma;
                        }
                        (Some(_), State::Open | State::Comma) => {
                            *state = State::Value;
                            break;
                        }
                        (_, State::Value) => return Err(self.unexpected("',' or ']'")),
                        (None, _) => return Err(self.unexpected("a value or ']'")),
                    },
                    Frame::Object { keys, state } => match (self.peek(), *state) {
                        (Some(b'}'), s) => {
                            if s == State::Comma {
                                self.trailing_comma();
                            }
                            self.pos += 1;
                            self.b.close(self.pos as u32);
                            frames.pop();
                        }
                        (Some(b','), State::Value) => {
                            self.pos += 1;
                            *state = State::Comma;
                        }
                        (Some(b'"'), State::Open | State::Comma) => {
                            let key = self.string()?;
                            let text = self.key_text(&key);
                            if keys.contains(&text) {
                                self.b.warning(
                                    key.clone(),
                                    format!(
                                        "duplicate key {}: most readers keep only the last one",
                                        &self.text[key.start as usize..key.end as usize]
                                    ),
                                );
                            } else {
                                keys.insert(text);
                            }
                            self.ws()?;
                            if self.peek() != Some(b':') {
                                return Err(self.unexpected("':' after the key"));
                            }
                            self.pos += 1;
                            *state = State::Value;
                            name = Some(key);
                            break;
                        }
                        (_, State::Value) => return Err(self.unexpected("',' or '}'")),
                        _ => return Err(self.unexpected("a key in double quotes or '}'")),
                    },
                }
            }
        }
    }

    fn leaf(&mut self, scalar: Scalar, span: Span, name: Option<Span>) -> Result<(), Halt> {
        let start = name.as_ref().map_or(span.start, |n| n.start);
        self.b
            .leaf(
                Kind::Scalar,
                Some(scalar),
                start..span.end,
                name,
                Some(span),
            )
            .map(drop)
            .map_err(|_| Halt::Stopped)
    }

    fn literal(&mut self, word: &str, scalar: Scalar, name: Option<Span>) -> Result<(), Halt> {
        if !self.src[self.pos..self.end].starts_with(word.as_bytes()) {
            return Err(self.unexpected("a value"));
        }
        let span = self.pos as u32..(self.pos + word.len()) as u32;
        self.pos += word.len();
        self.leaf(scalar, span, name)
    }

    /// A string token, quotes included. Escapes are checked, not decoded.
    fn string(&mut self) -> Result<Span, Halt> {
        let start = self.pos;
        self.pos += 1;
        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    return Err(Halt::Syntax(
                        start as u32..self.pos as u32,
                        "unterminated string".into(),
                    ));
                }
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(start as u32..self.pos as u32);
                }
                Some(b'\\') => {
                    let at = self.pos;
                    self.pos += 1;
                    match self.peek() {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                            self.pos += 1
                        }
                        Some(b'u') => {
                            let hex = self.src.get(self.pos + 1..self.pos + 5);
                            if hex.is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) {
                                self.pos += 5;
                            } else {
                                self.b.error(
                                    at as u32..(self.pos + 1) as u32,
                                    "\\u must be followed by four hexadecimal digits",
                                );
                                self.pos += 1;
                            }
                        }
                        _ => {
                            let len = self.char_len();
                            self.b.error(
                                at as u32..(self.pos + len) as u32,
                                "invalid escape sequence",
                            );
                            self.pos += len;
                        }
                    }
                }
                Some(c) if c < 0x20 => {
                    self.b.error(
                        self.pos as u32..self.pos as u32 + 1,
                        "control characters must be escaped in strings",
                    );
                    self.pos += 1;
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    fn number(&mut self) -> Result<Span, Halt> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    return Err(Halt::Syntax(
                        start as u32..self.pos as u32 + 1,
                        "numbers cannot have leading zeros".into(),
                    ));
                }
            }
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.number_error(start, "a digit was expected")),
        }
        let integer_end = self.pos;
        let mut integer = true;
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(self.number_error(start, "a digit was expected after '.'"));
            }
            self.digits();
            integer = false;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(self.number_error(start, "a digit was expected in the exponent"));
            }
            self.digits();
            integer = false;
        }
        let span = start as u32..self.pos as u32;
        if integer && integer_end - start >= 16 {
            let digits = &self.text[start..integer_end];
            let big = digits
                .trim_start_matches('-')
                .parse::<u128>()
                .map_or(true, |v| v > (1u128 << 53) - 1);
            if big {
                self.b.info(
                    span.clone(),
                    "integer beyond 2^53: JavaScript readers lose its precision",
                );
            }
        }
        Ok(span)
    }

    fn digits(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
    }

    fn number_error(&self, start: usize, why: &str) -> Halt {
        let end = (self.pos + self.char_len().max(1)).min(self.end.max(self.pos));
        Halt::Syntax(start as u32..end as u32, format!("invalid number: {why}"))
    }

    /// Whitespace and comments.
    pub(super) fn ws(&mut self) -> Result<(), Halt> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.pos += 1,
                Some(b'/') if self.src.get(self.pos + 1) == Some(&b'/') => {
                    let start = self.pos;
                    while self.peek().is_some_and(|c| c != b'\n') {
                        self.pos += 1;
                    }
                    self.comment(start);
                }
                Some(b'/') if self.src.get(self.pos + 1) == Some(&b'*') => {
                    let start = self.pos;
                    match self.text[self.pos + 2..self.end].find("*/") {
                        Some(i) => self.pos += i + 4,
                        None => {
                            return Err(Halt::Syntax(
                                start as u32..start as u32 + 2,
                                "unterminated comment".into(),
                            ));
                        }
                    }
                    self.comment(start);
                }
                _ => return Ok(()),
            }
        }
    }

    fn comment(&mut self, start: usize) {
        if !self.warned_comment {
            self.warned_comment = true;
            self.b.warning(
                start as u32..self.pos as u32,
                "comments are not part of JSON; accepted here",
            );
        }
    }

    fn trailing_comma(&mut self) {
        if !self.warned_comma {
            self.warned_comma = true;
            self.b.warning(
                self.pos as u32..self.pos as u32 + 1,
                "a comma before a closing bracket is not JSON; accepted here",
            );
        }
    }

    /// A key's text for duplicate detection: escapes decoded only when there are any.
    fn key_text(&self, key: &Span) -> Cow<'s, str> {
        let raw = &self.text[key.start as usize + 1..key.end as usize - 1];
        if !raw.contains('\\') {
            return Cow::Borrowed(raw);
        }
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        let mut pending_high: Option<u16> = None;
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let Ok(unit) = u16::from_str_radix(&hex, 16) else {
                        continue;
                    };
                    match (pending_high.take(), unit) {
                        (Some(high), 0xDC00..=0xDFFF) => {
                            out.extend(char::decode_utf16([high, unit]).flatten())
                        }
                        (_, 0xD800..=0xDBFF) => pending_high = Some(unit),
                        (_, u) => out.extend(char::decode_utf16([u]).flatten()),
                    }
                }
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{8}'),
                Some('f') => out.push('\u{c}'),
                Some(other) => out.push(other),
                None => {}
            }
        }
        Cow::Owned(out)
    }

    fn peek(&self) -> Option<u8> {
        (self.pos < self.end).then(|| self.src[self.pos])
    }

    fn char_len(&self) -> usize {
        self.text[self.pos.min(self.end)..self.end]
            .chars()
            .next()
            .map_or(0, char::len_utf8)
    }

    fn unexpected(&self, expected: &str) -> Halt {
        let len = self.char_len();
        let found = match self.text[self.pos.min(self.end)..self.end].chars().next() {
            None => "the end".to_owned(),
            Some(c) if c.is_control() => format!("{c:?}"),
            Some(c) => format!("'{c}'"),
        };
        Halt::Syntax(
            self.pos as u32..(self.pos + len) as u32,
            format!("expected {expected}, found {found}"),
        )
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::{Severity, Stop, dump};
    use super::*;

    pub fn parse(s: &str) -> Tree {
        Json.parse(s, &Limits::default(), &AtomicBool::new(false))
    }

    #[test]
    fn object_with_every_value_kind() {
        let s = r#"{"a": 1, "b": [true, null, "x"], "c": {"d": -2.5e3}}"#;
        let t = parse(s);
        assert_eq!(
            dump(&t, s),
            "\
Document [1] @0..52
  Object [3] @0..52
    Scalar(Number) \"a\" = 1 @1..7
    Array \"b\" [3] @9..31
      Scalar(Boolean) = true @15..19
      Scalar(Null) = null @21..25
      Scalar(String) = \"x\" @27..30
    Object \"c\" [1] @33..51
      Scalar(Number) \"d\" = -2.5e3 @39..50
"
        );
        assert!(t.diagnostics.is_empty());
        assert_eq!(t.stopped, None);
    }

    #[test]
    fn scalars_at_the_top_and_whitespace() {
        for s in ["42", " \"s\" ", "\n[]\n", "{}", "null"] {
            let t = parse(s);
            assert!(t.diagnostics.is_empty(), "{s:?}: {:?}", t.diagnostics);
            assert_eq!(t.node(super::super::ROOT).child_count(), 1);
        }
    }

    #[test]
    fn syntax_errors_keep_the_prefix() {
        let s = r#"{"a": 1, "b": [1, 2 "c"]}"#;
        let t = parse(s);
        assert_eq!(t.stopped, Some(Stop::Error));
        assert_eq!(t.errors(), 1);
        assert!(t.diagnostics[0].message.contains("expected ',' or ']'"));
        assert_eq!(t.diagnostics[0].span, 20..21);
        let d = dump(&t, s);
        assert!(d.contains("Scalar(Number) = 2"), "{d}");
    }

    #[test]
    fn error_messages() {
        let cases = [
            ("", "no JSON value"),
            ("{", "expected a key in double quotes or '}', found the end"),
            ("[1,", "expected a value or ']', found the end"),
            ("{\"a\" 1}", "expected ':' after the key, found '1'"),
            ("\"abc", "unterminated string"),
            ("01", "leading zeros"),
            ("1.", "a digit was expected after '.'"),
            ("-", "a digit was expected"),
            ("1e", "exponent"),
            ("nul", "expected a value"),
            ("[1] 2", "expected the end after the JSON value"),
            ("{'a': 1}", "expected a key in double quotes"),
            ("/* never closed", "unterminated comment"),
        ];
        for (s, want) in cases {
            let t = parse(s);
            assert_eq!(t.stopped, Some(Stop::Error), "{s:?}");
            let msg = &t
                .diagnostics
                .iter()
                .find(|d| d.severity == Severity::Error)
                .unwrap()
                .message;
            assert!(msg.contains(want), "{s:?}: {msg}");
        }
    }

    #[test]
    fn duplicate_keys_are_reported_even_when_escaped() {
        let s = r#"{"a": 1, "\u0061": 2, "b": {"a": 3}}"#;
        let t = parse(s);
        let warnings: Vec<_> = t
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .collect();
        assert_eq!(warnings.len(), 1, "{:?}", t.diagnostics);
        assert!(warnings[0].message.contains("duplicate key"));
        assert_eq!(warnings[0].span, 9..17);
    }

    #[test]
    fn large_integers_are_flagged() {
        let t = parse("[9007199254740991, 9007199254740992, -12345678901234567890, 1.5e300]");
        let infos: Vec<_> = t.diagnostics.iter().map(|d| d.span.clone()).collect();
        assert_eq!(infos, [19..35, 37..58]);
    }

    #[test]
    fn comments_and_trailing_commas_warn_once() {
        let s = "{\n // one\n \"a\": [1, 2,], /* two */ \"b\": 3,\n}";
        let t = parse(s);
        assert_eq!(t.stopped, None);
        assert_eq!(t.errors(), 0);
        assert_eq!(t.diagnostics.len(), 2, "{:?}", t.diagnostics);
    }

    #[test]
    fn string_problems_are_errors_but_parsing_goes_on() {
        let s = "[\"a\\qb\", \"tab\there\", \"\\u12\"]";
        let t = parse(s);
        assert_eq!(t.stopped, None);
        assert_eq!(t.errors(), 3, "{:?}", t.diagnostics);
    }

    #[test]
    fn depth_limit_stops_deep_nesting() {
        let s = "[".repeat(300) + &"]".repeat(300);
        let t = parse(&s);
        assert_eq!(t.stopped, Some(Stop::Depth));
        let s = "[".repeat(200) + &"]".repeat(200);
        assert_eq!(parse(&s).stopped, None);
    }

    #[test]
    fn cancellation_stops_a_large_parse() {
        let s = format!("[{}0]", "0,".repeat(100_000));
        let cancel = AtomicBool::new(true);
        let t = Json.parse(&s, &Limits::default(), &cancel);
        assert_eq!(t.stopped, Some(Stop::Cancelled));
    }

    #[test]
    fn path_at_finds_the_value_under_the_cursor() {
        let s = r#"{"a": {"b": [10, 20]}}"#;
        let t = parse(s);
        let at = s.find("20").unwrap() as u32;
        let path = t.path_at(at);
        let leaf = t.node(*path.last().unwrap());
        assert_eq!(&s[leaf.span.start as usize..leaf.span.end as usize], "20");
        assert_eq!(path.len(), 5, "document, object, b, array, 20");
    }

    /// Small deterministic generator, so failures reproduce without a dependency.
    pub struct Rng(pub u64);

    impl Rng {
        pub fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        pub fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    #[test]
    fn random_and_mutated_input_never_panics() {
        let seeds = [
            r#"{"a": [1, 2.5e-3, "x\n\u00e9", true, null], "b": {"c": {}}}"#,
            "[[[]], {}, \"\", -0, 1E+2]",
        ];
        let alphabet: &[u8] = b"{}[],:\"\\0123456789.eE+-tfnul /*\n\t\x01xyz";
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for round in 0..20_000 {
            let mut bytes: Vec<u8> = if round % 2 == 0 {
                seeds[rng.below(seeds.len())].as_bytes().to_vec()
            } else {
                (0..rng.below(40))
                    .map(|_| alphabet[rng.below(alphabet.len())])
                    .collect()
            };
            for _ in 0..rng.below(4) {
                if bytes.is_empty() {
                    break;
                }
                let i = rng.below(bytes.len());
                match rng.below(3) {
                    0 => bytes[i] = alphabet[rng.below(alphabet.len())],
                    1 => {
                        bytes.remove(i);
                    }
                    _ => bytes.insert(i, alphabet[rng.below(alphabet.len())]),
                }
            }
            let s = String::from_utf8_lossy(&bytes);
            let t = parse(&s);
            for id in 0..t.len() as u32 {
                let n = t.node(id);
                assert!(
                    n.span.start <= n.span.end && n.span.end as usize <= s.len(),
                    "{s:?}"
                );
            }
            let _ = t.path_at(rng.below(s.len() + 1) as u32);
        }
    }
}
