//! Geographic standards (`inspect::standards`): GeoJSON, KML/KMZ, GPX, ESRI Shapefile,
//! GeoPackage, MBTiles, GeoTIFF and LAS/LAZ.
//!
//! Fixtures in tests/fixtures/standards: places.geojson, broken.geojson, places.kml and
//! trail.gpx are written by hand; the binary ones (roads.* and wells.* shapefiles, city.gpkg,
//! raster.mbtiles, vector.mbtiles, utm.tif, geographic-bigtiff.tif, plain.tif, grid.las,
//! wkt14.las, compressed.laz, zipped.kmz) come from the standard library only:
//!
//!     python3 -I tools/make-gis-fixtures.py
use isolinear_formats::{
    detect,
    inspect::{Geometry, Inspection, Stage, State, Table, Visual, inspect_file},
};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/standards")
        .join(name)
}

fn inspect(path: &Path) -> Option<Result<Inspection, String>> {
    let head = std::fs::read(path).unwrap();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let format = detect(&name, &head[..head.len().min(65536)]).format.id;
    inspect_file(path, format, &|| false)
}

fn open(name: &str) -> Inspection {
    inspect(&fixture(name))
        .unwrap_or_else(|| panic!("{name} not recognized"))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn fact<'a>(i: &'a Inspection, label: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.label == label)
        .map(|f| f.value.as_str())
        .unwrap_or_else(|| panic!("no fact {label:?} in {:#?}", i.facts))
}

fn check(i: &Inspection, stage: Stage, contains: &str) -> State {
    i.checks
        .iter()
        .find(|c| c.stage == stage && c.detail.contains(contains))
        .unwrap_or_else(|| panic!("no {stage:?} check {contains:?} in {:#?}", i.checks))
        .state
}

fn table<'a>(i: &'a Inspection, name: &str) -> &'a Table {
    i.tables.iter().find(|t| t.name == name).unwrap_or_else(|| {
        panic!(
            "no table {name:?}; tables: {:?}",
            i.tables.iter().map(|t| &t.name).collect::<Vec<_>>()
        )
    })
}

fn cell<'a>(t: &'a Table, row: usize, column: &str) -> &'a str {
    let c = t
        .columns
        .iter()
        .position(|c| c == column)
        .unwrap_or_else(|| panic!("no column {column:?} in {:?}", t.columns));
    &t.rows[row].cells[c]
}

fn note(i: &Inspection, contains: &str) -> bool {
    i.notes.iter().any(|n| n.contains(contains))
}

