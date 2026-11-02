//! SDMX-JSON data messages (statistical data: `meta`/`header`, `data.dataSets`, `structure`).
//! Series and observation keys ("0:1:0") are decoded with the dimension values of the
//! message's structure.
use super::records::{Rows, join, nodes, s, span};
use crate::inspect::{
    Inspection, MAX_ROWS, Stage, State,
    jsondoc::{Node, Value},
    spec::JsonSpec,
};

pub static SDMX_JSON: JsonSpec = JsonSpec {
    family: "SDMX",
    kind: "SDMX-JSON data message",
    matches: |keys, head| {
        let has = |k: &str| keys.iter().any(|x| x == k);
        (has("meta") || has("header"))
            && (has("data") || has("dataSets"))
            && (head.contains("\"dataSets\"") || head.to_ascii_lowercase().contains("sdmx"))
    },
    version: |root| {
        let schema = s(root, "meta/schema");
        if let Some(i) = schema.find("sdmx-json") {
            let tail = &schema[i..];
            if let Some(v) = tail
                .split('/')
                .find(|p| p.chars().next().is_some_and(|c| c.is_ascii_digit()))
            {
                return Some(format!("SDMX-JSON {v}"));
            }
        }
        Some(if root.get("meta").is_some() {
            "SDMX-JSON (version not stated)".into()
        } else {
            "SDMX-JSON 1.0 draft layout".into()
        })
    },
    facts: &[],
    tables: &[],
    custom: Some(custom),
};

struct Component<'a> {
    id: String,
    name: String,
    values: Vec<&'a Node>,
}

fn components<'a>(structure: Option<&'a Node>, path: &str) -> Vec<Component<'a>> {
    let Some(st) = structure else {
        return vec![];
    };
    nodes(st, path)
        .into_iter()
        .map(|d| Component {
            id: s(d, "id"),
            name: localized(d, "name"),
            values: nodes(d, "values"),
        })
        .collect()
}

/// A name that may be a string or a map of languages (`{"en": "…"}`).
fn localized(n: &Node, key: &str) -> String {
    match n.get(key).map(|v| &v.value) {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Object(fields)) => fields
            .iter()
            .find(|(k, _)| k == "en")
            .or(fields.first())
            .map(|(_, v)| v.text())
            .unwrap_or_default(),
        _ => {
            let names = format!("{key}s");
            n.get(&names)
                .map(|_| localized(n, &names))
                .unwrap_or_default()
        }
    }
}

/// Value `index` of a component, as "id (name)" when the name says more.
fn value_label(c: &Component, index: &str) -> String {
    let Ok(i) = index.trim().parse::<usize>() else {
        return format!("?{index}");
    };
    match c.values.get(i) {
        Some(v) => {
            let (id, name) = (s(v, "id"), localized(v, "name"));
            if name.is_empty() || name == id {
                id
            } else if id.is_empty() {
                name
            } else {
                format!("{id} ({name})")
            }
        }
        None => format!("?{i}"),
    }
}

