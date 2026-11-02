//! SQLite pages and records, read directly without an SQL engine or sidecar access.
//! B-tree headers provide row counts; record and overflow bytes are decoded only for requested rows.

use super::{Column, Schema, TableInfo, TableSource, cut};
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub const READY: bool = true;
const MAGIC: &[u8] = b"SQLite format 3\0";
const MAX_DEPTH: usize = 40;
const MAX_PAYLOAD: u64 = 256 * 1024 * 1024;
const MAX_PAGES: usize = 4_000_000;
const COUNT_ALL_LIMIT: u64 = 64 * 1024 * 1024;
const MAX_SCHEMA_BYTES: usize = 64 * 1024 * 1024;
const MAX_SQL_BYTES: usize = 1024 * 1024;
const MAX_TABLES: usize = 200_000;
const MAX_COLUMNS: usize = 32_768;
const MAX_PAGE_CELLS: usize = 1_000_000;
const MAX_RESULT_BYTES: usize = 256 * 1024 * 1024;
const BLOB_SHOWN: usize = 256;

trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

fn damaged(what: &str) -> String {
    format!("The database is damaged: {what}")
}

struct Db {
    file: Box<dyn ReadSeek>,
    len: u64,
    page_size: u64,
    usable: u64,
    pages: u64,
    utf16: Option<bool>,
    header: [u8; 100],
    reads: usize,
    definitions: Vec<String>,
}

impl Db {
    fn new(mut file: impl ReadSeek + 'static) -> Result<Self, String> {
        let len = file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut header = [0u8; 100];
        file.read_exact(&mut header)
            .map_err(|_| "Not a SQLite database: too short".to_owned())?;
        if &header[..16] != MAGIC {
            return Err("Not a SQLite database".into());
        }
        let page_size = match u16::from_be_bytes([header[16], header[17]]) {
            1 => 65_536,
            n if n >= 512 && n.is_power_of_two() => u64::from(n),
            _ => return Err(damaged("invalid page size")),
        };
        let reserved = u64::from(header[20]);
        if page_size - reserved < 480 {
            return Err(damaged("invalid reserved space"));
        }
        let declared = u64::from(u32::from_be_bytes(
            header[28..32].try_into().unwrap_or_default(),
        ));
        let valid = header[24..28] == header[92..96] && declared > 0;
        let pages = if valid {
            declared.min(len / page_size)
        } else {
            len / page_size
        };
        let utf16 = match u32::from_be_bytes(header[56..60].try_into().unwrap_or_default()) {
            0 | 1 => None,
            2 => Some(true),
            3 => Some(false),
            _ => return Err(damaged("unknown text encoding")),
        };
        Ok(Self {
            file: Box::new(file),
            len,
            page_size,
            usable: page_size - reserved,
            pages,
            utf16,
            header,
            reads: 0,
            definitions: Vec::new(),
        })
    }
    fn page(&mut self, n: u64) -> Result<Vec<u8>, String> {
        if n == 0 || n > self.pages {
            return Err(damaged(&format!("page {n} does not exist")));
        }
        self.reads += 1;
        if self.reads > MAX_PAGES * 2 {
            return Err("The database page-read limit was exceeded".into());
        }
        let at = (n - 1)
            .checked_mul(self.page_size)
            .ok_or_else(|| damaged("page offset overflow"))?;
        let mut buf = vec![0; self.page_size as usize];
        self.file
            .seek(SeekFrom::Start(at))
            .and_then(|_| self.file.read_exact(&mut buf))
            .map_err(|_| damaged(&format!("page {n} is past the end of the file")))?;
        Ok(buf)
    }
    fn text(&self, bytes: &[u8]) -> String {
        match self.utf16 {
            None => String::from_utf8_lossy(bytes).into_owned(),
            Some(little) => {
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| {
                        if little {
                            u16::from_le_bytes([c[0], c[1]])
                        } else {
                            u16::from_be_bytes([c[0], c[1]])
                        }
                    })
                    .collect();
                String::from_utf16_lossy(&units)
            }
        }
    }
}

