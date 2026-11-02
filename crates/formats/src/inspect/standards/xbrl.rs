//! XBRL 2.1 instance documents and Inline XBRL 1.1 (facts tagged inside XHTML). Contexts,
//! units and facts are listed as they are written; the taxonomy (labels, calculations,
//! dimensional rules) is not loaded, so nothing is validated against it.
use super::support::{qualified, short, value};
use crate::inspect::{
    Inspection, MAX_ROWS, Row, Stage, State, Table,
    spec::{F, XmlSpec},
    xmldoc::Doc,
};
use std::collections::HashMap;

const INSTANCE_NS: &str = "http://www.xbrl.org/2003/instance";
const INLINE_NS: &str = "http://www.xbrl.org/2013/inlineXBRL";
const LINK_NS: &str = "http://www.xbrl.org/2003/linkbase";

/// Whether the start of an XHTML file declares the Inline XBRL namespace.
pub fn is_inline(head: &str) -> bool {
    head.contains(INLINE_NS)
}

struct Context {
    entity: String,
    period: String,
    dims: String,
}

/// "2025-12-31", "2025-01-01 – 2025-12-31" or "forever".
fn period(doc: &Doc, c: u32) -> String {
    if let Some(i) = value(doc, c, "period/instant") {
        return i;
    }
    match (
        value(doc, c, "period/startDate"),
        value(doc, c, "period/endDate"),
    ) {
        (Some(s), Some(e)) => format!("{s} – {e}"),
        (s, e) => {
            if !crate::inspect::spec::select(doc, c, "period/forever").is_empty() {
                "forever".into()
            } else {
                s.or(e).unwrap_or_default()
            }
        }
    }
}

/// "dim=member; dim=member" from the segment and scenario of a context.
fn dimensions(doc: &Doc, c: u32) -> String {
    let mut parts = vec![];
    for e in doc.descendants(c) {
        let el = doc.get(e);
        match el.name.as_str() {
            "explicitMember" => parts.push(format!(
                "{}={}",
                doc.attr(e, "dimension").unwrap_or("?"),
                doc.text(e)
            )),
            "typedMember" => parts.push(format!(
                "{}={}",
                doc.attr(e, "dimension").unwrap_or("?"),
                short(&doc.text(e), 80)
            )),
            _ => {}
        }
        if parts.len() >= 64 {
            break;
        }
    }
    parts.join("; ")
}

fn contexts_table(doc: &Doc, contexts: &[u32], out: &mut Inspection) -> HashMap<String, Context> {
    let mut map = HashMap::new();
    let mut table = Table {
        name: "Contexts".into(),
        columns: ["ID", "Entity", "Scheme", "Period", "Dimensions"]
            .map(String::from)
            .to_vec(),
        rows: vec![],
        total: (contexts.len() > MAX_ROWS).then_some(contexts.len() as u64),
    };
    for &c in contexts.iter().take(MAX_ROWS) {
        let id = doc.attr(c, "id").unwrap_or("").to_owned();
        let ctx = Context {
            entity: value(doc, c, "entity/identifier").unwrap_or_default(),
            period: period(doc, c),
            dims: dimensions(doc, c),
        };
        table.rows.push(Row {
            cells: vec![
                id.clone(),
                ctx.entity.clone(),
                value(doc, c, "entity/identifier/@scheme").unwrap_or_default(),
                ctx.period.clone(),
                ctx.dims.clone(),
            ],
            source: Some(doc.get(c).span.clone()),
        });
        map.insert(id, ctx);
    }
    if !table.rows.is_empty() {
        out.tables.push(table);
    }
    map
}

fn units_table(doc: &Doc, units: &[u32], out: &mut Inspection) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut table = Table {
        name: "Units".into(),
        columns: ["ID", "Measure"].map(String::from).to_vec(),
        rows: vec![],
        total: (units.len() > MAX_ROWS).then_some(units.len() as u64),
    };
    let measures = |doc: &Doc, from: u32| -> String {
        doc.get(from)
            .children
            .iter()
            .filter(|&&m| doc.get(m).name == "measure")
            .map(|&m| doc.text(m))
            .collect::<Vec<_>>()
            .join(" × ")
    };
    for &u in units.iter().take(MAX_ROWS) {
        let id = doc.attr(u, "id").unwrap_or("").to_owned();
        let divide = crate::inspect::spec::select(doc, u, "divide");
        let measure = match divide.first() {
            Some(&d) => {
                let num = crate::inspect::spec::select(doc, d, "unitNumerator");
                let den = crate::inspect::spec::select(doc, d, "unitDenominator");
                format!(
                    "{} / {}",
                    num.first().map(|&n| measures(doc, n)).unwrap_or_default(),
                    den.first().map(|&n| measures(doc, n)).unwrap_or_default()
                )
            }
            None => measures(doc, u),
        };
        table.rows.push(Row {
            cells: vec![id.clone(), measure.clone()],
            source: Some(doc.get(u).span.clone()),
        });
        map.insert(id, measure);
    }
    if !table.rows.is_empty() {
        out.tables.push(table);
    }
    map
}

