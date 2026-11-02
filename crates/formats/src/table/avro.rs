//! Avro object containers: the previous release's schema and binary decoders, with blocks
//! indexed by their headers and decoded only for requested rows. No schema or data is fetched.
use super::{Schema, TableInfo, TableSource};
use serde_json::Value as Json;
use std::{
    fmt::Write as _,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};
pub(super) mod display;
mod schema;
mod values;
use display::Text;
use values::{cell, utc};
pub const READY: bool = true;
const MAGIC: &[u8] = b"Obj\x01";
const MAX_BLOCK: u64 = 256 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_ITEMS: u64 = 50_000_000;
const MAX_META: usize = 10_000;
#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub footer_bytes: u64,
    pub columns: usize,
    pub row_groups: usize,
    pub batch_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            footer_bytes: 64 * 1024 * 1024,
            columns: 10_000,
            row_groups: 1_000_000,
            batch_bytes: 256 * 1024 * 1024,
        }
    }
}
struct Column {
    name: String,
    schema: S,
}
fn column(name: String, schema: S) -> Column {
    Column { name, schema }
}
#[derive(Clone, Debug)]
enum S {
    Null,
    Bool,
    Int,
    Long,
    Float,
    Double,
    Bytes,
    Str,
    /// A record, enum or fixed type, by index into `Types::named` (allows recursive types).
    Named(usize),
    Array(Box<S>),
    Map(Box<S>),
    Union(Vec<S>),
    Logical(Box<S>, Logical),
}
#[derive(Clone, Copy, Debug)]
enum Logical {
    Date,
    TimeMillis,
    TimeMicros,
    /// Units per second: 1,000 (millis), 1,000,000 (micros) or 1,000,000,000 (nanos).
    Timestamp(i64),
    Decimal(u32),
}
#[derive(Clone, Debug)]
enum Named {
    Record {
        name: String,
        fields: Vec<(String, S)>,
    },
    Enum {
        name: String,
        symbols: Vec<String>,
    },
    Fixed {
        name: String,
        size: usize,
    },
}

#[derive(Default)]
struct Types {
    named: Vec<Named>,
    /// Full and short names of the named types.
    names: Vec<(String, usize)>,
}
impl Types {
    fn lookup(&self, name: &str, namespace: &str) -> Option<usize> {
        if !namespace.is_empty() && !name.contains('.') {
            let qualified = format!("{namespace}.{name}");
            if let Some((_, index)) = self.names.iter().find(|(n, _)| n == &qualified) {
                return Some(*index);
            }
        }
        self.names
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, index)| *index)
    }

    fn define(&mut self, j: &Json, namespace: &str, named: Named) -> (usize, String) {
        let name = j.get("name").and_then(Json::as_str).unwrap_or("anonymous");
        let ns = j
            .get("namespace")
            .and_then(Json::as_str)
            .unwrap_or(namespace);
        let (full, ns) = match name.rsplit_once('.') {
            Some((space, _)) => (name.to_owned(), space.to_owned()),
            None if ns.is_empty() => (name.to_owned(), String::new()),
            None => (format!("{ns}.{name}"), ns.to_owned()),
        };
        let index = self.named.len();
        self.named.push(named);
        let short = full.rsplit('.').next().unwrap_or(&full).to_owned();
        self.names.push((full.clone(), index));
        if short != full {
            self.names.push((short, index));
        }
        (index, ns)
    }
    /// Wrap `base` in its logical type when the annotation fits the underlying type.
    fn logical(&self, j: &Json, base: S) -> S {
        let fixed = matches!(base, S::Named(i) if matches!(self.named[i], Named::Fixed { .. }));
        let logical = match (j.get("logicalType").and_then(Json::as_str), &base) {
            (Some("date"), S::Int) => Logical::Date,
            (Some("time-millis"), S::Int) => Logical::TimeMillis,
            (Some("time-micros"), S::Long) => Logical::TimeMicros,
            (Some("timestamp-millis" | "local-timestamp-millis"), S::Long) => {
                Logical::Timestamp(1_000)
            }
            (Some("timestamp-micros" | "local-timestamp-micros"), S::Long) => {
                Logical::Timestamp(1_000_000)
            }
            (Some("timestamp-nanos" | "local-timestamp-nanos"), S::Long) => {
                Logical::Timestamp(1_000_000_000)
            }
            (Some("decimal"), S::Bytes) | (Some("decimal"), S::Named(_))
                if fixed || matches!(base, S::Bytes) =>
            {
                let scale = match j.get("scale") {
                    Some(Json::Number(n)) => n.as_u64().unwrap_or(0).min(76) as u32,
                    _ => 0,
                };
                Logical::Decimal(scale.min(76))
            }
            _ => return base,
        };
        S::Logical(Box::new(base), logical)
    }
}

