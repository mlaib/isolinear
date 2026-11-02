//! ISO 10303-21 ("Part 21", the STEP physical file) shared by STEP and IFC: the HEADER and
//! DATA sections, entity instances `#id = TYPE(...)` and complex instances `#id = (A() B())`.
//! The file is scanned once for statement boundaries; parameters are parsed on demand from an
//! instance's bytes, so large models stay small in memory.
use super::cad::{Rows, clip, count_table};
use crate::inspect::{Inspection, Stage, State};
use std::{collections::HashMap, ops::Range};

/// Entity instances kept; larger files are read in part.
pub const MAX_ENTITIES: usize = 8_000_000;
/// Nesting of parameter lists.
const MAX_DEPTH: usize = 64;
/// Values in one instance's parameters.
const MAX_VALUES: usize = 4_000_000;

/// A parameter value.
#[derive(Clone, Debug, PartialEq)]
pub enum P {
    /// `$`
    Null,
    /// `*`
    Derived,
    Int(i64),
    Real(f64),
    Str(String),
    /// `.NAME.`
    Enum(String),
    /// `#id`
    Ref(u64),
    List(Vec<P>),
    /// `TYPE(value)`, e.g. `IFCLABEL('x')`.
    Typed(String, Vec<P>),
    /// `"0FF"`
    Binary(String),
}

impl P {
    pub fn str(&self) -> Option<&str> {
        match self {
            P::Str(s) => Some(s),
            P::Typed(_, v) => v.first().and_then(P::str),
            _ => None,
        }
    }
    pub fn reference(&self) -> Option<u64> {
        match self {
            P::Ref(r) => Some(*r),
            _ => None,
        }
    }
    pub fn list(&self) -> &[P] {
        match self {
            P::List(l) => l,
            _ => &[],
        }
    }
    pub fn num(&self) -> Option<f64> {
        match self {
            P::Int(i) => Some(*i as f64),
            P::Real(r) => Some(*r),
            P::Typed(_, v) => v.first().and_then(P::num),
            _ => None,
        }
    }
    pub fn enumeration(&self) -> Option<&str> {
        match self {
            P::Enum(e) => Some(e),
            _ => None,
        }
    }
    /// The value for reading: strings unquoted, lists joined, references as `#id`.
    pub fn show(&self) -> String {
        let mut s = String::new();
        self.write(&mut s, 0);
        s
    }
    fn write(&self, out: &mut String, depth: usize) {
        use std::fmt::Write;
        if out.len() > 400 {
            return;
        }
        match self {
            P::Null => out.push('$'),
            P::Derived => out.push('*'),
            P::Int(i) => {
                let _ = write!(out, "{i}");
            }
            P::Real(r) => out.push_str(&super::cad::num(*r)),
            P::Str(s) => out.push_str(s),
            P::Enum(e) => {
                let _ = write!(out, ".{e}.");
            }
            P::Ref(r) => {
                let _ = write!(out, "#{r}");
            }
            P::Binary(b) => {
                let _ = write!(out, "\"{b}\"");
            }
            P::List(items) | P::Typed(_, items) => {
                if let P::Typed(name, _) = self {
                    out.push_str(name);
                }
                out.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    if depth < 8 {
                        item.write(out, depth + 1);
                    }
                    if out.len() > 400 {
                        out.push('…');
                        break;
                    }
                }
                out.push(')');
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Entity {
    pub id: u64,
    /// Index into [`P21::kinds`].
    pub kind: u32,
    /// The instance's bytes, from `#` to before `;`.
    pub start: u32,
    pub end: u32,
    /// Where the record (after `=`) starts.
    pub body: u32,
}

pub struct P21<'a> {
    pub text: &'a str,
    /// Header entities: name and statement span.
    pub header: Vec<(String, Range<usize>)>,
    pub entities: Vec<Entity>,
    /// Entity type names, upper case; complex instances as "A + B + C".
    pub kinds: Vec<String>,
    kind_index: HashMap<String, u32>,
    by_id: Vec<(u64, u32)>,
    pub magic: bool,
    pub data: bool,
    pub ended: bool,
    /// Statements in DATA that are not entity instances.
    pub bad: u64,
    pub duplicates: u64,
    /// More than [`MAX_ENTITIES`] instances: the rest were not kept.
    pub truncated: bool,
}

#[derive(PartialEq)]
enum Section {
    None,
    Header,
    Data,
    Other,
}

fn skip_space_and_comments(b: &[u8], mut i: usize) -> usize {
    loop {
        if i >= b.len() {
            return b.len();
        }
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b[i..].starts_with(b"/*") {
            i = memchr::memmem::find(&b[i + 2..], b"*/")
                .map(|j| i + 2 + j + 2)
                .unwrap_or(b.len());
        } else {
            return i;
        }
    }
}

/// The end of the statement starting at `i` (the index of its `;`, or the end of the text).
fn statement_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() {
        let Some(j) = memchr::memchr3(b';', b'\'', b'/', &b[i..]) else {
            return b.len();
        };
        i += j;
        match b[i] {
            b';' => return i,
            b'\'' => {
                i = memchr::memchr(b'\'', &b[i + 1..])
                    .map(|k| i + 1 + k + 1)
                    .unwrap_or(b.len());
            }
            _ => {
                if b.get(i + 1) == Some(&b'*') {
                    i = memchr::memmem::find(&b[i + 2..], b"*/")
                        .map(|k| i + 2 + k + 2)
                        .unwrap_or(b.len());
                } else {
                    i += 1;
                }
            }
        }
    }
    b.len()
}

fn keyword(b: &[u8], i: usize) -> (String, usize) {
    let mut j = i;
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'-') {
        j += 1;
    }
    (String::from_utf8_lossy(&b[i..j]).to_ascii_uppercase(), j)
}

