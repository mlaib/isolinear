//! ESRI Shapefile (ESRI white paper, July 1998): the `.shp` shapes, with the attribute table
//! from the `.dbf` (dBASE III/IV), the record index `.shx`, the coordinate system `.prj` (WKT)
//! and the code page `.cpg` of the same name. These sidecars are the only other files read,
//! each bounded; a missing one gives a note. A `.dbf` opened alone shows its table, a `.shx`
//! opened alone explains what it indexes.
use super::geo::{Bounds, Map, Parts, TypeCounts, Wkt, epsg_label, num, wkt};
use crate::inspect::{
    Handler, Input, Inspection, MAX_INPUT, MAX_ROWS, Probe, Row, Stage, State, Table,
};
use std::path::{Path, PathBuf};

const FILE_CODE: i32 = 9994;
/// Largest `.prj` or `.cpg` read.
const SMALL_SIDECAR: u64 = 64 * 1024;
/// Records read from a `.shp` (each takes at least 12 bytes, so 256 MiB holds fewer).
const MAX_RECORDS: usize = 50_000_000;

fn be32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn le32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn le64f(b: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The main file header of a `.shp` or `.shx` (100 bytes).
struct Header {
    /// Length the header declares, in bytes.
    length: u64,
    shape_type: i32,
    bbox: [f64; 4],
    z: [f64; 2],
    m: [f64; 2],
}
fn header(b: &[u8]) -> Option<Header> {
    if b.len() < 100 || be32(b, 0)? != FILE_CODE || le32(b, 28)? != 1000 {
        return None;
    }
    let f = |at| le64f(b, at).unwrap_or(f64::NAN);
    Some(Header {
        length: u64::from(be32(b, 24)?.max(0) as u32) * 2,
        shape_type: le32(b, 32)?,
        bbox: [f(36), f(44), f(52), f(60)],
        z: [f(68), f(76)],
        m: [f(84), f(92)],
    })
}

pub fn shape_name(code: i32) -> &'static str {
    match code {
        0 => "Null",
        1 => "Point",
        3 => "PolyLine",
        5 => "Polygon",
        8 => "MultiPoint",
        11 => "PointZ",
        13 => "PolyLineZ",
        15 => "PolygonZ",
        18 => "MultiPointZ",
        21 => "PointM",
        23 => "PolyLineM",
        25 => "PolygonM",
        28 => "MultiPointM",
        31 => "MultiPatch",
        _ => "unknown",
    }
}

/// One shape record's content: its type and parts (x/y only; Z and M are skipped).
fn shape(c: &[u8]) -> Result<(i32, Parts), String> {
    let cut = || "a record is shorter than its shape".to_owned();
    let kind = le32(c, 0).ok_or_else(cut)?;
    let mut parts = Parts::default();
    let point = |at: usize| -> Option<[f64; 2]> { Some([le64f(c, at)?, le64f(c, at + 8)?]) };
    match kind {
        0 => {}
        1 | 11 | 21 => parts.points.push(point(4).ok_or_else(cut)?),
        8 | 18 | 28 => {
            let n = le32(c, 36).ok_or_else(cut)?.max(0) as usize;
            if n.saturating_mul(16) > c.len().saturating_sub(40) {
                return Err(cut());
            }
            for i in 0..n {
                parts.points.push(point(40 + i * 16).ok_or_else(cut)?);
            }
        }
        3 | 5 | 13 | 15 | 23 | 25 | 31 => {
            let np = le32(c, 36).ok_or_else(cut)?.max(0) as usize;
            let n = le32(c, 40).ok_or_else(cut)?.max(0) as usize;
            // MultiPatch adds a part type for each part.
            let types = if kind == 31 { np * 4 } else { 0 };
            let first = 44usize
                .checked_add(np.checked_mul(4).ok_or_else(cut)?)
                .and_then(|x| x.checked_add(types))
                .ok_or_else(cut)?;
            if first.saturating_add(n.saturating_mul(16)) > c.len() {
                return Err(cut());
            }
            let mut starts: Vec<usize> = (0..np)
                .map(|i| le32(c, 44 + i * 4).unwrap_or(0).max(0) as usize)
                .collect();
            starts.push(n);
            for w in starts.windows(2) {
                let (a, b) = (w[0].min(n), w[1].min(n));
                if a >= b {
                    continue;
                }
                let pts: Vec<[f64; 2]> = (a..b).filter_map(|i| point(first + i * 16)).collect();
                if matches!(kind, 3 | 13 | 23) {
                    parts.lines.push(pts);
                } else {
                    parts.rings.push(pts);
                }
            }
        }
        other => return Err(format!("unknown shape type {other}")),
    }
    Ok((kind, parts))
}

