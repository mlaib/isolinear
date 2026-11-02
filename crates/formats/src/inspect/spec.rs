//! XML and JSON standards described as data: how to recognize them, and paths to their facts
//! and table rows. A path is a list of steps separated by `/`, from the root (or from a row):
//!
//! - `Name`: child elements (XML, local name) or object members (JSON) called `Name`;
//!   JSON arrays are stepped through implicitly;
//! - `*`: any child; `**`: any descendant (or the node itself), then the next step;
//! - `..`: the parent (XML);
//! - `@attr` (XML, last step): an attribute value;
//! - `Name[Sub/Path=VALUE]`: only those whose `Sub/Path` has that text;
//! - `a | b`: the first alternative that gives a value;
//! - `count:PATH` and `sum:PATH`: how many matches, and the exact decimal sum of their values.
use super::{Inspection, Row, Stage, State, Table, jsondoc, xmldoc::Doc};
use std::ops::Range;

/// A fact: group heading, label, path.
#[derive(Clone, Copy)]
pub struct F(pub &'static str, pub &'static str, pub &'static str);

/// A table: name, path to its rows, and (heading, path from the row) for each column.
#[derive(Clone, Copy)]
pub struct T {
    pub name: &'static str,
    pub rows: &'static str,
    pub columns: &'static [(&'static str, &'static str)],
}

pub struct XmlSpec {
    pub family: &'static str,
    pub kind: &'static str,
    /// Whether this spec is for a document with this root (local name, namespace URI).
    pub matches: fn(&str, &str) -> bool,
    pub version: fn(&Doc) -> Option<String>,
    pub facts: &'static [F],
    pub tables: &'static [T],
    /// Further facts, checks or tables that paths cannot express.
    pub custom: Option<fn(&Doc, &mut Inspection)>,
}

pub struct JsonSpec {
    pub family: &'static str,
    pub kind: &'static str,
    /// Whether this spec is for a document with these top-level keys (and head text).
    pub matches: fn(&[String], &str) -> bool,
    pub version: fn(&jsondoc::Node) -> Option<String>,
    pub facts: &'static [F],
    pub tables: &'static [T],
    pub custom: Option<fn(&jsondoc::Node, &mut Inspection)>,
}

// ----- XML paths -----

/// The steps of a path: `/`-separated, except inside a `[...]` predicate.
fn steps(path: &str) -> Vec<&str> {
    let mut out = vec![];
    let (mut depth, mut start) = (0, 0);
    for (i, c) in path.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            '/' if depth == 0 => {
                out.push(&path[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&path[start..]);
    out.retain(|s| !s.is_empty());
    out
}

fn split_predicate(step: &str) -> (&str, Option<(&str, &str)>) {
    match step.find('[') {
        Some(i) if step.ends_with(']') => {
            let inner = &step[i + 1..step.len() - 1];
            let pred = inner.split_once('=');
            (&step[..i], pred)
        }
        _ => (step, None),
    }
}

/// Elements reached from `from` by `path` (attribute steps excluded).
pub fn select(doc: &Doc, from: u32, path: &str) -> Vec<u32> {
    let mut current = vec![from];
    for step in steps(path).into_iter().filter(|s| !s.starts_with('@')) {
        let (name, pred) = split_predicate(step);
        let mut next = vec![];
        for &e in &current {
            match name {
                "**" => {
                    next.push(e);
                    next.extend(doc.descendants(e));
                }
                ".." => next.extend(doc.get(e).parent),
                "." => next.push(e),
                _ => next.extend(
                    doc.get(e)
                        .children
                        .iter()
                        .copied()
                        .filter(|&c| name == "*" || doc.get(c).name == name),
                ),
            }
            if next.len() > super::MAX_ROWS * 4 {
                break;
            }
        }
        if let Some((sub, want)) = pred {
            next.retain(|&e| value_at(doc, e, sub).is_some_and(|(v, _)| v == want));
        }
        // `**` followed by a name: keep document order without duplicates.
        if name == "**" {
            next.sort_unstable();
            next.dedup();
        }
        current = next;
    }
    current
}

/// The value at `path` from `from`: text of the first element, or its attribute.
pub fn value_at(doc: &Doc, from: u32, path: &str) -> Option<(String, Range<u64>)> {
    for alt in path.split(" | ") {
        let alt = alt.trim();
        if let Some(rest) = alt.strip_prefix("count:") {
            return Some((
                select(doc, from, rest).len().to_string(),
                doc.get(from).span.clone(),
            ));
        }
        if let Some(rest) = alt.strip_prefix("sum:") {
            let values: Vec<String> = select(doc, from, rest)
                .iter()
                .map(|&e| doc.text(e))
                .collect();
            if values.is_empty() {
                continue;
            }
            return Some((
                decimal_sum(values.iter().map(String::as_str)).unwrap_or_else(|| "?".into()),
                doc.get(from).span.clone(),
            ));
        }
        let attr = steps(alt).last().copied().filter(|s| s.starts_with('@'));
        for e in select(doc, from, alt) {
            let span = doc.get(e).span.clone();
            let v = match attr {
                Some(a) => doc.attr(e, &a[1..]).map(str::to_owned),
                None => Some(doc.text(e)),
            };
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                return Some((v, span));
            }
        }
    }
    None
}

/// Facts and tables of `spec` over `doc`.
pub fn run_xml(spec: &XmlSpec, doc: &Doc) -> Inspection {
    let mut out = Inspection::new(spec.family, spec.kind);
    out.version = (spec.version)(doc);
    for F(group, label, path) in spec.facts {
        if let Some((v, span)) = value_at(doc, 0, path) {
            out.fact(group, label, v, Some(span));
        }
    }
    for t in spec.tables {
        let rows = select(doc, 0, t.rows);
        let total = rows.len();
        let mut table = Table {
            name: t.name.into(),
            columns: t.columns.iter().map(|(h, _)| (*h).to_owned()).collect(),
            rows: vec![],
            total: (total > super::MAX_ROWS).then_some(total as u64),
        };
        for &r in rows.iter().take(super::MAX_ROWS) {
            table.rows.push(Row {
                cells: t
                    .columns
                    .iter()
                    .map(|(_, p)| value_at(doc, r, p).map(|(v, _)| v).unwrap_or_default())
                    .collect(),
                source: Some(doc.get(r).span.clone()),
            });
        }
        if !table.rows.is_empty() {
            out.tables.push(table);
        }
    }
    if let Some(custom) = spec.custom {
        custom(doc, &mut out);
    }
    if doc.unclosed.is_empty() {
        out.check(
            Stage::Parsed,
            State::Passed,
            "Read as XML; the fields shown were found by name",
        );
    } else {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!(
                "The document ends before <{}> is closed: what comes before is shown",
                doc.unclosed.last().map(String::as_str).unwrap_or("")
            ),
        );
    }
    out.not_validated();
    out
}

// ----- JSON paths -----

pub fn select_json<'a>(from: &'a jsondoc::Node, path: &str) -> Vec<&'a jsondoc::Node> {
    use jsondoc::Value;
    let mut current = vec![from];
    for step in steps(path) {
        let (name, pred) = split_predicate(step);
        let mut next = vec![];
        for n in current {
            // Arrays are stepped through implicitly.
            let targets: Vec<&jsondoc::Node> = match (&n.value, name) {
                (Value::Array(items), n2) if n2 != "*" && n2 != "**" => items.iter().collect(),
                _ => vec![n],
            };
            for t in targets {
                match (name, &t.value) {
                    ("**", _) => {
                        let mut stack = vec![t];
                        while let Some(x) = stack.pop() {
                            next.push(x);
                            match &x.value {
                                Value::Array(items) => stack.extend(items.iter().rev()),
                                Value::Object(fields) => {
                                    stack.extend(fields.iter().rev().map(|(_, v)| v))
                                }
                                _ => {}
                            }
                            if next.len() > super::MAX_ROWS * 4 {
                                break;
                            }
                        }
                    }
                    ("*", Value::Array(items)) => next.extend(items.iter()),
                    ("*", Value::Object(fields)) => next.extend(fields.iter().map(|(_, v)| v)),
                    (key, Value::Object(fields)) => {
                        next.extend(fields.iter().filter(|(k, _)| k == key).map(|(_, v)| v))
                    }
                    _ => {}
                }
            }
        }
        // A named step that reached arrays gives their items.
        if name != "*" && name != "**" {
            next = next
                .into_iter()
                .flat_map(|n| match &n.value {
                    Value::Array(items) if pred.is_some() => items.iter().collect::<Vec<_>>(),
                    _ => vec![n],
                })
                .collect();
        }
        if let Some((sub, want)) = pred {
            next.retain(|n| json_value(n, sub).is_some_and(|(v, _)| v == want));
        }
        current = next;
    }
    current
}

