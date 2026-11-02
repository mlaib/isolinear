//! Shared pieces of the CAD, 3D and engineering handlers: meshes with merged vertices and
//! their measurements, 2D drawings with sampled arcs, bounded tables and number formatting.
use crate::inspect::{Inspection, MAX_ROWS, Row, Stage, State, Table, Visual};
use std::{collections::HashMap, ops::Range};

/// Triangles kept for the 3D view and the closed-mesh check; measurements use all of them.
pub const MAX_TRIANGLES: usize = 4_000_000;
/// Distinct vertices kept for the 3D view.
pub const MAX_VERTICES: usize = 4_000_000;
/// Points kept in a drawing.
pub const MAX_DRAWING_POINTS: usize = 4_000_000;
/// Segments of a sampled full circle (arcs get a share by their angle, at least 2).
pub const CIRCLE_SEGMENTS: usize = 72;

/// A number for reading: up to six significant digits, no trailing zeros.
pub fn num(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let a = v.abs();
    if a != 0.0 && !(1e-4..1e9).contains(&a) {
        return format!("{v:.4e}");
    }
    let decimals = if a >= 1.0 {
        (5 - a.log10().floor() as i32).clamp(0, 6) as usize
    } else {
        6
    };
    let s = format!("{v:.decimals$}");
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    };
    if s == "-0" { "0".into() } else { s }
}

/// A table filled row by row: rows past [`MAX_ROWS`] are counted, not kept.
pub struct Rows {
    table: Table,
    count: u64,
}
impl Rows {
    pub fn new(name: &str, columns: &[&str]) -> Self {
        Self {
            table: Table {
                name: name.into(),
                columns: columns.iter().map(|c| (*c).to_owned()).collect(),
                ..Table::default()
            },
            count: 0,
        }
    }
    pub fn push(&mut self, cells: Vec<String>, source: Option<Range<u64>>) {
        self.count += 1;
        if self.table.rows.len() < MAX_ROWS {
            self.table.rows.push(Row { cells, source });
        }
    }
    pub fn len(&self) -> u64 {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    /// Add the table to `out` when it has rows.
    pub fn finish(mut self, out: &mut Inspection) {
        if self.count > self.table.rows.len() as u64 {
            self.table.total = Some(self.count);
        }
        if !self.table.rows.is_empty() {
            out.tables.push(self.table);
        }
    }
}

/// A count table ("Type", "Count") sorted by count, largest first.
pub fn count_table(out: &mut Inspection, name: &str, what: &str, counts: &HashMap<String, u64>) {
    let mut list: Vec<(&String, &u64)> = counts.iter().collect();
    list.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let mut rows = Rows::new(name, &[what, "Count"]);
    for (k, n) in list {
        rows.push(vec![k.clone(), n.to_string()], None);
    }
    rows.finish(out);
}

/// Triangles gathered for the 3D view, with identical vertices merged, and the measurements
/// of every triangle seen (also those past the kept limit).
#[derive(Default)]
pub struct Mesh {
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    map: HashMap<[u32; 3], u32>,
    /// Triangles seen, kept or not.
    pub seen: u64,
    /// Triangles with a repeated corner (after merging) or non-finite coordinates.
    pub degenerate: u64,
    /// Triangles seen but not kept (limits).
    pub dropped: u64,
    area: f64,
    volume: f64,
    min: [f64; 3],
    max: [f64; 3],
}

impl Mesh {
    pub fn new() -> Self {
        Self {
            min: [f64::MAX; 3],
            max: [f64::MIN; 3],
            ..Self::default()
        }
    }
    fn vertex(&mut self, p: [f32; 3]) -> Option<u32> {
        // -0.0 and 0.0 are the same point.
        let key = p.map(|c| if c == 0.0 { 0 } else { c.to_bits() });
        if let Some(&i) = self.map.get(&key) {
            return Some(i);
        }
        if self.vertices.len() >= MAX_VERTICES {
            return None;
        }
        let i = self.vertices.len() as u32;
        self.vertices.push(p);
        self.map.insert(key, i);
        Some(i)
    }
    /// Add one triangle by its corners.
    pub fn triangle(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3]) {
        self.seen += 1;
        if !a.iter().chain(&b).chain(&c).all(|v| v.is_finite()) {
            self.degenerate += 1;
            return;
        }
        let [p, q, r] = [a, b, c].map(|v| v.map(f64::from));
        for v in [p, q, r] {
            for (k, &c) in v.iter().enumerate() {
                self.min[k] = self.min[k].min(c);
                self.max[k] = self.max[k].max(c);
            }
        }
        let u = sub(q, p);
        let w = sub(r, p);
        let n = cross(u, w);
        self.area += 0.5 * dot(n, n).sqrt();
        self.volume += dot(p, cross(q, r)) / 6.0;
        if self.triangles.len() >= MAX_TRIANGLES {
            self.dropped += 1;
            return;
        }
        match (self.vertex(a), self.vertex(b), self.vertex(c)) {
            (Some(i), Some(j), Some(k)) if i != j && j != k && i != k => {
                self.triangles.push([i, j, k])
            }
            (Some(_), Some(_), Some(_)) => self.degenerate += 1,
            _ => self.dropped += 1,
        }
    }
    pub fn has_bounds(&self) -> bool {
        self.min[0] <= self.max[0]
    }

