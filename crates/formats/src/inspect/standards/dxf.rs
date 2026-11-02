//! AutoCAD DXF drawings (ASCII group-code/value pairs): HEADER variables, the LAYER table,
//! blocks, and entities drawn in XY (LINE, LWPOLYLINE with bulges, POLYLINE/VERTEX, CIRCLE,
//! ARC, ELLIPSE, SPLINE as its control polygon, POINT; TEXT/MTEXT/INSERT positions as points).
//! Block references are listed, not expanded. Binary DXF is recognized and not read.
use super::cad::{Drawing, Rows, arc, clip, count_table, ellipse, lines, num};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};
use std::collections::{HashMap, HashSet};

pub struct Dxf;

const BINARY: &[u8] = b"AutoCAD Binary DXF";
/// Group pairs kept in one record.
const MAX_PAIRS: usize = 1_000_000;

fn starts_like_dxf(text: &str) -> bool {
    let first: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(4)
        .collect();
    matches!(
        first.as_slice(),
        ["0", "SECTION", ..] | ["999", _, "0", "SECTION"]
    )
}

impl Handler for Dxf {
    fn name(&self) -> &'static str {
        "DXF"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if probe.head.starts_with(BINARY) {
            return true;
        }
        let Some(text) = probe.text() else {
            return false;
        };
        starts_like_dxf(text)
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        if input.bytes.starts_with(BINARY) {
            let mut out = Inspection::new("DXF", "AutoCAD drawing exchange (binary DXF)");
            out.check(
                Stage::Parsed,
                State::NotDone,
                "Binary DXF is not supported; only ASCII DXF is read",
            );
            out.notes.push(
                "This is a binary DXF file. Isolinear reads ASCII DXF only; the raw views still show the bytes.".into(),
            );
            return Ok(out);
        }
        let text = input.text()?;
        inspect(&text, cancelled)
    }
}

fn acad_version(v: &str) -> Option<&'static str> {
    Some(match v {
        "AC1006" => "AutoCAD R10",
        "AC1009" => "AutoCAD R11/R12",
        "AC1012" => "AutoCAD R13",
        "AC1014" => "AutoCAD R14",
        "AC1015" => "AutoCAD 2000",
        "AC1018" => "AutoCAD 2004",
        "AC1021" => "AutoCAD 2007",
        "AC1024" => "AutoCAD 2010",
        "AC1027" => "AutoCAD 2013",
        "AC1032" => "AutoCAD 2018",
        _ => return None,
    })
}

fn insunits(n: i64) -> Option<&'static str> {
    Some(match n {
        0 => "unitless",
        1 => "inches",
        2 => "feet",
        3 => "miles",
        4 => "millimetres",
        5 => "centimetres",
        6 => "metres",
        7 => "kilometres",
        8 => "microinches",
        9 => "mils",
        10 => "yards",
        11 => "ångströms",
        12 => "nanometres",
        13 => "micrometres",
        14 => "decimetres",
        15 => "decametres",
        16 => "hectometres",
        17 => "gigametres",
        18 => "astronomical units",
        19 => "light years",
        20 => "parsecs",
        _ => return None,
    })
}

/// Polyline vertices with the bulge of the segment that starts at each.
type Vertices = Vec<([f64; 2], f64)>;
/// A POLYLINE waiting for its VERTEX records and SEQEND: its flags and vertices.
type OpenPolyline = Option<(i64, Vertices)>;

/// One record: the type after group 0, its pairs, and its byte span.
struct Record<'a> {
    kind: &'a str,
    pairs: Vec<(i32, &'a str)>,
    span: std::ops::Range<u64>,
}

impl<'a> Record<'a> {
    fn get(&self, code: i32) -> Option<&'a str> {
        self.pairs.iter().find(|p| p.0 == code).map(|p| p.1)
    }
    fn f(&self, code: i32) -> f64 {
        self.get(code)
            .and_then(|v| v.trim().parse().ok())
            .filter(|v: &f64| v.is_finite())
            .unwrap_or(0.0)
    }
    fn i(&self, code: i32) -> i64 {
        self.get(code)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    }
    fn xy(&self, x: i32) -> [f64; 2] {
        [self.f(x), self.f(x + 10)]
    }
    /// Repeated points (10/20 or 11/21 pairs), with the bulge (42) after each vertex.
    fn points(&self, x: i32) -> Vec<([f64; 2], f64)> {
        let mut pts: Vec<([f64; 2], f64)> = vec![];
        for &(c, v) in &self.pairs {
            let n = v.trim().parse::<f64>().unwrap_or(0.0);
            let n = if n.is_finite() { n } else { 0.0 };
            if c == x {
                pts.push(([n, 0.0], 0.0));
            } else if c == x + 10 {
                if let Some(p) = pts.last_mut() {
                    p.0[1] = n;
                }
            } else if c == 42
                && x == 10
                && let Some(p) = pts.last_mut()
            {
                p.1 = n;
            }
        }
        pts
    }
}

