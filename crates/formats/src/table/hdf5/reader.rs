//! HDF5 files through the Parquet table path, read straight from the file format: the
//! superblock, object headers, groups (symbol tables, link messages and dense link storage) and
//! the storage of one dataset at a time; each dataset is a table. Hand-written (no HDF5 library):
//! every address, length and count is checked against the file and a budget before reading or
//! allocating, walks are bounded and loops are detected, and a damaged object becomes a warning
//! while the rest of the file still lists. Only the given file is read: external links, external
//! storage and virtual datasets are named, never followed.
use super::super::{Column as TableColumn, Schema, TableInfo, TableSource, cut};
use arrow_array::{
    ArrayRef, RecordBatch, RecordBatchOptions,
    builder::{
        Float32Builder, Float64Builder, Int8Builder, Int16Builder, Int32Builder, Int64Builder,
        StringBuilder, UInt8Builder, UInt16Builder, UInt32Builder, UInt64Builder,
    },
};
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use arrow_schema::{DataType, Field, Schema as ArrowSchema};
const TABLE_BATCH_BYTES: usize = 256 * 1024 * 1024;
#[derive(Clone)]
struct Limits {
    columns: usize,
    batch_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            columns: 10_000,
            batch_bytes: TABLE_BATCH_BYTES,
        }
    }
}
use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const SIGNATURE: &[u8; 8] = b"\x89HDF\r\n\x1a\n";
/// Object headers visited while listing a file.
const MAX_OBJECTS: usize = 100_000;
/// Group nesting followed at most.
const MAX_DEPTH: usize = 64;
/// Messages per object header, and continuation blocks per header.
const MAX_MESSAGES: usize = 65_536;
const MAX_CONTINUATIONS: usize = 4_096;
/// B-tree depth at most (real files stay far below).
const MAX_BTREE_DEPTH: usize = 64;
/// Largest metadata block read in one piece (header chunk, heap, B-tree node, index block).
const MAX_BLOCK: u64 = 64 * 1024 * 1024;
/// Metadata bytes read to list one file.
const MAX_METADATA: u64 = 1024 * 1024 * 1024;
/// Chunks indexed for one dataset, and chunks read for one page.
const MAX_CHUNKS: u64 = 2_000_000;
/// Stored and decompressed bytes handled for one page.
const MAX_PAGE_WORK: u64 = 2 * 1024 * 1024 * 1024;
/// Blocks of one fractal heap (dense groups and attributes).
const MAX_HEAP_BLOCKS: usize = 100_000;
/// Datatype nesting (compounds in compounds, arrays of arrays ...).
const MAX_TYPE_DEPTH: usize = 16;
/// Largest element of a dataset or attribute.
const MAX_ELEMENT: usize = 16 * 1024 * 1024;
/// Attributes rendered for one file (more are counted, not shown).
const MAX_ATTRIBUTES: usize = 100_000;
const MAX_ATTRIBUTE_TEXT: usize = 16 * 1024 * 1024;
/// Values shown of an attribute, and characters of its text.
const ATTRIBUTE_VALUES: usize = 64;
const ATTRIBUTE_CHARS: usize = 1_000;
/// Characters of an attribute in the structure tree.
const TREE_VALUE_CHARS: usize = 80;
/// Size of the structure tree text.
const MAX_SCHEMA: usize = 1024 * 1024;
/// Text of one cell (variable-length sequences, arrays, compounds), and items per value.
const MAX_TEXT: usize = 1024 * 1024;
const MAX_ITEMS: usize = 10_000;
/// Warnings kept.
const MAX_WARNINGS: usize = 50;

fn damaged(what: &str) -> String {
    format!("The file is damaged: {what}")
}
fn io(e: std::io::Error) -> String {
    e.to_string()
}

/// Little-endian unsigned integer of up to 8 bytes.
fn le(b: &[u8]) -> u64 {
    b.iter()
        .take(8)
        .rev()
        .fold(0, |a, &x| (a << 8) | u64::from(x))
}
/// Unsigned integer of up to 16 bytes in either byte order.
fn uint(b: &[u8], big: bool) -> u128 {
    let fold = |a: u128, &x: &u8| (a << 8) | u128::from(x);
    if big {
        b.iter().take(16).fold(0, fold)
    } else {
        b.iter().take(16).rev().fold(0, fold)
    }
}
/// floor(log2(n)), 0 for 0.
fn log2(n: u64) -> u32 {
    63 - n.max(1).leading_zeros()
}
fn pad8(n: usize) -> usize {
    n.div_ceil(8).saturating_mul(8)
}
fn product(dims: &[u64]) -> Option<u64> {
    dims.iter().try_fold(1u64, |a, &d| a.checked_mul(d))
}
fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}
fn clip(mut s: String, max: usize) -> String {
    if let Some((i, _)) = s.char_indices().nth(max) {
        s.truncate(i);
        s.push('…');
    }
    s
}
fn tuple(dims: &[u64]) -> String {
    let parts: Vec<String> = dims
        .iter()
        .map(|&d| {
            if d == u64::MAX {
                "unlimited".to_owned()
            } else {
                d.to_string()
            }
        })
        .collect();
    if parts.len() == 1 {
        format!("({},)", parts[0])
    } else {
        format!("({})", parts.join(", "))
    }
}

/// A bounds-checked reader over one structure's bytes.
struct Cur<'a> {
    b: &'a [u8],
    at: usize,
}
impl<'a> Cur<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, at: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| damaged("a structure is cut short"))?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn skip(&mut self, n: usize) -> Result<(), String> {
        self.take(n).map(|_| ())
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(le(self.take(2)?) as u16)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(le(self.take(4)?) as u32)
    }
    fn uint(&mut self, n: usize) -> Result<u64, String> {
        Ok(le(self.take(n)?))
    }
    /// A file address; all bits set means "undefined".
    fn addr(&mut self, o: usize) -> Result<Option<u64>, String> {
        let s = self.take(o)?;
        Ok((!s.iter().all(|&b| b == 0xff)).then(|| le(s)))
    }
    fn sig(&mut self, sig: &[u8; 4]) -> Result<(), String> {
        if self.take(4)? != sig {
            return Err(damaged(&format!(
                "a {} block is missing",
                String::from_utf8_lossy(sig)
            )));
        }
        Ok(())
    }
    /// A NUL-terminated name, optionally padded to a multiple of 8 bytes.
    fn cstr(&mut self, pad: bool) -> Result<String, String> {
        let rest = &self.b[self.at.min(self.b.len())..];
        let n = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| damaged("a name is not terminated"))?;
        let s = lossy(&rest[..n]);
        self.skip(if pad { pad8(n + 1) } else { n + 1 })?;
        Ok(s)
    }
    fn left(&self) -> usize {
        self.b.len().saturating_sub(self.at)
    }
}

// ----- the file -----

struct Reader {
    file: File,
    cancelled: Arc<AtomicBool>,
    len: u64,
    /// Absolute position that file addresses are relative to (the superblock's, after a user block).
    base: u64,
    /// Sizes of offsets (addresses) and lengths.
    o: usize,
    l: usize,
    /// Metadata bytes that may still be read.
    budget: u64,
    /// Global heap collections read so far (variable-length data).
    globals: HashMap<u64, Arc<Vec<u8>>>,
    /// Object header address → path, for object references.
    paths: Arc<HashMap<u64, String>>,
}
impl Reader {
    fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err("cancelled".into())
        } else {
            Ok(())
        }
    }
    /// Absolute position of `len` bytes at file address `addr`, if they lie in the file.
    fn locate(&self, addr: u64, len: u64) -> Result<u64, String> {
        let at = self.base.checked_add(addr);
        match at.and_then(|a| a.checked_add(len)) {
            Some(end) if end <= self.len => Ok(at.unwrap_or_default()),
            _ => Err(damaged(&format!(
                "{len} bytes at address {addr:#x} lie outside the file"
            ))),
        }
    }
    fn read_into(&mut self, addr: u64, buf: &mut [u8]) -> Result<(), String> {
        self.check()?;
        let at = self.locate(addr, buf.len() as u64)?;
        self.file.seek(SeekFrom::Start(at)).map_err(io)?;
        self.file.read_exact(buf).map_err(io)
    }
    fn raw(&mut self, addr: u64, len: u64) -> Result<Vec<u8>, String> {
        self.locate(addr, len)?;
        let mut buf = vec![0; usize::try_from(len).map_err(|_| damaged("block too large"))?];
        self.read_into(addr, &mut buf)?;
        Ok(buf)
    }
    /// A metadata block: bounded in size and counted against the budget.
    fn meta(&mut self, addr: u64, len: u64) -> Result<Vec<u8>, String> {
        if len > MAX_BLOCK {
            return Err(damaged(&format!(
                "a {len}-byte metadata block is too large"
            )));
        }
        if len > self.budget {
            return Err(format!(
                "The file's structure is larger than the {} MiB Isolinear reads",
                MAX_METADATA >> 20
            ));
        }
        self.budget -= len;
        self.raw(addr, len)
    }
    /// Up to `len` bytes: fewer when the file ends first (for blocks sized by their own header).
    fn meta_upto(&mut self, addr: u64, len: u64) -> Result<Vec<u8>, String> {
        let at = self.locate(addr, 0)?;
        self.meta(addr, len.min(self.len - at))
    }
    /// An object of a global heap collection (variable-length strings and sequences).
    fn global(&mut self, addr: u64, index: u32) -> Result<Vec<u8>, String> {
        let col = match self.globals.get(&addr) {
            Some(c) => c.clone(),
            None => {
                let head = self.meta(addr, 8 + self.l as u64)?;
                let mut c = Cur::new(&head);
                c.sig(b"GCOL")?;
                c.skip(4)?;
                let size = c.uint(self.l)?;
                let col = Arc::new(self.meta(addr, size)?);
                let cached: usize = self.globals.values().map(|v| v.len()).sum();
                if self.globals.len() >= 256
                    || cached.saturating_add(col.len()) > MAX_BLOCK as usize
                {
                    self.globals.clear();
                }
                self.globals.insert(addr, col.clone());
                col
            }
        };
        let mut c = Cur::new(&col);
        c.skip(8 + self.l)?;
        while c.left() >= 8 + self.l {
            let i = c.u16()?;
            c.skip(6)?;
            let size = c.uint(self.l)?;
            if i == 0 {
                break;
            }
            let size = usize::try_from(size).map_err(|_| damaged("heap object too large"))?;
            if u32::from(i) == index {
                return Ok(c.take(size)?.to_vec());
            }
            c.skip(pad8(size))?;
        }
        Err(damaged(&format!(
            "global heap object {index} at {addr:#x} is missing"
        )))
    }
}

struct Superblock {
    version: u8,
    root: u64,
    eof: Option<u64>,
    extension: bool,
}

/// Open the file and read its superblock (at 0, 512, 1024, 2048 ...).
fn superblock(path: &Path, cancelled: Arc<AtomicBool>) -> Result<(Reader, Superblock), String> {
    let mut file = File::open(path).map_err(io)?;
    let len = file.metadata().map_err(io)?.len();
    let mut at = 0u64;
    let head = loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if at.saturating_add(8) > len || at > (1 << 40) {
            return Err("Not an HDF5 file: no signature".into());
        }
        let mut head = [0u8; 48];
        file.seek(SeekFrom::Start(at)).map_err(io)?;
        let n = file.read(&mut head).map_err(io)?;
        if n >= 8 && &head[..8] == SIGNATURE {
            break head;
        }
        at = if at == 0 { 512 } else { at * 2 };
    };
    let version = head[8];
    let (o, l) = match version {
        0 | 1 => (head[13] as usize, head[14] as usize),
        2 | 3 => (head[9] as usize, head[10] as usize),
        v => return Err(format!("HDF5 superblock version {v} is not supported")),
    };
    if ![2, 4, 8].contains(&o) || ![2, 4, 8].contains(&l) {
        return Err(damaged("invalid offset or length size"));
    }
    let mut r = Reader {
        file,
        cancelled,
        len,
        base: at,
        o,
        l,
        budget: MAX_METADATA,
        globals: HashMap::new(),
        paths: Arc::new(HashMap::new()),
    };
    // The rest of the superblock, read relative to its own position.
    let size = match version {
        0 => 24 + 4 * o + 2 * o + 24,
        1 => 28 + 4 * o + 2 * o + 24,
        _ => 12 + 4 * o + 4,
    } as u64;
    let b = r
        .raw(0, size)
        .map_err(|_| damaged("the superblock is cut short"))?;
    let mut c = Cur::new(&b);
    c.skip(if version == 0 {
        24
    } else if version == 1 {
        28
    } else {
        12
    })?;
    let base = c.addr(o)?.unwrap_or(0);
    let (root, eof, extension) = if version < 2 {
        let _free_space = c.addr(o)?;
        let eof = c.addr(o)?;
        let _driver = c.addr(o)?;
        let _name = c.addr(o)?;
        let root = c.addr(o)?;
        (root, eof, false)
    } else {
        let extension = c.addr(o)?;
        let eof = c.addr(o)?;
        let root = c.addr(o)?;
        (root, eof, extension.is_some())
    };
    // Addresses count from the superblock; a stored base address other than 0 replaces that.
    if base != 0 {
        r.base = base;
    }
    let root = root.ok_or_else(|| damaged("the root group has no address"))?;
    r.locate(root, 1)
        .map_err(|_| damaged("the superblock points outside the file"))?;
    Ok((
        r,
        Superblock {
            version,
            root,
            eof,
            extension,
        },
    ))
}

// ----- object headers -----

struct Msg {
    kind: u16,
    flags: u8,
    data: Vec<u8>,
}
struct Header {
    msgs: Vec<Msg>,
    /// Modification time (seconds since 1970) from a version 2 header.
    mtime: Option<u64>,
}

