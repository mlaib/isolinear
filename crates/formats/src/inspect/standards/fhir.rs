//! HL7 FHIR resources, in JSON (`resourceType`) or XML (namespace `http://hl7.org/fhir`). The
//! XML form is read as its JSON equivalent, so both show the same facts and tables.
use super::records::{Rows, XmlView, all, clip, join, nodes, s, span, str_after, xml_to_node};
use crate::inspect::{
    Inspection, Stage, State,
    jsondoc::{Node, Value},
    spec::{JsonSpec, XmlSpec},
};
use std::collections::HashSet;

pub const NS: &str = "http://hl7.org/fhir";

pub static FHIR_JSON: JsonSpec = JsonSpec {
    family: "HL7 FHIR",
    kind: "FHIR resource",
    matches: |keys, head| {
        keys.iter().any(|k| k == "resourceType")
            && str_after(head, "resourceType").is_some_and(|v| {
                v.starts_with(|c: char| c.is_ascii_uppercase())
                    && v.chars().all(|c| c.is_ascii_alphanumeric())
            })
    },
    version: |_| None,
    facts: &[],
    tables: &[],
    custom: Some(resource),
};

pub static FHIR_XML: XmlSpec = XmlSpec {
    family: "HL7 FHIR",
    kind: "FHIR resource",
    matches: |root, ns| ns == NS && root.starts_with(|c: char| c.is_ascii_uppercase()),
    version: |_| None,
    facts: &[],
    tables: &[],
    custom: Some(|doc, out| resource(&xml_to_node(doc, 0, XmlView::Fhir), out)),
};

type Field = fn(&Node) -> String;

/// A coded value: `display (system|code)`, for a Coding or each Coding of a CodeableConcept.
pub fn coded(n: &Node) -> String {
    let one = |c: &Node| {
        let (display, system, code) = (s(c, "display"), s(c, "system"), s(c, "code"));
        let sc = match (system.is_empty(), code.is_empty()) {
            (true, true) => String::new(),
            (true, false) => code,
            (false, true) => system,
            (false, false) => format!("{system}|{code}"),
        };
        match (display.is_empty(), sc.is_empty()) {
            (false, false) => format!("{display} ({sc})"),
            (false, true) => display,
            _ => sc,
        }
    };
    if n.get("coding").is_some() {
        let codings: Vec<String> = nodes(n, "coding").into_iter().map(one).collect();
        let text = s(n, "text");
        let joined = join(&codings, "; ");
        return match (text.is_empty(), joined.is_empty()) {
            (_, true) => text,
            (true, false) => joined,
            (false, false) if codings.iter().any(|c| c.starts_with(&text)) => joined,
            (false, false) => format!("{text}: {joined}"),
        };
    }
    if n.get("code").is_some() || n.get("system").is_some() || n.get("display").is_some() {
        return one(n);
    }
    // R5 CodeableReference.
    if let Some(c) = n.get("concept") {
        return coded(c);
    }
    if let Some(r) = n.get("reference") {
        return r.text();
    }
    match &n.value {
        Value::Object(_) => s(n, "text"),
        _ => n.text(),
    }
}

/// Every coded value at `path`, joined.
fn cc(r: &Node, path: &str) -> String {
    let v: Vec<String> = nodes(r, path).into_iter().map(coded).collect();
    join(&v, "; ")
}

fn quantity(q: &Node) -> String {
    if !matches!(q.value, Value::Object(_)) {
        return q.text();
    }
    let unit = s(q, "unit | code | currency");
    let v = format!("{}{}", s(q, "comparator"), s(q, "value"));
    join(&[v, unit], " ")
}

fn period(p: &Node) -> String {
    let (a, b) = (s(p, "start"), s(p, "end"));
    match (a.is_empty(), b.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("{a} –"),
        (true, false) => format!("– {b}"),
        _ => format!("{a} – {b}"),
    }
}

