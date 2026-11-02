//! NumPy `.npy` arrays as tables: the reference [`TableSource`].
//!
//! A one-dimensional array is one column; a two-dimensional array has one column per index of
//! its second axis; higher dimensions are flattened into columns. Fortran order is read as
//! stored. Supported element types: booleans, signed and unsigned integers, floats, complex
//! numbers, dates, durations, fixed-width bytes and Unicode strings, in either byte order. Object arrays (pickled
//! Python objects) are refused: reading them would mean running Python code.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{Column, Schema, TableInfo, TableSource, cut};

const MAGIC: &[u8] = b"\x93NUMPY";
/// Header bytes accepted at most (NumPy itself writes under 64 KiB).
const HEADER_MAX: usize = 1 << 20;
/// Columns shown at most; wider arrays are cut and the cut is stated.
const COLUMNS_MAX: u64 = 1000;
const PAGE_BYTES: u64 = 256 * 1024 * 1024;
const PAGE_CELLS: u64 = 2_000_000;
const PAGE_ROWS: u64 = 100_000;
const ELEMENT_MAX: usize = 16 * 1024 * 1024;
pub(super) trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bool,
    Int,
    Uint,
    Float,
    Complex,
    Bytes,
    Unicode,
    Datetime,
    Timedelta,
}

pub struct Npy {
    file: Box<dyn ReadSeek>,
    shape: Vec<u64>,
    time_unit: String,
    schema: Schema,
    /// Column-major data: element (r, c) is at c * rows + r.
    fortran: bool,
    kind: Kind,
    big_endian: bool,
    /// Bytes per element.
    width: usize,
    /// Byte offset of the data.
    data: u64,
    /// Elements per row as stored, and columns shown.
    row_len: u64,
    shown: usize,
}

impl Npy {
    pub fn open(path: &Path, _table: Option<&str>) -> Result<Npy, String> {
        Self::from_reader(
            path.file_name()
                .map_or("array".into(), |n| n.to_string_lossy().into_owned()),
            File::open(path).map_err(|e| e.to_string())?,
        )
    }

