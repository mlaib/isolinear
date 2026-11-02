//! AUTOSAR ARXML (`AUTOSAR` root in `http://autosar.org/schema/r4.0` or `http://autosar.org/3.x`):
//! the schema version, the package tree, software components with their ports, port
//! interfaces, data types and system/ECU configuration counts.
use super::cad::{Rows, clip};
use crate::inspect::{
    Inspection, MAX_ITEMS, OutlineNode, Stage, State,
    spec::{F, XmlSpec},
    xmldoc::Doc,
};
use std::collections::{HashMap, HashSet};

pub static ARXML: XmlSpec = XmlSpec {
    family: "AUTOSAR",
    kind: "ARXML model",
    matches: |root, ns| root == "AUTOSAR" && ns.starts_with("http://autosar.org/"),
    version,
    facts: &[
        F("Model", "Packages", "count:**/AR-PACKAGE"),
        F("Model", "Admin data", "ADMIN-DATA/LANGUAGE"),
    ],
    tables: &[],
    custom: Some(custom),
};

/// Element counts shown when present: (group, label, element names).
const COUNTS: &[(&str, &str, &[&str])] = &[
    (
        "Model",
        "Compositions",
        &["COMPOSITION-SW-COMPONENT-TYPE", "COMPOSITION-TYPE"],
    ),
    (
        "Model",
        "Sender-receiver interfaces",
        &["SENDER-RECEIVER-INTERFACE"],
    ),
    (
        "Model",
        "Client-server interfaces",
        &["CLIENT-SERVER-INTERFACE"],
    ),
    ("Model", "Runnables", &["RUNNABLE-ENTITY"]),
    ("System", "Systems", &["SYSTEM"]),
    ("System", "ECU instances", &["ECU-INSTANCE"]),
    (
        "System",
        "ECU configuration modules",
        &["ECUC-MODULE-CONFIGURATION-VALUES"],
    ),
    (
        "System",
        "ECU configuration containers",
        &["ECUC-CONTAINER-VALUE"],
    ),
    ("System", "CAN clusters", &["CAN-CLUSTER"]),
    ("System", "LIN clusters", &["LIN-CLUSTER"]),
    ("System", "FlexRay clusters", &["FLEXRAY-CLUSTER"]),
    ("System", "Ethernet clusters", &["ETHERNET-CLUSTER"]),
    (
        "System",
        "Frames",
        &["CAN-FRAME", "LIN-UNCONDITIONAL-FRAME", "FLEXRAY-FRAME"],
    ),
    ("System", "I-signals", &["I-SIGNAL"]),
    ("System", "I-PDUs", &["I-SIGNAL-I-PDU"]),
];

/// "http://autosar.org/schema/r4.0 AUTOSAR_4-3-0.xsd" -> "4.3.0"; 3.x from the namespace.
fn version(doc: &Doc) -> Option<String> {
    if let Some(loc) = doc.attr(0, "schemaLocation")
        && let Some(file) = loc.split_whitespace().nth(1)
    {
        let v = file
            .trim_end_matches(".xsd")
            .trim_start_matches("AUTOSAR_")
            .replace('-', ".");
        return Some(format!("AUTOSAR {v} ({file})"));
    }
    let ns = &doc.root().ns;
    ns.strip_prefix("http://autosar.org/")
        .map(|v| format!("AUTOSAR {}", v.trim_start_matches("schema/")))
}