// ----- sidecars -----

/// The sidecar with this extension next to `path` (same stem; lower or upper case).
fn sidecar(path: &Path, ext: &str) -> Option<PathBuf> {
    [ext.to_ascii_lowercase(), ext.to_ascii_uppercase()]
        .iter()
        .map(|e| path.with_extension(e))
        .find(|p| p.is_file())
}

/// A sidecar's bytes, at most `limit`; `(bytes, complete)`.
fn read_sidecar(path: &Path, limit: u64) -> Result<(Vec<u8>, bool), String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    let mut bytes = Vec::with_capacity(size.min(limit) as usize);
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok((bytes, size <= limit))
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ----- dBASE -----

#[derive(Clone, Copy, PartialEq, Debug)]
enum Encoding {
    Utf8,
    Cp1252,
    Latin1,
}
impl Encoding {
    fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Cp1252 => "Windows-1252 (ANSI)",
            Self::Latin1 => "ISO-8859-1",
        }
    }
    fn decode(self, b: &[u8]) -> String {
        match self {
            Self::Utf8 => String::from_utf8_lossy(b).into_owned(),
            Self::Latin1 => b.iter().map(|&c| char::from(c)).collect(),
            Self::Cp1252 => b.iter().map(|&c| cp1252(c)).collect(),
        }
    }
}
fn cp1252(c: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    match c {
        0x80..=0x9f => HIGH[usize::from(c - 0x80)],
        _ => char::from(c),
    }
}

/// The encoding named by a `.cpg` file, if Isolinear can decode it.
fn cpg_encoding(text: &str) -> Option<Encoding> {
    let t = text.trim().to_ascii_uppercase().replace([' ', '_'], "-");
    match t.as_str() {
        "UTF-8" | "UTF8" | "65001" => Some(Encoding::Utf8),
        "1252" | "CP1252" | "WINDOWS-1252" | "ANSI-1252" | "ANSI" => Some(Encoding::Cp1252),
        "ISO-8859-1" | "ISO8859-1" | "88591" | "LATIN1" | "LATIN-1" | "28591" => {
            Some(Encoding::Latin1)
        }
        _ => None,
    }
}

struct Field {
    name: String,
    kind: u8,
    length: usize,
    decimals: u8,
}
struct Dbf {
    version: u8,
    updated: String,
    declared: u64,
    fields: Vec<Field>,
    /// (deleted, values, byte range)
    records: Vec<(bool, Vec<String>, std::ops::Range<u64>)>,
    /// Records present in the file (EOF marker or end of data).
    present: u64,
    deleted: u64,
    encoding: Encoding,
    encoding_source: String,
    notes: Vec<String>,
}

fn dbf_version(v: u8) -> Option<&'static str> {
    Some(match v {
        0x02 => "FoxBASE",
        0x03 => "dBASE III / IV (no memo)",
        0x04 => "dBASE IV",
        0x05 => "dBASE 5",
        0x30..=0x32 => "Visual FoxPro",
        0x43 | 0x63 | 0x8b | 0xcb => "dBASE IV with memo",
        0x83 => "dBASE III with memo",
        0xf5 | 0xfb => "FoxPro with memo",
        _ => return None,
    })
}