    pub(super) fn from_reader(
        name: String,
        mut file: impl ReadSeek + 'static,
    ) -> Result<Npy, String> {
        let size = file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut head = [0u8; 10];
        file.read_exact(&mut head)
            .map_err(|_| "too short to be a NumPy array".to_owned())?;
        if &head[..6] != MAGIC {
            return Err("not a NumPy array (no \\x93NUMPY signature)".into());
        }
        let (major, minor) = (head[6], head[7]);
        let (header_len, header_start) = match major {
            1 => (u16::from_le_bytes([head[8], head[9]]) as usize, 10u64),
            2 | 3 => {
                let mut more = [0u8; 2];
                file.read_exact(&mut more).map_err(|e| e.to_string())?;
                (
                    u32::from_le_bytes([head[8], head[9], more[0], more[1]]) as usize,
                    12u64,
                )
            }
            v => return Err(format!("NumPy format version {v} is not known")),
        };
        if header_len > HEADER_MAX {
            return Err("The NumPy header exceeds 1 MiB".into());
        }
        if header_start + header_len as u64 > size {
            return Err("the header is longer than the file".into());
        }
        let mut header = vec![0u8; header_len];
        file.read_exact(&mut header).map_err(|e| e.to_string())?;
        let header = String::from_utf8_lossy(&header).into_owned();
        let descr = field(&header, "descr").ok_or("the header has no 'descr'")?;
        let descr = descr.trim_matches(|c| c == '\'' || c == '"');
        let fortran = field(&header, "fortran_order").is_some_and(|v| v.starts_with("True"));
        let shape = shape(&header).ok_or("the header has no readable 'shape'")?;

        if !descr.is_ascii() {
            return Err(format!("element type {descr} is not supported"));
        }
        let (order, rest) = descr.split_at(descr.len().min(1));
        let big_endian = order == ">";
        let rest = if matches!(order, "<" | ">" | "|" | "=") {
            rest
        } else {
            descr
        };
        let (code, width) = rest.split_at(rest.len().min(1));
        if code == "O" {
            return Err("object arrays hold pickled Python objects and are not opened".into());
        }
        let (width, time_unit) = match width.split_once('[') {
            Some((width, unit)) if matches!(code, "M" | "m") && unit.ends_with(']') => {
                (width, &unit[..unit.len() - 1])
            }
            _ => (width, ""),
        };
        if time_unit.len() > 32 {
            return Err("The NumPy time unit exceeds 32 bytes".into());
        }
        let width: usize = width
            .parse()
            .map_err(|_| format!("element type {descr} is not supported"))?;
        let kind = match (code, width) {
            ("b", 1) => Kind::Bool,
            ("i", 1 | 2 | 4 | 8) => Kind::Int,
            ("u", 1 | 2 | 4 | 8) => Kind::Uint,
            ("f", 2 | 4 | 8) => Kind::Float,
            ("c", 8 | 16) => Kind::Complex,
            ("S", 1..) => Kind::Bytes,
            ("U", 1..) => Kind::Unicode,
            ("M", 8) => Kind::Datetime,
            ("m", 8) => Kind::Timedelta,
            ("O", _) => {
                return Err("object arrays hold pickled Python objects and are not opened".into());
            }
            _ => return Err(format!("element type {descr} is not supported")),
        };
        let bytes_per = if kind == Kind::Unicode {
            width.checked_mul(4).ok_or("the Unicode width overflows")?
        } else {
            width
        };

        if bytes_per > ELEMENT_MAX {
            return Err("NumPy elements larger than 16 MiB are not shown".into());
        }
        let rows = shape.first().copied().unwrap_or(1);
        let row_len = shape
            .iter()
            .skip(1)
            .try_fold(1u64, |n, d| n.checked_mul(*d))
            .ok_or("the shape overflows")?;
        let elements = rows.checked_mul(row_len).ok_or("the shape overflows")?;
        let data = header_start + header_len as u64;
        let need = elements
            .checked_mul(bytes_per as u64)
            .ok_or("the shape overflows")?;
        if data.checked_add(need).is_none_or(|end| end > size) {
            return Err(format!(
                "the shape {shape:?} needs {need} bytes of data; the file has {}",
                size - data
            ));
        }
        let shown = row_len.min(COLUMNS_MAX) as usize;
        let columns = if shape.len() <= 1 {
            vec![Column {
                name: "value".into(),
                type_name: descr.to_owned(),
            }]
        } else {
            (0..shown)
                .map(|i| Column {
                    name: index_name(i as u64, &shape[1..]),
                    type_name: descr.to_owned(),
                })
                .collect()
        };
        let mut details = vec![
            ("Shape".into(), format!("{shape:?}")),
            ("Element type".into(), describe(kind, width, big_endian)),
            (
                "Order".into(),
                if fortran {
                    "Fortran (column-major)"
                } else {
                    "C (row-major)"
                }
                .into(),
            ),
            ("Format version".into(), format!("{major}.{minor}")),
        ];
        if row_len > COLUMNS_MAX {
            details.push((
                "Columns shown".into(),
                format!("the first {COLUMNS_MAX} of {row_len}"),
            ));
        }
        Ok(Npy {
            file: Box::new(file),
            time_unit: time_unit.to_owned(),
            shape: shape.clone(),
            schema: Schema {
                tables: vec![TableInfo {
                    name,
                    rows: Some(rows),
                }],
                table: 0,
                columns,
                rows: Some(rows),
                details,
                text: Some(header.trim().to_owned()),
            },
            kind,
            big_endian,
            width: bytes_per,
            data,
            row_len,
            shown,
            fortran: fortran && shape.len() > 1,
        })
    }

