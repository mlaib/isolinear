//! Akoma Ntoso (OASIS LegalDocML) legal documents: the document type, its FRBR identification
//! (work, expression, manifestation), lifecycle and references, an outline of the hierarchy
//! (book … article, paragraph) with byte ranges, and counts of amendments.
use super::support::{short, value};
use crate::inspect::{
    Inspection, MAX_ITEMS, OutlineNode, Row, Table,
    spec::{F, T, XmlSpec, select},
    xmldoc::Doc,
};

const NS: &str = "http://docs.oasis-open.org/legaldocml/ns/akn/3.0";

/// Hierarchical containers shown in the outline.
const HIERARCHY: &[&str] = &[
    "book",
    "part",
    "title",
    "subtitle",
    "chapter",
    "subchapter",
    "section",
    "subsection",
    "article",
    "paragraph",
    "subparagraph",
    "clause",
    "subclause",
    "rule",
    "subrule",
    "division",
    "subdivision",
    "tome",
    "point",
    "level",
    "hcontainer",
    // Judgments and debates.
    "introduction",
    "background",
    "arguments",
    "remedies",
    "motivation",
    "decision",
    "debateSection",
];

/// The document element under `akomaNtoso` (act, bill, judgment, …).
fn document(doc: &Doc) -> Option<u32> {
    doc.root()
        .children
        .iter()
        .copied()
        .find(|&c| doc.get(c).name != "components")
}

fn outline(doc: &Doc, from: u32, depth: u16, out: &mut Inspection) {
    if depth > 40 {
        return;
    }
    for &c in &doc.get(from).children {
        if out.outline.len() >= MAX_ITEMS {
            return;
        }
        let el = doc.get(c);
        // Akoma Ntoso's `title` inside the hierarchy is a container; in `meta` or the
        // preface it does not occur with that name, so the name is enough.
        if HIERARCHY.contains(&el.name.as_str()) {
            let num = value(doc, c, "num").unwrap_or_default();
            let heading = value(doc, c, "heading")
                .or_else(|| value(doc, c, "subheading"))
                .unwrap_or_default();
            let mut label = el.name.clone();
            if let Some(first) = label.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            // "Chapter I" when the number already names the level, else "Paragraph 1.".
            if num.to_lowercase().starts_with(&el.name.to_lowercase()) {
                label = num;
            } else if !num.is_empty() {
                label = format!("{label} {num}");
            }
            out.outline.push(OutlineNode {
                depth,
                label,
                value: short(&heading, 200),
                source: Some(el.span.clone()),
            });
            outline(doc, c, depth + 1, out);
        } else if !matches!(el.name.as_str(), "meta" | "num" | "heading" | "p") {
            outline(doc, c, depth, out);
        }
    }
}

fn custom(doc: &Doc, out: &mut Inspection) {
    if let Some(d) = document(doc) {
        let el = doc.get(d);
        let mut kind = el.name.clone();
        if let Some(n) = doc.attr(d, "name").filter(|n| *n != el.name) {
            kind = format!("{kind} ({n})");
        }
        out.kind = format!("Akoma Ntoso {kind}");
        out.fact("Document", "Type", el.name.clone(), Some(el.span.clone()));
        const BODIES: &[&str] = &[
            "body",
            "mainBody",
            "judgmentBody",
            "debateBody",
            "amendmentBody",
            "portionBody",
        ];
        let body = el
            .children
            .iter()
            .copied()
            .find(|&c| BODIES.contains(&doc.get(c).name.as_str()))
            .unwrap_or(d);
        outline(doc, body, 0, out);
    }
    // Modifications: what kind (substitution, insertion, repeal…), from where, to where.
    let mut table = Table {
        name: "Modifications".into(),
        columns: ["Direction", "Type", "Source", "Destination", "Force"]
            .map(String::from)
            .to_vec(),
        rows: vec![],
        total: None,
    };
    for (direction, path) in [
        ("active", "**/activeModifications/*"),
        ("passive", "**/passiveModifications/*"),
    ] {
        for m in select(doc, 0, path) {
            if table.rows.len() >= crate::inspect::MAX_ROWS {
                break;
            }
            table.rows.push(Row {
                cells: vec![
                    direction.into(),
                    doc.attr(m, "type")
                        .map(str::to_owned)
                        .unwrap_or_else(|| doc.get(m).name.clone()),
                    value(doc, m, "source/@href").unwrap_or_default(),
                    value(doc, m, "destination/@href").unwrap_or_default(),
                    value(doc, m, "force/@period").unwrap_or_default(),
                ],
                source: Some(doc.get(m).span.clone()),
            });
        }
    }
    if !table.rows.is_empty() {
        out.tables.push(table);
    }
    out.notes.push(
        "References are listed as written; the documents they point to are not resolved".into(),
    );
}

