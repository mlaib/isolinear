//! IGES (Initial Graphics Exchange Specification) files: 80-column records with the section
//! letter in column 73 (Start, Global, Directory, Parameter, Terminate). Shows the global
//! section, the directory entries with entity names, entity counts, and lines (110), points
//! (116), arcs (100, sampled) and copious-data polylines (106) drawn in XY. Transformation
//! matrices (124) referenced by an entity are applied. The Terminate counts are checked.
use super::cad::{Drawing, Rows, arc, clip, count_table, lines, num};
use crate::inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Stage, State};
use std::collections::HashMap;

pub struct Iges;

/// Whether a record is an IGES line: section letter in column 73, sequence number after it.
fn is_record(line: &str, letter: u8) -> bool {
    let b = line.as_bytes();
    b.len() >= 80 && b[72] == letter && b[73..80].iter().all(|c| c.is_ascii_digit() || *c == b' ')
}

impl Handler for Iges {
    fn name(&self) -> &'static str {
        "IGES"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let Some(text) = probe.text() else {
            return false;
        };
        let first = text.lines().next().unwrap_or("");
        let start = is_record(first, b'S') || is_record(first, b'C') || is_record(first, b'B');
        start && (probe.ext_is(&["igs", "iges"]) || first.as_bytes()[73..80] == *b"0000001")
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let text = input.text()?;
        inspect(&text, cancelled)
    }
}

/// Entity type names.
fn entity_name(t: i64) -> &'static str {
    match t {
        0 => "Null",
        100 => "Circular arc",
        102 => "Composite curve",
        104 => "Conic arc",
        106 => "Copious data",
        108 => "Plane",
        110 => "Line",
        112 => "Parametric spline curve",
        114 => "Parametric spline surface",
        116 => "Point",
        118 => "Ruled surface",
        120 => "Surface of revolution",
        122 => "Tabulated cylinder",
        123 => "Direction",
        124 => "Transformation matrix",
        125 => "Flash",
        126 => "Rational B-spline curve",
        128 => "Rational B-spline surface",
        130 => "Offset curve",
        140 => "Offset surface",
        141 => "Boundary",
        142 => "Curve on a parametric surface",
        143 => "Bounded surface",
        144 => "Trimmed surface",
        150 => "Block",
        152 => "Right angular wedge",
        154 => "Right circular cylinder",
        156 => "Right circular cone frustum",
        158 => "Sphere",
        160 => "Torus",
        162 => "Solid of revolution",
        164 => "Solid of linear extrusion",
        168 => "Ellipsoid",
        180 => "Boolean tree",
        182 => "Selected component",
        184 => "Solid assembly",
        186 => "Manifold solid B-rep object",
        190 => "Plane surface",
        192 => "Right circular cylindrical surface",
        194 => "Right circular conical surface",
        196 => "Spherical surface",
        198 => "Toroidal surface",
        202 => "Angular dimension",
        206 => "Diameter dimension",
        210 => "General label",
        212 => "General note",
        214 => "Leader (arrow)",
        216 => "Linear dimension",
        218 => "Ordinate dimension",
        220 => "Point dimension",
        222 => "Radius dimension",
        228 => "General symbol",
        230 => "Sectioned area",
        302 => "Associativity definition",
        304 => "Line font definition",
        306 => "Macro definition",
        308 => "Subfigure definition",
        310 => "Text font definition",
        312 => "Text display template",
        314 => "Colour definition",
        316 => "Units data",
        320 => "Network subfigure definition",
        322 => "Attribute table definition",
        402 => "Associativity instance",
        404 => "Drawing",
        406 => "Property",
        408 => "Singular subfigure instance",
        410 => "View",
        412 => "Rectangular array subfigure instance",
        414 => "Circular array subfigure instance",
        416 => "External reference (not opened)",
        418 => "Nodal load/constraint",
        420 => "Network subfigure instance",
        430 => "Solid instance",
        502 => "Vertex",
        504 => "Edge",
        508 => "Loop",
        510 => "Face",
        514 => "Shell",
        _ => "",
    }
}

fn units_name(flag: i64) -> Option<&'static str> {
    Some(match flag {
        1 => "inch",
        2 => "millimetre",
        4 => "foot",
        5 => "mile",
        6 => "metre",
        7 => "kilometre",
        8 => "mil",
        9 => "micrometre",
        10 => "centimetre",
        11 => "microinch",
        _ => return None,
    })
}