/// Points along a polyline whose segments may be arcs (bulge = tan(angle / 4)).
fn bulged(vertices: &[([f64; 2], f64)], closed: bool) -> Vec<[f64; 2]> {
    let mut out = vec![];
    let n = vertices.len();
    let segments = if closed { n } else { n.saturating_sub(1) };
    for k in 0..segments {
        let (p, b) = vertices[k];
        let (q, _) = vertices[(k + 1) % n];
        if out.is_empty() {
            out.push(p);
        }
        if b.abs() < 1e-9 {
            out.push(q);
            continue;
        }
        let theta = 4.0 * b.atan();
        let d = [q[0] - p[0], q[1] - p[1]];
        let c = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if c == 0.0 {
            continue;
        }
        let h = c / (2.0 * (theta / 2.0).tan());
        let mid = [(p[0] + q[0]) / 2.0, (p[1] + q[1]) / 2.0];
        let centre = [mid[0] - d[1] / c * h, mid[1] + d[0] / c * h];
        let r = ((p[0] - centre[0]).powi(2) + (p[1] - centre[1]).powi(2)).sqrt();
        let a0 = (p[1] - centre[1]).atan2(p[0] - centre[0]);
        out.extend(arc(centre, r, a0, a0 + theta).into_iter().skip(1));
    }
    out
}

fn pt(p: [f64; 2]) -> String {
    format!("({}, {})", num(p[0]), num(p[1]))
}

