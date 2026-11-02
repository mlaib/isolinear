//! OPC UA information models (NodeSet2 XML, root `UANodeSet` in
//! `http://opcfoundation.org/UA/2011/03/UANodeSet.xsd`): namespaces, models, aliases, nodes,
//! reference counts and the hierarchy given by ParentNodeId.
use super::cad::{Rows, clip, count_table};
use crate::inspect::{
    Inspection, MAX_ITEMS, OutlineNode, Stage, State,
    spec::{F, T, XmlSpec},
    xmldoc::Doc,
};
use std::collections::{HashMap, HashSet};

const NS: &str = "http://opcfoundation.org/UA/2011/03/UANodeSet.xsd";

const CLASSES: &[&str] = &[
    "UAObject",
    "UAVariable",
    "UAMethod",
    "UAView",
    "UAObjectType",
    "UAVariableType",
    "UADataType",
    "UAReferenceType",
];

pub static NODESET: XmlSpec = XmlSpec {
    family: "OPC UA",
    kind: "Information model (NodeSet)",
    matches: |root, ns| root == "UANodeSet" && ns == NS,
    version: |doc| {
        crate::inspect::spec::value_at(doc, 0, "Models/Model/@Version")
            .map(|v| format!("Model version {}", v.0))
    },
    facts: &[
        F("Model", "Model URI", "Models/Model/@ModelUri"),
        F("Model", "Version", "Models/Model/@Version"),
        F("Model", "Publication date", "Models/Model/@PublicationDate"),
        F("Model", "Last modified", "@LastModified"),
        F("Model", "Namespace URIs", "count:NamespaceUris/Uri"),
        F(
            "Model",
            "Required models",
            "count:Models/Model/RequiredModel",
        ),
        F("Model", "Aliases", "count:Aliases/Alias"),
        F("Nodes", "Objects", "count:UAObject"),
        F("Nodes", "Variables", "count:UAVariable"),
        F("Nodes", "Methods", "count:UAMethod"),
        F("Nodes", "Object types", "count:UAObjectType"),
        F("Nodes", "Variable types", "count:UAVariableType"),
        F("Nodes", "Data types", "count:UADataType"),
        F("Nodes", "Reference types", "count:UAReferenceType"),
    ],
    tables: &[
        T {
            name: "Namespace URIs",
            rows: "NamespaceUris/Uri",
            columns: &[("URI", ".")],
        },
        T {
            name: "Models",
            rows: "Models/Model",
            columns: &[
                ("Model URI", "@ModelUri"),
                ("Version", "@Version"),
                ("Publication date", "@PublicationDate"),
            ],
        },
        T {
            name: "Required models",
            rows: "Models/Model/RequiredModel",
            columns: &[
                ("Model URI", "@ModelUri"),
                ("Version", "@Version"),
                ("Publication date", "@PublicationDate"),
                ("Required by", "../@ModelUri"),
            ],
        },
        T {
            name: "Aliases",
            rows: "Aliases/Alias",
            columns: &[("Alias", "@Alias"), ("NodeId", ".")],
        },
    ],
    custom: Some(custom),
};

/// Whether a value is written as a NodeId ("i=13", "ns=1;s=Pump", "g=…", "b=…").
fn is_node_id(v: &str) -> bool {
    let v = match v.strip_prefix("ns=") {
        Some(rest) => rest.split_once(';').map(|x| x.1).unwrap_or(""),
        None => v,
    };
    v.starts_with("i=") || v.starts_with("s=") || v.starts_with("g=") || v.starts_with("b=")
}

