//! OGC GeoPackage (12-128r18): a SQLite database with `gpkg_contents`, spatial reference
//! systems and geometry columns. The first feature table's geometries (GeoPackage binary:
//! a "GP" header and an envelope, then well-known binary) are decoded for the map and a
//! features table. Read with Isolinear's own SQLite page reader: no SQL is run.
use super::geo::{Bounds, Map, Parts, TypeCounts, epsg_geographic, epsg_label, wkb, wkt};
use crate::{
    inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Row, Stage, State, Table},
    table::sqlite::{Reader, Value},
};
use std::collections::HashMap;

/// Features of the first feature table decoded at most.
pub const MAX_FEATURES: usize = 50_000;
/// Rows of the metadata tables (contents, reference systems, geometry columns) read.
const MAX_META: usize = 10_000;

const GPKG: u32 = u32::from_be_bytes(*b"GPKG");
const GP10: u32 = u32::from_be_bytes(*b"GP10");
const GP11: u32 = u32::from_be_bytes(*b"GP11");

pub struct GeoPackage;
impl Handler for GeoPackage {
    fn name(&self) -> &'static str {
        "GeoPackage"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if !probe.head.starts_with(b"SQLite format 3\0") || probe.head.len() < 100 {
            return false;
        }
        let id = u32::from_be_bytes(probe.head[68..72].try_into().unwrap_or_default());
        matches!(id, GPKG | GP10 | GP11) || probe.ext_is(&["gpkg"])
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        inspect(input, cancelled)
    }
}