/// SQLite's big-endian variable-length integer (1 to 9 bytes).
fn varint(b: &[u8], at: &mut usize) -> Result<u64, String> {
    let mut v = 0u64;
    for i in 0..9 {
        let byte = *b
            .get(*at)
            .ok_or_else(|| damaged("a number runs past its page"))?;
        *at += 1;
        if i == 8 {
            return Ok((v << 8) | u64::from(byte));
        }
        v = (v << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    Ok(v)
}

#[derive(Clone, Copy, PartialEq)]
enum PageKind {
    TableInterior,
    TableLeaf,
    IndexInterior,
    IndexLeaf,
}
struct BTreePage {
    data: Vec<u8>,
    /// Offset of the b-tree header (100 on page 1).
    base: usize,
    kind: PageKind,
    cells: usize,
    right: u64,
}
impl BTreePage {
    fn read(db: &mut Db, n: u64) -> Result<Self, String> {
        let mut data = db.page(n)?;
        data.truncate(db.usable as usize);
        let base = if n == 1 { 100 } else { 0 };
        let kind = match data[base] {
            2 => PageKind::IndexInterior,
            5 => PageKind::TableInterior,
            10 => PageKind::IndexLeaf,
            13 => PageKind::TableLeaf,
            _ => return Err(damaged(&format!("page {n} is not a b-tree page"))),
        };
        let cells = usize::from(u16::from_be_bytes([data[base + 3], data[base + 4]]));
        let interior = matches!(kind, PageKind::TableInterior | PageKind::IndexInterior);
        let right = if interior {
            u64::from(u32::from_be_bytes(
                data[base + 8..base + 12].try_into().unwrap_or_default(),
            ))
        } else {
            0
        };
        let header = if interior { 12 } else { 8 };
        if base + header + cells * 2 > data.len() {
            return Err(damaged(&format!("page {n} declares too many cells")));
        }
        Ok(Self {
            data,
            base,
            kind,
            cells,
            right,
        })
    }
    fn cell_offset(&self, i: usize) -> Result<usize, String> {
        let header = if matches!(self.kind, PageKind::TableInterior | PageKind::IndexInterior) {
            12
        } else {
            8
        };
        if i >= self.cells {
            return Err(damaged("cell index outside its page"));
        }
        let at = self.base + header + i * 2;
        let pair = self
            .data
            .get(at..at + 2)
            .ok_or_else(|| damaged("missing cell pointer"))?;
        let off = usize::from(u16::from_be_bytes([pair[0], pair[1]]));
        if off < self.base + header + self.cells * 2 {
            return Err(damaged("a cell overlaps its page header"));
        }
        if off >= self.data.len() {
            return Err(damaged("a cell points outside its page"));
        }
        Ok(off)
    }
    fn left_child(&self, i: usize) -> Result<u64, String> {
        let off = self.cell_offset(i)?;
        let b = self
            .data
            .get(off..off + 4)
            .ok_or_else(|| damaged("a cell runs past its page"))?;
        Ok(u64::from(u32::from_be_bytes(
            b.try_into().unwrap_or_default(),
        )))
    }
    /// The record bytes of cell `i` (leaf cells, and index interior cells), overflow included.
    fn payload(&self, db: &mut Db, i: usize) -> Result<(Option<i64>, Vec<u8>), String> {
        let mut at = self.cell_offset(i)?;
        if self.kind == PageKind::IndexInterior {
            at += 4;
        }
        let size = varint(&self.data, &mut at)?;
        let rowid = if self.kind == PageKind::TableLeaf {
            Some(varint(&self.data, &mut at)? as i64)
        } else {
            None
        };
        if size > db.len {
            return Err(damaged("a row is larger than the database"));
        }
        if size > MAX_PAYLOAD {
            return Err(format!("A row is larger than {} MiB", MAX_PAYLOAD >> 20));
        }
        let u = db.usable;
        let x = if self.kind == PageKind::TableLeaf {
            u - 35
        } else {
            (u - 12) * 64 / 255 - 23
        };
        let local = if size <= x {
            size
        } else {
            let m = (u - 12) * 32 / 255 - 23;
            let k = m + (size - m) % (u - 4);
            if k <= x { k } else { m }
        } as usize;
        let mut out = self
            .data
            .get(at..at + local)
            .ok_or_else(|| damaged("a cell runs past its page"))?
            .to_vec();
        if (local as u64) < size {
            let next = self
                .data
                .get(at + local..at + local + 4)
                .ok_or_else(|| damaged("a cell runs past its page"))?;
            let mut next = u64::from(u32::from_be_bytes(next.try_into().unwrap_or_default()));
            let mut seen = HashSet::new();
            while (out.len() as u64) < size {
                if !seen.insert(next) {
                    return Err(damaged("an overflow chain loops"));
                }
                let page = db.page(next)?;
                next = u64::from(u32::from_be_bytes(page[..4].try_into().unwrap_or_default()));
                let take = ((size - out.len() as u64).min(u - 4)) as usize;
                out.extend(&page[4..4 + take]);
            }
        }
        Ok((rowid, out))
    }
}

// ----- records -----

/// A stored record value; never evaluated.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}
fn record(db: &Db, b: &[u8]) -> Result<Vec<Value>, String> {
    let mut at = 0;
    let header = usize::try_from(varint(b, &mut at)?)
        .map_err(|_| damaged("record header length overflow"))?;
    if header > b.len() || header < at {
        return Err(damaged("a record header is invalid"));
    }
    let mut types = vec![];
    while at < header {
        types.push(varint(
            b.get(..header)
                .ok_or_else(|| damaged("record header is missing"))?,
            &mut at,
        )?);
        if types.len() > 32_768 {
            return Err(damaged("a record has too many columns"));
        }
    }
    let mut body = header;
    let mut take = |n: usize| -> Result<&[u8], String> {
        let end = body
            .checked_add(n)
            .ok_or_else(|| damaged("record length overflow"))?;
        let s = b
            .get(body..end)
            .ok_or_else(|| damaged("a record runs past its data"))?;
        body = end;
        Ok(s)
    };
    let int = |s: &[u8]| -> i64 {
        let mut v: i64 = if s[0] & 0x80 != 0 { -1 } else { 0 };
        for &byte in s {
            v = (v << 8) | i64::from(byte);
        }
        v
    };
    types
        .into_iter()
        .map(|t| {
            Ok(match t {
                0 => Value::Null,
                1 => Value::Int(int(take(1)?)),
                2 => Value::Int(int(take(2)?)),
                3 => Value::Int(int(take(3)?)),
                4 => Value::Int(int(take(4)?)),
                5 => Value::Int(int(take(6)?)),
                6 => Value::Int(int(take(8)?)),
                7 => Value::Real(f64::from_be_bytes(take(8)?.try_into().unwrap_or_default())),
                8 => Value::Int(0),
                9 => Value::Int(1),
                10 | 11 => return Err(damaged("a record uses a reserved type")),
                n if n % 2 == 0 => Value::Blob(
                    take(
                        usize::try_from((n - 12) / 2)
                            .map_err(|_| damaged("blob length overflow"))?,
                    )?
                    .to_vec(),
                ),
                n => Value::Text(db.text(take(
                    usize::try_from((n - 13) / 2).map_err(|_| damaged("text length overflow"))?,
                )?)),
            })
        })
        .collect()
}
fn show(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::Int(i) => Some(i.to_string()),
        Value::Real(r) => Some(format!("{r:?}")),
        Value::Text(t) => Some(t.clone()),
        Value::Blob(b) => {
            let mut s = String::from("x'");
            for byte in b.iter().take(BLOB_SHOWN / 2) {
                s.push_str(&format!("{byte:02x}"));
            }
            s.push('\'');
            if b.len() > BLOB_SHOWN / 2 {
                s.push_str(&format!("… ({} bytes)", b.len()));
            }
            Some(s)
        }
    }
}

// ----- walking a b-tree -----

/// A run of rows in key order: `count` cells of a leaf page from `first`, or one interior
/// cell of an index b-tree (WITHOUT ROWID tables keep rows in interior cells too).
#[derive(Clone, Copy)]
struct Run {
    page: u64,
    first: usize,
    count: usize,
}

/// Enumerate page runs with an explicit stack. Index interior cells are rows too.
fn runs(db: &mut Db, root: u64) -> Result<Vec<Run>, String> {
    enum Task {
        Visit(u64, usize),
        Emit(Run),
    }
    let mut stack = vec![Task::Visit(root, 0)];
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    while let Some(task) = stack.pop() {
        let (page, depth) = match task {
            Task::Emit(run) => {
                out.push(run);
                continue;
            }
            Task::Visit(page, depth) => (page, depth),
        };
        if depth > MAX_DEPTH {
            return Err(damaged("the b-tree is too deep"));
        }
        if !seen.insert(page) {
            return Err(damaged("the b-tree loops"));
        }
        if seen.len() > MAX_PAGES || out.len() + stack.len() > MAX_PAGES {
            return Err("The table exceeds the 4 million page/run limit".into());
        }
        let p = BTreePage::read(db, page)?;
        match p.kind {
            PageKind::TableLeaf | PageKind::IndexLeaf => {
                if p.cells > 0 {
                    out.push(Run {
                        page,
                        first: 0,
                        count: p.cells,
                    });
                }
            }
            PageKind::TableInterior | PageKind::IndexInterior => {
                stack.push(Task::Visit(p.right, depth + 1));
                for i in (0..p.cells).rev() {
                    if p.kind == PageKind::IndexInterior {
                        stack.push(Task::Emit(Run {
                            page,
                            first: i,
                            count: 1,
                        }));
                    }
                    stack.push(Task::Visit(p.left_child(i)?, depth + 1));
                }
            }
        }
    }
    Ok(out)
}

// ----- the schema -----

#[derive(Clone, Debug)]
struct DbColumn {
    name: String,
    declared: String,
    /// `INTEGER PRIMARY KEY`: stored as the rowid, NULL in the record.
    rowid: bool,
    /// Virtual generated column: not stored in the record.
    stored: bool,
}
#[derive(Clone, Debug)]
struct Table {
    name: String,
    kind: String,
    root: u64,
    columns: Vec<DbColumn>,
}

/// SQL tokens with their byte ranges (identifiers unquoted; strings and comments skipped).
fn tokens(sql: &str) -> Vec<(String, std::ops::Range<usize>)> {
    let b = sql.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b'"' | b'`' | b'[' | b'\'' => {
                let close = if c == b'[' { b']' } else { c };
                i += 1;
                let mut text = String::new();
                while i < b.len() {
                    if b[i] == close {
                        if close != b']' && b.get(i + 1) == Some(&close) {
                            text.push(close as char);
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    let ch = sql[i..].chars().next().unwrap_or(' ');
                    text.push(ch);
                    i += ch.len_utf8();
                }
                i = (i + 1).min(b.len());
                // String literals are kept quoted so they never read as keywords.
                out.push((
                    if c == b'\'' {
                        format!("'{text}'")
                    } else {
                        text
                    },
                    start..i,
                ));
            }
            b'(' | b')' | b',' => {
                i += 1;
                out.push(((c as char).to_string(), start..i));
            }
            _ => {
                while i < b.len()
                    && !matches!(
                        b[i],
                        b' ' | b'\t'
                            | b'\n'
                            | b'\r'
                            | b'('
                            | b')'
                            | b','
                            | b'"'
                            | b'`'
                            | b'['
                            | b'\''
                    )
                {
                    i += 1;
                }
                out.push((sql[start..i].to_owned(), start..i));
            }
        }
    }
    out
}

/// DbColumns of a `CREATE TABLE` statement, best effort; `None` for `CREATE TABLE ... AS SELECT`.
fn columns(sql: &str) -> (Option<Vec<DbColumn>>, bool) {
    let t = tokens(sql);
    let upper = |i: usize| {
        t.get(i)
            .map(|(s, _)| s.to_ascii_uppercase())
            .unwrap_or_default()
    };
    let without_rowid =
        t.windows(2).rev().take(4).any(|w| {
            w[0].0.eq_ignore_ascii_case("WITHOUT") && w[1].0.eq_ignore_ascii_case("ROWID")
        });
    let Some(open) = t.iter().position(|(s, _)| s == "(") else {
        return (None, without_rowid);
    };
    if t[..open].iter().any(|(s, _)| s.eq_ignore_ascii_case("AS")) {
        return (None, without_rowid);
    }
    // Split the definitions at top-level commas.
    let mut defs: Vec<Vec<usize>> = vec![vec![]];
    let mut depth = 0;
    for (i, (s, _)) in t.iter().enumerate().skip(open + 1) {
        match s.as_str() {
            "(" => depth += 1,
            ")" if depth == 0 => break,
            ")" => depth -= 1,
            "," if depth == 0 => {
                defs.push(vec![]);
                continue;
            }
            _ => {}
        }
        if let Some(def) = defs.last_mut() {
            def.push(i);
        }
    }
    const CONSTRAINTS: [&str; 5] = ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"];
    const COLUMN_KEYWORDS: [&str; 11] = [
        "CONSTRAINT",
        "PRIMARY",
        "NOT",
        "NULL",
        "UNIQUE",
        "CHECK",
        "DEFAULT",
        "COLLATE",
        "REFERENCES",
        "GENERATED",
        "AS",
    ];
    let mut cols = vec![];
    let mut column_pk: Vec<String> = vec![];
    let mut table_pk: Vec<String> = vec![];
    for def in defs.iter().filter(|d| !d.is_empty()) {
        let first = upper(def[0]);
        if CONSTRAINTS.contains(&first.as_str()) {
            // PRIMARY KEY (a, b) as a table constraint.
            if let Some(p) = def.iter().position(|&i| upper(i) == "PRIMARY") {
                let mut depth = 0;
                for &i in &def[p..] {
                    match t[i].0.as_str() {
                        "(" => depth += 1,
                        ")" => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        s if depth == 1 && s != "," => {
                            if !["ASC", "DESC", "COLLATE"]
                                .contains(&s.to_ascii_uppercase().as_str())
                            {
                                table_pk.push(s.to_owned());
                            }
                        }
                        _ => {}
                    }
                }
            }
            continue;
        }
        let name = t[def[0]].0.clone();
        let type_end = def[1..]
            .iter()
            .position(|&i| COLUMN_KEYWORDS.contains(&upper(i).as_str()))
            .map_or(def.len(), |p| p + 1);
        let declared = if type_end > 1 {
            sql[t[def[1]].1.start..t[def[type_end - 1]].1.end].to_owned()
        } else {
            String::new()
        };
        let words: Vec<String> = def.iter().map(|&i| upper(i)).collect();
        let pk = words.windows(2).any(|w| w[0] == "PRIMARY" && w[1] == "KEY");
        if pk {
            column_pk.push(name.clone());
        }
        let generated = words.iter().any(|w| w == "AS") && !words.iter().any(|w| w == "STORED");
        cols.push(DbColumn {
            rowid: pk
                && declared.eq_ignore_ascii_case("INTEGER")
                && !words.contains(&"DESC".to_owned()),
            name,
            declared,
            stored: !generated,
        });
    }
    if let [only] = table_pk.as_slice()
        && let Some(c) = cols.iter_mut().find(|c| c.name.eq_ignore_ascii_case(only))
        && c.declared.eq_ignore_ascii_case("INTEGER")
    {
        c.rowid = true;
    }
    if without_rowid {
        // Rows are stored primary key first, then the other columns in order.
        for c in &mut cols {
            c.rowid = false;
        }
        let pk = if table_pk.is_empty() {
            column_pk
        } else {
            table_pk
        };
        let mut ordered: Vec<DbColumn> = pk
            .iter()
            .filter_map(|n| {
                cols.iter()
                    .find(|c| c.name.eq_ignore_ascii_case(n))
                    .cloned()
            })
            .collect();
        ordered.extend(
            cols.into_iter()
                .filter(|c| !pk.iter().any(|n| n.eq_ignore_ascii_case(&c.name))),
        );
        return (Some(ordered), true);
    }
    (Some(cols), false)
}
fn schema(db: &mut Db) -> Result<Vec<Table>, String> {
    let mut tables = vec![Table {
        name: "sqlite_schema".into(),
        kind: "table".into(),
        root: 1,
        columns: ["type", "name", "tbl_name", "rootpage", "sql"]
            .iter()
            .map(|n| DbColumn {
                name: (*n).into(),
                declared: if *n == "rootpage" { "integer" } else { "text" }.into(),
                rowid: false,
                stored: true,
            })
            .collect(),
    }];
    let mut schema_bytes = 0usize;
    for run in runs(db, 1)? {
        let page = BTreePage::read(db, run.page)?;
        for i in run.first..run.first + run.count {
            let (_, payload) = page.payload(db, i)?;
            let values = record(db, &payload)?;
            let text = |i: usize| match values.get(i) {
                Some(Value::Text(s)) => s.clone(),
                _ => String::new(),
            };
            let kind = text(0);
            let sql = text(4);
            if sql.len() > MAX_SQL_BYTES {
                return Err("A CREATE statement exceeds 1 MiB".into());
            }
            schema_bytes = schema_bytes
                .checked_add(payload.len())
                .ok_or("Schema size overflow")?;
            if schema_bytes > MAX_SCHEMA_BYTES {
                return Err("The schema exceeds 64 MiB".into());
            }
            if !sql.is_empty() {
                db.definitions.push(sql.clone());
            }
            if db.definitions.len() > MAX_TABLES || tables.len() >= MAX_TABLES {
                return Err("The schema exceeds 200,000 entries".into());
            }
            if kind != "table" && kind != "view" {
                continue;
            }
            let root = match values.get(3) {
                Some(Value::Int(r)) => *r as u64,
                _ => 0,
            };
            let virtual_table = sql
                .split_whitespace()
                .nth(1)
                .is_some_and(|w| w.eq_ignore_ascii_case("VIRTUAL"));
            let (cols, _) = columns(&sql);
            tables.push(Table {
                name: text(1),
                kind: if virtual_table {
                    "virtual table".into()
                } else {
                    kind
                },
                root,
                columns: cols.unwrap_or_default(),
            });
        }
    }
    // User tables first, then internal ones, the schema table last.
    let schema_table = tables.remove(0);
    tables.sort_by_key(|t| (t.kind != "table", t.name.starts_with("sqlite_")));
    tables.push(schema_table);
    Ok(tables)
}

/// The table to show: the one asked for, else the first ordinary table.
fn pick<'a>(tables: &'a [Table], name: Option<&str>) -> Result<&'a Table, String> {
    match name {
        Some(n) => tables
            .iter()
            .find(|t| t.name == n)
            .ok_or_else(|| format!("There is no table named \"{n}\"")),
        None => Ok(tables
            .iter()
            .find(|t| t.kind == "table")
            .or_else(|| tables.first())
            .ok_or_else(|| "The database has no tables".to_owned())?),
    }
}