fn version_name(flag: i64) -> Option<&'static str> {
    Some(match flag {
        1 => "1.0",
        2 => "ANSI Y14.26M-1981",
        3 => "2.0",
        4 => "3.0",
        5 => "ASME/ANSI Y14.26M-1987 (4.0)",
        6 => "4.0",
        7 => "ASME Y14.26M-1989 (5.0)",
        8 => "5.0",
        9 => "5.1",
        10 => "5.2",
        11 => "5.3",
        _ => return None,
    })
}

/// Parameters split by the delimiters, Hollerith strings (`5HHello`) decoded.
fn split(s: &str, pd: char, rd: char) -> Vec<String> {
    let mut out = vec![];
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() && out.len() < 1_000_000 {
        while i < chars.len() && chars[i] == ' ' {
            i += 1;
        }
        let start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        let field = if i > start && chars.get(i) == Some(&'H') {
            let n: usize = chars[start..i]
                .iter()
                .collect::<String>()
                .parse()
                .unwrap_or(0);
            let from = i + 1;
            let to = from.saturating_add(n).min(chars.len());
            i = to;
            let f: String = chars[from..to].iter().collect();
            while i < chars.len() && chars[i] != pd && chars[i] != rd {
                i += 1;
            }
            f
        } else {
            while i < chars.len() && chars[i] != pd && chars[i] != rd {
                i += 1;
            }
            chars[start..i].iter().collect::<String>().trim().to_owned()
        };
        out.push(field);
        if i >= chars.len() || chars[i] == rd {
            break;
        }
        i += 1;
    }
    out
}

/// The Global section's fields, with its own delimiters (fields 1 and 2).
fn global(s: &str) -> (Vec<String>, char, char) {
    let mut chars = s.trim_start().chars().peekable();
    let mut pd = ',';
    let mut rd = ';';
    let mut rest: String = s.trim_start().to_owned();
    if rest.starts_with("1H") {
        pd = chars.nth(2).unwrap_or(',');
        rest = rest.chars().skip(3).collect();
    }
    // `rest` starts with the delimiter after field 1.
    if rest.starts_with(pd) {
        rest = rest.chars().skip(1).collect();
    }
    let r = rest.trim_start();
    if r.starts_with("1H") {
        rd = r.chars().nth(2).unwrap_or(';');
        rest = r.chars().skip(3).collect();
    } else {
        rest = r.to_owned();
    }
    if rest.starts_with(pd) {
        rest = rest.chars().skip(1).collect();
    } else if rest.starts_with(rd) {
        return (vec![pd.to_string(), rd.to_string()], pd, rd);
    }
    let mut fields = vec![pd.to_string(), rd.to_string()];
    fields.extend(split(&rest, pd, rd));
    (fields, pd, rd)
}

fn int(s: &str) -> Option<i64> {
    s.trim().parse().ok()
}
fn real(s: &str) -> Option<f64> {
    let t = s.trim().replace(['D', 'd'], "E");
    t.parse().ok().filter(|v: &f64| v.is_finite())
}

fn date(s: &str) -> String {
    let d: Vec<char> = s.chars().collect();
    match d.len() {
        15 if d[8] == '.' => format!(
            "{}-{}-{} {}:{}:{}",
            s.get(0..4).unwrap_or(""),
            s.get(4..6).unwrap_or(""),
            s.get(6..8).unwrap_or(""),
            s.get(9..11).unwrap_or(""),
            s.get(11..13).unwrap_or(""),
            s.get(13..15).unwrap_or("")
        ),
        13 if d[6] == '.' => format!(
            "19{}-{}-{} {}:{}:{}",
            s.get(0..2).unwrap_or(""),
            s.get(2..4).unwrap_or(""),
            s.get(4..6).unwrap_or(""),
            s.get(7..9).unwrap_or(""),
            s.get(9..11).unwrap_or(""),
            s.get(11..13).unwrap_or("")
        ),
        _ => s.to_owned(),
    }
}

struct Entry {
    seq: usize,
    kind: i64,
    pointer: i64,
    level: i64,
    transform: i64,
    status: String,
    color: i64,
    lines: i64,
    form: i64,
    label: String,
    span: std::ops::Range<u64>,
}

/// 3×4 transformation: rotation rows and translation.
type Matrix = [[f64; 4]; 3];