fn is_component(name: &str) -> bool {
    name.ends_with("-SW-COMPONENT-TYPE")
        || name.ends_with("-SOFTWARE-COMPONENT-TYPE")
        || name == "COMPOSITION-TYPE"
}
fn is_interface(name: &str) -> bool {
    matches!(
        name,
        "SENDER-RECEIVER-INTERFACE"
            | "CLIENT-SERVER-INTERFACE"
            | "MODE-SWITCH-INTERFACE"
            | "PARAMETER-INTERFACE"
            | "NV-DATA-INTERFACE"
            | "TRIGGER-INTERFACE"
            | "CALPRM-INTERFACE"
    )
}
fn is_data_type(name: &str) -> bool {
    matches!(
        name,
        "APPLICATION-PRIMITIVE-DATA-TYPE"
            | "APPLICATION-RECORD-DATA-TYPE"
            | "APPLICATION-ARRAY-DATA-TYPE"
            | "IMPLEMENTATION-DATA-TYPE"
            | "SW-BASE-TYPE"
            | "COMPU-METHOD"
            | "DATA-CONSTR"
            | "UNIT"
            | "INTEGER-TYPE"
            | "REAL-TYPE"
            | "BOOLEAN-TYPE"
            | "RECORD-TYPE"
            | "ARRAY-TYPE"
            | "STRING-TYPE"
            | "OPAQUE-TYPE"
    )
}

fn child(doc: &Doc, e: u32, name: &str) -> Option<u32> {
    doc.get(e)
        .children
        .iter()
        .copied()
        .find(|&c| doc.get(c).name == name)
}
fn short_name(doc: &Doc, e: u32) -> String {
    child(doc, e, "SHORT-NAME")
        .map(|c| doc.text(c))
        .unwrap_or_default()
}
/// Short names of the elements under `e/container/*`.
fn members(doc: &Doc, e: u32, container: &str) -> Vec<(String, u32)> {
    child(doc, e, container)
        .map(|c| {
            doc.get(c)
                .children
                .iter()
                .map(|&m| (short_name(doc, m), m))
                .collect()
        })
        .unwrap_or_default()
}

