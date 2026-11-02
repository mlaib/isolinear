//! HL7 CDA Release 2 and C-CDA clinical documents (`ClinicalDocument` in `urn:hl7-org:v3`).
use super::records::{Rows, clip, flatten, join, outline};
use crate::inspect::{
    Inspection,
    spec::{F, XmlSpec, select, value_at},
    xmldoc::Doc,
};

/// C-CDA document templates (US Realm), by template root.
const DOCUMENT_TEMPLATES: &[(&str, &str)] = &[
    (
        "2.16.840.1.113883.10.20.22.1.2",
        "Continuity of Care Document (CCD)",
    ),
    ("2.16.840.1.113883.10.20.22.1.3", "History and Physical"),
    ("2.16.840.1.113883.10.20.22.1.4", "Consultation Note"),
    (
        "2.16.840.1.113883.10.20.22.1.5",
        "Diagnostic Imaging Report",
    ),
    ("2.16.840.1.113883.10.20.22.1.6", "Procedure Note"),
    ("2.16.840.1.113883.10.20.22.1.7", "Operative Note"),
    ("2.16.840.1.113883.10.20.22.1.8", "Discharge Summary"),
    ("2.16.840.1.113883.10.20.22.1.9", "Progress Note"),
    ("2.16.840.1.113883.10.20.22.1.10", "Unstructured Document"),
    ("2.16.840.1.113883.10.20.22.1.13", "Transfer Summary"),
    ("2.16.840.1.113883.10.20.22.1.14", "Referral Note"),
    ("2.16.840.1.113883.10.20.22.1.15", "Care Plan"),
    ("2.16.840.1.113883.10.20.29.1", "Patient Generated Document"),
    (
        "2.16.840.1.113883.10.20.1",
        "Continuity of Care Document (HL7 CCD 1.0)",
    ),
    (
        "2.16.840.1.113883.10.20.22.1.1",
        "US Realm Header (generic C-CDA document)",
    ),
];

/// Common C-CDA section templates.
const SECTION_TEMPLATES: &[(&str, &str)] = &[
    (
        "2.16.840.1.113883.10.20.22.2.6",
        "Allergies and Intolerances",
    ),
    ("2.16.840.1.113883.10.20.22.2.1", "Medications"),
    ("2.16.840.1.113883.10.20.22.2.5", "Problems"),
    ("2.16.840.1.113883.10.20.22.2.3", "Results"),
    ("2.16.840.1.113883.10.20.22.2.4", "Vital Signs"),
    ("2.16.840.1.113883.10.20.22.2.2", "Immunizations"),
    ("2.16.840.1.113883.10.20.22.2.7", "Procedures"),
    ("2.16.840.1.113883.10.20.22.2.22", "Encounters"),
    ("2.16.840.1.113883.10.20.22.2.17", "Social History"),
    ("2.16.840.1.113883.10.20.22.2.10", "Plan of Treatment"),
    ("2.16.840.1.113883.10.20.22.2.14", "Functional Status"),
    ("2.16.840.1.113883.10.20.22.2.15", "Family History"),
    ("2.16.840.1.113883.10.20.22.2.21", "Advance Directives"),
    ("2.16.840.1.113883.10.20.22.2.18", "Payers"),
    ("2.16.840.1.113883.10.20.22.2.23", "Medical Equipment"),
    ("2.16.840.1.113883.10.20.22.2.11", "Discharge Medications"),
    ("2.16.840.1.113883.10.20.22.2.24", "Discharge Diagnosis"),
    (
        "2.16.840.1.113883.10.20.22.2.13",
        "Chief Complaint and Reason for Visit",
    ),
    ("2.16.840.1.113883.10.20.22.2.20", "Past Medical History"),
    ("2.16.840.1.113883.10.20.22.2.45", "Instructions"),
    ("2.16.840.1.113883.10.20.22.2.9", "Assessment and Plan"),
    ("2.16.840.1.113883.10.20.22.2.8", "Assessment"),
];

/// Clinical statements that can sit in an `entry` (or nested in one).
const STATEMENTS: &[&str] = &[
    "observation",
    "act",
    "substanceAdministration",
    "supply",
    "procedure",
    "encounter",
    "organizer",
    "regionOfInterest",
    "observationMedia",
];