pub fn json_value(from: &jsondoc::Node, path: &str) -> Option<(String, Range<u64>)> {
    for alt in path.split(" | ") {
        let alt = alt.trim();
        if let Some(rest) = alt.strip_prefix("count:") {
            let n: usize = select_json(from, rest)
                .iter()
                .map(|n| match &n.value {
                    jsondoc::Value::Array(items) => items.len(),
                    _ => 1,
                })
                .sum();
            return Some((n.to_string(), from.span.clone()));
        }
        if let Some(rest) = alt.strip_prefix("sum:") {
            let values: Vec<String> = select_json(from, rest).iter().map(|n| n.text()).collect();
            if values.is_empty() {
                continue;
            }
            return Some((
                decimal_sum(values.iter().map(String::as_str)).unwrap_or_else(|| "?".into()),
                from.span.clone(),
            ));
        }
        for n in select_json(from, alt) {
            let v = match &n.value {
                jsondoc::Value::Array(items) if items.iter().all(|i| i.is_scalar()) => items
                    .iter()
                    .map(|i| i.text())
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => n.text(),
            };
            if !v.is_empty() {
                return Some((v, n.span.clone()));
            }
        }
    }
    None
}

pub fn run_json(spec: &JsonSpec, root: &jsondoc::Node) -> Inspection {
    let mut out = Inspection::new(spec.family, spec.kind);
    out.version = (spec.version)(root);
    for F(group, label, path) in spec.facts {
        if let Some((v, span)) = json_value(root, path) {
            out.fact(group, label, v, Some(span));
        }
    }
    for t in spec.tables {
        let rows = select_json(root, t.rows);
        let total = rows.len();
        let mut table = Table {
            name: t.name.into(),
            columns: t.columns.iter().map(|(h, _)| (*h).to_owned()).collect(),
            rows: vec![],
            total: (total > super::MAX_ROWS).then_some(total as u64),
        };
        for r in rows.into_iter().take(super::MAX_ROWS) {
            table.rows.push(Row {
                cells: t
                    .columns
                    .iter()
                    .map(|(_, p)| json_value(r, p).map(|(v, _)| v).unwrap_or_default())
                    .collect(),
                source: Some(r.span.clone()),
            });
        }
        if !table.rows.is_empty() {
            out.tables.push(table);
        }
    }
    if let Some(custom) = spec.custom {
        custom(root, &mut out);
    }
    out.check(
        Stage::Parsed,
        State::Passed,
        "Read as JSON; the fields shown were found by name",
    );
    out.not_validated();
    out
}

