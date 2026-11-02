//! MBTiles 1.x (Mapbox): map tiles in a SQLite database, with a `metadata` table and a `tiles`
//! table (or a `tiles` view over `map` and `images`, read from those tables since no SQL is
//! run). Shows the metadata, tiles per zoom level, and the first tile of the lowest zoom as a
//! picture when it is PNG, JPEG or WebP; vector tiles are not rendered.
use super::geo::{Bounds, Map, Parts};
use super::gpkg::rows_by_name;
use crate::{
    inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Row, Stage, State, Table, Visual},
    table::sqlite::{Reader, Value},
};
use std::collections::BTreeMap;

const MBTILES: u32 = u32::from_be_bytes(*b"MPBX");
/// Largest tile shown as a picture.
const MAX_TILE: usize = 16 * 1024 * 1024;

pub struct MbTiles;
impl Handler for MbTiles {
    fn name(&self) -> &'static str {
        "MBTiles"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if !probe.head.starts_with(b"SQLite format 3\0") || probe.head.len() < 100 {
            return false;
        }
        let id = u32::from_be_bytes(probe.head[68..72].try_into().unwrap_or_default());
        id == MBTILES || probe.ext_is(&["mbtiles"])
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        inspect(input, cancelled)
    }
}

/// The picture format of tile bytes, from their signature.
fn sniff(b: &[u8]) -> &'static str {
    if b.starts_with(b"\x89PNG") {
        "png"
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpg"
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "webp"
    } else if b.starts_with(&[0x1f, 0x8b]) {
        "gzip"
    } else {
        ""
    }
}

#[derive(Default)]
struct Zoom {
    tiles: u64,
    cols: (i64, i64),
    rows: (i64, i64),
}