fn custom(doc: &Doc, out: &mut Inspection) {
    // Every element with a SHORT-NAME has an absolute path: /Pkg/Sub/Name.
    let mut paths: HashSet<String> = HashSet::new();
    let mut stack: Vec<(u32, String, u16)> = vec![(0, String::new(), 0)];
    let mut outline = vec![];
    let mut components = Rows::new(
        "Software components",
        &["Component", "Kind", "Package", "Ports"],
    );
    let mut ports = Rows::new("Ports", &["Component", "Port", "Direction", "Interface"]);
    let mut interfaces = Rows::new(
        "Port interfaces",
        &["Interface", "Kind", "Package", "Members"],
    );
    let mut types = Rows::new("Data types", &["Name", "Kind", "Category", "Package"]);
    let mut refs: Vec<String> = vec![];
    let mut visited = 0usize;
    while let Some((e, path, depth)) = stack.pop() {
        visited += 1;
        if visited > 4_000_000 {
            break;
        }
        let el = doc.get(e);
        let name = short_name(doc, e);
        let here = if name.is_empty() || e == 0 {
            path.clone()
        } else {
            format!("{path}/{name}")
        };
        let package = path.clone();
        if !name.is_empty() && e != 0 && paths.len() < 2_000_000 {
            paths.insert(here.clone());
        }
        let kind = el.name.as_str();
        let mut child_depth = depth;
        if kind == "AR-PACKAGE"
            || (e != 0 && depth < 64 && !name.is_empty() && parent_is_elements(doc, e))
        {
            if outline.len() < MAX_ITEMS {
                outline.push(OutlineNode {
                    depth,
                    label: clip(&name, 120),
                    value: if kind == "AR-PACKAGE" {
                        "package".into()
                    } else {
                        kind.to_owned()
                    },
                    source: Some(el.span.clone()),
                });
            }
            child_depth = depth + 1;
        }
        if is_component(kind) {
            let p = members(doc, e, "PORTS");
            for (port, pe) in &p {
                let pk = doc.get(*pe).name.as_str();
                let direction = match pk {
                    "P-PORT-PROTOTYPE" => "provided",
                    "R-PORT-PROTOTYPE" => "required",
                    "PR-PORT-PROTOTYPE" => "provided and required",
                    other => other,
                };
                let iface = [
                    "PROVIDED-INTERFACE-TREF",
                    "REQUIRED-INTERFACE-TREF",
                    "PROVIDED-REQUIRED-INTERFACE-TREF",
                ]
                .iter()
                .filter_map(|t| child(doc, *pe, t))
                .map(|c| doc.text(c))
                .next()
                .unwrap_or_default();
                if !iface.is_empty() && refs.len() < 1_000_000 {
                    refs.push(iface.clone());
                }
                ports.push(
                    vec![name.clone(), port.clone(), direction.to_owned(), iface],
                    Some(doc.get(*pe).span.clone()),
                );
            }
            components.push(
                vec![
                    name.clone(),
                    kind.to_owned(),
                    package.clone(),
                    p.len().to_string(),
                ],
                Some(el.span.clone()),
            );
        } else if is_interface(kind) {
            let mut list: Vec<String> = vec![];
            for container in [
                "DATA-ELEMENTS",
                "OPERATIONS",
                "MODE-GROUPS",
                "PARAMETERS",
                "NV-DATAS",
                "TRIGGERS",
            ] {
                for (m, me) in members(doc, e, container) {
                    let args = members(doc, me, "ARGUMENTS");
                    let ty = ["TYPE-TREF"]
                        .iter()
                        .filter_map(|t| child(doc, me, t))
                        .map(|c| doc.text(c))
                        .next();
                    let shown = if !args.is_empty() {
                        format!(
                            "{m}({})",
                            args.iter()
                                .map(|a| a.0.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    } else if let Some(t) = ty {
                        format!("{m}: {}", t.rsplit('/').next().unwrap_or(&t))
                    } else {
                        m
                    };
                    if list.len() < 64 {
                        list.push(shown);
                    }
                }
            }
            interfaces.push(
                vec![
                    name.clone(),
                    kind.to_owned(),
                    package.clone(),
                    clip(&list.join("; "), 300),
                ],
                Some(el.span.clone()),
            );
        } else if is_data_type(kind) && !name.is_empty() {
            let category = child(doc, e, "CATEGORY")
                .map(|c| doc.text(c))
                .unwrap_or_default();
            types.push(
                vec![name.clone(), kind.to_owned(), category, package.clone()],
                Some(el.span.clone()),
            );
        }
        if depth < 200 {
            for &c in el.children.iter().rev() {
                stack.push((c, here.clone(), child_depth));
            }
        }
    }
    out.outline.extend(outline);
    out.fact(
        "Model",
        "Software components",
        components.len().to_string(),
        None,
    );
    out.fact("Model", "Ports", ports.len().to_string(), None);
    out.fact(
        "Model",
        "Port interfaces",
        interfaces.len().to_string(),
        None,
    );
    out.fact("Model", "Data types", types.len().to_string(), None);
    let mut counts: HashMap<&str, u64> = HashMap::new();
    for el in &doc.elements {
        *counts.entry(el.name.as_str()).or_default() += 1;
    }
    for (group, label, names) in COUNTS {
        let n: u64 = names
            .iter()
            .map(|n| counts.get(n).copied().unwrap_or(0))
            .sum();
        if n > 0 {
            out.fact(group, label, n.to_string(), None);
        }
    }
    components.finish(out);
    ports.finish(out);
    interfaces.finish(out);
    types.finish(out);
    // Interface references: resolved inside this file, or (legitimately) in another one.
    if !refs.is_empty() {
        let outside = refs.iter().filter(|r| !paths.contains(r.as_str())).count();
        out.check(
            Stage::Semantic,
            if outside == 0 { State::Passed } else { State::Partial },
            if outside == 0 {
                format!("Port interfaces: all {} references resolve in this file", refs.len())
            } else {
                format!(
                    "Port interfaces: {outside} of {} references point outside this file (they may be defined in another ARXML file)",
                    refs.len()
                )
            },
        );
    }
}

/// Whether `e` is a model element: a child of ELEMENTS (R4) or of a 3.x package's ELEMENTS.
fn parent_is_elements(doc: &Doc, e: u32) -> bool {
    doc.get(e)
        .parent
        .is_some_and(|p| doc.get(p).name == "ELEMENTS")
}