fn inspect(text: &str, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let mut out = Inspection::new("DXF", "AutoCAD drawing exchange");
    // Pairs into records.
    let mut records: Vec<Record> = vec![];
    let mut it = lines(text);
    let (mut bad, mut eof) = (0u64, false);
    let mut n = 0u64;
    while let Some((at, code)) = it.next() {
        n += 1;
        if n.is_multiple_of(65536) && cancelled() {
            return Err("Cancelled".into());
        }
        let Some((vat, value)) = it.next() else {
            if !code.trim().is_empty() {
                bad += 1;
            }
            break;
        };
        let value = value.trim_end_matches(['\r', ' ']);
        let value = if value.trim().is_empty() {
            value
        } else {
            value.trim_start()
        };
        let Ok(code) = code.trim().parse::<i32>() else {
            bad += 1;
            continue;
        };
        let end = (vat + value.len()) as u64;
        if code == 0 {
            if value == "EOF" {
                eof = true;
                break;
            }
            records.push(Record {
                kind: value,
                pairs: vec![],
                span: at as u64..end,
            });
        } else if let Some(r) = records.last_mut() {
            if r.pairs.len() < MAX_PAIRS {
                r.pairs.push((code, value));
            }
            r.span.end = end;
        }
    }
    if records.is_empty() {
        return Err("No DXF records were found".into());
    }
    // Walk the sections.
    let mut section = "";
    let mut table = "";
    let mut header: Vec<(String, String, std::ops::Range<u64>)> = vec![];
    let mut layers = Rows::new(
        "Layers",
        &["Layer", "Colour", "Line type", "Frozen", "Off", "Locked"],
    );
    let mut layer_names: HashSet<String> = HashSet::new();
    let mut has_layer_table = false;
    let mut blocks = Rows::new("Blocks", &["Block", "Base point", "Entities"]);
    let mut block_names: HashSet<String> = HashSet::new();
    let mut current_block: Option<(String, [f64; 2], u64, std::ops::Range<u64>)> = None;
    let mut entities = Rows::new(
        "Entities",
        &["Type", "Layer", "Handle", "Coordinates", "Text or block"],
    );
    let mut kinds: HashMap<String, u64> = HashMap::new();
    let mut drawing = Drawing::default();
    let mut used_layers: HashSet<String> = HashSet::new();
    let mut inserted: HashSet<String> = HashSet::new();
    let (mut splines, mut inserts, mut meshes) = (0u64, 0u64, 0u64);
    let mut polyline: OpenPolyline = None;
    for r in &records {
        match r.kind {
            "SECTION" => {
                section = r.get(2).unwrap_or("");
                if section == "HEADER" {
                    read_header(r, &mut header);
                }
                continue;
            }
            "ENDSEC" => {
                section = "";
                continue;
            }
            _ => {}
        }
        match section {
            "TABLES" => match r.kind {
                "TABLE" => {
                    table = r.get(2).unwrap_or("");
                    if table == "LAYER" {
                        has_layer_table = true;
                    }
                }
                "ENDTAB" => table = "",
                "LAYER" if table == "LAYER" => {
                    let name = r.get(2).unwrap_or("").to_owned();
                    let colour = r.i(62);
                    let flags = r.i(70);
                    layer_names.insert(name.clone());
                    layers.push(
                        vec![
                            name,
                            colour.abs().to_string(),
                            r.get(6).unwrap_or("").to_owned(),
                            if flags & 1 != 0 { "yes" } else { "" }.into(),
                            if colour < 0 { "yes" } else { "" }.into(),
                            if flags & 4 != 0 { "yes" } else { "" }.into(),
                        ],
                        Some(r.span.clone()),
                    );
                }
                _ => {}
            },
            "BLOCKS" => match r.kind {
                "BLOCK" => {
                    let name = r.get(2).unwrap_or("").to_owned();
                    block_names.insert(name.clone());
                    current_block = Some((name, r.xy(10), 0, r.span.clone()));
                }
                "ENDBLK" => {
                    if let Some((name, base, count, span)) = current_block.take() {
                        blocks.push(vec![name, pt(base), count.to_string()], Some(span));
                    }
                }
                _ => {
                    if let Some(b) = &mut current_block
                        && !matches!(r.kind, "VERTEX" | "SEQEND" | "ATTRIB")
                    {
                        b.2 += 1;
                    }
                }
            },
            "ENTITIES" => {
                if r.kind == "VERTEX" {
                    if let Some((_, v)) = &mut polyline {
                        // Polyface face records (flag 128 without 64) are not vertices.
                        let flags = r.i(70);
                        if (flags & 128 == 0 || flags & 64 != 0) && v.len() < 10_000_000 {
                            v.push((r.xy(10), r.f(42)));
                        }
                    }
                    continue;
                }
                if r.kind == "SEQEND" {
                    if let Some((flags, v)) = polyline.take() {
                        if flags & (16 | 64) != 0 {
                            meshes += 1;
                        } else {
                            drawing.path(bulged(&v, flags & 1 != 0));
                        }
                    }
                    continue;
                }
                if r.kind == "ATTRIB" {
                    continue;
                }
                *kinds.entry(r.kind.to_owned()).or_default() += 1;
                let layer = r.get(8).unwrap_or("0").to_owned();
                if used_layers.len() < 100_000 {
                    used_layers.insert(layer.clone());
                }
                let (coords, label) = entity(r, &mut drawing, &mut polyline);
                match r.kind {
                    "SPLINE" => splines += 1,
                    "INSERT" => {
                        inserts += 1;
                        if let Some(b) = r.get(2)
                            && inserted.len() < 100_000
                        {
                            inserted.insert(b.to_owned());
                        }
                    }
                    _ => {}
                }
                entities.push(
                    vec![
                        r.kind.to_owned(),
                        layer,
                        r.get(5).unwrap_or("").to_owned(),
                        coords,
                        clip(&label, 120),
                    ],
                    Some(r.span.clone()),
                );
            }
            _ => {}
        }
    }
    // Header facts.
    let var = |name: &str| header.iter().find(|h| h.0 == name);
    let g = "Header";
    let mut unit = "drawing units".to_owned();
    if let Some((_, v, span)) = var("$ACADVER") {
        let name = acad_version(v)
            .map(|n| format!("{v} ({n})"))
            .unwrap_or(v.clone());
        out.version = Some(name.clone());
        out.fact(g, "Version", name, Some(span.clone()));
    }
    if let Some((_, v, span)) = var("$INSUNITS")
        && let Some(u) = v.trim().parse().ok().and_then(insunits)
    {
        out.fact(g, "Units", u, Some(span.clone()));
        if u != "unitless" {
            unit = u.to_owned();
        }
    }
    if let Some((_, v, span)) = var("$MEASUREMENT") {
        let m = match v.trim() {
            "0" => "imperial",
            "1" => "metric",
            _ => "",
        };
        out.fact(g, "Measurement", m, Some(span.clone()));
    }
    for (name, label) in [
        ("$EXTMIN", "Extents min"),
        ("$EXTMAX", "Extents max"),
        ("$DWGCODEPAGE", "Code page"),
    ] {
        if let Some((_, v, span)) = var(name) {
            out.fact(g, label, v.clone(), Some(span.clone()));
        }
    }
    let c = "Contents";
    out.fact(c, "Layers", layers.len().to_string(), None);
    out.fact(c, "Blocks", blocks.len().to_string(), None);
    out.fact(c, "Entities", entities.len().to_string(), None);
    if inserts > 0 {
        out.fact(
            c,
            "Block references (not expanded)",
            inserts.to_string(),
            None,
        );
    }
    let mut vars = Rows::new("Header variables", &["Variable", "Value"]);
    for (n, v, span) in &header {
        vars.push(vec![n.clone(), clip(v, 200)], Some(span.clone()));
    }
    vars.finish(&mut out);
    layers.finish(&mut out);
    blocks.finish(&mut out);
    count_table(&mut out, "Entity types", "Type", &kinds);
    entities.finish(&mut out);
    // Checks.
    let skipped = if bad > 0 {
        format!("; {bad} pairs without a numeric group code are skipped")
    } else {
        String::new()
    };
    if !eof {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("The file ends before 0/EOF: it may be cut; what was read is shown{skipped}"),
        );
    } else if bad > 0 {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("{} records read{skipped}", records.len()),
        );
    } else {
        out.check(
            Stage::Parsed,
            State::Passed,
            format!("{} records read", records.len()),
        );
    }
    let missing_layers: Vec<&String> = if has_layer_table {
        used_layers
            .iter()
            .filter(|l| *l != "0" && !layer_names.contains(*l))
            .collect()
    } else {
        vec![]
    };
    let missing_blocks: Vec<&String> = inserted
        .iter()
        .filter(|b| !block_names.contains(*b))
        .collect();
    if missing_layers.is_empty() && missing_blocks.is_empty() {
        out.check(
            Stage::Semantic,
            State::Passed,
            if has_layer_table {
                "References: every entity's layer and every inserted block is defined"
            } else {
                "References: every inserted block is defined (no LAYER table to check layers against)"
            },
        );
    } else {
        let mut parts = vec![];
        if !missing_layers.is_empty() {
            let mut l: Vec<&str> = missing_layers.iter().map(|s| s.as_str()).collect();
            l.sort_unstable();
            parts.push(format!(
                "layers not in the LAYER table: {}",
                clip(&l.join(", "), 200)
            ));
        }
        if !missing_blocks.is_empty() {
            let mut b: Vec<&str> = missing_blocks.iter().map(|s| s.as_str()).collect();
            b.sort_unstable();
            parts.push(format!("undefined blocks: {}", clip(&b.join(", "), 200)));
        }
        out.check(
            Stage::Semantic,
            State::Failed,
            format!("References: {}", parts.join("; ")),
        );
    }
    let mut note = "The drawing shows model-space entities projected onto the XY plane (elevations and extrusion directions are ignored); text and block references are marked as points, blocks are not expanded.".to_owned();
    if splines > 0 {
        note.push_str(" Splines are drawn as their control polygon.");
    }
    if meshes > 0 {
        note.push_str(&format!(
            " {meshes} polyface or mesh polylines are not drawn."
        ));
    }
    if !drawing.is_empty() {
        out.notes.push(note);
    }
    drawing.finish(&mut out, &unit);
    out.not_validated();
    Ok(out)
}