fn primitive(name: &str) -> Option<S> {
    Some(match name {
        "null" => S::Null,
        "boolean" => S::Bool,
        "int" => S::Int,
        "long" => S::Long,
        "float" => S::Float,
        "double" => S::Double,
        "bytes" => S::Bytes,
        "string" => S::Str,
        _ => return None,
    })
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
    items: u64,
}
fn damaged(what: &str) -> String {
    format!("The Avro data is damaged: {what}")
}
impl<'a> Cursor<'a> {
    fn long(&mut self) -> Result<i64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self
                .b
                .get(self.i)
                .ok_or_else(|| damaged("a value runs past its block"))?;
            self.i += 1;
            if shift == 63 && byte > 1 {
                return Err(damaged("a number is too large"));
            }
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok((v >> 1) as i64 ^ -((v & 1) as i64));
            }
        }
        Err(damaged("a number is too long"))
    }
    fn int(&mut self) -> Result<i32, String> {
        i32::try_from(self.long()?).map_err(|_| damaged("an int is out of range"))
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .i
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| damaged("a value runs past its block"))?;
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }
    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.long()?;
        let n = usize::try_from(n).map_err(|_| damaged("a negative length"))?;
        self.take(n)
    }
    fn count(&mut self, n: u64) -> Result<(), String> {
        self.items = self
            .items
            .checked_add(n)
            .ok_or_else(|| damaged("too many items in one block"))?;
        if self.items > MAX_ITEMS {
            return Err(damaged("too many items in one block"));
        }
        Ok(())
    }
    /// Items in the next array or map block (a negative count is followed by the block size).
    fn block(&mut self) -> Result<(u64, Option<usize>), String> {
        let raw = self.long()?;
        let (n, end) = if raw < 0 {
            let bytes = usize::try_from(self.long()?)
                .map_err(|_| damaged("a negative array or map block size"))?;
            let end = self
                .i
                .checked_add(bytes)
                .filter(|end| *end <= self.b.len())
                .ok_or_else(|| damaged("an array or map block runs past its container"))?;
            (raw.unsigned_abs(), Some(end))
        } else {
            (raw as u64, None)
        };
        self.count(n)?;
        Ok((n, end))
    }
    fn bool(&mut self) -> Result<bool, String> {
        match self.take(1)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(damaged("a boolean is neither 0 nor 1")),
        }
    }
    fn float(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(
            self.take(4)?.try_into().unwrap_or_default(),
        ))
    }
    fn double(&mut self) -> Result<f64, String> {
        Ok(f64::from_le_bytes(
            self.take(8)?.try_into().unwrap_or_default(),
        ))
    }
    fn branch<'s>(&mut self, branches: &'s [S]) -> Result<&'s S, String> {
        let i = self.long()?;
        usize::try_from(i)
            .ok()
            .and_then(|i| branches.get(i))
            .ok_or_else(|| damaged("a union branch does not exist"))
    }
}

fn hex(bytes: &[u8], out: &mut Text) {
    display::hex(bytes, out, true);
}
fn date(days: i64) -> String {
    if days.unsigned_abs() > 100_000_000 {
        return format!("{days} days");
    }
    utc(days * 86400)
        .split(' ')
        .next()
        .unwrap_or_default()
        .to_owned()
}
fn timestamp(value: i64, per_second: i64) -> String {
    let secs = value.div_euclid(per_second);
    if secs.unsigned_abs() > 8_000_000_000_000 {
        return value.to_string();
    }
    let digits = per_second.ilog10() as usize;
    format!("{}.{:0digits$}", utc(secs), value.rem_euclid(per_second))
}
fn time_of_day(value: i64, per_second: i64) -> String {
    let secs = value.div_euclid(per_second);
    let digits = per_second.ilog10() as usize;
    format!(
        "{:02}:{:02}:{:02}.{:0digits$}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60,
        value.rem_euclid(per_second)
    )
}
/// Big-endian two's complement unscaled value with `scale` decimal places.
fn decimal(bytes: &[u8], scale: u32, out: &mut Text) {
    if bytes.len() > 16 {
        return hex(bytes, out);
    }
    let mut v: i128 = if bytes.first().is_some_and(|b| b & 0x80 != 0) {
        -1
    } else {
        0
    };
    for &b in bytes {
        v = (v << 8) | i128::from(b);
    }
    let digits = v.unsigned_abs().to_string();
    let scale = scale as usize;
    if v < 0 {
        out.push('-');
    }
    if scale == 0 {
        out.push_str(&digits);
    } else {
        let padded = format!("{digits:0>width$}", width = scale + 1);
        let (int, frac) = padded.split_at(padded.len() - scale);
        let _ = write!(out, "{int}.{frac}");
    }
}

