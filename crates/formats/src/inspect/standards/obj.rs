//! Wavefront OBJ meshes: `v`/`vt`/`vn` and `f` (polygons fan-triangulated; `v`, `v/vt`,
//! `v//vn`, `v/vt/vn` and negative indices), with `o`, `g`, `usemtl` and `mtllib`. Material
//! libraries are other files: their names are listed, never opened.
use super::cad::{Mesh, Rows, clip, lines};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State, span_of};
use std::collections::HashMap;

pub struct Obj;

/// Vertex positions kept (12 bytes each).
const MAX_POSITIONS: usize = 20_000_000;
/// Corners of one face.
const MAX_CORNERS: usize = 4096;

impl Handler for Obj {
    fn name(&self) -> &'static str {
        "Wavefront OBJ"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if !probe.ext_is(&["obj"]) {
            return false;
        }
        let Some(text) = probe.text() else {
            return false;
        };
        text.lines().take(2000).any(|l| {
            let l = l.trim_start();
            l.starts_with("v ") || l.starts_with("f ") || l.starts_with("mtllib ")
        })
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let text = input.text()?;
        inspect(&text, cancelled)
    }
}

/// Counts for one object, group or material.
#[derive(Default)]
struct Part {
    faces: u64,
    materials: Vec<String>,
    first: Option<std::ops::Range<u64>>,
}

fn part<'a>(
    map: &'a mut HashMap<String, Part>,
    order: &mut Vec<String>,
    name: &str,
) -> &'a mut Part {
    if !map.contains_key(name) && order.len() < 100_000 {
        order.push(name.to_owned());
    }
    map.entry(name.to_owned()).or_default()
}

