//! GeoTIFF (OGC 19-008r4) on TIFF 6.0 and BigTIFF, either byte order: the first image's
//! layout (size, samples, bits, compression, tiles or strips) and its georeferencing — the
//! GeoKey directory, pixel scale, tie points or transformation — turned into a coordinate
//! system name, pixel size and bounds. Only TIFFs with a GeoKey directory are claimed; the
//! pixels themselves are shown by the image view.
use super::geo::{Bounds, Map, Parts, epsg_geographic, epsg_label, num, to_lonlat};
use crate::inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Row, Stage, State, Table};

const GEO_KEY_DIRECTORY: u16 = 34735;
const GEO_DOUBLE_PARAMS: u16 = 34736;
const GEO_ASCII_PARAMS: u16 = 34737;
const MODEL_PIXEL_SCALE: u16 = 33550;
const MODEL_TIEPOINT: u16 = 33922;
const MODEL_TRANSFORMATION: u16 = 34264;
const GDAL_METADATA: u16 = 42112;
const GDAL_NODATA: u16 = 42113;
/// Entries of one directory read at most.
const MAX_ENTRIES: u64 = 4096;
/// Values of one tag read at most.
const MAX_VALUES: u64 = 1_000_000;
/// Directories (images, overviews) counted at most.
const MAX_IFDS: usize = 1000;

#[derive(Clone, Copy)]
struct Tiff<'a> {
    b: &'a [u8],
    le: bool,
    big: bool,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    tag: u16,
    kind: u16,
    count: u64,
    /// Where the values are: inside the entry, or at an offset in the file.
    at: u64,
}

fn type_size(kind: u16) -> u64 {
    match kind {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 | 16 | 17 | 18 => 8,
        _ => 0,
    }
}