/// Exact sum of decimal numbers written as text ("1234.50", "-7"); `None` if one is not a number.
pub fn decimal_sum<'a>(values: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut parts: Vec<(i128, u32)> = vec![];
    for v in values {
        let v = v.trim();
        let (neg, digits) = match v.strip_prefix('-') {
            Some(d) => (true, d),
            None => (false, v.strip_prefix('+').unwrap_or(v)),
        };
        let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
        if int.is_empty() && frac.is_empty()
            || !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit())
            || int.len() + frac.len() > 30
        {
            return None;
        }
        let n: i128 = format!("{int}{frac}").parse().ok()?;
        parts.push((if neg { -n } else { n }, frac.len() as u32));
    }
    let scale = parts.iter().map(|(_, s)| *s).max().unwrap_or(0);
    let mut total: i128 = 0;
    for (n, s) in parts {
        total = total.checked_add(n.checked_mul(10i128.checked_pow(scale - s)?)?)?;
    }
    let neg = total < 0;
    let digits = total.unsigned_abs().to_string();
    let digits = format!("{digits:0>width$}", width = scale as usize + 1);
    let (int, frac) = digits.split_at(digits.len() - scale as usize);
    Some(format!(
        "{}{int}{}{frac}",
        if neg { "-" } else { "" },
        if scale > 0 { "." } else { "" }
    ))
}