/// Decode one value of schema `s`, appending its text. Strings are quoted inside containers.
struct Header {
    types: Types,
    columns: Vec<Column>,
    codec: String,
    sync: [u8; 16],
    metadata: Vec<(String, String)>,
    schema_text: String,
}
struct Block {
    /// Offset of the block's data.
    offset: u64,
    rows: u64,
    size: u64,
}

fn read_long(r: &mut impl Read) -> Result<i64, String> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let mut byte = [0u8];
        r.read_exact(&mut byte)
            .map_err(|_| damaged("the file ends inside a header"))?;
        if shift == 63 && byte[0] > 1 {
            return Err(damaged("a number is too large"));
        }
        v |= u64::from(byte[0] & 0x7f) << shift;
        if byte[0] & 0x80 == 0 {
            return Ok((v >> 1) as i64 ^ -((v & 1) as i64));
        }
    }
    Err(damaged("a number is too long"))
}
fn read_bytes(r: &mut BufReader<File>, budget: &mut u64) -> Result<Vec<u8>, String> {
    let n = u64::try_from(read_long(r)?).map_err(|_| damaged("a negative length"))?;
    if n > *budget
        || n > r
            .get_ref()
            .metadata()
            .map_err(|e| e.to_string())?
            .len()
            .saturating_sub(r.stream_position().map_err(|e| e.to_string())?)
    {
        return Err("The Avro header is larger than the metadata limit".into());
    }
    *budget -= n;
    let mut v = vec![0; n as usize];
    r.read_exact(&mut v)
        .map_err(|_| damaged("the file ends inside the header"))?;
    Ok(v)
}

fn header(r: &mut BufReader<File>, limits: &Limits) -> Result<Header, String> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if magic != MAGIC {
        return Err("Not an Avro object container file".into());
    }
    let mut budget = limits
        .footer_bytes
        .min(r.get_ref().metadata().map_err(|e| e.to_string())?.len());
    let mut metadata = vec![];
    loop {
        let n = read_long(r)?;
        if n == 0 {
            break;
        }
        let block_end = if n < 0 {
            let size = u64::try_from(read_long(r)?)
                .map_err(|_| damaged("a metadata block has a negative size"))?;
            let end = r
                .stream_position()
                .map_err(|e| e.to_string())?
                .checked_add(size)
                .filter(|end| *end <= r.get_ref().metadata().map_or(0, |m| m.len()))
                .ok_or_else(|| damaged("a metadata block runs past the file"))?;
            Some(end)
        } else {
            None
        };
        for _ in 0..n.unsigned_abs() {
            if metadata.len() >= MAX_META {
                return Err("The Avro header has too many metadata entries".into());
            }
            let key = String::from_utf8_lossy(&read_bytes(r, &mut budget)?).into_owned();
            let value = read_bytes(r, &mut budget)?;
            metadata.push((key, value));
        }
        if let Some(end) = block_end
            && r.stream_position().map_err(|e| e.to_string())? != end
        {
            return Err(damaged("a metadata block does not match its declared size"));
        }
    }
    let mut sync = [0u8; 16];
    r.read_exact(&mut sync)
        .map_err(|_| damaged("the file ends inside the header"))?;
    let schema_text = metadata
        .iter()
        .find(|(k, _)| k == "avro.schema")
        .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
        .ok_or("The Avro header has no schema")?;
    let codec = metadata
        .iter()
        .find(|(k, _)| k == "avro.codec")
        .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_else(|| "null".into());
    if schema_text.len() > 1024 * 1024 {
        return Err("The Avro schema exceeds the 1 MiB limit".into());
    }
    let json: Json =
        serde_json::from_str(&schema_text).map_err(|e| format!("Invalid Avro schema: {e}"))?;
    let mut types = Types::default();
    let root = types
        .parse(&json)
        .map_err(|e| format!("Invalid Avro schema: {e}"))?;
    let columns: Vec<Column> = match &root {
        S::Named(i) => match &types.named[*i] {
            Named::Record { fields, .. } => fields
                .iter()
                .map(|(n, s)| column(n.clone(), s.clone()))
                .collect(),
            _ => vec![column("value".into(), root.clone())],
        },
        _ => vec![column("value".into(), root.clone())],
    };
    if columns.len() > limits.columns {
        return Err(format!("More than {} columns", limits.columns));
    }
    let pretty = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    Ok(Header {
        types,
        columns,
        codec,
        sync,
        metadata: metadata
            .into_iter()
            .filter(|(k, _)| k != "avro.schema")
            .map(|(k, v)| (k, String::from_utf8_lossy(&v).chars().take(4096).collect()))
            .collect(),
        schema_text: pretty,
    })
}

