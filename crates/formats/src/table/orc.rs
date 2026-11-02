//! Apache ORC tables using the previous release's protobuf, compression and RLE decoders.
//! Metadata is read at open; only the stripes a row request touches are decoded.
use super::avro::{Limits, display::Text};
use super::{Schema, TableInfo, TableSource};
use std::{
    collections::HashMap,
    fmt::Write as _,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
mod rows;
use rows::decode_stripe;
pub const READY: bool = true;
const MAGIC: &[u8] = b"ORC";
/// Deepest type tree (lists of structs of maps, ...).
const MAX_DEPTH: usize = 64;
/// Types (every column, nested ones included) in the footer at most.
const MAX_TYPES: usize = 100_000;
/// Streams listed in one stripe footer at most.
const MAX_STREAMS: usize = 1_000_000;
/// Largest compression block a file may declare (writers use 64 KiB to 1 MiB).
const MAX_BLOCK: u64 = 16 * 1024 * 1024;
/// User metadata entries at most.
const MAX_META: usize = 10_000;
/// Values decoded per stripe read: this many, plus `ITEMS_PER_BYTE` per decompressed stream byte
/// (RLE runs expand at most about 520 times), so values that take no input, such as the fields of
/// an empty struct, cannot make a damaged count spin for ever.
const ITEMS_BASE: u64 = 10_000_000;
const ITEMS_PER_BYTE: u64 = 1024;
/// Root fields whose statistics are listed at most.
const MAX_STATISTICS: usize = 1000;
/// 2015-01-01 00:00:00 in seconds since 1970: ORC timestamps count from there.
const ORC_EPOCH: i64 = 1_420_070_400;

// Stream kinds (`Stream.Kind` in orc_proto.proto).
const PRESENT: u64 = 0;
const DATA: u64 = 1;
const LENGTH: u64 = 2;
const DICTIONARY_DATA: u64 = 3;
const SECONDARY: u64 = 5;

fn damaged(what: &str) -> String {
    format!("The ORC data is damaged: {what}")
}
fn zigzag(v: u64) -> i64 {
    (v >> 1) as i64 ^ -((v & 1) as i64)
}

// ----- a small protobuf reader for the metadata messages -----

#[derive(Clone, Copy)]
enum Wire<'a> {
    /// Varint, fixed32 and fixed64 values.
    Int(u64),
    Bytes(&'a [u8]),
}
impl<'a> Wire<'a> {
    fn int(self) -> u64 {
        match self {
            Wire::Int(v) => v,
            Wire::Bytes(_) => 0,
        }
    }
    fn bytes(self) -> &'a [u8] {
        match self {
            Wire::Bytes(b) => b,
            Wire::Int(_) => &[],
        }
    }
    fn text(self) -> String {
        String::from_utf8_lossy(self.bytes()).into_owned()
    }
    /// One or more values of a repeated integer field, packed or not; `count` is shared with the
    /// other repeated fields it bounds.
    fn ints(self, out: &mut Vec<u64>, count: &mut usize, cap: usize) -> Result<(), String> {
        let mut push = |v: u64| {
            *count += 1;
            if *count > cap {
                return Err(damaged("a metadata list is too long"));
            }
            out.push(v);
            Ok(())
        };
        match self {
            Wire::Int(v) => push(v),
            Wire::Bytes(b) => {
                let mut p = Proto { b, i: 0 };
                while p.i < b.len() {
                    push(p.varint()?)?;
                }
                Ok(())
            }
        }
    }
}

struct Proto<'a> {
    b: &'a [u8],
    i: usize,
}
impl<'a> Proto<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }
    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self
                .b
                .get(self.i)
                .ok_or_else(|| damaged("a metadata message ends early"))?;
            self.i += 1;
            if shift == 63 && byte > 1 {
                return Err(damaged("a metadata number is too large"));
            }
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(damaged("a metadata number is too long"))
    }
    fn take(&mut self, n: u64) -> Result<&'a [u8], String> {
        let end = usize::try_from(n)
            .ok()
            .and_then(|n| self.i.checked_add(n))
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| damaged("a metadata field runs past its message"))?;
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }
    /// The next field number and value, `None` at the end of the message.
    fn field(&mut self) -> Result<Option<(u64, Wire<'a>)>, String> {
        if self.i >= self.b.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        if key >> 3 == 0 {
            return Err(damaged("a metadata field has number zero"));
        }
        let value = match key & 7 {
            0 => Wire::Int(self.varint()?),
            1 => Wire::Int(u64::from_le_bytes(
                self.take(8)?.try_into().unwrap_or_default(),
            )),
            2 => {
                let n = self.varint()?;
                Wire::Bytes(self.take(n)?)
            }
            5 => Wire::Int(u64::from(u32::from_le_bytes(
                self.take(4)?.try_into().unwrap_or_default(),
            ))),
            _ => return Err(damaged("a metadata field has an unknown wire type")),
        };
        Ok(Some((key >> 3, value)))
    }
}

// ----- metadata messages -----

struct PostScript {
    footer: u64,
    compression: u64,
    block: u64,
    version: Vec<u64>,
    metadata: u64,
    writer_version: Option<u64>,
    magic: bool,
}
fn postscript(b: &[u8]) -> Result<PostScript, String> {
    let mut ps = PostScript {
        footer: 0,
        compression: 0,
        block: 256 * 1024,
        version: vec![],
        metadata: 0,
        writer_version: None,
        magic: false,
    };
    let mut p = Proto::new(b);
    let mut count = 0;
    while let Some((f, w)) = p.field()? {
        match f {
            1 => ps.footer = w.int(),
            2 => ps.compression = w.int(),
            3 => ps.block = w.int(),
            4 => w.ints(&mut ps.version, &mut count, 8)?,
            5 => ps.metadata = w.int(),
            6 => ps.writer_version = Some(w.int()),
            8000 => ps.magic = w.bytes() == MAGIC,
            _ => {}
        }
    }
    Ok(ps)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Boolean,
    Byte,
    Short,
    Int,
    Long,
    Float,
    Double,
    String,
    Binary,
    Timestamp,
    List,
    Map,
    Struct,
    Union,
    Decimal,
    Date,
    Varchar,
    Char,
    /// Timestamp with local time zone: an instant, stored in UTC.
    Instant,
}
impl Kind {
    fn from(v: u64) -> Option<Kind> {
        use Kind::*;
        [
            Boolean, Byte, Short, Int, Long, Float, Double, String, Binary, Timestamp, List, Map,
            Struct, Union, Decimal, Date, Varchar, Char, Instant,
        ]
        .get(usize::try_from(v).ok()?)
        .copied()
    }
    fn nested(self) -> bool {
        matches!(self, Kind::List | Kind::Map | Kind::Struct | Kind::Union)
    }
}

struct Type {
    kind: Kind,
    children: Vec<usize>,
    names: Vec<String>,
    length: Option<u64>,
    precision: Option<u64>,
    scale: u64,
}

struct StripeInfo {
    offset: u64,
    index: u64,
    data: u64,
    footer: u64,
    rows: u64,
}
impl StripeInfo {
    fn size(&self) -> u64 {
        self.index
            .saturating_add(self.data)
            .saturating_add(self.footer)
    }
}

struct Footer {
    stripes: Vec<StripeInfo>,
    types: Vec<Type>,
    metadata: Vec<(String, Vec<u8>)>,
    statistics: Vec<Vec<u8>>,
    stride: Option<u64>,
    writer: Option<u64>,
    software: Option<String>,
    encrypted: bool,
    calendar: Option<u64>,
}