/// Whether the text starts like a Part 21 file.
pub fn is_p21(head: &[u8]) -> bool {
    let i = skip_space_and_comments(head, bom(head));
    head[i.min(head.len())..].starts_with(b"ISO-10303-21;")
}

/// Bytes of a UTF-8 byte-order mark at the start.
fn bom(b: &[u8]) -> usize {
    if b.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 }
}

/// The schema names of FILE_SCHEMA in the head, upper case.
pub fn head_schema(head: &[u8]) -> Option<String> {
    let at = memchr::memmem::find(head, b"FILE_SCHEMA")?;
    let end = statement_end(head, at);
    Some(String::from_utf8_lossy(&head[at..end]).to_ascii_uppercase())
}

impl<'a> P21<'a> {
    pub fn parse(text: &'a str, cancelled: &dyn Fn() -> bool) -> Result<P21<'a>, String> {
        let b = text.as_bytes();
        let mut p = P21 {
            text,
            header: vec![],
            entities: vec![],
            kinds: vec![],
            kind_index: HashMap::new(),
            by_id: vec![],
            magic: false,
            data: false,
            ended: false,
            bad: 0,
            duplicates: 0,
            truncated: false,
        };
        let mut section = Section::None;
        let mut i = bom(b);
        let mut n = 0u64;
        while i < b.len() {
            n += 1;
            if n.is_multiple_of(65536) && cancelled() {
                return Err("Cancelled".into());
            }
            let start = skip_space_and_comments(b, i);
            if start >= b.len() {
                break;
            }
            let end = statement_end(b, start);
            i = end + 1;
            let stmt = &b[start..end];
            if stmt.first() == Some(&b'#') {
                if section != Section::Data {
                    p.bad += 1;
                    continue;
                }
                if p.entities.len() >= MAX_ENTITIES {
                    p.truncated = true;
                    continue;
                }
                match p.instance(start, end) {
                    Some(e) => p.entities.push(e),
                    None => p.bad += 1,
                }
                continue;
            }
            let (word, after) = keyword(b, start);
            match word.as_str() {
                "ISO-10303-21" => p.magic = true,
                "HEADER" => section = Section::Header,
                "DATA" => {
                    section = Section::Data;
                    p.data = true;
                }
                "ENDSEC" => section = Section::None,
                "END-ISO-10303-21" => {
                    p.ended = true;
                    break;
                }
                "ANCHOR" | "REFERENCE" | "SIGNATURE" => section = Section::Other,
                _ if section == Section::Header && after > start => {
                    if p.header.len() < 1000 {
                        p.header.push((word, start..end));
                    }
                }
                _ if section == Section::Data => p.bad += 1,
                _ => {}
            }
            if !p.magic && n == 1 {
                return Err("The file does not start with ISO-10303-21;".into());
            }
        }
        if !p.magic {
            return Err("The file does not start with ISO-10303-21;".into());
        }
        p.by_id = p
            .entities
            .iter()
            .enumerate()
            .map(|(k, e)| (e.id, k as u32))
            .collect();
        p.by_id.sort_unstable();
        let before = p.by_id.len();
        p.by_id.dedup_by_key(|x| x.0);
        p.duplicates = (before - p.by_id.len()) as u64;
        Ok(p)
    }

    fn instance(&mut self, start: usize, end: usize) -> Option<Entity> {
        let b = self.text.as_bytes();
        let mut j = start + 1;
        let digits = j;
        while j < end && b[j].is_ascii_digit() {
            j += 1;
        }
        let id: u64 = std::str::from_utf8(&b[digits..j]).ok()?.parse().ok()?;
        j = skip_space_and_comments(&b[..end], j);
        if b.get(j) != Some(&b'=') {
            return None;
        }
        let body = skip_space_and_comments(&b[..end], j + 1);
        let kind = if b.get(body) == Some(&b'(') {
            let parts = record(&self.text[body..end])?;
            parts
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(" + ")
        } else {
            let (word, after) = keyword(b, body);
            if word.is_empty() || after == body {
                return None;
            }
            word
        };
        let kind = match self.kind_index.get(&kind) {
            Some(&k) => k,
            None => {
                let k = self.kinds.len() as u32;
                self.kind_index.insert(kind.clone(), k);
                self.kinds.push(kind);
                k
            }
        };
        Some(Entity {
            id,
            kind,
            start: start as u32,
            end: end as u32,
            body: body as u32,
        })
    }

    pub fn kind(&self, e: &Entity) -> &str {
        &self.kinds[e.kind as usize]
    }
    pub fn get(&self, id: u64) -> Option<&Entity> {
        let k = self.by_id.binary_search_by_key(&id, |x| x.0).ok()?;
        Some(&self.entities[self.by_id[k].1 as usize])
    }
    pub fn span(&self, e: &Entity) -> Range<u64> {
        u64::from(e.start)..u64::from(e.end) + 1
    }
    /// The parts of an instance: one (type, parameters) for a simple instance, several for a
    /// complex one.
    pub fn parts(&self, e: &Entity) -> Vec<(String, Vec<P>)> {
        record(&self.text[e.body as usize..e.end as usize]).unwrap_or_default()
    }
    /// The parameters of a simple instance (the first part of a complex one).
    pub fn params(&self, e: &Entity) -> Vec<P> {
        self.parts(e)
            .into_iter()
            .next()
            .map(|p| p.1)
            .unwrap_or_default()
    }
    /// Parameters of the instance `id` if it has type `kind`.
    pub fn params_of(&self, id: u64, kind: &str) -> Option<Vec<P>> {
        let e = self.get(id)?;
        (self.kind(e) == kind).then(|| self.params(e))
    }
    /// The parameters of a header entity.
    pub fn header_params(&self, name: &str) -> Option<(Vec<P>, Range<u64>)> {
        let (_, span) = self.header.iter().find(|(n, _)| n == name)?;
        let parts = record(&self.text[span.clone()])?;
        Some((
            parts.into_iter().next()?.1,
            span.start as u64..span.end as u64 + 1,
        ))
    }
    /// Instances of one type, in file order.
    pub fn of_kind<'s>(&'s self, kind: &str) -> impl Iterator<Item = &'s Entity> + 's {
        let k = self.kind_index.get(kind).copied();
        self.entities.iter().filter(move |e| Some(e.kind) == k)
    }
    pub fn count(&self, kind: &str) -> usize {
        self.of_kind(kind).count()
    }
    pub fn type_counts(&self) -> HashMap<String, u64> {
        let mut counts = vec![0u64; self.kinds.len()];
        for e in &self.entities {
            counts[e.kind as usize] += 1;
        }
        self.kinds.iter().cloned().zip(counts).collect()
    }
    /// References to instances that are not in the file: (count, first example).
    pub fn dangling(&self, cancelled: &dyn Fn() -> bool) -> (u64, u64, Option<(u64, u64)>) {
        let b = self.text.as_bytes();
        let (mut total, mut missing, mut first) = (0u64, 0u64, None);
        for (n, e) in self.entities.iter().enumerate() {
            if n % 65536 == 0 && cancelled() {
                break;
            }
            let mut i = e.body as usize;
            let end = e.end as usize;
            while i < end {
                let Some(j) = memchr::memchr2(b'#', b'\'', &b[i..end]) else {
                    break;
                };
                i += j;
                if b[i] == b'\'' {
                    i = memchr::memchr(b'\'', &b[i + 1..end])
                        .map(|k| i + 1 + k + 1)
                        .unwrap_or(end);
                    continue;
                }
                let s = i + 1;
                i = s;
                while i < end && b[i].is_ascii_digit() {
                    i += 1;
                }
                if let Ok(id) = std::str::from_utf8(&b[s..i]).unwrap_or("").parse::<u64>() {
                    total += 1;
                    if self.get(id).is_none() {
                        missing += 1;
                        first.get_or_insert((e.id, id));
                    }
                }
            }
        }
        (total, missing, first)
    }
}

