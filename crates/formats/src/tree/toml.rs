//! TOML structural index over retained source (TOML 1.0, plus the 1.1 relaxations: newlines and
//! trailing commas in inline tables, `\e` and `\xHH` escapes, times without seconds).
//!
//! The tree follows the source: key/value pairs before the first header are top-level nodes, and
//! every `[table]` or `[[array of tables]]` header is a node whose children are the pairs below it,
//! in source order. Tables are never merged or re-ordered. Dotted keys stay one node named by the
//! dotted key as written. Redefinitions (duplicate keys, a table defined twice, a key that is
//! both a value and a table) are warnings; nodes are kept, never hidden. Syntax errors stop the
//! index and keep the parsed prefix.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Builder, Kind, Limits, Scalar, Stop, Tree, TreeFormat};

pub const FORMAT: Option<&'static dyn TreeFormat> = Some(&Toml);

pub struct Toml;

enum Fail {
    Syntax(usize, String),
    Stopped,
    Cancelled(usize),
}

impl TreeFormat for Toml {
    fn parse(&self, source: &str, limits: &Limits, cancel: &AtomicBool) -> Tree {
        let start = if source.starts_with('\u{FEFF}') { 3 } else { 0 };
        let mut p = Parser {
            src: source,
            b: source.as_bytes(),
            pos: start,
            tree: Builder::new(source.len(), limits, cancel),
            stack: Vec::new(),
            table: false,
            scope: 0,
            keys: Keys::new(),
            cancel,
            last_end: start,
        };
        let result = p.poll(start).and_then(|()| p.run());
        match result {
            Ok(()) => {
                p.close_table();
                p.tree.finish(source.len() as u32)
            }
            Err(Fail::Syntax(offset, message)) => {
                p.tree.fail(offset as u32..offset as u32, message);
                p.tree.finish(p.pos.max(p.last_end).max(offset) as u32)
            }
            Err(Fail::Stopped) => p.tree.finish(p.pos.max(p.last_end) as u32),
            Err(Fail::Cancelled(at)) => {
                let mut tree = p.tree.finish(at.max(p.last_end) as u32);
                tree.stopped = Some(Stop::Cancelled);
                tree
            }
        }
    }
}

/// What a dotted path names, for redefinition checks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Def {
    /// Created as the parent of a header, e.g. `a` in `[a.b]`.
    Implicit,
    /// Created by a dotted key, e.g. `a` in `a.b = 1`.
    Dotted,
    /// Defined by a `[header]`.
    Table,
    /// Array of tables; holds the id of its current element.
    Array(u32),
    /// A value (inline tables included: they are sealed).
    Value,
}