    fn element(&self, bytes: &[u8]) -> Option<String> {
        let le = !self.big_endian;
        let int = |b: &[u8]| -> i128 {
            let mut buf = [0u8; 16];
            if le {
                buf[..b.len()].copy_from_slice(b);
                if b.last().is_some_and(|x| x & 0x80 != 0) {
                    buf[b.len()..].fill(0xFF);
                }
                i128::from_le_bytes(buf)
            } else {
                let mut r = b.to_vec();
                r.reverse();
                buf[..r.len()].copy_from_slice(&r);
                if r.last().is_some_and(|x| x & 0x80 != 0) {
                    buf[r.len()..].fill(0xFF);
                }
                i128::from_le_bytes(buf)
            }
        };
        let uint = |b: &[u8]| -> u128 {
            let mut buf = [0u8; 16];
            let mut r = b.to_vec();
            if !le {
                r.reverse();
            }
            buf[..r.len()].copy_from_slice(&r);
            u128::from_le_bytes(buf)
        };
        let float = |b: &[u8]| -> f64 {
            let mut r = b.to_vec();
            if !le {
                r.reverse();
            }
            match r.len() {
                2 => half(u16::from_le_bytes([r[0], r[1]])),
                4 => f32::from_le_bytes([r[0], r[1], r[2], r[3]]) as f64,
                8 => f64::from_le_bytes([r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]]),
                _ => f64::NAN,
            }
        };
        Some(match self.kind {
            Kind::Datetime | Kind::Timedelta => {
                let value = int(bytes) as i64;
                if value == i64::MIN {
                    return None;
                }
                if self.kind == Kind::Datetime {
                    datetime(value, &self.time_unit)
                } else {
                    format!("{value} [{}]", self.time_unit)
                }
            }
            Kind::Bool => (bytes[0] != 0).to_string(),
            Kind::Int => int(bytes).to_string(),
            Kind::Uint => uint(bytes).to_string(),
            Kind::Float => number(float(bytes)),
            Kind::Complex => {
                let (re, im) = bytes.split_at(bytes.len() / 2);
                let (re, im) = (float(re), float(im));
                format!(
                    "{}{}{}j",
                    number(re),
                    if im < 0.0 { "-" } else { "+" },
                    number(im.abs())
                )
            }
            Kind::Bytes => {
                let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
                cut(String::from_utf8_lossy(&bytes[..end]).into_owned())
            }
            Kind::Unicode => {
                let chars: String = bytes
                    .chunks(4)
                    .map(|c| {
                        let v = if le {
                            u32::from_le_bytes([c[0], c[1], c[2], c[3]])
                        } else {
                            u32::from_be_bytes([c[0], c[1], c[2], c[3]])
                        };
                        char::from_u32(v).unwrap_or('\u{FFFD}')
                    })
                    .collect();
                cut(chars.trim_end_matches('\0').to_owned())
            }
        })
    }
}

impl TableSource for Npy {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn rows(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        let total = self.schema.rows.unwrap_or(0);
        let end = start.saturating_add(count as u64).min(total);
        if start >= end {
            return Ok(Vec::new());
        }
        let rows = end - start;
        let cells = rows
            .checked_mul(self.shown as u64)
            .ok_or("page size overflows")?;
        if rows > PAGE_ROWS
            || cells > PAGE_CELLS
            || cells
                .checked_mul(self.width as u64)
                .is_none_or(|n| n > PAGE_BYTES)
        {
            return Err(
                "The NumPy page exceeds 100,000 rows, 2,000,000 cells or 256 MiB; request fewer rows".into(),
            );
        }
        let w = self.width as u64;
        let mut out = Vec::with_capacity(rows as usize);
        let mut cell = vec![0; self.width];
        let mut displayed = 0usize;
        if !self.fortran {
            let mut buffer = vec![0; self.shown * self.width];
            for r in start..end {
                let at = r
                    .checked_mul(self.row_len)
                    .and_then(|n| n.checked_mul(w))
                    .and_then(|n| n.checked_add(self.data))
                    .ok_or("array offset overflows")?;
                self.file
                    .seek(SeekFrom::Start(at))
                    .map_err(|e| e.to_string())?;
                self.file
                    .read_exact(&mut buffer)
                    .map_err(|e| e.to_string())?;
                let row = buffer
                    .chunks_exact(self.width)
                    .map(|cell| self.element(cell))
                    .inspect(|value| {
                        displayed = displayed.saturating_add(value.as_ref().map_or(0, String::len));
                    })
                    .collect();
                if displayed > PAGE_BYTES as usize {
                    return Err("Displayed NumPy page exceeds 256 MiB".into());
                }
                out.push(row);
            }
            return Ok(out);
        }
        for r in start..end {
            let mut row = Vec::with_capacity(self.shown);
            for c in 0..self.shown as u64 {
                let element = if self.fortran {
                    let mut coordinates = vec![0; self.shape.len()];
                    coordinates[0] = r;
                    let mut rest = c;
                    for i in (1..self.shape.len()).rev() {
                        coordinates[i] = rest % self.shape[i].max(1);
                        rest /= self.shape[i].max(1);
                    }
                    let mut stride = 1u64;
                    let mut element = 0u64;
                    for (coordinate, dim) in coordinates.iter().zip(&self.shape) {
                        element = element
                            .checked_add(
                                coordinate
                                    .checked_mul(stride)
                                    .ok_or("array offset overflows")?,
                            )
                            .ok_or("array offset overflows")?;
                        stride = stride.checked_mul(*dim).ok_or("array stride overflows")?;
                    }
                    element
                } else {
                    r.checked_mul(self.row_len)
                        .and_then(|n| n.checked_add(c))
                        .ok_or("array offset overflows")?
                };
                let at = element
                    .checked_mul(w)
                    .and_then(|n| n.checked_add(self.data))
                    .ok_or("array offset overflows")?;
                self.file
                    .seek(SeekFrom::Start(at))
                    .map_err(|e| e.to_string())?;
                self.file.read_exact(&mut cell).map_err(|e| e.to_string())?;
                let value = self.element(&cell);
                displayed = displayed.saturating_add(value.as_ref().map_or(0, String::len));
                if displayed > PAGE_BYTES as usize {
                    return Err("Displayed NumPy page exceeds 256 MiB".into());
                }
                row.push(value);
            }
            out.push(row);
        }
        Ok(out)
    }
}

