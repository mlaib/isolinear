//! IFC building models (ISO 16739; IFC2X3, IFC4, IFC4X3) in the ISO 10303-21 syntax: the
//! spatial structure (project, site, building, storey, space) from IfcRelAggregates and
//! IfcRelContainedInSpatialStructure, elements by class, property sets and quantities from
//! IfcRelDefinesByProperties, units and the authoring application. Geometry is not drawn.
use super::{
    cad::{Rows, clip, count_table, num},
    p21::{self, Entity, P, P21},
};
use crate::inspect::{Handler, Input, Inspection, MAX_ITEMS, OutlineNode, Probe, Stage, State};
use std::collections::{HashMap, HashSet};

pub struct Ifc;

impl Handler for Ifc {
    fn name(&self) -> &'static str {
        "IFC"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if !p21::is_p21(probe.head) {
            return false;
        }
        match p21::head_schema(probe.head) {
            Some(schema) => schema.contains("IFC"),
            None => probe.ext_is(&["ifc"]),
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
        let mut out = Inspection::new("IFC (ISO 16739)", "Building information model");
        out.version = (!schema.is_empty()).then(|| clip(&schema, 60));
        p21::common(&p, &mut out, cancelled);
        let m = Model::new(&p);
        m.project(&mut out);
        m.units(&mut out);
        m.spatial(&mut out);
        m.elements(&mut out);
        m.properties(&mut out);
        m.global_ids(&mut out, cancelled);
        out.notes.push(
            "Geometry (extrusions, B-reps, meshes) is not tessellated, so there is no 3D view."
                .into(),
        );
        out.not_validated();
        Ok(out)
    }
}

/// Readable names of common classes (files write them in upper case).
const CLASSES: &[&str] = &[
    "IfcProject",
    "IfcSite",
    "IfcBuilding",
    "IfcBuildingStorey",
    "IfcSpace",
    "IfcZone",
    "IfcFacility",
    "IfcFacilityPart",
    "IfcBridge",
    "IfcRoad",
    "IfcRailway",
    "IfcAlignment",
    "IfcWall",
    "IfcWallStandardCase",
    "IfcWallElementedCase",
    "IfcCurtainWall",
    "IfcSlab",
    "IfcSlabStandardCase",
    "IfcRoof",
    "IfcBeam",
    "IfcBeamStandardCase",
    "IfcColumn",
    "IfcColumnStandardCase",
    "IfcMember",
    "IfcPlate",
    "IfcDoor",
    "IfcDoorStandardCase",
    "IfcWindow",
    "IfcWindowStandardCase",
    "IfcStair",
    "IfcStairFlight",
    "IfcRamp",
    "IfcRampFlight",
    "IfcRailing",
    "IfcCovering",
    "IfcFooting",
    "IfcPile",
    "IfcChimney",
    "IfcShadingDevice",
    "IfcBuildingElementProxy",
    "IfcBuildingElementPart",
    "IfcOpeningElement",
    "IfcFurnishingElement",
    "IfcFurniture",
    "IfcSystemFurnitureElement",
    "IfcFlowTerminal",
    "IfcFlowSegment",
    "IfcFlowFitting",
    "IfcFlowController",
    "IfcDistributionElement",
    "IfcDistributionPort",
    "IfcPipeSegment",
    "IfcDuctSegment",
    "IfcPipeFitting",
    "IfcDuctFitting",
    "IfcAirTerminal",
    "IfcSanitaryTerminal",
    "IfcLightFixture",
    "IfcLamp",
    "IfcOutlet",
    "IfcSwitchingDevice",
    "IfcCableSegment",
    "IfcValve",
    "IfcPump",
    "IfcBoiler",
    "IfcElementAssembly",
    "IfcReinforcingBar",
    "IfcReinforcingMesh",
    "IfcTendon",
    "IfcDiscreteAccessory",
    "IfcMechanicalFastener",
    "IfcFastener",
    "IfcGeographicElement",
    "IfcVirtualElement",
    "IfcAnnotation",
    "IfcGrid",
    "IfcCivilElement",
    "IfcTransportElement",
    "IfcSensor",
    "IfcActuator",
    "IfcAlarm",
    "IfcFireSuppressionTerminal",
    "IfcElectricAppliance",
    "IfcUnitaryEquipment",
    "IfcCourse",
    "IfcPavement",
    "IfcKerb",
    "IfcEarthworksFill",
    "IfcEarthworksCut",
];