fn stripe_info(b: &[u8]) -> Result<StripeInfo, String> {
    let mut s = StripeInfo {
        offset: 0,
        index: 0,
        data: 0,
        footer: 0,
        rows: 0,
    };
    let mut p = Proto::new(b);
    while let Some((f, w)) = p.field()? {
        match f {
            1 => s.offset = w.int(),
            2 => s.index = w.int(),
            3 => s.data = w.int(),
            4 => s.footer = w.int(),
            5 => s.rows = w.int(),
            _ => {}
        }
    }
    Ok(s)
}

/// One `Type`; `children` and `names` count every subtype and field name in the footer, which a
/// tree of `MAX_TYPES` types cannot exceed.
fn type_of(b: &[u8], children: &mut usize, names: &mut usize) -> Result<Type, String> {
    let mut kind = None;
    let mut subtypes = vec![];
    let mut t = Type {
        kind: Kind::Struct,
        children: vec![],
        names: vec![],
        length: None,
        precision: None,
        scale: 0,
    };
    let mut p = Proto::new(b);
    while let Some((f, w)) = p.field()? {
        match f {
            1 => {
                kind = Some(
                    Kind::from(w.int())
                        .ok_or_else(|| damaged(&format!("unknown column type {}", w.int())))?,
                )
            }
            2 => w.ints(&mut subtypes, children, MAX_TYPES)?,
            3 => {
                *names += 1;
                if *names > MAX_TYPES {
                    return Err(damaged("too many field names"));
                }
                t.names.push(w.text());
            }
            4 => t.length = Some(w.int()),
            5 => t.precision = Some(w.int()),
            6 => t.scale = w.int(),
            _ => {}
        }
    }
    t.kind = kind.ok_or_else(|| damaged("a column has no type"))?;
    t.children = subtypes
        .into_iter()
        .map(|s| usize::try_from(s).unwrap_or(usize::MAX))
        .collect();
    Ok(t)
}

fn footer(b: &[u8], limits: &Limits) -> Result<Footer, String> {
    let mut out = Footer {
        stripes: vec![],
        types: vec![],
        metadata: vec![],
        statistics: vec![],
        stride: None,
        writer: None,
        software: None,
        encrypted: false,
        calendar: None,
    };
    let (mut children, mut names) = (0, 0);
    let mut p = Proto::new(b);
    while let Some((f, w)) = p.field()? {
        match f {
            3 => {
                if out.stripes.len() >= limits.row_groups {
                    return Err(format!("More than {} stripes", limits.row_groups));
                }
                out.stripes.push(stripe_info(w.bytes())?);
            }
            4 => {
                if out.types.len() >= MAX_TYPES {
                    return Err(format!(
                        "More than {MAX_TYPES} columns (nested ones included)"
                    ));
                }
                out.types
                    .push(type_of(w.bytes(), &mut children, &mut names)?);
            }
            5 => {
                if out.metadata.len() >= MAX_META {
                    return Err(damaged("too many user metadata entries"));
                }
                let (mut name, mut value) = (String::new(), vec![]);
                let mut m = Proto::new(w.bytes());
                while let Some((f, w)) = m.field()? {
                    match f {
                        1 => name = w.text(),
                        2 => value = w.bytes().to_vec(),
                        _ => {}
                    }
                }
                out.metadata.push((name, value));
            }
            7 => {
                if out.statistics.len() < MAX_TYPES {
                    out.statistics.push(w.bytes().to_vec());
                }
            }
            8 => out.stride = Some(w.int()),
            9 => out.writer = Some(w.int()),
            10 => out.encrypted = true,
            11 => out.calendar = Some(w.int()),
            12 => out.software = Some(w.text()),
            _ => {}
        }
    }
    if out.types.is_empty() {
        return Err(damaged("the footer lists no columns"));
    }
    Ok(out)
}

/// Checks that the types form a tree below type 0 (children after their parent, each used once,
/// at most `MAX_DEPTH` deep) with the right number of children per kind.
fn check_tree(types: &[Type]) -> Result<(), String> {
    let mut seen = vec![false; types.len()];
    let mut stack = vec![(0usize, 0usize)];
    while let Some((i, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err(damaged("the types are nested too deeply"));
        }
        let t = types
            .get(i)
            .ok_or_else(|| damaged("a column type does not exist"))?;
        let n = t.children.len();
        let fits = match t.kind {
            Kind::List => n == 1,
            Kind::Map => n == 2,
            Kind::Union => (1..=256).contains(&n),
            Kind::Struct => true,
            _ => n == 0,
        };
        if !fits {
            return Err(damaged(&format!("a {:?} column has {n} children", t.kind)));
        }
        for &child in t.children.iter().rev() {
            let visited = seen
                .get_mut(child)
                .ok_or_else(|| damaged("the column types do not form a tree"))?;
            if child <= i || *visited {
                return Err(damaged("the column types do not form a tree"));
            }
            *visited = true;
            stack.push((child, depth + 1));
        }
    }
    Ok(())
}

/// Readable type name, as ORC tools print it (`bigint`, `array<int>`, `decimal(10,2)`).
fn describe(types: &[Type], i: usize) -> String {
    enum Part {
        Node(usize),
        Text(String),
    }
    let mut stack = vec![Part::Node(i)];
    let mut out = String::new();
    while let Some(part) = stack.pop() {
        let i = match part {
            Part::Text(text) => {
                out.push_str(&text);
                continue;
            }
            Part::Node(i) => i,
        };
        let Some(t) = types.get(i) else {
            out.push('?');
            continue;
        };
        if t.kind.nested() {
            out.push_str(match t.kind {
                Kind::List => "array<",
                Kind::Map => "map<",
                Kind::Union => "uniontype<",
                _ => "struct<",
            });
            stack.push(Part::Text(">".into()));
            for (k, &child) in t.children.iter().enumerate().rev() {
                stack.push(Part::Node(child));
                if t.kind == Kind::Struct {
                    stack.push(Part::Text(format!("{}:", field_name(t, k))));
                }
                if k > 0 {
                    stack.push(Part::Text(",".into()));
                }
            }
        } else {
            let scalar: String = match t.kind {
                Kind::Boolean => "boolean".into(),
                Kind::Byte => "tinyint".into(),
                Kind::Short => "smallint".into(),
                Kind::Int => "int".into(),
                Kind::Long => "bigint".into(),
                Kind::Float => "float".into(),
                Kind::Double => "double".into(),
                Kind::String => "string".into(),
                Kind::Binary => "binary".into(),
                Kind::Timestamp => "timestamp".into(),
                Kind::Instant => "timestamp with local time zone".into(),
                Kind::Date => "date".into(),
                Kind::Varchar => format!("varchar({})", t.length.unwrap_or(0)),
                Kind::Char => format!("char({})", t.length.unwrap_or(0)),
                Kind::Decimal => match t.precision {
                    Some(p) if p > 0 => format!("decimal({p},{})", t.scale),
                    _ => "decimal".into(),
                },
                _ => "?".into(),
            };
            out.push_str(&scalar);
        }
    }
    out
}
fn field_name(t: &Type, k: usize) -> String {
    t.names
        .get(k)
        .cloned()
        .unwrap_or_else(|| format!("_col{k}"))
}

/// The type tree as indented text, one line per column with its id.
fn schema_text(types: &[Type], i: usize, name: &str, depth: usize, out: &mut String) {
    let mut stack = vec![(i, name.to_owned(), depth)];
    while let Some((i, name, depth)) = stack.pop() {
        let Some(t) = types.get(i) else {
            continue;
        };
        out.extend(std::iter::repeat_n("  ", depth));
        let ty = if t.kind.nested() {
            match t.kind {
                Kind::List => "array",
                Kind::Map => "map",
                Kind::Union => "uniontype",
                _ => "struct",
            }
            .to_owned()
        } else {
            describe(types, i)
        };
        let _ = writeln!(out, "{name}: {ty}  (column {i})");
        for (k, &c) in t.children.iter().enumerate().rev() {
            let child = match t.kind {
                Kind::List => "element".into(),
                Kind::Map if k == 0 => "key".into(),
                Kind::Map => "value".into(),
                Kind::Union => format!("tag {k}"),
                _ => field_name(t, k),
            };
            stack.push((c, child, depth + 1));
        }
    }
}