/// The header and every block's position and row count; damage after some blocks keeps those.
fn scan(file: File, limits: &Limits) -> Result<(File, Header, Vec<Block>, Option<String>), String> {
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut r = BufReader::new(file);
    let header = header(&mut r, limits)?;
    let mut pos = r.stream_position().map_err(|e| e.to_string())?;
    let mut blocks = vec![];
    let mut warning = None;
    while pos < len {
        if blocks.len() >= limits.row_groups {
            warning = Some(format!(
                "Only the first {} blocks are read",
                limits.row_groups
            ));
            break;
        }
        let block = (|| -> Result<Block, String> {
            let rows =
                u64::try_from(read_long(&mut r)?).map_err(|_| damaged("a negative row count"))?;
            let size =
                u64::try_from(read_long(&mut r)?).map_err(|_| damaged("a negative block size"))?;
            let offset = r.stream_position().map_err(|e| e.to_string())?;
            if size > MAX_BLOCK {
                return Err(format!("A block is larger than {} MiB", MAX_BLOCK >> 20));
            }
            if rows > MAX_ITEMS
                || offset
                    .checked_add(size)
                    .and_then(|v| v.checked_add(16))
                    .is_none_or(|end| end > len)
            {
                return Err(damaged("a block runs past the end of the file"));
            }
            r.seek_relative(size as i64).map_err(|e| e.to_string())?;
            let mut sync = [0u8; 16];
            r.read_exact(&mut sync).map_err(|e| e.to_string())?;
            if sync != header.sync {
                return Err(damaged("a block's sync marker does not match"));
            }
            Ok(Block { offset, rows, size })
        })();
        match block {
            Ok(b) => {
                pos = r.stream_position().map_err(|e| e.to_string())?;
                blocks.push(b);
            }
            Err(e) => {
                if blocks.is_empty() && !e.starts_with("A block is larger") {
                    return Err(e);
                }
                warning = Some(format!(
                    "{e} (after {} blocks; the rest is not shown)",
                    blocks.len()
                ));
                break;
            }
        }
    }
    Ok((r.into_inner(), header, blocks, warning))
}

fn decompress(codec: &str, raw: Vec<u8>) -> Result<Vec<u8>, String> {
    let capped = |mut r: Box<dyn Read + '_>| -> Result<Vec<u8>, String> {
        let mut out = vec![];
        r.by_ref()
            .take(MAX_BLOCK + 1)
            .read_to_end(&mut out)
            .map_err(|e| damaged(&format!("a block does not decompress ({e})")))?;
        if out.len() as u64 > MAX_BLOCK {
            return Err(format!(
                "A decompressed block is larger than {} MiB",
                MAX_BLOCK >> 20
            ));
        }
        Ok(out)
    };
    match codec {
        "null" => Ok(raw),
        "deflate" => capped(Box::new(flate2::read::DeflateDecoder::new(raw.as_slice()))),
        "zstandard" => {
            let mut decoder =
                zstd::stream::read::Decoder::new(raw.as_slice()).map_err(|e| e.to_string())?;
            decoder.window_log_max(28).map_err(|e| e.to_string())?;
            capped(Box::new(decoder))
        }
        "snappy" => {
            let Some(split) = raw.len().checked_sub(4) else {
                return Err(damaged("a snappy block is too short"));
            };
            let (data, crc) = raw.split_at(split);
            let n = snap::raw::decompress_len(data).map_err(|e| damaged(&e.to_string()))?;
            if n as u64 > MAX_BLOCK {
                return Err(format!(
                    "A decompressed block is larger than {} MiB",
                    MAX_BLOCK >> 20
                ));
            }
            let out = snap::raw::Decoder::new()
                .decompress_vec(data)
                .map_err(|e| damaged(&e.to_string()))?;
            let mut check = flate2::Crc::new();
            check.update(&out);
            if check.sum().to_be_bytes() != crc {
                return Err(damaged("a snappy block's checksum does not match"));
            }
            Ok(out)
        }
        other => Err(format!(
            "Avro blocks compressed with \"{other}\" cannot be read (supported: null, deflate, snappy, zstandard)"
        )),
    }
}

struct Avro {
    file: File,
    header: Header,
    blocks: Vec<Block>,
    schema: Schema,
}