fn read_header(r: &mut Reader, addr: u64) -> Result<Header, String> {
    let first = r.meta_upto(addr, 64)?;
    let mut mtime = None;
    let v2 = first.starts_with(b"OHDR");
    let (start, size, order) = if v2 {
        let mut c = Cur::new(&first);
        c.skip(4)?;
        if c.u8()? != 2 {
            return Err(damaged("unknown object header version"));
        }
        let flags = c.u8()?;
        if flags & 0x20 != 0 {
            c.skip(4)?;
            mtime = Some(u64::from(c.u32()?));
            c.skip(8)?;
        }
        if flags & 0x10 != 0 {
            c.skip(4)?;
        }
        let size = c.uint(1 << (flags & 3))?;
        (addr.saturating_add(c.at as u64), size, flags & 0x04 != 0)
    } else if first.first() == Some(&1) {
        let mut c = Cur::new(&first);
        c.skip(8)?;
        (addr.saturating_add(16), u64::from(c.u32()?), false)
    } else {
        return Err(damaged(&format!("no object header at {addr:#x}")));
    };
    let mut msgs = vec![];
    let mut chunks = vec![(start, size)];
    let mut seen = HashSet::new();
    let mut i = 0;
    while let Some(&(at, len)) = chunks.get(i) {
        i += 1;
        if i > MAX_CONTINUATIONS || !seen.insert(at) {
            return Err(damaged("object header continuations loop"));
        }
        let b = r.meta(at, len)?;
        let body = if v2 && i > 1 {
            if b.len() < 8 || &b[..4] != b"OCHK" {
                return Err(damaged("an object header continuation is missing"));
            }
            &b[4..b.len() - 4]
        } else {
            &b[..]
        };
        let mut c = Cur::new(body);
        let head = if v2 { 4 + 2 * usize::from(order) } else { 8 };
        while c.left() >= head {
            let (kind, size, flags) = if v2 {
                let kind = u16::from(c.u8()?);
                let size = c.u16()?;
                let flags = c.u8()?;
                if order {
                    c.skip(2)?;
                }
                (kind, size, flags)
            } else {
                let kind = c.u16()?;
                let size = c.u16()?;
                let flags = c.u8()?;
                c.skip(3)?;
                (kind, size, flags)
            };
            let data = c.take(usize::from(size))?;
            if kind == 0x10 {
                let mut d = Cur::new(data);
                if let Some(a) = d.addr(r.o)? {
                    chunks.push((a, d.uint(r.l)?));
                }
            } else if kind != 0 {
                if msgs.len() >= MAX_MESSAGES {
                    return Err(damaged("an object header has too many messages"));
                }
                msgs.push(Msg {
                    kind,
                    flags,
                    data: data.to_vec(),
                });
            }
        }
    }
    Ok(Header { msgs, mtime })
}

/// Address of the object header a shared message lives in (committed datatypes).
fn shared_address(data: &[u8], o: usize, l: usize) -> Result<u64, String> {
    let mut c = Cur::new(data);
    let version = c.u8()?;
    let kind = c.u8()?;
    let addr = match version {
        1 => {
            c.skip(6 + l)?;
            c.addr(o)?
        }
        2 => c.addr(o)?,
        3 if kind == 2 => c.addr(o)?,
        3 => return Err("shared object header messages are not supported".into()),
        _ => return Err(damaged("unknown shared message version")),
    };
    addr.ok_or_else(|| damaged("a shared message has no address"))
}

/// The bytes of a message, following a shared message to the header that holds it.
fn message_data(r: &mut Reader, m: &Msg) -> Result<Vec<u8>, String> {
    if m.flags & 0x02 == 0 {
        return Ok(m.data.clone());
    }
    let addr = shared_address(&m.data, r.o, r.l)?;
    let header = read_header(r, addr)?;
    header
        .msgs
        .into_iter()
        .find(|s| s.kind == m.kind && s.flags & 0x02 == 0)
        .map(|s| s.data)
        .ok_or_else(|| damaged("a shared message is missing"))
}

// ----- datatypes and dataspaces -----

