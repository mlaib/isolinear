//! GeoJSON (RFC 7946, and the 2008 draft's `crs` member): features with their properties and
//! geometries, drawn on a longitude/latitude map. Coordinates are read as written; nothing is
//! reprojected.
use super::geo::{Bounds, MAX_NESTING, Map, Parts, TypeCounts, num};
use crate::inspect::{
    Inspection, MAX_ROWS, Row, Stage, State, Table,
    jsondoc::{Node, Value},
    spec::{F, JsonSpec},
};

/// Property columns in the features table.
const MAX_COLUMNS: usize = 50;
const GEOMETRIES: &[&str] = &[
    "Point",
    "MultiPoint",
    "LineString",
    "MultiLineString",
    "Polygon",
    "MultiPolygon",
    "GeometryCollection",
];

pub static GEOJSON: JsonSpec = JsonSpec {
    family: "GeoJSON",
    kind: "GeoJSON — geographic features",
    matches,
    version: |_| None,
    facts: &[F("Document", "Type", "type"), F("Document", "Name", "name")],
    tables: &[],
    custom: Some(custom),
};

/// The value of the top-level `"type"` member, read from the start of the file.
fn top_type(head: &str) -> Option<String> {
    let b = head.as_bytes();
    let (mut depth, mut i) = (0i32, 0usize);
    while i < b.len() {
        match b[i] {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                let key = head.get(start..i.min(b.len()))?;
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if depth == 1 && key == "type" && b.get(j) == Some(&b':') {
                    let rest = head.get(j + 1..)?.trim_start();
                    let value = rest.strip_prefix('"')?;
                    return value.split('"').next().map(str::to_owned);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn matches(keys: &[String], head: &str) -> bool {
    let has = |k: &str| keys.iter().any(|x| x == k);
    if !has("type") || !head.trim_start().starts_with('{') {
        return false;
    }
    match top_type(head).as_deref() {
        Some("FeatureCollection") => true,
        Some("Feature") => has("geometry") || has("properties"),
        Some(t) if GEOMETRIES.contains(&t) => has("coordinates") || has("geometries"),
        _ => false,
    }
}

fn number(n: &Node) -> Option<f64> {
    match &n.value {
        Value::Number(s) => s.parse().ok(),
        _ => None,
    }
}

/// A position: an array of two or more numbers (longitude, latitude, optional altitude).
fn position(n: &Node) -> Option<[f64; 2]> {
    let items = n.items();
    if items.len() < 2 {
        return None;
    }
    Some([number(&items[0])?, number(&items[1])?])
}

fn positions(n: &Node, problems: &mut Vec<String>, what: &str) -> Vec<[f64; 2]> {
    let mut out = vec![];
    for p in n.items() {
        match position(p) {
            Some(p) => out.push(p),
            None => {
                problems.push(format!("{what}: a position is not two numbers"));
                break;
            }
        }
    }
    out
}

fn ring(n: &Node, problems: &mut Vec<String>) -> Vec<[f64; 2]> {
    let r = positions(n, problems, "Polygon");
    if r.len() < 4 {
        problems.push("Polygon: a ring has fewer than four positions".into());
    } else if r.first() != r.last() {
        problems.push("Polygon: a ring is not closed (first and last positions differ)".into());
    }
    r
}

/// Parts of a geometry object; its type name, or `None` when it is missing or unknown.
fn geometry(
    g: &Node,
    depth: usize,
    parts: &mut Parts,
    problems: &mut Vec<String>,
) -> Option<&'static str> {
    if depth > MAX_NESTING {
        problems.push("Geometry collections nest too deeply".into());
        return None;
    }
    let kind = g.get("type").and_then(Node::str)?;
    let kind = GEOMETRIES
        .iter()
        .copied()
        .find(|k| *k == kind)
        .or_else(|| {
            problems.push(format!("Unknown geometry type \"{kind}\""));
            None
        })?;
    if kind == "GeometryCollection" {
        for member in g.get("geometries").map(Node::items).unwrap_or_default() {
            geometry(member, depth + 1, parts, problems);
        }
        return Some(kind);
    }
    let Some(c) = g.get("coordinates") else {
        problems.push(format!("{kind} without coordinates"));
        return Some(kind);
    };
    match kind {
        "Point" => match position(c) {
            Some(p) => parts.points.push(p),
            None if c.items().is_empty() => {}
            None => problems.push("Point: the position is not two numbers".into()),
        },
        "MultiPoint" => parts.points.extend(positions(c, problems, kind)),
        "LineString" => {
            let l = positions(c, problems, kind);
            if l.len() < 2 {
                problems.push("LineString with fewer than two positions".into());
            }
            parts.lines.push(l);
        }
        "MultiLineString" => {
            for l in c.items() {
                parts.lines.push(positions(l, problems, kind));
            }
        }
        "Polygon" => {
            for r in c.items() {
                parts.rings.push(ring(r, problems));
            }
        }
        _ => {
            for poly in c.items() {
                for r in poly.items() {
                    parts.rings.push(ring(r, problems));
                }
            }
        }
    }
    Some(kind)
}

fn feature_name(props: Option<&Node>) -> String {
    let Some(p) = props else {
        return String::new();
    };
    ["name", "Name", "NAME", "title", "Title"]
        .iter()
        .find_map(|k| p.get(k).filter(|v| v.is_scalar()).map(Node::text))
        .unwrap_or_default()
}

fn custom(root: &Node, out: &mut Inspection) {
    let kind = root.get("type").and_then(Node::str).unwrap_or("");
    // A feature collection, one feature, or a bare geometry treated as one feature.
    let features: Vec<&Node> = match kind {
        "FeatureCollection" => root
            .get("features")
            .map(|f| f.items().iter().collect())
            .unwrap_or_default(),
        _ => vec![root],
    };
    let mut keys: Vec<String> = vec![];
    let mut more_keys = false;
    for f in &features {
        if let Some(Value::Object(fields)) = f.get("properties").map(|p| &p.value) {
            for (k, _) in fields {
                if !keys.contains(k) {
                    if keys.len() < MAX_COLUMNS {
                        keys.push(k.clone());
                    } else {
                        more_keys = true;
                    }
                }
            }
        }
    }
    let mut table = Table {
        name: "Features".into(),
        columns: std::iter::once("id".to_owned())
            .chain(keys.iter().cloned())
            .chain(std::iter::once("geometry".to_owned()))
            .collect(),
        rows: vec![],
        total: (features.len() > MAX_ROWS).then_some(features.len() as u64),
    };
    let mut map = Map::default();
    let mut types = TypeCounts::default();
    let mut bounds = Bounds::default();
    let mut problems: Vec<String> = vec![];
    let mut without = 0usize;
    for (index, f) in features.iter().enumerate() {
        let bare = kind != "FeatureCollection" && kind != "Feature";
        let (geom, props) = if bare {
            (Some(*f), None)
        } else {
            if f.get("type").and_then(Node::str) != Some("Feature") {
                problems.push(format!(
                    "Feature {}: \"type\" is not \"Feature\"",
                    index + 1
                ));
            }
            (
                f.get("geometry").filter(|g| g.value != Value::Null),
                f.get("properties"),
            )
        };
        let mut parts = Parts::default();
        let mut mine = vec![];
        let gtype = match geom {
            Some(g) => geometry(g, 0, &mut parts, &mut mine),
            None => {
                without += 1;
                None
            }
        };
        problems.extend(
            mine.into_iter()
                .map(|p| format!("Feature {}: {p}", index + 1)),
        );
        types.add(gtype.unwrap_or("no geometry"));
        parts.bounds(&mut bounds);
        let name = feature_name(props);
        map.add(&name, parts);
        if table.rows.len() < MAX_ROWS {
            let mut cells = vec![f.get("id").map(Node::text).unwrap_or_default()];
            for k in &keys {
                cells.push(
                    props
                        .and_then(|p| p.get(k))
                        .map(Node::text)
                        .unwrap_or_default(),
                );
            }
            cells.push(gtype.unwrap_or("").to_owned());
            table.rows.push(Row {
                cells,
                source: Some(f.span.clone()),
            });
        }
    }
    out.fact(
        "Features",
        "Features",
        features.len().to_string(),
        Some(root.span.clone()),
    );
    out.fact("Features", "Geometry types", types.describe(), None);
    if without > 0 {
        out.fact("Features", "Without geometry", without.to_string(), None);
    }
    out.fact("Extent", "Bounding box", bounds.describe_lonlat(), None);
    if let Some(b) = root.get("bbox") {
        let values: Vec<String> = b
            .items()
            .iter()
            .map(|v| number(v).map(num).unwrap_or_else(|| v.text()))
            .collect();
        out.fact(
            "Extent",
            "Declared bounding box",
            values.join(", "),
            Some(b.span.clone()),
        );
    }
    // The 2008 draft's `crs` member (removed by RFC 7946, still written by some tools).
    let mut lonlat = true;
    if let Some(crs) = root.get("crs") {
        let props = crs.get("properties");
        let name = props
            .and_then(|p| p.get("name").or_else(|| p.get("href")))
            .map(Node::text)
            .unwrap_or_else(|| crs.text());
        lonlat = name.contains("CRS84") || name.ends_with(":4326") || name.ends_with("::4326");
        out.fact(
            "Extent",
            "CRS (legacy member)",
            name,
            Some(crs.span.clone()),
        );
        out.notes.push(
            "The file has a \"crs\" member (GeoJSON 2008); RFC 7946 removed it and requires WGS 84 longitude/latitude. Coordinates are shown as written."
                .into(),
        );
    }
    if more_keys {
        out.notes.push(format!(
            "The features table shows the first {MAX_COLUMNS} property names"
        ));
    }
    if !table.rows.is_empty() {
        out.tables.push(table);
    }
    map.finish(
        lonlat && (bounds.is_empty() || bounds.fits_lonlat()),
        &mut out.visuals,
        &mut out.notes,
    );
    if problems.is_empty() {
        out.check(
            Stage::Semantic,
            State::Passed,
            "Geometries are well formed (positions, rings closed with four or more positions)",
        );
    } else {
        out.check(
            Stage::Semantic,
            State::Failed,
            format!(
                "{} geometry problem{}; first: {}",
                problems.len(),
                if problems.len() == 1 { "" } else { "s" },
                problems[0]
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognizes_geojson_only() {
        let keys = |h: &str| crate::inspect::jsondoc::top_keys(h);
        let fc = r#"{"type": "FeatureCollection", "features": []}"#;
        assert!(matches(&keys(fc), fc));
        let p = r#"{"type":"Point","coordinates":[1,2]}"#;
        assert!(matches(&keys(p), p));
        let other = r#"{"type": "Point", "x": 1}"#;
        assert!(!matches(&keys(other), other));
        let nested = r#"{"data": {"type": "FeatureCollection"}, "type": "report"}"#;
        assert!(!matches(&keys(nested), nested));
    }
}