/// Footnotes attached to each fact ID through the footnote links' locators and arcs.
fn footnotes(doc: &Doc) -> HashMap<String, usize> {
    let mut label_to_fact: HashMap<String, Vec<String>> = HashMap::new();
    let mut counts = HashMap::new();
    let links: Vec<u32> = doc
        .elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.name == "footnoteLink" && e.ns == LINK_NS)
        .map(|(i, _)| i as u32)
        .collect();
    for &l in &links {
        for &c in &doc.get(l).children {
            if doc.get(c).name != "loc" {
                continue;
            }
            if let (Some(href), Some(label)) = (doc.attr(c, "href"), doc.attr(c, "label")) {
                let id = href.rsplit('#').next().unwrap_or(href).to_owned();
                label_to_fact.entry(label.to_owned()).or_default().push(id);
            }
        }
        for &c in &doc.get(l).children {
            if doc.get(c).name != "footnoteArc" {
                continue;
            }
            if let Some(ids) = doc.attr(c, "from").and_then(|f| label_to_fact.get(f)) {
                for id in ids {
                    *counts.entry(id.clone()).or_insert(0) += 1;
                }
            }
        }
    }
    counts
}

/// Unresolved context and unit references, as a business-rule check on the instance itself.
fn reference_check(out: &mut Inspection, facts: usize, missing_ctx: usize, missing_unit: usize) {
    if facts == 0 {
        return;
    }
    let state = if missing_ctx + missing_unit == 0 {
        State::Passed
    } else {
        State::Failed
    };
    out.check(
        Stage::Semantic,
        state,
        format!(
            "Context and unit references of {facts} facts: {missing_ctx} unknown contexts, {missing_unit} unknown units"
        ),
    );
}

fn taxonomy_notes(out: &mut Inspection) {
    out.check(
        Stage::Semantic,
        State::NotDone,
        "XBRL calculations, dimensions and taxonomy rules were not checked (the taxonomy is not loaded)",
    );
    out.notes.push(
        "Concepts are shown by their QName; labels, calculations and presentation come from the taxonomy, which Isolinear does not fetch"
            .into(),
    );
}

const INFRASTRUCTURE: &[&str] = &[
    "schemaRef",
    "linkbaseRef",
    "roleRef",
    "arcroleRef",
    "context",
    "unit",
    "footnoteLink",
];

fn instance(doc: &Doc, out: &mut Inspection) {
    let root = doc.root();
    let contexts: Vec<u32> = root
        .children
        .iter()
        .copied()
        .filter(|&c| doc.get(c).name == "context")
        .collect();
    let units: Vec<u32> = root
        .children
        .iter()
        .copied()
        .filter(|&c| doc.get(c).name == "unit")
        .collect();
    let ctx = contexts_table(doc, &contexts, out);
    let unit_map = units_table(doc, &units, out);
    let notes = footnotes(doc);
    let facts: Vec<u32> = root
        .children
        .iter()
        .copied()
        .filter(|&c| {
            let e = doc.get(c);
            !(INFRASTRUCTURE.contains(&e.name.as_str()) && (e.ns == INSTANCE_NS || e.ns == LINK_NS))
        })
        .collect();
    let (mut missing_ctx, mut missing_unit) = (0, 0);
    let mut table = Table {
        name: "Facts".into(),
        columns: [
            "Concept",
            "Value",
            "Context",
            "Period",
            "Entity",
            "Unit",
            "Decimals",
            "Dimensions",
            "Footnotes",
        ]
        .map(String::from)
        .to_vec(),
        rows: vec![],
        total: (facts.len() > MAX_ROWS).then_some(facts.len() as u64),
    };
    for &f in facts.iter().take(MAX_ROWS) {
        let context = doc.attr(f, "contextRef").unwrap_or("").to_owned();
        let unit = doc.attr(f, "unitRef").unwrap_or("").to_owned();
        let c = ctx.get(&context);
        if !context.is_empty() && c.is_none() {
            missing_ctx += 1;
        }
        if !unit.is_empty() && !unit_map.contains_key(&unit) {
            missing_unit += 1;
        }
        let nil = doc.attr(f, "nil") == Some("true");
        let value = if nil {
            "(nil)".into()
        } else if doc.get(f).children.is_empty() {
            short(&doc.text(f), 300)
        } else {
            format!("(tuple of {} items)", doc.get(f).children.len())
        };
        let notes = doc
            .attr(f, "id")
            .and_then(|id| notes.get(id))
            .map(|n| n.to_string())
            .unwrap_or_default();
        table.rows.push(Row {
            cells: vec![
                qualified(doc, f),
                value,
                context,
                c.map(|c| c.period.clone()).unwrap_or_default(),
                c.map(|c| c.entity.clone()).unwrap_or_default(),
                unit_map.get(&unit).cloned().unwrap_or(unit),
                doc.attr(f, "decimals")
                    .or(doc.attr(f, "precision").map(|_| "(precision)"))
                    .unwrap_or("")
                    .to_owned(),
                c.map(|c| c.dims.clone()).unwrap_or_default(),
                notes,
            ],
            source: Some(doc.get(f).span.clone()),
        });
    }
    out.fact("Instance", "Facts", facts.len().to_string(), None);
    reference_check(out, facts.len(), missing_ctx, missing_unit);
    if !table.rows.is_empty() {
        out.tables.insert(0, table);
    }
    taxonomy_notes(out);
}