/// "400 values, has nulls, min 1, max 399" from a `ColumnStatistics` message.
fn statistics(b: &[u8]) -> Option<String> {
    let mut parts = vec![];
    let (mut values, mut nulls) = (None, None);
    let mut range = None;
    let mut p = Proto::new(b);
    while let Some((f, w)) = p.field().ok()? {
        match f {
            1 => values = Some(w.int()),
            10 => nulls = Some(w.int() != 0),
            2 | 3 | 4 | 6 | 7 => {
                let (mut min, mut max) = (None, None);
                let mut m = Proto::new(w.bytes());
                while let Some((g, v)) = m.field().ok()? {
                    let text = match (f, v) {
                        (2, Wire::Int(v)) => zigzag(v).to_string(),
                        (3, Wire::Int(v)) => f64::from_bits(v).to_string(),
                        (7, Wire::Int(v)) => date_text(zigzag(v)),
                        (4, Wire::Bytes(s)) => {
                            let s = String::from_utf8_lossy(s);
                            let mut q = Text::default();
                            super::avro::display::quote(
                                &s.chars().take(64).collect::<String>(),
                                &mut q,
                            );
                            q.finish()
                        }
                        (6, Wire::Bytes(s)) => {
                            String::from_utf8_lossy(s).chars().take(64).collect()
                        }
                        _ => continue,
                    };
                    match g {
                        1 => min = Some(text),
                        2 => max = Some(text),
                        _ => {}
                    }
                }
                if let (Some(min), Some(max)) = (min, max) {
                    range = Some(format!("min {min}, max {max}"));
                }
            }
            _ => {}
        }
    }
    parts.push(format!("{} values", values?));
    match nulls {
        Some(true) => parts.push("has nulls".into()),
        Some(false) => parts.push("no nulls".into()),
        None => {}
    }
    parts.extend(range);
    Some(parts.join(", "))
}

struct StripeFooter {
    /// Kind, column and length of each stream, in file order.
    streams: Vec<(u64, usize, u64)>,
    /// Encoding kind and dictionary size per column.
    encodings: Vec<(u64, u64)>,
    timezone: String,
}
fn stripe_footer(b: &[u8]) -> Result<StripeFooter, String> {
    let mut out = StripeFooter {
        streams: vec![],
        encodings: vec![],
        timezone: String::new(),
    };
    let mut p = Proto::new(b);
    while let Some((f, w)) = p.field()? {
        match f {
            1 => {
                if out.streams.len() >= MAX_STREAMS {
                    return Err(damaged("a stripe lists too many streams"));
                }
                let (mut kind, mut column, mut length) = (0, 0, 0);
                let mut m = Proto::new(w.bytes());
                while let Some((f, w)) = m.field()? {
                    match f {
                        1 => kind = w.int(),
                        2 => column = w.int(),
                        3 => length = w.int(),
                        _ => {}
                    }
                }
                let column = usize::try_from(column).unwrap_or(usize::MAX);
                out.streams.push((kind, column, length));
            }
            2 => {
                if out.encodings.len() >= MAX_TYPES {
                    return Err(damaged("a stripe lists too many column encodings"));
                }
                let (mut kind, mut size) = (0, 0);
                let mut m = Proto::new(w.bytes());
                while let Some((f, w)) = m.field()? {
                    match f {
                        1 => kind = w.int(),
                        2 => size = w.int(),
                        _ => {}
                    }
                }
                out.encodings.push((kind, size));
            }
            3 => out.timezone = w.text(),
            _ => {}
        }
    }
    Ok(out)
}

// ----- compression -----

#[derive(Clone, Copy, Debug, PartialEq)]
enum Codec {
    None,
    Zlib,
    Snappy,
    Zstd,
    Lz4,
}

impl Codec {
    fn name(self) -> &'static str {
        match self {
            Codec::None => "NONE",
            Codec::Zlib => "ZLIB",
            Codec::Snappy => "SNAPPY",
            Codec::Zstd => "ZSTD",
            Codec::Lz4 => "LZ4",
        }
    }
}

/// Decompress a stream or metadata section: chunks with 3-byte little-endian headers
/// `(length << 1) | original`, each at most `block` bytes once decompressed, `cap` in all.
fn decompress(codec: Codec, block: usize, raw: &[u8], cap: u64) -> Result<Vec<u8>, String> {
    let too_large = || format!("Decoded data exceeds the {} MiB table budget", cap >> 20);
    if codec == Codec::None {
        if raw.len() as u64 > cap {
            return Err(too_large());
        }
        return Ok(raw.to_vec());
    }
    let mut out = vec![];
    let mut i = 0usize;
    let mut scratch = vec![];
    while i < raw.len() {
        let h = raw
            .get(i..i + 3)
            .ok_or_else(|| damaged("a compression chunk header is cut short"))?;
        let h = u32::from(h[0]) | u32::from(h[1]) << 8 | u32::from(h[2]) << 16;
        let len = (h >> 1) as usize;
        i += 3;
        let chunk = raw
            .get(i..i.saturating_add(len))
            .ok_or_else(|| damaged("a compression chunk runs past its stream"))?;
        i += len;
        let piece: &[u8] = if h & 1 == 1 {
            chunk
        } else {
            decompress_chunk(codec, block, chunk, &mut scratch)?;
            &scratch
        };
        if (out.len() + piece.len()) as u64 > cap {
            return Err(too_large());
        }
        out.extend_from_slice(piece);
    }
    Ok(out)
}

fn decompress_chunk(
    codec: Codec,
    block: usize,
    chunk: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), String> {
    let bad = |e: &dyn std::fmt::Display| damaged(&format!("a chunk does not decompress ({e})"));
    let too_large = || damaged("a chunk decompresses to more than its block size");
    out.clear();
    let capped = |r: &mut dyn Read, out: &mut Vec<u8>| -> Result<(), String> {
        r.take(block as u64 + 1)
            .read_to_end(out)
            .map_err(|e| bad(&e))?;
        if out.len() > block {
            return Err(too_large());
        }
        Ok(())
    };
    match codec {
        Codec::None => out.extend_from_slice(chunk),
        Codec::Zlib => capped(&mut flate2::read::DeflateDecoder::new(chunk), out)?,
        Codec::Zstd => {
            let mut d = zstd::stream::read::Decoder::new(chunk).map_err(|e| bad(&e))?;
            // A frame's window sets the decoder's memory: no ORC chunk needs more than 16 MiB.
            d.window_log_max(24).map_err(|e| bad(&e))?;
            capped(&mut d, out)?
        }
        Codec::Snappy => {
            let n = snap::raw::decompress_len(chunk).map_err(|e| bad(&e))?;
            if n > block {
                return Err(too_large());
            }
            out.resize(n, 0);
            let n = snap::raw::Decoder::new()
                .decompress(chunk, out)
                .map_err(|e| bad(&e))?;
            out.truncate(n);
        }
        Codec::Lz4 => {
            out.resize(block, 0);
            let n = lz4_flex::block::decompress_into(chunk, out).map_err(|e| bad(&e))?;
            out.truncate(n);
        }
    }
    Ok(())
}

// ----- the file tail -----