/// The text after `'key':` in the header dictionary, up to the next top-level comma.
fn field<'h>(header: &'h str, key: &str) -> Option<&'h str> {
    let at = header
        .find(&format!("'{key}'"))
        .or_else(|| header.find(&format!("\"{key}\"")))?;
    let rest = header[at + key.len() + 2..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim())
}

fn shape(header: &str) -> Option<Vec<u64>> {
    let at = header
        .find("'shape'")
        .or_else(|| header.find("\"shape\""))?;
    let rest = &header[at..];
    let open = rest.find('(')?;
    let close = open + rest[open..].find(')')?;
    let dimensions = &rest[open + 1..close];
    if dimensions
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .count()
        > 64
    {
        return None;
    }
    dimensions
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('L').parse().ok())
        .collect()
}

/// Column name for flat index `i` over the trailing axes: `3` or `[1, 2]`.
fn index_name(i: u64, axes: &[u64]) -> String {
    if axes.len() == 1 {
        return i.to_string();
    }
    let mut rest = i;
    let mut parts = vec![0; axes.len()];
    for (k, len) in axes.iter().enumerate().rev() {
        parts[k] = rest % len.max(&1);
        rest /= len.max(&1);
    }
    format!("{parts:?}")
}

fn describe(kind: Kind, width: usize, big: bool) -> String {
    let name = match kind {
        Kind::Bool => "boolean".into(),
        Kind::Int => format!("{}-bit signed integer", width * 8),
        Kind::Uint => format!("{}-bit unsigned integer", width * 8),
        Kind::Float => format!("{}-bit float", width * 8),
        Kind::Complex => format!("{}-bit complex", width * 8),
        Kind::Bytes => format!("{width}-byte string"),
        Kind::Unicode => format!("{}-character Unicode string", width / 4),
        Kind::Datetime => "datetime64".into(),
        Kind::Timedelta => "timedelta64".into(),
    };
    if width > 1 && kind != Kind::Bytes {
        format!(
            "{name}, {}",
            if big { "big-endian" } else { "little-endian" }
        )
    } else {
        name
    }
}

/// Shortest text that reads back as the same double.
fn number(x: f64) -> String {
    if x.is_nan() {
        "nan".into()
    } else if x.is_infinite() {
        if x > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        format!("{x}")
    }
}

/// IEEE 754 half precision to double.
fn half(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (h >> 10) & 0x1F;
    let frac = (h & 0x3FF) as f64;
    match exp {
        0 => sign * frac * 2f64.powi(-24),
        31 if frac == 0.0 => sign * f64::INFINITY,
        31 => f64::NAN,
        e => sign * (1.0 + frac / 1024.0) * 2f64.powi(e as i32 - 15),
    }
}