/// A choice element (`value[x]`, `effective[x]`, `onset[x]`) as text.
fn choice(r: &Node, prefix: &str) -> String {
    let Value::Object(fields) = &r.value else {
        return String::new();
    };
    for (k, v) in fields {
        let Some(kind) = k.strip_prefix(prefix) else {
            continue;
        };
        if !kind.starts_with(|c: char| c.is_ascii_uppercase()) {
            continue;
        }
        return match kind {
            "Quantity" | "Age" | "Duration" | "Distance" | "Count" | "Money" | "SimpleQuantity" => {
                quantity(v)
            }
            "CodeableConcept" | "Coding" | "CodeableReference" => coded(v),
            "Period" => period(v),
            "Range" => {
                let (lo, hi) = (
                    v.get("low").map(quantity).unwrap_or_default(),
                    v.get("high").map(quantity).unwrap_or_default(),
                );
                format!("{lo} – {hi}")
            }
            "Ratio" => format!(
                "{} / {}",
                v.get("numerator").map(quantity).unwrap_or_default(),
                v.get("denominator").map(quantity).unwrap_or_default()
            ),
            "SampledData" => format!("sampled data ({} period)", s(v, "period")),
            "Reference" => reference_text(v),
            "Attachment" => join(&[s(v, "title"), s(v, "contentType")], " "),
            _ => v.text(),
        };
    }
    String::new()
}

fn reference_text(r: &Node) -> String {
    let (reference, display) = (s(r, "reference"), s(r, "display"));
    match (reference.is_empty(), display.is_empty()) {
        (false, false) => format!("{reference} ({display})"),
        (false, true) => reference,
        _ => display,
    }
}

fn refs(r: &Node, path: &str) -> String {
    let v: Vec<String> = nodes(r, path).into_iter().map(reference_text).collect();
    join(&v, "; ")
}

fn human_names(r: &Node) -> String {
    let names: Vec<String> = nodes(r, "name")
        .into_iter()
        .map(|n| {
            let text = s(n, "text");
            if !text.is_empty() {
                return text;
            }
            join(
                &[
                    all(n, "prefix", " "),
                    all(n, "given", " "),
                    s(n, "family"),
                    all(n, "suffix", " "),
                ],
                " ",
            )
        })
        .collect();
    join(&names, "; ")
}

fn identifiers(r: &Node) -> String {
    let ids: Vec<String> = nodes(r, "identifier")
        .into_iter()
        .take(20)
        .map(|i| {
            let (value, system) = (s(i, "value"), s(i, "system"));
            let kind = i.get("type").map(coded).unwrap_or_default();
            let mut t = value;
            if !system.is_empty() {
                t = format!("{t} ({system})");
            }
            if !kind.is_empty() {
                t = format!("{kind}: {t}");
            }
            t
        })
        .collect();
    join(&ids, "; ")
}

fn addresses(r: &Node) -> String {
    let a: Vec<String> = nodes(r, "address")
        .into_iter()
        .map(|a| {
            let text = s(a, "text");
            if !text.is_empty() {
                return text;
            }
            join(
                &[
                    all(a, "line", ", "),
                    s(a, "postalCode"),
                    s(a, "city"),
                    s(a, "state"),
                    s(a, "country"),
                ],
                " ",
            )
        })
        .collect();
    join(&a, "; ")
}

fn telecom(r: &Node) -> String {
    let t: Vec<String> = nodes(r, "telecom")
        .into_iter()
        .map(|t| join(&[s(t, "system"), s(t, "value")], ": "))
        .collect();
    join(&t, "; ")
}

fn observation_value(r: &Node) -> String {
    let v = choice(r, "value");
    if !v.is_empty() {
        return v;
    }
    let comps: Vec<String> = nodes(r, "component")
        .into_iter()
        .take(6)
        .map(|c| {
            let code = c
                .get("code")
                .map(|c| {
                    let t = s(c, "text");
                    if t.is_empty() {
                        s(c, "coding/display | coding/code")
                    } else {
                        t
                    }
                })
                .unwrap_or_default();
            format!("{code} {}", choice(c, "value"))
        })
        .collect();
    if !comps.is_empty() {
        return join(&comps, "; ");
    }
    r.get("dataAbsentReason")
        .map(|d| format!("absent: {}", coded(d)))
        .unwrap_or_default()
}