/// Spatial structure classes (upper case).
const SPATIAL: &[&str] = &[
    "IFCPROJECT",
    "IFCSITE",
    "IFCBUILDING",
    "IFCBUILDINGSTOREY",
    "IFCSPACE",
    "IFCFACILITY",
    "IFCFACILITYPART",
    "IFCBRIDGE",
    "IFCROAD",
    "IFCRAILWAY",
    "IFCZONE",
];

fn class_name(upper: &str) -> String {
    CLASSES
        .iter()
        .find(|c| c.eq_ignore_ascii_case(upper))
        .map(|c| (*c).to_owned())
        .unwrap_or_else(|| match upper.strip_prefix("IFC") {
            Some(rest) if !rest.is_empty() => {
                let mut s = String::from("Ifc");
                let mut chars = rest.chars();
                if let Some(c) = chars.next() {
                    s.push(c);
                    s.extend(chars.map(|c| c.to_ascii_lowercase()));
                }
                s
            }
            _ => upper.to_owned(),
        })
}

/// An IFC value for reading: `IFCLABEL('x')` -> x, `.T.` -> true.
fn value(v: &P) -> String {
    match v {
        P::Typed(_, items) if items.len() == 1 => value(&items[0]),
        P::Enum(e) => match e.as_str() {
            "T" => "true".into(),
            "F" => "false".into(),
            "U" => "unknown".into(),
            other => other.to_owned(),
        },
        P::List(items) => items.iter().map(value).collect::<Vec<_>>().join(", "),
        P::Real(r) => num(*r),
        P::Null => String::new(),
        other => other.show(),
    }
}

/// A property or quantity: name, value, unit and source.
type Item = (String, String, String, Option<std::ops::Range<u64>>);

struct Model<'a, 'b> {
    p: &'b P21<'a>,
    /// Spatial and element children: aggregates first, then contained elements.
    children: HashMap<u64, Vec<u64>>,
    container: HashMap<u64, u64>,
    contained: Vec<u64>,
    /// Unit names by IFC unit type ("LENGTHUNIT" -> "millimetre").
    units: Vec<(String, String, u64)>,
}

impl<'a, 'b> Model<'a, 'b> {
    fn new(p: &'b P21<'a>) -> Self {
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut container = HashMap::new();
        let mut contained = vec![];
        // IfcRelAggregates(GlobalId, OwnerHistory, Name, Description, RelatingObject, RelatedObjects)
        for kind in ["IFCRELAGGREGATES", "IFCRELNESTS"] {
            for e in p.of_kind(kind) {
                let v = p.params(e);
                if let Some(parent) = v.get(4).and_then(P::reference) {
                    let list = children.entry(parent).or_default();
                    for r in v
                        .get(5)
                        .map(P::list)
                        .unwrap_or(&[])
                        .iter()
                        .filter_map(P::reference)
                    {
                        if list.len() < 1_000_000 {
                            list.push(r);
                        }
                    }
                }
            }
        }
        // IfcRelContainedInSpatialStructure(GlobalId, OwnerHistory, Name, Description,
        // RelatedElements, RelatingStructure)
        for e in p.of_kind("IFCRELCONTAINEDINSPATIALSTRUCTURE") {
            let v = p.params(e);
            if let Some(parent) = v.get(5).and_then(P::reference) {
                let list = children.entry(parent).or_default();
                for r in v
                    .get(4)
                    .map(P::list)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(P::reference)
                {
                    if list.len() < 1_000_000 {
                        list.push(r);
                    }
                    container.insert(r, parent);
                    contained.push(r);
                }
            }
        }
        let mut m = Model {
            p,
            children,
            container,
            contained,
            units: vec![],
        };
        m.units = m.read_units();
        m
    }

    /// (GlobalId, Name) of a rooted object.
    fn named(&self, id: u64) -> (String, String, String) {
        let Some(e) = self.p.get(id) else {
            return (format!("#{id}"), String::new(), String::new());
        };
        let v = self.p.params(e);
        let s = |k: usize| v.get(k).and_then(P::str).unwrap_or("").to_owned();
        (class_name(self.p.kind(e)), s(0), s(2))
    }
    fn label(&self, id: u64) -> String {
        let (class, _, name) = self.named(id);
        if name.is_empty() {
            class
        } else {
            format!("{class} “{name}”")
        }
    }