/// Whether the bytes start like a dBASE table.
fn looks_dbf(b: &[u8]) -> bool {
    if b.len() < 33 || dbf_version(b[0]).is_none() {
        return false;
    }
    let (month, day) = (b[2], b[3]);
    let header = usize::from(u16::from_le_bytes([b[8], b[9]]));
    let record = usize::from(u16::from_le_bytes([b[10], b[11]]));
    (1..=12).contains(&month) && day <= 31 && header >= 33 && record >= 1
}

fn read_dbf(b: &[u8], cpg: Option<&str>, cancelled: &dyn Fn() -> bool) -> Result<Dbf, String> {
    if !looks_dbf(b) {
        return Err("Not a dBASE table".into());
    }
    let declared = u64::from(u32::from_le_bytes([b[4], b[5], b[6], b[7]]));
    let header_len = usize::from(u16::from_le_bytes([b[8], b[9]]));
    let record_len = usize::from(u16::from_le_bytes([b[10], b[11]]));
    let mut fields = vec![];
    let mut at = 32;
    while at + 32 <= header_len.min(b.len()) && b[at] != 0x0d && fields.len() < 2048 {
        let d = &b[at..at + 32];
        let name_end = d[..11].iter().position(|&c| c == 0).unwrap_or(11);
        fields.push(Field {
            name: String::from_utf8_lossy(&d[..name_end]).trim().to_owned(),
            kind: d[11],
            length: usize::from(d[16]),
            decimals: d[17],
        });
        at += 32;
    }
    let mut notes = vec![];
    let width: usize = 1 + fields.iter().map(|f| f.length).sum::<usize>();
    if width > record_len {
        return Err(format!(
            "The fields ({width} bytes) are wider than a record ({record_len} bytes)"
        ));
    }
    let (encoding, encoding_source) = match cpg {
        Some(t) => match cpg_encoding(t) {
            Some(e) => (e, format!("from the .cpg file (\"{}\")", t.trim())),
            None => {
                notes.push(format!(
                    "The .cpg file names the code page \"{}\", which Isolinear does not decode; text is shown as UTF-8 where valid",
                    t.trim()
                ));
                (Encoding::Utf8, "assumed (unsupported .cpg)".into())
            }
        },
        None => match b[29] {
            0x03 | 0x57 => (
                Encoding::Cp1252,
                format!("from the language driver byte (0x{:02x}: ANSI)", b[29]),
            ),
            _ => {
                let body = b.get(header_len..).unwrap_or_default();
                if std::str::from_utf8(body).is_ok() {
                    (Encoding::Utf8, "assumed (valid UTF-8, no .cpg)".into())
                } else {
                    (Encoding::Cp1252, "assumed (not UTF-8, no .cpg)".into())
                }
            }
        },
    };
    let mut records = vec![];
    let (mut present, mut deleted) = (0u64, 0u64);
    let mut at = header_len;
    while at < b.len() && b[at] != 0x1a {
        let Some(r) = b.get(at..at + record_len) else {
            notes.push("The last record of the .dbf is cut short".into());
            break;
        };
        if present.is_multiple_of(65_536) && cancelled() {
            return Err("Cancelled".into());
        }
        present += 1;
        let gone = r[0] == b'*';
        deleted += u64::from(gone);
        if records.len() < MAX_ROWS {
            let mut values = Vec::with_capacity(fields.len());
            let mut x = 1;
            for f in &fields {
                let raw = &r[x..x + f.length];
                x += f.length;
                let text = encoding.decode(raw);
                let t = text.trim();
                values.push(match f.kind {
                    b'L' => match t {
                        "T" | "t" | "Y" | "y" => "true".into(),
                        "F" | "f" | "N" | "n" => "false".into(),
                        _ => String::new(),
                    },
                    b'D' if t.len() == 8 && t.bytes().all(|c| c.is_ascii_digit()) => {
                        format!("{}-{}-{}", &t[..4], &t[4..6], &t[6..])
                    }
                    b'M' if !t.is_empty() => format!("(memo block {t}, not read)"),
                    b'N' | b'F' if t.starts_with('*') => String::new(),
                    _ => t.to_owned(),
                });
            }
            records.push((gone, values, at as u64..(at + record_len) as u64));
        }
        at += record_len;
    }
    if present != declared {
        notes.push(format!(
            "The .dbf header declares {declared} records; {present} are present"
        ));
    }
    if fields.iter().any(|f| f.kind == b'M') {
        notes.push("Memo fields point into a .dbt/.fpt file, which is not read".into());
    }
    let updated = format!("{:04}-{:02}-{:02}", 1900 + u32::from(b[1]), b[2], b[3]);
    Ok(Dbf {
        version: b[0],
        updated,
        declared,
        fields,
        records,
        present,
        deleted,
        encoding,
        encoding_source,
        notes,
    })
}