const PATIENT: &[(&str, Field)] = &[
    ("Patient name", human_names),
    ("Patient gender", |r| s(r, "gender")),
    ("Patient birth date", |r| s(r, "birthDate")),
    ("Patient identifiers", identifiers),
    ("Patient address", addresses),
    ("Patient telecom", telecom),
    ("Patient deceased", |r| choice(r, "deceased")),
    ("Patient active", |r| s(r, "active")),
    ("Managing organization", |r| refs(r, "managingOrganization")),
];
const OBSERVATION: &[(&str, Field)] = &[
    ("Code", |r| cc(r, "code")),
    ("Value", observation_value),
    ("Effective", |r| choice(r, "effective")),
    ("Status", |r| s(r, "status")),
    ("Interpretation", |r| cc(r, "interpretation")),
    ("Category", |r| cc(r, "category")),
    ("Reference range", |r| {
        nodes(r, "referenceRange")
            .first()
            .map(|rr| {
                let lo = rr.get("low").map(quantity).unwrap_or_default();
                let hi = rr.get("high").map(quantity).unwrap_or_default();
                let t = s(rr, "text");
                if lo.is_empty() && hi.is_empty() {
                    t
                } else {
                    format!("{lo} – {hi}")
                }
            })
            .unwrap_or_default()
    }),
    ("Subject", |r| refs(r, "subject")),
];
const ENCOUNTER: &[(&str, Field)] = &[
    ("Status", |r| s(r, "status")),
    ("Class", |r| cc(r, "class")),
    ("Type", |r| cc(r, "type")),
    ("Period", |r| {
        r.get("period")
            .or(r.get("actualPeriod"))
            .map(period)
            .unwrap_or_default()
    }),
    ("Reason", |r| cc(r, "reasonCode | reason/value")),
    ("Service provider", |r| refs(r, "serviceProvider")),
    ("Subject", |r| refs(r, "subject")),
];
const CONDITION: &[(&str, Field)] = &[
    ("Code", |r| cc(r, "code")),
    ("Clinical status", |r| cc(r, "clinicalStatus")),
    ("Verification status", |r| cc(r, "verificationStatus")),
    ("Category", |r| cc(r, "category")),
    ("Severity", |r| cc(r, "severity")),
    ("Onset", |r| choice(r, "onset")),
    ("Abatement", |r| choice(r, "abatement")),
    ("Recorded", |r| s(r, "recordedDate")),
    ("Subject", |r| refs(r, "subject")),
];
const MEDICATION_REQUEST: &[(&str, Field)] = &[
    ("Medication", |r| {
        let v = choice(r, "medication");
        if v.is_empty() {
            r.get("medication").map(coded).unwrap_or_default()
        } else {
            v
        }
    }),
    ("Status", |r| s(r, "status")),
    ("Intent", |r| s(r, "intent")),
    ("Authored on", |r| s(r, "authoredOn")),
    ("Dosage", |r| all(r, "dosageInstruction/text", "; ")),
    ("Requester", |r| refs(r, "requester")),
    ("Subject", |r| refs(r, "subject")),
];
const DIAGNOSTIC_REPORT: &[(&str, Field)] = &[
    ("Code", |r| cc(r, "code")),
    ("Status", |r| s(r, "status")),
    ("Category", |r| cc(r, "category")),
    ("Effective", |r| choice(r, "effective")),
    ("Issued", |r| s(r, "issued")),
    ("Results", |r| {
        let n = nodes(r, "result").len();
        if n == 0 { String::new() } else { n.to_string() }
    }),
    ("Conclusion", |r| clip(&s(r, "conclusion"), 200)),
    ("Performer", |r| refs(r, "performer")),
    ("Subject", |r| refs(r, "subject")),
];
const PROCEDURE: &[(&str, Field)] = &[
    ("Code", |r| cc(r, "code")),
    ("Status", |r| s(r, "status")),
    ("Performed", |r| {
        let v = choice(r, "performed");
        if v.is_empty() {
            choice(r, "occurrence")
        } else {
            v
        }
    }),
    ("Body site", |r| cc(r, "bodySite")),
    ("Reason", |r| cc(r, "reasonCode | reason/concept")),
    ("Outcome", |r| cc(r, "outcome")),
    ("Subject", |r| refs(r, "subject")),
];
const OTHER: &[(&str, Field)] = &[
    ("Status", |r| s(r, "status")),
    ("Code", |r| cc(r, "code")),
    ("Name", |r| match r.get("name").map(|n| &n.value) {
        Some(Value::String(n)) => n.clone(),
        _ => human_names(r),
    }),
    ("Title", |r| s(r, "title")),
    ("URL", |r| s(r, "url")),
    ("Subject", |r| refs(r, "subject | patient")),
];