struct Tail {
    codec: Codec,
    block: usize,
    version: String,
    writer_version: Option<u64>,
    footer: Footer,
    /// Root columns: name and type index.
    roots: Vec<(String, usize)>,
    stripe_statistics: Option<usize>,
    warning: Option<String>,
}

fn read_at(file: &mut File, offset: u64, len: u64) -> Result<Vec<u8>, String> {
    let file_len = file.metadata().map_err(|e| e.to_string())?.len();
    if len > Limits::default().batch_bytes as u64
        || offset.checked_add(len).is_none_or(|end| end > file_len)
    {
        return Err(damaged("a read runs past the file or the 256 MiB limit"));
    }
    let mut buf = vec![0; usize::try_from(len).map_err(|e| e.to_string())?];
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_exact(&mut buf))
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

fn tail(file: &mut File, limits: &Limits) -> Result<Tail, String> {
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut head = [0u8; 3];
    if file.read_exact(&mut head).is_err() || head != MAGIC {
        return Err("Not an ORC file".into());
    }
    let ps_len = u64::from(
        *read_at(file, len.saturating_sub(1), 1)?
            .first()
            .ok_or("Not an ORC file: too short")?,
    );
    if ps_len == 0 || ps_len + 1 + 3 > len {
        return Err(damaged("the postscript length is invalid"));
    }
    let ps = postscript(&read_at(file, len - 1 - ps_len, ps_len)?)
        .map_err(|e| format!("{e} (postscript)"))?;
    if !ps.magic {
        return Err(damaged(
            "the postscript has no ORC signature (the file is truncated or not ORC)",
        ));
    }
    let codec = match ps.compression {
        0 => Codec::None,
        1 => Codec::Zlib,
        2 => Codec::Snappy,
        4 => Codec::Lz4,
        5 => Codec::Zstd,
        3 => {
            return Err(
                "LZO-compressed ORC files cannot be read (supported: none, zlib, snappy, zstd, lz4)"
                    .into(),
            );
        }
        other => return Err(format!("Unknown ORC compression kind {other}")),
    };
    if codec != Codec::None && !(1..=MAX_BLOCK).contains(&ps.block) {
        return Err(format!(
            "An ORC compression block size of {} bytes is not supported (at most {} MiB)",
            ps.block,
            MAX_BLOCK >> 20
        ));
    }
    let block = ps.block.min(MAX_BLOCK) as usize;
    let tail_len = ps
        .footer
        .checked_add(ps.metadata)
        .ok_or_else(|| damaged("the footer length is invalid"))?;
    if tail_len > limits.footer_bytes {
        return Err(format!(
            "ORC metadata limit exceeded: footer and metadata are {} MiB (limit {} MiB)",
            tail_len >> 20,
            limits.footer_bytes >> 20
        ));
    }
    let content_end = len - 1 - ps_len;
    if tail_len > content_end - 3 {
        return Err(damaged("the footer runs past the start of the file"));
    }
    let data_end = content_end - tail_len;
    let raw = read_at(file, data_end, tail_len)?;
    let (meta_raw, footer_raw) = raw.split_at(ps.metadata as usize);
    let footer_bytes = decompress(codec, block, footer_raw, limits.footer_bytes)
        .map_err(|e| format!("{e} (footer)"))?;
    let mut footer = footer(&footer_bytes, limits)?;
    check_tree(&footer.types)?;
    let roots: Vec<(String, usize)> = match footer.types.first() {
        Some(t) if t.kind == Kind::Struct => t
            .children
            .iter()
            .enumerate()
            .map(|(k, &c)| (field_name(t, k), c))
            .collect(),
        _ => vec![("value".into(), 0)],
    };
    if roots.len() > limits.columns {
        return Err(format!("More than {} columns", limits.columns));
    }
    let mut warning = None;
    if let Some(bad) = footer
        .stripes
        .iter()
        .position(|s| s.offset < 3 || s.offset.saturating_add(s.size()) > data_end)
    {
        warning = Some(format!(
            "Stripe {} lies outside the file (after {bad} stripes; the rest is not shown)",
            bad + 1
        ));
        footer.stripes.truncate(bad);
    }
    let stripe_statistics = if ps.metadata == 0 {
        None
    } else {
        let counted = decompress(codec, block, meta_raw, limits.footer_bytes).and_then(|m| {
            let mut p = Proto::new(&m);
            let mut n = 0;
            while let Some((f, _)) = p.field()? {
                n += usize::from(f == 1);
            }
            Ok(n)
        });
        match counted {
            Ok(n) => Some(n),
            Err(e) => {
                warning.get_or_insert(format!("The stripe statistics are not read: {e}"));
                None
            }
        }
    };
    Ok(Tail {
        codec,
        block,
        version: ps
            .version
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("."),
        writer_version: ps.writer_version,
        footer,
        roots,
        stripe_statistics,
        warning,
    })
}

fn read_stripe_footer(
    file: &mut File,
    tail: &Tail,
    stripe: &StripeInfo,
    limits: &Limits,
) -> Result<StripeFooter, String> {
    if stripe.footer > limits.footer_bytes {
        return Err(damaged("a stripe footer is larger than the metadata limit"));
    }
    let at = stripe
        .offset
        .checked_add(stripe.index)
        .and_then(|n| n.checked_add(stripe.data))
        .ok_or_else(|| damaged("a stripe offset is too large"))?;
    let raw = read_at(file, at, stripe.footer)?;
    stripe_footer(&decompress(
        tail.codec,
        tail.block,
        &raw,
        limits.footer_bytes,
    )?)
}