fn template_name<'a>(table: &[(&str, &'a str)], root: &str) -> Option<&'a str> {
    // Exact match first; section templates also have ".1" variants (entries required).
    table
        .iter()
        .find(|(r, _)| *r == root)
        .or_else(|| {
            table
                .iter()
                .find(|(r, _)| root.strip_suffix(".1").is_some_and(|stem| stem == *r))
        })
        .map(|(_, n)| *n)
}

fn template_ids(doc: &Doc, e: u32) -> Vec<(String, String)> {
    select(doc, e, "templateId")
        .into_iter()
        .map(|t| {
            (
                doc.attr(t, "root").unwrap_or("").to_owned(),
                doc.attr(t, "extension").unwrap_or("").to_owned(),
            )
        })
        .collect()
}

fn version(doc: &Doc) -> Option<String> {
    let header = template_ids(doc, 0)
        .into_iter()
        .find(|(r, _)| r == "2.16.840.1.113883.10.20.22.1.1");
    Some(match header {
        Some((_, ext)) if ext == "2015-08-01" => "CDA R2, C-CDA R2.1".into(),
        Some((_, ext)) if ext == "2014-06-09" => "CDA R2, C-CDA R2.0".into(),
        Some((_, ext)) if ext.is_empty() => "CDA R2, C-CDA R1.1".into(),
        Some((_, ext)) => format!("CDA R2, C-CDA (templates {ext})"),
        None => "CDA R2".into(),
    })
}

/// A CD/CE/CV code: `displayName (codeSystem|code)`, or its original text.
fn code(doc: &Doc, e: u32) -> String {
    let display = doc.attr(e, "displayName").unwrap_or("").to_owned();
    let (system, c) = (
        doc.attr(e, "codeSystemName")
            .or(doc.attr(e, "codeSystem"))
            .unwrap_or(""),
        doc.attr(e, "code").unwrap_or(""),
    );
    let sc = join(&[system.to_owned(), c.to_owned()], "|");
    let original = value_at(doc, e, "originalText").map(|v| v.0);
    let display = if display.is_empty() {
        original.unwrap_or_default()
    } else {
        display
    };
    match (display.is_empty(), sc.is_empty()) {
        (false, false) => format!("{display} ({sc})"),
        (false, true) => display,
        (true, _) if doc.attr(e, "nullFlavor").is_some() => {
            format!("no code ({})", doc.attr(e, "nullFlavor").unwrap_or(""))
        }
        _ => sc,
    }
}

/// A `value` element (PQ, CD, ST, IVL…): its readable form.
fn value(doc: &Doc, e: u32) -> String {
    if let Some(v) = doc.attr(e, "value") {
        return join(
            &[v.to_owned(), doc.attr(e, "unit").unwrap_or("").to_owned()],
            " ",
        );
    }
    if doc.attr(e, "code").is_some() || doc.attr(e, "displayName").is_some() {
        return code(doc, e);
    }
    let lo = value_at(doc, e, "low/@value").map(|v| v.0);
    let hi = value_at(doc, e, "high/@value").map(|v| v.0);
    if lo.is_some() || hi.is_some() {
        return format!("{} – {}", lo.unwrap_or_default(), hi.unwrap_or_default());
    }
    clip(&doc.text(e), 120)
}

fn time(doc: &Doc, e: u32) -> String {
    value_at(
        doc,
        e,
        "effectiveTime/@value | effectiveTime/low/@value | effectiveTime/center/@value",
    )
    .map(|v| v.0)
    .unwrap_or_default()
}