/// The header section is one record: `9 $NAME` followed by the variable's groups.
fn read_header(r: &Record, header: &mut Vec<(String, String, std::ops::Range<u64>)>) {
    let mut name: Option<String> = None;
    let mut values: Vec<&str> = vec![];
    for &(c, v) in r.pairs.iter().skip(1).chain(std::iter::once(&(9, ""))) {
        if c == 9 {
            if let Some(n) = name.take()
                && header.len() < 2000
            {
                header.push((n, values.join(", "), r.span.clone()));
            }
            values.clear();
            name = (!v.is_empty()).then(|| v.to_owned());
        } else if values.len() < 16 {
            values.push(v);
        }
    }
}

/// Draw one entity; returns its key coordinates and text or block name for the table.
fn entity(r: &Record, drawing: &mut Drawing, polyline: &mut OpenPolyline) -> (String, String) {
    let deg = |v: f64| v.to_radians();
    match r.kind {
        "LINE" => {
            let (a, b) = (r.xy(10), r.xy(11));
            drawing.path(vec![a, b]);
            (format!("{} → {}", pt(a), pt(b)), String::new())
        }
        "POINT" => {
            drawing.point(r.xy(10));
            (pt(r.xy(10)), String::new())
        }
        "CIRCLE" => {
            let (c, rad) = (r.xy(10), r.f(40));
            drawing.path(arc(c, rad, 0.0, std::f64::consts::TAU));
            (format!("centre {} r {}", pt(c), num(rad)), String::new())
        }
        "ARC" => {
            let (c, rad) = (r.xy(10), r.f(40));
            let (a0, mut a1) = (r.f(50), r.f(51));
            if a1 <= a0 {
                a1 += 360.0;
            }
            drawing.path(arc(c, rad, deg(a0), deg(a1)));
            (
                format!(
                    "centre {} r {}, {}° to {}°",
                    pt(c),
                    num(rad),
                    num(r.f(50)),
                    num(r.f(51))
                ),
                String::new(),
            )
        }
        "ELLIPSE" => {
            let (c, major, ratio) = (r.xy(10), r.xy(11), r.f(40));
            let (s, mut e) = (
                r.f(41),
                r.get(42).map(|_| r.f(42)).unwrap_or(std::f64::consts::TAU),
            );
            if e <= s {
                e += std::f64::consts::TAU;
            }
            drawing.path(ellipse(c, major, ratio, s, e));
            (
                format!("centre {} axis {} ratio {}", pt(c), pt(major), num(ratio)),
                String::new(),
            )
        }
        "LWPOLYLINE" => {
            let v = r.points(10);
            let closed = r.i(70) & 1 != 0;
            let shown = v.first().map(|p| pt(p.0)).unwrap_or_default();
            drawing.path(bulged(&v, closed));
            (
                format!(
                    "{} vertices from {shown}{}",
                    v.len(),
                    if closed { ", closed" } else { "" }
                ),
                String::new(),
            )
        }
        "POLYLINE" => {
            let flags = r.i(70);
            *polyline = Some((flags, vec![]));
            (
                format!(
                    "flags {flags}{}",
                    if flags & 1 != 0 { ", closed" } else { "" }
                ),
                String::new(),
            )
        }
        "SPLINE" => {
            let control: Vec<[f64; 2]> = r.points(10).into_iter().map(|p| p.0).collect();
            let fit: Vec<[f64; 2]> = r.points(11).into_iter().map(|p| p.0).collect();
            let pts = if control.len() >= 2 {
                control.clone()
            } else {
                fit.clone()
            };
            drawing.path(pts);
            (
                format!(
                    "degree {}, {} control points, {} fit points",
                    r.i(71),
                    control.len(),
                    fit.len()
                ),
                String::new(),
            )
        }
        "TEXT" | "MTEXT" | "ATTDEF" => {
            drawing.point(r.xy(10));
            let mut text: String = r.pairs.iter().filter(|p| p.0 == 3).map(|p| p.1).collect();
            text.push_str(r.get(1).unwrap_or(""));
            (pt(r.xy(10)), text)
        }
        "INSERT" => {
            drawing.point(r.xy(10));
            (pt(r.xy(10)), r.get(2).unwrap_or("").to_owned())
        }
        _ => (
            r.get(10).map(|_| pt(r.xy(10))).unwrap_or_default(),
            String::new(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bulge_is_a_quarter_arc() {
        let b = (std::f64::consts::PI / 8.0).tan();
        let pts = bulged(&[([1.0, 0.0], b), ([0.0, 1.0], 0.0)], false);
        let mid = pts[pts.len() / 2];
        assert!(((mid[0] * mid[0] + mid[1] * mid[1]).sqrt() - 1.0).abs() < 1e-9);
        assert!((pts.last().unwrap()[1] - 1.0).abs() < 1e-9);
    }
}