#[derive(Clone, Debug)]
struct Dtype {
    size: usize,
    class: Class,
}
#[derive(Clone, Debug)]
struct Member {
    name: String,
    offset: usize,
    ty: Dtype,
}
#[derive(Clone, Debug)]
enum Class {
    Int {
        signed: bool,
        big: bool,
    },
    Float {
        big: bool,
        ieee: bool,
    },
    Str {
        utf8: bool,
        pad: u8,
    },
    VlenStr {
        utf8: bool,
    },
    Vlen(Box<Dtype>),
    Compound(Vec<Member>),
    Enum(Box<Dtype>, Vec<(String, Vec<u8>)>),
    Array(Vec<u64>, Box<Dtype>),
    /// Bitfield, opaque or time: shown as hex.
    Bytes(&'static str, String),
    Reference {
        region: bool,
    },
}

fn parse_type(c: &mut Cur, depth: usize) -> Result<Dtype, String> {
    enum Task {
        Read(usize),
        Compound {
            size: usize,
            version: u8,
            left: u32,
            members: Vec<Member>,
            depth: usize,
        },
        Member {
            size: usize,
            version: u8,
            left: u32,
            members: Vec<Member>,
            depth: usize,
            name: String,
            offset: usize,
            dims: Vec<u64>,
        },
        Enum {
            size: usize,
            version: u8,
            n: u32,
        },
        Vlen {
            size: usize,
            bits: u32,
        },
        Array {
            size: usize,
            dims: Vec<u64>,
        },
    }
    let mut tasks = vec![Task::Read(depth)];
    let mut value: Option<Dtype> = None;
    let mut nodes = 0usize;
    while let Some(task) = tasks.pop() {
        match task {
            Task::Read(depth) => {
                nodes += 1;
                if depth > MAX_TYPE_DEPTH || nodes > MAX_ITEMS {
                    return Err(damaged(
                        "datatypes nest too deeply or contain too many members",
                    ));
                }
                let b0 = c.u8()?;
                let (class, version) = (b0 & 0x0f, b0 >> 4);
                let bits = c.uint(3)? as u32;
                let size = c.u32()? as usize;
                if size > MAX_ELEMENT {
                    return Err(damaged(&format!("a {size}-byte element is too large")));
                }
                let big = bits & 1 != 0;
                let class = match class {
                    0 => {
                        c.skip(4)?;
                        Class::Int {
                            signed: bits & 0x08 != 0,
                            big,
                        }
                    }
                    1 => {
                        c.skip(5)?;
                        let exponent = c.u8()?;
                        c.skip(1)?;
                        let mantissa = c.u8()?;
                        c.skip(4)?;
                        let ieee = bits & 0x40 == 0
                            && matches!(
                                (size, exponent, mantissa),
                                (2, 5, 10) | (4, 8, 23) | (8, 11, 52)
                            );
                        Class::Float { big, ieee }
                    }
                    2 => {
                        c.skip(2)?;
                        Class::Bytes("time", String::new())
                    }
                    3 => Class::Str {
                        utf8: (bits >> 4) & 0x0f == 1,
                        pad: (bits & 0x0f) as u8,
                    },
                    4 => {
                        c.skip(4)?;
                        Class::Bytes("bitfield", String::new())
                    }
                    5 => {
                        let tag = c.take((bits & 0xff) as usize)?;
                        let end = tag.iter().position(|&b| b == 0).unwrap_or(tag.len());
                        Class::Bytes("opaque", lossy(&tag[..end]))
                    }
                    6 => {
                        tasks.push(Task::Compound {
                            size,
                            version,
                            left: bits & 0xffff,
                            members: vec![],
                            depth,
                        });
                        continue;
                    }
                    7 => Class::Reference {
                        region: bits & 0x0f == 1,
                    },
                    8 => {
                        tasks.push(Task::Enum {
                            size,
                            version,
                            n: bits & 0xffff,
                        });
                        tasks.push(Task::Read(depth + 1));
                        continue;
                    }
                    9 => {
                        tasks.push(Task::Vlen { size, bits });
                        tasks.push(Task::Read(depth + 1));
                        continue;
                    }
                    10 => {
                        let rank = usize::from(c.u8()?);
                        if version < 3 {
                            c.skip(3)?;
                        }
                        let dims = (0..rank)
                            .map(|_| c.u32().map(u64::from))
                            .collect::<Result<Vec<_>, _>>()?;
                        if version < 3 {
                            c.skip(4 * rank)?;
                        }
                        tasks.push(Task::Array { size, dims });
                        tasks.push(Task::Read(depth + 1));
                        continue;
                    }
                    k => return Err(damaged(&format!("unknown datatype class {k}"))),
                };
                value = Some(Dtype { size, class });
            }
            Task::Compound {
                size,
                version,
                left,
                members,
                depth,
            } => {
                if left == 0 {
                    value = Some(Dtype {
                        size,
                        class: Class::Compound(members),
                    });
                    continue;
                }
                let name = c.cstr(version < 3)?;
                let offset = match version {
                    1 | 2 => c.u32()? as usize,
                    _ => c.uint(log2(size as u64) as usize / 8 + 1)? as usize,
                };
                let mut dims = Vec::new();
                if version == 1 {
                    let rank = usize::from(c.u8()?);
                    c.skip(11)?;
                    let all = (0..4)
                        .map(|_| c.u32().map(u64::from))
                        .collect::<Result<Vec<_>, _>>()?;
                    if (1..=4).contains(&rank) {
                        dims.extend_from_slice(&all[..rank]);
                    }
                }
                tasks.push(Task::Member {
                    size,
                    version,
                    left,
                    members,
                    depth,
                    name,
                    offset,
                    dims,
                });
                tasks.push(Task::Read(depth + 1));
            }
            Task::Member {
                size,
                version,
                left,
                mut members,
                depth,
                name,
                offset,
                dims,
            } => {
                let mut ty = value
                    .take()
                    .ok_or_else(|| damaged("missing member datatype"))?;
                if !dims.is_empty() {
                    let n = product(&dims)
                        .and_then(|n| n.checked_mul(ty.size as u64))
                        .filter(|&n| n <= MAX_ELEMENT as u64)
                        .ok_or_else(|| damaged("a compound member is too large"))?;
                    ty = Dtype {
                        size: n as usize,
                        class: Class::Array(dims, Box::new(ty)),
                    };
                }
                if offset.checked_add(ty.size).is_none_or(|e| e > size) {
                    return Err(damaged("a compound member lies outside its element"));
                }
                members.push(Member { name, offset, ty });
                tasks.push(Task::Compound {
                    size,
                    version,
                    left: left - 1,
                    members,
                    depth,
                });
            }
            Task::Enum { size, version, n } => {
                if n > MAX_ITEMS as u32 {
                    return Err(damaged("too many enum members"));
                }
                let base = value
                    .take()
                    .ok_or_else(|| damaged("missing enum datatype"))?;
                if base.size != size {
                    return Err(damaged("enum and base datatype sizes differ"));
                }
                let names = (0..n)
                    .map(|_| c.cstr(version < 3))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut values = Vec::new();
                for name in names {
                    values.push((name, c.take(base.size)?.to_vec()));
                }
                value = Some(Dtype {
                    size,
                    class: Class::Enum(Box::new(base), values),
                });
            }
            Task::Vlen { size, bits } => {
                let base = value
                    .take()
                    .ok_or_else(|| damaged("missing variable-length datatype"))?;
                value = Some(Dtype {
                    size,
                    class: if bits & 0x0f == 1 {
                        Class::VlenStr {
                            utf8: (bits >> 8) & 0x0f == 1,
                        }
                    } else {
                        Class::Vlen(Box::new(base))
                    },
                });
            }
            Task::Array { size, dims } => {
                let base = value
                    .take()
                    .ok_or_else(|| damaged("missing array datatype"))?;
                if product(&dims)
                    .and_then(|n| n.checked_mul(base.size as u64))
                    .is_none_or(|n| n > size as u64)
                {
                    return Err(damaged("an array type is larger than its element"));
                }
                value = Some(Dtype {
                    size,
                    class: Class::Array(dims, Box::new(base)),
                });
            }
        }
    }
    value.ok_or_else(|| damaged("missing datatype"))
}

impl Dtype {
    fn describe(&self) -> String {
        enum Task<'a> {
            Type(&'a Dtype),
            Text(String),
        }
        let mut stack = vec![Task::Type(self)];
        let mut out = String::new();
        while let Some(task) = stack.pop() {
            if out.len() > 2000 {
                break;
            }
            let ty = match task {
                Task::Type(ty) => ty,
                Task::Text(s) => {
                    out.push_str(&s);
                    continue;
                }
            };
            let endian = |big: bool| if big { " big-endian" } else { "" };
            let charset = |utf8: bool| if utf8 { "UTF-8" } else { "ASCII" };
            let text = match &ty.class {
                Class::Int { signed, big } => format!(
                    "{}int{}{}",
                    if *signed { "" } else { "u" },
                    ty.size * 8,
                    endian(*big)
                ),
                Class::Float { big, ieee: true } => format!("float{}{}", ty.size * 8, endian(*big)),
                Class::Float { .. } => format!("float ({} bytes, not IEEE)", ty.size),
                Class::Str { utf8, .. } => format!("string[{}] {}", ty.size, charset(*utf8)),
                Class::VlenStr { utf8 } => format!("string {} (variable length)", charset(*utf8)),
                Class::Vlen(base) => {
                    stack.push(Task::Type(base));
                    "sequence of ".into()
                }
                Class::Compound(members) => {
                    stack.push(Task::Text("}".into()));
                    for (i, m) in members.iter().enumerate().rev() {
                        stack.push(Task::Type(&m.ty));
                        stack.push(Task::Text(format!(
                            "{}{}: ",
                            if i == 0 { "" } else { ", " },
                            m.name
                        )));
                    }
                    "compound {".into()
                }
                Class::Enum(base, _) if ty.is_bool() => {
                    stack.push(Task::Text(")".into()));
                    stack.push(Task::Type(base));
                    "bool (enum ".into()
                }
                Class::Enum(base, names) => {
                    stack.push(Task::Text(format!(
                        " {{{}}}",
                        names
                            .iter()
                            .map(|(n, _)| n.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                    stack.push(Task::Type(base));
                    "enum ".into()
                }
                Class::Array(dims, base) => {
                    stack.push(Task::Text(format!(
                        "[{}]",
                        dims.iter()
                            .map(u64::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                    stack.push(Task::Type(base));
                    String::new()
                }
                Class::Bytes(what, tag) if tag.is_empty() => format!("{what} ({} bytes)", ty.size),
                Class::Bytes(what, tag) => format!("{what} ({} bytes, {tag})", ty.size),
                Class::Reference { region: false } => "object reference".into(),
                Class::Reference { region: true } => "region reference".into(),
            };
            out.push_str(&text);
        }
        clip(out, 500)
    }
    /// h5py's booleans: an enum of FALSE and TRUE.
    fn is_bool(&self) -> bool {
        matches!(&self.class, Class::Enum(_, names)
            if names.len() == 2 && names[0].0 == "FALSE" && names[1].0 == "TRUE")
    }
}

#[derive(Clone, Debug)]
struct Space {
    /// Empty for a scalar.
    dims: Vec<u64>,
    /// Maximum dimensions (`u64::MAX`: unlimited), when given.
    max: Option<Vec<u64>>,
    /// No data at all (null dataspace).
    null: bool,
}
impl Space {
    fn elements(&self) -> Option<u64> {
        if self.null {
            Some(0)
        } else {
            product(&self.dims)
        }
    }
    fn describe(&self) -> String {
        if self.null {
            "null (no data)".into()
        } else if self.dims.is_empty() {
            "scalar".into()
        } else {
            tuple(&self.dims)
        }
    }
}

fn parse_space(c: &mut Cur, l: usize) -> Result<Space, String> {
    let version = c.u8()?;
    let rank = usize::from(c.u8()?);
    let flags = c.u8()?;
    if rank > 32 {
        return Err(damaged("a dataspace has more than 32 dimensions"));
    }
    let null = match version {
        1 => {
            c.skip(5)?;
            false
        }
        2 => c.u8()? == 2,
        v => return Err(damaged(&format!("unknown dataspace version {v}"))),
    };
    let unlimited = if l >= 8 {
        u64::MAX
    } else {
        (1u64 << (8 * l)) - 1
    };
    let dims: Vec<u64> = (0..rank).map(|_| c.uint(l)).collect::<Result<_, _>>()?;
    let max = if flags & 1 != 0 {
        Some(
            (0..rank)
                .map(|_| c.uint(l).map(|m| if m == unlimited { u64::MAX } else { m }))
                .collect::<Result<_, _>>()?,
        )
    } else {
        None
    };
    Ok(Space { dims, max, null })
}

// ----- storage -----

#[derive(Clone, Debug)]
enum Index {
    BTree1(Option<u64>),
    /// Address, and the stored size and filter mask of a filtered chunk.
    Single(Option<u64>, Option<(u64, u32)>),
    Implicit(Option<u64>),
    Fixed(Option<u64>),
    Extensible(Option<u64>),
    BTree2(Option<u64>),
}
#[derive(Clone, Debug)]
enum Layout {
    Compact(Vec<u8>),
    Contiguous(Option<u64>, Option<u64>),
    Chunked {
        dims: Vec<u64>,
        index: Index,
        /// Partial edge chunks are stored without filters.
        unfiltered_edges: bool,
    },
    Virtual,
}

fn parse_layout(c: &mut Cur, o: usize, l: usize) -> Result<Layout, String> {
    let version = c.u8()?;
    let chunk_dims = |dims: Vec<u64>| -> Result<Vec<u64>, String> {
        let rank = dims.len().saturating_sub(1);
        let dims = dims[..rank].to_vec();
        if rank == 0 || dims.contains(&0) {
            return Err(damaged("invalid chunk dimensions"));
        }
        Ok(dims)
    };
    match version {
        1 | 2 => {
            let rank = usize::from(c.u8()?);
            let class = c.u8()?;
            c.skip(5)?;
            let addr = if class != 0 { c.addr(o)? } else { None };
            let dims: Vec<u64> = (0..rank)
                .map(|_| c.u32().map(u64::from))
                .collect::<Result<_, _>>()?;
            match class {
                0 => {
                    let size = c.u32()? as usize;
                    Ok(Layout::Compact(c.take(size)?.to_vec()))
                }
                1 => Ok(Layout::Contiguous(addr, None)),
                2 => Ok(Layout::Chunked {
                    dims: chunk_dims(dims)?,
                    index: Index::BTree1(addr),
                    unfiltered_edges: false,
                }),
                k => Err(damaged(&format!("unknown layout class {k}"))),
            }
        }
        3 | 4 => match c.u8()? {
            0 => {
                let size = usize::from(c.u16()?);
                Ok(Layout::Compact(c.take(size)?.to_vec()))
            }
            1 => {
                let addr = c.addr(o)?;
                Ok(Layout::Contiguous(addr, Some(c.uint(l)?)))
            }
            2 if version == 3 => {
                let rank = usize::from(c.u8()?);
                let addr = c.addr(o)?;
                let dims: Vec<u64> = (0..rank)
                    .map(|_| c.u32().map(u64::from))
                    .collect::<Result<_, _>>()?;
                Ok(Layout::Chunked {
                    dims: chunk_dims(dims)?,
                    index: Index::BTree1(addr),
                    unfiltered_edges: false,
                })
            }
            2 => {
                let flags = c.u8()?;
                let rank = usize::from(c.u8()?);
                let width = usize::from(c.u8()?);
                if !(1..=8).contains(&width) {
                    return Err(damaged("invalid chunk dimension size"));
                }
                let dims: Vec<u64> = (0..rank).map(|_| c.uint(width)).collect::<Result<_, _>>()?;
                let index = match c.u8()? {
                    1 => {
                        let filtered = if flags & 2 != 0 {
                            Some((c.uint(l)?, c.u32()?))
                        } else {
                            None
                        };
                        Index::Single(c.addr(o)?, filtered)
                    }
                    2 => Index::Implicit(c.addr(o)?),
                    3 => {
                        c.skip(1)?;
                        Index::Fixed(c.addr(o)?)
                    }
                    4 => {
                        c.skip(5)?;
                        Index::Extensible(c.addr(o)?)
                    }
                    5 => {
                        c.skip(6)?;
                        Index::BTree2(c.addr(o)?)
                    }
                    k => return Err(damaged(&format!("unknown chunk index type {k}"))),
                };
                Ok(Layout::Chunked {
                    dims: chunk_dims(dims)?,
                    index,
                    unfiltered_edges: flags & 1 != 0,
                })
            }
            3 => Ok(Layout::Virtual),
            k => Err(damaged(&format!("unknown layout class {k}"))),
        },
        v => Err(damaged(&format!("unknown layout version {v}"))),
    }
}

#[derive(Clone, Debug)]
struct Filter {
    id: u16,
    name: String,
    params: Vec<u32>,
}
impl Filter {
    fn supported(&self) -> bool {
        matches!(self.id, 1..=3 | 32015)
    }
    fn label(&self) -> String {
        match self.id {
            1 => match self.params.first() {
                Some(level) => format!("gzip level {level}"),
                None => "gzip".into(),
            },
            2 => "shuffle".into(),
            3 => "fletcher32".into(),
            4 => "szip (4)".into(),
            5 => "nbit (5)".into(),
            6 => "scale-offset (6)".into(),
            32000 => "lzf (32000)".into(),
            32001 => "blosc (32001)".into(),
            32004 => "lz4 (32004)".into(),
            32008 => "bitshuffle (32008)".into(),
            32013 => "zfp (32013)".into(),
            32015 => "zstd".into(),
            id if self.name.is_empty() => format!("filter {id}"),
            id => format!("{} ({id})", self.name),
        }
    }
}

fn parse_filters(c: &mut Cur) -> Result<Vec<Filter>, String> {
    let version = c.u8()?;
    let n = c.u8()?;
    if version == 1 {
        c.skip(6)?;
    } else if version != 2 {
        return Err(damaged(&format!(
            "unknown filter pipeline version {version}"
        )));
    }
    let mut filters = vec![];
    for _ in 0..n {
        let id = c.u16()?;
        let name_len = if version == 1 || id >= 256 {
            usize::from(c.u16()?)
        } else {
            0
        };
        c.skip(2)?;
        let values = usize::from(c.u16()?);
        let name = c.take(if version == 1 {
            pad8(name_len)
        } else {
            name_len
        })?;
        let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        let params = (0..values)
            .map(|_| c.u32())
            .collect::<Result<Vec<_>, _>>()?;
        if version == 1 && values % 2 == 1 {
            c.skip(4)?;
        }
        filters.push(Filter {
            id,
            name: lossy(&name[..end]),
            params,
        });
    }
    Ok(filters)
}

fn parse_fill(kind: u16, c: &mut Cur) -> Result<Option<Vec<u8>>, String> {
    if kind == 0x04 {
        let size = c.u32()? as usize;
        return Ok(Some(c.take(size)?.to_vec()));
    }
    let version = c.u8()?;
    let defined = match version {
        1 | 2 => {
            c.skip(2)?;
            version == 1 || c.u8()? != 0
        }
        3 => c.u8()? & 0x20 != 0,
        _ => return Err(damaged("unknown fill value version")),
    };
    if version == 1 {
        c.skip(1)?;
    }
    if !defined {
        return Ok(None);
    }
    let size = c.u32()? as usize;
    Ok(Some(c.take(size)?.to_vec()))
}

// ----- B-trees and heaps -----

/// Leaf entries of a version 1 B-tree of `node_type`: (key before the child, child address).
fn btree1(
    r: &mut Reader,
    root: u64,
    node_type: u8,
    key: usize,
    max: u64,
) -> Result<Vec<(Vec<u8>, u64)>, String> {
    let o = r.o;
    let mut out = vec![];
    let mut seen = HashSet::new();
    let mut stack = vec![(root, None::<u8>)];
    while let Some((at, level)) = stack.pop() {
        if !seen.insert(at) {
            return Err(damaged("a B-tree loops"));
        }
        let head = r.meta(at, 8 + 2 * o as u64)?;
        let mut c = Cur::new(&head);
        c.sig(b"TREE")?;
        if c.u8()? != node_type {
            return Err(damaged("a B-tree node has the wrong type"));
        }
        let lvl = c.u8()?;
        let n = usize::from(c.u16()?);
        if usize::from(lvl) > MAX_BTREE_DEPTH || level.is_some_and(|l| l != lvl) {
            return Err(damaged("a B-tree node is at the wrong level"));
        }
        let step = key + o;
        let body = r.meta(at.saturating_add(8 + 2 * o as u64), (n * step + key) as u64)?;
        let mut children = vec![];
        for i in 0..n {
            let mut c = Cur::new(&body[i * step..]);
            let k = c.take(key)?;
            if let Some(child) = c.addr(o)? {
                children.push((k.to_vec(), child));
            }
        }
        if lvl == 0 {
            if out.len() as u64 + children.len() as u64 > max {
                return Err(format!("More than {max} entries in a B-tree"));
            }
            out.extend(children);
        } else {
            stack.extend(children.into_iter().rev().map(|(_, a)| (a, Some(lvl - 1))));
        }
    }
    Ok(out)
}

/// Records of a version 2 B-tree, with its type and record size.
fn btree2(r: &mut Reader, addr: u64, max: u64) -> Result<(u8, usize, Vec<Vec<u8>>), String> {
    let (o, l) = (r.o, r.l);
    let head = r.meta(addr, (22 + o + l) as u64)?;
    let mut c = Cur::new(&head);
    c.sig(b"BTHD")?;
    c.skip(1)?;
    let kind = c.u8()?;
    let node = u64::from(c.u32()?);
    let rec = usize::from(c.u16()?);
    let depth = usize::from(c.u16()?);
    c.skip(2)?;
    let root = c.addr(o)?;
    let root_records = u64::from(c.u16()?);
    let total = c.uint(l)?;
    if rec == 0 || node < 10 + rec as u64 || node > MAX_BLOCK || depth > MAX_BTREE_DEPTH {
        return Err(damaged("invalid B-tree header"));
    }
    if total > max {
        return Err(format!("More than {max} entries in a B-tree"));
    }
    // Sizes of the record counts kept in internal nodes, as the library computes them.
    let leaf_max = (node - 10) / rec as u64;
    let count_size = log2(leaf_max) as usize / 8 + 1;
    let mut info = vec![(leaf_max, leaf_max, 0usize)];
    for d in 1..=depth {
        let below = info[d - 1];
        let pointer = (o + count_size + if d > 1 { below.2 } else { 0 }) as u64;
        let max = node.saturating_sub(10 + pointer) / (rec as u64 + pointer);
        let cum = max
            .saturating_add(1)
            .saturating_mul(below.1)
            .saturating_add(max);
        info.push((max, cum, log2(cum) as usize / 8 + 1));
    }
    let mut out = vec![];
    let mut seen = HashSet::new();
    let mut stack = vec![(root, root_records, depth)];
    while let Some((at, n, d)) = stack.pop() {
        let Some(at) = at else { continue };
        if !seen.insert(at) {
            return Err(damaged("a B-tree loops"));
        }
        if n > info[d].0 {
            return Err(damaged("a B-tree node has too many records"));
        }
        let b = r.meta(at, node)?;
        let mut c = Cur::new(&b);
        c.sig(if d == 0 { b"BTLF" } else { b"BTIN" })?;
        c.skip(1)?;
        if c.u8()? != kind {
            return Err(damaged("a B-tree node has the wrong type"));
        }
        for _ in 0..n {
            out.push(c.take(rec)?.to_vec());
            if out.len() as u64 > max {
                return Err(format!("More than {max} entries in a B-tree"));
            }
        }
        if d > 0 {
            for _ in 0..=n {
                let child = c.addr(o)?;
                let records = c.uint(count_size)?;
                if d > 1 {
                    c.skip(info[d - 1].2)?;
                }
                stack.push((child, records, d - 1));
            }
        }
    }
    Ok((kind, rec, out))
}

/// The local heap of an old-style group: names of its links.
fn local_heap(r: &mut Reader, addr: u64) -> Result<Vec<u8>, String> {
    let (o, l) = (r.o, r.l);
    let head = r.meta(addr, (8 + 2 * l + o) as u64)?;
    let mut c = Cur::new(&head);
    c.sig(b"HEAP")?;
    c.skip(4)?;
    let size = c.uint(l)?;
    c.skip(l)?;
    let data = c
        .addr(o)?
        .ok_or_else(|| damaged("a local heap has no data"))?;
    r.meta(data, size)
}
fn heap_name(heap: &[u8], at: u64) -> Result<String, String> {
    let rest = usize::try_from(at)
        .ok()
        .and_then(|a| heap.get(a..))
        .ok_or_else(|| damaged("a name lies outside its heap"))?;
    Cur::new(rest).cstr(false)
}

/// A fractal heap (dense links and attributes): its direct blocks by heap offset.
struct FractalHeap {
    id_len: usize,
    off_size: usize,
    len_size: usize,
    /// (heap offset, address, size), sorted by offset.
    blocks: Vec<(u64, u64, u64)>,
}
impl FractalHeap {
    fn open(r: &mut Reader, addr: u64) -> Result<Self, String> {
        let (o, l) = (r.o, r.l);
        let b = r.meta(addr, (22 + 12 * l + 3 * o) as u64)?;
        let mut c = Cur::new(&b);
        c.sig(b"FRHP")?;
        c.skip(1)?;
        let id_len = usize::from(c.u16()?);
        let filters = c.u16()?;
        c.skip(1)?;
        let max_managed = u64::from(c.u32()?);
        c.skip(l + o + l + o + 8 * l)?;
        let width = u64::from(c.u16()?);
        let start = c.uint(l)?;
        let max_direct = c.uint(l)?;
        let max_bits = u32::from(c.u16()?);
        c.skip(2)?;
        let root = c.addr(o)?;
        let rows = u64::from(c.u16()?);
        if filters > 0 {
            return Err("compressed fractal heaps are not supported".into());
        }
        if width == 0
            || !width.is_power_of_two()
            || !start.is_power_of_two()
            || !max_direct.is_power_of_two()
            || max_direct < start
            || max_direct > MAX_BLOCK
            || !(1..=64).contains(&max_bits)
        {
            return Err(damaged("invalid fractal heap header"));
        }
        let off_size = max_bits.div_ceil(8) as usize;
        let len_size =
            (log2(max_direct).div_ceil(8) as usize).min(log2(max_managed) as usize / 8 + 1);
        if id_len < 1 + off_size + len_size.max(1) || id_len > 64 {
            return Err(damaged("invalid fractal heap ID length"));
        }
        let mut heap = Self {
            id_len,
            off_size,
            len_size,
            blocks: vec![],
        };
        let geometry = Geometry {
            width,
            start,
            max_direct_rows: u64::from(log2(max_direct) - log2(start) + 2),
            first_row_bits: log2(start) + log2(width),
            off_size,
        };
        if let Some(root) = root {
            if rows == 0 {
                heap.blocks.push((0, root, start));
            } else {
                let mut seen = HashSet::new();
                heap.indirect(r, &geometry, root, 0, rows, 0, &mut seen)?;
            }
        }
        heap.blocks.sort_unstable();
        Ok(heap)
    }

    #[allow(clippy::too_many_arguments)]
    fn indirect(
        &mut self,
        r: &mut Reader,
        g: &Geometry,
        addr: u64,
        offset: u64,
        rows: u64,
        depth: usize,
        seen: &mut HashSet<u64>,
    ) -> Result<(), String> {
        let mut pending = vec![(addr, offset, rows, depth)];
        while let Some((addr, offset, rows, depth)) = pending.pop() {
            r.check()?;
            if depth > 16 || rows > 64 || seen.len() >= MAX_HEAP_BLOCKS || !seen.insert(addr) {
                return Err(damaged("a fractal heap loops"));
            }
            let o = r.o;
            let entries = rows * g.width;
            let size = 4 + 1 + o as u64 + g.off_size as u64 + entries * o as u64 + 4;
            let b = r.meta(addr, size)?;
            let mut c = Cur::new(&b);
            c.sig(b"FHIB")?;
            c.skip(1 + o + g.off_size)?;
            let mut at = offset;
            for row in 0..rows {
                let block = if row < 2 {
                    g.start
                } else {
                    g.start
                        .checked_shl((row - 1) as u32)
                        .filter(|&b| b != 0)
                        .ok_or_else(|| damaged("a fractal heap is too large"))?
                };
                for _ in 0..g.width {
                    if let Some(child) = c.addr(o)? {
                        if row < g.max_direct_rows {
                            if self.blocks.len() >= MAX_HEAP_BLOCKS {
                                return Err(damaged("a fractal heap has too many blocks"));
                            }
                            self.blocks.push((at, child, block));
                        } else {
                            let child_rows = u64::from(log2(block) + 1)
                                .saturating_sub(u64::from(g.first_row_bits));
                            if child_rows >= rows {
                                return Err(damaged("a fractal heap loops"));
                            }
                            if pending.len() >= MAX_HEAP_BLOCKS {
                                return Err(damaged("a fractal heap has too many blocks"));
                            }
                            pending.push((child, at, child_rows, depth + 1));
                        }
                    }
                    at = at
                        .checked_add(block)
                        .ok_or_else(|| damaged("a fractal heap is too large"))?;
                }
            }
        }
        Ok(())
    }

    fn get(&self, r: &mut Reader, id: &[u8]) -> Result<Vec<u8>, String> {
        let id = id
            .get(..self.id_len)
            .ok_or_else(|| damaged("a heap ID is cut short"))?;
        match id[0] >> 4 {
            0 => {
                let mut c = Cur::new(&id[1..]);
                let offset = c.uint(self.off_size)?;
                let len = c.uint(self.len_size)?;
                let i = self.blocks.partition_point(|b| b.0 <= offset);
                let (start, addr, size) = *i
                    .checked_sub(1)
                    .and_then(|i| self.blocks.get(i))
                    .ok_or_else(|| damaged("a heap object is missing"))?;
                let within = offset - start;
                if within.checked_add(len).is_none_or(|e| e > size) {
                    return Err(damaged("a heap object lies outside its block"));
                }
                r.meta(addr.saturating_add(within), len)
            }
            2 => {
                let (len, at) = if self.id_len <= 18 {
                    (usize::from(id[0] & 0x0f) + 1, 1)
                } else {
                    (
                        (usize::from(id[0] & 0x0f) << 8 | usize::from(*id.get(1).unwrap_or(&0)))
                            + 1,
                        2,
                    )
                };
                id.get(at..at + len)
                    .map(<[u8]>::to_vec)
                    .ok_or_else(|| damaged("a tiny heap object is cut short"))
            }
            1 => Err("huge fractal heap objects are not supported".into()),
            _ => Err(damaged("unknown heap ID")),
        }
    }
}
struct Geometry {
    width: u64,
    start: u64,
    max_direct_rows: u64,
    first_row_bits: u32,
    off_size: usize,
}

/// Objects stored densely: a fractal heap and a version 2 B-tree naming them.
fn dense(r: &mut Reader, heap: u64, tree: u64, id_at: usize) -> Result<Vec<Vec<u8>>, String> {
    let heap = FractalHeap::open(r, heap)?;
    let (_, _, records) = btree2(r, tree, MAX_OBJECTS as u64)?;
    records
        .iter()
        .map(|rec| heap.get(r, rec.get(id_at..).unwrap_or_default()))
        .collect()
}

// ----- values as text -----

fn float(b: &[u8], big: bool) -> f64 {
    let v = uint(b, big);
    match b.len() {
        2 => f64::from(half(v as u16)),
        4 => f64::from(f32::from_bits(v as u32)),
        _ => f64::from_bits(v as u64),
    }
}
fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((bits >> 10) & 0x1f);
    let man = f32::from(bits & 0x3ff);
    match exp {
        0 => sign * man * 2f32.powi(-24),
        31 if man == 0.0 => sign * f32::INFINITY,
        31 => f32::NAN,
        e => sign * (1.0 + man / 1024.0) * 2f32.powi(e - 15),
    }
}
fn hex(b: &[u8]) -> String {
    let mut s = String::from("0x");
    for x in b.iter().take(256) {
        let _ = write!(s, "{x:02x}");
    }
    if b.len() > 256 {
        s.push('…');
    }
    s
}
fn fixed_str(b: &[u8], pad: u8) -> &[u8] {
    match pad {
        0 => &b[..b.iter().position(|&x| x == 0).unwrap_or(b.len())],
        2 => {
            let end = b.iter().rposition(|&x| x != b' ').map_or(0, |i| i + 1);
            &b[..end]
        }
        _ => {
            let end = b.iter().rposition(|&x| x != 0).map_or(0, |i| i + 1);
            &b[..end]
        }
    }
}

/// The bytes of a variable-length value (length, global heap collection, object index).
fn vlen(r: &mut Reader, b: &[u8]) -> Result<(u64, Vec<u8>), String> {
    let mut c = Cur::new(b);
    let n = u64::from(c.u32()?);
    let addr = c.addr(r.o)?;
    let index = c.u32()?;
    match addr {
        Some(a) if a != 0 && n > 0 => Ok((n, r.global(a, index)?)),
        _ => Ok((0, vec![])),
    }
}

/// Text of one value; `quote` puts strings in quotes (inside lists and compounds).
#[derive(Clone)]
enum ValueBytes<'a> {
    Borrowed(&'a [u8]),
    Owned(Arc<Vec<u8>>, usize),
}
impl ValueBytes<'_> {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Borrowed(b) => b,
            Self::Owned(b, at) => b.get(*at..).unwrap_or_default(),
        }
    }
    fn tail(&self, offset: usize) -> Result<Self, String> {
        if offset > self.bytes().len() {
            return Err(damaged("a value is cut short"));
        }
        Ok(match self {
            Self::Borrowed(b) => Self::Borrowed(&b[offset..]),
            Self::Owned(b, at) => Self::Owned(
                b.clone(),
                at.checked_add(offset)
                    .ok_or_else(|| damaged("value offset overflows"))?,
            ),
        })
    }
}

fn text(
    r: &mut Reader,
    ty: &Dtype,
    b: &[u8],
    quote: bool,
    _depth: usize,
    out: &mut String,
) -> Result<(), String> {
    enum Task<'a> {
        Value(&'a Dtype, ValueBytes<'a>, bool),
        List(&'a Dtype, ValueBytes<'a>, u64, usize),
        Compound(&'a [Member], ValueBytes<'a>, usize),
    }
    let mut tasks = vec![Task::Value(ty, ValueBytes::Borrowed(b), quote)];
    while let Some(task) = tasks.pop() {
        r.check()?;
        if out.len() > MAX_TEXT {
            out.push_str("… (value truncated)");
            break;
        }
        match task {
            Task::List(base, data, n, i) => {
                let size = base.size.max(1);
                let available = data.bytes().len() / size;
                if i as u64 >= n.min(available as u64).min(MAX_ITEMS as u64) {
                    if n > MAX_ITEMS as u64 || n > available as u64 {
                        out.push_str(", …");
                    }
                    out.push(']');
                } else {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    let value = data.tail(i * size)?;
                    tasks.push(Task::List(base, data, n, i + 1));
                    tasks.push(Task::Value(base, value, true));
                }
            }
            Task::Compound(members, data, i) => {
                if let Some(member) = members.get(i) {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    let _ = write!(out, "{}: ", member.name);
                    let value = data.tail(member.offset)?;
                    tasks.push(Task::Compound(members, data, i + 1));
                    tasks.push(Task::Value(&member.ty, value, true));
                } else {
                    out.push('}');
                }
            }
            Task::Value(ty, data, quote) => {
                let b = data
                    .bytes()
                    .get(..ty.size)
                    .ok_or_else(|| damaged("a value is cut short"))?;
                let string = |s: &[u8], out: &mut String| {
                    let shown = clip(
                        String::from_utf8_lossy(&s[..s.len().min(MAX_TEXT * 4)]).into_owned(),
                        MAX_TEXT,
                    );
                    if quote {
                        let _ = write!(out, "{shown:?}");
                    } else {
                        out.push_str(&shown);
                    }
                    if s.len() > MAX_TEXT * 4 {
                        out.push_str("… (value truncated)");
                    }
                };
                match &ty.class {
                    Class::Int { signed, big } if ty.size <= 16 => {
                        let v = uint(b, *big);
                        if *signed && ty.size > 0 {
                            let shift = 128 - 8 * ty.size as u32;
                            let _ = write!(out, "{}", ((v << shift) as i128) >> shift);
                        } else {
                            let _ = write!(out, "{v}");
                        }
                    }
                    Class::Float { big, ieee: true } => {
                        if ty.size == 8 {
                            let _ = write!(out, "{}", float(b, *big));
                        } else {
                            let _ = write!(out, "{}", float(b, *big) as f32);
                        }
                    }
                    Class::Str { pad, .. } => string(fixed_str(b, *pad), out),
                    Class::VlenStr { .. } => {
                        let (n, data) = vlen(r, b)?;
                        string(
                            &data[..usize::try_from(n).unwrap_or(usize::MAX).min(data.len())],
                            out,
                        );
                    }
                    Class::Vlen(base) => {
                        let (n, data) = vlen(r, b)?;
                        out.push('[');
                        tasks.push(Task::List(base, ValueBytes::Owned(Arc::new(data), 0), n, 0));
                    }
                    Class::Array(dims, base) => {
                        out.push('[');
                        tasks.push(Task::List(base, data, product(dims).unwrap_or(0), 0));
                    }
                    Class::Compound(members) => {
                        out.push('{');
                        tasks.push(Task::Compound(members, data, 0));
                    }
                    Class::Enum(base, names) => match names.iter().find(|(_, v)| v.as_slice() == b)
                    {
                        Some((name, _)) => out.push_str(name),
                        None => tasks.push(Task::Value(base, data, quote)),
                    },
                    Class::Reference { region: false } if ty.size >= r.o => {
                        let addr = le(&b[..r.o]);
                        match r.paths.get(&addr) {
                            Some(p) => out.push_str(p),
                            None if b[..r.o].iter().all(|&x| x == 0) => {
                                out.push_str("(null reference)")
                            }
                            None => {
                                let _ = write!(out, "object at {addr:#x}");
                            }
                        }
                    }
                    _ => out.push_str(&hex(b)),
                }
            }
        }
    }
    Ok(())
}

/// An attribute's value as text: one value, or a list of the first few.
fn attribute_text(r: &mut Reader, a: &Attribute) -> String {
    let n = a.space.elements().unwrap_or(0);
    let mut out = String::new();
    let size = a.ty.size.max(1);
    if a.space.null {
        return "(no value)".into();
    }
    let result = if a.space.dims.is_empty() {
        text(r, &a.ty, &a.data, false, 0, &mut out)
    } else {
        (|| {
            out.push('[');
            for i in 0..n.min(ATTRIBUTE_VALUES as u64) as usize {
                if i > 0 {
                    out.push_str(", ");
                }
                text(
                    r,
                    &a.ty,
                    a.data.get(i * size..).unwrap_or_default(),
                    true,
                    0,
                    &mut out,
                )?;
            }
            if n > ATTRIBUTE_VALUES as u64 {
                let _ = write!(out, ", … ({n} values)");
            }
            out.push(']');
            Ok(())
        })()
    };
    if let Err(e) = result {
        let _ = write!(out, " (damaged: {e})");
    }
    clip(out, ATTRIBUTE_CHARS)
}

struct Attribute {
    name: String,
    ty: Dtype,
    space: Space,
    data: Vec<u8>,
}

fn parse_attribute(r: &mut Reader, data: &[u8]) -> Result<Attribute, String> {
    let mut c = Cur::new(data);
    let version = c.u8()?;
    if !(1..=3).contains(&version) {
        return Err(damaged(&format!("unknown attribute version {version}")));
    }
    let flags = c.u8()?;
    let name_len = usize::from(c.u16()?);
    let type_len = usize::from(c.u16()?);
    let space_len = usize::from(c.u16()?);
    if version == 3 {
        c.skip(1)?;
    }
    let size = |n: usize| if version == 1 { pad8(n) } else { n };
    let name = c.take(size(name_len))?;
    let name = lossy(&name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())]);
    let type_bytes = c.take(size(type_len))?;
    let space_bytes = c.take(size(space_len))?;
    let ty = if version > 1 && flags & 1 != 0 {
        let addr = shared_address(type_bytes, r.o, r.l)?;
        let header = read_header(r, addr)?;
        let m = header
            .msgs
            .iter()
            .find(|m| m.kind == 0x03 && m.flags & 0x02 == 0)
            .ok_or_else(|| damaged("a shared datatype is missing"))?;
        parse_type(&mut Cur::new(&m.data), 0)?
    } else {
        parse_type(&mut Cur::new(type_bytes), 0)?
    };
    if version > 1 && flags & 2 != 0 {
        return Err("shared dataspaces are not supported".into());
    }
    let space = parse_space(&mut Cur::new(space_bytes), r.l)?;
    let bytes = space
        .elements()
        .and_then(|n| n.checked_mul(ty.size as u64))
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| damaged("an attribute is too large"))?;
    let data = c.take(bytes)?.to_vec();
    Ok(Attribute {
        name,
        ty,
        space,
        data,
    })
}

// ----- the structure of a file -----

enum Target {
    Hard(u64),
    Soft(String),
    External(String, String),
    Other(u8),
}
struct Link {
    name: String,
    target: Target,
}

fn parse_link(data: &[u8], o: usize) -> Result<Link, String> {
    let mut c = Cur::new(data);
    if c.u8()? != 1 {
        return Err(damaged("unknown link version"));
    }
    let flags = c.u8()?;
    let kind = if flags & 0x08 != 0 { c.u8()? } else { 0 };
    if flags & 0x04 != 0 {
        c.skip(8)?;
    }
    if flags & 0x10 != 0 {
        c.skip(1)?;
    }
    let n = c.uint(1 << (flags & 3))?;
    let name = lossy(c.take(usize::try_from(n).unwrap_or(usize::MAX))?);
    let target = match kind {
        0 => Target::Hard(
            c.addr(o)?
                .ok_or_else(|| damaged("a hard link has no address"))?,
        ),
        1 => {
            let n = usize::from(c.u16()?);
            Target::Soft(lossy(c.take(n)?))
        }
        64 => {
            let n = usize::from(c.u16()?);
            let mut d = Cur::new(c.take(n)?);
            d.skip(1)?;
            let file = d.cstr(false)?;
            let object = d.cstr(false)?;
            Target::External(file, object)
        }
        k => Target::Other(k),
    };
    Ok(Link { name, target })
}

#[derive(Clone, Debug)]
struct Dset {
    path: String,
    ty: Dtype,
    space: Space,
    layout: Layout,
    filters: Vec<Filter>,
    fill: Option<Vec<u8>>,
    external: bool,
    /// Attributes (name, value text).
    attrs: Vec<(String, String)>,
    mtime: Option<String>,
    /// Why the dataset could not be read at all.
    damage: Option<String>,
}
impl Dset {
    /// Dimensions as a table: a scalar is one row, a null dataspace none.
    fn shape(&self) -> Vec<u64> {
        if self.space.null {
            vec![0]
        } else if self.space.dims.is_empty() {
            vec![1]
        } else {
            self.space.dims.clone()
        }
    }
    fn rows(&self) -> u64 {
        self.shape()[0]
    }
    fn layout_label(&self) -> String {
        match &self.layout {
            Layout::Compact(_) => "compact".into(),
            Layout::Contiguous(..) => "contiguous".into(),
            Layout::Chunked { dims, .. } => format!("chunked {}", tuple(dims)),
            Layout::Virtual => "virtual".into(),
        }
    }
    fn describe(&self) -> String {
        let mut s = format!(
            "{} {}, {}",
            self.ty.describe(),
            self.space.describe(),
            self.layout_label()
        );
        for f in &self.filters {
            let _ = write!(s, ", {}", f.label());
        }
        s
    }
    /// Why it cannot be shown as a table, if it cannot.
    fn problem(&self, limits: &Limits) -> Option<String> {
        if let Some(d) = &self.damage {
            return Some(d.clone());
        }
        if self.external {
            return Some("its data is stored in external files, which are not opened".into());
        }
        if matches!(self.layout, Layout::Virtual) {
            return Some("a virtual dataset: its data lives in other files".into());
        }
        if let Some(f) = self.filters.iter().find(|f| !f.supported()) {
            return Some(format!("filter {} is not supported", f.label()));
        }
        if self.ty.size == 0 {
            return Some("its elements have no size".into());
        }
        let shape = self.shape();
        let fields = match &self.ty.class {
            Class::Compound(m) => m.len().max(1) as u64,
            _ => 1,
        };
        if shape.len() > 2 {
            let columns = product(&shape[1..]).and_then(|n| n.checked_mul(fields));
            if columns.is_none_or(|c| c > limits.columns as u64) {
                return Some(format!(
                    "its {} dimensions flatten into more than {} columns",
                    shape.len(),
                    limits.columns
                ));
            }
        }
        if let Layout::Chunked { dims, .. } = &self.layout {
            if dims.len() != shape.len() {
                return Some(damaged("chunk and dataset dimensions differ"));
            }
            let bytes = product(dims).and_then(|n| n.checked_mul(self.ty.size as u64));
            if bytes.is_none_or(|b| b > limits.batch_bytes.min(TABLE_BATCH_BYTES) as u64) {
                return Some("its chunks are larger than the table budget".into());
            }
        }
        None
    }
}

struct Structure {
    facts: Vec<(String, String)>,
    warnings: Vec<String>,
    /// Datasets by path.
    datasets: Vec<Dset>,
    schema: String,
    paths: Arc<HashMap<u64, String>>,
    /// Chunk indexes read so far, by dataset path.
    chunks: Mutex<Vec<(String, Arc<ChunkMap>)>>,
}

enum WalkTask {
    Object(u64, String, String, usize),
    Link(Link, String, usize),
}
struct Walk<'a> {
    pending: Vec<WalkTask>,
    r: &'a mut Reader,
    visited: HashMap<u64, String>,
    datasets: Vec<Dset>,
    schema: String,
    warnings: Vec<String>,
    objects: usize,
    groups: usize,
    attributes: usize,
    attribute_bytes: usize,
    attributes_cut: bool,
}
impl Walk<'_> {
    fn warn(&mut self, w: String) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(w);
        }
    }
    fn line(&mut self, depth: usize, text: &str) {
        if self.schema.len() > MAX_SCHEMA {
            return;
        }
        for _ in 0..depth {
            self.schema.push_str("  ");
        }
        self.schema.push_str(&clip(text.replace('\n', " "), 1000));
        self.schema.push('\n');
    }

    fn attributes(&mut self, h: &Header, path: &str) -> Vec<(String, String)> {
        if self.attributes_cut {
            return Vec::new();
        }
        let mut raw = vec![];
        for m in &h.msgs {
            match m.kind {
                0x0c => raw.push(Ok(m.data.clone())),
                0x15 => {
                    let found = (|| {
                        let mut c = Cur::new(&m.data);
                        c.skip(1)?;
                        let flags = c.u8()?;
                        if flags & 1 != 0 {
                            c.skip(2)?;
                        }
                        let heap = c.addr(self.r.o)?;
                        let tree = c.addr(self.r.o)?;
                        match (heap, tree) {
                            (Some(h), Some(t)) => dense(self.r, h, t, 0),
                            _ => Ok(vec![]),
                        }
                    })();
                    match found {
                        Ok(v) => raw.extend(v.into_iter().map(Ok)),
                        Err(e) => raw.push(Err(e)),
                    }
                }
                _ => {}
            }
        }
        let mut out = vec![];
        for data in raw {
            if self.attributes >= MAX_ATTRIBUTES {
                self.attributes_cut = true;
                self.warn("Only the first 100,000 attributes are shown".into());
                break;
            }
            self.attributes += 1;
            match data.and_then(|d| parse_attribute(self.r, &d)) {
                Ok(a) => {
                    let value = attribute_text(self.r, &a);
                    self.attribute_bytes = self
                        .attribute_bytes
                        .saturating_add(a.name.len())
                        .saturating_add(value.len());
                    if self.attribute_bytes > MAX_ATTRIBUTE_TEXT {
                        self.attributes_cut = true;
                        self.warn(
                            "Attribute text exceeds 16 MiB; further attributes are not shown"
                                .into(),
                        );
                        break;
                    }
                    out.push((a.name, value));
                }
                Err(e) => self.warn(format!("{path}: an attribute could not be read ({e})")),
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn links(&mut self, h: &Header, path: &str) -> Vec<Link> {
        let o = self.r.o;
        let mut links = vec![];
        for m in &h.msgs {
            let found: Result<Vec<Link>, String> = match m.kind {
                0x06 => parse_link(&m.data, o).map(|l| vec![l]),
                0x11 => {
                    let mut c = Cur::new(&m.data);
                    match (c.addr(o), c.addr(o)) {
                        (Ok(Some(tree)), Ok(Some(heap))) => symbol_table(self.r, tree, heap),
                        _ => Err(damaged("invalid symbol table message")),
                    }
                }
                0x02 => (|| {
                    let mut c = Cur::new(&m.data);
                    c.skip(1)?;
                    let flags = c.u8()?;
                    if flags & 1 != 0 {
                        c.skip(8)?;
                    }
                    match (c.addr(o)?, c.addr(o)?) {
                        (Some(heap), Some(tree)) => dense(self.r, heap, tree, 4)?
                            .iter()
                            .map(|d| parse_link(d, o))
                            .collect(),
                        _ => Ok(vec![]),
                    }
                })(),
                _ => continue,
            };
            match found {
                Ok(l) => links.extend(l),
                Err(e) => self.warn(format!(
                    "{path}: some members of the group could not be listed ({e})"
                )),
            }
        }
        links.sort_by(|a, b| a.name.cmp(&b.name));
        links
    }

    fn visit(&mut self, addr: u64, path: &str, name: &str, depth: usize) {
        if self.objects >= MAX_OBJECTS {
            if self.objects == MAX_OBJECTS {
                self.objects += 1;
                self.warn(format!("Only the first {MAX_OBJECTS} objects are listed"));
            }
            return;
        }
        self.objects += 1;
        self.visited.insert(addr, path.to_owned());
        let header = match read_header(self.r, addr) {
            Ok(h) => h,
            Err(e) => {
                self.line(depth, &format!("{name}: damaged object ({e})"));
                self.warn(format!("{path}: {e}"));
                return;
            }
        };
        let attrs = self.attributes(&header, path);
        let has = |k: u16| header.msgs.iter().any(|m| m.kind == k);
        if has(0x08) {
            let d = dataset(self.r, &header, path, attrs);
            if let Some(e) = &d.damage {
                self.warn(format!("{path}: {e}"));
            }
            self.line(depth, &format!("{name}: dataset {}", d.describe()));
            for (k, v) in &d.attrs {
                self.line(
                    depth + 1,
                    &format!("@{k} = {}", clip(v.clone(), TREE_VALUE_CHARS)),
                );
            }
            self.datasets.push(d);
        } else if depth == 0 || has(0x11) || has(0x02) || has(0x06) {
            self.groups += 1;
            self.line(
                depth,
                &if depth == 0 {
                    "/ (root group)".to_owned()
                } else {
                    format!("{name}/")
                },
            );
            for (k, v) in &attrs {
                self.line(
                    depth + 1,
                    &format!("@{k} = {}", clip(v.clone(), TREE_VALUE_CHARS)),
                );
            }
            let links = self.links(&header, path);
            for link in links.into_iter().rev() {
                if self.pending.len() >= MAX_OBJECTS {
                    self.warn(format!(
                        "Only the first {MAX_OBJECTS} pending links are listed"
                    ));
                    break;
                }
                self.pending
                    .push(WalkTask::Link(link, path.to_owned(), depth + 1));
            }
        } else if has(0x03) {
            let ty = header
                .msgs
                .iter()
                .find(|m| m.kind == 0x03)
                .and_then(|m| parse_type(&mut Cur::new(&m.data), 0).ok())
                .map_or_else(|| "(damaged)".to_owned(), |t| t.describe());
            self.line(depth, &format!("{name}: named datatype {ty}"));
        } else {
            self.line(depth, &format!("{name}: object"));
        }
    }
}

/// Links of an old-style group: a version 1 B-tree of symbol table nodes, names in a local heap.
fn symbol_table(r: &mut Reader, tree: u64, heap: u64) -> Result<Vec<Link>, String> {
    let (o, l) = (r.o, r.l);
    let names = local_heap(r, heap)?;
    let nodes = btree1(r, tree, 0, l, MAX_OBJECTS as u64)?;
    let mut links = vec![];
    for (_, node) in nodes {
        let head = r.meta(node, 8)?;
        let mut c = Cur::new(&head);
        c.sig(b"SNOD")?;
        c.skip(2)?;
        let n = u64::from(c.u16()?);
        let entry = 2 * o + 24;
        let body = r.meta(node.saturating_add(8), n * entry as u64)?;
        let mut c = Cur::new(&body);
        for _ in 0..n {
            let name = heap_name(&names, c.uint(o)?)?;
            let addr = c.addr(o)?;
            let cache = c.u32()?;
            c.skip(4)?;
            let scratch = c.take(16)?;
            let target = if cache == 2 {
                Target::Soft(heap_name(&names, le(&scratch[..4]))?)
            } else {
                Target::Hard(addr.ok_or_else(|| damaged("a group member has no address"))?)
            };
            if links.len() >= MAX_OBJECTS {
                return Err(format!("More than {MAX_OBJECTS} members"));
            }
            links.push(Link { name, target });
        }
    }
    Ok(links)
}

fn utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

fn dataset(r: &mut Reader, h: &Header, path: &str, attrs: Vec<(String, String)>) -> Dset {
    let (o, l) = (r.o, r.l);
    let mut ty = None;
    let mut space = None;
    let mut layout = None;
    let mut filters = vec![];
    let mut fill = None;
    let mut external = false;
    let mut mtime = h.mtime.map(utc);
    let mut damage = None;
    for m in &h.msgs {
        if !matches!(
            m.kind,
            0x01 | 0x03..=0x05 | 0x07 | 0x08 | 0x0b | 0x0e | 0x12
        ) {
            continue;
        }
        let parsed = message_data(r, m).and_then(|data| {
            let mut c = Cur::new(&data);
            match m.kind {
                0x01 => space = Some(parse_space(&mut c, l)?),
                0x03 => ty = Some(parse_type(&mut c, 0)?),
                0x04 | 0x05 => fill = parse_fill(m.kind, &mut c)?,
                0x07 => external = true,
                0x08 => layout = Some(parse_layout(&mut c, o, l)?),
                0x0b => filters = parse_filters(&mut c)?,
                0x0e => mtime = Some(lossy(c.take(14)?)),
                _ => {
                    c.skip(4)?;
                    mtime = Some(utc(u64::from(c.u32()?)));
                }
            }
            Ok(())
        });
        if let Err(e) = parsed {
            damage.get_or_insert(e);
        }
    }
    let mut d = Dset {
        path: path.to_owned(),
        ty: ty.clone().unwrap_or(Dtype {
            size: 0,
            class: Class::Bytes("unknown", String::new()),
        }),
        space: space.clone().unwrap_or(Space {
            dims: vec![],
            max: None,
            null: true,
        }),
        layout: layout.clone().unwrap_or(Layout::Virtual),
        filters,
        fill,
        external,
        attrs,
        mtime,
        damage,
    };
    if d.damage.is_none() && (ty.is_none() || space.is_none() || layout.is_none()) {
        d.damage = Some(damaged("the datatype, dataspace or layout is missing"));
    }
    if d.fill.as_ref().is_some_and(|f| f.len() != d.ty.size) {
        d.fill = None;
    }
    d
}

fn walk(r: &mut Reader, sb: &Superblock) -> Structure {
    let mut w = Walk {
        pending: vec![WalkTask::Object(sb.root, "/".into(), "/".into(), 0)],
        r,
        visited: HashMap::new(),
        datasets: vec![],
        schema: String::new(),
        warnings: vec![],
        objects: 0,
        groups: 0,
        attributes: 0,
        attribute_bytes: 0,
        attributes_cut: false,
    };
    while let Some(task) = w.pending.pop() {
        if w.r.check().is_err() {
            break;
        }
        match task {
            WalkTask::Object(addr, path, name, depth) => w.visit(addr, &path, &name, depth),
            WalkTask::Link(link, path, depth) => {
                if path.len().saturating_add(link.name.len()).saturating_add(1) > 65536 {
                    w.warn("A link path exceeds 64 KiB; it is not followed".into());
                    continue;
                }
                let child = if path == "/" {
                    format!("/{}", link.name)
                } else {
                    format!("{path}/{}", link.name)
                };
                match link.target {
                    Target::Hard(a) => {
                        if let Some(first) = w.visited.get(&a).cloned() {
                            w.line(
                                depth,
                                &format!(
                                    "{} → {first} (hard link to an object already listed)",
                                    link.name
                                ),
                            );
                        } else if depth > MAX_DEPTH {
                            w.warn(format!("{child}: groups nest deeper than {MAX_DEPTH}"));
                        } else {
                            w.pending.push(WalkTask::Object(a, child, link.name, depth));
                        }
                    }
                    Target::Soft(p) => w.line(depth, &format!("{} → {p} (soft link)", link.name)),
                    Target::External(f, p) => w.line(
                        depth,
                        &format!("{} → {p} in {f} (external link, not followed)", link.name),
                    ),
                    Target::Other(k) => w.line(
                        depth,
                        &format!("{} (user-defined link type {k})", link.name),
                    ),
                }
            }
        }
    }
    if w.schema.len() > MAX_SCHEMA {
        w.schema
            .push_str("… (the rest of the structure is not shown)\n");
    }
    let mut datasets = std::mem::take(&mut w.datasets);
    datasets.sort_by(|a, b| a.path.cmp(&b.path));
    let mut facts = vec![
        (
            "Format".into(),
            format!("HDF5, superblock version {}", sb.version),
        ),
        (
            "Address and length sizes".into(),
            format!("{} and {} bytes", w.r.o, w.r.l),
        ),
        (
            "Objects".into(),
            format!(
                "{} ({} groups, {} datasets)",
                w.objects.min(MAX_OBJECTS),
                w.groups,
                datasets.len()
            ),
        ),
    ];
    if w.r.base > 0 {
        facts.push(("User block".into(), format!("{} bytes", w.r.base)));
    }
    if sb.extension {
        facts.push(("Superblock extension".into(), "present (not read)".into()));
    }
    let mut warnings = std::mem::take(&mut w.warnings);
    if let Some(eof) = sb.eof
        && eof > w.r.len
    {
        warnings.insert(
            0,
            format!(
                "The file is truncated: it should be {eof} bytes long, it is {}",
                w.r.len
            ),
        );
    }
    let paths = w.visited.iter().map(|(a, p)| (*a, p.clone())).collect();
    Structure {
        facts,
        warnings,
        datasets,
        schema: std::mem::take(&mut w.schema),
        paths: Arc::new(paths),
        chunks: Mutex::new(vec![]),
    }
}

fn open_structure(
    path: &Path,
    cancelled: Arc<AtomicBool>,
) -> Result<(Arc<Structure>, Reader), String> {
    let (mut r, sb) = superblock(path, cancelled)?;
    let s = Arc::new(walk(&mut r, &sb));
    r.check()?;
    r.budget = MAX_METADATA;
    r.paths = s.paths.clone();
    Ok((s, r))
}

/// The dataset to show: the one asked for, else the first that can be shown.
fn pick<'a>(
    s: &'a Structure,
    name: Option<&str>,
    limits: &Limits,
) -> Result<Option<&'a Dset>, String> {
    match name {
        Some(n) => {
            let d = s
                .datasets
                .iter()
                .find(|d| d.path == n || d.path.strip_prefix('/') == Some(n))
                .ok_or_else(|| format!("There is no dataset named \"{n}\""))?;
            match d.problem(limits) {
                Some(p) => Err(format!("{} cannot be shown: {p}", d.path)),
                None => Ok(Some(d)),
            }
        }
        None => Ok(s.datasets.iter().find(|d| d.problem(limits).is_none())),
    }
}