fn custom(doc: &Doc, out: &mut Inspection) {
    let nodes: Vec<u32> = doc
        .get(0)
        .children
        .iter()
        .copied()
        .filter(|&c| CLASSES.contains(&doc.get(c).name.as_str()))
        .collect();
    let aliases: HashSet<String> = doc
        .get(0)
        .children
        .iter()
        .filter(|&&c| doc.get(c).name == "Aliases")
        .flat_map(|&c| doc.get(c).children.clone())
        .filter_map(|a| doc.attr(a, "Alias").map(str::to_owned))
        .collect();
    let display = |e: u32| -> String {
        doc.get(e)
            .children
            .iter()
            .find(|&&c| doc.get(c).name == "DisplayName")
            .map(|&c| doc.text(c))
            .unwrap_or_default()
    };
    let mut by_id: HashMap<&str, u32> = HashMap::new();
    for &e in &nodes {
        if let Some(id) = doc.attr(e, "NodeId") {
            by_id.insert(id, e);
        }
    }
    let mut rows = Rows::new(
        "Nodes",
        &[
            "NodeId",
            "Class",
            "BrowseName",
            "DisplayName",
            "DataType",
            "Parent",
            "Type definition",
        ],
    );
    let mut refs: HashMap<String, u64> = HashMap::new();
    let mut children: HashMap<&str, Vec<u32>> = HashMap::new();
    let mut unknown_alias: HashSet<String> = HashSet::new();
    let mut ids: HashSet<&str> = HashSet::new();
    let mut duplicates = 0u64;
    for &e in &nodes {
        let id = doc.attr(e, "NodeId").unwrap_or("");
        if !ids.insert(id) {
            duplicates += 1;
        }
        let parent = doc.attr(e, "ParentNodeId").unwrap_or("");
        let mut type_def = String::new();
        for r in doc
            .get(e)
            .children
            .iter()
            .filter(|&&c| doc.get(c).name == "References")
            .flat_map(|&c| doc.get(c).children.clone())
        {
            let kind = doc.attr(r, "ReferenceType").unwrap_or("").to_owned();
            if kind == "HasTypeDefinition" && type_def.is_empty() {
                type_def = doc.text(r);
            }
            if !kind.is_empty() && !is_node_id(&kind) && !aliases.contains(&kind) {
                unknown_alias.insert(kind.clone());
            }
            if refs.len() < 10_000 {
                *refs.entry(kind).or_default() += 1;
            }
        }
        let data_type = doc.attr(e, "DataType").unwrap_or("");
        if !data_type.is_empty() && !is_node_id(data_type) && !aliases.contains(data_type) {
            unknown_alias.insert(data_type.to_owned());
        }
        if !parent.is_empty() {
            children.entry(parent).or_default().push(e);
        }
        rows.push(
            vec![
                id.to_owned(),
                doc.get(e).name.clone(),
                doc.attr(e, "BrowseName").unwrap_or("").to_owned(),
                display(e),
                data_type.to_owned(),
                parent.to_owned(),
                type_def,
            ],
            Some(doc.get(e).span.clone()),
        );
    }
    rows.finish(out);
    out.fact(
        "Nodes",
        "References",
        refs.values().sum::<u64>().to_string(),
        None,
    );
    count_table(out, "Reference types used", "Reference type", &refs);
    // Hierarchy: nodes whose parent is not in this file are roots.
    let mut stack: Vec<(u32, u16)> = nodes
        .iter()
        .rev()
        .filter(|&&e| {
            doc.attr(e, "ParentNodeId")
                .is_none_or(|p| !by_id.contains_key(p))
        })
        .filter(|&&e| {
            children.contains_key(doc.attr(e, "NodeId").unwrap_or(""))
                || doc.attr(e, "ParentNodeId").is_some()
        })
        .map(|&e| (e, 0))
        .collect();
    let mut seen: HashSet<u32> = HashSet::new();
    while let Some((e, depth)) = stack.pop() {
        if out.outline.len() >= MAX_ITEMS {
            break;
        }
        let browse = doc.attr(e, "BrowseName").unwrap_or("");
        let mut value = format!(
            "{} {}",
            doc.get(e).name,
            doc.attr(e, "NodeId").unwrap_or("")
        );
        if depth == 0
            && let Some(p) = doc.attr(e, "ParentNodeId")
        {
            value.push_str(&format!(" (parent {p} outside this file)"));
        }
        out.outline.push(OutlineNode {
            depth,
            label: clip(browse, 120),
            value,
            source: Some(doc.get(e).span.clone()),
        });
        if !seen.insert(e) || depth >= 64 {
            continue;
        }
        if let Some(kids) = children.get(doc.attr(e, "NodeId").unwrap_or("")) {
            for &k in kids.iter().rev() {
                stack.push((k, depth + 1));
            }
        }
    }
    // Checks.
    if unknown_alias.is_empty() {
        out.check(
            Stage::Semantic,
            State::Passed,
            "Aliases: every DataType and ReferenceType name is a NodeId or a defined alias",
        );
    } else {
        let mut list: Vec<String> = unknown_alias.into_iter().collect();
        list.sort();
        out.check(
            Stage::Semantic,
            State::Failed,
            format!("Aliases: not defined: {}", clip(&list.join(", "), 200)),
        );
    }
    out.check(
        Stage::Semantic,
        if duplicates == 0 {
            State::Passed
        } else {
            State::Failed
        },
        if duplicates == 0 {
            format!("NodeIds: all {} are unique", nodes.len())
        } else {
            format!("NodeIds: {duplicates} are used by more than one node")
        },
    );
}