fn metadata(file: &mut File, tail: &Tail, limits: &Limits) -> Result<Schema, String> {
    let footer = &tail.footer;
    let types = &footer.types;
    let mut key_value = vec![];
    let codec = match tail.codec {
        Codec::None => "none".to_owned(),
        c => format!("{}, {} KiB blocks", c.name(), tail.block >> 10),
    };
    let writer = match footer.writer {
        Some(0) => "ORC Java".to_owned(),
        Some(1) => "ORC C++".into(),
        Some(2) => "Presto".into(),
        Some(3) => "Scritchley Go".into(),
        Some(4) => "Trino".into(),
        Some(5) => "CUDF".into(),
        Some(n) => format!("writer {n}"),
        None => "unknown writer".into(),
    };
    let writer = match &footer.software {
        Some(v) if !v.is_empty() => format!("{writer} {v}"),
        _ => writer,
    };
    key_value.push(("ORC version".into(), tail.version.clone()));
    key_value.push(("Written by".into(), writer.clone()));
    if let Some(v) = tail.writer_version {
        key_value.push(("Writer version".into(), v.to_string()));
    }
    key_value.push(("Compression".into(), codec));
    key_value.push(("Stripes".into(), footer.stripes.len().to_string()));
    if let Some(n) = tail.stripe_statistics {
        key_value.push(("Stripe statistics".into(), n.to_string()));
    }
    if let Some(stride) = footer.stride {
        key_value.push(("Row index stride".into(), stride.to_string()));
    }
    match footer.calendar {
        Some(1) => key_value.push(("Calendar".into(), "Julian/Gregorian".into())),
        Some(2) => key_value.push(("Calendar".into(), "proleptic Gregorian".into())),
        _ => {}
    }
    if footer.encrypted {
        key_value.push((
            "Encryption".into(),
            "some columns are encrypted; they cannot be shown".into(),
        ));
    }
    for (name, value) in &footer.metadata {
        let value = String::from_utf8_lossy(value);
        let shown: String = value.chars().take(4096).collect();
        key_value.push((
            name.clone(),
            if shown.len() < value.len() {
                format!("{shown}… [{} bytes]", value.len())
            } else {
                shown
            },
        ));
    }
    if types.iter().any(|t| t.kind == Kind::Timestamp)
        && let Some(first) = footer.stripes.first()
    {
        match read_stripe_footer(file, tail, first, limits) {
            Ok(sf) => {
                let zone = if sf.timezone.is_empty() {
                    "not recorded".to_owned()
                } else {
                    sf.timezone.clone()
                };
                key_value.push(("Writer time zone".into(), zone));
                let utc = [
                    "", "UTC", "GMT", "Etc/UTC", "Etc/GMT", "UCT", "Etc/UCT", "Z",
                ];
                if !utc.contains(&sf.timezone.as_str()) {
                    key_value.push((
                        "Timestamps".into(),
                        format!(
                            "shown as written in {}; a daylight-saving difference from 2015-01-01 is not applied",
                            sf.timezone
                        ),
                    ));
                }
            }
            Err(e) => key_value.insert(0, ("Warning".into(), format!("Stripe 1: {e}"))),
        }
    }
    for (name, col) in tail.roots.iter().take(MAX_STATISTICS) {
        if let Some(s) = footer.statistics.get(*col).and_then(|b| statistics(b)) {
            key_value.push((format!("Statistics of {name}"), s));
        }
    }
    if let Some(w) = &tail.warning {
        key_value.insert(0, ("Warning".into(), w.clone()));
    }
    let mut schema = String::new();
    match types.first() {
        Some(t) if t.kind == Kind::Struct => {
            for (k, &c) in t.children.iter().enumerate() {
                schema_text(types, c, &field_name(t, k), 0, &mut schema);
            }
        }
        _ => schema_text(types, 0, "value", 0, &mut schema),
    }
    let rows = tail
        .footer
        .stripes
        .iter()
        .try_fold(0u64, |n, s| n.checked_add(s.rows))
        .ok_or_else(|| damaged("the row count is too large"))?;
    key_value.push(("Rows".into(), rows.to_string()));
    Ok(Schema {
        tables: vec![TableInfo {
            name: "rows".into(),
            rows: Some(rows),
        }],
        table: 0,
        columns: tail
            .roots
            .iter()
            .map(|(name, c)| super::Column {
                name: name.clone(),
                type_name: describe(types, *c),
            })
            .collect(),
        rows: Some(rows),
        details: key_value,
        text: Some(schema),
    })
}
// ----- run-length decoding -----

/// A decompressed stream and a read position.
struct Input {
    b: Vec<u8>,
    i: usize,
}
fn ended() -> String {
    damaged("a stream ends before its values")
}
impl Input {
    fn new(b: Vec<u8>) -> Self {
        Self { b, i: 0 }
    }
    fn byte(&mut self) -> Result<u8, String> {
        let v = *self.b.get(self.i).ok_or_else(ended)?;
        self.i += 1;
        Ok(v)
    }
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self
            .i
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(ended)?;
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }
    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            if shift == 63 && byte > 1 {
                return Err(damaged("a number is too large"));
            }
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(damaged("a number is too long"))
    }
    /// `n` big-endian bytes (n ≤ 8).
    fn be(&mut self, n: usize) -> Result<u64, String> {
        Ok(self
            .take(n)?
            .iter()
            .fold(0u64, |v, &b| v << 8 | u64::from(b)))
    }
    /// `n` values of `width` bits, most significant bit first, from the next whole bytes.
    fn unpack(&mut self, n: usize, width: u32, out: &mut Vec<u64>) -> Result<(), String> {
        let bytes = n.checked_mul(width as usize).ok_or_else(ended)?.div_ceil(8);
        let data = self.take(bytes)?;
        let mask = mask(width);
        let (mut acc, mut bits) = (0u128, 0u32);
        let mut next = data.iter();
        for _ in 0..n {
            while bits < width {
                acc = acc << 8 | u128::from(*next.next().ok_or_else(ended)?);
                bits += 8;
            }
            bits -= width;
            out.push((acc >> bits) as u64 & mask);
            acc &= (1u128 << bits) - 1;
        }
        Ok(())
    }
}
fn mask(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}
/// Bit width from the 5-bit code of RLE v2 headers.
fn width(code: u8) -> u32 {
    match code {
        0..=23 => u32::from(code) + 1,
        24 => 26,
        25 => 28,
        26 => 30,
        27 => 32,
        28 => 40,
        29 => 48,
        30 => 56,
        _ => 64,
    }
}
/// The smallest width RLE v2 packs `bits` in.
fn closest_width(bits: u32) -> u32 {
    match bits {
        0..=24 => bits.max(1),
        25..=26 => 26,
        27..=28 => 28,
        29..=30 => 30,
        31..=32 => 32,
        33..=40 => 40,
        41..=48 => 48,
        49..=56 => 56,
        _ => 64,
    }
}

/// Byte run-length encoding: a control byte, then a repeated byte (3-130 times) or 1-128 literals.
struct ByteRle {
    input: Input,
    value: u8,
    repeat: bool,
    left: usize,
}
impl ByteRle {
    fn new(b: Vec<u8>) -> Self {
        Self {
            input: Input::new(b),
            value: 0,
            repeat: false,
            left: 0,
        }
    }
    fn next(&mut self) -> Result<u8, String> {
        if self.left == 0 {
            let h = self.input.byte()? as i8;
            if h >= 0 {
                self.left = h as usize + 3;
                self.repeat = true;
                self.value = self.input.byte()?;
            } else {
                self.left = usize::from(h.unsigned_abs());
                self.repeat = false;
            }
        }
        self.left -= 1;
        if self.repeat {
            Ok(self.value)
        } else {
            self.input.byte()
        }
    }
}

/// Booleans: byte RLE of bytes holding eight values each, most significant bit first.
struct BoolRle {
    bytes: ByteRle,
    current: u8,
    bits: u32,
}
impl BoolRle {
    fn new(b: Vec<u8>) -> Self {
        Self {
            bytes: ByteRle::new(b),
            current: 0,
            bits: 0,
        }
    }
    fn next(&mut self) -> Result<bool, String> {
        if self.bits == 0 {
            self.current = self.bytes.next()?;
            self.bits = 8;
        }
        self.bits -= 1;
        Ok(self.current >> self.bits & 1 == 1)
    }
}