fn field_type(k: u8) -> &'static str {
    match k {
        b'C' => "character",
        b'N' => "numeric",
        b'F' => "float",
        b'L' => "logical",
        b'D' => "date",
        b'M' => "memo",
        b'I' => "integer",
        b'B' => "double / binary",
        b'T' => "date-time",
        b'Y' => "currency",
        _ => "other",
    }
}

fn dbf_facts(out: &mut Inspection, d: &Dbf) {
    out.fact(
        "Attributes (.dbf)",
        "Format",
        dbf_version(d.version).unwrap_or(""),
        Some(0..1),
    );
    out.fact(
        "Attributes (.dbf)",
        "Last updated",
        d.updated.clone(),
        Some(1..4),
    );
    out.fact(
        "Attributes (.dbf)",
        "Records",
        if d.present == d.declared {
            d.present.to_string()
        } else {
            format!("{} (header declares {})", d.present, d.declared)
        },
        Some(4..8),
    );
    if d.deleted > 0 {
        out.fact(
            "Attributes (.dbf)",
            "Deleted records",
            d.deleted.to_string(),
            None,
        );
    }
    out.fact(
        "Attributes (.dbf)",
        "Fields",
        d.fields.len().to_string(),
        None,
    );
    out.fact(
        "Attributes (.dbf)",
        "Text encoding",
        format!("{}, {}", d.encoding.name(), d.encoding_source),
        None,
    );
    out.tables.push(Table {
        name: "Fields".into(),
        columns: ["Name", "Type", "Length", "Decimals"]
            .map(str::to_owned)
            .to_vec(),
        rows: d
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| Row {
                cells: vec![
                    f.name.clone(),
                    format!("{} ({})", char::from(f.kind), field_type(f.kind)),
                    f.length.to_string(),
                    f.decimals.to_string(),
                ],
                source: Some(32 + i as u64 * 32..64 + i as u64 * 32),
            })
            .collect(),
        total: None,
    });
    out.notes.extend(d.notes.iter().cloned());
}

/// The table of a `.dbf` alone.
fn dbf_table(d: &Dbf, with_sources: bool) -> Table {
    Table {
        name: "Records".into(),
        columns: std::iter::once("Deleted".to_owned())
            .chain(d.fields.iter().map(|f| f.name.clone()))
            .collect(),
        rows: d
            .records
            .iter()
            .map(|(gone, values, span)| Row {
                cells: std::iter::once(if *gone { "deleted" } else { "" }.to_owned())
                    .chain(values.iter().cloned())
                    .collect(),
                source: with_sources.then(|| span.clone()),
            })
            .collect(),
        total: (d.present as usize > d.records.len()).then_some(d.present),
    }
}

// ----- the handler -----

pub struct Shapefile;
impl Handler for Shapefile {
    fn name(&self) -> &'static str {
        "ESRI Shapefile"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        match probe.ext.as_str() {
            "shp" | "shx" => header(probe.head).is_some(),
            "dbf" => looks_dbf(probe.head),
            _ => false,
        }
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let ext = input
            .path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "dbf" => inspect_dbf(input, cancelled),
            "shx" => inspect_shx(input),
            _ => inspect_shp(input, cancelled),
        }
    }
}

