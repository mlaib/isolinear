//! A small read-only JSON document for standards: values with their byte spans. Numbers keep
//! their text as written; duplicate keys are kept in order (the first is used by lookups).
use std::ops::Range;

pub const MAX_VALUES: usize = 8_000_000;
pub const MAX_DEPTH: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// The number as written.
    Number(String),
    String(String),
    Array(Vec<Node>),
    Object(Vec<(String, Node)>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub value: Value,
    pub span: Range<u64>,
}

impl Node {
    pub fn get(&self, key: &str) -> Option<&Node> {
        match &self.value {
            Value::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn items(&self) -> &[Node] {
        match &self.value {
            Value::Array(items) => items,
            _ => &[],
        }
    }
    pub fn str(&self) -> Option<&str> {
        match &self.value {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
    /// A scalar as text (strings unquoted); objects and arrays summarized.
    pub fn text(&self) -> String {
        match &self.value {
            Value::Null => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.clone(),
            Value::String(s) => s.clone(),
            Value::Array(items) => format!("[{} items]", items.len()),
            Value::Object(fields) => format!("{{{} keys}}", fields.len()),
        }
    }
    pub fn is_scalar(&self) -> bool {
        !matches!(self.value, Value::Array(_) | Value::Object(_))
    }
}

struct Parser<'a> {
    b: &'a [u8],
    at: usize,
    count: usize,
}

pub fn parse(source: &str) -> Result<Node, String> {
    let mut p = Parser {
        b: source.as_bytes(),
        at: 0,
        count: 0,
    };
    // A byte-order mark is allowed at the start.
    if source.starts_with('\u{feff}') {
        p.at = 3;
    }
    let node = p.value(0)?;
    p.ws();
    if p.at < p.b.len() {
        return Err(format!(
            "Unexpected text after the JSON value at byte {}",
            p.at
        ));
    }
    Ok(node)
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.at < self.b.len() && matches!(self.b[self.at], b' ' | b'\t' | b'\n' | b'\r') {
            self.at += 1;
        }
    }
    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at byte {}", self.at))
    }
    fn value(&mut self, depth: usize) -> Result<Node, String> {
        if depth > MAX_DEPTH {
            return self.err("Nesting too deep");
        }
        self.count += 1;
        if self.count > MAX_VALUES {
            return self.err("Too many values");
        }
        self.ws();
        let start = self.at;
        let value = match self.b.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                let mut fields = vec![];
                self.ws();
                if self.b.get(self.at) == Some(&b'}') {
                    self.at += 1;
                } else {
                    loop {
                        self.ws();
                        if self.b.get(self.at) != Some(&b'"') {
                            return self.err("Expected a key");
                        }
                        let key = self.string()?;
                        self.ws();
                        if self.b.get(self.at) != Some(&b':') {
                            return self.err("Expected ':'");
                        }
                        self.at += 1;
                        let v = self.value(depth + 1)?;
                        fields.push((key, v));
                        self.ws();
                        match self.b.get(self.at) {
                            Some(b',') => self.at += 1,
                            Some(b'}') => {
                                self.at += 1;
                                break;
                            }
                            _ => return self.err("Expected ',' or '}'"),
                        }
                    }
                }
                Value::Object(fields)
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = vec![];
                self.ws();
                if self.b.get(self.at) == Some(&b']') {
                    self.at += 1;
                } else {
                    loop {
                        items.push(self.value(depth + 1)?);
                        self.ws();
                        match self.b.get(self.at) {
                            Some(b',') => self.at += 1,
                            Some(b']') => {
                                self.at += 1;
                                break;
                            }
                            _ => return self.err("Expected ',' or ']'"),
                        }
                    }
                }
                Value::Array(items)
            }
            Some(b'"') => Value::String(self.string()?),
            Some(b't') if self.b[self.at..].starts_with(b"true") => {
                self.at += 4;
                Value::Bool(true)
            }
            Some(b'f') if self.b[self.at..].starts_with(b"false") => {
                self.at += 5;
                Value::Bool(false)
            }
            Some(b'n') if self.b[self.at..].starts_with(b"null") => {
                self.at += 4;
                Value::Null
            }
            Some(c) if *c == b'-' || c.is_ascii_digit() => {
                while self.at < self.b.len()
                    && matches!(
                        self.b[self.at],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.at += 1;
                }
                Value::Number(String::from_utf8_lossy(&self.b[start..self.at]).into_owned())
            }
            _ => return self.err("Expected a value"),
        };
        Ok(Node {
            value,
            span: start as u64..self.at as u64,
        })
    }
    fn string(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let run = self.b[self.at..]
                .iter()
                .position(|&c| c == b'"' || c == b'\\')
                .map(|i| self.at + i);
            let Some(stop) = run else {
                return self.err("Unterminated string");
            };
            out.push_str(&String::from_utf8_lossy(&self.b[self.at..stop]));
            self.at = stop + 1;
            if self.b[stop] == b'"' {
                return Ok(out);
            }
            let c = *self.b.get(self.at).ok_or("Unterminated escape")?;
            self.at += 1;
            match c {
                b'n' => out.push('\n'),
                b't' => out.push('\t'),
                b'r' => out.push('\r'),
                b'b' => out.push('\u{8}'),
                b'f' => out.push('\u{c}'),
                b'u' => {
                    let hex = |p: &mut Self| -> Option<u32> {
                        let h = std::str::from_utf8(p.b.get(p.at..p.at + 4)?).ok()?;
                        p.at += 4;
                        u32::from_str_radix(h, 16).ok()
                    };
                    let mut code = hex(self).ok_or("Bad \\u escape")?;
                    if (0xd800..0xdc00).contains(&code) && self.b[self.at..].starts_with(b"\\u") {
                        self.at += 2;
                        let low = hex(self).ok_or("Bad \\u escape")?;
                        code =
                            0x10000 + ((code - 0xd800) << 10) + (low.wrapping_sub(0xdc00) & 0x3ff);
                    }
                    out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                }
                other => out.push(other as char),
            }
        }
    }
}

/// Top-level object keys from the start of a file (for recognition), without parsing it all.
pub fn top_keys(head: &str) -> Vec<String> {
    // Keys at depth 1: scan strings followed by ':' while tracking nesting.
    let b = head.as_bytes();
    let (mut depth, mut i, mut keys) = (0i32, 0usize, vec![]);
    while i < b.len() && keys.len() < 64 {
        match b[i] {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                let end = i.min(b.len());
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if depth == 1 && b.get(j) == Some(&b':') {
                    keys.push(String::from_utf8_lossy(&b[start..end]).into_owned());
                }
            }
            _ => {}
        }
        i += 1;
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn values_spans_and_escapes() {
        let src = r#"{"a": [1, 2.50e3, true, null], "s": "x\"yé😀", "o": {}}"#;
        let n = parse(src).unwrap();
        let a = n.get("a").unwrap();
        assert_eq!(a.items()[1].text(), "2.50e3");
        assert_eq!(
            &src[a.span.start as usize..a.span.end as usize],
            "[1, 2.50e3, true, null]"
        );
        assert_eq!(n.get("s").unwrap().str(), Some("x\"yé😀"));
        assert_eq!(n.get("o").unwrap().text(), "{0 keys}");
        assert!(parse("{\"a\":}").is_err());
        assert!(parse("[1,2").is_err());
        assert!(parse("[1] x").is_err());
        assert!(parse(&"[".repeat(300)).is_err());
        assert_eq!(
            top_keys(r#"{"resourceType": "Bundle", "entry": [{"x": 1}]}"#),
            vec!["resourceType", "entry"]
        );
    }
}