    fn project(&self, out: &mut Inspection) {
        let g = "Project";
        if let Some(e) = self.p.of_kind("IFCPROJECT").next() {
            let v = self.p.params(e);
            let s = |k: usize| v.get(k).and_then(P::str).unwrap_or("").to_owned();
            let span = Some(self.p.span(e));
            out.fact(g, "Name", s(2), span.clone());
            out.fact(g, "Long name", s(5), span.clone());
            out.fact(g, "Description", s(3), span.clone());
            out.fact(g, "Phase", s(6), span.clone());
            out.fact(g, "GlobalId", s(0), span);
        }
        for (kind, label) in [("IFCSITE", "Site"), ("IFCBUILDING", "Building")] {
            let names: Vec<String> = self
                .p
                .of_kind(kind)
                .take(16)
                .map(|e| self.named(e.id).2)
                .filter(|n| !n.is_empty())
                .collect();
            out.fact(g, label, names.join(", "), None);
        }
        let storeys = self.p.count("IFCBUILDINGSTOREY");
        if storeys > 0 {
            out.fact(g, "Storeys", storeys.to_string(), None);
        }
        let spaces = self.p.count("IFCSPACE");
        if spaces > 0 {
            out.fact(g, "Spaces", spaces.to_string(), None);
        }
        // IfcApplication(ApplicationDeveloper, Version, ApplicationFullName, ApplicationIdentifier)
        if let Some(e) = self.p.of_kind("IFCAPPLICATION").next() {
            let v = self.p.params(e);
            let s = |k: usize| v.get(k).and_then(P::str).unwrap_or("").to_owned();
            let app = format!("{} {}", s(2), s(1)).trim().to_owned();
            out.fact(g, "Authoring application", app, Some(self.p.span(e)));
        }
    }

    fn read_units(&self) -> Vec<(String, String, u64)> {
        let mut list = vec![];
        let ids: Vec<u64> = self
            .p
            .of_kind("IFCUNITASSIGNMENT")
            .flat_map(|e| {
                self.p
                    .params(e)
                    .first()
                    .map(|l| l.list().iter().filter_map(P::reference).collect::<Vec<_>>())
                    .unwrap_or_default()
            })
            .take(256)
            .collect();
        for id in ids {
            let Some(e) = self.p.get(id) else { continue };
            let v = self.p.params(e);
            let kind = v.get(1).and_then(P::enumeration).unwrap_or("").to_owned();
            let name = match self.p.kind(e) {
                // IfcSIUnit(Dimensions, UnitType, Prefix, Name)
                "IFCSIUNIT" => format!(
                    "{}{}",
                    v.get(2)
                        .and_then(P::enumeration)
                        .unwrap_or("")
                        .to_lowercase(),
                    v.get(3)
                        .and_then(P::enumeration)
                        .unwrap_or("")
                        .to_lowercase()
                        .replace('_', " ")
                ),
                // IfcConversionBasedUnit(Dimensions, UnitType, Name, ConversionFactor)
                "IFCCONVERSIONBASEDUNIT" | "IFCCONVERSIONBASEDUNITWITHOFFSET" => {
                    v.get(2).and_then(P::str).unwrap_or("").to_lowercase()
                }
                // IfcMonetaryUnit(Currency)
                "IFCMONETARYUNIT" => {
                    let c = v.first().map(value).unwrap_or_default();
                    list.push(("MONETARYUNIT".into(), c, id));
                    continue;
                }
                "IFCDERIVEDUNIT" => "derived".into(),
                _ => continue,
            };
            list.push((kind, name, id));
        }
        list
    }

    fn unit(&self, kind: &str) -> &str {
        self.units
            .iter()
            .find(|u| u.0 == kind)
            .map(|u| u.1.as_str())
            .unwrap_or("")
    }

    fn units(&self, out: &mut Inspection) {
        let mut rows = Rows::new("Units", &["#", "Quantity", "Unit"]);
        for (kind, name, id) in &self.units {
            let quantity = kind.strip_suffix("UNIT").unwrap_or(kind).to_lowercase();
            let mut q = quantity.replace('_', " ");
            if let Some(c) = q.get_mut(..1) {
                c.make_ascii_uppercase();
            }
            if matches!(
                kind.as_str(),
                "LENGTHUNIT" | "AREAUNIT" | "VOLUMEUNIT" | "PLANEANGLEUNIT"
            ) {
                out.fact(
                    "Units",
                    &format!("{q} unit"),
                    name.clone(),
                    self.p.get(*id).map(|e| self.p.span(e)),
                );
            }
            rows.push(
                vec![format!("#{id}"), q, name.clone()],
                self.p.get(*id).map(|e| self.p.span(e)),
            );
        }
        rows.finish(out);
    }