fn inspect(text: &str, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let mut out = Inspection::new("Wavefront OBJ", "Polygon mesh");
    let mut positions: Vec<[f32; 3]> = vec![];
    let (mut texcoords, mut normals, mut faces, mut lines_n, mut points) = (0u64, 0, 0u64, 0, 0);
    let (mut bad_index, mut bad_line, mut polygons) = (0u64, 0u64, 0u64);
    let mut mesh = Mesh::new();
    let mut libs: Vec<String> = vec![];
    let (mut objects, mut groups, mut materials) = (HashMap::new(), HashMap::new(), HashMap::new());
    let (mut o_order, mut g_order, mut m_order) = (vec![], vec![], vec![]);
    let (mut object, mut group, mut material) = (
        "(none)".to_owned(),
        "(none)".to_owned(),
        "(none)".to_owned(),
    );
    let mut pending = String::new();
    let mut corners: Vec<[f32; 3]> = vec![];
    for (n, (_, raw)) in lines(text).enumerate() {
        if n % 65536 == 0 && cancelled() {
            return Err("Cancelled".into());
        }
        // A trailing backslash continues the line.
        let line: &str = if let Some(head) = raw.strip_suffix('\\') {
            if pending.len() < 1 << 20 {
                pending.push_str(head);
                pending.push(' ');
            }
            continue;
        } else if !pending.is_empty() {
            pending.push_str(raw);
            pending.as_str()
        } else {
            raw
        };
        let line = line.split('#').next().unwrap_or("").trim();
        let mut words = line.split_ascii_whitespace();
        let Some(key) = words.next() else {
            pending.clear();
            continue;
        };
        let span = span_of(text, raw);
        match key {
            "v" => {
                let mut p = [0f32; 3];
                let mut ok = true;
                for c in &mut p {
                    match words.next().and_then(|w| w.parse().ok()) {
                        Some(v) => *c = v,
                        None => ok = false,
                    }
                }
                if !ok {
                    bad_line += 1;
                }
                if positions.len() < MAX_POSITIONS {
                    positions.push(p);
                }
            }
            "vt" => texcoords += 1,
            "vn" => normals += 1,
            "f" => {
                faces += 1;
                corners.clear();
                let count = positions.len() as i64;
                for w in words.by_ref().take(MAX_CORNERS) {
                    let first = w.split('/').next().unwrap_or("");
                    let index = first.parse::<i64>().ok().and_then(|i| match i {
                        i if i > 0 && i <= count => Some(i - 1),
                        i if i < 0 && -i <= count => Some(count + i),
                        _ => None,
                    });
                    match index {
                        Some(i) => corners.push(positions[i as usize]),
                        None => bad_index += 1,
                    }
                }
                if corners.len() > 3 {
                    polygons += 1;
                }
                for k in 1..corners.len().saturating_sub(1) {
                    mesh.triangle(corners[0], corners[k], corners[k + 1]);
                }
                for (map, order, name) in [
                    (&mut objects, &mut o_order, &object),
                    (&mut groups, &mut g_order, &group),
                ] {
                    let p = part(map, order, name);
                    p.faces += 1;
                    if !p.materials.contains(&material) && p.materials.len() < 32 {
                        p.materials.push(material.clone());
                    }
                }
                part(&mut materials, &mut m_order, &material).faces += 1;
            }
            "l" => lines_n += 1,
            "p" => points += 1,
            "o" => {
                object = clip(&words.collect::<Vec<_>>().join(" "), 120);
                part(&mut objects, &mut o_order, &object).first = span;
            }
            "g" => {
                group = clip(&words.collect::<Vec<_>>().join(" "), 120);
                if group.is_empty() {
                    group = "default".into();
                }
                part(&mut groups, &mut g_order, &group).first = span;
            }
            "usemtl" => {
                material = clip(&words.collect::<Vec<_>>().join(" "), 120);
                part(&mut materials, &mut m_order, &material).first = span;
            }
            "mtllib" => {
                for w in words {
                    if libs.len() < 64 && !libs.iter().any(|l| l == w) {
                        libs.push(w.to_owned());
                    }
                }
            }
            "s" | "vp" | "cstype" | "deg" | "bmat" | "step" | "curv" | "curv2" | "surf"
            | "parm" | "trim" | "hole" | "scrv" | "sp" | "end" | "con" | "mg" | "lod" | "bevel"
            | "c_interp" | "d_interp" | "usemap" | "maplib" | "shadow_obj" | "trace_obj"
            | "ctech" | "stech" => {}
            _ => bad_line += 1,
        }
        pending.clear();
    }
    let g = "Contents";
    out.fact(g, "Vertices", positions.len().to_string(), None);
    out.fact(g, "Texture coordinates", texcoords.to_string(), None);
    out.fact(g, "Normals", normals.to_string(), None);
    out.fact(g, "Faces", faces.to_string(), None);
    if polygons > 0 {
        out.fact(
            g,
            "Faces with more than three corners",
            polygons.to_string(),
            None,
        );
    }
    if lines_n > 0 {
        out.fact(g, "Line elements (not drawn)", lines_n.to_string(), None);
    }
    if points > 0 {
        out.fact(g, "Point elements (not drawn)", points.to_string(), None);
    }
    out.fact(g, "Material libraries (not opened)", libs.join(", "), None);
    let named = |order: &Vec<String>| order.iter().filter(|n| *n != "(none)").count();
    out.fact(g, "Objects", named(&o_order).to_string(), None);
    out.fact(g, "Groups", named(&g_order).to_string(), None);
    out.fact(g, "Materials used", named(&m_order).to_string(), None);
    for (title, what, map, order) in [
        ("Objects", "Object", &objects, &o_order),
        ("Groups", "Group", &groups, &g_order),
    ] {
        if named(order) == 0 {
            continue;
        }
        let mut rows = Rows::new(title, &[what, "Faces", "Materials"]);
        for name in order {
            let p = &map[name];
            if name == "(none)" && p.faces == 0 {
                continue;
            }
            rows.push(
                vec![name.clone(), p.faces.to_string(), p.materials.join(", ")],
                p.first.clone(),
            );
        }
        rows.finish(&mut out);
    }
    if named(&m_order) > 0 {
        let mut rows = Rows::new("Materials", &["Material", "Faces"]);
        for name in &m_order {
            let p = &materials[name];
            if name == "(none)" && p.faces == 0 {
                continue;
            }
            rows.push(vec![name.clone(), p.faces.to_string()], p.first.clone());
        }
        rows.finish(&mut out);
    }
    if positions.len() >= MAX_POSITIONS {
        out.notes.push(format!(
            "Only the first {MAX_POSITIONS} vertices are kept; faces using later ones are skipped."
        ));
    }
    if bad_line > 0 {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("{bad_line} lines are not understood; they are skipped"),
        );
    } else {
        out.check(Stage::Parsed, State::Passed, "Every line was understood");
    }
    out.check(
        Stage::Semantic,
        if bad_index == 0 {
            State::Passed
        } else {
            State::Failed
        },
        if bad_index == 0 {
            "Face indices: every corner refers to a vertex defined before it".to_owned()
        } else {
            format!("Face indices: {bad_index} corners refer to no defined vertex")
        },
    );
    if faces == 0 && positions.is_empty() {
        return Err("No vertices or faces were found".into());
    }
    out.notes.push(
        "OBJ has no unit: sizes are in the file's own units. Polygons are split into triangles from their first corner. The volume is meaningful only for a closed mesh.".into(),
    );
    mesh.finish(&mut out, None);
    out.not_validated();
    Ok(out)
}