/// Whether two decimal numbers written as text are equal ("10.00" and "10" are).
pub fn decimal_eq(a: &str, b: &str) -> bool {
    let negated = match b.trim().strip_prefix('-') {
        Some(rest) => rest.to_owned(),
        None => format!("-{}", b.trim().trim_start_matches('+')),
    };
    decimal_sum([a, negated.as_str()].into_iter())
        .is_some_and(|d| d.chars().all(|c| matches!(c, '0' | '.' | '-')))
}

/// Compare a declared total with the computed one, as a business-rule check.
pub fn compare_total(
    out: &mut Inspection,
    what: &str,
    declared: Option<String>,
    computed: Option<String>,
) {
    match (declared, computed) {
        (Some(d), Some(c)) => {
            let same = decimal_eq(&d, &c);
            out.check(
                Stage::Semantic,
                if same { State::Passed } else { State::Failed },
                format!("{what}: declared {d}, computed {c}"),
            );
        }
        (Some(d), None) => out.check(
            Stage::Semantic,
            State::NotDone,
            format!("{what}: declared {d}; nothing to compare with"),
        ),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xml_paths() {
        let src = r#"<D><H><Id>M1</Id></H><T k="a"><Amt Ccy="EUR">10.50</Amt><S><Cd>OK</Cd></S></T><T k="b"><Amt Ccy="USD">-0.5</Amt><S><Cd>NO</Cd></S></T></D>"#;
        let doc = Doc::parse(src, &|| false).unwrap();
        assert_eq!(value_at(&doc, 0, "H/Id").unwrap().0, "M1");
        assert_eq!(value_at(&doc, 0, "**/Id").unwrap().0, "M1");
        assert_eq!(select(&doc, 0, "T").len(), 2);
        assert_eq!(value_at(&doc, 0, "T[S/Cd=NO]/Amt/@Ccy").unwrap().0, "USD");
        assert_eq!(value_at(&doc, 0, "T/@k").unwrap().0, "a");
        assert_eq!(value_at(&doc, 0, "count:T").unwrap().0, "2");
        assert_eq!(value_at(&doc, 0, "sum:T/Amt").unwrap().0, "10.00");
        assert_eq!(value_at(&doc, 0, "Missing | H/Id").unwrap().0, "M1");
        let t = select(&doc, 0, "T")[1];
        assert_eq!(value_at(&doc, t, "../H/Id").unwrap().0, "M1");
    }
    #[test]
    fn json_paths() {
        let n = jsondoc::parse(r#"{"entry":[{"resource":{"resourceType":"Patient","id":"p1","name":[{"family":"Doe"}]}},{"resource":{"resourceType":"Observation","id":"o1"}}]}"#).unwrap();
        assert_eq!(json_value(&n, "entry/resource/id").unwrap().0, "p1");
        assert_eq!(select_json(&n, "entry/resource").len(), 2);
        assert_eq!(
            json_value(&n, "entry/resource[resourceType=Observation]/id")
                .unwrap()
                .0,
            "o1"
        );
        assert_eq!(json_value(&n, "count:entry").unwrap().0, "2");
        assert_eq!(json_value(&n, "**/family").unwrap().0, "Doe");
    }
    #[test]
    fn decimal_sums_are_exact() {
        assert_eq!(decimal_sum(["0.1", "0.2"].into_iter()).unwrap(), "0.3");
        assert_eq!(
            decimal_sum(["1000", "-0.01"].into_iter()).unwrap(),
            "999.99"
        );
        assert_eq!(decimal_sum(["-1.5", "0.5"].into_iter()).unwrap(), "-1.0");
        assert_eq!(
            decimal_sum(["12345678901234567890.12", "1"].into_iter()).unwrap(),
            "12345678901234567891.12"
        );
        assert!(decimal_sum(["1,5"].into_iter()).is_none());
        let mut i = Inspection::new("x", "y");
        compare_total(
            &mut i,
            "Control sum",
            Some("10.00".into()),
            Some("10".into()),
        );
        assert_eq!(i.checks.last().unwrap().state, State::Passed);
        compare_total(
            &mut i,
            "Control sum",
            Some("10.01".into()),
            Some("10".into()),
        );
        assert_eq!(i.checks.last().unwrap().state, State::Failed);
    }
}