/// Fields of a resource type, and the heading for a table of them in a Bundle.
fn fields(rt: &str) -> (&'static [(&'static str, Field)], &'static str) {
    match rt {
        "Patient" => (PATIENT, "Patients"),
        "Observation" => (OBSERVATION, "Observations"),
        "Encounter" => (ENCOUNTER, "Encounters"),
        "Condition" => (CONDITION, "Conditions"),
        "MedicationRequest" => (MEDICATION_REQUEST, "Medication requests"),
        "DiagnosticReport" => (DIAGNOSTIC_REPORT, "Diagnostic reports"),
        "Procedure" => (PROCEDURE, "Procedures"),
        _ => (OTHER, ""),
    }
}

/// One line about a resource, without personal values (a Patient's name and birth date stay
/// in the Patients table, whose columns can be masked).
fn summary(r: &Node) -> String {
    let rt = s(r, "resourceType");
    if rt == "Patient" {
        let n = nodes(r, "identifier").len();
        return join(
            &[
                s(r, "gender"),
                if n > 0 {
                    format!("{n} identifier(s)")
                } else {
                    String::new()
                },
            ],
            ", ",
        );
    }
    let (list, _) = fields(&rt);
    let parts: Vec<String> = list
        .iter()
        .filter(|(label, _)| *label != "Subject")
        .map(|(_, f)| clip(&f(r), 80))
        .filter(|v| !v.is_empty())
        .take(3)
        .collect();
    join(&parts, " · ")
}

fn release(v: &str) -> &'static str {
    let mm: String = v.split('.').take(2).collect::<Vec<_>>().join(".");
    match mm.as_str() {
        "1.0" => "DSTU2",
        "1.4" | "1.6" | "1.8" => "STU3 draft",
        "3.0" => "STU3",
        "3.3" | "3.5" => "R4 draft",
        "4.0" => "R4",
        "4.1" | "4.2" => "R4B draft",
        "4.3" => "R4B",
        "4.4" | "4.5" | "4.6" => "R5 draft",
        "5.0" => "R5",
        "6.0" => "R6",
        _ => "unknown release",
    }
}

/// The FHIR version: stated (`fhirVersion`), or inferred from a profile's canonical URL.
fn fhir_version(root: &Node) -> Result<String, String> {
    let stated = s(root, "fhirVersion");
    if !stated.is_empty() {
        return Ok(format!("{stated} ({})", release(&stated)));
    }
    let mut profiles = nodes(root, "meta/profile");
    profiles.extend(nodes(root, "entry/resource/meta/profile"));
    for p in profiles.iter().take(1000) {
        let p = p.text();
        for (marker, rel) in [
            ("/fhir/R5/", "R5"),
            ("/fhir/5.0/", "R5"),
            ("/fhir/R4B/", "R4B"),
            ("/fhir/4.3/", "R4B"),
            ("/fhir/R4/", "R4"),
            ("/fhir/4.0/", "R4"),
            ("/fhir/STU3/", "STU3"),
            ("/fhir/3.0/", "STU3"),
            ("/fhir/DSTU2/", "DSTU2"),
            ("/fhir/us/core/", "R4 (US Core profile)"),
            ("/fhir/uv/ips/", "R4 (IPS profile)"),
        ] {
            if p.contains(marker) {
                return Ok(format!("{rel}, inferred from the profile {p}"));
            }
        }
    }
    Err(
        "The FHIR version is not stated in the file (no fhirVersion, no versioned profile); \
         it is shown as unknown"
            .into(),
    )
}