/// NumPy dates use a unit relative to 1970; i128 avoids overflow at the i64 endpoints.
fn datetime(value: i64, unit: &str) -> String {
    let v = i128::from(value);
    let date = |days: i128| {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i128::from(m <= 2);
        format!("{y:04}-{m:02}-{d:02}")
    };
    match unit {
        "Y" => return (1970 + v).to_string(),
        "M" => return format!("{:04}-{:02}", 1970 + v.div_euclid(12), v.rem_euclid(12) + 1),
        "W" => return date(v * 7),
        "D" => return date(v),
        _ => {}
    }
    let (scale, digits) = match unit {
        "h" | "m" | "s" | "" => (1, 0),
        "ms" => (1000, 3),
        "us" => (1_000_000, 6),
        "ns" => (1_000_000_000, 9),
        _ => return format!("{v} [{unit}]"),
    };
    let seconds = match unit {
        "h" => v * 3600,
        "m" => v * 60,
        _ => v.div_euclid(scale),
    };
    let time = seconds.rem_euclid(86_400);
    let mut out = format!(
        "{}T{:02}:{:02}:{:02}",
        date(seconds.div_euclid(86_400)),
        time / 3600,
        time / 60 % 60,
        time % 60
    );
    if digits > 0 {
        out.push_str(&format!(".{:0digits$}", v.rem_euclid(scale)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version 1.0 file as NumPy writes it.
    fn npy(descr: &str, fortran: bool, shape: &str, data: &[u8]) -> Vec<u8> {
        let mut header = format!(
            "{{'descr': '{descr}', 'fortran_order': {}, 'shape': {shape}, }}",
            if fortran { "True" } else { "False" }
        );
        while (10 + header.len() + 1) % 64 != 0 {
            header.push(' ');
        }
        header.push('\n');
        let mut out = MAGIC.to_vec();
        out.extend([1, 0]);
        out.extend((header.len() as u16).to_le_bytes());
        out.extend(header.as_bytes());
        out.extend(data);
        out
    }

    fn open(bytes: &[u8]) -> Result<Npy, String> {
        let dir = std::env::temp_dir().join(format!(
            "isolinear-npy-{}-{}",
            std::process::id(),
            bytes.len() ^ bytes.iter().map(|&b| b as usize).sum::<usize>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.npy");
        std::fs::write(&p, bytes).unwrap();
        let r = Npy::open(&p, None);
        std::fs::remove_dir_all(&dir).ok();
        r
    }

    fn texts(rows: Vec<Vec<Option<String>>>) -> Vec<Vec<String>> {
        rows.into_iter()
            .map(|r| r.into_iter().map(Option::unwrap).collect())
            .collect()
    }

    #[test]
    fn one_dimension_is_one_column() {
        let data: Vec<u8> = [1i32, -2, 3].iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut a = open(&npy("<i4", false, "(3,)", &data)).unwrap();
        assert_eq!(a.schema().columns.len(), 1);
        assert_eq!(a.schema().rows, Some(3));
        assert_eq!(texts(a.rows(1, 10).unwrap()), [["-2"], ["3"]]);
    }

    #[test]
    fn two_dimensions_c_and_fortran_order() {
        let data: Vec<u8> = [1.5f64, 2.0, 3.25, 4.0, 5.0, 6.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut c = open(&npy("<f8", false, "(2, 3)", &data)).unwrap();
        assert_eq!(
            c.schema()
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["0", "1", "2"]
        );
        assert_eq!(
            texts(c.rows(0, 2).unwrap()),
            [["1.5", "2", "3.25"], ["4", "5", "6"]]
        );
        let mut f = open(&npy("<f8", true, "(2, 3)", &data)).unwrap();
        assert_eq!(
            texts(f.rows(0, 2).unwrap()),
            [["1.5", "3.25", "5"], ["2", "4", "6"]]
        );
    }

    #[test]
    fn element_types() {
        let mut b = open(&npy(">u2", false, "(2,)", &[0x01, 0x02, 0xFF, 0xFF])).unwrap();
        assert_eq!(texts(b.rows(0, 2).unwrap()), [["258"], ["65535"]]);
        let mut s = open(&npy("|S3", false, "(2,)", b"ab\0xyz")).unwrap();
        assert_eq!(texts(s.rows(0, 2).unwrap()), [["ab"], ["xyz"]]);
        let u: Vec<u8> = "hé\0"
            .chars()
            .flat_map(|c| (c as u32).to_le_bytes())
            .collect();
        let mut u = open(&npy("<U3", false, "(1,)", &u)).unwrap();
        assert_eq!(texts(u.rows(0, 1).unwrap()), [["hé"]]);
        let mut h = open(&npy("<f2", false, "(1,)", &0x3C00u16.to_le_bytes())).unwrap();
        assert_eq!(texts(h.rows(0, 1).unwrap()), [["1"]]);
        let c: Vec<u8> = [1.0f32, -2.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut c = open(&npy("<c8", false, "(1,)", &c)).unwrap();
        assert_eq!(texts(c.rows(0, 1).unwrap()), [["1-2j"]]);
        let mut t = open(&npy("|b1", false, "(2,)", &[1, 0])).unwrap();
        assert_eq!(texts(t.rows(0, 2).unwrap()), [["true"], ["false"]]);
    }

    #[test]
    fn refusals() {
        assert!(open(b"nope").is_err());
        assert!(
            open(&npy("|O", false, "(1,)", &[0; 8]))
                .err()
                .unwrap()
                .contains("pickled")
        );
        assert!(
            open(&npy("<i4", false, "(10,)", &[0; 8]))
                .err()
                .unwrap()
                .contains("needs 40 bytes")
        );
        assert!(open(&npy("<i4", false, "(4294967296, 4294967296, 16)", &[])).is_err());
        let mut v = npy("<i4", false, "(1,)", &[0; 4]);
        v[6] = 9;
        assert!(open(&v).err().unwrap().contains("version 9"));
    }

    #[test]
    fn seekable_memory_dates_empty_arrays_and_fortran_cubes() {
        let mut cube = Npy::from_reader(
            "cube".into(),
            std::io::Cursor::new(npy("|u1", true, "(2, 2, 3)", &(0..12).collect::<Vec<_>>())),
        )
        .unwrap();
        assert_eq!(
            texts(cube.rows(1, 1).unwrap()),
            [["1", "5", "9", "3", "7", "11"]]
        );
        let mut empty = open(&npy("|u1", false, "(3, 0)", &[])).unwrap();
        assert_eq!(
            empty.rows(0, 10).unwrap(),
            vec![Vec::<Option<String>>::new(); 3]
        );
        let data: Vec<_> = [0i64, -1, i64::MIN, i64::MAX]
            .into_iter()
            .flat_map(i64::to_le_bytes)
            .collect();
        let mut dates = open(&npy("<M8[ns]", false, "(4,)", &data)).unwrap();
        let values = dates.rows(0, 4).unwrap();
        assert_eq!(
            values[0][0].as_deref(),
            Some("1970-01-01T00:00:00.000000000")
        );
        assert_eq!(
            values[1][0].as_deref(),
            Some("1969-12-31T23:59:59.999999999")
        );
        assert_eq!(values[2][0], None);
        assert_eq!(
            values[3][0].as_deref(),
            Some("2262-04-11T23:47:16.854775807")
        );
    }

    #[test]
    fn malformed_width_shape_and_page_limits() {
        for (descr, shape) in [
            ("<é4", "(1,)"),
            ("<U18446744073709551615", "(0,)"),
            ("|S16777217", "(0,)"),
            ("<i8", "(18446744073709551615, 2)"),
        ] {
            assert!(open(&npy(descr, false, shape, &[])).is_err());
        }
        let dimensions = format!("({})", "1,".repeat(65));
        assert!(open(&npy("|u1", false, &dimensions, &[1])).is_err());
        let mut wide = open(&npy("|u1", false, "(2001, 1000)", &vec![0; 2_001_000])).unwrap();
        assert_eq!(wide.rows(0, 200).unwrap().len(), 200);
        assert!(wide.rows(0, 2001).unwrap_err().contains("2,000,000"));
        assert!(wide.rows(u64::MAX, usize::MAX).unwrap().is_empty());
    }

    #[test]
    fn twenty_thousand_mutated_arrays_never_panic() {
        let bases = [
            npy("<i4", false, "(2, 3)", &[0; 24]),
            npy("<U2", true, "(2, 2)", &[0; 32]),
            npy("<M8[D]", false, "(4,)", &[0; 32]),
        ];
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for i in 0..20_000 {
            let mut bytes = bases[i % bases.len()].clone();
            for _ in 0..next() % 8 + 1 {
                let at = next() as usize % bytes.len();
                bytes[at] = next() as u8;
            }
            if next() % 4 == 0 {
                bytes.truncate(next() as usize % bytes.len());
            }
            if let Ok(mut array) = Npy::from_reader("mutated".into(), std::io::Cursor::new(bytes)) {
                let _ = array.rows(0, 4);
                let _ = array.rows(u64::MAX, usize::MAX);
            }
        }
    }

    #[test]
    fn wide_arrays_are_cut_and_say_so() {
        let mut a = open(&npy("|u1", false, "(1, 3000)", &[7; 3000])).unwrap();
        assert_eq!(a.schema().columns.len(), 1000);
        assert!(a.schema().details.iter().any(|(k, _)| k == "Columns shown"));
        assert_eq!(a.rows(0, 1).unwrap()[0].len(), 1000);
    }
}