/// Integer RLE, version 1 (DIRECT, DICTIONARY) or 2 (DIRECT_V2, DICTIONARY_V2). One run (at most
/// 512 values) is decoded at a time; each run consumes input, so damage ends in an error.
struct IntRle {
    input: Input,
    signed: bool,
    v2: bool,
    run: Vec<i64>,
    at: usize,
    scratch: Vec<u64>,
}
impl IntRle {
    fn new(b: Vec<u8>, signed: bool, v2: bool) -> Self {
        Self {
            input: Input::new(b),
            signed,
            v2,
            run: Vec::with_capacity(512),
            at: 0,
            scratch: vec![],
        }
    }
    fn next(&mut self) -> Result<i64, String> {
        if self.at >= self.run.len() {
            self.run.clear();
            self.at = 0;
            if self.v2 {
                self.run_v2()?;
            } else {
                self.run_v1()?;
            }
        }
        let v = *self.run.get(self.at).ok_or_else(ended)?;
        self.at += 1;
        Ok(v)
    }
    /// The next value as a non-negative count or index.
    fn index(&mut self) -> Result<usize, String> {
        usize::try_from(self.next()? as u64).map_err(|_| damaged("a length is too large"))
    }
    fn number(&mut self) -> Result<i64, String> {
        let v = self.input.varint()?;
        Ok(if self.signed { zigzag(v) } else { v as i64 })
    }
    fn run_v1(&mut self) -> Result<(), String> {
        let h = self.input.byte()? as i8;
        if h >= 0 {
            let delta = i64::from(self.input.byte()? as i8);
            let base = self.number()?;
            for k in 0..i64::from(h) + 3 {
                self.run.push(base.wrapping_add(delta.wrapping_mul(k)));
            }
        } else {
            for _ in 0..h.unsigned_abs() {
                let v = self.number()?;
                self.run.push(v);
            }
        }
        Ok(())
    }
    fn run_v2(&mut self) -> Result<(), String> {
        let h = self.input.byte()?;
        let decode = |v: u64, signed: bool| if signed { zigzag(v) } else { v as i64 };
        match h >> 6 {
            // SHORT_REPEAT: 1-8 byte value repeated 3-10 times.
            0 => {
                let bytes = usize::from(h >> 3 & 7) + 1;
                let v = decode(self.input.be(bytes)?, self.signed);
                for _ in 0..(h & 7) + 3 {
                    self.run.push(v);
                }
            }
            // DIRECT: 1-512 bit-packed values.
            1 => {
                let w = width(h >> 1 & 0x1f);
                let n = (usize::from(h & 1) << 8 | usize::from(self.input.byte()?)) + 1;
                self.scratch.clear();
                self.input.unpack(n, w, &mut self.scratch)?;
                let signed = self.signed;
                self.run
                    .extend(self.scratch.iter().map(|&v| decode(v, signed)));
            }
            // PATCHED_BASE: values above a base, with the widest values patched in from a list.
            2 => {
                let w = width(h >> 1 & 0x1f);
                let n = (usize::from(h & 1) << 8 | usize::from(self.input.byte()?)) + 1;
                let third = self.input.byte()?;
                let fourth = self.input.byte()?;
                let base_bytes = usize::from(third >> 5) + 1;
                let patch_width = width(third & 0x1f);
                let gap_width = u32::from(fourth >> 5) + 1;
                let patches = usize::from(fourth & 0x1f);
                if gap_width + patch_width > 64 || w + patch_width > 64 {
                    return Err(damaged("a patched run is wider than 64 bits"));
                }
                let raw = self.input.be(base_bytes)?;
                let sign = 1u64 << (base_bytes * 8 - 1);
                let base = if raw & sign != 0 {
                    -((raw & !sign) as i64)
                } else {
                    raw as i64
                };
                self.scratch.clear();
                self.input.unpack(n, w, &mut self.scratch)?;
                let mut list = Vec::with_capacity(patches);
                self.input
                    .unpack(patches, closest_width(gap_width + patch_width), &mut list)?;
                let (mut pos, mut k) = (0usize, 0usize);
                while k < list.len() {
                    let mut gap = 0usize;
                    let mut patch = 0;
                    while k < list.len() {
                        let e = list[k];
                        gap += (e >> patch_width) as usize;
                        patch = e & mask(patch_width);
                        // A gap of 255 with no patch only moves on to the next entry.
                        if e >> patch_width == 255 && patch == 0 {
                            k += 1;
                        } else {
                            break;
                        }
                    }
                    if k >= list.len() {
                        break;
                    }
                    pos += gap;
                    let v = self
                        .scratch
                        .get_mut(pos)
                        .ok_or_else(|| damaged("a patch lies outside its run"))?;
                    *v |= patch << w;
                    k += 1;
                }
                self.run
                    .extend(self.scratch.iter().map(|&v| base.wrapping_add(v as i64)));
            }
            // DELTA: a base, a first delta, then fixed or bit-packed deltas.
            _ => {
                let code = h >> 1 & 0x1f;
                let n = (usize::from(h & 1) << 8 | usize::from(self.input.byte()?)) + 1;
                let base = self.number()?;
                let delta = zigzag(self.input.varint()?);
                self.run.push(base);
                let mut prev = base;
                if code == 0 {
                    for _ in 1..n {
                        prev = prev.wrapping_add(delta);
                        self.run.push(prev);
                    }
                } else if n >= 2 {
                    prev = prev.wrapping_add(delta);
                    self.run.push(prev);
                    self.scratch.clear();
                    self.input.unpack(n - 2, width(code), &mut self.scratch)?;
                    for &d in &self.scratch {
                        prev = if delta < 0 {
                            prev.wrapping_sub(d as i64)
                        } else {
                            prev.wrapping_add(d as i64)
                        };
                        self.run.push(prev);
                    }
                }
            }
        }
        Ok(())
    }
}

// ----- values as text -----