/// Rows of a metadata table as maps from lower-case column name to value.
pub fn rows_by_name(
    db: &mut Reader,
    table: &str,
    limit: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<(Vec<HashMap<String, Value>>, u64), String> {
    let columns: Vec<String> = db
        .columns(table)
        .ok_or_else(|| format!("The table {table} is missing"))?
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let mut out = vec![];
    let total = db.rows(table, limit, cancelled, &mut |row| {
        out.push(columns.iter().cloned().zip(row).collect());
        true
    })?;
    Ok((out, total))
}

fn text(row: &HashMap<String, Value>, key: &str) -> String {
    row.get(key).map(Value::display).unwrap_or_default()
}

/// A GeoPackage binary geometry: its type, parts and SRS id; `Ok(None)` for an empty one.
pub fn gp_geometry(b: &[u8]) -> Result<Option<(&'static str, Parts)>, String> {
    if b.len() < 8 || &b[..2] != b"GP" {
        return Err("not a GeoPackage geometry (no \"GP\" header)".into());
    }
    let flags = b[3];
    if flags & 0x20 != 0 {
        return Err("an extended (non-standard) geometry type".into());
    }
    let envelope = match (flags >> 1) & 0x07 {
        0 => 0,
        1 => 32,
        2 | 3 => 48,
        4 => 64,
        _ => return Err("an invalid envelope code".into()),
    };
    if flags & 0x10 != 0 {
        return Ok(None);
    }
    let body = b.get(8 + envelope..).ok_or("the geometry is cut short")?;
    let (kind, parts, _) = wkb(body)?;
    Ok(Some((kind, parts)))
}

fn inspect(input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let mut db = Reader::open(input.path)?;
    let has_contents = db.kind("gpkg_contents") == Some("table");
    let app = db.application_id();
    if !has_contents && !matches!(app, GPKG | GP10 | GP11) {
        return Err("Not a GeoPackage: no gpkg_contents table".into());
    }
    let mut out = Inspection::new("OGC GeoPackage", "GeoPackage — geospatial SQLite database");
    let uv = db.user_version();
    out.version = match app {
        GPKG if uv > 0 => Some(format!(
            "GeoPackage {}.{}.{}",
            uv / 10_000,
            uv / 100 % 100,
            uv % 100
        )),
        GP10 => Some("GeoPackage 1.0".into()),
        GP11 => Some("GeoPackage 1.1".into()),
        _ => None,
    };
    out.fact(
        "Database",
        "Application id",
        String::from_utf8_lossy(&app.to_be_bytes()).into_owned(),
        Some(68..72),
    );
    if !matches!(app, GPKG | GP10 | GP11) {
        out.notes.push(
            "The database header does not carry the GeoPackage application id (\"GPKG\")".into(),
        );
    }
    if db.wal_mode() {
        out.notes.push(
            "The database uses a write-ahead log; changes still in the -wal file are not shown here"
                .into(),
        );
    }
    // Spatial reference systems.
    let mut srs: HashMap<i64, (String, bool)> = HashMap::new();
    if db.kind("gpkg_spatial_ref_sys") == Some("table") {
        let (rows, total) = rows_by_name(&mut db, "gpkg_spatial_ref_sys", MAX_META, cancelled)?;
        let mut t = Table {
            name: "Spatial reference systems".into(),
            columns: [
                "SRS id",
                "Name",
                "Organization",
                "Code",
                "Kind",
                "Description",
            ]
            .map(str::to_owned)
            .to_vec(),
            total: (total as usize > rows.len()).then_some(total),
            ..Table::default()
        };
        for r in &rows {
            let id = r.get("srs_id").and_then(Value::as_i64).unwrap_or(0);
            let org = text(r, "organization");
            let code = r
                .get("organization_coordsys_id")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let def = wkt(&text(r, "definition"));
            let geographic = match &def {
                Some(w) => w.geographic,
                None => org.eq_ignore_ascii_case("EPSG") && epsg_geographic(code as u32),
            };
            let kind = match &def {
                Some(w) if w.geographic => "geographic",
                Some(w) if w.projected => "projected",
                _ if id == 0 || id == -1 => "undefined",
                _ => "",
            };
            srs.insert(id, (text(r, "srs_name"), geographic));
            t.rows.push(Row {
                cells: vec![
                    id.to_string(),
                    text(r, "srs_name"),
                    org,
                    code.to_string(),
                    kind.into(),
                    text(r, "description"),
                ],
                source: None,
            });
        }
        out.tables.push(t);
    } else {
        out.notes
            .push("The gpkg_spatial_ref_sys table is missing".into());
    }
    let srs_label = |id: i64| match srs.get(&id) {
        Some((name, _)) if !name.is_empty() => format!("{id} ({name})"),
        _ => id.to_string(),
    };
    // Geometry columns: table -> (column, type, srs).
    let mut geometry_columns: HashMap<String, (String, String, i64)> = HashMap::new();
    if db.kind("gpkg_geometry_columns") == Some("table") {
        let (rows, _) = rows_by_name(&mut db, "gpkg_geometry_columns", MAX_META, cancelled)?;
        let mut t = Table {
            name: "Geometry columns".into(),
            columns: ["Table", "Column", "Geometry type", "SRS", "Z", "M"]
                .map(str::to_owned)
                .to_vec(),
            ..Table::default()
        };
        let flag = |v: String| match v.as_str() {
            "0" => "prohibited".to_owned(),
            "1" => "mandatory".to_owned(),
            "2" => "optional".to_owned(),
            _ => v,
        };
        for r in &rows {
            let srs_id = r.get("srs_id").and_then(Value::as_i64).unwrap_or(0);
            geometry_columns.insert(
                text(r, "table_name").to_ascii_lowercase(),
                (
                    text(r, "column_name"),
                    text(r, "geometry_type_name"),
                    srs_id,
                ),
            );
            t.rows.push(Row {
                cells: vec![
                    text(r, "table_name"),
                    text(r, "column_name"),
                    text(r, "geometry_type_name"),
                    srs_label(srs_id),
                    flag(text(r, "z")),
                    flag(text(r, "m")),
                ],
                source: None,
            });
        }
        out.tables.push(t);
    }
    // Contents.
    let mut first_features: Option<(String, i64)> = None;
    let mut by_type = TypeCounts::default();
    let mut contents = Table {
        name: "Contents".into(),
        columns: [
            "Table",
            "Data type",
            "Identifier",
            "Description",
            "Bounds (min x, min y — max x, max y)",
            "SRS",
            "Rows",
            "Last change",
        ]
        .map(str::to_owned)
        .to_vec(),
        ..Table::default()
    };
    if has_contents {
        let (rows, _) = rows_by_name(&mut db, "gpkg_contents", MAX_META, cancelled)?;
        for r in &rows {
            let name = text(r, "table_name");
            let data_type = text(r, "data_type");
            by_type.add(&data_type);
            let srs_id = r.get("srs_id").and_then(Value::as_i64);
            if data_type == "features" && first_features.is_none() {
                first_features = Some((name.clone(), srs_id.unwrap_or(0)));
            }
            let mut b = Bounds::default();
            let v = |k: &str| r.get(k).and_then(Value::as_f64);
            if let (Some(a), Some(c), Some(d), Some(e)) =
                (v("min_x"), v("min_y"), v("max_x"), v("max_y"))
            {
                b.add([a, c]);
                b.add([d, e]);
            }
            let count = if db.kind(&name) == Some("table") {
                db.rows(&name, 0, cancelled, &mut |_| true)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|e| e)
            } else {
                String::new()
            };
            contents.rows.push(Row {
                cells: vec![
                    name,
                    data_type,
                    text(r, "identifier"),
                    text(r, "description"),
                    b.describe(),
                    srs_id.map(srs_label).unwrap_or_default(),
                    count,
                    text(r, "last_change"),
                ],
                source: None,
            });
        }
    }
    out.fact(
        "Contents",
        "Tables listed",
        contents.rows.len().to_string(),
        None,
    );
    out.fact("Contents", "Data types", by_type.describe(), None);
    out.tables.insert(0, contents);
    out.check(
        Stage::Parsed,
        State::Passed,
        "Read the GeoPackage tables with Isolinear's SQLite page reader (no SQL run)",
    );
    // The first feature table.
    if let Some((table, contents_srs)) = first_features {
        features(
            &mut db,
            &mut out,
            &table,
            contents_srs,
            &geometry_columns,
            &srs,
            cancelled,
        )?;
    } else {
        out.notes
            .push("No feature table is listed in gpkg_contents; nothing to draw".into());
    }
    out.not_validated();
    Ok(out)
}