pub static CDA: XmlSpec = XmlSpec {
    family: "HL7 CDA",
    kind: "Clinical document",
    matches: |root, ns| root == "ClinicalDocument" && ns == "urn:hl7-org:v3",
    version,
    facts: &[
        F("Document", "Title", "title"),
        F("Document", "Type code", "code/@code"),
        F("Document", "Type", "code/@displayName"),
        F(
            "Document",
            "Type code system",
            "code/@codeSystemName | code/@codeSystem",
        ),
        F("Document", "Effective time", "effectiveTime/@value"),
        F("Document", "Confidentiality", "confidentialityCode/@code"),
        F("Document", "Language", "languageCode/@code"),
        F("Document", "Document ID", "id/@extension | id/@root"),
        F("Document", "Set ID", "setId/@extension | setId/@root"),
        F("Document", "Version number", "versionNumber/@value"),
        F(
            "Patient",
            "Patient name",
            "recordTarget/patientRole/patient/name",
        ),
        F(
            "Patient",
            "Patient gender",
            "recordTarget/patientRole/patient/administrativeGenderCode/@displayName | recordTarget/patientRole/patient/administrativeGenderCode/@code",
        ),
        F(
            "Patient",
            "Patient birth time",
            "recordTarget/patientRole/patient/birthTime/@value",
        ),
        F(
            "Patient",
            "Patient address",
            "recordTarget/patientRole/addr",
        ),
        F(
            "Patient",
            "Patient telecom",
            "recordTarget/patientRole/telecom/@value",
        ),
        F(
            "Patient",
            "Patient language",
            "recordTarget/patientRole/patient/languageCommunication/languageCode/@code",
        ),
        F("Author", "Time", "author/time/@value"),
        F(
            "Author",
            "Author name",
            "author/assignedAuthor/assignedPerson/name",
        ),
        F(
            "Author",
            "Software",
            "author/assignedAuthor/assignedAuthoringDevice/softwareName | author/assignedAuthor/assignedAuthoringDevice/manufacturerModelName",
        ),
        F(
            "Author",
            "Organization",
            "author/assignedAuthor/representedOrganization/name",
        ),
        F(
            "Custodian",
            "Organization",
            "custodian/assignedCustodian/representedCustodianOrganization/name",
        ),
        F(
            "Custodian",
            "Organization ID",
            "custodian/assignedCustodian/representedCustodianOrganization/id/@extension | custodian/assignedCustodian/representedCustodianOrganization/id/@root",
        ),
        F(
            "Encounter",
            "Period start",
            "componentOf/encompassingEncounter/effectiveTime/low/@value | componentOf/encompassingEncounter/effectiveTime/@value",
        ),
        F(
            "Encounter",
            "Period end",
            "componentOf/encompassingEncounter/effectiveTime/high/@value",
        ),
        F("Body", "Sections", "count:**/section"),
        F("Body", "Structured entries", "count:**/section/entry"),
    ],
    tables: &[],
    custom: Some(custom),
};

