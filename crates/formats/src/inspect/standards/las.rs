//! ASPRS LAS 1.0–1.4 point clouds and LAZ (LASzip-compressed LAS): the public header block,
//! the variable-length records (with the coordinate system they carry), and for uncompressed
//! LAS a sample of the points: classification counts and a top view (x/y in the file's
//! coordinates, drawn as a map that is not geographic). LAZ point data is not decompressed.
use super::geo::{Bounds, Map, Parts, epsg_label, num, wkt};
use crate::inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Row, Stage, State, Table};
use std::collections::BTreeMap;

/// Points sampled for classification counts and the top view.
pub const MAX_SAMPLE: u64 = 200_000;
/// Variable-length records listed at most.
const MAX_VLRS: u32 = 10_000;

fn u16le(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .unwrap_or(0)
}
fn u32le(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map(u32::from_le_bytes)
        .unwrap_or(0)
}
fn i32le(b: &[u8], at: usize) -> i32 {
    u32le(b, at) as i32
}
fn u64le(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8)
        .and_then(|s| s.try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0)
}
fn f64le(b: &[u8], at: usize) -> f64 {
    f64::from_bits(u64le(b, at))
}
fn cstr(b: &[u8], at: usize, len: usize) -> String {
    let s = b.get(at..at + len).unwrap_or_default();
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..end]).trim().to_owned()
}

/// ASPRS standard classes (LAS 1.4 R15, table 17).
pub fn class_name(c: u8) -> String {
    match c {
        0 => "Created, never classified".into(),
        1 => "Unclassified".into(),
        2 => "Ground".into(),
        3 => "Low vegetation".into(),
        4 => "Medium vegetation".into(),
        5 => "High vegetation".into(),
        6 => "Building".into(),
        7 => "Low point (noise)".into(),
        8 => "Model key-point / reserved".into(),
        9 => "Water".into(),
        10 => "Rail".into(),
        11 => "Road surface".into(),
        12 => "Overlap / reserved".into(),
        13 => "Wire — guard (shield)".into(),
        14 => "Wire — conductor (phase)".into(),
        15 => "Transmission tower".into(),
        16 => "Wire-structure connector".into(),
        17 => "Bridge deck".into(),
        18 => "High noise".into(),
        19..=63 => format!("Reserved ({c})"),
        _ => format!("User-defined ({c})"),
    }
}

pub struct Las;
impl Handler for Las {
    fn name(&self) -> &'static str {
        "LAS/LAZ"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        probe.head.starts_with(b"LASF") && probe.head.len() >= 227 && probe.head[24] == 1
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        inspect(input, cancelled)
    }
}