    /// Facts ("Geometry"), the closed-mesh check and the 3D view. `unit` names the length unit
    /// when the format has one.
    pub fn finish(self, out: &mut Inspection, unit: Option<&str>) {
        let g = "Geometry";
        out.fact(g, "Triangles", self.seen.to_string(), None);
        out.fact(
            g,
            "Vertices (merged)",
            self.vertices.len().to_string(),
            None,
        );
        if self.degenerate > 0 {
            out.fact(g, "Degenerate triangles", self.degenerate.to_string(), None);
        }
        let u = unit.map(|u| format!(" {u}")).unwrap_or_default();
        if self.has_bounds() {
            let size: Vec<String> = (0..3).map(|k| num(self.max[k] - self.min[k])).collect();
            out.fact(
                g,
                "Size (x × y × z)",
                format!("{}{u}", size.join(" × ")),
                None,
            );
            out.fact(
                g,
                "Bounding box",
                format!(
                    "({}, {}, {}) to ({}, {}, {})",
                    num(self.min[0]),
                    num(self.min[1]),
                    num(self.min[2]),
                    num(self.max[0]),
                    num(self.max[1]),
                    num(self.max[2])
                ),
                None,
            );
            let u2 = unit.map(|u| format!(" {u}²")).unwrap_or_default();
            let u3 = unit.map(|u| format!(" {u}³")).unwrap_or_default();
            out.fact(g, "Surface area", format!("{}{u2}", num(self.area)), None);
            out.fact(
                g,
                "Volume (signed tetrahedra)",
                format!("{}{u3}", num(self.volume.abs())),
                None,
            );
        }
        // Closed: every edge is shared by exactly two triangles.
        if self.dropped > 0 {
            out.check(
                Stage::Semantic,
                State::NotDone,
                format!(
                    "Closed mesh: not checked, {} triangles exceed the {MAX_TRIANGLES} kept",
                    self.dropped
                ),
            );
        } else if !self.triangles.is_empty() {
            let mut edges: HashMap<(u32, u32), (u32, i32)> = HashMap::new();
            for t in &self.triangles {
                for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                    let e = edges.entry((a.min(b), a.max(b))).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += if a < b { 1 } else { -1 };
                }
            }
            let boundary = edges.values().filter(|e| e.0 == 1).count();
            let shared_more = edges.values().filter(|e| e.0 > 2).count();
            let flipped = edges.values().filter(|e| e.0 == 2 && e.1 != 0).count();
            if boundary == 0 && shared_more == 0 {
                let orient = if flipped == 0 {
                    "consistently oriented"
                } else {
                    "but some neighbouring triangles face opposite ways"
                };
                out.check(
                    Stage::Semantic,
                    State::Passed,
                    format!(
                        "Closed mesh: each of the {} edges is shared by exactly two triangles, {orient}",
                        edges.len()
                    ),
                );
            } else {
                out.check(
                    Stage::Semantic,
                    State::Failed,
                    format!(
                        "Closed mesh: {boundary} edges belong to one triangle only (holes), {shared_more} to more than two"
                    ),
                );
                out.notes
                    .push("The mesh is not closed, so its volume is not meaningful.".into());
            }
        }
        if self.dropped > 0 {
            out.notes.push(format!(
                "The 3D view shows the first {} triangles; measurements use all {}.",
                self.triangles.len(),
                self.seen
            ));
        }
        if !self.triangles.is_empty() {
            out.visuals.push(Visual::Mesh {
                vertices: self.vertices,
                triangles: self.triangles,
            });
        }
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// A 2D line drawing in progress: polylines and marked points.
#[derive(Default)]
pub struct Drawing {
    pub paths: Vec<Vec<[f32; 2]>>,
    /// Single points (text positions, point entities), drawn as small crosses.
    pub points: Vec<[f32; 2]>,
    total: usize,
    pub truncated: bool,
}

impl Drawing {
    /// Add a polyline (at least two finite points; others are skipped).
    pub fn path(&mut self, pts: Vec<[f64; 2]>) {
        let pts: Vec<[f32; 2]> = pts
            .into_iter()
            .map(|p| [p[0] as f32, p[1] as f32])
            .filter(|p| p[0].is_finite() && p[1].is_finite())
            .collect();
        if pts.len() < 2 {
            return;
        }
        if self.total + pts.len() > MAX_DRAWING_POINTS {
            self.truncated = true;
            return;
        }
        self.total += pts.len();
        self.paths.push(pts);
    }
    pub fn point(&mut self, p: [f64; 2]) {
        let p = [p[0] as f32, p[1] as f32];
        if !p[0].is_finite() || !p[1].is_finite() {
            return;
        }
        if self.total + 5 > MAX_DRAWING_POINTS {
            self.truncated = true;
            return;
        }
        self.total += 5;
        self.points.push(p);
    }
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.points.is_empty()
    }
    /// The drawing as a visual (points become crosses sized to the drawing), if not empty.
    pub fn finish(mut self, out: &mut Inspection, unit: &str) {
        if self.is_empty() {
            return;
        }
        let (mut min, mut max) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in self.paths.iter().flatten().chain(&self.points) {
            for k in 0..2 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
        let extent = (max[0] - min[0]).max(max[1] - min[1]);
        let r = if extent > 0.0 { extent * 0.006 } else { 0.5 };
        for p in std::mem::take(&mut self.points) {
            let [x, y] = p;
            self.paths
                .push(vec![[x - r, y], [x + r, y], [x, y], [x, y + r], [x, y - r]]);
        }
        if self.truncated {
            out.notes.push(format!(
                "The drawing keeps the first {MAX_DRAWING_POINTS} points; later geometry is not drawn."
            ));
        }
        out.visuals.push(Visual::Drawing {
            paths: self.paths,
            unit: unit.into(),
        });
    }
}

/// Points along a circular arc from `start` to `end` (radians, counter-clockwise when
/// `end > start`), endpoints included.
pub fn arc(center: [f64; 2], radius: f64, start: f64, end: f64) -> Vec<[f64; 2]> {
    let sweep = end - start;
    if !sweep.is_finite() || !radius.is_finite() || !center[0].is_finite() || !center[1].is_finite()
    {
        return vec![];
    }
    let n = ((sweep.abs() / std::f64::consts::TAU * CIRCLE_SEGMENTS as f64).ceil() as usize)
        .clamp(2, CIRCLE_SEGMENTS);
    (0..=n)
        .map(|i| {
            let a = start + sweep * i as f64 / n as f64;
            [center[0] + radius * a.cos(), center[1] + radius * a.sin()]
        })
        .collect()
}

/// Points along an ellipse (`major` is the major axis end relative to the centre, `ratio` the
/// minor/major ratio) between two parameters in radians.
pub fn ellipse(
    center: [f64; 2],
    major: [f64; 2],
    ratio: f64,
    start: f64,
    end: f64,
) -> Vec<[f64; 2]> {
    let minor = [-major[1] * ratio, major[0] * ratio];
    let sweep = end - start;
    if !sweep.is_finite() || !ratio.is_finite() {
        return vec![];
    }
    let n = ((sweep.abs() / std::f64::consts::TAU * CIRCLE_SEGMENTS as f64).ceil() as usize)
        .clamp(2, CIRCLE_SEGMENTS);
    (0..=n)
        .map(|i| {
            let t = start + sweep * i as f64 / n as f64;
            let (s, c) = t.sin_cos();
            [
                center[0] + major[0] * c + minor[0] * s,
                center[1] + major[1] * c + minor[1] * s,
            ]
        })
        .collect()
}

/// Lines of a text with their byte offsets (`\n`, `\r\n` or `\r` endings).
pub fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut at = 0;
    let b = text.as_bytes();
    std::iter::from_fn(move || {
        if at >= b.len() {
            return None;
        }
        let start = at;
        let end = memchr::memchr2(b'\n', b'\r', &b[at..])
            .map(|i| at + i)
            .unwrap_or(b.len());
        at = end;
        match b.get(at) {
            Some(b'\r') => {
                at += 1;
                if b.get(at) == Some(&b'\n') {
                    at += 1;
                }
            }
            Some(_) => at += 1,
            None => {}
        }
        Some((start, &text[start..end]))
    })
}