fn custom(doc: &Doc, out: &mut Inspection) {
    // Document type from the templates.
    let templates = template_ids(doc, 0);
    let name = templates
        .iter()
        .filter_map(|(r, _)| template_name(DOCUMENT_TEMPLATES, r))
        .min_by_key(|n| n.starts_with("US Realm Header"));
    out.kind = match name {
        Some(n) => format!("C-CDA {n}"),
        None => "CDA clinical document".into(),
    };
    let listed: Vec<String> = templates
        .iter()
        .map(|(r, e)| {
            let n = template_name(DOCUMENT_TEMPLATES, r).unwrap_or("");
            join(
                &[
                    if e.is_empty() {
                        r.clone()
                    } else {
                        format!("{r}:{e}")
                    },
                    if n.is_empty() {
                        String::new()
                    } else {
                        format!("({n})")
                    },
                ],
                " ",
            )
        })
        .collect();
    out.fact("Document", "Templates", listed.join("; "), None);
    // Patient identifiers (all of them).
    let ids: Vec<String> = select(doc, 0, "recordTarget/patientRole/id")
        .into_iter()
        .take(20)
        .map(|i| {
            let (ext, root) = (
                doc.attr(i, "extension").unwrap_or(""),
                doc.attr(i, "root").unwrap_or(""),
            );
            if ext.is_empty() {
                root.to_owned()
            } else {
                format!("{ext} ({root})")
            }
        })
        .collect();
    out.fact("Patient", "Patient IDs", ids.join("; "), None);

    // Sections, nested through component/section, and their entries.
    let mut entries = Rows::new(
        "Entries",
        &[
            "Section",
            "Entry type",
            "Code",
            "Value",
            "Time",
            "Status",
            "Within",
        ],
    );
    let body = select(doc, 0, "component/structuredBody");
    if body.is_empty()
        && let Some(&nb) = select(doc, 0, "component/nonXMLBody").first()
    {
        let media = value_at(doc, nb, "text/@mediaType").map(|v| v.0);
        outline(
            out,
            0,
            "Non-XML body",
            format!(
                "{} (embedded content is not decoded)",
                media.unwrap_or_else(|| "unspecified media type".into())
            ),
            Some(doc.get(nb).span.clone()),
        );
    }
    let mut stack: Vec<(u32, u16)> = body
        .iter()
        .flat_map(|&b| select(doc, b, "component/section"))
        .rev()
        .map(|s| (s, 0))
        .collect();
    let mut visited = 0;
    while let Some((sec, depth)) = stack.pop() {
        visited += 1;
        if visited > 10_000 {
            break;
        }
        let title = value_at(doc, sec, "title").map(|v| v.0).unwrap_or_default();
        let code_el = select(doc, sec, "code").first().copied();
        let tids = template_ids(doc, sec);
        let known = tids
            .iter()
            .find_map(|(r, _)| template_name(SECTION_TEMPLATES, r));
        let label = if title.is_empty() {
            known.unwrap_or("Section").to_owned()
        } else {
            title.clone()
        };
        let section_entries = select(doc, sec, "entry");
        let value = join(
            &[
                code_el.map(|c| code(doc, c)).unwrap_or_default(),
                match known {
                    Some(k) => format!("template: {k}"),
                    None => tids
                        .first()
                        .map(|(r, _)| format!("template {r}"))
                        .unwrap_or_default(),
                },
                format!("{} structured entries", section_entries.len()),
            ],
            " · ",
        );
        outline(
            out,
            depth,
            label.clone(),
            value,
            Some(doc.get(sec).span.clone()),
        );
        if let Some(&text) = select(doc, sec, "text").first() {
            let narrative = flatten(doc, text, 500);
            if !narrative.is_empty() {
                outline(
                    out,
                    depth + 1,
                    "Narrative",
                    narrative,
                    Some(doc.get(text).span.clone()),
                );
            }
        }
        for entry in section_entries {
            for &st in &doc.get(entry).children {
                statement(doc, st, &label, "", &mut entries, 0);
            }
        }
        for sub in select(doc, sec, "component/section").into_iter().rev() {
            stack.push((sub, depth + 1));
        }
    }
    entries.finish(out);
    out.notes.push(
        "Narrative text is shown flattened (first 500 characters per section); codes are not looked up in their code systems".into(),
    );
}

/// A clinical statement row, and the statements nested in it (entryRelationship, component).
fn statement(doc: &Doc, st: u32, section: &str, within: &str, rows: &mut Rows, depth: usize) {
    let kind = doc.get(st).name.as_str();
    if !STATEMENTS.contains(&kind) || depth > 6 {
        return;
    }
    let code_text = select(doc, st, "code")
        .first()
        .map(|&c| code(doc, c))
        .unwrap_or_default();
    let mut value_text = select(doc, st, "value")
        .first()
        .map(|&v| value(doc, v))
        .unwrap_or_default();
    if value_text.is_empty() && kind == "substanceAdministration" {
        value_text = select(
            doc,
            st,
            "consumable/manufacturedProduct/manufacturedMaterial/code",
        )
        .first()
        .map(|&c| code(doc, c))
        .unwrap_or_default();
        let dose = value_at(doc, st, "doseQuantity/@value").map(|v| v.0);
        if let Some(d) = dose {
            let unit = value_at(doc, st, "doseQuantity/@unit")
                .map(|v| v.0)
                .unwrap_or_default();
            value_text = join(
                &[value_text, format!("dose {d} {unit}").trim().into()],
                ", ",
            );
        }
    }
    rows.push(
        vec![
            section.to_owned(),
            kind.to_owned(),
            code_text,
            value_text,
            time(doc, st),
            value_at(doc, st, "statusCode/@code")
                .map(|v| v.0)
                .unwrap_or_default(),
            within.to_owned(),
        ],
        Some(doc.get(st).span.clone()),
    );
    let nested = [
        select(doc, st, "entryRelationship"),
        select(doc, st, "component"),
    ];
    for rel in nested.into_iter().flatten() {
        for &child in &doc.get(rel).children {
            statement(doc, child, section, kind, rows, depth + 1);
        }
    }
}