fn read_cpg(path: &Path) -> Option<String> {
    let p = sidecar(path, "cpg")?;
    read_sidecar(&p, SMALL_SIDECAR)
        .ok()
        .map(|(b, _)| String::from_utf8_lossy(&b).into_owned())
}

fn inspect_dbf(input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let cpg = read_cpg(input.path);
    let d = read_dbf(input.bytes, cpg.as_deref(), cancelled)?;
    let mut out = Inspection::new("dBASE", "dBASE table (.dbf)");
    out.version = dbf_version(d.version).map(str::to_owned);
    dbf_facts(&mut out, &d);
    out.tables.push(dbf_table(&d, true));
    if sidecar(input.path, "shp").is_some() {
        out.notes.push(
            "This is the attribute table of a shapefile; open the .shp to see the shapes".into(),
        );
    }
    out.check(
        Stage::Parsed,
        if d.present == d.declared && input.complete {
            State::Passed
        } else {
            State::Partial
        },
        "Read the field descriptors and records",
    );
    out.not_validated();
    Ok(out)
}

fn inspect_shx(input: &Input) -> Result<Inspection, String> {
    let b = input.bytes;
    let h = header(b).ok_or("Not a shapefile index")?;
    let mut out = Inspection::new("ESRI Shapefile", "Shapefile index (.shx)");
    out.notes.push(
        "A .shx file only indexes the shapes of the .shp file with the same name: where each record starts and how long it is. Open the .shp to see the shapes and attributes."
            .into(),
    );
    let n = (b.len().saturating_sub(100)) / 8;
    out.fact(
        "Index",
        "Shape type",
        shape_name(h.shape_type),
        Some(32..36),
    );
    out.fact("Index", "Records indexed", n.to_string(), None);
    out.fact("Index", "Bounding box", bbox_text(&h.bbox), Some(36..68));
    let mut t = Table {
        name: "Index entries".into(),
        columns: ["Record", "Offset (bytes)", "Length (bytes)"]
            .map(str::to_owned)
            .to_vec(),
        total: (n > MAX_ROWS).then_some(n as u64),
        ..Table::default()
    };
    for i in 0..n.min(MAX_ROWS) {
        let at = 100 + i * 8;
        let off = be32(b, at).unwrap_or(0).max(0) as u64 * 2;
        let len = be32(b, at + 4).unwrap_or(0).max(0) as u64 * 2;
        t.rows.push(Row {
            cells: vec![(i + 1).to_string(), off.to_string(), len.to_string()],
            source: Some(at as u64..at as u64 + 8),
        });
    }
    out.tables.push(t);
    for (ext, what) in [("shp", "shapes"), ("dbf", "attributes")] {
        if sidecar(input.path, ext).is_none() {
            out.notes.push(format!(
                "The .{ext} file ({what}) is not next to this index"
            ));
        }
    }
    out.check(
        Stage::Parsed,
        if h.length == b.len() as u64 {
            State::Passed
        } else {
            State::Partial
        },
        format!(
            "Header read; the index declares {} bytes, the file has {}",
            h.length,
            b.len()
        ),
    );
    out.not_validated();
    Ok(out)
}

fn bbox_text(b: &[f64; 4]) -> String {
    format!(
        "{}, {} — {}, {}",
        num(b[0]),
        num(b[1]),
        num(b[2]),
        num(b[3])
    )
}