    fn spatial(&self, out: &mut Inspection) {
        let roots: Vec<u64> = self.p.of_kind("IFCPROJECT").map(|e| e.id).collect();
        let mut seen: HashSet<u64> = HashSet::new();
        let mut stack: Vec<(u64, u16)> = roots.iter().rev().map(|&r| (r, 0)).collect();
        while let Some((id, depth)) = stack.pop() {
            if out.outline.len() >= MAX_ITEMS {
                break;
            }
            let (class, gid, name) = self.named(id);
            out.outline.push(OutlineNode {
                depth,
                label: if name.is_empty() {
                    class.clone()
                } else {
                    format!("{class} “{name}”")
                },
                value: gid,
                source: self.p.get(id).map(|e| self.p.span(e)),
            });
            if !seen.insert(id) || depth >= 32 {
                continue;
            }
            if let Some(kids) = self.children.get(&id) {
                for &k in kids.iter().rev() {
                    stack.push((k, depth + 1));
                }
            }
        }
    }

    fn elements(&self, out: &mut Inspection) {
        // Counts of spatial and element classes (known names, plus anything contained).
        let contained_kinds: HashSet<u32> = self
            .contained
            .iter()
            .filter_map(|&id| self.p.get(id).map(|e| e.kind))
            .collect();
        let mut counts: HashMap<String, u64> = HashMap::new();
        for e in &self.p.entities {
            let kind = self.p.kind(e);
            if contained_kinds.contains(&e.kind)
                || SPATIAL.contains(&kind)
                || CLASSES.iter().any(|c| c.eq_ignore_ascii_case(kind))
            {
                *counts.entry(class_name(kind)).or_default() += 1;
            }
        }
        let total: u64 = counts
            .iter()
            .filter(|(k, _)| !SPATIAL.iter().any(|s| s.eq_ignore_ascii_case(k)))
            .map(|(_, n)| n)
            .sum();
        out.fact("Project", "Elements", total.to_string(), None);
        count_table(out, "Elements by class", "Class", &counts);
        // IfcElement(GlobalId, OwnerHistory, Name, Description, ObjectType, ObjectPlacement,
        // Representation, Tag)
        let mut rows = Rows::new(
            "Elements",
            &["#", "Class", "Name", "Type", "Tag", "GlobalId", "Container"],
        );
        for &id in &self.contained {
            let Some(e) = self.p.get(id) else { continue };
            let v = self.p.params(e);
            let s = |k: usize| v.get(k).and_then(P::str).unwrap_or("").to_owned();
            let container = self
                .container
                .get(&id)
                .map(|&c| self.label(c))
                .unwrap_or_default();
            rows.push(
                vec![
                    format!("#{id}"),
                    class_name(self.p.kind(e)),
                    s(2),
                    s(4),
                    s(7),
                    s(0),
                    container,
                ],
                Some(self.p.span(e)),
            );
        }
        rows.finish(out);
    }

    fn properties(&self, out: &mut Inspection) {
        let mut props = Rows::new(
            "Properties",
            &["Element", "#", "Property set", "Property", "Value", "Unit"],
        );
        let mut quants = Rows::new(
            "Quantities",
            &["Element", "#", "Quantity set", "Quantity", "Value", "Unit"],
        );
        let (mut psets, mut qsets) = (HashSet::new(), HashSet::new());
        let mut cache: HashMap<u64, Vec<Item>> = HashMap::new();
        // IfcRelDefinesByProperties(GlobalId, OwnerHistory, Name, Description, RelatedObjects,
        // RelatingPropertyDefinition)
        for rel in self.p.of_kind("IFCRELDEFINESBYPROPERTIES") {
            let v = self.p.params(rel);
            let Some(def) = v.get(5).and_then(P::reference) else {
                continue;
            };
            let Some(d) = self.p.get(def) else { continue };
            let is_q = match self.p.kind(d) {
                "IFCPROPERTYSET" => false,
                "IFCELEMENTQUANTITY" => true,
                _ => continue,
            };
            let dv = self.p.params(d);
            let set_name = dv.get(2).and_then(P::str).unwrap_or("").to_owned();
            if is_q {
                qsets.insert(def);
            } else {
                psets.insert(def);
            }
            let items = cache
                .entry(def)
                .or_insert_with(|| self.set_items(&dv, is_q))
                .clone();
            for obj in v
                .get(4)
                .map(P::list)
                .unwrap_or(&[])
                .iter()
                .filter_map(P::reference)
            {
                let element = self.label(obj);
                for (name, val, unit, span) in &items {
                    let rows = if is_q { &mut quants } else { &mut props };
                    if rows.len() as usize >= crate::inspect::MAX_ROWS {
                        rows.push(vec![], None);
                        continue;
                    }
                    rows.push(
                        vec![
                            element.clone(),
                            format!("#{obj}"),
                            set_name.clone(),
                            name.clone(),
                            val.clone(),
                            unit.clone(),
                        ],
                        span.clone(),
                    );
                }
            }
        }
        out.fact("Properties", "Property sets", psets.len().to_string(), None);
        out.fact("Properties", "Quantity sets", qsets.len().to_string(), None);
        props.finish(out);
        quants.finish(out);
    }