fn display_columns(table: &Table, runs: &[Run], db: &mut Db) -> Result<Vec<DbColumn>, String> {
    if !table.columns.is_empty() {
        return Ok(table.columns.clone());
    }
    // Unknown column list (CREATE TABLE ... AS SELECT): as many as the first record has.
    let n = match runs.first() {
        Some(run) => {
            let page = BTreePage::read(db, run.page)?;
            let (_, payload) = page.payload(db, run.first)?;
            record(db, &payload)?.len()
        }
        None => 0,
    };
    Ok((1..=n)
        .map(|i| DbColumn {
            name: format!("column {i}"),
            declared: String::new(),
            rowid: false,
            stored: true,
        })
        .collect())
}

pub fn open(path: &Path, table: Option<&str>) -> Result<Box<dyn TableSource>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    Ok(Box::new(Sqlite::new(Db::new(file)?, table)?))
}

struct Sqlite {
    db: Db,
    schema: Schema,
    runs: Vec<Run>,
    columns: Vec<DbColumn>,
}

impl Sqlite {
    fn new(mut db: Db, name: Option<&str>) -> Result<Self, String> {
        let tables = schema(&mut db)?;
        let chosen = pick(&tables, name)?.clone();
        if chosen.kind != "table" || chosen.root == 0 {
            return Err(format!(
                "{:?} is a {}; Isolinear reads stored tables only (it never runs SQL)",
                chosen.name, chosen.kind
            ));
        }
        let selected_runs = runs(&mut db, chosen.root)?;
        let cols = display_columns(&chosen, &selected_runs, &mut db)?;
        if cols.len() > MAX_COLUMNS {
            return Err("The table exceeds 32,768 columns".into());
        }
        let total = selected_runs.iter().map(|r| r.count as u64).sum();
        let mut infos = Vec::new();
        let mut warnings = Vec::new();
        for table in &tables {
            let rows = if table.name == chosen.name {
                Some(total)
            } else if db.len <= COUNT_ALL_LIMIT && table.kind == "table" && table.root != 0 {
                match runs(&mut db, table.root) {
                    Ok(runs) => Some(runs.iter().map(|r| r.count as u64).sum()),
                    Err(e) => {
                        warnings.push(format!("{}: {e}", table.name));
                        None
                    }
                }
            } else {
                None
            };
            infos.push(TableInfo {
                name: table.name.clone(),
                rows,
            });
        }
        let h = &db.header;
        let be32 = |at: usize| {
            u32::from_be_bytes(
                h.get(at..at + 4)
                    .and_then(|b| b.try_into().ok())
                    .unwrap_or_default(),
            )
        };
        let mut details = vec![
            ("Page size".into(), format!("{} bytes", db.page_size)),
            ("Pages".into(), db.pages.to_string()),
            (
                "Text encoding".into(),
                match db.utf16 {
                    None => "UTF-8",
                    Some(true) => "UTF-16le",
                    Some(false) => "UTF-16be",
                }
                .into(),
            ),
            ("Schema version".into(), be32(40).to_string()),
            ("User version".into(), be32(60).to_string()),
            ("Application id".into(), format!("0x{:08x}", be32(68))),
            (
                "Journal mode".into(),
                if h[18] == 2 { "WAL" } else { "rollback" }.into(),
            ),
            (
                "SQL".into(),
                "Definitions are shown as text; SQL is never run".into(),
            ),
        ];
        if h[18] == 2 {
            warnings.push("Only the main database is read; committed changes in a write-ahead log may be missing".into());
        }
        for t in &tables {
            if t.kind != "table" {
                details.push((
                    t.name.clone(),
                    format!("{}; cannot be read without running SQL", t.kind),
                ));
            }
        }
        for warning in warnings {
            details.push(("Warning".into(), warning));
        }
        let schema = Schema {
            table: tables
                .iter()
                .position(|t| t.name == chosen.name)
                .unwrap_or(0),
            tables: infos,
            rows: Some(total),
            columns: cols
                .iter()
                .map(|c| Column {
                    name: c.name.clone(),
                    type_name: format!(
                        "{}{}{}",
                        if c.declared.is_empty() {
                            "any"
                        } else {
                            &c.declared
                        },
                        if c.rowid { " (rowid)" } else { "" },
                        if c.stored {
                            ""
                        } else {
                            " (computed, not stored)"
                        }
                    ),
                })
                .collect(),
            details,
            text: Some(db.definitions.join(";\n")),
        };
        db.reads = 0;
        Ok(Self {
            db,
            schema,
            runs: selected_runs,
            columns: cols,
        })
    }
}