/// A record: `NAME(params)`, or `(A(..) B(..))` for a complex instance.
pub fn record(s: &str) -> Option<Vec<(String, Vec<P>)>> {
    let mut r = Reader {
        b: s.as_bytes(),
        at: 0,
        values: 0,
    };
    r.ws();
    if r.peek() == Some(b'(') {
        r.at += 1;
        let mut parts = vec![];
        loop {
            r.ws();
            if r.peek() == Some(b')') || r.at >= r.b.len() {
                break;
            }
            let name = r.name()?;
            r.ws();
            let P::List(params) = r.list(0)? else {
                return None;
            };
            parts.push((name, params));
            if parts.len() > 256 {
                return None;
            }
        }
        Some(parts)
    } else {
        let name = r.name()?;
        r.ws();
        let P::List(params) = r.list(0)? else {
            return None;
        };
        Some(vec![(name, params)])
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
    values: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.at).copied()
    }
    fn ws(&mut self) {
        self.at = skip_space_and_comments(self.b, self.at);
    }
    fn name(&mut self) -> Option<String> {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            self.at += 1;
        }
        (self.at > start)
            .then(|| String::from_utf8_lossy(&self.b[start..self.at]).to_ascii_uppercase())
    }
    fn list(&mut self, depth: usize) -> Option<P> {
        if depth > MAX_DEPTH || self.peek() != Some(b'(') {
            return None;
        }
        self.at += 1;
        let mut items = vec![];
        loop {
            self.ws();
            match self.peek()? {
                b')' => {
                    self.at += 1;
                    return Some(P::List(items));
                }
                b',' => self.at += 1,
                _ => {
                    items.push(self.value(depth + 1)?);
                    self.values += 1;
                    if self.values > MAX_VALUES {
                        return None;
                    }
                }
            }
        }
    }
    fn value(&mut self, depth: usize) -> Option<P> {
        self.ws();
        let c = self.peek()?;
        Some(match c {
            b'$' => {
                self.at += 1;
                P::Null
            }
            b'*' => {
                self.at += 1;
                P::Derived
            }
            b'(' => self.list(depth)?,
            b'#' => {
                self.at += 1;
                let s = self.at;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.at += 1;
                }
                P::Ref(
                    std::str::from_utf8(&self.b[s..self.at])
                        .ok()?
                        .parse()
                        .ok()?,
                )
            }
            b'\'' => {
                self.at += 1;
                let s = self.at;
                loop {
                    let j = memchr::memchr(b'\'', &self.b[self.at..])?;
                    self.at += j + 1;
                    if self.peek() == Some(b'\'') {
                        self.at += 1;
                    } else {
                        break;
                    }
                }
                P::Str(decode_string(&self.b[s..self.at - 1]))
            }
            b'"' => {
                self.at += 1;
                let s = self.at;
                let j = memchr::memchr(b'"', &self.b[self.at..])?;
                self.at += j + 1;
                P::Binary(clip(&String::from_utf8_lossy(&self.b[s..s + j]), 64))
            }
            b'.' if self
                .b
                .get(self.at + 1)
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_') =>
            {
                self.at += 1;
                let s = self.at;
                let j = memchr::memchr(b'.', &self.b[self.at..])?;
                self.at += j + 1;
                P::Enum(String::from_utf8_lossy(&self.b[s..s + j]).into_owned())
            }
            c if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() => {
                let s = self.at;
                while self
                    .peek()
                    .is_some_and(|c| matches!(c, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
                {
                    self.at += 1;
                }
                let t = std::str::from_utf8(&self.b[s..self.at]).ok()?;
                match t.parse::<i64>() {
                    Ok(i) => P::Int(i),
                    Err(_) => P::Real(t.parse().ok()?),
                }
            }
            c if c.is_ascii_alphabetic() => {
                let name = self.name()?;
                self.ws();
                let P::List(items) = self.list(depth)? else {
                    return None;
                };
                P::Typed(name, items)
            }
            _ => return None,
        })
    }
}

