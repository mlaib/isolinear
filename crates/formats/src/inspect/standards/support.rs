//! Small helpers shared by the business-document specs: decimal sums over paths, tables over
//! element lists the path language cannot select in one go, namespace prefixes, and the names
//! of common e-invoicing guidelines.
use crate::inspect::{
    Inspection, MAX_ROWS, Row, Stage, State, Table,
    spec::{decimal_sum, select, value_at},
    xmldoc::Doc,
};

/// The value at `path` from `from`, without its span.
pub fn value(doc: &Doc, from: u32, path: &str) -> Option<String> {
    value_at(doc, from, path).map(|v| v.0)
}

/// The exact sum of the values at `path`; `None` when nothing matches or one is not a number.
pub fn sum_at(doc: &Doc, from: u32, path: &str) -> Option<String> {
    let values: Vec<String> = select(doc, from, path)
        .iter()
        .map(|&e| doc.text(e))
        .collect();
    if values.is_empty() {
        return None;
    }
    decimal_sum(values.iter().map(String::as_str))
}

/// `-x` for a decimal written as text.
pub fn negate(s: &str) -> String {
    let s = s.trim();
    match s.strip_prefix('-') {
        Some(rest) => rest.to_owned(),
        None => format!("-{}", s.trim_start_matches('+')),
    }
}

/// `a + b - c ...`: terms with a sign, `None` when a term is missing or not a number.
pub fn signed_sum(terms: &[(bool, Option<String>)]) -> Option<String> {
    let mut values = vec![];
    for (plus, v) in terms {
        let v = v.as_ref()?;
        values.push(if *plus { v.clone() } else { negate(v) });
    }
    decimal_sum(values.iter().map(String::as_str))
}

/// A table over the given rows, with columns as paths from each row (as in a spec's tables).
pub fn table(doc: &Doc, name: &str, rows: &[u32], columns: &[(&str, &str)]) -> Option<Table> {
    let mut t = Table {
        name: name.into(),
        columns: columns.iter().map(|(h, _)| (*h).to_owned()).collect(),
        rows: vec![],
        total: (rows.len() > MAX_ROWS).then_some(rows.len() as u64),
    };
    for &r in rows.iter().take(MAX_ROWS) {
        t.rows.push(Row {
            cells: columns
                .iter()
                .map(|(_, p)| value(doc, r, p).unwrap_or_default())
                .collect(),
            source: Some(doc.get(r).span.clone()),
        });
    }
    (!t.rows.is_empty()).then_some(t)
}

/// Rows selected by several paths, in document order without duplicates.
pub fn select_any(doc: &Doc, from: u32, paths: &[&str]) -> Vec<u32> {
    let mut out: Vec<u32> = paths.iter().flat_map(|p| select(doc, from, p)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Elements anywhere with this local name whose namespace starts with `ns`.
pub fn find_all(doc: &Doc, name: &str, ns: &str) -> Vec<u32> {
    doc.elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.name == name && e.ns.starts_with(ns))
        .map(|(i, _)| i as u32)
        .take(MAX_ROWS)
        .collect()
}

/// The prefix bound to the element's namespace, from the `xmlns:` declarations of it and its
/// ancestors (attributes are kept by local name, so `xmlns:us-gaap` is `us-gaap`).
pub fn prefix_of(doc: &Doc, e: u32) -> Option<String> {
    let ns = &doc.get(e).ns;
    if ns.is_empty() {
        return None;
    }
    let mut at = Some(e);
    let mut steps = 0;
    while let Some(i) = at {
        let el = doc.get(i);
        if let Some((k, _)) = el.attrs.iter().find(|(k, v)| v == ns && k != "xmlns") {
            return Some(k.clone());
        }
        if el.attrs.iter().any(|(k, v)| k == "xmlns" && v == ns) {
            return None;
        }
        at = el.parent;
        steps += 1;
        if steps > 300 {
            break;
        }
    }
    None
}

/// `prefix:name` of the element, or its local name.
pub fn qualified(doc: &Doc, e: u32) -> String {
    match prefix_of(doc, e) {
        Some(p) => format!("{p}:{}", doc.get(e).name),
        None => doc.get(e).name.clone(),
    }
}

/// At most `n` characters, with an ellipsis when cut.
pub fn short(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// Compare a declared amount with one computed from other declared amounts.
pub fn compare(
    out: &mut Inspection,
    what: &str,
    declared: Option<String>,
    computed: Option<String>,
) {
    if declared.is_some() && computed.is_none() {
        out.check(
            Stage::Semantic,
            State::NotDone,
            format!("{what}: not every amount needed is present"),
        );
        return;
    }
    crate::inspect::spec::compare_total(out, what, declared, computed);
}

/// The readable name of an EN 16931 customization (UBL `CustomizationID`, CII guideline ID).
pub fn guideline(id: &str) -> Option<String> {
    let id = id.trim();
    let lower = id.to_ascii_lowercase();
    if lower.contains("poacc:billing:3.0") {
        return Some("Peppol BIS Billing 3.0".into());
    }
    if let Some(i) = lower.find("xrechnung_") {
        let v: String = id[i + 10..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        return Some(format!("XRechnung {v}").trim().to_owned());
    }
    if lower.contains("factur-x.eu:1p0:extended") || lower.contains("cii:extended") {
        return Some("Factur-X / ZUGFeRD EXTENDED".into());
    }
    if lower.contains("factur-x.eu:1p0:basicwl") {
        return Some("Factur-X / ZUGFeRD BASIC WL".into());
    }
    if lower.contains("factur-x.eu:1p0:basic") {
        return Some("Factur-X / ZUGFeRD BASIC".into());
    }
    if lower.contains("factur-x.eu:1p0:minimum") {
        return Some("Factur-X / ZUGFeRD MINIMUM".into());
    }
    if lower.contains("zugferd") {
        return Some("ZUGFeRD".into());
    }
    if lower == "urn:cen.eu:en16931:2017" {
        return Some("EN 16931 (Factur-X / ZUGFeRD EN 16931 profile)".into());
    }
    if lower.starts_with("urn:cen.eu:en16931:2017") {
        return Some("EN 16931 based customization".into());
    }
    None
}

/// Names of the Electronic Address Scheme codes used for Peppol endpoints (the common ones).
pub fn eas_name(code: &str) -> Option<&'static str> {
    Some(match code.trim() {
        "0002" => "FR SIRENE",
        "0007" => "SE organisation number",
        "0009" => "FR SIRET",
        "0060" => "DUNS",
        "0088" => "GLN",
        "0106" => "NL Chamber of Commerce",
        "0151" => "AU ABN",
        "0184" => "DK CVR (DIGSTORG)",
        "0190" => "NL OIN",
        "0192" => "NO organisation number",
        "0195" => "SG UEN",
        "0204" => "DE Leitweg-ID",
        "0208" => "BE enterprise number",
        "0209" => "GS1 identification key",
        "0210" => "IT codice fiscale",
        "0211" => "IT partita IVA",
        "9914" => "AT VAT",
        "9915" => "AT government",
        "9920" => "ES VAT",
        "9925" => "BE VAT",
        "9930" => "DE VAT",
        "9938" => "LU VAT",
        "9944" => "NL VAT",
        "9945" => "PL VAT",
        "9946" => "PT VAT",
        "9957" => "FR VAT",
        _ => return None,
    })
}