fn describe(rt: &str) -> &'static str {
    match rt {
        "Bundle" => "a collection of resources",
        "Patient" => "demographics of a patient",
        "Observation" => "a measurement or assertion",
        "Encounter" => "an interaction with a patient",
        "Condition" => "a problem or diagnosis",
        "MedicationRequest" => "a medication order",
        "DiagnosticReport" => "results of diagnostic investigations",
        "Procedure" => "an action performed on a patient",
        "CapabilityStatement" => "what a FHIR server or client supports",
        "OperationOutcome" => "errors, warnings and information from an operation",
        "Practitioner" => "a health care professional",
        "Organization" => "an organization",
        "AllergyIntolerance" => "an allergy or intolerance",
        "Immunization" => "a vaccination",
        _ => "FHIR resource",
    }
}

pub fn resource(root: &Node, out: &mut Inspection) {
    let rt = s(root, "resourceType");
    out.kind = format!("{rt} — {}", describe(&rt));
    match fhir_version(root) {
        Ok(v) => out.version = Some(v),
        Err(note) => {
            out.version = Some("unknown".into());
            out.notes.push(note);
        }
    }
    let src = span(root);
    out.fact("Resource", "Type", rt.clone(), src.clone());
    for (label, path) in [
        ("ID", "id"),
        ("Version ID", "meta/versionId"),
        ("Last updated", "meta/lastUpdated"),
        ("Source", "meta/source"),
        ("Language", "language"),
        ("Narrative status", "text/status"),
    ] {
        if let Some(n) = nodes(root, path).first() {
            out.fact("Resource", label, n.text(), span(n));
        }
    }
    out.fact(
        "Resource",
        "Profiles",
        all(root, "meta/profile", ", "),
        None,
    );
    out.fact(
        "Resource",
        "Security labels",
        cc(root, "meta/security"),
        None,
    );
    out.fact("Resource", "Tags", cc(root, "meta/tag"), None);

    let mut in_bundle = false;
    match rt.as_str() {
        "Bundle" => {
            bundle(root, out);
            in_bundle = true;
        }
        "CapabilityStatement" => capability(root, out),
        "OperationOutcome" => operation_outcome(root, out),
        _ => {
            let (list, _) = fields(&rt);
            let group = if list.as_ptr() == OTHER.as_ptr() {
                "Details"
            } else {
                rt.as_str()
            };
            for (label, f) in list {
                out.fact(group, label, f(root), src.clone());
            }
        }
    }
    references(root, out, in_bundle);
    out.notes.push(
        "Coded values are shown as display (system|code); terminologies are not looked up".into(),
    );
}

fn bundle(root: &Node, out: &mut Inspection) {
    let entries = nodes(root, "entry");
    for (label, path) in [
        ("Bundle type", "type"),
        ("Total (declared)", "total"),
        ("Timestamp", "timestamp"),
        ("Identifier", "identifier/value"),
    ] {
        if let Some(n) = nodes(root, path).first() {
            out.fact("Bundle", label, n.text(), span(n));
        }
    }
    out.fact("Bundle", "Entries", entries.len().to_string(), span(root));
    let mut table = Rows::new(
        "Entries",
        &[
            "#",
            "Full URL",
            "Resource type",
            "ID",
            "Summary",
            "Request",
            "Response status",
        ],
    );
    let mut by_type: Vec<(String, Rows)> = vec![];
    let mut counts: Vec<(String, usize)> = vec![];
    for (i, e) in entries.iter().enumerate() {
        let r = e.get("resource");
        let rt = r.map(|r| s(r, "resourceType")).unwrap_or_default();
        table.push(
            vec![
                (i + 1).to_string(),
                s(e, "fullUrl"),
                rt.clone(),
                r.map(|r| s(r, "id")).unwrap_or_default(),
                r.map(summary).unwrap_or_default(),
                join(&[s(e, "request/method"), s(e, "request/url")], " "),
                s(e, "response/status"),
            ],
            span(e),
        );
        match counts.iter_mut().find(|(t, _)| *t == rt) {
            Some((_, n)) => *n += 1,
            None => counts.push((rt.clone(), 1)),
        }
        let (list, heading) = fields(&rt);
        let Some(r) = r.filter(|_| !heading.is_empty()) else {
            continue;
        };
        if !by_type.iter().any(|(t, _)| *t == rt) {
            let mut cols = vec!["ID"];
            cols.extend(list.iter().map(|(l, _)| *l));
            by_type.push((rt.clone(), Rows::new(heading, &cols)));
        }
        if let Some((_, rows)) = by_type.iter_mut().find(|(t, _)| *t == rt) {
            let mut cells = vec![s(r, "id")];
            cells.extend(list.iter().map(|(_, f)| f(r)));
            rows.push(cells, span(r));
        }
    }
    for (t, n) in counts.iter().filter(|(t, _)| !t.is_empty()) {
        out.fact("Resources in the Bundle", t, n.to_string(), None);
    }
    table.finish(out);
    for (_, rows) in by_type {
        rows.finish(out);
    }
    let declared = s(root, "total");
    if !declared.is_empty() && s(root, "type") == "searchset" {
        out.notes.push(format!(
            "The search matched {declared} resources; this page holds {} entries",
            entries.len()
        ));
    }
}