// ----- columns -----

struct Column {
    name: String,
    /// Element within the row (flattened trailing dimensions).
    k: u64,
    /// Byte offset of the field within the element.
    offset: usize,
    ty: Arc<Dtype>,
}
struct View {
    rows: u64,
    columns: Vec<Column>,
    /// Elements per row in the file.
    row_elements: u64,
    capped: Option<String>,
}

fn view(d: &Dset, limits: &Limits) -> Result<View, String> {
    let shape = d.shape();
    let rows = shape[0];
    let row_elements = product(&shape[1..]).ok_or_else(|| damaged("dimensions overflow"))?;
    let mut fields: Vec<(Option<&str>, usize, &Dtype)> = match &d.ty.class {
        Class::Compound(m) if !m.is_empty() => m
            .iter()
            .map(|m| (Some(m.name.as_str()), m.offset, &m.ty))
            .collect(),
        _ => vec![(None, 0, &d.ty)],
    };
    let mut capped = None;
    if fields.len() > limits.columns {
        capped = Some(format!(
            "Only the first {} of {} compound fields are shown",
            limits.columns,
            fields.len()
        ));
        fields.truncate(limits.columns);
    }
    let per = fields.len().max(1) as u64;
    let shown = if shape.len() <= 1 {
        1
    } else {
        let max = (limits.columns as u64 / per).max(1);
        if shape.len() == 2 && row_elements > max {
            capped = Some(format!(
                "Only the first {max} of {row_elements} columns are shown"
            ));
        } else if row_elements > max {
            return Err(format!("More than {} columns", limits.columns));
        }
        row_elements.min(max)
    };
    let types: Vec<_> = fields
        .iter()
        .map(|(_, _, ty)| Arc::new((*ty).clone()))
        .collect();
    let mut columns = vec![];
    for k in 0..shown {
        let index = if shape.len() <= 1 {
            None
        } else if shape.len() == 2 {
            Some(k.to_string())
        } else {
            let mut rest = k;
            let mut parts = vec![];
            for &n in shape[1..].iter().rev() {
                parts.push((rest % n.max(1)).to_string());
                rest /= n.max(1);
            }
            parts.reverse();
            Some(parts.join(","))
        };
        for (&(field, offset, _), ty) in fields.iter().zip(&types) {
            let name = match (&index, field) {
                (None, None) => "value".to_owned(),
                (None, Some(f)) => f.to_owned(),
                (Some(i), None) => i.clone(),
                (Some(i), Some(f)) => format!("{i}.{f}"),
            };
            columns.push(Column {
                name,
                k,
                offset,
                ty: ty.clone(),
            });
        }
    }
    Ok(View {
        rows,
        columns,
        row_elements,
        capped,
    })
}