pub static INSTANCE: XmlSpec = XmlSpec {
    family: "XBRL",
    kind: "XBRL instance",
    matches: |root, ns| root == "xbrl" && ns == INSTANCE_NS,
    version: |_| Some("XBRL 2.1".into()),
    facts: &[
        F("Instance", "Taxonomy entry point", "schemaRef/@href"),
        F("Instance", "Linkbase", "linkbaseRef/@href"),
        F("Instance", "Contexts", "count:context"),
        F("Instance", "Units", "count:unit"),
        F("Instance", "Footnotes", "count:footnoteLink/footnote"),
        F("Entity", "Identifier", "context/entity/identifier"),
        F("Entity", "Scheme", "context/entity/identifier/@scheme"),
    ],
    tables: &[],
    custom: Some(instance),
};

fn inline(doc: &Doc, out: &mut Inspection) {
    let contexts: Vec<u32> = super::support::find_all(doc, "context", INSTANCE_NS);
    let units: Vec<u32> = super::support::find_all(doc, "unit", INSTANCE_NS);
    let ctx = contexts_table(doc, &contexts, out);
    let unit_map = units_table(doc, &units, out);
    let facts: Vec<u32> = doc
        .elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.ns == INLINE_NS && (e.name == "nonFraction" || e.name == "nonNumeric"))
        .map(|(i, _)| i as u32)
        .collect();
    let (mut missing_ctx, mut missing_unit) = (0, 0);
    let mut table = Table {
        name: "Facts".into(),
        columns: [
            "Concept",
            "Displayed value",
            "Type",
            "Scale",
            "Sign",
            "Format",
            "Context",
            "Period",
            "Unit",
            "Decimals",
            "Dimensions",
        ]
        .map(String::from)
        .to_vec(),
        rows: vec![],
        total: (facts.len() > MAX_ROWS).then_some(facts.len() as u64),
    };
    for &f in facts.iter().take(MAX_ROWS) {
        let context = doc.attr(f, "contextRef").unwrap_or("").to_owned();
        let unit = doc.attr(f, "unitRef").unwrap_or("").to_owned();
        let c = ctx.get(&context);
        if !context.is_empty() && c.is_none() {
            missing_ctx += 1;
        }
        if !unit.is_empty() && !unit_map.contains_key(&unit) {
            missing_unit += 1;
        }
        let attr = |n: &str| doc.attr(f, n).unwrap_or("").to_owned();
        let numeric = doc.get(f).name == "nonFraction";
        table.rows.push(Row {
            cells: vec![
                attr("name"),
                if doc.attr(f, "nil") == Some("true") {
                    "(nil)".into()
                } else {
                    short(&doc.text(f), 300)
                },
                if numeric { "numeric" } else { "text" }.into(),
                attr("scale"),
                attr("sign"),
                attr("format"),
                context,
                c.map(|c| c.period.clone()).unwrap_or_default(),
                unit_map.get(&unit).cloned().unwrap_or(unit),
                attr("decimals"),
                c.map(|c| c.dims.clone()).unwrap_or_default(),
            ],
            source: Some(doc.get(f).span.clone()),
        });
    }
    let numeric = facts
        .iter()
        .filter(|&&f| doc.get(f).name == "nonFraction")
        .count();
    out.fact("Inline XBRL", "Numeric facts", numeric.to_string(), None);
    out.fact(
        "Inline XBRL",
        "Text facts",
        (facts.len() - numeric).to_string(),
        None,
    );
    out.fact("Inline XBRL", "Contexts", contexts.len().to_string(), None);
    out.fact("Inline XBRL", "Units", units.len().to_string(), None);
    if let Some(&c) = contexts.first() {
        out.fact(
            "Entity",
            "Identifier",
            value(doc, c, "entity/identifier").unwrap_or_default(),
            Some(doc.get(c).span.clone()),
        );
    }
    reference_check(out, facts.len(), missing_ctx, missing_unit);
    if !table.rows.is_empty() {
        out.tables.insert(0, table);
    }
    out.notes.push(
        "Values are shown as displayed; transformation formats (ixt:), scale and sign are listed but not applied, and ix:continuation chains are not followed"
            .into(),
    );
    taxonomy_notes(out);
}

pub static INLINE: XmlSpec = XmlSpec {
    family: "XBRL",
    kind: "Inline XBRL report",
    matches: |root, _| root == "html",
    version: |_| Some("Inline XBRL 1.1".into()),
    facts: &[
        F("Document", "Title", "head/title"),
        F(
            "Inline XBRL",
            "Taxonomy entry point",
            "**/references/schemaRef/@href",
        ),
    ],
    tables: &[],
    custom: Some(inline),
};