fn capability(root: &Node, out: &mut Inspection) {
    for (label, path) in [
        ("Status", "status"),
        ("Date", "date"),
        ("Publisher", "publisher"),
        ("Kind", "kind"),
        ("FHIR version", "fhirVersion"),
        ("Software", "software/name"),
        ("Software version", "software/version"),
        ("Implementation", "implementation/description"),
        ("Endpoint (not contacted)", "implementation/url"),
    ] {
        out.fact("Capabilities", label, s(root, path), None);
    }
    out.fact("Capabilities", "Formats", all(root, "format", ", "), None);
    let mut rows = Rows::new(
        "Supported resources",
        &[
            "Mode",
            "Resource",
            "Interactions",
            "Search parameters",
            "Profile",
        ],
    );
    for rest in nodes(root, "rest") {
        for r in nodes(rest, "resource") {
            rows.push(
                vec![
                    s(rest, "mode"),
                    s(r, "type"),
                    all(r, "interaction/code", ", "),
                    all(r, "searchParam/name", ", "),
                    s(r, "profile"),
                ],
                span(r),
            );
        }
    }
    rows.finish(out);
}

fn operation_outcome(root: &Node, out: &mut Inspection) {
    let mut rows = Rows::new(
        "Issues",
        &["Severity", "Code", "Details", "Diagnostics", "Location"],
    );
    for i in nodes(root, "issue") {
        rows.push(
            vec![
                s(i, "severity"),
                s(i, "code"),
                i.get("details").map(coded).unwrap_or_default(),
                s(i, "diagnostics"),
                all(i, "expression | location", ", "),
            ],
            span(i),
        );
    }
    rows.finish(out);
}

/// What a reference points at, for the check.
#[derive(PartialEq)]
enum Target {
    Resolved,
    Unresolved,
    /// Outside the file (an absolute URL, a conditional reference): not checked.
    NotChecked,
}

struct Index {
    full_urls: HashSet<String>,
    type_ids: HashSet<String>,
    bases: HashSet<String>,
}

/// "http://x/fhir/Patient/1/_history/2" -> ("http://x/fhir/", "Patient/1").
fn split_reference(r: &str) -> (String, String) {
    let r = r.split("/_history/").next().unwrap_or(r);
    let parts: Vec<&str> = r.rsplitn(3, '/').collect();
    if parts.len() >= 2 && parts[1].starts_with(|c: char| c.is_ascii_uppercase()) {
        let base = if parts.len() == 3 {
            format!("{}/", parts[2])
        } else {
            String::new()
        };
        (base, format!("{}/{}", parts[1], parts[0]))
    } else {
        (String::new(), r.to_owned())
    }
}

fn resolve(reference: &str, contained: &HashSet<String>, index: Option<&Index>) -> Target {
    if let Some(local) = reference.strip_prefix('#') {
        return if local.is_empty() || contained.contains(local) {
            Target::Resolved
        } else {
            Target::Unresolved
        };
    }
    let Some(index) = index else {
        return Target::NotChecked;
    };
    if index.full_urls.contains(reference) {
        return Target::Resolved;
    }
    if reference.contains('?') {
        return Target::NotChecked;
    }
    if reference.starts_with("urn:") {
        return Target::Unresolved;
    }
    let (base, type_id) = split_reference(reference);
    let absolute = reference.contains("://");
    if absolute {
        return if index.bases.contains(&base) && index.type_ids.contains(&type_id) {
            Target::Resolved
        } else {
            Target::NotChecked
        };
    }
    if index.type_ids.contains(&type_id) {
        Target::Resolved
    } else {
        Target::Unresolved
    }
}