fn arrow_type(ty: &Dtype) -> DataType {
    match (&ty.class, ty.size) {
        (Class::Int { signed: true, .. }, 1) => DataType::Int8,
        (Class::Int { signed: true, .. }, 2) => DataType::Int16,
        (Class::Int { signed: true, .. }, 4) => DataType::Int32,
        (Class::Int { signed: true, .. }, 8) => DataType::Int64,
        (Class::Int { signed: false, .. }, 1) => DataType::UInt8,
        (Class::Int { signed: false, .. }, 2) => DataType::UInt16,
        (Class::Int { signed: false, .. }, 4) => DataType::UInt32,
        (Class::Int { signed: false, .. }, 8) => DataType::UInt64,
        (Class::Float { ieee: true, .. }, 2 | 4) => DataType::Float32,
        (Class::Float { ieee: true, .. }, 8) => DataType::Float64,
        _ => DataType::Utf8,
    }
}

enum Builder {
    I8(Int8Builder),
    I16(Int16Builder),
    I32(Int32Builder),
    I64(Int64Builder),
    U8(UInt8Builder),
    U16(UInt16Builder),
    U32(UInt32Builder),
    U64(UInt64Builder),
    F32(Float32Builder),
    F64(Float64Builder),
    Text(StringBuilder),
}
impl Builder {
    fn new(t: &DataType) -> Self {
        match t {
            DataType::Int8 => Self::I8(Int8Builder::new()),
            DataType::Int16 => Self::I16(Int16Builder::new()),
            DataType::Int32 => Self::I32(Int32Builder::new()),
            DataType::Int64 => Self::I64(Int64Builder::new()),
            DataType::UInt8 => Self::U8(UInt8Builder::new()),
            DataType::UInt16 => Self::U16(UInt16Builder::new()),
            DataType::UInt32 => Self::U32(UInt32Builder::new()),
            DataType::UInt64 => Self::U64(UInt64Builder::new()),
            DataType::Float32 => Self::F32(Float32Builder::new()),
            DataType::Float64 => Self::F64(Float64Builder::new()),
            _ => Self::Text(StringBuilder::new()),
        }
    }
    fn push(&mut self, r: &mut Reader, ty: &Dtype, b: &[u8]) {
        let Some(b) = b.get(..ty.size) else {
            if let Self::Text(t) = self {
                t.append_value("(cut short)");
            }
            return;
        };
        let big = matches!(
            ty.class,
            Class::Int { big: true, .. } | Class::Float { big: true, .. }
        );
        let v = uint(b, big);
        match self {
            Self::I8(x) => x.append_value(v as i8),
            Self::I16(x) => x.append_value(v as i16),
            Self::I32(x) => x.append_value(v as i32),
            Self::I64(x) => x.append_value(v as i64),
            Self::U8(x) => x.append_value(v as u8),
            Self::U16(x) => x.append_value(v as u16),
            Self::U32(x) => x.append_value(v as u32),
            Self::U64(x) => x.append_value(v as u64),
            Self::F32(x) => x.append_value(float(b, big) as f32),
            Self::F64(x) => x.append_value(float(b, big)),
            Self::Text(x) => {
                let mut s = String::new();
                if let Err(e) = text(r, ty, b, false, 0, &mut s) {
                    let _ = write!(s, "(damaged: {e})");
                }
                x.append_value(cut(s));
            }
        }
    }
    fn finish(self) -> ArrayRef {
        match self {
            Self::I8(mut x) => Arc::new(x.finish()),
            Self::I16(mut x) => Arc::new(x.finish()),
            Self::I32(mut x) => Arc::new(x.finish()),
            Self::I64(mut x) => Arc::new(x.finish()),
            Self::U8(mut x) => Arc::new(x.finish()),
            Self::U16(mut x) => Arc::new(x.finish()),
            Self::U32(mut x) => Arc::new(x.finish()),
            Self::U64(mut x) => Arc::new(x.finish()),
            Self::F32(mut x) => Arc::new(x.finish()),
            Self::F64(mut x) => Arc::new(x.finish()),
            Self::Text(mut x) => Arc::new(x.finish()),
        }
    }
}