pub fn open(path: &Path, table: Option<&str>) -> Result<Box<dyn TableSource>, String> {
    if table.is_some_and(|name| name != "rows") {
        return Err("An Avro container has one table, named rows".into());
    }
    Avro::open(path).map(|source| Box::new(source) as Box<dyn TableSource>)
}
impl Avro {
    fn open(path: &Path) -> Result<Self, String> {
        let file =
            File::open(path).map_err(|e| format!("The Avro file could not be opened: {e}"))?;
        let (file, header, blocks, warning) = scan(file, &Limits::default())?;
        let rows = blocks
            .iter()
            .try_fold(0u64, |n, b| n.checked_add(b.rows))
            .ok_or_else(|| damaged("the row count is too large"))?;
        let mut details = vec![
            ("Codec".into(), header.codec.clone()),
            ("Blocks".into(), blocks.len().to_string()),
            (
                "Sync markers".into(),
                "Checked for every indexed block".into(),
            ),
        ];
        if let Some(warning) = warning {
            details.insert(0, ("Warning".into(), warning));
        }
        details.extend(header.metadata.clone());
        let schema = Schema {
            tables: vec![TableInfo {
                name: "rows".into(),
                rows: Some(rows),
            }],
            table: 0,
            columns: header
                .columns
                .iter()
                .map(|c| super::Column {
                    name: c.name.clone(),
                    type_name: header.types.describe(&c.schema),
                })
                .collect(),
            rows: Some(rows),
            details,
            text: Some(header.schema_text.clone()),
        };
        Ok(Self {
            file,
            header,
            blocks,
            schema,
        })
    }
}
impl TableSource for Avro {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn rows(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        let end = request_end(&self.schema, start, count)?;
        if end <= start {
            return Ok(vec![]);
        }
        let mut rows = Vec::new();
        let mut at = 0u64;
        let mut bytes = 0usize;
        for (index, block) in self.blocks.iter().enumerate() {
            let block_end = at
                .checked_add(block.rows)
                .ok_or_else(|| damaged("the row count is too large"))?;
            if at >= end {
                break;
            }
            if block_end > start {
                self.file
                    .seek(SeekFrom::Start(block.offset))
                    .map_err(|e| e.to_string())?;
                let file_len = self.file.metadata().map_err(|e| e.to_string())?.len();
                if block
                    .offset
                    .checked_add(block.size)
                    .and_then(|n| n.checked_add(16))
                    .is_none_or(|end| end > file_len)
                {
                    return Err(damaged("a block runs past the end of the file"));
                }
                let mut raw = vec![0; block.size as usize];
                self.file.read_exact(&mut raw).map_err(|e| e.to_string())?;
                let mut sync = [0; 16];
                self.file.read_exact(&mut sync).map_err(|e| e.to_string())?;
                if sync != self.header.sync {
                    return Err(damaged("a block's sync marker changed"));
                }
                let data = decompress(&self.header.codec, raw)
                    .map_err(|e| format!("{e} (block {})", index + 1))?;
                let mut c = Cursor {
                    b: &data,
                    i: 0,
                    items: 0,
                };
                let from = start.saturating_sub(at);
                let to = (end - at).min(block.rows);
                for row in 0..to {
                    c.count(1)?;
                    let mut cells = Vec::new();
                    for col in &self.header.columns {
                        let value = cell(&mut c, &col.schema, &self.header.types).map_err(|e| {
                            format!("{e} (block {}, row {row}, column {})", index + 1, col.name)
                        })?;
                        if row >= from {
                            bytes = bytes.saturating_add(value.as_ref().map_or(0, String::len));
                            check_output(bytes)?;
                            cells.push(value);
                        }
                    }
                    if row >= from {
                        rows.push(cells);
                    }
                }
                if to == block.rows && c.i != data.len() {
                    return Err(damaged("a block contains bytes after its declared rows"));
                }
            }
            at = block_end;
        }
        Ok(rows)
    }
}

pub(super) fn request_end(schema: &Schema, start: u64, count: usize) -> Result<u64, String> {
    let end = start
        .saturating_add(count as u64)
        .min(schema.rows.unwrap_or(u64::MAX));
    let cells = end
        .saturating_sub(start)
        .saturating_mul(schema.columns.len().max(1) as u64);
    if cells > 1_000_000 {
        return Err("A table request is limited to 1,000,000 cells; request fewer rows".into());
    }
    Ok(end)
}
pub(super) fn check_output(bytes: usize) -> Result<(), String> {
    if bytes > Limits::default().batch_bytes {
        return Err(
            "A table request exceeds the 256 MiB display budget; request fewer rows".into(),
        );
    }
    Ok(())
}
#[cfg(test)]
mod tests;