/// Collect `reference` strings under `n` (not into nested Bundle entries' resources).
fn collect_refs<'a>(
    n: &'a Node,
    path: &mut Vec<String>,
    found: &mut Vec<(String, &'a Node, String)>,
    depth: usize,
) {
    if depth > 64 || found.len() >= 100_000 {
        return;
    }
    match &n.value {
        Value::Object(fields) => {
            for (k, v) in fields {
                if k == "reference" {
                    if let Value::String(r) = &v.value {
                        found.push((path.join("."), n, r.clone()));
                    }
                    continue;
                }
                if k == "resource" && depth == 1 {
                    continue;
                }
                path.push(k.clone());
                collect_refs(v, path, found, depth + 1);
                path.pop();
            }
        }
        Value::Array(items) => {
            for i in items {
                collect_refs(i, path, found, depth + 1);
            }
        }
        _ => {}
    }
}

fn references(root: &Node, out: &mut Inspection, in_bundle: bool) {
    let mut resources: Vec<(String, &Node)> = vec![];
    let index = if in_bundle {
        let mut index = Index {
            full_urls: HashSet::new(),
            type_ids: HashSet::new(),
            bases: HashSet::new(),
        };
        for e in nodes(root, "entry") {
            let full = s(e, "fullUrl");
            let Some(r) = e.get("resource") else {
                continue;
            };
            let type_id = format!("{}/{}", s(r, "resourceType"), s(r, "id"));
            if !full.is_empty() {
                index.full_urls.insert(full.clone());
                let (base, _) = split_reference(&full);
                index.bases.insert(base);
            }
            index.type_ids.insert(type_id.clone());
            resources.push((
                if full.is_empty() || !full.contains("://") {
                    type_id
                } else {
                    split_reference(&full).1
                },
                r,
            ));
        }
        Some(index)
    } else {
        resources.push((
            format!("{}/{}", s(root, "resourceType"), s(root, "id")),
            root,
        ));
        None
    };
    let mut table = Rows::new(
        "References",
        &[
            "From",
            "Element",
            "Reference",
            "Display",
            "Resolves in this file",
        ],
    );
    let (mut ok, mut bad, mut unchecked) = (0u64, 0u64, 0u64);
    let mut bad_examples: Vec<String> = vec![];
    for (from, r) in &resources {
        let contained: HashSet<String> = nodes(r, "contained/id")
            .into_iter()
            .map(|n| n.text())
            .collect();
        let mut found = vec![];
        collect_refs(r, &mut vec![], &mut found, 1);
        for (path, holder, reference) in found {
            let target = resolve(&reference, &contained, index.as_ref());
            let state = match target {
                Target::Resolved => {
                    ok += 1;
                    "yes"
                }
                Target::Unresolved => {
                    bad += 1;
                    if bad_examples.len() < 5 {
                        bad_examples.push(reference.clone());
                    }
                    "no"
                }
                Target::NotChecked => {
                    unchecked += 1;
                    "not checked (outside the file)"
                }
            };
            table.push(
                vec![
                    from.clone(),
                    path,
                    reference,
                    s(holder, "display"),
                    state.into(),
                ],
                span(holder),
            );
        }
    }
    let total = table.seen();
    table.finish(out);
    if total == 0 {
        return;
    }
    out.fact("References", "References", total.to_string(), None);
    let detail = format!(
        "References{}: {ok} resolve, {bad} do not{}; {unchecked} outside the file not checked",
        if in_bundle { " within the Bundle" } else { "" },
        if bad_examples.is_empty() {
            String::new()
        } else {
            format!(" ({})", bad_examples.join(", "))
        }
    );
    let state = if bad > 0 {
        State::Failed
    } else if ok > 0 {
        State::Passed
    } else {
        State::NotDone
    };
    out.check(Stage::Semantic, state, detail);
}
