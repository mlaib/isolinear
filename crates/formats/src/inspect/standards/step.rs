//! STEP product models (ISO 10303-21 files of AP203, AP214, AP242 and others): the header,
//! entity counts and instances, the product structure (PRODUCT, PRODUCT_DEFINITION,
//! NEXT_ASSEMBLY_USAGE_OCCURRENCE) as an assembly tree, units and geometry counts. B-rep
//! geometry is counted, not tessellated: there is no 3D view.
use super::{
    cad::{Rows, clip},
    p21::{self, P, P21},
};
use crate::inspect::{Handler, Input, Inspection, MAX_ITEMS, OutlineNode, Probe};
use std::collections::HashMap;

pub struct Step;

impl Handler for Step {
    fn name(&self) -> &'static str {
        "STEP"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if !p21::is_p21(probe.head) {
            return false;
        }
        match p21::head_schema(probe.head) {
            Some(schema) => !schema.contains("IFC"),
            None => !probe.ext_is(&["ifc"]),
        }
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let text = input.text()?;
        let p = P21::parse(&text, cancelled)?;
        let schema = p
            .header_params("FILE_SCHEMA")
            .and_then(|(v, _)| {
                v.first()
                    .and_then(|l| l.list().first())
                    .and_then(P::str)
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let mut out = Inspection::new("STEP (ISO 10303-21)", &protocol(&schema));
        out.version = (!schema.is_empty()).then(|| clip(&schema, 120));
        p21::common(&p, &mut out, cancelled);
        products(&p, &mut out);
        units(&p, &mut out);
        geometry(&p, &mut out);
        out.notes.push(
            "B-rep geometry (faces, edges, surfaces) is counted but not tessellated, so there is no 3D view.".into(),
        );
        out.not_validated();
        Ok(out)
    }
}

/// The application protocol, from the schema name.
fn protocol(schema: &str) -> String {
    let s = schema.to_ascii_uppercase();
    let name = if s.starts_with("CONFIG_CONTROL_DESIGN") {
        "AP203 — configuration-controlled 3D design"
    } else if s.contains("AP203") {
        "AP203 edition 2 — configuration-controlled 3D design"
    } else if s.starts_with("AUTOMOTIVE_DESIGN") {
        "AP214 — automotive mechanical design"
    } else if s.contains("AP242") {
        "AP242 — managed model-based 3D engineering"
    } else if s.contains("AP209") {
        "AP209 — multidisciplinary analysis and design"
    } else if s.contains("AP210") || s.contains("ELECTRONIC_ASSEMBLY") {
        "AP210 — electronic assembly, interconnect and packaging"
    } else if s.contains("AP238") || s.contains("INTEGRATED_CNC") {
        "AP238 — STEP-NC machining"
    } else if s.starts_with("STRUCTURAL_FRAME_SCHEMA") {
        "CIS/2 — structural steel frames"
    } else {
        "STEP product model"
    };
    name.into()
}

fn text(v: &[P], k: usize) -> String {
    v.get(k).and_then(P::str).unwrap_or("").to_owned()
}

fn products(p: &P21, out: &mut Inspection) {
    // PRODUCT(id, name, description, frame_of_reference)
    let mut product: HashMap<u64, (String, String)> = HashMap::new();
    let mut rows = Rows::new("Products", &["#", "Product ID", "Name", "Description"]);
    for e in p.of_kind("PRODUCT") {
        let v = p.params(e);
        product.insert(e.id, (text(&v, 0), text(&v, 1)));
        rows.push(
            vec![format!("#{}", e.id), text(&v, 0), text(&v, 1), text(&v, 2)],
            Some(p.span(e)),
        );
    }
    out.fact("Products", "Products", rows.len().to_string(), None);
    rows.finish(out);
    // Formation -> product; definition -> formation.
    let mut formation: HashMap<u64, u64> = HashMap::new();
    for kind in [
        "PRODUCT_DEFINITION_FORMATION",
        "PRODUCT_DEFINITION_FORMATION_WITH_SPECIFIED_SOURCE",
    ] {
        for e in p.of_kind(kind) {
            if let Some(r) = p.params(e).get(2).and_then(P::reference) {
                formation.insert(e.id, r);
            }
        }
    }
    let mut definitions: Vec<u64> = vec![];
    let mut def_product: HashMap<u64, u64> = HashMap::new();
    for kind in [
        "PRODUCT_DEFINITION",
        "PRODUCT_DEFINITION_WITH_ASSOCIATED_DOCUMENTS",
    ] {
        for e in p.of_kind(kind) {
            definitions.push(e.id);
            if let Some(f) = p.params(e).get(2).and_then(P::reference)
                && let Some(&prod) = formation.get(&f)
            {
                def_product.insert(e.id, prod);
            }
        }
    }
    definitions.sort_unstable();
    let name_of = |pd: u64| -> (String, String) {
        def_product
            .get(&pd)
            .and_then(|pr| product.get(pr))
            .cloned()
            .unwrap_or_else(|| (String::new(), format!("#{pd}")))
    };
    // NEXT_ASSEMBLY_USAGE_OCCURRENCE(id, name, description, relating, related, designator)
    let mut children: HashMap<u64, Vec<(u64, String, u64)>> = HashMap::new();
    let mut related_any: std::collections::HashSet<u64> = Default::default();
    let mut usage = Rows::new(
        "Assembly usages",
        &[
            "#",
            "Assembly",
            "Component",
            "Occurrence ID",
            "Occurrence name",
        ],
    );
    for e in p.of_kind("NEXT_ASSEMBLY_USAGE_OCCURRENCE") {
        let v = p.params(e);
        let (Some(parent), Some(child)) = (
            v.get(3).and_then(P::reference),
            v.get(4).and_then(P::reference),
        ) else {
            continue;
        };
        let occ = if text(&v, 1).is_empty() {
            text(&v, 0)
        } else {
            text(&v, 1)
        };
        usage.push(
            vec![
                format!("#{}", e.id),
                name_of(parent).1,
                name_of(child).1,
                text(&v, 0),
                text(&v, 1),
            ],
            Some(p.span(e)),
        );
        related_any.insert(child);
        let list = children.entry(parent).or_default();
        if list.len() < 100_000 {
            list.push((child, occ, e.id));
        }
    }
    out.fact("Products", "Assembly usages", usage.len().to_string(), None);
    usage.finish(out);
    // The tree: definitions that are no one's component are roots.
    let roots: Vec<u64> = definitions
        .iter()
        .copied()
        .filter(|d| !related_any.contains(d))
        .collect();
    let mut outline = vec![];
    let mut stack: Vec<(u64, u16, String, Vec<u64>)> = roots
        .iter()
        .rev()
        .map(|&r| (r, 0u16, String::new(), vec![]))
        .collect();
    while let Some((pd, depth, occ, path)) = stack.pop() {
        if outline.len() >= MAX_ITEMS {
            break;
        }
        let (id, name) = name_of(pd);
        let value = match (id.is_empty(), occ.is_empty()) {
            (false, false) => format!("{id} · {occ}"),
            (false, true) => id,
            (true, _) => occ,
        };
        outline.push(OutlineNode {
            depth,
            label: if name.is_empty() {
                format!("#{pd}")
            } else {
                name
            },
            value,
            source: p.get(pd).map(|e| p.span(e)),
        });
        if path.contains(&pd) || depth >= 32 {
            continue;
        }
        if let Some(kids) = children.get(&pd) {
            let mut path = path.clone();
            path.push(pd);
            for (child, occ, _) in kids.iter().rev() {
                stack.push((*child, depth + 1, occ.clone(), path.clone()));
            }
        }
    }
    out.outline.extend(outline);
}

/// "MILLI" + "METRE" -> "millimetre".
fn si_name(prefix: Option<&str>, name: &str) -> String {
    format!(
        "{}{}",
        prefix.unwrap_or("").to_ascii_lowercase(),
        name.to_ascii_lowercase().replace('_', " ")
    )
}

fn units(p: &P21, out: &mut Inspection) {
    let mut rows = Rows::new("Units", &["#", "Quantity", "Unit"]);
    for e in &p.entities {
        let kind = p.kind(e);
        if !(kind.contains("SI_UNIT") || kind.contains("CONVERSION_BASED_UNIT")) {
            continue;
        }
        let parts = p.parts(e);
        let quantity = parts
            .iter()
            .find_map(|(n, _)| match n.as_str() {
                "LENGTH_UNIT" => Some("Length"),
                "PLANE_ANGLE_UNIT" => Some("Plane angle"),
                "SOLID_ANGLE_UNIT" => Some("Solid angle"),
                "MASS_UNIT" => Some("Mass"),
                "TIME_UNIT" => Some("Time"),
                "AREA_UNIT" => Some("Area"),
                "VOLUME_UNIT" => Some("Volume"),
                "THERMODYNAMIC_TEMPERATURE_UNIT" => Some("Temperature"),
                "RATIO_UNIT" => Some("Ratio"),
                _ => None,
            })
            .unwrap_or("Other");
        let unit = parts.iter().find_map(|(n, v)| match n.as_str() {
            // SI_UNIT(prefix, name); simple instances write the dimensions first.
            "SI_UNIT" => {
                let v: Vec<&P> = v.iter().filter(|x| !matches!(x, P::Derived)).collect();
                let (prefix, name) = match v.as_slice() {
                    [pr, n, ..] => (pr.enumeration(), n.enumeration()?),
                    [n] => (None, n.enumeration()?),
                    _ => return None,
                };
                Some(si_name(prefix, name))
            }
            "CONVERSION_BASED_UNIT" => v.first().and_then(P::str).map(|s| s.to_lowercase()),
            _ => None,
        });
        let Some(unit) = unit else { continue };
        if quantity == "Length" || quantity == "Plane angle" {
            let label = format!("{quantity} unit");
            if !out.facts.iter().any(|f| f.label == label) {
                out.fact("Units", &label, unit.clone(), Some(p.span(e)));
            }
        }
        rows.push(
            vec![format!("#{}", e.id), quantity.into(), unit],
            Some(p.span(e)),
        );
    }
    rows.finish(out);
}

fn geometry(p: &P21, out: &mut Inspection) {
    let counts = p.type_counts();
    let get = |k: &str| counts.get(k).copied().unwrap_or(0);
    let g = "Geometry (counted, not drawn)";
    let solids = get("MANIFOLD_SOLID_BREP") + get("BREP_WITH_VOIDS");
    if solids > 0 {
        out.fact(g, "B-rep solids", solids.to_string(), None);
    }
    for (kind, label) in [
        ("SHAPE_REPRESENTATION", "Shape representations"),
        (
            "ADVANCED_BREP_SHAPE_REPRESENTATION",
            "Advanced B-rep representations",
        ),
        (
            "MANIFOLD_SURFACE_SHAPE_REPRESENTATION",
            "Surface representations",
        ),
        ("CLOSED_SHELL", "Closed shells"),
        ("OPEN_SHELL", "Open shells"),
        ("ADVANCED_FACE", "Advanced faces"),
        ("FACE_SURFACE", "Faces"),
        ("EDGE_CURVE", "Edges"),
        ("VERTEX_POINT", "Vertices"),
        ("CARTESIAN_POINT", "Cartesian points"),
        ("PLANE", "Planes"),
        ("CYLINDRICAL_SURFACE", "Cylindrical surfaces"),
        ("CONICAL_SURFACE", "Conical surfaces"),
        ("SPHERICAL_SURFACE", "Spherical surfaces"),
        ("TOROIDAL_SURFACE", "Toroidal surfaces"),
        ("B_SPLINE_SURFACE_WITH_KNOTS", "B-spline surfaces"),
        ("LINE", "Lines"),
        ("CIRCLE", "Circles"),
        ("ELLIPSE", "Ellipses"),
        ("B_SPLINE_CURVE_WITH_KNOTS", "B-spline curves"),
        ("TRIANGULATED_FACE", "Triangulated faces (tessellated)"),
        ("STYLED_ITEM", "Styled items (colours)"),
    ] {
        let n = get(kind);
        if n > 0 {
            out.fact(g, label, n.to_string(), None);
        }
    }
}