/// The map: (points, lines, rings, geographic) and the bounds of every coordinate.
fn map(i: &Inspection) -> ((usize, usize, usize), bool, [f64; 4]) {
    let (features, geographic) = i
        .visuals
        .iter()
        .find_map(|v| match v {
            Visual::Map {
                features,
                geographic,
            } => Some((features, *geographic)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no map in {:?}", i.kind));
    let (mut p, mut l, mut r) = (0, 0, 0);
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut add = |q: &[f64; 2]| {
        b = [
            b[0].min(q[0]),
            b[1].min(q[1]),
            b[2].max(q[0]),
            b[3].max(q[1]),
        ];
    };
    for f in features {
        match &f.geometry {
            Geometry::Points(pts) => {
                p += pts.len();
                pts.iter().for_each(&mut add);
            }
            Geometry::Lines(lines) => {
                l += lines.len();
                lines.iter().flatten().for_each(&mut add);
            }
            Geometry::Polygons(rings) => {
                r += rings.len();
                rings.iter().flatten().for_each(&mut add);
            }
        }
    }
    ((p, l, r), geographic, b)
}

fn close(a: f64, b: f64, tolerance: f64) -> bool {
    (a - b).abs() <= tolerance
}

#[test]
fn geojson_features_properties_and_every_geometry_type() {
    let i = open("places.geojson");
    assert_eq!(i.family, "GeoJSON");
    assert_eq!(fact(&i, "Name"), "Luxembourg places");
    assert_eq!(fact(&i, "Features"), "8");
    let types = fact(&i, "Geometry types");
    for t in [
        "Point × 1",
        "MultiPoint × 1",
        "LineString × 1",
        "MultiLineString × 1",
        "Polygon × 1",
        "MultiPolygon × 1",
        "GeometryCollection × 1",
        "no geometry × 1",
    ] {
        assert!(types.contains(t), "{types}");
    }
    assert_eq!(fact(&i, "Without geometry"), "1");
    assert_eq!(fact(&i, "Declared bounding box"), "6, 49.5, 6.2, 49.7");
    assert_eq!(
        fact(&i, "Bounding box"),
        "west 6, south 49.5, east 6.2, north 49.7"
    );
    let t = table(&i, "Features");
    assert_eq!(t.rows.len(), 8);
    assert_eq!(
        t.columns,
        [
            "id",
            "name",
            "kind",
            "visitors",
            "title",
            "length_km",
            "nested",
            "geometry"
        ]
    );
    assert_eq!(cell(t, 0, "id"), "p1");
    assert_eq!(cell(t, 1, "id"), "2");
    assert_eq!(cell(t, 2, "name"), "Pétrusse walk");
    assert_eq!(cell(t, 4, "geometry"), "Polygon");
    // Rows point at their feature in the file.
    let text = std::fs::read_to_string(fixture("places.geojson")).unwrap();
    let src = t.rows[2].source.clone().unwrap();
    assert!(text[src.start as usize..src.end as usize].starts_with('{'));
    assert!(text[src.start as usize..src.end as usize].contains("Pétrusse"));
    // 1 + 2 + 1 points; 1 + 2 + 1 lines; 2 + 2 rings.
    let (counts, geographic, b) = map(&i);
    assert_eq!(counts, (4, 4, 4));
    assert!(geographic);
    assert_eq!(b, [6.0, 49.5, 6.2, 49.7]);
    // Feature names from "name" or "title".
    let names: Vec<&str> = match &i.visuals[0] {
        Visual::Map { features, .. } => features.iter().map(|f| f.name.as_str()).collect(),
        _ => vec![],
    };
    assert!(names.contains(&"Wells") && names.contains(&"Cathedral"));
    assert_eq!(check(&i, Stage::Semantic, "well formed"), State::Passed);
    assert_eq!(check(&i, Stage::Schema, ""), State::NotDone);
}

#[test]
fn geojson_rule_failures_and_the_legacy_crs_member() {
    let i = open("broken.geojson");
    assert_eq!(
        fact(&i, "CRS (legacy member)"),
        "urn:ogc:def:crs:OGC:1.3:CRS84"
    );
    assert!(note(&i, "RFC 7946 removed it"));
    assert_eq!(check(&i, Stage::Semantic, "not closed"), State::Failed);
    let detail = &i
        .checks
        .iter()
        .find(|c| c.stage == Stage::Semantic)
        .unwrap()
        .detail;
    assert!(detail.starts_with("3 geometry problems"), "{detail}");
    // JSON that only mentions a feature collection deeper down is not claimed.
    let dir = temp("geojson");
    let other = dir.join("other.json");
    std::fs::write(
        &other,
        r#"{"report": {"type": "FeatureCollection"}, "type": "x"}"#,
    )
    .unwrap();
    assert!(inspect(&other).is_none());
    let point = dir.join("point.json");
    std::fs::write(&point, r#"{"type": "Point", "coordinates": [6.1, 49.6]}"#).unwrap();
    let i = inspect(&point).unwrap().unwrap();
    assert_eq!(fact(&i, "Geometry types"), "Point × 1");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn kml_outline_placemarks_geometries_and_links_not_fetched() {
    let i = open("places.kml");
    assert_eq!(i.family, "KML");
    assert_eq!(i.version.as_deref(), Some("KML 2.2"));
    assert_eq!(fact(&i, "Name"), "Field trip");
    assert_eq!(fact(&i, "Placemarks"), "6");
    assert_eq!(fact(&i, "Folders"), "2");
    assert_eq!(fact(&i, "Network links"), "1");
    let outline: Vec<(u16, &str, &str)> = i
        .outline
        .iter()
        .map(|o| (o.depth, o.label.as_str(), o.value.as_str()))
        .collect();
    assert_eq!(outline[0], (0, "Document", "Field trip"));
    assert_eq!(outline[1], (1, "Folder", "Sights"));
    assert_eq!(outline[2], (2, "Placemark", "Cathedral"));
    assert!(outline.contains(&(1, "NetworkLink", "Weather")));
    let t = table(&i, "Placemarks");
    assert_eq!(t.rows.len(), 6);
    assert_eq!(cell(t, 0, "Description"), "Gothic cathedral & crypt");
    assert_eq!(cell(t, 0, "Style"), "#red");
    assert_eq!(cell(t, 0, "Folder"), "Sights");
    assert_eq!(cell(t, 1, "Coordinates"), "3");
    assert_eq!(cell(t, 2, "Geometry"), "Polygon");
    assert_eq!(cell(t, 2, "Coordinates"), "8");
    assert_eq!(cell(t, 3, "Geometry"), "MultiGeometry");
    assert_eq!(cell(t, 4, "Geometry"), "LinearRing");
    assert_eq!(cell(t, 5, "Geometry"), "gx:Track");
    assert_eq!(cell(t, 5, "Coordinates"), "2");
    // Points: Cathedral, the MultiGeometry's point; lines: Corniche, MultiGeometry, track;
    // rings: the park's two and the fence.
    let (counts, geographic, b) = map(&i);
    assert_eq!(counts, (2, 3, 3));
    assert!(geographic);
    assert_eq!(b, [6.0, 49.5, 6.14, 49.63]);
    assert!(note(&i, "Not fetched"));
    assert!(note(&i, "https://example.com/weather.kml"));
    assert_eq!(check(&i, Stage::Semantic, "readable"), State::Passed);
}

#[test]
fn kmz_reads_its_kml_and_lists_the_other_entries() {
    let i = open("zipped.kmz");
    assert!(i.kind.starts_with("KMZ"), "{}", i.kind);
    assert_eq!(fact(&i, "KML document"), "doc.kml");
    assert_eq!(fact(&i, "Name"), "Zipped places");
    let entries = table(&i, "Archive entries");
    assert_eq!(entries.rows.len(), 1);
    assert_eq!(cell(entries, 0, "Name"), "files/pin.png");
    let (counts, _, _) = map(&i);
    assert_eq!(counts, (1, 0, 0));
    assert!(note(&i, "http://example.com/live.kml"));
    // A KMZ without KML is an error, not a panic.
    let dir = temp("kmz");
    let bad = dir.join("empty.kmz");
    let zip = std::fs::read(fixture("zipped.kmz")).unwrap();
    let cut = &zip[..zip.len() - 30];
    std::fs::write(&bad, cut).unwrap();
    assert!(inspect(&bad).unwrap().is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn gpx_tracks_distances_elevation_times_and_profile() {
    let i = open("trail.gpx");
    assert_eq!(i.family, "GPX");
    assert_eq!(i.version.as_deref(), Some("GPX 1.1"));
    assert_eq!(fact(&i, "Name"), "Morning loop");
    assert_eq!(fact(&i, "Author"), "A. Walker");
    assert_eq!(fact(&i, "Time"), "2026-05-01T07:55:00Z");
    assert_eq!(fact(&i, "Waypoints"), "2");
    let w = table(&i, "Waypoints");
    assert_eq!(cell(w, 1, "Description"), "Bench & view");
    let r = table(&i, "Routes");
    // 0.01° of latitude: about 1.112 km.
    assert_eq!(cell(r, 0, "Distance (km)"), "1.112");
    let t = table(&i, "Tracks");
    assert_eq!(cell(t, 0, "Name"), "Recorded");
    assert_eq!(cell(t, 0, "Segments"), "2");
    assert_eq!(cell(t, 0, "Points"), "5");
    // Two 111 m steps in the first segment, one in the second; the gap is not travelled.
    assert_eq!(cell(t, 0, "Distance (km)"), "0.334");
    // Gain within segments 5 + 8, loss 3 (the jump between segments is not counted).
    assert_eq!(cell(t, 0, "Elevation gain (m)"), "13");
    assert_eq!(cell(t, 0, "Elevation loss (m)"), "3");
    assert_eq!(cell(t, 0, "Start"), "2026-05-01 08:00:00 UTC");
    assert_eq!(cell(t, 0, "Duration"), "12 min 31 s");
    assert_eq!(cell(t, 1, "Duration"), "");
    assert_eq!(cell(t, 1, "Elevation gain (m)"), "");
    // Waypoints as points; the route, two segments and the second track as lines.
    let (counts, geographic, _) = map(&i);
    assert_eq!(counts, (2, 4, 0));
    assert!(geographic);
    let profile = i
        .visuals
        .iter()
        .find_map(|v| match v {
            Visual::Plot { channels } => Some(&channels[0]),
            _ => None,
        })
        .unwrap();
    assert!(profile.name.contains("Recorded"));
    assert_eq!(profile.unit, "m");
    assert_eq!(profile.samples[0], 300.0);
    assert!(close(*profile.samples.last().unwrap() as f64, 318.0, 0.5));
    let max = profile.samples.iter().copied().fold(f32::MIN, f32::max);
    assert!(close(max as f64, 318.0, 0.5));
    assert!(note(&i, "one second stands for one kilometre"));
    assert_eq!(check(&i, Stage::Semantic, "in order"), State::Passed);
}

#[test]
fn shapefile_with_all_sidecars() {
    let i = open("roads.shp");
    assert_eq!(i.family, "ESRI Shapefile");
    assert_eq!(fact(&i, "Shape type"), "PolyLine");
    assert_eq!(fact(&i, "Records"), "3");
    assert_eq!(fact(&i, "Bounding box"), "6.09, 49.58 — 6.16, 49.64");
    assert_eq!(fact(&i, "Name"), "GCS_WGS_1984");
    assert!(fact(&i, "Kind").starts_with("geographic"));
    assert_eq!(
        fact(&i, "Text encoding"),
        "UTF-8, from the .cpg file (\"UTF-8\")"
    );
    assert_eq!(fact(&i, "Deleted records"), "1");
    let t = table(&i, "Records");
    assert_eq!(
        t.columns,
        [
            "Record", "Geometry", "Parts", "Points", "Deleted", "NAME", "LANES", "OPENED", "LIT"
        ]
    );
    assert_eq!(cell(t, 0, "NAME"), "Rue de l'Église");
    assert_eq!(cell(t, 0, "OPENED"), "1998-04-12");
    assert_eq!(cell(t, 0, "LIT"), "true");
    assert_eq!(cell(t, 1, "Parts"), "2");
    assert_eq!(cell(t, 1, "Points"), "4");
    assert_eq!(cell(t, 2, "Deleted"), "deleted");
    assert_eq!(cell(t, 2, "LIT"), "");
    let fields = table(&i, "Fields");
    assert_eq!(cell(fields, 1, "Type"), "N (numeric)");
    let (counts, geographic, b) = map(&i);
    assert_eq!(counts, (0, 4, 0));
    assert!(geographic);
    assert_eq!(b, [6.09, 49.58, 6.16, 49.64]);
    assert_eq!(check(&i, Stage::Parsed, "Every shape"), State::Passed);
    assert_eq!(
        check(&i, Stage::Semantic, ".shp 3, .shx 3, .dbf 3"),
        State::Passed
    );
}

#[test]
fn projected_point_z_shapefile_in_windows_1252() {
    let i = open("wells.shp");
    assert_eq!(fact(&i, "Shape type"), "PointZ");
    assert_eq!(fact(&i, "Z range"), "118 — 120.5");
    assert_eq!(fact(&i, "Name"), "WGS_1984_UTM_Zone_32N (EPSG:32632)");
    assert_eq!(fact(&i, "Kind"), "projected");
    assert!(fact(&i, "Text encoding").starts_with("Windows-1252"));
    assert!(fact(&i, "Sidecars missing").contains(".cpg"));
    let t = table(&i, "Records");
    assert_eq!(cell(t, 0, "OWNER"), "Müller");
    assert_eq!(cell(t, 1, "OWNER"), "Société €");
    let (counts, geographic, _) = map(&i);
    assert_eq!(counts, (2, 0, 0));
    assert!(!geographic);
}

#[test]
fn shapefile_sidecars_missing_dbf_alone_and_index_alone() {
    let dir = temp("shp");
    // The .shp alone: a note for each missing sidecar, shapes still shown.
    let shp = dir.join("roads.shp");
    std::fs::copy(fixture("roads.shp"), &shp).unwrap();
    let i = inspect(&shp).unwrap().unwrap();
    assert_eq!(fact(&i, "Sidecars missing"), ".shx, .dbf, .prj, .cpg");
    assert!(note(&i, "Missing next to the .shp"));
    assert_eq!(map(&i).0, (0, 4, 0));
    // Without a .prj the coordinates fit longitude/latitude.
    assert!(map(&i).1);
    assert!(i.checks.iter().all(|c| c.stage != Stage::Semantic));
    // A .dbf whose record count disagrees with the shapes fails the rule.
    std::fs::copy(fixture("wells.dbf"), dir.join("roads.dbf")).unwrap();
    let i = inspect(&shp).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Semantic, "Record counts"), State::Failed);
    // The .dbf alone shows its table.
    let i = open("roads.dbf");
    assert_eq!(i.family, "dBASE");
    let t = table(&i, "Records");
    assert_eq!(t.rows.len(), 3);
    assert_eq!(cell(t, 1, "NAME"), "Boulevard Royal");
    assert_eq!(cell(t, 1, "LANES"), "4");
    assert!(note(&i, "open the .shp"));
    // The .shx alone explains itself.
    let i = open("roads.shx");
    assert!(i.kind.contains("index"));
    assert_eq!(fact(&i, "Records indexed"), "3");
    assert!(note(&i, "only indexes"));
    assert_eq!(cell(table(&i, "Index entries"), 0, "Offset (bytes)"), "100");
    // A file named .dbf that is not dBASE is not claimed.
    let fake = dir.join("fake.dbf");
    std::fs::write(&fake, b"hello, this is not a table at all, honestly").unwrap();
    assert!(inspect(&fake).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn geopackage_contents_reference_systems_and_features() {
    let i = open("city.gpkg");
    assert_eq!(i.family, "OGC GeoPackage");
    assert_eq!(i.version.as_deref(), Some("GeoPackage 1.3.0"));
    assert_eq!(fact(&i, "Application id"), "GPKG");
    assert_eq!(fact(&i, "Tables listed"), "3");
    assert_eq!(fact(&i, "Data types"), "features × 2, attributes × 1");
    let c = table(&i, "Contents");
    assert_eq!(cell(c, 0, "Table"), "poi");
    assert_eq!(cell(c, 0, "Identifier"), "Points of interest");
    assert_eq!(cell(c, 0, "Rows"), "4");
    assert_eq!(cell(c, 0, "SRS"), "4326 (WGS 84 geodetic)");
    assert_eq!(
        cell(c, 0, "Bounds (min x, min y — max x, max y)"),
        "6.1, 49.59 — 6.16, 49.63"
    );
    let srs = table(&i, "Spatial reference systems");
    assert_eq!(srs.rows.len(), 3);
    assert_eq!(cell(srs, 2, "Kind"), "geographic");
    let g = table(&i, "Geometry columns");
    assert_eq!(cell(g, 1, "Column"), "shape");
    assert_eq!(cell(g, 1, "Z"), "prohibited");
    assert_eq!(fact(&i, "Table"), "poi");
    assert_eq!(fact(&i, "Geometry column"), "geom");
    assert_eq!(fact(&i, "Empty geometries"), "1");
    let f = table(&i, "Features of poi");
    assert_eq!(f.columns, ["fid", "name", "kind", "rating", "geometry"]);
    assert_eq!(
        f.rows[0].cells,
        ["1", "Cathedral", "church", "4.5", "Point"]
    );
    assert_eq!(cell(f, 1, "rating"), "4.0");
    assert_eq!(cell(f, 2, "geometry"), "LineString");
    assert_eq!(cell(f, 3, "geometry"), "empty");
    let (counts, geographic, b) = map(&i);
    assert_eq!(counts, (2, 1, 0));
    assert!(geographic);
    assert_eq!(b, [6.10, 49.59, 6.16, 49.63]);
    assert_eq!(
        check(&i, Stage::Semantic, "valid GeoPackage binary"),
        State::Passed
    );
    // An ordinary SQLite database is not claimed.
    assert!(
        inspect(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/sqlite-xlsx/shop.sqlite")
        )
        .is_none()
    );
}

#[test]
fn mbtiles_metadata_zoom_levels_and_a_tile_picture() {
    let i = open("raster.mbtiles");
    assert_eq!(i.family, "MBTiles");
    assert_eq!(fact(&i, "Name"), "Test raster");
    assert_eq!(fact(&i, "Format"), "png");
    assert_eq!(fact(&i, "Bounds"), "5.7,49.4,6.6,50.2");
    assert_eq!(fact(&i, "Tiles"), "5");
    assert_eq!(fact(&i, "Zoom levels present"), "0 to 1 (2 levels)");
    let z = table(&i, "Zoom levels");
    assert_eq!(z.rows[1].cells[..4], ["1", "4", "0 – 1", "0 – 1"]);
    assert_eq!(cell(z, 1, "Coverage"), "100.0000 % of the world");
    let (bytes, caption) = i
        .visuals
        .iter()
        .find_map(|v| match v {
            Visual::Picture { bytes, caption } => Some((bytes, caption)),
            _ => None,
        })
        .unwrap();
    assert!(bytes.starts_with(b"\x89PNG"));
    assert!(caption.starts_with("Tile 0/0/0"), "{caption}");
    let (counts, geographic, b) = map(&i);
    assert_eq!(counts, (0, 0, 1));
    assert!(geographic);
    assert_eq!(b, [5.7, 49.4, 6.6, 50.2]);
    assert_eq!(check(&i, Stage::Semantic, "Zoom range"), State::Passed);
}

#[test]
fn mbtiles_view_over_map_and_images_with_vector_tiles() {
    let i = open("vector.mbtiles");
    assert_eq!(fact(&i, "Tiles"), "3");
    assert!(note(&i, "view over the map and images"));
    assert!(note(&i, "vector tiles"));
    assert!(
        i.visuals
            .iter()
            .all(|v| !matches!(v, Visual::Picture { .. }))
    );
    // The metadata says zoom 0 to 3; the tiles stop at 1.
    assert_eq!(check(&i, Stage::Semantic, "Zoom range"), State::Failed);
}

#[test]
fn geotiff_projected_with_gdal_tags() {
    let i = open("utm.tif");
    assert_eq!(i.family, "GeoTIFF");
    assert_eq!(i.version.as_deref(), Some("TIFF 6.0"));
    assert_eq!(fact(&i, "Width"), "4 pixels");
    assert_eq!(fact(&i, "Height"), "3 pixels");
    assert_eq!(fact(&i, "Bits per sample"), "8");
    assert_eq!(fact(&i, "Compression"), "none");
    assert_eq!(fact(&i, "Layout"), "strips of 3 rows");
    assert_eq!(fact(&i, "Model type"), "projected");
    assert_eq!(fact(&i, "Raster type"), "pixel is area");
    assert_eq!(
        fact(&i, "Projected CRS"),
        "EPSG:32632 — WGS 84 / UTM zone 32N"
    );
    assert_eq!(fact(&i, "Citation"), "WGS 84 / UTM zone 32N");
    assert_eq!(fact(&i, "Pixel size"), "30 × 30");
    assert_eq!(
        fact(&i, "Bounds (model coordinates)"),
        "500000, 5539910 — 500120, 5540000"
    );
    // 500000 E is the zone's central meridian, 9°E.
    assert!(fact(&i, "Geographic bounds").starts_with("west 9, south 50.01"));
    assert_eq!(fact(&i, "No-data value"), "0");
    let gdal = table(&i, "GDAL metadata");
    assert_eq!(
        gdal.rows[1].cells,
        ["STATISTICS_MEAN", "0", "statistics", "5.5"]
    );
    let keys = table(&i, "GeoKeys");
    assert_eq!(keys.rows.len(), 4);
    assert_eq!(cell(keys, 3, "Name"), "ProjectedCSType");
    let (counts, geographic, b) = map(&i);
    assert_eq!(counts, (0, 0, 1));
    assert!(geographic);
    assert!(
        close(b[0], 9.0, 1e-6) && close(b[3], 50.0127, 1e-3),
        "{b:?}"
    );
    assert_eq!(check(&i, Stage::Semantic, "version 1"), State::Passed);
}

#[test]
fn geotiff_big_endian_bigtiff_geographic_and_plain_tiff_not_claimed() {
    let i = open("geographic-bigtiff.tif");
    assert_eq!(i.version.as_deref(), Some("BigTIFF"));
    assert_eq!(fact(&i, "Byte order"), "big-endian (MM)");
    assert_eq!(fact(&i, "Geographic CRS"), "EPSG:4326 — WGS 84");
    assert_eq!(fact(&i, "Raster type"), "pixel is point");
    assert_eq!(
        fact(&i, "Geographic bounds"),
        "west 5, south 49.25, east 6, north 50"
    );
    assert!(map(&i).1);
    assert!(inspect(&fixture("plain.tif")).is_none());
}

#[test]
fn las_header_vlrs_classes_and_top_view() {
    let i = open("grid.las");
    assert_eq!(i.family, "ASPRS LAS");
    assert_eq!(i.version.as_deref(), Some("LAS 1.2"));
    assert_eq!(fact(&i, "System identifier"), "ISOLINEAR TEST");
    assert_eq!(fact(&i, "Generating software"), "make-gis-fixtures.py");
    assert_eq!(fact(&i, "Creation date"), "2026, day 290 of the year");
    assert_eq!(fact(&i, "Point data format"), "1");
    assert_eq!(fact(&i, "Points"), "1000");
    assert_eq!(fact(&i, "Points by return"), "1: 1000");
    assert_eq!(fact(&i, "Scale (x, y, z)"), "0.01, 0.01, 0.01");
    assert_eq!(
        fact(&i, "Projected CRS (GeoKeys)"),
        "EPSG:32632 — WGS 84 / UTM zone 32N"
    );
    assert_eq!(fact(&i, "Points sampled"), "all 1000");
    let vlr = table(&i, "Variable-length records");
    assert_eq!(vlr.rows[0].cells[..2], ["LASF_Projection", "34735"]);
    let classes = table(&i, "Classifications");
    let rows: Vec<(&str, &str, &str)> = classes
        .rows
        .iter()
        .map(|r| {
            (
                r.cells[0].as_str(),
                r.cells[1].as_str(),
                r.cells[2].as_str(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("2", "Ground", "334"),
            ("5", "High vegetation", "333"),
            ("6", "Building", "333")
        ]
    );
    let (counts, geographic, b) = map(&i);
    assert_eq!(counts, (1000, 0, 0));
    assert!(!geographic);
    assert!(close(b[0], 500_000.0, 1e-6) && close(b[2], 500_009.0, 1e-6));
    assert!(close(b[1], 5_540_000.0, 1e-6) && close(b[3], 5_540_049.5, 1e-6));
    assert!(note(&i, "top view"));
    assert_eq!(
        check(&i, Stage::Semantic, "within the header's bounds"),
        State::Passed
    );
}

#[test]
fn las_14_with_wkt_and_laz_header_only() {
    let i = open("wkt14.las");
    assert_eq!(i.version.as_deref(), Some("LAS 1.4"));
    assert_eq!(fact(&i, "Points"), "4");
    assert_eq!(fact(&i, "WKT"), "WGS_1984_UTM_Zone_32N (EPSG:32632)");
    let classes = table(&i, "Classifications");
    assert_eq!(classes.rows.len(), 3);
    assert_eq!(cell(classes, 2, "Name"), "High noise");
    let i = open("compressed.laz");
    assert!(i.kind.starts_with("LAZ"));
    assert_eq!(fact(&i, "Point data format"), "1 (compressed bit set)");
    assert_eq!(
        table(&i, "Variable-length records").rows[0].cells[4],
        "LASzip compression parameters"
    );
    assert!(note(&i, "does not decompress"));
    assert_eq!(
        check(&i, Stage::Parsed, "compressed points"),
        State::Partial
    );
    assert!(i.visuals.is_empty());
}

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("isolinear-gis-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const ALL: &[&str] = &[
    "places.geojson",
    "places.kml",
    "zipped.kmz",
    "trail.gpx",
    "roads.shp",
    "roads.dbf",
    "roads.shx",
    "wells.shp",
    "city.gpkg",
    "raster.mbtiles",
    "vector.mbtiles",
    "utm.tif",
    "geographic-bigtiff.tif",
    "grid.las",
    "wkt14.las",
    "compressed.laz",
];

#[test]
fn truncated_inputs_do_not_panic() {
    let dir = temp("cut");
    for name in ALL {
        let bytes = std::fs::read(fixture(name)).unwrap();
        let ext = Path::new(name).extension().unwrap().to_str().unwrap();
        for cut in [
            0,
            1,
            7,
            50,
            100,
            101,
            227,
            300,
            bytes.len() / 3,
            bytes.len() / 2,
            bytes.len() - 1,
        ] {
            let cut = cut.min(bytes.len());
            let p = dir.join(format!("cut{cut}.{ext}"));
            std::fs::write(&p, &bytes[..cut]).unwrap();
            let started = std::time::Instant::now();
            let _ = inspect(&p);
            assert!(started.elapsed().as_secs() < 2, "{name} cut at {cut}");
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn randomly_damaged_inputs_do_not_panic() {
    let dir = temp("mutated");
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for name in ALL {
        let bytes = std::fs::read(fixture(name)).unwrap();
        let ext = Path::new(name).extension().unwrap().to_str().unwrap();
        for round in 0..60 {
            let mut b = bytes.clone();
            for _ in 0..1 + round % 8 {
                let at = (next() % b.len() as u64) as usize;
                b[at] = (next() >> 24) as u8;
            }
            // Sometimes a 4-byte field becomes huge or negative.
            if round % 5 == 0 && b.len() > 8 {
                let at = (next() % (b.len() as u64 - 4)) as usize;
                b[at..at + 4].copy_from_slice(&[0xff, 0xff, 0xff, 0x7f]);
            }
            let p = dir.join(format!("m{round}.{ext}"));
            std::fs::write(&p, &b).unwrap();
            let started = std::time::Instant::now();
            let _ = inspect(&p);
            assert!(started.elapsed().as_secs() < 3, "{name} round {round}");
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}