fn features(
    db: &mut Reader,
    out: &mut Inspection,
    table: &str,
    contents_srs: i64,
    geometry_columns: &HashMap<String, (String, String, i64)>,
    srs: &HashMap<i64, (String, bool)>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    let Some(columns) = db.columns(table) else {
        out.notes
            .push(format!("The feature table \"{table}\" is missing"));
        return Ok(());
    };
    if db.kind(table) != Some("table") {
        out.notes.push(format!(
            "\"{table}\" is a {}; it cannot be read without running SQL",
            db.kind(table).unwrap_or("view")
        ));
        return Ok(());
    }
    let (geom_col, srs_id) = match geometry_columns.get(&table.to_ascii_lowercase()) {
        Some((c, _, s)) => (c.clone(), *s),
        None => {
            // No registration: the first column declared as a geometry type.
            let types = db.column_types(table).unwrap_or_default();
            let guess = columns
                .iter()
                .zip(&types)
                .find(|(_, t)| {
                    let t = t.to_ascii_uppercase();
                    ["GEOMETRY", "POINT", "LINESTRING", "POLYGON", "MULTI"]
                        .iter()
                        .any(|k| t.contains(k))
                })
                .map(|(c, _)| c.clone())
                .unwrap_or_default();
            (guess, contents_srs)
        }
    };
    let geom_index = columns
        .iter()
        .position(|c| c.eq_ignore_ascii_case(&geom_col));
    let name_index = columns.iter().position(|c| {
        let l = c.to_ascii_lowercase();
        l == "name" || l == "title"
    });
    let mut t = Table {
        name: format!("Features of {table}"),
        columns: columns
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != geom_index)
            .map(|(_, c)| c.clone())
            .chain(std::iter::once("geometry".to_owned()))
            .collect(),
        ..Table::default()
    };
    let mut map = Map::default();
    let mut bounds = Bounds::default();
    let mut types = TypeCounts::default();
    let mut bad = 0u64;
    let mut first_bad: Option<String> = None;
    let mut empty = 0u64;
    let total = db.rows(table, MAX_FEATURES, cancelled, &mut |row| {
        let mut kind = String::new();
        if let Some(gi) = geom_index {
            match row.get(gi) {
                Some(Value::Blob(b)) => match gp_geometry(b) {
                    Ok(Some((k, parts))) => {
                        kind = k.into();
                        parts.bounds(&mut bounds);
                        let name = name_index
                            .and_then(|i| row.get(i))
                            .map(Value::display)
                            .unwrap_or_default();
                        map.add(&name, parts);
                    }
                    Ok(None) => {
                        kind = "empty".into();
                        empty += 1;
                    }
                    Err(e) => {
                        bad += 1;
                        first_bad.get_or_insert(e);
                        kind = "unreadable".into();
                    }
                },
                Some(Value::Null) | None => kind = "no geometry".into(),
                Some(_) => {
                    bad += 1;
                    first_bad.get_or_insert("the geometry is not a BLOB".into());
                    kind = "unreadable".into();
                }
            }
        }
        types.add(&kind);
        if t.rows.len() < MAX_ROWS {
            let cells = row
                .iter()
                .enumerate()
                .filter(|(i, _)| Some(*i) != geom_index)
                .map(|(_, v)| v.display())
                .chain(std::iter::once(kind))
                .collect();
            t.rows.push(Row {
                cells,
                source: None,
            });
        }
        true
    })?;
    let read = t.rows.len() as u64;
    if total > read {
        t.total = Some(total);
        out.notes.push(format!(
            "The first {MAX_FEATURES} features of {table} are decoded; it has {total}"
        ));
    }
    out.fact("First feature table", "Table", table, None);
    out.fact("First feature table", "Geometry column", geom_col, None);
    out.fact("First feature table", "Features", total.to_string(), None);
    out.fact(
        "First feature table",
        "Geometry types",
        types.describe(),
        None,
    );
    out.fact("First feature table", "Bounds", bounds.describe(), None);
    let (srs_name, geographic) = match srs.get(&srs_id) {
        Some((n, g)) => (n.clone(), *g),
        None => (String::new(), srs_id == 4326),
    };
    out.fact(
        "First feature table",
        "Spatial reference system",
        if srs_name.is_empty() {
            epsg_label(srs_id.max(0) as u32)
        } else {
            format!("{srs_id} ({srs_name})")
        },
        None,
    );
    if empty > 0 {
        out.fact(
            "First feature table",
            "Empty geometries",
            empty.to_string(),
            None,
        );
    }
    out.tables.push(t);
    let geographic = geographic || (srs_id <= 0 && bounds.fits_lonlat());
    map.finish(geographic, &mut out.visuals, &mut out.notes);
    if bad == 0 {
        out.check(
            Stage::Semantic,
            State::Passed,
            format!("All {read} geometries read are valid GeoPackage binary"),
        );
    } else {
        out.check(
            Stage::Semantic,
            State::Failed,
            format!(
                "{bad} of {read} geometries cannot be decoded; first: {}",
                first_bad.unwrap_or_default()
            ),
        );
    }
    Ok(())
}