impl TableSource for Sqlite {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn rows(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        let total = self.schema.rows.unwrap_or(0);
        let end = start.saturating_add(count as u64).min(total);
        if start >= end {
            return Ok(Vec::new());
        }
        if (end - start).saturating_mul(self.columns.len().max(1) as u64) > MAX_PAGE_CELLS as u64 {
            return Err("A page request exceeds 1,000,000 cells; request fewer rows".into());
        }
        self.db.reads = 0;
        let mut out = Vec::new();
        let mut shown_bytes = 0usize;
        let mut at = 0u64;
        for run in &self.runs {
            let run_end = at + run.count as u64;
            if at >= end {
                break;
            }
            if run_end > start {
                let page = BTreePage::read(&mut self.db, run.page)?;
                let first = start.saturating_sub(at) as usize;
                let last = (end.min(run_end) - at) as usize;
                for i in first..last {
                    let (rowid, payload) = page.payload(&mut self.db, run.first + i)?;
                    let values = record(&self.db, &payload)?;
                    let mut slot = 0;
                    let row: Vec<Option<String>> = self
                        .columns
                        .iter()
                        .map(|c| {
                            let value = if c.rowid {
                                rowid.map(Value::Int).unwrap_or(Value::Null)
                            } else if c.stored {
                                values.get(slot).cloned().unwrap_or(Value::Null)
                            } else {
                                Value::Null
                            };
                            if c.stored {
                                slot += 1;
                            }
                            let declared = c.declared.to_ascii_uppercase();
                            let value = match value {
                                Value::Int(i)
                                    if !declared.contains("INT")
                                        && ["REAL", "FLOA", "DOUB"]
                                            .iter()
                                            .any(|s| declared.contains(s)) =>
                                {
                                    Value::Real(i as f64)
                                }
                                v => v,
                            };
                            show(&value).map(cut)
                        })
                        .collect();
                    shown_bytes = row
                        .iter()
                        .flatten()
                        .try_fold(shown_bytes, |total, cell| total.checked_add(cell.len()))
                        .ok_or("Page text size overflow")?;
                    if shown_bytes > MAX_RESULT_BYTES {
                        return Err("A page exceeds 256 MiB of text; request fewer rows".into());
                    }
                    out.push(row);
                }
            }
            at = run_end;
        }
        Ok(out)
    }
}