    /// (name, value, unit, source) of each property or quantity of a set.
    fn set_items(&self, set: &[P], quantities: bool) -> Vec<Item> {
        // IfcPropertySet(.., HasProperties) at 4; IfcElementQuantity(.., MethodOfMeasurement,
        // Quantities) at 5.
        let list = set
            .get(if quantities { 5 } else { 4 })
            .map(P::list)
            .unwrap_or(&[]);
        let mut items = vec![];
        for id in list.iter().filter_map(P::reference).take(10_000) {
            let Some(e) = self.p.get(id) else { continue };
            let v = self.p.params(e);
            let name = v.first().and_then(P::str).unwrap_or("").to_owned();
            let kind = self.p.kind(e);
            let (val, unit) = if quantities {
                // IfcQuantityLength(Name, Description, Unit, LengthValue, Formula)
                let unit_kind = match kind {
                    "IFCQUANTITYLENGTH" => "LENGTHUNIT",
                    "IFCQUANTITYAREA" => "AREAUNIT",
                    "IFCQUANTITYVOLUME" => "VOLUMEUNIT",
                    "IFCQUANTITYWEIGHT" => "MASSUNIT",
                    "IFCQUANTITYTIME" => "TIMEUNIT",
                    _ => "",
                };
                (
                    v.get(3).map(value).unwrap_or_default(),
                    self.unit(unit_kind).to_owned(),
                )
            } else {
                match kind {
                    // IfcPropertySingleValue(Name, Description, NominalValue, Unit)
                    "IFCPROPERTYSINGLEVALUE"
                    | "IFCPROPERTYENUMERATEDVALUE"
                    | "IFCPROPERTYLISTVALUE" => {
                        (v.get(2).map(value).unwrap_or_default(), String::new())
                    }
                    // IfcPropertyBoundedValue(Name, Description, Upper, Lower, Unit, SetPoint)
                    "IFCPROPERTYBOUNDEDVALUE" => (
                        format!(
                            "{} – {}",
                            v.get(3).map(value).unwrap_or_default(),
                            v.get(2).map(value).unwrap_or_default()
                        ),
                        String::new(),
                    ),
                    other => (class_name(other), String::new()),
                }
            };
            items.push((name, clip(&val, 200), unit, Some(self.p.span(e))));
        }
        items
    }

    /// GlobalIds: 22 characters of the IFC base-64 alphabet, unique.
    fn global_ids(&self, out: &mut Inspection, cancelled: &dyn Fn() -> bool) {
        let rooted = |kind: &str| {
            kind.starts_with("IFCREL")
                || matches!(kind, "IFCPROPERTYSET" | "IFCELEMENTQUANTITY")
                || CLASSES.iter().any(|c| c.eq_ignore_ascii_case(kind))
        };
        let mut seen: HashSet<String> = HashSet::new();
        let (mut total, mut malformed, mut duplicate) = (0u64, 0u64, 0u64);
        let mut first_bad: Option<&Entity> = None;
        for (n, e) in self.p.entities.iter().enumerate() {
            if n % 65536 == 0 && cancelled() {
                return;
            }
            if !rooted(self.p.kind(e)) {
                continue;
            }
            let v = self.p.params(e);
            let Some(gid) = v.first().and_then(P::str) else {
                continue;
            };
            total += 1;
            let ok = gid.len() == 22
                && gid
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$');
            if !ok {
                malformed += 1;
                first_bad.get_or_insert(e);
            } else if !seen.insert(gid.to_owned()) {
                duplicate += 1;
                first_bad.get_or_insert(e);
            }
        }
        if total == 0 {
            return;
        }
        let state = if malformed + duplicate == 0 {
            State::Passed
        } else {
            State::Failed
        };
        let detail = if malformed + duplicate == 0 {
            format!("GlobalIds: {total} checked, all well-formed and unique")
        } else {
            format!(
                "GlobalIds: {malformed} malformed and {duplicate} repeated of {total} (first: #{})",
                first_bad.map(|e| e.id).unwrap_or(0)
            )
        };
        out.check(Stage::Semantic, state, detail);
    }
}