// ----- chunk indexes -----

#[derive(Clone, Copy, Debug)]
struct Chunk {
    addr: u64,
    size: u64,
    mask: u32,
}
enum ChunkMap {
    Empty,
    /// Chunks by their linear position in the current chunk grid (B-tree indexes).
    Keyed {
        down: Vec<u64>,
        map: HashMap<u64, Chunk>,
    },
    /// Chunks by array index (fixed and extensible arrays); `order` puts the unlimited
    /// dimension first, `down` are the strides of that order.
    Listed {
        order: Vec<usize>,
        down: Vec<u64>,
        entries: Vec<Option<Chunk>>,
    },
    Implicit {
        addr: u64,
        down: Vec<u64>,
        size: u64,
    },
    Single(Chunk),
}
impl ChunkMap {
    fn get(&self, scaled: &[u64]) -> Option<Chunk> {
        let at = |down: &[u64], order: &mut dyn Iterator<Item = usize>| {
            down.iter()
                .zip(order)
                .try_fold(0u64, |a, (d, i)| a.checked_add(d.checked_mul(scaled[i])?))
        };
        match self {
            Self::Empty => None,
            Self::Keyed { down, map } => map.get(&at(down, &mut (0..scaled.len()))?).copied(),
            Self::Listed {
                order,
                down,
                entries,
            } => {
                let i = at(down, &mut order.iter().copied())?;
                entries.get(usize::try_from(i).ok()?).copied().flatten()
            }
            Self::Implicit { addr, down, size } => {
                let i = at(down, &mut (0..scaled.len()))?;
                Some(Chunk {
                    addr: addr.checked_add(i.checked_mul(*size)?)?,
                    size: *size,
                    mask: 0,
                })
            }
            Self::Single(c) => scaled.iter().all(|&s| s == 0).then_some(*c),
        }
    }
}

/// Row-major strides of `dims`.
fn strides(dims: &[u64]) -> Vec<u64> {
    let mut down = vec![1u64; dims.len()];
    for d in (0..dims.len().saturating_sub(1)).rev() {
        down[d] = down[d + 1].saturating_mul(dims[d + 1]);
    }
    down
}

/// One fixed- or extensible-array element: a chunk address, with size and mask when filtered.
fn array_entry(b: &[u8], o: usize, filtered: bool) -> Option<Chunk> {
    let mut c = Cur::new(b);
    let addr = c.addr(o).ok()??;
    if filtered {
        let size = c.uint(b.len().checked_sub(o + 4)?).ok()?;
        Some(Chunk {
            addr,
            size,
            mask: c.u32().ok()?,
        })
    } else {
        Some(Chunk {
            addr,
            size: 0,
            mask: 0,
        })
    }
}

fn fixed_array(r: &mut Reader, addr: u64) -> Result<Vec<Option<Chunk>>, String> {
    let (o, l) = (r.o, r.l);
    let head = r.meta(addr, (12 + l + o) as u64)?;
    let mut c = Cur::new(&head);
    c.sig(b"FAHD")?;
    c.skip(1)?;
    let filtered = c.u8()? == 1;
    let size = usize::from(c.u8()?);
    let page_bits = c.u8()?;
    let n = c.uint(l)?;
    let block = c.addr(o)?;
    if size < o + if filtered { 5 } else { 0 } || page_bits > 32 {
        return Err(damaged("invalid fixed array header"));
    }
    if n > MAX_CHUNKS {
        return Err(format!("More than {MAX_CHUNKS} chunks"));
    }
    let Some(block) = block else {
        return Ok(vec![None; n as usize]);
    };
    let page = 1u64 << page_bits;
    let prefix = 6 + o as u64;
    let mut out = Vec::with_capacity(n as usize);
    if n <= page {
        let b = r.meta(block, prefix + n * size as u64 + 4)?;
        if &b[..4] != b"FADB" {
            return Err(damaged("a fixed array block is missing"));
        }
        for i in 0..n as usize {
            let at = prefix as usize + i * size;
            out.push(array_entry(&b[at..at + size], o, filtered));
        }
    } else {
        let pages = n.div_ceil(page);
        let bitmap = pages.div_ceil(8);
        let b = r.meta(block, prefix + bitmap)?;
        if &b[..4] != b"FADB" {
            return Err(damaged("a fixed array block is missing"));
        }
        let mut at = block.saturating_add(prefix + bitmap + 4);
        for p in 0..pages {
            let count = page.min(n - p * page);
            let bytes = count * size as u64 + 4;
            let set = b[(prefix + p / 8) as usize] & (0x80 >> (p % 8)) != 0;
            if set {
                let pb = r.meta(at, bytes)?;
                for i in 0..count as usize {
                    out.push(array_entry(&pb[i * size..(i + 1) * size], o, filtered));
                }
            } else {
                out.extend(std::iter::repeat_n(None, count as usize));
            }
            at = at.saturating_add(bytes);
        }
    }
    Ok(out)
}