fn inspect(input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let b = input.bytes;
    if b.len() < 227 || !b.starts_with(b"LASF") {
        return Err("Not a LAS file, or its header is cut short".into());
    }
    let (major, minor) = (b[24], b[25]);
    let header_size = usize::from(u16le(b, 94));
    let point_offset = u32le(b, 96) as usize;
    let vlr_count = u32le(b, 100);
    let format_byte = b[104];
    // LASzip sets the two high bits of the point format.
    let compressed_bit = format_byte & 0xc0 != 0;
    let format = format_byte & 0x3f;
    let record_len = usize::from(u16le(b, 105));
    let legacy_count = u64::from(u32le(b, 107));
    let mut by_return: Vec<u64> = (0..5).map(|i| u64::from(u32le(b, 111 + i * 4))).collect();
    let scale = [f64le(b, 131), f64le(b, 139), f64le(b, 147)];
    let offset = [f64le(b, 155), f64le(b, 163), f64le(b, 171)];
    let (max, min) = (
        [f64le(b, 179), f64le(b, 195), f64le(b, 211)],
        [f64le(b, 187), f64le(b, 203), f64le(b, 219)],
    );
    let mut count = legacy_count;
    if minor >= 4 && header_size >= 375 && b.len() >= 375 {
        let full = u64le(b, 247);
        if full > 0 {
            count = full;
        }
        by_return = (0..15).map(|i| u64le(b, 255 + i * 8)).collect();
    }
    let mut out = Inspection::new("ASPRS LAS", "LAS point cloud");
    out.version = Some(format!("LAS {major}.{minor}"));
    let g = "Header";
    out.fact(g, "Version", format!("{major}.{minor}"), Some(24..26));
    out.fact(g, "System identifier", cstr(b, 26, 32), Some(26..58));
    out.fact(g, "Generating software", cstr(b, 58, 32), Some(58..90));
    let (day, year) = (u16le(b, 90), u16le(b, 92));
    if year > 0 {
        out.fact(
            g,
            "Creation date",
            format!("{year}, day {day} of the year"),
            Some(90..94),
        );
    }
    out.fact(g, "File source id", u16le(b, 4).to_string(), Some(4..6));
    out.fact(
        g,
        "Point data format",
        format!(
            "{format}{}",
            if compressed_bit {
                " (compressed bit set)"
            } else {
                ""
            }
        ),
        Some(104..105),
    );
    out.fact(
        g,
        "Point record length",
        format!("{record_len} bytes"),
        Some(105..107),
    );
    out.fact(g, "Points", count.to_string(), Some(107..111));
    out.fact(
        g,
        "Points by return",
        by_return
            .iter()
            .enumerate()
            .filter(|(_, n)| **n > 0)
            .map(|(i, n)| format!("{}: {n}", i + 1))
            .collect::<Vec<_>>()
            .join(", "),
        None,
    );
    out.fact(
        g,
        "Scale (x, y, z)",
        scale.iter().map(|v| num(*v)).collect::<Vec<_>>().join(", "),
        Some(131..155),
    );
    out.fact(
        g,
        "Offset (x, y, z)",
        offset
            .iter()
            .map(|v| num(*v))
            .collect::<Vec<_>>()
            .join(", "),
        Some(155..179),
    );
    out.fact(
        g,
        "Bounds",
        format!(
            "x {} — {}, y {} — {}, z {} — {}",
            num(min[0]),
            num(max[0]),
            num(min[1]),
            num(max[1]),
            num(min[2]),
            num(max[2])
        ),
        Some(179..227),
    );
    out.fact(
        g,
        "Variable-length records",
        vlr_count.to_string(),
        Some(100..104),
    );
    // Variable-length records.
    let mut vlrs = Table {
        name: "Variable-length records".into(),
        columns: ["User ID", "Record ID", "Length", "Description", "Meaning"]
            .map(str::to_owned)
            .to_vec(),
        ..Table::default()
    };
    let mut laszip = false;
    let mut at = header_size;
    for _ in 0..vlr_count.min(MAX_VLRS) {
        if at.saturating_add(54) > b.len() || at + 54 > point_offset {
            out.notes
                .push("The variable-length records run past the start of the points".into());
            break;
        }
        let user = cstr(b, at + 2, 16);
        let id = u16le(b, at + 18);
        let len = usize::from(u16le(b, at + 20));
        let desc = cstr(b, at + 22, 32);
        let data_at = at + 54;
        let data = b.get(data_at..data_at + len).unwrap_or_default();
        let mut meaning = String::new();
        match (user.as_str(), id) {
            ("laszip encoded", 22204) => {
                laszip = true;
                meaning = "LASzip compression parameters".into();
            }
            ("LASF_Projection", 2112) => {
                let text = String::from_utf8_lossy(data)
                    .trim_end_matches('\0')
                    .to_owned();
                if let Some(w) = wkt(&text) {
                    let label = match w.epsg {
                        Some(c) => format!("{} (EPSG:{c})", w.name),
                        None => w.name.clone(),
                    };
                    out.fact(
                        "Coordinate system",
                        "WKT",
                        label.clone(),
                        Some(data_at as u64..(data_at + len) as u64),
                    );
                    meaning = format!("OGC WKT: {label}");
                }
            }
            ("LASF_Projection", 34735) => {
                // GeoKey directory: version, revision, minor, count, then 4 shorts per key.
                let keys: Vec<u16> = data
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                let mut found = vec![];
                if keys.len() >= 4 {
                    for k in keys[4..].chunks_exact(4).take(usize::from(keys[3])) {
                        if k[1] == 0 && matches!(k[0], 3072 | 2048) && !matches!(k[3], 0 | 32767) {
                            found.push((k[0], u32::from(k[3])));
                        }
                    }
                }
                for (key, code) in &found {
                    out.fact(
                        "Coordinate system",
                        if *key == 3072 {
                            "Projected CRS (GeoKeys)"
                        } else {
                            "Geographic CRS (GeoKeys)"
                        },
                        epsg_label(*code),
                        Some(data_at as u64..(data_at + len) as u64),
                    );
                }
                meaning = "GeoTIFF GeoKey directory".into();
            }
            ("LASF_Projection", 34736) => meaning = "GeoTIFF double parameters".into(),
            ("LASF_Projection", 34737) => meaning = "GeoTIFF ASCII parameters".into(),
            ("LASF_Spec", 0) => meaning = "Classification lookup".into(),
            ("LASF_Spec", 3) => meaning = "Text area description".into(),
            ("LASF_Spec", 4) => meaning = "Extra bytes description".into(),
            ("LASF_Spec", 7) => meaning = "Superseded".into(),
            ("LASF_Spec", 100..=354) => meaning = "Waveform packet descriptor".into(),
            _ => {}
        }
        if vlrs.rows.len() < MAX_ROWS {
            vlrs.rows.push(Row {
                cells: vec![user, id.to_string(), len.to_string(), desc, meaning],
                source: Some(at as u64..(data_at + len) as u64),
            });
        }
        at = data_at + len;
    }
    if !vlrs.rows.is_empty() {
        out.tables.push(vlrs);
    }
    if laszip
        || compressed_bit
        || input
            .path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("laz"))
    {
        out.kind = "LAZ point cloud (LASzip-compressed LAS)".into();
        out.notes.push(
            "The points are LASzip-compressed; Isolinear reads the header and records but does not decompress points, so no classification counts or top view are shown."
                .into(),
        );
        out.check(
            Stage::Parsed,
            State::Partial,
            "Read the header and variable-length records; compressed points not decoded",
        );
        out.not_validated();
        return Ok(out);
    }
    // Uncompressed points: an even sample.
    let class_at = match format {
        0..=5 => 15,
        6..=10 => 16,
        _ => {
            out.notes.push(format!(
                "Point data format {format} is not known; points are not read"
            ));
            out.check(
                Stage::Parsed,
                State::Partial,
                "Header read; unknown point format",
            );
            out.not_validated();
            return Ok(out);
        }
    };
    let min_len = if format <= 5 { 20 } else { 30 };
    if record_len < min_len {
        return Err(format!(
            "Point records of {record_len} bytes are too short for format {format}"
        ));
    }
    let available = (b.len().saturating_sub(point_offset) / record_len) as u64;
    let present = count.min(available);
    if present < count {
        out.notes.push(format!(
            "The header declares {count} points; the file holds {present}"
        ));
    }
    let sample = present.min(MAX_SAMPLE);
    let step = if sample == 0 { 1 } else { present / sample };
    let mut classes: BTreeMap<u8, u64> = BTreeMap::new();
    let mut by_class: BTreeMap<u8, Vec<[f64; 2]>> = BTreeMap::new();
    let mut bounds = Bounds::default();
    let mut outside = 0u64;
    for k in 0..sample {
        if k.is_multiple_of(65_536) && cancelled() {
            return Err("Cancelled".into());
        }
        let i = (k * step) as usize;
        let at = point_offset + i * record_len;
        let raw = class_at + at;
        let c = b.get(raw).copied().unwrap_or(0);
        let class = if format <= 5 { c & 0x1f } else { c };
        *classes.entry(class).or_default() += 1;
        let x = f64::from(i32le(b, at)) * scale[0] + offset[0];
        let y = f64::from(i32le(b, at + 4)) * scale[1] + offset[1];
        let z = f64::from(i32le(b, at + 8)) * scale[2] + offset[2];
        let tol = |lo: f64, hi: f64, v: f64, s: f64| v >= lo - s.abs() && v <= hi + s.abs();
        if !(tol(min[0], max[0], x, scale[0])
            && tol(min[1], max[1], y, scale[1])
            && tol(min[2], max[2], z, scale[2]))
        {
            outside += 1;
        }
        bounds.add([x, y]);
        by_class.entry(class).or_default().push([x, y]);
    }
    out.fact(
        "Sample",
        "Points sampled",
        if sample == present {
            format!("all {present}")
        } else {
            format!("{sample} of {present} (every {step}th)")
        },
        None,
    );
    let mut table = Table {
        name: if sample == present {
            "Classifications"
        } else {
            "Classifications (sample)"
        }
        .into(),
        columns: ["Class", "Name", "Points", "Share"]
            .map(str::to_owned)
            .to_vec(),
        ..Table::default()
    };
    for (c, n) in &classes {
        table.rows.push(Row {
            cells: vec![
                c.to_string(),
                class_name(*c),
                n.to_string(),
                format!("{:.1} %", *n as f64 / sample.max(1) as f64 * 100.0),
            ],
            source: None,
        });
    }
    if !table.rows.is_empty() {
        out.tables.push(table);
    }
    let mut map = Map::default();
    for (c, pts) in by_class {
        map.add(
            &class_name(c),
            Parts {
                points: pts,
                ..Parts::default()
            },
        );
    }
    map.finish(false, &mut out.visuals, &mut out.notes);
    if sample > 0 {
        out.notes.push(
            "The map is a top view of the sampled points (x and y in the file's coordinates, height ignored), one colour layer per class."
                .into(),
        );
    }
    out.check(
        Stage::Parsed,
        if present == count && input.complete {
            State::Passed
        } else {
            State::Partial
        },
        format!("Read the header, variable-length records and {sample} sampled points"),
    );
    if sample > 0 {
        out.check(
            Stage::Semantic,
            if outside == 0 {
                State::Passed
            } else {
                State::Failed
            },
            if outside == 0 {
                "Every sampled point lies within the header's bounds".to_owned()
            } else {
                format!("{outside} of {sample} sampled points lie outside the header's bounds")
            },
        );
    }
    out.not_validated();
    Ok(out)
}