/// `s` as a JSON string literal.
fn civil(days: i64) -> (i64, i64, i64) {
    // Civil-from-days (H. Hinnant); callers keep |days| within a billion.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}
fn date_text(days: i64) -> String {
    if days.unsigned_abs() > 1_000_000_000 {
        return format!("{days} days");
    }
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}")
}
/// Seconds and nanoseconds since 1970 as `YYYY-MM-DD HH:MM:SS` with as many fraction digits (3,
/// 6 or 9) as the value needs.
fn time_text(secs: i64, nanos: u64, out: &mut Text) {
    let days = secs.div_euclid(86_400);
    if days.unsigned_abs() > 1_000_000_000 {
        let _ = write!(out, "{secs} s");
        return;
    }
    let rem = secs.rem_euclid(86_400);
    let _ = write!(
        out,
        "{} {:02}:{:02}:{:02}",
        date_text(days),
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    );
    if nanos != 0 {
        let digits = if nanos.is_multiple_of(1_000_000) {
            3
        } else if nanos.is_multiple_of(1000) {
            6
        } else {
            9
        };
        let _ = write!(out, ".{:0digits$}", nanos / 10u64.pow(9 - digits as u32));
    }
}
/// An ORC timestamp: seconds from 2015-01-01 and nanoseconds with their trailing zeros counted in
/// the low three bits.
fn timestamp(secs: i64, raw: u64, instant: bool, out: &mut Text) -> Result<(), String> {
    let zeros = (raw & 7) as u32;
    // Read as signed, like the reference reader: the C++ writer stores the negative remainder of
    // a truncating division for instants before 1970 (Arrow's timestamps, for one).
    let mut nanos = (raw as i64) >> 3;
    if zeros != 0 {
        nanos = nanos.checked_mul(10i64.pow(zeros + 1)).unwrap_or(i64::MAX);
    }
    if nanos.unsigned_abs() > 999_999_999 {
        return Err(damaged("a timestamp's nanoseconds are out of range"));
    }
    let mut secs = secs
        .checked_add(ORC_EPOCH)
        .ok_or_else(|| damaged("a timestamp is out of range"))?;
    // Writers round seconds before 1970 towards zero when there is a fraction of a millisecond
    // or more; readers undo it the same way.
    if secs < 0 && nanos > 999_999 {
        secs -= 1;
    }
    if nanos < 0 {
        secs -= 1;
        nanos += 1_000_000_000;
    }
    time_text(secs, nanos as u64, out);
    if instant {
        out.push_str(" UTC");
    }
    Ok(())
}
/// An unbounded base-128 varint (zig-zag), as ORC stores decimals.
fn decimal_varint(input: &mut Input) -> Result<i128, String> {
    let mut v = 0u128;
    for shift in (0..128).step_by(7) {
        let byte = input.byte()?;
        if shift == 126 && byte > 3 {
            return Err(damaged("a decimal is too large"));
        }
        v |= u128::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok((v >> 1) as i128 ^ -((v & 1) as i128));
        }
    }
    Err(damaged("a decimal is too long"))
}
/// `v` with `scale` decimal places, shown with at least the type's scale.
fn decimal(v: i128, scale: i64, type_scale: u64, out: &mut Text) -> Result<(), String> {
    if !(-38..=38).contains(&scale) {
        return Err(damaged("a decimal scale is out of range"));
    }
    let (mut v, mut scale) = (v, scale);
    if scale < 0 {
        match v.checked_mul(10i128.pow(scale.unsigned_abs() as u32)) {
            Some(x) => (v, scale) = (x, 0),
            None => {
                let _ = write!(out, "{v}E{}", -scale);
                return Ok(());
            }
        }
    }
    while (scale as u64) < type_scale.min(38) {
        match v.checked_mul(10) {
            Some(x) => (v, scale) = (x, scale + 1),
            None => break,
        }
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
    Ok(())
}

// ----- columns -----

/// Values left to decode and text bytes produced in one stripe read.
struct Budget {
    items: u64,
    text: usize,
    cap: usize,
}
impl Budget {
    fn item(&mut self) -> Result<(), String> {
        self.items = self
            .items
            .checked_sub(1)
            .ok_or_else(|| damaged("too many values in one stripe"))?;
        Ok(())
    }
    fn fits(&self, extra: usize) -> Result<(), String> {
        if self.text.saturating_add(extra) > self.cap {
            return Err(format!(
                "Decoded data exceeds the {} MiB table budget",
                self.cap >> 20
            ));
        }
        Ok(())
    }
    fn text(&mut self, n: usize) -> Result<(), String> {
        self.fits(n)?;
        self.text += n;
        Ok(())
    }
}

enum Body {
    Bool(BoolRle),
    Byte(ByteRle),
    /// smallint, int, bigint.
    Int(IntRle),
    Float(Input),
    Double(Input),
    /// string, varchar, char and binary stored directly: lengths, then the bytes.
    Direct {
        data: Input,
        lengths: IntRle,
        binary: bool,
    },
    /// Dictionary-encoded strings: entry ends in `dict`, then an entry index per value.
    Dict {
        dict: Vec<u8>,
        ends: Vec<usize>,
        indices: IntRle,
    },
    Decimal {
        data: Input,
        scales: IntRle,
        scale: u64,
    },
    Date(IntRle),
    Timestamp {
        secs: IntRle,
        nanos: IntRle,
        instant: bool,
    },
    List {
        lengths: IntRle,
        child: usize,
    },
    Map {
        lengths: IntRle,
        keys: usize,
        values: usize,
    },
    Struct {
        names: Vec<String>,
        fields: Vec<usize>,
    },
    Union {
        tags: ByteRle,
        branches: Vec<usize>,
    },
}
impl Body {
    /// The next string or binary value.
    fn bytes(&mut self) -> Result<&[u8], String> {
        match self {
            Body::Direct { data, lengths, .. } => {
                let n = lengths.index()?;
                data.take(n)
            }
            Body::Dict {
                dict,
                ends,
                indices,
            } => {
                let i = indices.index()?;
                let end = *ends
                    .get(i)
                    .ok_or_else(|| damaged("a dictionary entry does not exist"))?;
                let start = match i.checked_sub(1) {
                    Some(p) => ends.get(p).copied().unwrap_or(0),
                    None => 0,
                };
                dict.get(start..end)
                    .ok_or_else(|| damaged("a dictionary entry runs past its stream"))
            }
            _ => Err("internal column mismatch".into()),
        }
    }
}

/// One column of a stripe: its null flags and the decoder for its type.
struct Reader {
    kind: Kind,
    present: Option<BoolRle>,
    body: Body,
}

/// The streams of one stripe that its columns use, read and decompressed on demand.
struct Streams<'f> {
    file: &'f mut File,
    table: HashMap<(usize, u64), (u64, u64)>,
    codec: Codec,
    block: usize,
    /// Decompressed bytes still allowed, and read so far.
    budget: u64,
    read: u64,
}
impl Streams<'_> {
    /// A stream's decompressed bytes; a stream the writer left out (it had no values) is empty.
    fn get(&mut self, column: usize, kind: u64) -> Result<Vec<u8>, String> {
        let Some(&(offset, len)) = self.table.get(&(column, kind)) else {
            return Ok(vec![]);
        };
        if len > self.budget {
            return Err(format!(
                "Decoded data exceeds the {} MiB table budget",
                self.budget >> 20
            ));
        }
        let raw = read_at(self.file, offset, len)?;
        let data = decompress(self.codec, self.block, &raw, self.budget)?;
        self.budget = self.budget.saturating_sub(data.len() as u64);
        self.read = self.read.saturating_add(data.len() as u64);
        Ok(data)
    }
}

impl Reader {
    fn new(
        s: &mut Streams,
        types: &[Type],
        encodings: &[(u64, u64)],
        col: usize,
    ) -> Result<Reader, String> {
        let t = types
            .get(col)
            .ok_or_else(|| damaged("a column type does not exist"))?;
        let &(encoding, dict_size) = encodings
            .get(col)
            .ok_or_else(|| damaged("a column has no encoding in its stripe"))?;
        let v2 = match encoding {
            0 | 1 => false,
            2 | 3 => true,
            other => return Err(format!("ORC column encoding {other} is not supported")),
        };
        let present = s.get(col, PRESENT)?;
        let present = (!present.is_empty()).then(|| BoolRle::new(present));
        let body = match t.kind {
            Kind::Boolean => Body::Bool(BoolRle::new(s.get(col, DATA)?)),
            Kind::Byte => Body::Byte(ByteRle::new(s.get(col, DATA)?)),
            Kind::Short | Kind::Int | Kind::Long => {
                Body::Int(IntRle::new(s.get(col, DATA)?, true, v2))
            }
            Kind::Float => Body::Float(Input::new(s.get(col, DATA)?)),
            Kind::Double => Body::Double(Input::new(s.get(col, DATA)?)),
            Kind::String | Kind::Varchar | Kind::Char | Kind::Binary
                if encoding == 0 || encoding == 2 =>
            {
                Body::Direct {
                    data: Input::new(s.get(col, DATA)?),
                    lengths: IntRle::new(s.get(col, LENGTH)?, false, v2),
                    binary: t.kind == Kind::Binary,
                }
            }
            Kind::String | Kind::Varchar | Kind::Char => {
                let dict = s.get(col, DICTIONARY_DATA)?;
                let mut lengths = IntRle::new(s.get(col, LENGTH)?, false, v2);
                // Entries are distinct, so only one can be empty.
                if dict_size > dict.len() as u64 + 1 {
                    return Err(damaged("a dictionary has more entries than bytes"));
                }
                let index_bytes = dict_size
                    .checked_mul(std::mem::size_of::<usize>() as u64)
                    .filter(|n| *n <= s.budget)
                    .ok_or_else(|| damaged("dictionary indices exceed the stripe memory budget"))?;
                s.budget -= index_bytes;
                let mut ends = Vec::with_capacity(dict_size as usize);
                let mut end = 0usize;
                for _ in 0..dict_size {
                    end = end
                        .checked_add(lengths.index()?)
                        .filter(|&e| e <= dict.len())
                        .ok_or_else(|| damaged("a dictionary entry runs past its stream"))?;
                    ends.push(end);
                }
                Body::Dict {
                    dict,
                    ends,
                    indices: IntRle::new(s.get(col, DATA)?, false, v2),
                }
            }
            Kind::Binary => return Err(damaged("a binary column is dictionary-encoded")),
            Kind::Decimal => Body::Decimal {
                data: Input::new(s.get(col, DATA)?),
                scales: IntRle::new(s.get(col, SECONDARY)?, true, v2),
                scale: t.scale,
            },
            Kind::Date => Body::Date(IntRle::new(s.get(col, DATA)?, true, v2)),
            Kind::Timestamp | Kind::Instant => Body::Timestamp {
                secs: IntRle::new(s.get(col, DATA)?, true, v2),
                nanos: IntRle::new(s.get(col, SECONDARY)?, false, v2),
                instant: t.kind == Kind::Instant,
            },
            Kind::List => {
                let lengths = IntRle::new(s.get(col, LENGTH)?, false, v2);
                let mut c = t.children.clone();
                Body::List {
                    lengths,
                    child: c.pop().ok_or("internal column mismatch")?,
                }
            }
            Kind::Map => {
                let lengths = IntRle::new(s.get(col, LENGTH)?, false, v2);
                let mut c = t.children.clone();
                let values = c.pop().ok_or("internal column mismatch")?;
                let keys = c.pop().ok_or("internal column mismatch")?;
                Body::Map {
                    lengths,
                    keys,
                    values,
                }
            }
            Kind::Struct => Body::Struct {
                names: (0..t.children.len()).map(|k| field_name(t, k)).collect(),
                fields: t.children.clone(),
            },
            Kind::Union => Body::Union {
                tags: ByteRle::new(s.get(col, DATA)?),
                branches: t.children.clone(),
            },
        };
        Ok(Reader {
            kind: t.kind,
            present,
            body,
        })
    }