fn inspect_shp(input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let b = input.bytes;
    let h = header(b).ok_or("Not a shapefile (the file code 9994 is missing)")?;
    let mut out = Inspection::new(
        "ESRI Shapefile",
        &format!("Shapefile — {} shapes", shape_name(h.shape_type)),
    );
    out.fact(
        "Shapes (.shp)",
        "Shape type",
        shape_name(h.shape_type),
        Some(32..36),
    );
    out.fact(
        "Shapes (.shp)",
        "Bounding box",
        bbox_text(&h.bbox),
        Some(36..68),
    );
    if matches!(h.shape_type, 11 | 13 | 15 | 18 | 31) {
        out.fact(
            "Shapes (.shp)",
            "Z range",
            format!("{} — {}", num(h.z[0]), num(h.z[1])),
            Some(68..84),
        );
    }
    if matches!(h.shape_type, 11 | 13 | 15 | 18 | 21 | 23 | 25 | 28 | 31) && h.m[0] > -1e38 {
        out.fact(
            "Shapes (.shp)",
            "M range",
            format!("{} — {}", num(h.m[0]), num(h.m[1])),
            Some(84..100),
        );
    }
    // Sidecars: the coordinate system, the code page, the attributes and the index.
    let mut crs: Option<Wkt> = None;
    let mut present = vec![];
    let mut missing = vec![];
    for ext in ["shx", "dbf", "prj", "cpg"] {
        match sidecar(input.path, ext) {
            Some(p) => present.push(file_name(&p)),
            None => missing.push(format!(".{ext}")),
        }
    }
    if let Some(p) = sidecar(input.path, "prj")
        && let Ok((bytes, _)) = read_sidecar(&p, SMALL_SIDECAR)
    {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        crs = wkt(&text);
        if let Some(w) = &crs {
            let label = match w.epsg {
                Some(code) if w.name.is_empty() => epsg_label(code),
                Some(code) => format!("{} (EPSG:{code})", w.name),
                None => w.name.clone(),
            };
            out.fact("Coordinate system (.prj)", "Name", label, None);
            out.fact(
                "Coordinate system (.prj)",
                "Kind",
                if w.geographic {
                    "geographic (longitude/latitude)"
                } else if w.projected {
                    "projected"
                } else {
                    "other"
                },
                None,
            );
        } else {
            out.notes
                .push("The .prj file is not a WKT coordinate system".into());
        }
    }
    let cpg = read_cpg(input.path);
    let mut dbf: Option<Dbf> = None;
    if let Some(p) = sidecar(input.path, "dbf") {
        match read_sidecar(&p, MAX_INPUT) {
            Ok((bytes, complete)) => match read_dbf(&bytes, cpg.as_deref(), cancelled) {
                Ok(d) => {
                    if !complete {
                        out.notes.push(format!(
                            "Only the first {} MiB of the .dbf were read",
                            MAX_INPUT >> 20
                        ));
                    }
                    dbf = Some(d);
                }
                Err(e) => out.notes.push(format!("The .dbf cannot be read: {e}")),
            },
            Err(e) => out.notes.push(format!("The .dbf cannot be read: {e}")),
        }
    }
    let shx_records = sidecar(input.path, "shx")
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len().saturating_sub(100) / 8);
    // The shapes.
    let name_field = dbf.as_ref().and_then(|d| {
        d.fields
            .iter()
            .position(|f| f.name.eq_ignore_ascii_case("name"))
            .or_else(|| d.fields.iter().position(|f| f.kind == b'C'))
    });
    let mut map = Map::default();
    let mut bounds = Bounds::default();
    let mut types = TypeCounts::default();
    let mut rows = vec![];
    let mut count = 0u64;
    let mut problem: Option<String> = None;
    let end = (h.length as usize).min(b.len());
    let mut at = 100usize;
    while at + 8 <= end && (count as usize) < MAX_RECORDS {
        if count.is_multiple_of(65_536) && cancelled() {
            return Err("Cancelled".into());
        }
        let words = be32(b, at + 4).unwrap_or(-1);
        let Some(len) = usize::try_from(words).ok().and_then(|w| w.checked_mul(2)) else {
            problem = Some(format!("record {} has a negative length", count + 1));
            break;
        };
        let Some(content) = b.get(at + 8..at + 8 + len) else {
            problem = Some(format!("record {} is cut short", count + 1));
            break;
        };
        let index = count as usize;
        count += 1;
        let (kind, parts, err) = match shape(content) {
            Ok((k, p)) => (k, p, None),
            Err(e) => (-1, Parts::default(), Some(e)),
        };
        if let Some(e) = err {
            problem.get_or_insert(format!("record {}: {e}", index + 1));
        }
        types.add(if kind < 0 {
            "unreadable"
        } else {
            shape_name(kind)
        });
        parts.bounds(&mut bounds);
        let attrs = dbf.as_ref().and_then(|d| d.records.get(index));
        let name = match (attrs, name_field) {
            (Some((_, v, _)), Some(f)) => v.get(f).cloned().unwrap_or_default(),
            _ => String::new(),
        };
        if rows.len() < MAX_ROWS {
            let mut cells = vec![
                (index + 1).to_string(),
                if kind < 0 {
                    "unreadable".into()
                } else {
                    shape_name(kind).to_owned()
                },
                (parts.lines.len() + parts.rings.len()).to_string(),
                parts.coordinates().to_string(),
            ];
            if let Some(d) = &dbf {
                match attrs {
                    Some((gone, values, _)) => {
                        cells.push(if *gone { "deleted" } else { "" }.into());
                        cells.extend(values.iter().cloned());
                    }
                    None => cells.extend(std::iter::repeat_n(String::new(), d.fields.len() + 1)),
                }
            }
            rows.push(Row {
                cells,
                source: Some(at as u64..(at + 8 + len) as u64),
            });
        }
        map.add(&name, parts);
        at += 8 + len;
    }
    out.fact("Shapes (.shp)", "Records", count.to_string(), None);
    out.fact("Shapes (.shp)", "Shapes found", types.describe(), None);
    if h.length != b.len() as u64 && input.complete {
        out.notes.push(format!(
            "The header declares {} bytes; the file has {}",
            h.length,
            b.len()
        ));
    }
    out.fact("Files", "Sidecars found", present.join(", "), None);
    if !missing.is_empty() {
        out.fact("Files", "Sidecars missing", missing.join(", "), None);
        out.notes.push(format!(
            "Missing next to the .shp: {} ({}).",
            missing.join(", "),
            missing
                .iter()
                .map(|m| match m.as_str() {
                    ".shx" => "the record index",
                    ".dbf" => "attributes",
                    ".prj" => "the coordinate system",
                    _ => "the text encoding of attributes",
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut columns: Vec<String> = ["Record", "Geometry", "Parts", "Points"]
        .map(str::to_owned)
        .to_vec();
    if let Some(d) = &dbf {
        dbf_facts(&mut out, d);
        columns.push("Deleted".into());
        columns.extend(d.fields.iter().map(|f| f.name.clone()));
    }
    out.tables.insert(
        0,
        Table {
            name: "Records".into(),
            columns,
            rows,
            total: (count as usize > MAX_ROWS).then_some(count),
        },
    );
    let geographic = match &crs {
        Some(w) => w.geographic,
        None => bounds.fits_lonlat(),
    };
    if crs.is_none() && geographic {
        out.notes.push(
            "No .prj: the coordinates fit longitude/latitude, so the map treats them as such"
                .into(),
        );
    }
    map.finish(geographic, &mut out.visuals, &mut out.notes);
    match &problem {
        None => out.check(Stage::Parsed, State::Passed, "Every shape record was read"),
        Some(p) => out.check(
            Stage::Parsed,
            State::Partial,
            format!("Read {count} records; {p}"),
        ),
    }
    // The three files must describe the same records.
    let mut counts = vec![format!(".shp {count}")];
    let mut same = true;
    if let Some(n) = shx_records {
        counts.push(format!(".shx {n}"));
        same &= n == count;
    }
    if let Some(d) = &dbf {
        counts.push(format!(".dbf {}", d.present));
        same &= d.present == count;
    }
    if counts.len() > 1 {
        out.check(
            Stage::Semantic,
            if same { State::Passed } else { State::Failed },
            format!("Record counts: {}", counts.join(", ")),
        );
    }
    out.not_validated();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn code_pages() {
        assert_eq!(cpg_encoding("UTF-8\n"), Some(Encoding::Utf8));
        assert_eq!(cpg_encoding("1252"), Some(Encoding::Cp1252));
        assert_eq!(cpg_encoding("big5"), None);
        assert_eq!(Encoding::Cp1252.decode(b"caf\xe9 \x80"), "café €");
    }
}