fn custom(root: &Node, out: &mut Inspection) {
    let header = root.get("meta").or(root.get("header"));
    if let Some(h) = header {
        for (label, path) in [
            ("Message ID", "id"),
            ("Prepared", "prepared"),
            ("Test", "test"),
            ("Sender", "sender/id"),
            ("Sender name", "sender/name"),
            ("Receiver", "receiver/id"),
            ("Languages", "contentLanguages"),
            ("Schema", "schema"),
        ] {
            if let Some(n) = nodes(h, path).first() {
                let v = if label == "Sender name" {
                    localized(h.get("sender").unwrap_or(h), "name")
                } else {
                    s(h, path)
                };
                out.fact("Header", label, v, span(n));
            }
        }
    }
    let data = root.get("data").unwrap_or(root);
    let structures: Vec<&Node> = if let Some(st) = data.get("structure").or(root.get("structure")) {
        vec![st]
    } else {
        nodes(data, "structures")
    };
    let mut datasets = nodes(data, "dataSets");
    if datasets.is_empty() {
        datasets = nodes(root, "dataSets");
    }
    let first = structures.first().copied();
    out.fact(
        "Structure",
        "Name",
        first.map(|st| localized(st, "name")).unwrap_or_default(),
        first.and_then(span),
    );
    out.fact(
        "Structure",
        "Dataflow",
        first
            .map(|st| s(st, "links/urn | links/href | dataflow"))
            .unwrap_or_default(),
        None,
    );
    out.fact("Data", "Data sets", datasets.len().to_string(), None);

    // Dimensions and attributes of the first structure.
    let mut dt = Rows::new(
        "Dimensions",
        &[
            "Level",
            "Position",
            "ID",
            "Name",
            "Role",
            "Values",
            "First values",
        ],
    );
    let mut at = Rows::new(
        "Attributes",
        &["Level", "ID", "Name", "Values", "First values"],
    );
    if let Some(st) = first {
        for (level, path) in [
            ("data set", "dimensions/dataset"),
            ("series", "dimensions/series"),
            ("observation", "dimensions/observation"),
        ] {
            for (i, d) in nodes(st, path).into_iter().enumerate() {
                let c = Component {
                    id: s(d, "id"),
                    name: localized(d, "name"),
                    values: nodes(d, "values"),
                };
                let firsts: Vec<String> = (0..c.values.len().min(5))
                    .map(|v| value_label(&c, &v.to_string()))
                    .collect();
                let pos = s(d, "keyPosition");
                dt.push(
                    vec![
                        level.into(),
                        if pos.is_empty() { i.to_string() } else { pos },
                        c.id.clone(),
                        c.name.clone(),
                        s(d, "role | roles"),
                        c.values.len().to_string(),
                        firsts.join(", "),
                    ],
                    span(d),
                );
                if level == "data set" && c.values.len() == 1 {
                    out.fact("Fixed dimensions", &c.id, value_label(&c, "0"), span(d));
                }
            }
        }
        for (level, path) in [
            ("data set", "attributes/dataSet"),
            ("series", "attributes/series"),
            ("observation", "attributes/observation"),
        ] {
            for d in nodes(st, path) {
                let c = Component {
                    id: s(d, "id"),
                    name: localized(d, "name"),
                    values: nodes(d, "values"),
                };
                let firsts: Vec<String> = (0..c.values.len().min(5))
                    .map(|v| value_label(&c, &v.to_string()))
                    .collect();
                at.push(
                    vec![
                        level.into(),
                        c.id.clone(),
                        c.name.clone(),
                        c.values.len().to_string(),
                        firsts.join(", "),
                    ],
                    span(d),
                );
            }
        }
    }
    dt.finish(out);
    at.finish(out);

    // Observations, with keys decoded.
    let mut table: Option<Rows> = None;
    let mut bad_keys = 0u64;
    let mut observations = 0u64;
    for (di, ds) in datasets.iter().enumerate() {
        let st = s(ds, "structure")
            .parse::<usize>()
            .ok()
            .and_then(|i| structures.get(i).copied())
            .or(first);
        let series_dims = components(st, "dimensions/series");
        let obs_dims = components(st, "dimensions/observation");
        let series_attrs = components(st, "attributes/series");
        let obs_attrs = components(st, "attributes/observation");
        let rows = table.get_or_insert_with(|| {
            let mut cols: Vec<String> = vec![];
            if datasets.len() > 1 {
                cols.push("Data set".into());
            }
            cols.extend(series_dims.iter().map(|d| d.id.clone()));
            cols.extend(obs_dims.iter().map(|d| d.id.clone()));
            cols.push("Value".into());
            cols.extend(series_attrs.iter().map(|a| a.id.clone()));
            cols.extend(obs_attrs.iter().map(|a| a.id.clone()));
            let refs: Vec<&str> = cols.iter().map(String::as_str).collect();
            Rows::new("Observations", &refs)
        });
        let decode = |key: &str, dims: &[Component], bad: &mut u64| -> Vec<String> {
            let parts: Vec<&str> = if key.is_empty() {
                vec![]
            } else {
                key.split(':').collect()
            };
            if parts.len() != dims.len() {
                *bad += 1;
            }
            (0..dims.len())
                .map(|i| {
                    parts
                        .get(i)
                        .map(|p| value_label(&dims[i], p))
                        .unwrap_or_default()
                })
                .collect()
        };
        let attr_values = |list: Option<&Node>, attrs: &[Component], skip: usize| -> Vec<String> {
            let items: Vec<&Node> = list.map(|l| l.items().iter().collect()).unwrap_or_default();
            (0..attrs.len())
                .map(|i| match items.get(i + skip) {
                    Some(v) if v.is_scalar() && !v.text().is_empty() => {
                        value_label(&attrs[i], &v.text())
                    }
                    _ => String::new(),
                })
                .collect()
        };
        let mut emit = |rows: &mut Rows,
                        series_cells: &[String],
                        series_attr_cells: &[String],
                        obs_key: &str,
                        obs: &Node,
                        bad: &mut u64| {
            observations += 1;
            if rows.seen() as usize > MAX_ROWS {
                rows.push(vec![], None);
                return;
            }
            let mut cells: Vec<String> = vec![];
            if datasets.len() > 1 {
                cells.push((di + 1).to_string());
            }
            cells.extend_from_slice(series_cells);
            cells.extend(decode(obs_key, &obs_dims, bad));
            let value = match &obs.value {
                Value::Array(items) => items.first().map(|v| v.text()).unwrap_or_default(),
                _ => obs.text(),
            };
            cells.push(value);
            cells.extend_from_slice(series_attr_cells);
            cells.extend(attr_values(Some(obs), &obs_attrs, 1));
            rows.push(cells, span(obs));
        };
        if let Some(Value::Object(series)) = ds.get("series").map(|n| &n.value) {
            for (key, sr) in series {
                let series_cells = decode(key, &series_dims, &mut bad_keys);
                let series_attr_cells = attr_values(sr.get("attributes"), &series_attrs, 0);
                if let Some(Value::Object(obs)) = sr.get("observations").map(|n| &n.value) {
                    for (ok, ov) in obs {
                        emit(
                            rows,
                            &series_cells,
                            &series_attr_cells,
                            ok,
                            ov,
                            &mut bad_keys,
                        );
                    }
                }
            }
        } else if let Some(Value::Object(obs)) = ds.get("observations").map(|n| &n.value) {
            // Flat layout: each key covers every observation-level dimension.
            for (ok, ov) in obs {
                emit(rows, &[], &[], ok, ov, &mut bad_keys);
            }
        }
    }
    out.fact("Data", "Observations", observations.to_string(), None);
    if let Some(t) = table {
        t.finish(out);
    }
    if observations > 0 {
        out.check(
            Stage::Semantic,
            if bad_keys > 0 { State::Failed } else { State::Passed },
            format!(
                "Series and observation keys match the structure's dimensions: {} key(s) with the wrong number of positions",
                bad_keys
            ),
        );
    }
    out.notes.push(join(
        &[
            "Dimension and attribute values are decoded from the message's own structure; code lists are not fetched".into(),
            if structures.is_empty() {
                "this message has no structure, so keys are shown as numbers".into()
            } else {
                String::new()
            },
        ],
        "; ",
    ));
}