impl<'a> Tiff<'a> {
    /// Byte order and variant from the header; the first directory's offset.
    fn open(b: &'a [u8]) -> Option<(Self, u64)> {
        let le = match b.get(..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let mut t = Tiff { b, le, big: false };
        match t.u16(2)? {
            42 => Some((t, u64::from(t.u32(4)?))),
            43 => {
                if t.u16(4)? != 8 {
                    return None;
                }
                t.big = true;
                Some((t, t.u64(8)?))
            }
            _ => None,
        }
    }
    fn bytes<const N: usize>(&self, at: u64) -> Option<[u8; N]> {
        let at = usize::try_from(at).ok()?;
        self.b.get(at..at.checked_add(N)?)?.try_into().ok()
    }
    fn u16(&self, at: u64) -> Option<u16> {
        let b = self.bytes::<2>(at)?;
        Some(if self.le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }
    fn u32(&self, at: u64) -> Option<u32> {
        let b = self.bytes::<4>(at)?;
        Some(if self.le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }
    fn u64(&self, at: u64) -> Option<u64> {
        let b = self.bytes::<8>(at)?;
        Some(if self.le {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        })
    }
    fn f32(&self, at: u64) -> Option<f32> {
        self.u32(at).map(f32::from_bits)
    }
    fn f64(&self, at: u64) -> Option<f64> {
        self.u64(at).map(f64::from_bits)
    }
    /// The entries of the directory at `offset` (relative to `self.b`), and the next offset.
    fn directory(&self, offset: u64) -> Option<(Vec<Entry>, u64)> {
        let (count, head, size, inline) = if self.big {
            (self.u64(offset)?, 8, 20, 8)
        } else {
            (u64::from(self.u16(offset)?), 2, 12, 4)
        };
        let count = count.min(MAX_ENTRIES);
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let e = offset.checked_add(head + i * size)?;
            let tag = self.u16(e)?;
            let kind = self.u16(e + 2)?;
            let (n, value) = if self.big {
                (self.u64(e + 4)?, e + 12)
            } else {
                (u64::from(self.u32(e + 4)?), e + 8)
            };
            let bytes = n.checked_mul(type_size(kind))?;
            let at = if bytes <= inline {
                value
            } else if self.big {
                self.u64(value)?
            } else {
                u64::from(self.u32(value)?)
            };
            out.push(Entry {
                tag,
                kind,
                count: n,
                at,
            });
        }
        let next_at = offset.saturating_add(head + count * size);
        let next = if self.big {
            self.u64(next_at).unwrap_or(0)
        } else {
            u64::from(self.u32(next_at).unwrap_or(0))
        };
        Some((out, next))
    }
    /// Numeric values of an entry (at most [`MAX_VALUES`]).
    fn numbers(&self, e: &Entry) -> Vec<f64> {
        let size = type_size(e.kind);
        let mut out = vec![];
        for i in 0..e.count.min(MAX_VALUES) {
            let at = e.at.saturating_add(i * size);
            let v = match e.kind {
                1 | 7 => self.bytes::<1>(at).map(|b| f64::from(b[0])),
                6 => self.bytes::<1>(at).map(|b| f64::from(b[0] as i8)),
                3 => self.u16(at).map(f64::from),
                8 => self.u16(at).map(|v| f64::from(v as i16)),
                4 | 13 => self.u32(at).map(f64::from),
                9 => self.u32(at).map(|v| f64::from(v as i32)),
                11 => self.f32(at).map(f64::from),
                12 => self.f64(at),
                16 | 18 => self.u64(at).map(|v| v as f64),
                17 => self.u64(at).map(|v| v as i64 as f64),
                5 | 10 => match (self.u32(at), self.u32(at.saturating_add(4))) {
                    (Some(n), Some(d)) if d != 0 => Some(if e.kind == 10 {
                        f64::from(n as i32) / f64::from(d as i32)
                    } else {
                        f64::from(n) / f64::from(d)
                    }),
                    _ => None,
                },
                _ => None,
            };
            match v {
                Some(v) => out.push(v),
                None => break,
            }
        }
        out
    }
    fn ascii(&self, e: &Entry) -> String {
        let Ok(at) = usize::try_from(e.at) else {
            return String::new();
        };
        let n = e.count.min(MAX_VALUES) as usize;
        let b = self
            .b
            .get(at..at.saturating_add(n).min(self.b.len()))
            .unwrap_or_default();
        String::from_utf8_lossy(b).trim_end_matches('\0').to_owned()
    }
}

/// Whether the first directory has a GeoKey directory. `data` holds the file's start; when
/// the directory lies beyond it, `more` reads the bytes at an offset.
fn has_geokeys(head: &[u8], more: impl FnOnce(u64, usize) -> Option<Vec<u8>>) -> bool {
    let Some((t, ifd)) = Tiff::open(head) else {
        return false;
    };
    let entry = if t.big { 20u64 } else { 12 };
    let within = |len: u64| {
        ifd.checked_add(len)
            .is_some_and(|end| end <= head.len() as u64)
    };
    let scan = |t: &Tiff, at: u64| {
        t.directory(at)
            .is_some_and(|(es, _)| es.iter().any(|e| e.tag == GEO_KEY_DIRECTORY))
    };
    if within(8) {
        let n = if t.big {
            t.u64(ifd)
        } else {
            t.u16(ifd).map(u64::from)
        };
        if n.is_some_and(|n| within(8 + n.min(MAX_ENTRIES) * entry)) {
            return scan(&t, ifd);
        }
    }
    // The directory is past the probe: read just it.
    let len = (8 + MAX_ENTRIES * entry + 8) as usize;
    let Some(chunk) = more(ifd, len) else {
        return false;
    };
    let shifted = Tiff {
        b: &chunk,
        le: t.le,
        big: t.big,
    };
    scan(&shifted, 0)
}

pub struct GeoTiff;
impl Handler for GeoTiff {
    fn name(&self) -> &'static str {
        "GeoTIFF"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if !(probe.head.starts_with(b"II") || probe.head.starts_with(b"MM")) {
            return false;
        }
        let path = probe.path;
        let allowed = probe.ext_is(&["tif", "tiff", "gtiff", "gtif"]);
        has_geokeys(probe.head, |at, len| {
            use std::io::{Read, Seek, SeekFrom};
            if !allowed {
                return None;
            }
            let mut f = std::fs::File::open(path).ok()?;
            f.seek(SeekFrom::Start(at)).ok()?;
            let mut buf = vec![];
            f.take(len as u64).read_to_end(&mut buf).ok()?;
            Some(buf)
        })
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let _ = cancelled;
        inspect(input.bytes, input.complete)
    }
}

fn compression(c: u32) -> String {
    match c {
        1 => "none".into(),
        2 => "CCITT RLE".into(),
        3 => "CCITT Group 3".into(),
        4 => "CCITT Group 4".into(),
        5 => "LZW".into(),
        6 | 7 => "JPEG".into(),
        8 | 32946 => "Deflate".into(),
        32773 => "PackBits".into(),
        34712 => "JPEG 2000".into(),
        34887 => "LERC".into(),
        34925 => "LZMA".into(),
        50000 => "Zstandard".into(),
        50001 => "WebP".into(),
        50002 => "JPEG XL".into(),
        n => format!("code {n}"),
    }
}

fn key_name(id: u16) -> String {
    match id {
        1024 => "GTModelType".into(),
        1025 => "GTRasterType".into(),
        1026 => "GTCitation".into(),
        2048 => "GeographicType".into(),
        2049 => "GeogCitation".into(),
        2050 => "GeogGeodeticDatum".into(),
        2051 => "GeogPrimeMeridian".into(),
        2052 => "GeogLinearUnits".into(),
        2054 => "GeogAngularUnits".into(),
        2056 => "GeogEllipsoid".into(),
        2057 => "GeogSemiMajorAxis".into(),
        2058 => "GeogSemiMinorAxis".into(),
        2059 => "GeogInvFlattening".into(),
        3072 => "ProjectedCSType".into(),
        3073 => "PCSCitation".into(),
        3074 => "Projection".into(),
        3075 => "ProjCoordTrans".into(),
        3076 => "ProjLinearUnits".into(),
        3078..=3095 => format!("Projection parameter ({id})"),
        4096 => "VerticalCSType".into(),
        4097 => "VerticalCitation".into(),
        4098 => "VerticalDatum".into(),
        4099 => "VerticalUnits".into(),
        _ => format!("Key {id}"),
    }
}

fn unit(code: u32) -> String {
    match code {
        9001 => "metre".into(),
        9002 => "foot".into(),
        9003 => "US survey foot".into(),
        9101 => "radian".into(),
        9102 => "degree".into(),
        9122 => "degree (supplier to define representation)".into(),
        32767 => "user-defined".into(),
        n => format!("code {n}"),
    }
}

/// A GeoKey's value: a number, numbers from GeoDoubleParams, or text from GeoAsciiParams.
#[derive(Clone, Debug)]
enum KeyValue {
    Short(u32),
    Doubles(Vec<f64>),
    Text(String),
}

/// From pixel (column, row) to model coordinates.
type PixelToModel = dyn Fn(f64, f64) -> [f64; 2];

fn inspect(b: &[u8], complete: bool) -> Result<Inspection, String> {
    let (t, first) = Tiff::open(b).ok_or("Not a TIFF file")?;
    let (entries, mut next) = t
        .directory(first)
        .ok_or("The first image directory is outside the file")?;
    let find = |tag: u16| entries.iter().find(|e| e.tag == tag);
    let num1 = |tag: u16| find(tag).and_then(|e| t.numbers(e).first().copied());
    let geo = find(GEO_KEY_DIRECTORY).ok_or("Not a GeoTIFF: no GeoKey directory")?;
    let mut out = Inspection::new("GeoTIFF", "GeoTIFF — georeferenced raster");
    out.version = Some(if t.big { "BigTIFF" } else { "TIFF 6.0" }.into());
    // Count the directories (overviews, other pages), bounded and loop-safe.
    let mut ifds = 1usize;
    let mut seen = vec![first];
    while next != 0 && ifds < MAX_IFDS && !seen.contains(&next) {
        seen.push(next);
        match t.directory(next) {
            Some((_, n)) => {
                ifds += 1;
                next = n;
            }
            None => break,
        }
    }
    let width = num1(256).unwrap_or(0.0) as u64;
    let height = num1(257).unwrap_or(0.0) as u64;
    let bits: Vec<String> = find(258)
        .map(|e| t.numbers(e))
        .unwrap_or_default()
        .iter()
        .take(16)
        .map(|v| v.to_string())
        .collect();
    let samples = num1(277).unwrap_or(1.0) as u32;
    let g = "Image";
    out.fact(
        g,
        "Byte order",
        if t.le {
            "little-endian (II)"
        } else {
            "big-endian (MM)"
        },
        Some(0..2),
    );
    out.fact(g, "Width", format!("{width} pixels"), None);
    out.fact(g, "Height", format!("{height} pixels"), None);
    out.fact(g, "Samples per pixel", samples.to_string(), None);
    out.fact(g, "Bits per sample", bits.join(", "), None);
    out.fact(
        g,
        "Sample format",
        match num1(339).map(|v| v as u32) {
            Some(2) => "signed integer",
            Some(3) => "floating point",
            Some(1) | None => "unsigned integer",
            _ => "other",
        },
        None,
    );
    out.fact(
        g,
        "Compression",
        compression(num1(259).unwrap_or(1.0) as u32),
        None,
    );
    out.fact(
        g,
        "Photometric",
        match num1(262).map(|v| v as u32) {
            Some(0) => "white is zero".into(),
            Some(1) => "black is zero (greyscale)".into(),
            Some(2) => "RGB".into(),
            Some(3) => "palette".into(),
            Some(5) => "CMYK".into(),
            Some(6) => "YCbCr".into(),
            Some(n) => format!("code {n}"),
            None => String::new(),
        },
        None,
    );
    out.fact(
        g,
        "Planar configuration",
        match num1(284).map(|v| v as u32) {
            Some(2) => "separate planes",
            _ => "interleaved",
        },
        None,
    );
    match (num1(322), num1(323)) {
        (Some(tw), Some(th)) => out.fact(g, "Layout", format!("tiles of {tw} × {th}"), None),
        _ => out.fact(
            g,
            "Layout",
            format!(
                "strips of {} rows",
                num1(278).map(|r| (r as u64).min(height)).unwrap_or(height)
            ),
            None,
        ),
    }
    out.fact(g, "Images in the file", ifds.to_string(), None);
    // GeoKeys.
    let dir: Vec<u32> = t.numbers(geo).iter().map(|v| *v as u32).collect();
    let doubles = find(GEO_DOUBLE_PARAMS)
        .map(|e| t.numbers(e))
        .unwrap_or_default();
    let ascii = find(GEO_ASCII_PARAMS)
        .map(|e| t.ascii(e))
        .unwrap_or_default();
    let mut keys: Vec<(u16, KeyValue)> = vec![];
    let mut consistent = dir.len() >= 4 && dir[0] == 1;
    if dir.len() >= 4 {
        let n = dir[3] as usize;
        if 4 + n * 4 > dir.len() {
            consistent = false;
        }
        for k in dir[4..].chunks_exact(4).take(n) {
            let (id, loc, count, value) = (k[0] as u16, k[1] as u16, k[2] as usize, k[3] as usize);
            let v = match loc {
                0 => KeyValue::Short(value as u32),
                GEO_DOUBLE_PARAMS => match doubles.get(value..value.saturating_add(count)) {
                    Some(d) => KeyValue::Doubles(d.to_vec()),
                    None => {
                        consistent = false;
                        continue;
                    }
                },
                GEO_ASCII_PARAMS => {
                    match ascii.get(value..value.saturating_add(count).min(ascii.len())) {
                        Some(s) => KeyValue::Text(s.trim_end_matches(['|', '\0']).to_owned()),
                        None => {
                            consistent = false;
                            continue;
                        }
                    }
                }
                _ => {
                    consistent = false;
                    continue;
                }
            };
            keys.push((id, v));
        }
    }
    let short = |id: u16| {
        keys.iter().find_map(|(k, v)| match v {
            KeyValue::Short(s) if *k == id => Some(*s),
            _ => None,
        })
    };
    let text = |id: u16| {
        keys.iter().find_map(|(k, v)| match v {
            KeyValue::Text(s) if *k == id => Some(s.clone()),
            _ => None,
        })
    };
    let g = "Georeferencing";
    let model = short(1024);
    out.fact(
        g,
        "Model type",
        match model {
            Some(1) => "projected".into(),
            Some(2) => "geographic (longitude/latitude)".into(),
            Some(3) => "geocentric".into(),
            Some(n) => format!("code {n}"),
            None => String::new(),
        },
        None,
    );
    out.fact(
        g,
        "Raster type",
        match short(1025) {
            Some(1) => "pixel is area",
            Some(2) => "pixel is point",
            _ => "",
        },
        None,
    );
    let projected = short(3072).filter(|c| *c != 0 && *c != 32767);
    let geographic = short(2048).filter(|c| *c != 0 && *c != 32767);
    let epsg = projected.or(geographic);
    if let Some(c) = projected {
        out.fact(g, "Projected CRS", epsg_label(c), None);
    }
    if let Some(c) = geographic {
        out.fact(g, "Geographic CRS", epsg_label(c), None);
    }
    if epsg.is_none() && (short(3072) == Some(32767) || short(2048) == Some(32767)) {
        out.fact(g, "CRS", "user-defined (described by its parameters)", None);
    }
    for (id, label) in [
        (1026, "Citation"),
        (3073, "Projection citation"),
        (2049, "Datum citation"),
    ] {
        if let Some(s) = text(id) {
            out.fact(g, label, s, None);
        }
    }
    if let Some(u) = short(3076) {
        out.fact(g, "Linear units", unit(u), None);
    }
    if let Some(u) = short(2054) {
        out.fact(g, "Angular units", unit(u), None);
    }
    if let Some(v) = short(4096).filter(|c| *c != 0) {
        out.fact(g, "Vertical CRS", epsg_label(v), None);
    }
    // Pixel to model coordinates.
    let scale = find(MODEL_PIXEL_SCALE)
        .map(|e| t.numbers(e))
        .unwrap_or_default();
    let ties = find(MODEL_TIEPOINT)
        .map(|e| t.numbers(e))
        .unwrap_or_default();
    let matrix = find(MODEL_TRANSFORMATION)
        .map(|e| t.numbers(e))
        .unwrap_or_default();
    let transform: Option<Box<PixelToModel>> = if matrix.len() >= 16 {
        let m = matrix.clone();
        out.fact(
            g,
            "Pixel size",
            format!(
                "{} × {} (with rotation terms {}, {})",
                num(m[0]),
                num(m[5]),
                num(m[1]),
                num(m[4])
            ),
            None,
        );
        Some(Box::new(move |i, j| {
            [m[0] * i + m[1] * j + m[3], m[4] * i + m[5] * j + m[7]]
        }))
    } else if ties.len() >= 6 && scale.len() >= 2 {
        let (ti, tj, tx, ty) = (ties[0], ties[1], ties[3], ties[4]);
        let (sx, sy) = (scale[0], scale[1]);
        out.fact(g, "Pixel size", format!("{} × {}", num(sx), num(sy)), None);
        out.fact(
            g,
            "Origin (upper left)",
            format!("{}, {}", num(tx - ti * sx), num(ty + tj * sy)),
            None,
        );
        Some(Box::new(move |i, j| {
            [tx + (i - ti) * sx, ty - (j - tj) * sy]
        }))
    } else {
        None
    };
    if ties.len() >= 12 && transform.is_none() {
        out.fact(
            g,
            "Control points",
            format!("{} tie points (no pixel scale)", ties.len() / 6),
            None,
        );
        out.notes.push(
            "The image is georeferenced by control points only; its bounds would need a fitted transformation and are not computed"
                .into(),
        );
    }
    let mut footprint: Option<Vec<[f64; 2]>> = None;
    let mut lonlat = false;
    if let Some(f) = &transform {
        let (w, h) = (width as f64, height as f64);
        let corners: Vec<[f64; 2]> = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h), (0.0, 0.0)]
            .iter()
            .map(|&(i, j)| f(i, j))
            .collect();
        let mut model_bounds = Bounds::default();
        corners.iter().for_each(|p| model_bounds.add(*p));
        out.fact(
            g,
            "Bounds (model coordinates)",
            model_bounds.describe(),
            None,
        );
        let as_lonlat: Option<Vec<[f64; 2]>> = if model == Some(2)
            || geographic.is_some() && projected.is_none()
            || epsg.is_some_and(epsg_geographic)
        {
            Some(corners.clone())
        } else {
            epsg.and_then(|c| corners.iter().map(|p| to_lonlat(c, p[0], p[1])).collect())
        };
        match as_lonlat {
            Some(ll) => {
                let mut b = Bounds::default();
                ll.iter().for_each(|p| b.add(*p));
                if b.fits_lonlat() {
                    out.fact(g, "Geographic bounds", b.describe_lonlat(), None);
                    footprint = Some(ll);
                    lonlat = true;
                } else {
                    footprint = Some(corners);
                }
            }
            None => {
                if projected.is_some() {
                    out.notes.push(
                        "Geographic bounds are computed for longitude/latitude, Web Mercator and UTM systems; this one is shown in its own coordinates"
                            .into(),
                    );
                }
                footprint = Some(corners);
            }
        }
    }
    if let Some(ring) = footprint {
        let mut map = Map::default();
        map.add(
            "Raster footprint",
            Parts {
                rings: vec![ring],
                ..Parts::default()
            },
        );
        map.finish(lonlat, &mut out.visuals, &mut out.notes);
    }
    // GDAL's own tags.
    if let Some(e) = find(GDAL_NODATA) {
        out.fact("GDAL", "No-data value", t.ascii(e).trim().to_owned(), None);
    }
    if let Some(e) = find(GDAL_METADATA) {
        let xml = t.ascii(e);
        let mut table = Table {
            name: "GDAL metadata".into(),
            columns: ["Name", "Band", "Role", "Value"]
                .map(str::to_owned)
                .to_vec(),
            ..Table::default()
        };
        for item in xml.split("<Item").skip(1).take(MAX_ROWS) {
            let attr = |n: &str| {
                item.split_once(&format!("{n}=\""))
                    .and_then(|(_, r)| r.split('"').next())
                    .unwrap_or("")
                    .to_owned()
            };
            let value = item
                .split_once('>')
                .and_then(|(_, r)| r.split("</Item>").next())
                .unwrap_or("")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&");
            table.rows.push(Row {
                cells: vec![attr("name"), attr("sample"), attr("role"), value],
                source: None,
            });
        }
        if !table.rows.is_empty() {
            out.tables.push(table);
        }
    }
    out.tables.insert(
        0,
        Table {
            name: "GeoKeys".into(),
            columns: ["Key", "Name", "Value"].map(str::to_owned).to_vec(),
            rows: keys
                .iter()
                .map(|(id, v)| Row {
                    cells: vec![
                        id.to_string(),
                        key_name(*id),
                        match v {
                            KeyValue::Short(s) => match id {
                                2048 | 3072 | 4096 if *s != 32767 && *s != 0 => epsg_label(*s),
                                2052 | 2054 | 3076 | 4099 => unit(*s),
                                _ => s.to_string(),
                            },
                            KeyValue::Doubles(d) => {
                                d.iter().map(|x| num(*x)).collect::<Vec<_>>().join(", ")
                            }
                            KeyValue::Text(s) => s.clone(),
                        },
                    ],
                    source: None,
                })
                .collect(),
            total: None,
        },
    );
    out.check(
        Stage::Parsed,
        if complete {
            State::Passed
        } else {
            State::Partial
        },
        "Read the first image directory and its GeoTIFF tags",
    );
    out.check(
        Stage::Semantic,
        if consistent { State::Passed } else { State::Failed },
        if consistent {
            "The GeoKey directory is version 1 and every key's value is where it says"
        } else {
            "The GeoKey directory is inconsistent (wrong version, or keys point outside their parameters)"
        },
    );
    out.notes.push(
        "The pixels are shown by the image view; this overview describes the georeferencing."
            .into(),
    );
    out.not_validated();
    Ok(out)
}