fn extensible_array(r: &mut Reader, addr: u64) -> Result<Vec<Option<Chunk>>, String> {
    let (o, l) = (r.o, r.l);
    let head = r.meta(addr, (16 + 6 * l + o) as u64)?;
    let mut c = Cur::new(&head);
    c.sig(b"EAHD")?;
    c.skip(1)?;
    let filtered = c.u8()? == 1;
    let size = usize::from(c.u8()?);
    let max_bits = u32::from(c.u8()?);
    let index_elements = u64::from(c.u8()?);
    let block_min = u64::from(c.u8()?);
    let super_min = u64::from(c.u8()?);
    let page_bits = u32::from(c.u8()?);
    c.skip(4 * l)?;
    let n = c.uint(l)?;
    c.skip(l)?;
    let index = c.addr(o)?;
    if size < o + if filtered { 5 } else { 0 }
        || !(1..=64).contains(&max_bits)
        || !block_min.is_power_of_two()
        || !super_min.is_power_of_two()
        || page_bits > 32
        || log2(block_min) > max_bits
    {
        return Err(damaged("invalid extensible array header"));
    }
    if n > MAX_CHUNKS {
        return Err(format!("More than {MAX_CHUNKS} chunks"));
    }
    let mut out: Vec<Option<Chunk>> = Vec::with_capacity(n as usize);
    let Some(index) = index else {
        return Ok(vec![None; n as usize]);
    };
    let supers = (1 + max_bits - log2(block_min)) as usize;
    let index_supers = 2 * log2(super_min) as usize;
    let index_blocks = 2 * (super_min as usize - 1);
    let super_addrs = supers.saturating_sub(index_supers);
    let offset_size = max_bits.div_ceil(8) as usize;
    let b = r.meta(
        index,
        (6 + o) as u64
            + index_elements * size as u64
            + (index_blocks + super_addrs) as u64 * o as u64
            + 4,
    )?;
    let mut c = Cur::new(&b);
    c.sig(b"EAIB")?;
    c.skip(2 + o)?;
    for _ in 0..index_elements {
        let e = c.take(size)?;
        if (out.len() as u64) < n {
            out.push(array_entry(e, o, filtered));
        }
    }
    let blocks: Vec<Option<u64>> = (0..index_blocks)
        .map(|_| c.addr(o))
        .collect::<Result<_, _>>()?;
    let super_blocks: Vec<Option<u64>> = (0..super_addrs)
        .map(|_| c.addr(o))
        .collect::<Result<_, _>>()?;
    let page = 1u64 << page_bits;
    let mut direct = blocks.into_iter();
    for s in 0..supers {
        if out.len() as u64 >= n {
            break;
        }
        let count = 1u64 << (s / 2);
        let elements = (1u64 << s.div_ceil(2)).saturating_mul(block_min);
        let pages = if elements > page { elements / page } else { 0 };
        // Data block addresses (and which pages are initialised) for this super block.
        let (addrs, bitmap): (Vec<Option<u64>>, Vec<u8>) = if s < index_supers {
            (
                (0..count).map(|_| direct.next().flatten()).collect(),
                vec![],
            )
        } else {
            match super_blocks.get(s - index_supers).copied().flatten() {
                None => (vec![None; count.min(n) as usize], vec![]),
                Some(a) => {
                    let map = if pages > 0 {
                        count.saturating_mul(pages.div_ceil(8))
                    } else {
                        0
                    };
                    let sb = r.meta(
                        a,
                        ((6 + o + offset_size + 4) as u64)
                            .saturating_add(map)
                            .saturating_add(count.saturating_mul(o as u64)),
                    )?;
                    let mut c = Cur::new(&sb);
                    c.sig(b"EASB")?;
                    c.skip(2 + o + offset_size)?;
                    let bitmap = c.take(map as usize)?.to_vec();
                    let addrs = (0..count).map(|_| c.addr(o)).collect::<Result<_, _>>()?;
                    (addrs, bitmap)
                }
            }
        };
        for (j, block) in addrs.into_iter().enumerate() {
            let want = elements.min(n - out.len() as u64);
            if want == 0 {
                break;
            }
            let Some(block) = block else {
                out.extend(std::iter::repeat_n(None, want as usize));
                continue;
            };
            let prefix = (6 + o + offset_size) as u64;
            if pages == 0 {
                let b = r.meta(block, prefix + want * size as u64)?;
                if &b[..4] != b"EADB" {
                    return Err(damaged("an extensible array block is missing"));
                }
                for i in 0..want as usize {
                    let at = prefix as usize + i * size;
                    out.push(array_entry(&b[at..at + size], o, filtered));
                }
            } else {
                let per = pages.div_ceil(8) as usize;
                let mut at = block.saturating_add(prefix + 4);
                for p in 0..pages {
                    let left = n - out.len() as u64;
                    if left == 0 {
                        break;
                    }
                    let count = page.min(left);
                    let set = bitmap.is_empty()
                        || bitmap
                            .get(j.saturating_mul(per).saturating_add((p / 8) as usize))
                            .is_some_and(|&x| x & (0x80 >> (p % 8)) != 0);
                    if set {
                        let pb = r.meta(at, count * size as u64)?;
                        for i in 0..count as usize {
                            out.push(array_entry(&pb[i * size..(i + 1) * size], o, filtered));
                        }
                    } else {
                        out.extend(std::iter::repeat_n(None, count as usize));
                    }
                    at = at.saturating_add(page * size as u64 + 4);
                }
            }
        }
    }
    Ok(out)
}

fn chunk_map(r: &mut Reader, d: &Dset) -> Result<ChunkMap, String> {
    let Layout::Chunked {
        dims: chunk, index, ..
    } = &d.layout
    else {
        return Ok(ChunkMap::Empty);
    };
    let o = r.o;
    let shape = d.shape();
    let rank = chunk.len();
    let grid: Vec<u64> = shape
        .iter()
        .zip(chunk)
        .map(|(&n, &c)| n.div_ceil(c))
        .collect();
    let bytes = product(chunk).unwrap_or(0).saturating_mul(d.ty.size as u64);
    // Strides by maximum dimensions (fixed arrays, implicit index), the unlimited one first
    // (extensible arrays).
    let max = d.space.max.clone().unwrap_or_else(|| shape.clone());
    let unlimited = max.iter().position(|&m| m == u64::MAX).unwrap_or(0);
    let max_grid = |order: &[usize]| -> Vec<u64> {
        let dims: Vec<u64> = order
            .iter()
            .map(|&i| max.get(i).copied().unwrap_or(1).div_ceil(chunk[i]).max(1))
            .collect();
        strides(&dims)
    };
    let natural: Vec<usize> = (0..rank).collect();
    let in_grid = |scaled: &[u64]| scaled.iter().zip(&grid).all(|(s, g)| s < g);
    Ok(match index {
        Index::BTree1(None)
        | Index::Single(None, _)
        | Index::Implicit(None)
        | Index::Fixed(None)
        | Index::Extensible(None)
        | Index::BTree2(None) => ChunkMap::Empty,
        Index::BTree1(Some(a)) => {
            let down = strides(&grid);
            let mut map = HashMap::new();
            for (key, addr) in btree1(r, *a, 1, 8 + 8 * (rank + 1), MAX_CHUNKS)? {
                let mut c = Cur::new(&key);
                let size = u64::from(c.u32()?);
                let mask = c.u32()?;
                let mut scaled = Vec::with_capacity(rank);
                for &cd in chunk {
                    let off = c.uint(8)?;
                    if off % cd != 0 {
                        return Err(damaged("a chunk is not aligned"));
                    }
                    scaled.push(off / cd);
                }
                if in_grid(&scaled) {
                    let key = scaled
                        .iter()
                        .zip(&down)
                        .fold(0u64, |k, (s, d)| k.saturating_add(s.saturating_mul(*d)));
                    map.insert(key, Chunk { addr, size, mask });
                }
            }
            ChunkMap::Keyed { down, map }
        }
        Index::BTree2(Some(a)) => {
            let down = strides(&grid);
            let (kind, rec, records) = btree2(r, *a, MAX_CHUNKS)?;
            let size_len = match kind {
                10 => 0,
                11 => rec
                    .checked_sub(o + 4 + 8 * rank)
                    .filter(|n| (1..=8).contains(n))
                    .ok_or_else(|| damaged("invalid chunk record size"))?,
                _ => return Err(damaged("a chunk index has the wrong type")),
            };
            let mut map = HashMap::new();
            for record in records {
                let mut c = Cur::new(&record);
                let Some(addr) = c.addr(o)? else { continue };
                let (size, mask) = if kind == 11 {
                    (c.uint(size_len)?, c.u32()?)
                } else {
                    (bytes, 0)
                };
                let scaled: Vec<u64> = (0..rank).map(|_| c.uint(8)).collect::<Result<_, _>>()?;
                if in_grid(&scaled) {
                    let key = scaled
                        .iter()
                        .zip(&down)
                        .fold(0u64, |k, (s, d)| k.saturating_add(s.saturating_mul(*d)));
                    map.insert(key, Chunk { addr, size, mask });
                }
            }
            ChunkMap::Keyed { down, map }
        }
        Index::Fixed(Some(a)) => ChunkMap::Listed {
            down: max_grid(&natural),
            order: natural,
            entries: fixed_array(r, *a)?,
        },
        Index::Extensible(Some(a)) => {
            let mut order = vec![unlimited];
            order.extend((0..rank).filter(|&i| i != unlimited));
            ChunkMap::Listed {
                down: max_grid(&order),
                order,
                entries: extensible_array(r, *a)?,
            }
        }
        Index::Implicit(Some(a)) => ChunkMap::Implicit {
            addr: *a,
            down: max_grid(&natural),
            size: bytes,
        },
        Index::Single(Some(a), filtered) => ChunkMap::Single(Chunk {
            addr: *a,
            size: filtered.map_or(bytes, |f| f.0),
            mask: filtered.map_or(0, |f| f.1),
        }),
    })
}

fn cached_chunk_map(s: &Structure, r: &mut Reader, d: &Dset) -> Result<Arc<ChunkMap>, String> {
    if let Some(m) = s
        .chunks
        .lock()
        .ok()
        .and_then(|c| c.iter().find(|(p, _)| *p == d.path).map(|(_, m)| m.clone()))
    {
        return Ok(m);
    }
    let m = Arc::new(chunk_map(r, d)?);
    if let Ok(mut c) = s.chunks.lock() {
        c.push((d.path.clone(), m.clone()));
        if c.len() > 8 {
            c.remove(0);
        }
    }
    Ok(m)
}

// ----- reading data -----

/// Undo the filters of one chunk (in reverse order, skipping those its mask marks as not
/// applied); no stage may produce more than `cap` bytes.
fn unfilter(
    mut data: Vec<u8>,
    mask: u32,
    filters: &[Filter],
    element: usize,
    cap: usize,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let check = || {
        if cancelled.load(Ordering::Relaxed) {
            Err("cancelled".to_owned())
        } else {
            Ok(())
        }
    };
    let bounded = |reader: &mut dyn Read| -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            check()?;
            let len = buffer.len().min(cap.saturating_sub(out.len()) + 1);
            let n = reader
                .read(&mut buffer[..len])
                .map_err(|e| damaged(&format!("a chunk cannot be decompressed ({e})")))?;
            if n == 0 {
                return Ok(out);
            }
            if out.len() + n > cap {
                return Err(damaged(&format!("a chunk expands past its {cap} bytes")));
            }
            out.extend_from_slice(&buffer[..n]);
        }
    };
    for (i, f) in filters.iter().enumerate().rev() {
        check()?;
        if i < 32 && mask & (1 << i) != 0 {
            continue;
        }
        data = match f.id {
            1 => bounded(&mut flate2::read::ZlibDecoder::new(data.as_slice()))?,
            2 => {
                let size = f.params.first().map_or(element, |&s| s as usize).max(1);
                let n = data.len() / size;
                let mut out = data.clone();
                for (j, &x) in data[..n * size].iter().enumerate() {
                    if j % 65536 == 0 {
                        check()?;
                    }
                    out[(j % n) * size + j / n] = x;
                }
                out
            }
            3 => {
                if data.len() < 4 {
                    return Err(damaged("a chunk is shorter than its checksum"));
                }
                data.truncate(data.len() - 4);
                data
            }
            32015 => {
                let mut z = zstd::stream::read::Decoder::new(data.as_slice())
                    .map_err(|e| damaged(&format!("a chunk cannot be decompressed ({e})")))?;
                z.window_log_max(28).map_err(io)?;
                bounded(&mut z)?
            }
            _ => return Err(format!("filter {} is not supported", f.label())),
        };
    }
    Ok(data)
}

/// Fills a destination from a byte offset in a block (a chunk, compact or contiguous data).
type Source<'a> = dyn FnMut(u64, &mut [u8]) -> Result<(), String> + 'a;

/// Selection in dataset element coordinates.
struct Selection {
    start: Vec<u64>,
    count: Vec<u64>,
    element: u64,
}

/// Copy the part of a block of `shape` elements at `at` that the selection covers into `out`
/// (the selection in row-major order); `read(byte offset in the block, destination)`.
fn copy_block(
    sel: &Selection,
    at: &[u64],
    shape: &[u64],
    out: &mut [u8],
    read: &mut Source,
) -> Result<(), String> {
    let rank = at.len();
    if rank == 0 || shape.len() != rank || sel.start.len() != rank || sel.count.len() != rank {
        return Err(damaged("selection dimensions differ"));
    }
    let mut lo = vec![0u64; rank];
    let mut hi = vec![0u64; rank];
    let mut block_end = vec![0u64; rank];
    let mut selection_end = vec![0u64; rank];
    for d in 0..rank {
        block_end[d] = at[d]
            .checked_add(shape[d])
            .ok_or_else(|| damaged("block dimensions overflow"))?;
        selection_end[d] = sel.start[d]
            .checked_add(sel.count[d])
            .ok_or_else(|| damaged("selection dimensions overflow"))?;
        lo[d] = at[d].max(sel.start[d]);
        hi[d] = block_end[d].min(selection_end[d]);
        if lo[d] >= hi[d] {
            return Ok(());
        }
    }
    let whole = |d: usize| {
        lo[d] == at[d]
            && hi[d] == block_end[d]
            && lo[d] == sel.start[d]
            && hi[d] == selection_end[d]
    };
    let mut k = rank - 1;
    while k > 0 && whole(k) {
        k -= 1;
    }
    let run = (k..rank)
        .try_fold(sel.element, |n, d| n.checked_mul(hi[d] - lo[d]))
        .ok_or_else(|| damaged("selection byte size overflows"))?;
    let run = usize::try_from(run).map_err(|_| damaged("selection is too large"))?;
    let src_down = strides(shape);
    let dst_down = strides(&sel.count);
    let mut p = lo.clone();
    loop {
        let src = (0..rank)
            .try_fold(0u64, |n, d| {
                n.checked_add((p[d] - at[d]).checked_mul(src_down[d])?)
            })
            .and_then(|n| n.checked_mul(sel.element))
            .ok_or_else(|| damaged("source offset overflows"))?;
        let dst = (0..rank)
            .try_fold(0u64, |n, d| {
                n.checked_add((p[d] - sel.start[d]).checked_mul(dst_down[d])?)
            })
            .and_then(|n| n.checked_mul(sel.element))
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| damaged("page offset overflows"))?;
        let end = dst
            .checked_add(run)
            .ok_or_else(|| damaged("page offset overflows"))?;
        let target = out
            .get_mut(dst..end)
            .ok_or_else(|| damaged("a selection lies outside its buffer"))?;
        read(src, target)?;
        let mut d = k;
        loop {
            if d == 0 {
                return Ok(());
            }
            d -= 1;
            p[d] += 1;
            if p[d] < hi[d] {
                break;
            }
            p[d] = lo[d];
        }
    }
}