pub static DOCUMENT: XmlSpec = XmlSpec {
    family: "Akoma Ntoso",
    kind: "Akoma Ntoso document",
    matches: |root, ns| root == "akomaNtoso" && ns.starts_with(NS),
    version: |doc| {
        let ns = &doc.root().ns;
        Some(match ns.strip_prefix(NS) {
            Some(rest) if !rest.is_empty() => format!("3.0 {}", rest.trim_start_matches('/')),
            _ => "3.0".into(),
        })
    },
    facts: &[
        F("Document", "Name", "*/@name"),
        F(
            "Document",
            "Title",
            "**/preface/**/docTitle | **/coverPage/**/docTitle | **/meta/**/FRBRWork/FRBRname/@value",
        ),
        F(
            "Document",
            "Number",
            "**/preface/**/docNumber | **/FRBRWork/FRBRnumber/@value",
        ),
        F("Document", "Date", "**/preface/**/docDate/@date"),
        F("Work", "URI", "**/identification/FRBRWork/FRBRthis/@value"),
        F(
            "Work",
            "Work URI",
            "**/identification/FRBRWork/FRBRuri/@value",
        ),
        F("Work", "Date", "**/identification/FRBRWork/FRBRdate/@date"),
        F(
            "Work",
            "Date meaning",
            "**/identification/FRBRWork/FRBRdate/@name",
        ),
        F(
            "Work",
            "Author",
            "**/identification/FRBRWork/FRBRauthor/@href",
        ),
        F(
            "Work",
            "Country",
            "**/identification/FRBRWork/FRBRcountry/@value",
        ),
        F(
            "Work",
            "Subtype",
            "**/identification/FRBRWork/FRBRsubtype/@value",
        ),
        F(
            "Expression",
            "URI",
            "**/identification/FRBRExpression/FRBRthis/@value",
        ),
        F(
            "Expression",
            "Date",
            "**/identification/FRBRExpression/FRBRdate/@date",
        ),
        F(
            "Expression",
            "Author",
            "**/identification/FRBRExpression/FRBRauthor/@href",
        ),
        F(
            "Expression",
            "Language",
            "**/identification/FRBRExpression/FRBRlanguage/@language",
        ),
        F(
            "Manifestation",
            "URI",
            "**/identification/FRBRManifestation/FRBRthis/@value",
        ),
        F(
            "Manifestation",
            "Date",
            "**/identification/FRBRManifestation/FRBRdate/@date",
        ),
        F(
            "Manifestation",
            "Author",
            "**/identification/FRBRManifestation/FRBRauthor/@href",
        ),
        F("Counts", "Lifecycle events", "count:**/lifecycle/eventRef"),
        F("Counts", "References (meta)", "count:**/meta/references/*"),
        F("Counts", "Inline references", "count:**/ref"),
        F(
            "Counts",
            "Active modifications",
            "count:**/activeModifications/*",
        ),
        F(
            "Counts",
            "Passive modifications",
            "count:**/passiveModifications/*",
        ),
        F("Counts", "Articles", "count:**/article"),
    ],
    tables: &[
        T {
            name: "Lifecycle",
            rows: "**/lifecycle/eventRef",
            columns: &[
                ("Date", "@date"),
                ("Type", "@type"),
                ("Source", "@source"),
                ("ID", "@eId | @id"),
            ],
        },
        T {
            name: "References",
            rows: "**/meta/references/*",
            columns: &[
                ("ID", "@eId | @id"),
                ("Link", "@href"),
                ("Shown as", "@showAs"),
            ],
        },
        T {
            name: "Inline references",
            rows: "**/ref",
            columns: &[("Text", "."), ("Link", "@href")],
        },
    ],
    custom: Some(custom),
};