/// Truncate text for a table cell.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numbers_lines_and_arcs() {
        assert_eq!(num(10.0), "10");
        assert_eq!(num(0.1 + 0.2), "0.3");
        assert_eq!(num(-1234.5678), "-1234.57");
        assert_eq!(num(-0.0000001), "-1.0000e-7");
        let l: Vec<(usize, &str)> = lines("a\r\nb\rc\n\nd").collect();
        assert_eq!(l, [(0, "a"), (3, "b"), (5, "c"), (7, ""), (8, "d")]);
        let a = arc([0.0, 0.0], 1.0, 0.0, std::f64::consts::FRAC_PI_2);
        assert!((a.last().unwrap()[1] - 1.0).abs() < 1e-9);
        assert_eq!(a.len(), 19);
    }
    #[test]
    fn closed_tetrahedron() {
        let p = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let mut m = Mesh::new();
        for [a, b, c] in [[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]] {
            m.triangle(p[a], p[b], p[c]);
        }
        let mut i = Inspection::new("x", "y");
        m.finish(&mut i, None);
        assert_eq!(i.checks.last().unwrap().state, State::Passed);
        let vol = i
            .facts
            .iter()
            .find(|f| f.label.starts_with("Volume"))
            .unwrap();
        assert_eq!(vol.value, "0.166667");
    }
}