/// The raw elements of a selection, fill values where nothing is stored.
fn read_selection(
    s: &Structure,
    r: &mut Reader,
    d: &Dset,
    sel: &Selection,
    limits: &Limits,
) -> Result<Vec<u8>, String> {
    let shape = d.shape();
    let element = d.ty.size;
    let n = product(&sel.count).unwrap_or(u64::MAX);
    let bytes = n
        .checked_mul(element as u64)
        .filter(|&b| b <= limits.batch_bytes as u64)
        .ok_or("The selected rows exceed the table budget")? as usize;
    let mut out = match &d.fill {
        Some(f) if f.iter().any(|&b| b != 0) => f.repeat(n as usize),
        _ => vec![0; bytes],
    };
    let total = product(&shape)
        .and_then(|n| n.checked_mul(element as u64))
        .ok_or_else(|| damaged("dimensions overflow"))?;
    let origin = vec![0u64; shape.len()];
    match &d.layout {
        Layout::Compact(data) => {
            if (data.len() as u64) < total {
                return Err(damaged("compact data is cut short"));
            }
            copy_block(sel, &origin, &shape, &mut out, &mut |off, dst| {
                let src = data
                    .get(off as usize..off as usize + dst.len())
                    .ok_or_else(|| damaged("compact data is cut short"))?;
                dst.copy_from_slice(src);
                Ok(())
            })?;
        }
        Layout::Contiguous(None, _) => {}
        Layout::Contiguous(Some(addr), size) => {
            if size.is_some_and(|s| s < total) {
                return Err(damaged("contiguous data is smaller than the dataset"));
            }
            r.locate(*addr, total)?;
            copy_block(sel, &origin, &shape, &mut out, &mut |off, dst| {
                r.read_into(addr.saturating_add(off), dst)
            })?;
        }
        Layout::Chunked {
            dims: chunk,
            unfiltered_edges,
            ..
        } => {
            let map = cached_chunk_map(s, r, d)?;
            let chunk_bytes = product(chunk)
                .and_then(|n| n.checked_mul(element as u64))
                .ok_or_else(|| damaged("chunk dimensions overflow"))?;
            let rank = chunk.len();
            let lo: Vec<u64> = (0..rank).map(|i| sel.start[i] / chunk[i]).collect();
            let hi: Vec<u64> = (0..rank)
                .map(|i| (sel.start[i] + sel.count[i] - 1) / chunk[i])
                .collect();
            let chunks = lo
                .iter()
                .zip(&hi)
                .try_fold(1u64, |a, (l, h)| a.checked_mul(h - l + 1));
            if chunks.is_none_or(|c| c > MAX_CHUNKS) {
                return Err(format!("These rows span more than {MAX_CHUNKS} chunks"));
            }
            let mut work = 0u64;
            let mut p = lo.clone();
            loop {
                if let Some(c) = map.get(&p) {
                    let at: Vec<u64> = (0..rank).map(|i| p[i] * chunk[i]).collect();
                    let edge =
                        (0..rank).any(|i| at[i].checked_add(chunk[i]).is_none_or(|n| n > shape[i]));
                    let filtered = !(d.filters.is_empty() || edge && *unfiltered_edges);
                    let stored = if filtered { c.size } else { chunk_bytes };
                    work = work.saturating_add(stored).saturating_add(chunk_bytes);
                    if work > MAX_PAGE_WORK {
                        return Err(
                            "These rows need too much data; select fewer rows or columns".into(),
                        );
                    }
                    if stored > limits.batch_bytes.min(TABLE_BATCH_BYTES) as u64 {
                        return Err(damaged("a chunk is larger than the table budget"));
                    }
                    let raw = r.raw(c.addr, stored)?;
                    let data = if filtered {
                        unfilter(
                            raw,
                            c.mask,
                            &d.filters,
                            element,
                            chunk_bytes as usize + 16,
                            &r.cancelled,
                        )?
                    } else {
                        raw
                    };
                    if (data.len() as u64) < chunk_bytes {
                        return Err(damaged("a chunk is cut short"));
                    }
                    copy_block(sel, &at, chunk, &mut out, &mut |off, dst| {
                        let offset = usize::try_from(off)
                            .map_err(|_| damaged("chunk offset is too large"))?;
                        let end = offset
                            .checked_add(dst.len())
                            .ok_or_else(|| damaged("chunk offset overflows"))?;
                        let source = data
                            .get(offset..end)
                            .ok_or_else(|| damaged("chunk data is cut short"))?;
                        dst.copy_from_slice(source);
                        Ok(())
                    })?;
                }
                let mut i = rank;
                loop {
                    if i == 0 {
                        return Ok(out);
                    }
                    i -= 1;
                    p[i] += 1;
                    if p[i] <= hi[i] {
                        break;
                    }
                    p[i] = lo[i];
                }
            }
        }
        Layout::Virtual => return Err("Virtual datasets are not supported".into()),
    }
    Ok(out)
}

// ----- the table path -----

fn schema(
    s: &Structure,
    r: &mut Reader,
    table: Option<&str>,
    limits: &Limits,
) -> Result<Schema, String> {
    let chosen = pick(s, table, limits)?;
    let tables = s
        .datasets
        .iter()
        .map(|d| TableInfo {
            name: d.path.clone(),
            rows: Some(d.rows()),
        })
        .collect();
    let mut key_value: Vec<(String, String)> = s
        .warnings
        .iter()
        .map(|w| ("Warning".to_owned(), w.clone()))
        .collect();
    key_value.extend(s.facts.iter().cloned());
    let mut fields = vec![];
    let mut rows = 0;
    match chosen {
        None => {
            let reason = if s.datasets.is_empty() {
                "The file has no datasets"
            } else {
                "No dataset of this file can be shown"
            };
            key_value.insert(0, ("Warning".into(), reason.into()));
        }
        Some(d) => {
            let v = view(d, limits)?;
            if let Some(c) = &v.capped {
                key_value.insert(0, ("Warning".into(), c.clone()));
            }
            rows = v.rows;
            fields = v
                .columns
                .iter()
                .map(|c| TableColumn {
                    name: c.name.clone(),
                    type_name: c.ty.describe(),
                })
                .collect();
            key_value.push(("Dataset".into(), d.path.clone()));
            key_value.push(("Datatype".into(), d.ty.describe()));
            key_value.push(("Shape".into(), d.space.describe()));
            if let Some(max) = &d.space.max
                && *max != d.space.dims
            {
                key_value.push(("Maximum shape".into(), tuple(max)));
            }
            key_value.push(("Layout".into(), d.layout_label()));
            if let Layout::Chunked { index, .. } = &d.layout {
                let name = match index {
                    Index::BTree1(_) => "version 1 B-tree",
                    Index::Single(..) => "single chunk",
                    Index::Implicit(_) => "implicit",
                    Index::Fixed(_) => "fixed array",
                    Index::Extensible(_) => "extensible array",
                    Index::BTree2(_) => "version 2 B-tree",
                };
                key_value.push(("Chunk index".into(), name.into()));
            }
            if !d.filters.is_empty() {
                key_value.push((
                    "Filters".into(),
                    d.filters
                        .iter()
                        .map(Filter::label)
                        .collect::<Vec<_>>()
                        .join(", "),
                ));
            }
            if let Some(fill) = &d.fill {
                let mut t = String::new();
                if text(r, &d.ty, fill, false, 0, &mut t).is_ok() {
                    key_value.push(("Fill value".into(), clip(t, ATTRIBUTE_CHARS)));
                }
            }
            if let Some(m) = &d.mtime {
                key_value.push(("Modified".into(), m.clone()));
            }
            for (k, v) in &d.attrs {
                key_value.push((format!("@{k}"), v.clone()));
            }
        }
    }
    for d in &s.datasets {
        let description = match d.problem(limits) {
            Some(problem) => format!("{}; not shown: {problem}", d.describe()),
            None => d.describe(),
        };
        key_value.push((format!("Dataset {}", d.path), description));
    }
    Ok(Schema {
        rows: Some(rows),
        columns: fields,
        text: Some(s.schema.clone()),
        details: key_value,
        tables,
        table: chosen
            .and_then(|d| {
                s.datasets
                    .iter()
                    .position(|candidate| candidate.path == d.path)
            })
            .unwrap_or(0),
    })
}

/// Batches of `columns` (ascending, deduplicated, as the Parquet path expects) for rows
/// `[first_row, first_row + count)` of `table`. Only the chunks (or the stretch of contiguous
/// data) holding those rows are read.
#[allow(clippy::too_many_arguments)]
fn batch(
    s: &Structure,
    r: &mut Reader,
    d: &Dset,
    v: &View,
    columns: &[usize],
    first_row: u64,
    count: usize,
    limits: &Limits,
) -> Result<RecordBatch, String> {
    let mut projection = columns.to_vec();
    projection.sort_unstable();
    projection.dedup();
    if let Some(c) = projection.iter().find(|&&c| c >= v.columns.len()) {
        return Err(format!("Column {c} does not exist"));
    }
    let schema = Arc::new(ArrowSchema::new(
        projection
            .iter()
            .map(|&c| Field::new(&v.columns[c].name, arrow_type(&v.columns[c].ty), true))
            .collect::<Vec<_>>(),
    ));
    let shape = d.shape();
    let first = first_row.min(v.rows);
    let mut end = first_row.saturating_add(count as u64).min(v.rows);
    // Elements of each row to read: for 2-D data only the span of the chosen columns.
    let (k0, k1) = match (shape.len(), projection.first(), projection.last()) {
        (2, Some(&a), Some(&b)) => (v.columns[a].k, v.columns[b].k + 1),
        _ => (0, v.row_elements),
    };
    let element = d.ty.size as u64;
    let row_bytes = (k1 - k0).saturating_mul(element).max(1);
    if row_bytes > limits.batch_bytes as u64 {
        return Err("A row is larger than the table budget; select fewer columns".into());
    }
    end = end.min(first.saturating_add((limits.batch_bytes as u64 / row_bytes).max(1)));
    let n = end.saturating_sub(first);
    if n > 100_000
        || n.checked_mul(projection.len() as u64)
            .is_none_or(|cells| cells > 2_000_000)
    {
        return Err(
            "The HDF5 page exceeds 100,000 rows or 2,000,000 cells; request fewer rows".into(),
        );
    }
    let mut builders: Vec<Builder> = projection
        .iter()
        .map(|&c| Builder::new(&arrow_type(&v.columns[c].ty)))
        .collect();
    if n > 0 && !projection.is_empty() {
        let mut start = vec![first];
        let mut counts = vec![n];
        if shape.len() == 2 {
            start.push(k0);
            counts.push(k1 - k0);
        } else {
            start.extend(std::iter::repeat_n(0, shape.len().saturating_sub(1)));
            counts.extend(shape.iter().skip(1));
        }
        let sel = Selection {
            start,
            count: counts,
            element,
        };
        let buf = read_selection(s, r, d, &sel, limits)?;
        let per_row = (k1 - k0) as usize * d.ty.size;
        let mut text_bytes = 0usize;
        for row in 0..n as usize {
            r.check()?;
            for (b, &c) in builders.iter_mut().zip(&projection) {
                let col = &v.columns[c];
                let at = row * per_row + (col.k - k0) as usize * d.ty.size + col.offset;
                let before = match b {
                    Builder::Text(text) => text.values_slice().len(),
                    _ => 0,
                };
                b.push(r, &col.ty, buf.get(at..).unwrap_or_default());
                if let Builder::Text(text) = b {
                    text_bytes += text.values_slice().len() - before;
                }
                if text_bytes > TABLE_BATCH_BYTES {
                    return Err("The HDF5 page text exceeds 256 MiB; request fewer rows".into());
                }
            }
        }
    }
    let arrays: Vec<ArrayRef> = builders.into_iter().map(Builder::finish).collect();
    let batch = RecordBatch::try_new_with_options(
        schema,
        arrays,
        &RecordBatchOptions::new().with_row_count(Some(n as usize)),
    )
    .map_err(|e| e.to_string())?;
    Ok(batch)
}

pub(super) struct Hdf5 {
    structure: Arc<Structure>,
    reader: Reader,
    schema: Schema,
    limits: Limits,
    view: Option<View>,
    stamp: std::fs::Metadata,
}
impl Hdf5 {
    pub(super) fn open(
        path: &Path,
        table: Option<&str>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        Self::with_limits(path, table, Limits::default(), cancelled)
    }
    fn with_limits(
        path: &Path,
        table: Option<&str>,
        limits: Limits,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let (structure, mut reader) = open_structure(path, cancelled)?;
        let schema = schema(&structure, &mut reader, table, &limits)?;
        let view = pick(&structure, table, &limits)?
            .map(|d| view(d, &limits))
            .transpose()?;
        let stamp = reader.file.metadata().map_err(io)?;
        Ok(Self {
            structure,
            reader,
            schema,
            limits,
            view,
            stamp,
        })
    }
    fn page(
        &mut self,
        columns: &[usize],
        start: u64,
        count: usize,
    ) -> Result<Vec<Vec<Option<String>>>, String> {
        self.reader.check()?;
        let stamp = self.reader.file.metadata().map_err(io)?;
        if stamp.len() != self.stamp.len() || stamp.modified().ok() != self.stamp.modified().ok() {
            return Err("The HDF5 file changed; reopen it before reading more rows".into());
        }
        let Some(view) = &self.view else {
            return Ok(Vec::new());
        };
        let d = self
            .structure
            .datasets
            .get(self.schema.table)
            .ok_or("No dataset selected")?;
        self.reader.budget = MAX_METADATA;
        let batch = batch(
            &self.structure,
            &mut self.reader,
            d,
            view,
            columns,
            start,
            count,
            &self.limits,
        )?;
        let options = FormatOptions::default();
        let formatters = batch
            .columns()
            .iter()
            .map(|a| ArrayFormatter::try_new(a.as_ref(), &options))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        let mut chars = 0usize;
        for row in 0..batch.num_rows() {
            self.reader.check()?;
            let cells = formatters
                .iter()
                .map(|f| cut(f.value(row).to_string()))
                .map(|s| {
                    chars = chars.saturating_add(s.len());
                    Some(s)
                })
                .collect();
            if chars > TABLE_BATCH_BYTES {
                return Err("The displayed page exceeds 256 MiB; request fewer rows".into());
            }
            rows.push(cells);
        }
        self.reader.check()?;
        Ok(rows)
    }
}
impl TableSource for Hdf5 {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn rows(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        let columns: Vec<_> = (0..self.schema.columns.len()).collect();
        self.page(&columns, start, count)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