/// Interned key paths: each id is one table/key; children are keyed by their decoded name.
/// Memory grows with the number of key occurrences in the source, never with path length.
struct Keys {
    defs: Vec<(Def, HashMap<Box<str>, u32>)>,
}
impl Keys {
    fn new() -> Self {
        Self {
            defs: vec![(Def::Table, HashMap::new())],
        }
    }
    fn anon(&mut self, def: Def) -> u32 {
        self.defs.push((def, HashMap::new()));
        (self.defs.len() - 1) as u32
    }
    /// The child `name` of `parent`, created with `def` when absent; and whether it existed.
    fn child(&mut self, parent: u32, name: &str, def: Def) -> (u32, bool) {
        if let Some(&id) = self.defs[parent as usize].1.get(name) {
            return (id, true);
        }
        let id = self.anon(def);
        self.defs[parent as usize].1.insert(name.into(), id);
        (id, false)
    }
    fn def(&self, id: u32) -> Def {
        self.defs[id as usize].0
    }
    fn set(&mut self, id: u32, def: Def) {
        self.defs[id as usize].0 = def;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Array,
    /// Inline table with the key scope of its members.
    Inline(u32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    /// A value is required here.
    Value,
    /// After `[` or `,` in an array: a value or `]`.
    Element,
    /// After a value inside a container: `,` or the closing bracket.
    After,
    /// After `{` or `,` in an inline table: a key or `}`.
    Member,
}

struct Open {
    ctx: Ctx,
}

struct Parser<'a, 'c> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
    tree: Builder<'c>,
    stack: Vec<Open>,
    /// Open table header node.
    table: bool,
    /// Key scope of the current table.
    scope: u32,
    keys: Keys,
    cancel: &'c AtomicBool,
    last_end: usize,
}

/// A parsed (possibly dotted) key: span as written and decoded segments.
struct Key {
    start: usize,
    end: usize,
    segments: Vec<String>,
}
impl Key {
    fn display(&self) -> String {
        self.segments
            .iter()
            .map(|s| {
                if !s.is_empty()
                    && s.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                {
                    s.clone()
                } else {
                    format!("{s:?}")
                }
            })
            .collect::<Vec<_>>()
            .join(".")
    }
}

fn describe(source: &str, pos: usize) -> String {
    match source.get(pos..).and_then(|s| s.chars().next()) {
        Some('\n') | Some('\r') => "end of line".into(),
        Some(c) if c.is_control() => format!("U+{:04X}", c as u32),
        Some(c) => format!("'{c}'"),
        None => "end of input".into(),
    }
}

fn bare(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

impl Parser<'_, '_> {
    fn run(&mut self) -> Result<(), Fail> {
        loop {
            self.poll(self.pos)?;
            self.skip_blank_lines()?;
            let Some(&c) = self.b.get(self.pos) else {
                return Ok(());
            };
            if c == b'[' {
                self.header()?;
            } else {
                let scope = self.scope;
                self.pair(scope)?;
            }
            self.end_of_line()?;
        }
    }

    fn poll(&self, at: usize) -> Result<(), Fail> {
        if self.cancel.load(Ordering::Relaxed) {
            // A scanner can be inside a UTF-8 character when cancellation arrives.
            let mut end = at;
            while !self.src.is_char_boundary(end) {
                end -= 1;
            }
            return Err(Fail::Cancelled(end));
        }
        Ok(())
    }

    fn unexpected(&self, expected: &str) -> Fail {
        Fail::Syntax(
            self.pos,
            format!(
                "expected {expected}, found {}",
                describe(self.src, self.pos)
            ),
        )
    }

    fn warn(&mut self, offset: usize, message: String) {
        self.tree.warning(offset as u32..offset as u32, message);
    }

    fn skip_space(&mut self) -> Result<(), Fail> {
        while matches!(self.b.get(self.pos), Some(b' ' | b'\t')) {
            if self.pos.is_multiple_of(4096) {
                self.poll(self.pos)?;
            }
            self.pos += 1;
        }
        Ok(())
    }

    fn skip_comment(&mut self) -> Result<(), Fail> {
        if self.b.get(self.pos) != Some(&b'#') {
            return Ok(());
        }
        while let Some(&c) = self.b.get(self.pos) {
            if self.pos.is_multiple_of(4096) {
                self.poll(self.pos)?;
            }
            match c {
                b'\n' => break,
                b'\r' if self.b.get(self.pos + 1) == Some(&b'\n') => break,
                b'\t' => {}
                0..=0x1F | 0x7F => {
                    return Err(Fail::Syntax(
                        self.pos,
                        format!("Control character U+{c:04X} is not allowed in a comment"),
                    ));
                }
                _ => {}
            }
            self.pos += 1;
        }
        Ok(())
    }

    /// Whitespace, comments and line breaks between top-level statements or array elements.
    fn skip_blank_lines(&mut self) -> Result<(), Fail> {
        loop {
            self.poll(self.pos)?;
            self.skip_space()?;
            self.skip_comment()?;
            match self.b.get(self.pos) {
                Some(b'\n') => self.pos += 1,
                Some(b'\r') if self.b.get(self.pos + 1) == Some(&b'\n') => self.pos += 2,
                _ => return Ok(()),
            }
        }
    }

    fn end_of_line(&mut self) -> Result<(), Fail> {
        self.skip_space()?;
        self.skip_comment()?;
        match self.b.get(self.pos) {
            None | Some(b'\n') => Ok(()),
            Some(b'\r') if self.b.get(self.pos + 1) == Some(&b'\n') => Ok(()),
            _ => Err(self.unexpected("a line break")),
        }
    }

    fn close_table(&mut self) {
        if self.table {
            self.tree.close(self.last_end as u32);
            self.table = false;
        }
    }

    /// `[table]` or `[[array of tables]]`.
    fn header(&mut self) -> Result<(), Fail> {
        let start = self.pos;
        let array = self.b.get(start + 1) == Some(&b'[');
        self.pos += if array { 2 } else { 1 };
        self.skip_space()?;
        let key = self.key()?;
        self.skip_space()?;
        let close: &[u8] = if array { b"]]" } else { b"]" };
        if !self.b[self.pos..].starts_with(close) {
            return Err(self.unexpected(if array { "']]'" } else { "']'" }));
        }
        self.pos += close.len();
        self.close_table();
        self.last_end = self.pos;
        self.tree
            .open(
                Kind::Table,
                start as u32,
                Some(start as u32..self.pos as u32),
            )
            .map_err(|_| Fail::Stopped)?;
        self.table = true;
        self.scope = self.resolve_header(&key, array);
        Ok(())
    }

    /// Walk the parent segments of a header path; returns the parent id, or None on a conflict.
    fn header_parent(&mut self, key: &Key) -> Option<u32> {
        let mut id = 0;
        let last = key.segments.len().saturating_sub(1);
        for (i, seg) in key.segments[..last].iter().enumerate() {
            let (child, _) = self.keys.child(id, seg, Def::Implicit);
            id = match self.keys.def(child) {
                Def::Array(current) => current,
                Def::Value => {
                    let path = Key {
                        segments: key.segments[..=i].to_vec(),
                        ..*key
                    };
                    let message = format!(
                        "Key {} is already defined as a value; it cannot hold table [{}]",
                        preview(&path.display(), 80),
                        preview(&key.display(), 80)
                    );
                    self.warn(key.start, message);
                    return None;
                }
                _ => child,
            };
        }
        Some(id)
    }

    fn resolve_header(&mut self, key: &Key, array: bool) -> u32 {
        let Some(parent) = self.header_parent(key) else {
            return self.keys.anon(Def::Table);
        };
        let last = key.segments.last().map_or("", String::as_str);
        let shown = preview(&key.display(), 80);
        let (id, existed) = self.keys.child(parent, last, Def::Table);
        if array {
            match (existed, self.keys.def(id)) {
                (false, _) | (true, Def::Array(_)) => {
                    let element = self.keys.anon(Def::Table);
                    self.keys.set(id, Def::Array(element));
                    element
                }
                (true, _) => {
                    let message =
                        format!("[[{shown}]] redefines {shown}, which is not an array of tables");
                    self.warn(key.start, message);
                    self.keys.anon(Def::Table)
                }
            }
        } else if !existed {
            id
        } else {
            let problem = match self.keys.def(id) {
                Def::Implicit => {
                    self.keys.set(id, Def::Table);
                    return id;
                }
                Def::Table => format!("Duplicate table [{shown}]"),
                Def::Dotted => format!("Table [{shown}] was already defined by dotted keys"),
                Def::Array(_) => format!("Table [{shown}] is already an array of tables"),
                Def::Value => format!("Table [{shown}] is already defined as a value"),
            };
            self.warn(key.start, problem);
            if self.keys.def(id) == Def::Table {
                id
            } else {
                self.keys.anon(Def::Table)
            }
        }
    }

    /// Define the dotted key `key` in `scope`; returns the scope for an inline-table value.
    fn resolve_pair(&mut self, scope: u32, key: &Key) -> u32 {
        let mut id = scope;
        let last = key.segments.len().saturating_sub(1);
        let shown = preview(&key.display(), 80);
        for seg in &key.segments[..last] {
            let (child, _) = self.keys.child(id, seg, Def::Dotted);
            id = match self.keys.def(child) {
                Def::Dotted | Def::Implicit => child,
                Def::Table | Def::Array(_) => {
                    let message =
                        format!("Dotted key {shown} extends a table defined by a header elsewhere");
                    self.warn(key.start, message);
                    match self.keys.def(child) {
                        Def::Array(current) => current,
                        _ => child,
                    }
                }
                Def::Value => {
                    let message =
                        format!("Dotted key {shown} extends a key that is already a value");
                    self.warn(key.start, message);
                    return self.keys.anon(Def::Value);
                }
            };
        }
        let name = key.segments.last().map_or("", String::as_str);
        let (id, existed) = self.keys.child(id, name, Def::Value);
        if existed {
            let message = match self.keys.def(id) {
                Def::Value => format!("Duplicate key {shown}"),
                _ => format!("Key {shown} is already defined as a table"),
            };
            self.warn(key.start, message);
            return self.keys.anon(Def::Value);
        }
        id
    }

    /// A simple or dotted key.
    fn key(&mut self) -> Result<Key, Fail> {
        let start = self.pos;
        let mut segments = Vec::new();
        loop {
            self.poll(self.pos)?;
            let b = self.b;
            let seg_start = self.pos;
            match b.get(seg_start) {
                Some(b'"') => {
                    let end = self.basic_string(seg_start, false)?;
                    segments.push(decode_basic(&self.src[seg_start + 1..end - 1]));
                    self.pos = end;
                }
                Some(b'\'') => {
                    let end = self.literal_string(seg_start, false)?;
                    segments.push(self.src[seg_start + 1..end - 1].to_owned());
                    self.pos = end;
                }
                Some(&c) if bare(c) => {
                    let mut i = seg_start;
                    while b.get(i).is_some_and(|&c| bare(c)) {
                        if i.is_multiple_of(4096) {
                            self.poll(i)?;
                        }
                        i += 1;
                    }
                    segments.push(self.src[seg_start..i].to_owned());
                    self.pos = i;
                }
                _ => return Err(self.unexpected("a key")),
            }
            let end = self.pos;
            self.skip_space()?;
            if self.b.get(self.pos) == Some(&b'.') {
                self.pos += 1;
                self.skip_space()?;
            } else {
                self.pos = end;
                return Ok(Key {
                    start,
                    end,
                    segments,
                });
            }
        }
    }

    /// `key = value`, defined in key scope `scope`.
    fn pair(&mut self, scope: u32) -> Result<(), Fail> {
        let key = self.key()?;
        self.skip_space()?;
        if self.b.get(self.pos) != Some(&b'=') {
            return Err(self.unexpected("'=' after the key"));
        }
        self.pos += 1;
        self.skip_space()?;
        let inline_scope = self.resolve_pair(scope, &key);
        self.value(Some((key.start as u32, key.end as u32)), inline_scope)
    }

    /// One complete value, including arrays and inline tables.
    fn value(&mut self, name: Option<(u32, u32)>, scope: u32) -> Result<(), Fail> {
        let base = self.stack.len();
        let mut name = name;
        let mut scope = Some(scope);
        let mut st = St::Value;
        loop {
            self.poll(self.pos)?;
            let in_array = matches!(self.stack.last().map(|o| o.ctx), Some(Ctx::Array));
            if st != St::Value {
                // Arrays, and inline tables since TOML 1.1, may span lines and hold comments.
                self.skip_blank_lines()?;
            }
            let Some(&c) = self.b.get(self.pos) else {
                let what = match st {
                    St::Value | St::Element => "a value",
                    St::Member => "a key",
                    St::After if in_array => "',' or ']'",
                    St::After => "',' or '}'",
                };
                return Err(Fail::Syntax(
                    self.pos,
                    format!("Unexpected end of input where {what} was expected"),
                ));
            };
            match st {
                St::Element | St::After if in_array && c == b']' => {
                    self.close();
                    if self.stack.len() <= base {
                        return Ok(());
                    }
                    st = St::After;
                }
                St::Member | St::After if !in_array && c == b'}' => {
                    self.close();
                    if self.stack.len() <= base {
                        return Ok(());
                    }
                    st = St::After;
                }
                St::After => {
                    if c != b',' {
                        return Err(self.unexpected(if in_array {
                            "',' or ']'"
                        } else {
                            "',' or '}'"
                        }));
                    }
                    self.pos += 1;
                    st = if in_array { St::Element } else { St::Member };
                }
                St::Member => {
                    let Some(Open {
                        ctx: Ctx::Inline(inline),
                        ..
                    }) = self.stack.last()
                    else {
                        return Err(self.unexpected("a key"));
                    };
                    let inline = *inline;
                    let key = self.key()?;
                    self.skip_space()?;
                    if self.b.get(self.pos) != Some(&b'=') {
                        return Err(self.unexpected("'=' after the key"));
                    }
                    self.pos += 1;
                    self.skip_space()?;
                    scope = Some(self.resolve_pair(inline, &key));
                    name = Some((key.start as u32, key.end as u32));
                    st = St::Value;
                }
                St::Value | St::Element => {
                    let start = self.pos;
                    match c {
                        b'[' | b'{' => {
                            let (kind, ctx) = if c == b'[' {
                                (Kind::Array, Ctx::Array)
                            } else {
                                let id = scope.take().unwrap_or_else(|| self.keys.anon(Def::Value));
                                (Kind::Object, Ctx::Inline(id))
                            };
                            let name = name.take().map(|(s, e)| s..e);
                            let node_start = name.as_ref().map_or(start as u32, |n| n.start);
                            self.tree
                                .open(kind, node_start, name)
                                .map_err(|_| Fail::Stopped)?;
                            self.stack.push(Open { ctx });
                            self.pos += 1;
                            st = if c == b'[' { St::Element } else { St::Member };
                        }
                        _ => {
                            let (kind, end) = self.scalar()?;
                            let name = name.take().map(|(s, e)| s..e);
                            let node_start = name.as_ref().map_or(start as u32, |n| n.start);
                            self.tree
                                .leaf(
                                    Kind::Scalar,
                                    Some(kind),
                                    node_start..end as u32,
                                    name,
                                    Some(start as u32..end as u32),
                                )
                                .map_err(|_| Fail::Stopped)?;
                            self.last_end = self.last_end.max(end);
                            scope = None;
                            self.pos = end;
                            if self.stack.len() <= base {
                                return Ok(());
                            }
                            st = St::After;
                        }
                    }
                }
            }
        }
    }

    fn close(&mut self) {
        let end = self.pos + 1;
        if self.stack.pop().is_some() {
            self.tree.close(end as u32);
        }
        self.pos = end;
        self.last_end = self.last_end.max(end);
    }

    /// A scalar value at `self.pos`: its kind and end.
    fn scalar(&self) -> Result<(Scalar, usize), Fail> {
        let b = self.b;
        let start = self.pos;
        match b[start] {
            b'"' => {
                let multi = b[start..].starts_with(b"\"\"\"");
                Ok((Scalar::String, self.basic_string(start, multi)?))
            }
            b'\'' => {
                let multi = b[start..].starts_with(b"'''");
                Ok((Scalar::String, self.literal_string(start, multi)?))
            }
            _ => {
                let mut end = start;
                while b
                    .get(end)
                    .is_some_and(|&c| c.is_ascii_alphanumeric() || b"_+-.:".contains(&c))
                {
                    if end.is_multiple_of(4096) {
                        self.poll(end)?;
                    }
                    end += 1;
                }
                // A space may separate a date from its time: 1979-05-27 07:32:00.
                if is_date(&b[start..end])
                    && b.get(end) == Some(&b' ')
                    && b.get(end + 1).is_some_and(u8::is_ascii_digit)
                {
                    end += 1;
                    while b
                        .get(end)
                        .is_some_and(|&c| c.is_ascii_alphanumeric() || b"_+-.:".contains(&c))
                    {
                        if end.is_multiple_of(4096) {
                            self.poll(end)?;
                        }
                        end += 1;
                    }
                }
                if end == start {
                    return Err(Fail::Syntax(
                        start,
                        format!(
                            "Unexpected {} where a value was expected",
                            describe(self.src, start)
                        ),
                    ));
                }
                match classify(&self.src[start..end]) {
                    Some(kind) => Ok((kind, end)),
                    None => Err(Fail::Syntax(
                        start,
                        format!(
                            "Invalid value {}: expected a string, number, boolean, date-time, array or inline table",
                            preview(&self.src[start..end], 40)
                        ),
                    )),
                }
            }
        }
    }

    /// End of a basic string starting at `start` (after the closing quotes).
    fn basic_string(&self, start: usize, multi: bool) -> Result<usize, Fail> {
        let b = self.b;
        let mut i = start + if multi { 3 } else { 1 };
        let unterminated = || {
            Fail::Syntax(
                start,
                "Unterminated string: missing closing quote".to_owned(),
            )
        };
        let mut steps = 0usize;
        loop {
            steps += 1;
            if steps.is_multiple_of(4096) {
                self.poll(i)?;
            }
            let Some(&c) = b.get(i) else {
                return Err(unterminated());
            };
            match c {
                b'"' => {
                    if !multi {
                        return Ok(i + 1);
                    }
                    if b[i..].starts_with(b"\"\"\"") {
                        // Up to two quotes may directly precede the closing delimiter.
                        let mut end = i + 3;
                        let mut extra = 0;
                        while extra < 2 && b.get(end) == Some(&b'"') {
                            end += 1;
                            extra += 1;
                        }
                        return Ok(end);
                    }
                    i += 1;
                }
                b'\\' => {
                    let escape = i;
                    let width = match b.get(i + 1) {
                        Some(b'b' | b't' | b'n' | b'f' | b'r' | b'e' | b'"' | b'\\') => 0,
                        Some(b'x') => 2,
                        Some(b'u') => 4,
                        Some(b'U') => 8,
                        Some(b' ' | b'\t' | b'\r' | b'\n') if multi => {
                            // Line-ending backslash: only whitespace may follow on its line.
                            let mut j = i + 1;
                            while matches!(b.get(j), Some(b' ' | b'\t')) {
                                if j.is_multiple_of(4096) {
                                    self.poll(j)?;
                                }
                                j += 1;
                            }
                            match b.get(j) {
                                Some(b'\n') => {}
                                Some(b'\r') if b.get(j + 1) == Some(&b'\n') => {}
                                _ => {
                                    return Err(Fail::Syntax(
                                        escape,
                                        "Invalid escape: a backslash followed by spaces must end the line".into(),
                                    ));
                                }
                            }
                            i = j + 1;
                            continue;
                        }
                        None => return Err(unterminated()),
                        _ => {
                            return Err(Fail::Syntax(
                                escape,
                                format!(
                                    "Invalid escape sequence \\{}",
                                    describe(self.src, i + 1).trim_matches('\'')
                                ),
                            ));
                        }
                    };
                    let digits = b.get(i + 2..i + 2 + width);
                    let code = digits
                        .filter(|d| d.iter().all(u8::is_ascii_hexdigit))
                        .and_then(|d| std::str::from_utf8(d).ok())
                        .and_then(|d| u32::from_str_radix(d, 16).ok());
                    if width > 0 && code.and_then(char::from_u32).is_none() {
                        return Err(Fail::Syntax(
                            escape,
                            "Invalid escape: expected a Unicode scalar value in hexadecimal".into(),
                        ));
                    }
                    i += 2 + width;
                }
                b'\n' if multi => i += 1,
                b'\r' if multi && b.get(i + 1) == Some(&b'\n') => i += 2,
                b'\n' | b'\r' => return Err(unterminated()),
                b'\t' => i += 1,
                0..=0x1F | 0x7F => {
                    return Err(Fail::Syntax(
                        i,
                        format!("Control character U+{c:04X} must be escaped in a string"),
                    ));
                }
                _ => i += 1,
            }
        }
    }

    /// End of a literal string starting at `start` (after the closing quotes).
    fn literal_string(&self, start: usize, multi: bool) -> Result<usize, Fail> {
        let b = self.b;
        let mut i = start + if multi { 3 } else { 1 };
        let mut steps = 0usize;
        loop {
            steps += 1;
            if steps.is_multiple_of(4096) {
                self.poll(i)?;
            }
            let Some(&c) = b.get(i) else {
                return Err(Fail::Syntax(
                    start,
                    "Unterminated string: missing closing quote".into(),
                ));
            };
            match c {
                b'\'' => {
                    if !multi {
                        return Ok(i + 1);
                    }
                    if b[i..].starts_with(b"'''") {
                        let mut end = i + 3;
                        let mut extra = 0;
                        while extra < 2 && b.get(end) == Some(&b'\'') {
                            end += 1;
                            extra += 1;
                        }
                        return Ok(end);
                    }
                    i += 1;
                }
                b'\n' if multi => i += 1,
                b'\r' if multi && b.get(i + 1) == Some(&b'\n') => i += 2,
                b'\n' | b'\r' => {
                    return Err(Fail::Syntax(
                        start,
                        "Unterminated string: missing closing quote".into(),
                    ));
                }
                b'\t' => i += 1,
                0..=0x1F | 0x7F => {
                    return Err(Fail::Syntax(
                        i,
                        format!("Control character U+{c:04X} is not allowed in a literal string"),
                    ));
                }
                _ => i += 1,
            }
        }
    }
}

fn digits2(b: &[u8]) -> Option<u32> {
    match b {
        [a, c] if a.is_ascii_digit() && c.is_ascii_digit() => {
            Some(u32::from(a - b'0') * 10 + u32::from(c - b'0'))
        }
        _ => None,
    }
}

fn is_date(b: &[u8]) -> bool {
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[7] == b'-'
        && digits2(&b[5..7]).is_some_and(|m| (1..=12).contains(&m))
        && digits2(&b[8..10]).is_some_and(|d| (1..=31).contains(&d))
}

/// `HH:MM[:SS[.fraction]]`; returns the length consumed.
fn time_len(b: &[u8]) -> Option<usize> {
    if b.len() < 5 || b[2] != b':' {
        return None;
    }
    digits2(&b[..2]).filter(|h| *h <= 23)?;
    digits2(&b[3..5]).filter(|m| *m <= 59)?;
    let mut i = 5;
    if b.get(5) == Some(&b':') {
        digits2(b.get(6..8)?).filter(|s| *s <= 60)?;
        i = 8;
        if b.get(8) == Some(&b'.') {
            let n = b[9..].iter().take_while(|c| c.is_ascii_digit()).count();
            if n == 0 {
                return None;
            }
            i = 9 + n;
        }
    }
    Some(i)
}

fn is_datetime(t: &[u8]) -> bool {
    if !is_date(t) {
        return time_len(t) == Some(t.len());
    }
    if t.len() == 10 {
        return true;
    }
    if !matches!(t[10], b'T' | b't' | b' ') {
        return false;
    }
    let Some(n) = time_len(&t[11..]) else {
        return false;
    };
    match &t[11 + n..] {
        [] | [b'Z'] | [b'z'] => true,
        [b'+' | b'-', h1, h2, b':', m1, m2] => {
            digits2(&[*h1, *h2]).is_some_and(|h| h <= 23)
                && digits2(&[*m1, *m2]).is_some_and(|m| m <= 59)
        }
        _ => false,
    }
}

/// Digits of `radix` with single underscores only between digits.
fn digits_ok(t: &[u8], digit: fn(u8) -> bool) -> bool {
    !t.is_empty()
        && digit(t[0])
        && digit(t[t.len() - 1])
        && t.iter().all(|&c| c == b'_' || digit(c))
        && !t.windows(2).any(|w| w == b"__")
}

fn is_integer(t: &[u8]) -> bool {
    for (prefix, digit) in [
        (b"0x", (|c: u8| c.is_ascii_hexdigit()) as fn(u8) -> bool),
        (b"0o", |c: u8| (b'0'..=b'7').contains(&c)),
        (b"0b", |c: u8| c == b'0' || c == b'1'),
    ] {
        if let Some(rest) = t.strip_prefix(prefix.as_slice()) {
            return digits_ok(rest, digit);
        }
    }
    let u = t
        .strip_prefix(b"+")
        .or_else(|| t.strip_prefix(b"-"))
        .unwrap_or(t);
    digits_ok(u, |c| c.is_ascii_digit()) && (u == b"0" || u[0] != b'0')
}

fn is_float(t: &[u8]) -> bool {
    let u = t
        .strip_prefix(b"+")
        .or_else(|| t.strip_prefix(b"-"))
        .unwrap_or(t);
    if u == b"inf" || u == b"nan" {
        return true;
    }
    let (mantissa, exponent) = match u.iter().position(|&c| c == b'e' || c == b'E') {
        Some(i) => (&u[..i], Some(&u[i + 1..])),
        None => (u, None),
    };
    let (int, frac) = match mantissa.iter().position(|&c| c == b'.') {
        Some(i) => (&mantissa[..i], Some(&mantissa[i + 1..])),
        None => (mantissa, None),
    };
    let dec = |s: &[u8]| digits_ok(s, |c| c.is_ascii_digit());
    if !dec(int) || (int.len() > 1 && int[0] == b'0') {
        return false;
    }
    if frac.is_none() && exponent.is_none() {
        return false;
    }
    frac.is_none_or(dec)
        && exponent.is_none_or(|e| {
            let e = e
                .strip_prefix(b"+")
                .or_else(|| e.strip_prefix(b"-"))
                .unwrap_or(e);
            dec(e)
        })
}

fn classify(t: &str) -> Option<Scalar> {
    let b = t.as_bytes();
    match t {
        "true" | "false" => Some(Scalar::Boolean),
        _ if is_datetime(b) => Some(Scalar::Date),
        _ if is_integer(b) || is_float(b) => Some(Scalar::Number),
        _ => None,
    }
}

/// Decode the inside of a basic string (escapes; line-ending backslashes in multi-line ones).
pub(crate) fn decode_basic(inner: &str) -> String {
    let b = inner.as_bytes();
    let mut out = String::with_capacity(inner.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            let next = b[i..]
                .iter()
                .position(|&c| c == b'\\')
                .map_or(b.len(), |p| i + p);
            out.push_str(inner.get(i..next).unwrap_or(""));
            i = next;
            continue;
        }
        let simple = match b.get(i + 1) {
            Some(b'b') => Some('\u{8}'),
            Some(b't') => Some('\t'),
            Some(b'n') => Some('\n'),
            Some(b'f') => Some('\u{c}'),
            Some(b'r') => Some('\r'),
            Some(b'e') => Some('\u{1b}'),
            Some(b'"') => Some('"'),
            Some(b'\\') => Some('\\'),
            _ => None,
        };
        if let Some(c) = simple {
            out.push(c);
            i += 2;
            continue;
        }
        let width = match b.get(i + 1) {
            Some(b'x') => 2,
            Some(b'u') => 4,
            Some(b'U') => 8,
            Some(b' ' | b'\t' | b'\r' | b'\n') => {
                // Line-ending backslash: drop it and all following whitespace.
                let mut j = i + 1;
                while matches!(b.get(j), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                    j += 1;
                }
                i = j;
                continue;
            }
            _ => 0,
        };
        let code = inner
            .get(i + 2..i + 2 + width)
            .filter(|_| width > 0)
            .and_then(|h| u32::from_str_radix(h, 16).ok());
        match code {
            Some(code) => {
                out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                i += 2 + width;
            }
            None => {
                out.push('\\');
                i += 1;
            }
        }
    }
    out
}

fn preview(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{Severity, dump};
    use super::*;

    fn parse(s: &str) -> Tree {
        Toml.parse(s, &Limits::default(), &AtomicBool::new(false))
    }
    fn check_spans(source: &str, tree: &Tree) {
        let mut last_start = 0;
        for id in 0..tree.len() as u32 {
            let n = tree.node(id);
            assert!(n.span.start >= last_start, "{source:?}: {n:?}");
            last_start = n.span.start;
            for r in std::iter::once(&n.span)
                .chain(n.name.iter())
                .chain(n.value.iter())
            {
                assert!(
                    r.start <= r.end && r.end as usize <= source.len(),
                    "{source:?}: {n:?}"
                );
                assert!(
                    source.is_char_boundary(r.start as usize)
                        && source.is_char_boundary(r.end as usize),
                    "{source:?}: {n:?}"
                );
                assert!(
                    n.span.start <= r.start && r.end <= n.span.end,
                    "{source:?}: {n:?}"
                );
            }
            if let Some(parent) = n.parent() {
                let p = tree.node(parent);
                assert!(
                    p.span.start <= n.span.start && n.span.end <= p.span.end,
                    "{source:?}: {n:?} parent {p:?}"
                );
            }
        }
    }
    fn warnings(tree: &Tree) -> Vec<(usize, &str)> {
        tree.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .map(|d| (d.span.start as usize, d.message.as_str()))
            .collect()
    }
    const SAMPLE: &str = r#"# Isolinear sample
title = "TOML \"example\""
site.owner = 'Ann'   # dotted key

[server]
host = "127.0.0.1"
ports = [ 8000, 8001,
  8002, ]  # trailing comma
enabled = true
started = 1979-05-27 07:32:00-08:00

[server.limits]
inline = { max = 1_000, ratio = 0.5, nested = { deep = [] } }
"quoted key" = """
multi
line"""
raw = '''C:\no\escape'''

[[products]]
name = "Hammer"
sku = 0x7F

[[products]]
name = "Nail"
"#;

    #[test]
    fn spans_follow_the_source_grouped_by_headers() {
        let tree = parse(SAMPLE);
        assert_eq!(tree.stopped, None);
        assert!(tree.diagnostics.is_empty(), "{:?}", tree.diagnostics);
        check_spans(SAMPLE, &tree);
        let names: Vec<_> = (1..tree.len() as u32)
            .map(|id| {
                tree.node(id)
                    .name
                    .as_ref()
                    .map(|s| &SAMPLE[s.start as usize..s.end as usize])
            })
            .collect();
        assert_eq!(
            names,
            [
                Some("title"),
                Some("site.owner"),
                Some("[server]"),
                Some("host"),
                Some("ports"),
                None,
                None,
                None,
                Some("enabled"),
                Some("started"),
                Some("[server.limits]"),
                Some("inline"),
                Some("max"),
                Some("ratio"),
                Some("nested"),
                Some("deep"),
                Some("\"quoted key\""),
                Some("raw"),
                Some("[[products]]"),
                Some("name"),
                Some("sku"),
                Some("[[products]]"),
                Some("name")
            ]
        );
        let server = tree.node(3);
        assert_eq!(
            &SAMPLE[server.span.start as usize..server.span.end as usize],
            &SAMPLE[SAMPLE.find("[server]").unwrap()..SAMPLE.find("-08:00").unwrap() + 6]
        );
        assert_eq!(server.child_count(), 4);
        assert_eq!(tree.node(10).scalar, Some(Scalar::Date));
        assert_eq!(tree.node(9).scalar, Some(Scalar::Boolean));
        assert_eq!(tree.node(12).kind, Kind::Object);
        let val = tree.node(1).value.clone().unwrap();
        assert_eq!(
            &SAMPLE[val.start as usize..val.end as usize],
            r#""TOML \"example\"""#
        );
    }

    #[test]
    fn strings_numbers_and_dates() {
        for ok in [
            "a = 1",
            "a = +17",
            "a = -0",
            "a = 1_000_000",
            "a = 0xDEAD_beef",
            "a = 0o755",
            "a = 0b1101",
            "a = 3.14",
            "a = -0.01",
            "a = 5e+22",
            "a = 6.626e-34",
            "a = inf",
            "a = -nan",
            "a = 1979-05-27T07:32:00Z",
            "a = 1979-05-27t07:32:00.999+05:30",
            "a = 1979-05-27",
            "a = 07:32:00",
            "a = 07:32",
            "a = 1979-05-27T07:32",
            "a = \"\\u00E9\"",
            "a = \"\\e\\x41\"",
            "a = \"\"\"\nx\\\n   y\"\"\"",
            "a = '''one''two'''''",
            "a = \"\"\"\"quoted\"\"\"\"",
        ] {
            let tree = parse(ok);
            assert!(tree.stopped.is_none(), "{ok}: {:?}", tree.diagnostics);
            assert_eq!(tree.len(), 2, "{ok}");
            check_spans(ok, &tree);
        }
        for bad in [
            "a = 01",
            "a = 1__0",
            "a = _1",
            "a = 1.",
            "a = .5",
            "a = 0x",
            "a = +0x1",
            "a = 1e",
            "a = 1979-13-01",
            "a = 25:00:00",
            "a = tru",
            "a = \"\\q\"",
            "a = \"\\u12\"",
            "a = \"\\u+061\"",
            "a = \"\\x+1\"",
            "a = \"open",
            "a = 'open",
            "a = \"\"\"never",
            "a = \"bad\u{1}\"",
        ] {
            let tree = parse(bad);
            assert!(tree.stopped.is_some(), "accepted {bad}");
            let error = tree
                .diagnostics
                .iter()
                .find(|d| d.severity == Severity::Error)
                .unwrap();
            assert!(error.span.start as usize >= 4, "{bad}: {error:?}");
        }
    }

    #[test]
    fn redefinitions_are_reported_never_hidden() {
        let source = "a = 1\na = 2\nb.c = 1\n[b]\n[t]\nx = 1\n[t]\nx = 2\n[[arr]]\n[arr.sub]\n[[arr]]\n[arr.sub]\n[t.x]\ninline = { k = 1, k = 2 }\n";
        let tree = parse(source);
        assert!(tree.stopped.is_none(), "{:?}", tree.diagnostics);
        check_spans(source, &tree);
        let w = warnings(&tree);
        let at = |s: &str, nth: usize| source.match_indices(s).nth(nth).unwrap().0;
        let messages: Vec<&str> = w.iter().map(|(_, m)| *m).collect();
        assert_eq!(w[0].0, at("a = 2", 0), "{messages:?}");
        assert!(w[0].1.contains("Duplicate key a"));
        assert!(
            w[1].1.contains("[b] was already defined by dotted keys"),
            "{messages:?}"
        );
        assert!(w[2].1.contains("Duplicate table [t]"), "{messages:?}");
        assert!(w[3].1.contains("Duplicate key x"), "{messages:?}");
        assert!(
            w[4].1.contains("[t.x]") || w[4].1.contains("x"),
            "{messages:?}"
        );
        assert!(w[5].1.contains("Duplicate key k"), "{messages:?}");
        assert_eq!(w.len(), 6, "{messages:?}");
        // Each array-of-tables element is its own scope: [arr.sub] twice is fine.
        let tables = (1..tree.len() as u32)
            .filter(|&id| tree.node(id).kind == Kind::Table)
            .count();
        assert_eq!(tables, 8, "every header stays visible");
    }

    #[test]
    fn malformed_inputs_keep_prefix_and_report_position() {
        for (text, at) in [
            ("a = 1\nb = \n", "\n"),
            ("a = 1\nb 2\n", "2"),
            ("a = 1\n[t\nx = 1\n", "\nx"),
            ("a = [1, 2\n", ""),
            ("a = [1,, 2]\n", ", 2]"),
            ("a = { x = 1 y = 2 }\n", "y = 2"),
            ("a = 1 b = 2\n", "b = 2"),
            ("a = 1\n= 2\n", "= 2"),
            ("a = 1\n[[t]\n", "]\n"),
            ("a = 1 # bad \u{7} comment\n", "\u{7}"),
        ] {
            let tree = parse(text);
            assert!(tree.stopped.is_some(), "accepted {text:?}");
            let error = tree
                .diagnostics
                .iter()
                .find(|d| d.severity == Severity::Error)
                .unwrap();
            let want = if at.is_empty() {
                text.len()
            } else {
                text[6..].find(at).unwrap() + 6
            };
            assert_eq!(error.span.start as usize, want, "{text:?}: {error:?}");
            check_spans(text, &tree);
            assert_eq!(
                tree.node(1)
                    .name
                    .as_ref()
                    .map(|r| &text[r.start as usize..r.end as usize]),
                Some("a"),
                "prefix kept for {text:?}"
            );
        }
    }

    #[test]
    fn depth_node_and_diagnostic_limits_are_enforced() {
        for source in [
            format!("a = {}{}", "[".repeat(300), "]".repeat(300)),
            format!("a = {}1{}", "{b=".repeat(300), "}".repeat(300)),
        ] {
            let t = parse(&source);
            assert_eq!(t.stopped, Some(Stop::Depth));
            check_spans(&source, &t);
        }
        let wide = format!("a = [{}]", "1, ".repeat(100));
        let t = Toml.parse(
            &wide,
            &Limits {
                max_depth: 10,
                max_nodes: 50,
                ..Limits::default()
            },
            &AtomicBool::new(false),
        );
        assert_eq!(t.len(), 50);
        assert_eq!(t.stopped, Some(Stop::Nodes));
        assert_eq!(t.node(1).child_count(), 48);
        check_spans(&wide, &t);
        let t = Toml.parse(
            "a=1\na=2\na=3\nb=",
            &Limits {
                max_diagnostics: 1,
                ..Limits::default()
            },
            &AtomicBool::new(false),
        );
        assert_eq!(t.diagnostics.len(), 1);
        assert_eq!(t.more_diagnostics, 2);
        assert_eq!(t.stopped, Some(Stop::Error));
        let t = Toml.parse(
            "[a]\nb=[]",
            &Limits {
                max_depth: 1,
                ..Limits::default()
            },
            &AtomicBool::new(false),
        );
        assert_eq!(t.stopped, Some(Stop::Depth));
    }

    #[test]
    fn cancellation_including_inputs_without_nodes() {
        for source in ["", "a=1", "# only a comment", "a=\"unterminated"] {
            let t = Toml.parse(source, &Limits::default(), &AtomicBool::new(true));
            assert_eq!(t.stopped, Some(Stop::Cancelled));
            check_spans(source, &t);
        }
    }

    #[test]
    fn crlf_and_bom_are_tolerated() {
        let source = "\u{FEFF}a = 1\r\n[t]\r\nb = [\r\n 2,\r\n]\r\n";
        let tree = parse(source);
        assert!(tree.stopped.is_none(), "{:?}", tree.diagnostics);
        check_spans(source, &tree);
        assert_eq!(tree.len(), 5);
    }

    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// Parsing, tree rendering and cursor lookup must never panic on arbitrary UTF-8.
    #[test]
    fn random_input_never_panics() {
        let alphabet = [
            "[",
            "]",
            "[[",
            "]]",
            "{",
            "}",
            "=",
            " = ",
            ",",
            ".",
            "#",
            "\"",
            "'",
            "\"\"\"",
            "'''",
            "\\",
            "\\u",
            "\n",
            "\r\n",
            " ",
            "\t",
            "a",
            "k",
            "1",
            "0x",
            "_",
            "-",
            "+",
            "e",
            ":",
            "T",
            "1979-05-27",
            "07:32",
            "true",
            "inf",
            "é",
            "日",
            "\u{FEFF}",
        ];
        let mut rng = XorShift(0x2545_F491_4F6C_DD1D);
        for _ in 0..30_000 {
            let len = (rng.next() % 16) as usize + 1;
            let text: String = (0..len)
                .map(|_| alphabet[(rng.next() % alphabet.len() as u64) as usize])
                .collect();
            let tree = parse(&text);
            check_spans(&text, &tree);
            let _ = dump(&tree, &text);
            let _ = tree.path_at((rng.next() % (text.len() as u64 + 1)) as u32);
        }
    }
    #[test]
    fn source_order_and_header_extent_snapshot() {
        let s = "a.b=1\n[t]\nx={a=[true, 'é']}\n# tail\n\n[[p]]\n";
        let t = parse(s);
        check_spans(s, &t);
        assert_eq!(
            dump(&t, s),
            "\
Document [3] @0..43
  Scalar(Number) a.b = 1 @0..5
  Table [t] [1] @6..28
    Object x [1] @10..28
      Array a [2] @13..27
        Scalar(Boolean) = true @16..20
        Scalar(String) = 'é' @22..26
  Table [[p]] [0] @37..42
"
        );
    }

    #[test]
    fn fixtures() {
        for (s, errors, warnings) in [
            (
                include_str!("../../../../tests/fixtures/tree/toml/Cargo.toml"),
                0,
                0,
            ),
            (
                include_str!("../../../../tests/fixtures/tree/toml/pyproject.toml"),
                0,
                0,
            ),
            (
                include_str!("../../../../tests/fixtures/tree/toml/products.toml"),
                0,
                0,
            ),
            (
                include_str!("../../../../tests/fixtures/tree/toml/redefinitions.toml"),
                0,
                6,
            ),
            (
                include_str!("../../../../tests/fixtures/tree/toml/malformed-array.toml"),
                1,
                0,
            ),
            (
                include_str!("../../../../tests/fixtures/tree/toml/malformed-string.toml"),
                1,
                0,
            ),
        ] {
            let t = parse(s);
            check_spans(s, &t);
            assert_eq!(t.errors(), errors, "{}", dump(&t, s));
            assert_eq!(self::warnings(&t).len(), warnings);
            assert_eq!(t.stopped, (errors > 0).then_some(Stop::Error));
        }
    }

    #[test]
    fn redefinition_paths_and_escaped_keys() {
        for (s, count) in [
            (
                r#"a=1
"\u0061"=2
'quoted.key'=1
"quoted.key"=2"#,
                2,
            ),
            ("a=1\n[a.b]", 1),
            ("a=1\na.b=2", 1),
            ("a.b=1\na=2", 1),
            ("a={}\n[a.b]", 1),
            ("[a]\n[[a]]", 1),
            ("[[a]]\n[a]", 1),
            ("[a.b]\n[a]", 0),
            ("[a.b]\n[a]\nb.c=1", 1),
            ("[[a.b]]\n[a]\nb.c=1", 1),
            ("[[a]]\nx=1\n[[a]]\nx=2", 0),
            ("a={b.c=1, b=2}", 1),
            ("a=[{x=1}, {x=2}]", 0),
        ] {
            let t = parse(s);
            assert_eq!(t.stopped, None, "{}", dump(&t, s));
            assert_eq!(warnings(&t).len(), count, "{}", dump(&t, s));
            check_spans(s, &t);
        }
    }

    #[test]
    fn toml_11_inline_tables_empty_inputs_and_error_messages() {
        for s in [
            "",
            "# comment",
            " \r\n\t",
            "x={\n# comment\na=1,\n}",
            "a={}",
            "a=[]",
        ] {
            let t = parse(s);
            assert_eq!(t.stopped, None, "{}", dump(&t, s));
            assert!(t.diagnostics.is_empty());
            check_spans(s, &t);
        }
        for (s, message) in [
            ("[a", "expected ']', found end of input"),
            ("a 1", "expected '=' after the key, found '1'"),
            ("a=", "a value was expected"),
            ("a=[1 2]", "expected ',' or ']', found '2'"),
            ("a=\"\\q\"", "Invalid escape sequence"),
            ("a=tru", "Invalid value tru"),
        ] {
            let t = parse(s);
            assert_eq!(t.stopped, Some(Stop::Error));
            assert!(
                t.diagnostics[0].message.contains(message),
                "{}",
                dump(&t, s)
            );
        }
    }
    #[test]
    fn cancellation_during_long_scans() {
        for prefix in ["#", "x=\"", "x='", " "] {
            let unit = if prefix == " " { " " } else { "é" };
            let s = format!("{prefix}{}", unit.repeat(16_000_000));
            // Set beforehand, so the result does not depend on machine speed: a scan that
            // missed its polls would run to the end and fail here.
            let cancel = AtomicBool::new(true);
            let t = Toml.parse(&s, &Limits::default(), &cancel);
            assert_eq!(t.stopped, Some(Stop::Cancelled), "{prefix:?}");
            check_spans(&s, &t);
        }
        // Two-byte escapes starting at an odd offset must not miss cancellation polls.
        let s = format!("x = \"{}", "\\n".repeat(8_000_000));
        let cancel = AtomicBool::new(true);
        let t = Toml.parse(&s, &Limits::default(), &cancel);
        assert_eq!(t.stopped, Some(Stop::Cancelled));
        check_spans(&s, &t);
    }
}