/// A Part 21 string's text: `''`, `\\`, `\X\hh` (ISO 8859-1), `\X2\…\X0\` (UTF-16),
/// `\X4\…\X0\` (UCS-4) and `\S\c` decoded; code page switches (`\Pa\`) dropped.
pub fn decode_string(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    if !s.contains('\\') && !s.contains("''") {
        return s.into_owned();
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(b.len());
    let mut i = 0;
    let hex = |h: &[u8]| {
        std::str::from_utf8(h)
            .ok()
            .and_then(|t| u32::from_str_radix(t, 16).ok())
    };
    while i < b.len() {
        if b[i] == b'\'' && b.get(i + 1) == Some(&b'\'') {
            out.push('\'');
            i += 2;
        } else if b[i..].starts_with(b"\\\\") {
            out.push('\\');
            i += 2;
        } else if b[i..].starts_with(b"\\X\\")
            && b.get(i + 3..i + 5)
                .is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit))
        {
            if let Some(c) = hex(&b[i + 3..i + 5]).and_then(char::from_u32) {
                out.push(c);
            }
            i += 5;
        } else if b[i..].starts_with(b"\\X2\\") || b[i..].starts_with(b"\\X4\\") {
            let width = if b[i + 2] == b'2' { 4 } else { 8 };
            i += 4;
            let mut units = vec![];
            while i + width <= b.len() && !b[i..].starts_with(b"\\X0\\") {
                match hex(&b[i..i + width]) {
                    Some(u) => units.push(u),
                    None => break,
                }
                i += width;
            }
            if b[i..].starts_with(b"\\X0\\") {
                i += 4;
            }
            if width == 4 {
                let u16s: Vec<u16> = units.iter().map(|&u| u as u16).collect();
                out.push_str(&String::from_utf16_lossy(&u16s));
            } else {
                out.extend(units.into_iter().filter_map(char::from_u32));
            }
        } else if b[i..].starts_with(b"\\S\\") && b.get(i + 3).is_some_and(u8::is_ascii) {
            if let Some(c) = char::from_u32(u32::from(b[i + 3]) + 128) {
                out.push(c);
            }
            i += 4;
        } else if b[i] == b'\\' && b.get(i + 1) == Some(&b'P') && b.get(i + 3) == Some(&b'\\') {
            i += 4;
        } else {
            let ch = s[i..].chars().next().unwrap_or('\u{fffd}');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// The header facts, type counts, instances table and the structural checks shared by STEP
/// and IFC.
pub fn common(p: &P21, out: &mut Inspection, cancelled: &dyn Fn() -> bool) {
    let g = "Header";
    let text = |v: Option<&P>| -> String {
        match v {
            Some(P::List(items)) => items
                .iter()
                .map(P::show)
                .filter(|s| !s.is_empty() && s != "$")
                .collect::<Vec<_>>()
                .join("; "),
            Some(P::Null) | None => String::new(),
            Some(v) => v.show(),
        }
    };
    if let Some((v, span)) = p.header_params("FILE_DESCRIPTION") {
        out.fact(
            g,
            "Description",
            clip(&text(v.first()), 300),
            Some(span.clone()),
        );
        out.fact(g, "Implementation level", text(v.get(1)), Some(span));
    }
    if let Some((v, span)) = p.header_params("FILE_NAME") {
        for (k, label) in [
            (0, "File name"),
            (1, "Time stamp"),
            (2, "Author"),
            (3, "Organization"),
            (4, "Preprocessor"),
            (5, "Originating system"),
            (6, "Authorization"),
        ] {
            out.fact(g, label, clip(&text(v.get(k)), 300), Some(span.clone()));
        }
    }
    if let Some((v, span)) = p.header_params("FILE_SCHEMA") {
        out.fact(g, "Schema", text(v.first()), Some(span));
    }
    out.fact(
        "Data",
        "Entity instances",
        p.entities.len().to_string(),
        None,
    );
    out.fact("Data", "Entity types", p.kinds.len().to_string(), None);
    count_table(out, "Entity types", "Type", &p.type_counts());
    let mut rows = Rows::new("Entities", &["#", "Type", "Parameters"]);
    for (n, e) in p.entities.iter().enumerate() {
        if n % 65536 == 0 && cancelled() {
            break;
        }
        if rows.len() as usize >= crate::inspect::MAX_ROWS {
            rows.push(vec![], None);
            continue;
        }
        let params = p.parts(e);
        let shown = if params.len() == 1 {
            params[0]
                .1
                .iter()
                .take(4)
                .map(P::show)
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            params
                .iter()
                .map(|(n, v)| {
                    format!(
                        "{n}({})",
                        v.iter().map(P::show).collect::<Vec<_>>().join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        rows.push(
            vec![
                format!("#{}", e.id),
                p.kind(e).to_owned(),
                clip(&shown, 160),
            ],
            Some(p.span(e)),
        );
    }
    rows.finish(out);
    // Parsed.
    if p.truncated {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("Only the first {MAX_ENTITIES} entity instances are kept"),
        );
    } else if !p.data {
        out.check(Stage::Parsed, State::Failed, "No DATA section was found");
    } else if !p.ended {
        out.check(
            Stage::Parsed,
            State::Partial,
            "The file ends before END-ISO-10303-21: it may be cut; the instances read are shown",
        );
    } else if p.bad > 0 {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!(
                "{} statements in DATA are not entity instances; they are skipped",
                p.bad
            ),
        );
    } else {
        out.check(
            Stage::Parsed,
            State::Passed,
            format!("{} entity instances read", p.entities.len()),
        );
    }
    // References.
    let (total, missing, first) = p.dangling(cancelled);
    if p.duplicates > 0 {
        out.check(
            Stage::Semantic,
            State::Failed,
            format!(
                "Instance names: {} #ids are used more than once",
                p.duplicates
            ),
        );
    }
    match first {
        None => out.check(
            Stage::Semantic,
            State::Passed,
            format!("References: all {total} #references point to instances in the file"),
        ),
        Some((from, to)) => out.check(
            Stage::Semantic,
            if p.ended { State::Failed } else { State::Partial },
            format!(
                "References: {missing} of {total} #references point to no instance (first: #{to} in #{from})"
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_strings_and_scanning() {
        let r = record("PRODUCT('A''s','N\\X2\\00E9\\X0\\',$,(#2,#3),.T.,1.5E2,-3,IFCLABEL('x'))")
            .unwrap();
        assert_eq!(r[0].0, "PRODUCT");
        let v = &r[0].1;
        assert_eq!(v[0], P::Str("A's".into()));
        assert_eq!(v[1], P::Str("Né".into()));
        assert_eq!(v[3], P::List(vec![P::Ref(2), P::Ref(3)]));
        assert_eq!(v[4], P::Enum("T".into()));
        assert_eq!(v[5], P::Real(150.0));
        assert_eq!(v[6], P::Int(-3));
        assert_eq!(v[7].str(), Some("x"));
        let c = record("( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.) )").unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(c[2].1[0], P::Enum("MILLI".into()));
        let src = "ISO-10303-21;\nHEADER;FILE_SCHEMA(('X;Y'));ENDSEC;\nDATA;\n/* c; */#1=A('a;b',#2);\n#2=(B()C(1));\n#3=D(#9);ENDSEC;END-ISO-10303-21;";
        let p = P21::parse(src, &|| false).unwrap();
        assert_eq!(p.entities.len(), 3);
        assert_eq!(p.kind(&p.entities[1]), "B + C");
        assert!(p.ended);
        assert_eq!(p.header_params("FILE_SCHEMA").unwrap().0[0].show(), "(X;Y)");
        let (total, missing, first) = p.dangling(&|| false);
        assert_eq!((total, missing, first), (2, 1, Some((3, 9))));
        assert!(record(&"(".repeat(200)).is_none());
    }
}
