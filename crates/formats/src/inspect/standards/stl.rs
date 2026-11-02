//! STL triangle meshes (stereolithography), ASCII (`solid` … `facet normal`) and binary
//! (80-byte header, triangle count, 50-byte records). A binary file may also begin with
//! "solid", so a binary file is recognized by its size: 84 + 50 × count bytes.
use super::cad::{Mesh, clip};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};

pub struct Stl;

/// The declared triangle count when the file's size is exactly that of a binary STL.
fn binary_count(head: &[u8], size: u64) -> Option<u32> {
    let n = u32::from_le_bytes(head.get(80..84)?.try_into().ok()?);
    (84 + 50 * u64::from(n) == size).then_some(n)
}

fn looks_ascii(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(&head[..head.len().min(4096)]);
    let t = text.trim_start();
    t.as_bytes()
        .get(..5)
        .is_some_and(|w| w.eq_ignore_ascii_case(b"solid"))
        && (t.contains("facet") || t.len() < 200)
}

impl Handler for Stl {
    fn name(&self) -> &'static str {
        "STL"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if probe.ext_is(&["stl"]) {
            // Binary STL has no signature: a file named .stl is read unless it is tiny.
            return looks_ascii(probe.head) || probe.size >= 84;
        }
        // Without the extension, only an unmistakable ASCII STL.
        let text = String::from_utf8_lossy(&probe.head[..probe.head.len().min(1024)]);
        let t = text.trim_start();
        t.starts_with("solid") && t.contains("facet normal") && t.contains("outer loop")
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        if binary_count(b, input.size).is_some() || !looks_ascii(b) {
            binary(input, cancelled)
        } else {
            ascii(input, cancelled)
        }
    }
}

fn f32_at(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn binary(input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let b = input.bytes;
    if b.len() < 84 {
        return Err("Too short for a binary STL file (less than 84 bytes)".into());
    }
    let mut out = Inspection::new("STL", "Triangle mesh (binary STL)");
    out.version = Some("Binary".into());
    let header: String = b[..80]
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| {
            if (0x20..0x7f).contains(&c) {
                c as char
            } else {
                ' '
            }
        })
        .collect();
    out.fact("File", "Header", clip(header.trim(), 80), Some(0..80));
    let declared = u32::from_le_bytes(b[80..84].try_into().unwrap_or_default());
    out.fact(
        "File",
        "Triangles (declared)",
        declared.to_string(),
        Some(80..84),
    );
    let available = (b.len() - 84) / 50;
    let n = (declared as usize).min(available);
    let mut mesh = Mesh::new();
    let mut coloured = 0u64;
    for i in 0..n {
        if i % 65536 == 0 && cancelled() {
            return Err("Cancelled".into());
        }
        let at = 84 + i * 50;
        let v = |k: usize| {
            let o = at + 12 + k * 12;
            [f32_at(b, o), f32_at(b, o + 4), f32_at(b, o + 8)]
        };
        mesh.triangle(v(0), v(1), v(2));
        if u16::from_le_bytes([b[at + 48], b[at + 49]]) != 0 {
            coloured += 1;
        }
    }
    if coloured > 0 {
        out.fact(
            "File",
            "Triangles with attribute bytes",
            coloured.to_string(),
            None,
        );
        out.notes.push(
            "Some triangles carry attribute bytes (often a colour in VisCAM/SolidView or Materialise conventions); they are not interpreted.".into(),
        );
    }
    let expected = 84 + 50 * u64::from(declared);
    if (declared as usize) > available {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!(
                "The header declares {declared} triangles but the file holds {available}: the file is cut; the triangles present are shown"
            ),
        );
    } else if input.size != expected {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!(
                "The file has {} bytes after the {declared} declared triangles; they are ignored",
                input.size.saturating_sub(expected)
            ),
        );
    } else {
        out.check(
            Stage::Parsed,
            State::Passed,
            format!("{declared} triangles; the size matches the declared count"),
        );
    }
    finish(&mut out, mesh);
    Ok(out)
}

fn ascii(input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let text = input.text()?;
    let mut out = Inspection::new("STL", "Triangle mesh (ASCII STL)");
    out.version = Some("ASCII".into());
    let mut mesh = Mesh::new();
    let mut names = vec![];
    let (mut solids, mut facets, mut bad, mut ended) = (0u64, 0u64, 0u64, false);
    let mut loop_pts: Vec<[f32; 3]> = vec![];
    let mut words = text.split_ascii_whitespace();
    let mut count = 0u64;
    while let Some(w) = words.next() {
        count += 1;
        if count.is_multiple_of(1 << 20) && cancelled() {
            return Err("Cancelled".into());
        }
        match w {
            "solid" => {
                solids += 1;
                ended = false;
            }
            "facet" => facets += 1,
            "outer" => loop_pts.clear(),
            "vertex" => {
                let mut p = [0f32; 3];
                let mut ok = true;
                for c in &mut p {
                    match words.next().and_then(|s| s.parse::<f32>().ok()) {
                        Some(v) => *c = v,
                        None => ok = false,
                    }
                }
                if ok && loop_pts.len() < 1024 {
                    loop_pts.push(p);
                } else if !ok {
                    bad += 1;
                }
            }
            "endloop" => {
                if loop_pts.len() < 3 {
                    bad += 1;
                }
                // Some exporters write polygons: fan-triangulate them.
                for k in 1..loop_pts.len().saturating_sub(1) {
                    mesh.triangle(loop_pts[0], loop_pts[k], loop_pts[k + 1]);
                }
                loop_pts.clear();
            }
            "endsolid" => ended = true,
            _ => {}
        }
    }
    // Solid names: the text after "solid" on its line.
    for line in text.lines().filter(|l| l.trim_start().starts_with("solid")) {
        if names.len() >= 32 {
            break;
        }
        let name = line.trim_start()[5..].trim();
        if !name.is_empty() {
            names.push(clip(name, 80));
        }
    }
    out.fact("File", "Solids", solids.to_string(), None);
    out.fact("File", "Solid names", names.join(", "), None);
    out.fact("File", "Facets", facets.to_string(), None);
    let skipped = if bad > 0 {
        format!("; {bad} facets with unreadable vertices or fewer than three are skipped")
    } else {
        String::new()
    };
    if !ended {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("The file ends before \"endsolid\": it may be cut; the facets read are shown{skipped}"),
        );
    } else if bad > 0 {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("{facets} facets read{skipped}"),
        );
    } else {
        out.check(
            Stage::Parsed,
            State::Passed,
            format!("{facets} facets read"),
        );
    }
    if mesh.seen == 0 {
        return Err("No facets with three vertices were found".into());
    }
    finish(&mut out, mesh);
    Ok(out)
}

fn finish(out: &mut Inspection, mesh: Mesh) {
    out.notes.push(
        "STL has no unit: sizes are in the file's own units (often millimetres). The volume is meaningful only for a closed mesh.".into(),
    );
    mesh.finish(out, None);
    out.not_validated();
}