// ----- tables of a database, for standards built on SQLite -----

impl Value {
    /// The value as text for a table cell (BLOBs as elided hex); empty for NULL.
    pub fn display(&self) -> String {
        show(self).unwrap_or_default()
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(i) => Some(*i as f64),
            Self::Real(r) => Some(*r),
            Self::Text(t) => t.trim().parse().ok(),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Real(r) if r.fract() == 0.0 && r.abs() < 9e15 => Some(*r as i64),
            Self::Text(t) => t.trim().parse().ok(),
            _ => None,
        }
    }
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(t),
            _ => None,
        }
    }
    pub fn as_blob(&self) -> Option<&[u8]> {
        match self {
            Self::Blob(b) => Some(b),
            Self::Text(t) => Some(t.as_bytes()),
            _ => None,
        }
    }
}

/// Read-only access to the stored tables of a database already in memory, for standards built
/// on SQLite (GeoPackage, MBTiles). The same checked page reader as the table view: no SQL is
/// run, views cannot be read, and no write-ahead log is looked for.
pub struct Reader {
    db: Db,
    tables: Vec<Table>,
}
impl Reader {
    /// Opens the database at `path`, reading pages only as they are needed. No write-ahead log is
    /// looked for.
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let mut db = Db::new(file)?;
        let tables = schema(&mut db)?;
        Ok(Self { db, tables })
    }
    fn header32(&self, at: usize) -> u32 {
        u32::from_be_bytes(self.db.header[at..at + 4].try_into().unwrap_or_default())
    }
    /// The header's application id (GeoPackage writes "GPKG").
    pub fn application_id(&self) -> u32 {
        self.header32(68)
    }
    pub fn user_version(&self) -> u32 {
        self.header32(60)
    }
    /// Whether the header says the database uses a write-ahead log.
    pub fn wal_mode(&self) -> bool {
        self.db.header[18] == 2
    }
    /// Tables and views: (name, kind), user tables first.
    pub fn tables(&self) -> Vec<(String, String)> {
        self.tables
            .iter()
            .map(|t| (t.name.clone(), t.kind.clone()))
            .collect()
    }
    fn find(&self, name: &str) -> Option<&Table> {
        self.tables
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
    }
    /// The kind of `name` ("table", "view", "virtual table"), if it exists.
    pub fn kind(&self, name: &str) -> Option<&str> {
        self.find(name).map(|t| t.kind.as_str())
    }
    /// Column names of a table as declared (empty when they cannot be read from its SQL).
    pub fn columns(&self, name: &str) -> Option<Vec<String>> {
        self.find(name)
            .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
    }
    /// Declared column types of a table, in column order.
    pub fn column_types(&self, name: &str) -> Option<Vec<String>> {
        self.find(name)
            .map(|t| t.columns.iter().map(|c| c.declared.clone()).collect())
    }
    /// Rows of the stored table `name` in key order: `each` gets the values by column (an
    /// `INTEGER PRIMARY KEY` filled from the rowid) for up to `limit` rows, and may return
    /// false to stop. Returns the number of rows in the table.
    pub fn rows(
        &mut self,
        name: &str,
        limit: usize,
        cancelled: &dyn Fn() -> bool,
        each: &mut dyn FnMut(Vec<Value>) -> bool,
    ) -> Result<u64, String> {
        let table = self
            .find(name)
            .cloned()
            .ok_or_else(|| format!("There is no table named \"{name}\""))?;
        if table.kind != "table" || table.root == 0 {
            return Err(format!(
                "\"{}\" is a {}; it cannot be read without running SQL",
                table.name, table.kind
            ));
        }
        let db = &mut self.db;
        let runs = runs(db, table.root)?;
        let total: u64 = runs.iter().map(|r| r.count as u64).sum();
        let cols = display_columns(&table, &runs, db)?;
        let mut slot = vec![None; cols.len()];
        let mut n = 0;
        for (i, c) in cols.iter().enumerate() {
            if c.stored {
                slot[i] = Some(n);
                n += 1;
            }
        }
        let real: Vec<bool> = cols
            .iter()
            .map(|c| {
                let t = c.declared.to_ascii_uppercase();
                !t.contains("INT") && ["REAL", "FLOA", "DOUB"].iter().any(|k| t.contains(k))
            })
            .collect();
        let mut seen = 0usize;
        'runs: for run in &runs {
            if seen >= limit {
                break;
            }
            if cancelled() {
                return Err("Cancelled".into());
            }
            let page = BTreePage::read(db, run.page)?;
            for i in 0..run.count {
                if seen >= limit {
                    break 'runs;
                }
                let (rowid, payload) = page.payload(db, run.first + i)?;
                let values = record(db, &payload)?;
                let row = cols
                    .iter()
                    .enumerate()
                    .map(|(c, col)| {
                        let v = if col.rowid {
                            rowid.map(Value::Int).unwrap_or(Value::Null)
                        } else {
                            slot[c]
                                .and_then(|s| values.get(s).cloned())
                                .unwrap_or(Value::Null)
                        };
                        match v {
                            Value::Int(i) if real[c] => Value::Real(i as f64),
                            v => v,
                        }
                    })
                    .collect();
                seen += 1;
                if !each(row) {
                    break 'runs;
                }
            }
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests;