fn inspect(input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let mut db = Reader::open(input.path)?;
    if db.kind("metadata").is_none() || db.kind("tiles").is_none() {
        return Err("Not an MBTiles database: the metadata and tiles tables are missing".into());
    }
    let mut out = Inspection::new("MBTiles", "MBTiles — map tiles in SQLite");
    // Metadata.
    let mut meta: Vec<(String, String)> = vec![];
    if db.kind("metadata") == Some("table") {
        let (rows, _) = rows_by_name(&mut db, "metadata", 10_000, cancelled)?;
        for r in rows {
            let get = |k: &str| r.get(k).map(Value::display).unwrap_or_default();
            meta.push((get("name"), get("value")));
        }
    }
    let m = |k: &str| {
        meta.iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    out.version = Some(m("version"))
        .filter(|v| !v.is_empty())
        .map(|v| format!("tileset version {v}"));
    for (label, key) in [
        ("Name", "name"),
        ("Description", "description"),
        ("Format", "format"),
        ("Type", "type"),
        ("Bounds", "bounds"),
        ("Center", "center"),
        ("Minimum zoom", "minzoom"),
        ("Maximum zoom", "maxzoom"),
        ("Attribution", "attribution"),
    ] {
        out.fact("Metadata", label, m(key), None);
    }
    out.tables.push(Table {
        name: "Metadata".into(),
        columns: vec!["Name".into(), "Value".into()],
        rows: meta
            .iter()
            .map(|(n, v)| Row {
                cells: vec![
                    n.clone(),
                    if v.chars().count() > 2000 {
                        format!(
                            "{}… ({} characters)",
                            v.chars().take(2000).collect::<String>(),
                            v.chars().count()
                        )
                    } else {
                        v.clone()
                    },
                ],
                source: None,
            })
            .collect(),
        total: None,
    });
    // Tiles per zoom level, and the first tile of the lowest zoom.
    let mut zooms: BTreeMap<i64, Zoom> = BTreeMap::new();
    let mut tile: Option<(i64, i64, i64, Vec<u8>)> = None;
    let count = |z: i64, x: i64, y: i64, zooms: &mut BTreeMap<i64, Zoom>| {
        let e = zooms.entry(z).or_insert(Zoom {
            tiles: 0,
            cols: (x, x),
            rows: (y, y),
        });
        e.tiles += 1;
        e.cols = (e.cols.0.min(x), e.cols.1.max(x));
        e.rows = (e.rows.0.min(y), e.rows.1.max(y));
    };
    let lower = |z: i64, t: &Option<(i64, i64, i64, Vec<u8>)>| t.as_ref().is_none_or(|t| z < t.0);
    if db.kind("tiles") == Some("table") {
        let cols = db.columns("tiles").unwrap_or_default();
        let at = |n: &str| cols.iter().position(|c| c.eq_ignore_ascii_case(n));
        let (zi, xi, yi, di) = (
            at("zoom_level"),
            at("tile_column"),
            at("tile_row"),
            at("tile_data"),
        );
        db.rows("tiles", usize::MAX, cancelled, &mut |row| {
            let v = |i: Option<usize>| i.and_then(|i| row.get(i)).and_then(Value::as_i64);
            let (z, x, y) = (
                v(zi).unwrap_or(-1),
                v(xi).unwrap_or(-1),
                v(yi).unwrap_or(-1),
            );
            count(z, x, y, &mut zooms);
            if lower(z, &tile)
                && let Some(b) = di.and_then(|i| row.get(i)).and_then(Value::as_blob)
            {
                tile = Some((z, x, y, b.to_vec()));
            }
            true
        })?;
    } else if db.kind("map") == Some("table") && db.kind("images") == Some("table") {
        out.notes.push(
            "\"tiles\" is a view over the map and images tables; they were read directly (no SQL is run)"
                .into(),
        );
        let cols = db.columns("map").unwrap_or_default();
        let at = |n: &str| cols.iter().position(|c| c.eq_ignore_ascii_case(n));
        let (zi, xi, yi, ii) = (
            at("zoom_level"),
            at("tile_column"),
            at("tile_row"),
            at("tile_id"),
        );
        let mut wanted: Option<(i64, i64, i64, String)> = None;
        db.rows("map", usize::MAX, cancelled, &mut |row| {
            let v = |i: Option<usize>| i.and_then(|i| row.get(i)).and_then(Value::as_i64);
            let (z, x, y) = (
                v(zi).unwrap_or(-1),
                v(xi).unwrap_or(-1),
                v(yi).unwrap_or(-1),
            );
            count(z, x, y, &mut zooms);
            if wanted.as_ref().is_none_or(|w| z < w.0) {
                let id = ii
                    .and_then(|i| row.get(i))
                    .map(Value::display)
                    .unwrap_or_default();
                wanted = Some((z, x, y, id));
            }
            true
        })?;
        if let Some((z, x, y, id)) = wanted {
            let cols = db.columns("images").unwrap_or_default();
            let at = |n: &str| cols.iter().position(|c| c.eq_ignore_ascii_case(n));
            let (di, ii) = (at("tile_data"), at("tile_id"));
            db.rows("images", usize::MAX, cancelled, &mut |row| {
                let this = ii.and_then(|i| row.get(i)).map(Value::display);
                if this.as_deref() == Some(id.as_str()) {
                    if let Some(b) = di.and_then(|i| row.get(i)).and_then(Value::as_blob) {
                        tile = Some((z, x, y, b.to_vec()));
                    }
                    return false;
                }
                true
            })?;
        }
    } else {
        out.notes.push(
            "\"tiles\" is a view Isolinear cannot read without running SQL; tile counts are not shown"
                .into(),
        );
    }
    let total: u64 = zooms.values().map(|z| z.tiles).sum();
    out.fact("Tiles", "Tiles", total.to_string(), None);
    out.fact(
        "Tiles",
        "Zoom levels present",
        match (zooms.keys().next(), zooms.keys().next_back()) {
            (Some(a), Some(b)) if a == b => a.to_string(),
            (Some(a), Some(b)) => format!("{a} to {b} ({} levels)", zooms.len()),
            _ => String::new(),
        },
        None,
    );
    let mut zt = Table {
        name: "Zoom levels".into(),
        columns: ["Zoom", "Tiles", "Columns", "Rows (TMS)", "Coverage"]
            .map(str::to_owned)
            .to_vec(),
        ..Table::default()
    };
    for (z, info) in zooms.iter().take(MAX_ROWS) {
        let side = if (0..=30).contains(z) { 1u64 << z } else { 0 };
        zt.rows.push(Row {
            cells: vec![
                z.to_string(),
                info.tiles.to_string(),
                format!("{} – {}", info.cols.0, info.cols.1),
                format!("{} – {}", info.rows.0, info.rows.1),
                if side > 0 {
                    format!(
                        "{:.4} % of the world",
                        info.tiles as f64 / (side as f64 * side as f64) * 100.0
                    )
                } else {
                    String::new()
                },
            ],
            source: None,
        });
    }
    if !zt.rows.is_empty() {
        out.tables.push(zt);
    }
    // The bounds as a rectangle on a longitude/latitude map.
    let b: Vec<f64> = m("bounds")
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    if let [w, s, e, n] = b[..] {
        let mut bb = Bounds::default();
        bb.add([w, s]);
        bb.add([e, n]);
        if bb.fits_lonlat() {
            let mut map = Map::default();
            map.add(
                "Tileset bounds",
                Parts {
                    rings: vec![vec![[w, s], [e, s], [e, n], [w, n], [w, s]]],
                    ..Parts::default()
                },
            );
            map.finish(true, &mut out.visuals, &mut out.notes);
        }
    }
    let format = m("format").to_ascii_lowercase();
    match tile {
        Some((z, x, y, bytes)) => {
            let kind = sniff(&bytes);
            if matches!(kind, "png" | "jpg" | "webp") && bytes.len() <= MAX_TILE {
                out.visuals.push(Visual::Picture {
                    bytes,
                    caption: format!("Tile {z}/{x}/{y} (lowest zoom, TMS row)"),
                });
            } else if format == "pbf" || format == "mvt" || kind == "gzip" {
                out.notes.push(
                    "The tiles are vector tiles (Mapbox Vector Tile, protocol buffers); they are not rendered"
                        .into(),
                );
            } else {
                out.notes.push(format!(
                    "The first tile is not a PNG, JPEG or WebP picture (format \"{format}\"); it is not shown"
                ));
            }
        }
        None if total > 0 => out.notes.push("No tile data could be read".into()),
        None => {}
    }
    out.check(
        Stage::Parsed,
        State::Passed,
        "Read the metadata and tiles tables with Isolinear's SQLite page reader (no SQL run)",
    );
    // Declared zoom range against the tiles present.
    let declared = (
        m("minzoom").trim().parse::<i64>(),
        m("maxzoom").trim().parse::<i64>(),
    );
    if let ((Ok(lo), Ok(hi)), Some(a), Some(b)) =
        (declared, zooms.keys().next(), zooms.keys().next_back())
    {
        out.check(
            Stage::Semantic,
            if lo == *a && hi == *b {
                State::Passed
            } else {
                State::Failed
            },
            format!("Zoom range: metadata says {lo} to {hi}, tiles cover {a} to {b}"),
        );
    }
    out.not_validated();
    Ok(out)
}