    fn present(&mut self) -> Result<bool, String> {
        match &mut self.present {
            Some(p) => p.next(),
            None => Ok(true),
        }
    }
}
#[cfg(test)]
mod primitive_tests {
    use super::*;

    fn ints(bytes: &[u8], v2: bool, n: usize) -> Vec<i64> {
        let mut r = IntRle::new(bytes.to_vec(), false, v2);
        (0..n).map(|_| r.next().unwrap()).collect()
    }

    /// The worked examples of the ORC specification (unsigned runs).
    #[test]
    fn run_length_examples_from_the_specification() {
        assert_eq!(ints(&[0x61, 0x00, 0x07], false, 100), vec![7; 100]);
        assert_eq!(
            ints(&[0x61, 0xff, 0x64], false, 100),
            (1..=100).rev().collect::<Vec<_>>()
        );
        assert_eq!(
            ints(&[0xfb, 0x02, 0x03, 0x04, 0x07, 0x0b], false, 5),
            vec![2, 3, 4, 7, 11]
        );
        assert_eq!(ints(&[0x0a, 0x27, 0x10], true, 5), vec![10000; 5]);
        assert_eq!(
            ints(
                &[0x5e, 0x03, 0x5c, 0xa1, 0xab, 0x1e, 0xde, 0xad, 0xbe, 0xef],
                true,
                4
            ),
            vec![23713, 43806, 57005, 48879]
        );
        let patched = [
            0x8e, 0x13, 0x2b, 0x21, 0x07, 0xd0, 0x1e, 0x00, 0x14, 0x70, 0x28, 0x32, 0x3c, 0x46,
            0x50, 0x5a, 0x64, 0x6e, 0x78, 0x82, 0x8c, 0x96, 0xa0, 0xaa, 0xb4, 0xbe, 0xfc, 0xe8,
        ];
        assert_eq!(
            ints(&patched, true, 20),
            vec![
                2030, 2000, 2020, 1000000, 2040, 2050, 2060, 2070, 2080, 2090, 2100, 2110, 2120,
                2130, 2140, 2150, 2160, 2170, 2180, 2190
            ]
        );
        assert_eq!(
            ints(&[0xc6, 0x09, 0x02, 0x02, 0x22, 0x42, 0x42, 0x46], true, 10),
            vec![2, 3, 5, 7, 11, 13, 17, 19, 23, 29]
        );
        let mut bytes = ByteRle::new(vec![0x61, 0x00, 0xfe, 0x44, 0x45]);
        let got: Vec<u8> = (0..102).map(|_| bytes.next().unwrap()).collect();
        assert_eq!(&got[98..], &[0, 0, 0x44, 0x45]);
        assert!(bytes.next().is_err(), "the stream ends");
        let mut bools = BoolRle::new(vec![0xff, 0x80]);
        let got: Vec<bool> = (0..8).map(|_| bools.next().unwrap()).collect();
        assert_eq!(got, [true, false, false, false, false, false, false, false]);
    }

    #[test]
    fn decimals_and_timestamps_are_exact() {
        let text = |v: i128, scale: i64, type_scale: u64| {
            let mut out = Text::default();
            decimal(v, scale, type_scale, &mut out).unwrap();
            out.finish()
        };
        assert_eq!(text(-5, 2, 2), "-0.05");
        assert_eq!(text(15, 1, 2), "1.50");
        assert_eq!(text(12, -2, 0), "1200");
        assert_eq!(text(i128::MAX, 0, 38), i128::MAX.to_string());
        let ts = |secs: i64, raw: u64| {
            let mut out = Text::default();
            timestamp(secs, raw, false, &mut out).unwrap();
            out.finish()
        };
        assert_eq!(ts(0, 0), "2015-01-01 00:00:00");
        // 5 * 10^(2+1) nanoseconds: three trailing zeros removed, recorded as 2.
        assert_eq!(ts(1, 5 << 3 | 2), "2015-01-01 00:00:01.000005");
        assert_eq!(
            ts(-ORC_EPOCH - 1, 1_000_000 << 3),
            "1969-12-31 23:59:58.001"
        );
        // A negative remainder (C++ writer, before 1970) borrows a second.
        assert_eq!(
            ts(-ORC_EPOCH - 1, (-250_000_000i64 << 3) as u64),
            "1969-12-31 23:59:58.750"
        );
        assert!(timestamp(0, 2_000_000_000 << 3, false, &mut Text::default()).is_err());
        assert!(timestamp(i64::MAX, 0, false, &mut Text::default()).is_err());
        assert_eq!(date_text(-1), "1969-12-31");
        assert_eq!(date_text(i64::MIN), format!("{} days", i64::MIN));
    }
}

struct Orc {
    file: File,
    tail: Tail,
    schema: Schema,
}

pub fn open(path: &Path, table: Option<&str>) -> Result<Box<dyn TableSource>, String> {
    if table.is_some_and(|name| name != "rows") {
        return Err("An ORC file has one table, named rows".into());
    }
    Orc::open(path).map(|source| Box::new(source) as Box<dyn TableSource>)
}
impl Orc {
    fn open(path: &Path) -> Result<Self, String> {
        let mut file =
            File::open(path).map_err(|e| format!("The ORC file could not be opened: {e}"))?;
        let limits = Limits::default();
        let tail = tail(&mut file, &limits)?;
        let schema = metadata(&mut file, &tail, &limits)?;
        Ok(Self { file, tail, schema })
    }
}
impl TableSource for Orc {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn rows(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        let end = super::avro::request_end(&self.schema, start, count)?;
        if end <= start {
            return Ok(vec![]);
        }
        if self.tail.footer.encrypted {
            return Err("Encrypted ORC columns cannot be shown".into());
        }
        let mut at = 0u64;
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        for (index, stripe) in self.tail.footer.stripes.iter().enumerate() {
            if at >= end {
                break;
            }
            let next = at
                .checked_add(stripe.rows)
                .ok_or_else(|| damaged("the row count is too large"))?;
            if next > start {
                let decoded = decode_stripe(
                    &mut self.file,
                    &self.tail,
                    index,
                    start.saturating_sub(at),
                    (end - at).min(stripe.rows),
                )
                .map_err(|e| format!("{e} (stripe {})", index + 1))?;
                for row in &decoded {
                    for value in row.iter().flatten() {
                        bytes = bytes.saturating_add(value.len());
                    }
                }
                super::avro::check_output(bytes)?;
                rows.extend(decoded);
            }
            at = next;
        }
        Ok(rows)
    }
}
#[cfg(test)]
mod tests;