fn inspect(text: &str, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let mut out = Inspection::new("IGES", "CAD exchange file");
    let first = text.lines().next().unwrap_or("");
    if first.as_bytes().get(72) == Some(&b'C') {
        out.check(
            Stage::Parsed,
            State::NotDone,
            "Compressed ASCII IGES is not supported; only the fixed 80-column form is read",
        );
        return Ok(out);
    }
    if first.as_bytes().get(72) == Some(&b'B') {
        out.check(
            Stage::Parsed,
            State::NotDone,
            "Binary IGES is not supported; only the fixed 80-column form is read",
        );
        return Ok(out);
    }
    let (mut start, mut glob) = (String::new(), String::new());
    let mut dir: Vec<(usize, &str)> = vec![];
    let mut par: Vec<(usize, &str)> = vec![];
    let mut term: Option<(usize, &str)> = None;
    let mut counts = [0u64; 4];
    let mut bad = 0u64;
    let mut start_span = None;
    for (n, (at, line)) in lines(text).enumerate() {
        if n % 65536 == 0 && cancelled() {
            return Err("Cancelled".into());
        }
        if line.is_empty() {
            continue;
        }
        let b = line.as_bytes();
        if b.len() < 73 || !line.is_char_boundary(72) || !line.is_char_boundary(73) {
            bad += 1;
            continue;
        }
        let data = &line[..72];
        match b[72] {
            b'S' => {
                counts[0] += 1;
                start_span.get_or_insert(at as u64..(at + line.len()) as u64);
                if start.len() < 8192 {
                    start.push_str(data.trim_end());
                    start.push('\n');
                }
            }
            b'G' => {
                counts[1] += 1;
                if glob.len() < 1 << 20 {
                    glob.push_str(data);
                }
            }
            b'D' => {
                counts[2] += 1;
                dir.push((at, line));
            }
            b'P' => {
                counts[3] += 1;
                par.push((at, line));
            }
            b'T' => term = Some((at, line)),
            _ => bad += 1,
        }
    }
    if counts[1] == 0 && dir.is_empty() {
        return Err("No Global or Directory section was found".into());
    }
    out.fact(
        "Start",
        "Start section",
        clip(start.trim(), 600),
        start_span,
    );
    // Global section.
    let (g, _pd, _rd) = global(&glob);
    let field = |k: usize| g.get(k - 1).map(String::as_str).unwrap_or("");
    let gg = "Global";
    out.fact(gg, "Product (sender)", field(3), None);
    out.fact(gg, "File name", field(4), None);
    out.fact(gg, "Native system", field(5), None);
    out.fact(gg, "Preprocessor version", field(6), None);
    out.fact(gg, "Product (receiver)", field(12), None);
    out.fact(gg, "Model space scale", field(13), None);
    let unit = match int(field(14)).and_then(units_name) {
        Some(u) => u.to_owned(),
        None => field(15).to_lowercase(),
    };
    out.fact(gg, "Units", unit.clone(), None);
    out.fact(gg, "Generated", date(field(18)), None);
    out.fact(gg, "Minimum resolution", field(19), None);
    out.fact(gg, "Maximum coordinate", field(20), None);
    out.fact(gg, "Author", field(21), None);
    out.fact(gg, "Organization", field(22), None);
    let version = int(field(23)).and_then(version_name).map(str::to_owned);
    out.fact(
        gg,
        "IGES version",
        version.clone().unwrap_or_default(),
        None,
    );
    out.fact(gg, "Model created", date(field(25)), None);
    out.fact(
        gg,
        "Delimiters",
        format!("parameter {:?}, record {:?}", field(1), field(2)),
        None,
    );
    out.version = version;
    let pd = field(1).chars().next().unwrap_or(',');
    let rd = field(2).chars().next().unwrap_or(';');
    // Directory entries: two records each, nine 8-column fields per record.
    let f8 = |line: &str, k: usize| line.get(k * 8..k * 8 + 8).unwrap_or("").to_owned();
    let mut entries: Vec<Entry> = vec![];
    for pair in dir.chunks(2) {
        let [(at, l1), (_, l2)] = pair else {
            bad += 1;
            continue;
        };
        entries.push(Entry {
            seq: entries.len() * 2 + 1,
            kind: int(&f8(l1, 0)).unwrap_or(-1),
            pointer: int(&f8(l1, 1)).unwrap_or(0),
            level: int(&f8(l1, 4)).unwrap_or(0),
            transform: int(&f8(l1, 6)).unwrap_or(0),
            status: f8(l1, 8),
            color: int(&f8(l2, 2)).unwrap_or(0),
            lines: int(&f8(l2, 3)).unwrap_or(0),
            form: int(&f8(l2, 4)).unwrap_or(0),
            label: f8(l2, 7).trim().to_owned(),
            span: *at as u64..(*at + l1.len() + 1 + l2.len()) as u64,
        });
    }
    // Parameter data of an entry.
    let params = |e: &Entry| -> Option<(Vec<String>, std::ops::Range<u64>)> {
        let first = usize::try_from(e.pointer).ok()?.checked_sub(1)?;
        let n = usize::try_from(e.lines).ok()?.clamp(1, 100_000);
        let last = first.checked_add(n)?.min(par.len());
        if first >= last {
            return None;
        }
        let mut s = String::new();
        for (_, l) in &par[first..last] {
            s.push_str(l.get(..64).unwrap_or(""));
        }
        let span = par[first].0 as u64..(par[last - 1].0 + par[last - 1].1.len()) as u64;
        Some((split(&s, pd, rd), span))
    };
    let by_seq: HashMap<usize, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.seq, i))
        .collect();
    let matrix = |seq: i64| -> Option<Matrix> {
        let e = &entries[*by_seq.get(&usize::try_from(seq).ok()?)?];
        if e.kind != 124 {
            return None;
        }
        let (v, _) = params(e)?;
        let r: Vec<f64> = v
            .iter()
            .skip(1)
            .take(12)
            .map(|s| real(s).unwrap_or(0.0))
            .collect();
        (r.len() == 12).then(|| {
            [
                [r[0], r[1], r[2], r[3]],
                [r[4], r[5], r[6], r[7]],
                [r[8], r[9], r[10], r[11]],
            ]
        })
    };
    // Chain of transforms from an entry (its own first), at most 8 deep.
    let chain = |mut seq: i64| -> Vec<Matrix> {
        let mut list = vec![];
        while seq > 0 && list.len() < 8 {
            let Some(m) = matrix(seq) else { break };
            list.push(m);
            seq = by_seq
                .get(&(seq as usize))
                .map(|&i| entries[i].transform)
                .unwrap_or(0);
        }
        list
    };
    let apply = |ms: &[Matrix], p: [f64; 3]| -> [f64; 2] {
        let mut p = p;
        for m in ms {
            p = [0, 1, 2].map(|k| m[k][0] * p[0] + m[k][1] * p[1] + m[k][2] * p[2] + m[k][3]);
        }
        [p[0], p[1]]
    };
    let mut table = Rows::new(
        "Directory entries",
        &[
            "DE",
            "Type",
            "Entity",
            "Form",
            "Label",
            "Level",
            "Colour",
            "Transform",
            "Parameters",
        ],
    );
    let mut kinds: HashMap<String, u64> = HashMap::new();
    let mut drawing = Drawing::default();
    let (mut drawn, mut mismatched, mut transformed) = (0u64, 0u64, 0u64);
    for (n, e) in entries.iter().enumerate() {
        if n % 16384 == 0 && cancelled() {
            return Err("Cancelled".into());
        }
        let name = entity_name(e.kind);
        *kinds
            .entry(format!("{} {}", e.kind, name).trim().to_owned())
            .or_default() += 1;
        let p = params(e);
        if let Some((v, _)) = &p {
            if int(v.first().map(String::as_str).unwrap_or("")) != Some(e.kind) {
                mismatched += 1;
            }
        } else if e.kind != 0 {
            mismatched += 1;
        }
        if table.len() < MAX_ROWS as u64 {
            let shown = p
                .as_ref()
                .map(|(v, _)| {
                    v.iter()
                        .skip(1)
                        .take(8)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            table.push(
                vec![
                    e.seq.to_string(),
                    e.kind.to_string(),
                    name.to_owned(),
                    e.form.to_string(),
                    e.label.clone(),
                    e.level.to_string(),
                    e.color.to_string(),
                    if e.transform > 0 {
                        e.transform.to_string()
                    } else {
                        String::new()
                    },
                    clip(&shown, 160),
                ],
                Some(e.span.clone()),
            );
        } else {
            table.push(vec![], None);
        }
        // Blanked (hidden) entities are not drawn.
        if e.status.get(..2) == Some("01") {
            continue;
        }
        let Some((v, _)) = p else { continue };
        let r = |k: usize| v.get(k).and_then(|s| real(s)).unwrap_or(0.0);
        let ms = if e.transform > 0 {
            chain(e.transform)
        } else {
            vec![]
        };
        if !ms.is_empty() {
            transformed += 1;
        }
        match e.kind {
            110 => {
                drawing.path(vec![
                    apply(&ms, [r(1), r(2), r(3)]),
                    apply(&ms, [r(4), r(5), r(6)]),
                ]);
                drawn += 1;
            }
            116 => {
                drawing.point(apply(&ms, [r(1), r(2), r(3)]));
                drawn += 1;
            }
            100 => {
                let (zt, c, s, t) = (r(1), [r(2), r(3)], [r(4), r(5)], [r(6), r(7)]);
                let radius = ((s[0] - c[0]).powi(2) + (s[1] - c[1]).powi(2)).sqrt();
                let a0 = (s[1] - c[1]).atan2(s[0] - c[0]);
                let mut a1 = (t[1] - c[1]).atan2(t[0] - c[0]);
                if a1 <= a0 + 1e-12 {
                    a1 += std::f64::consts::TAU;
                }
                let pts = arc(c, radius, a0, a1)
                    .into_iter()
                    .map(|p| apply(&ms, [p[0], p[1], zt]))
                    .collect();
                drawing.path(pts);
                drawn += 1;
            }
            // Copious data as 2D (11), 3D (12) or closed planar (63) polylines.
            106 if matches!(e.form, 11 | 12 | 63) => {
                let ip = r(1) as i64;
                let count = (r(2) as usize).min(1_000_000);
                let (first, step, z) = match ip {
                    1 => (4, 2, Some(r(3))),
                    2 => (3, 3, None),
                    3 => (3, 6, None),
                    _ => continue,
                };
                let pts: Vec<[f64; 2]> = (0..count)
                    .map(|i| {
                        let k = first + i * step;
                        apply(&ms, [r(k), r(k + 1), z.unwrap_or_else(|| r(k + 2))])
                    })
                    .collect();
                drawing.path(pts);
                drawn += 1;
            }
            _ => {}
        }
    }
    out.fact("Directory", "Entities", entries.len().to_string(), None);
    out.fact("Directory", "Entities drawn", drawn.to_string(), None);
    count_table(&mut out, "Entity types", "Entity", &kinds);
    table.finish(&mut out);
    // Checks.
    if bad > 0 {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!(
                "{bad} records are not 80-column IGES records of a known section; they are skipped"
            ),
        );
    } else {
        out.check(
            Stage::Parsed,
            State::Passed,
            format!(
                "{} directory entries, {} parameter records",
                entries.len(),
                par.len()
            ),
        );
    }
    match term {
        Some((at, t)) => {
            let declared: Vec<Option<u64>> = (0..4)
                .map(|k| {
                    t.get(k * 8 + 1..k * 8 + 8)
                        .and_then(|s| s.trim().parse().ok())
                })
                .collect();
            let ok = (0..4).all(|k| declared[k] == Some(counts[k]));
            let shown = |v: &[Option<u64>]| {
                ["S", "G", "D", "P"]
                    .iter()
                    .zip(v)
                    .map(|(l, n)| format!("{l} {}", n.map(|n| n.to_string()).unwrap_or("?".into())))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let actual: Vec<Option<u64>> = counts.iter().map(|&n| Some(n)).collect();
            out.check(
                Stage::Semantic,
                if ok { State::Passed } else { State::Failed },
                if ok {
                    format!(
                        "Terminate section: record counts match ({})",
                        shown(&actual)
                    )
                } else {
                    format!(
                        "Terminate section: declares {}, the file has {}",
                        shown(&declared),
                        shown(&actual)
                    )
                },
            );
            out.fact(
                "File",
                "Terminate record",
                t.get(..32).unwrap_or(""),
                Some(at as u64..(at + t.len()) as u64),
            );
        }
        None => out.check(
            Stage::Semantic,
            State::Failed,
            "Terminate section: missing (the file may be cut)",
        ),
    }
    out.check(
        Stage::Semantic,
        if mismatched == 0 { State::Passed } else { State::Failed },
        if mismatched == 0 {
            "Directory entries: each points to parameter data of its own entity type".to_owned()
        } else {
            format!("Directory entries: {mismatched} point to missing parameter data or data of another type")
        },
    );
    if !drawing.is_empty() {
        out.notes.push(format!(
            "The drawing shows lines, points, arcs and copious-data polylines projected onto the XY plane{}; surfaces, splines and solids are listed, not drawn.",
            if transformed > 0 { ", after their transformation matrices" } else { "" }
        ));
    }
    let _ = num;
    drawing.finish(&mut out, &unit);
    out.not_validated();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hollerith_and_delimiters() {
        assert_eq!(
            split("110,1.0,2HA,,3.5D1;", ',', ';'),
            ["110", "1.0", "A,", "3.5D1"]
        );
        assert_eq!(real("3.5D1"), Some(35.0));
        let (g, pd, rd) = global("1H,,1H;,4HPart,8Hpart.igs;");
        assert_eq!((pd, rd), (',', ';'));
        assert_eq!(g[2..], ["Part", "part.igs"]);
        let (g, _, _) = global(",,4HPart;");
        assert_eq!(g[2], "Part");
        let (g, pd, rd) = global("1H//1H#/2HAB#");
        assert_eq!((pd, rd, g[2].as_str()), ('/', '#', "AB"));
    }
}
