//! SDMX-ML statistical data and structure messages (2.0, 2.1 and 3.0): the header, the
//! referenced dataflow and data structure, observations with their series keys, and for
//! structure messages the dataflows, dimensions, attributes, codelists and concepts.
use super::support::short;
use crate::inspect::{
    Inspection, MAX_ROWS, Row, Table,
    spec::{F, T, XmlSpec, select},
    xmldoc::Doc,
};
use std::ops::Range;

/// An observation: its bytes and its (component, value) pairs.
type ObsRow = (Range<u64>, Vec<(String, String)>);

fn is_message(ns: &str) -> bool {
    let ns = ns.to_ascii_lowercase();
    ns.contains("sdmx.org/resources/sdmxml/schemas/") && ns.ends_with("/message")
}

fn version(doc: &Doc) -> Option<String> {
    let ns = doc.root().ns.to_ascii_lowercase();
    let v = ns
        .split('/')
        .find(|p| p.starts_with('v') && p.contains('_'))?;
    Some(format!("SDMX-ML {}", v[1..].replace('_', ".")))
}

const HEADER: [F; 14] = [
    F("Header", "ID", "Header/ID"),
    F("Header", "Test", "Header/Test"),
    F("Header", "Prepared", "Header/Prepared"),
    F("Header", "Sender", "Header/Sender/@id"),
    F("Header", "Sender name", "Header/Sender/Name"),
    F("Header", "Receiver", "Header/Receiver/@id"),
    F("Header", "Data set ID", "Header/DataSetID"),
    F("Header", "Action", "Header/DataSetAction"),
    F("Header", "Reporting begins", "Header/ReportingBegin"),
    F("Header", "Reporting ends", "Header/ReportingEnd"),
    F(
        "Structure",
        "Data structure",
        "Header/Structure/Structure/Ref/@id | Header/Structure/Structure | Header/KeyFamilyRef",
    ),
    F(
        "Structure",
        "Agency",
        "Header/Structure/Structure/Ref/@agencyID | Header/Structure/StructureUsage/Ref/@agencyID",
    ),
    F(
        "Structure",
        "Dataflow",
        "Header/Structure/StructureUsage/Ref/@id | Header/Structure/StructureUsage | Header/Structure/Dataflow",
    ),
    F(
        "Structure",
        "Dimension at observation",
        "Header/Structure/@dimensionAtObservation",
    ),
];

/// Attributes that are XML machinery rather than SDMX components.
fn component_attr(k: &str) -> bool {
    !matches!(k, "type" | "schemaLocation" | "nil") && !k.starts_with("xmlns")
}

/// Key/value pairs of a series: `SeriesKey/Value` (generic) or the attributes (specific).
fn series_pairs(doc: &Doc, s: u32) -> Vec<(String, String)> {
    let mut pairs = vec![];
    let generic: Vec<u32> = select(doc, s, "SeriesKey/Value");
    if !generic.is_empty() {
        for v in generic
            .into_iter()
            .chain(select(doc, s, "Attributes/Value"))
        {
            let k = doc.attr(v, "id").or(doc.attr(v, "concept")).unwrap_or("?");
            let val = doc
                .attr(v, "value")
                .map(str::to_owned)
                .unwrap_or_else(|| doc.text(v));
            pairs.push((k.to_owned(), val));
        }
    } else {
        pairs.extend(
            doc.get(s)
                .attrs
                .iter()
                .filter(|(k, _)| component_attr(k))
                .cloned(),
        );
    }
    pairs
}

/// Pairs of one observation, the time and value under the usual names.
fn obs_pairs(doc: &Doc, o: u32) -> Vec<(String, String)> {
    let mut pairs = vec![];
    let el = doc.get(o);
    let mut generic = false;
    for &c in &el.children {
        let child = doc.get(c);
        match child.name.as_str() {
            "ObsDimension" | "ObsKey" => {
                generic = true;
                if child.children.is_empty() {
                    let k = doc.attr(c, "id").unwrap_or("TIME_PERIOD");
                    pairs.push((k.to_owned(), doc.attr(c, "value").unwrap_or("").to_owned()));
                } else {
                    for v in select(doc, c, "Value") {
                        let k = doc.attr(v, "id").unwrap_or("?");
                        pairs.push((k.to_owned(), doc.attr(v, "value").unwrap_or("").to_owned()));
                    }
                }
            }
            "Time" => {
                generic = true;
                pairs.push(("TIME_PERIOD".into(), doc.text(c)));
            }
            "ObsValue" => {
                generic = true;
                pairs.push((
                    "OBS_VALUE".into(),
                    doc.attr(c, "value").unwrap_or("").to_owned(),
                ));
            }
            "Attributes" => {
                for v in select(doc, c, "Value") {
                    let k = doc.attr(v, "id").or(doc.attr(v, "concept")).unwrap_or("?");
                    pairs.push((k.to_owned(), doc.attr(v, "value").unwrap_or("").to_owned()));
                }
            }
            _ => {}
        }
    }
    if !generic {
        for (k, v) in el.attrs.iter().filter(|(k, _)| component_attr(k)) {
            // SDMX 2.0 compact data names the time and value TIME and OBS_VALUE.
            let k = if k == "TIME" {
                "TIME_PERIOD"
            } else {
                k.as_str()
            };
            pairs.push((k.to_owned(), v.clone()));
        }
    }
    pairs
}

fn add_column(columns: &mut Vec<String>, k: &str) {
    if columns.len() < 64 && !columns.iter().any(|c| c == k) {
        columns.push(k.to_owned());
    }
}

fn observations(doc: &Doc, out: &mut Inspection) {
    let datasets: Vec<u32> = doc
        .elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.name == "DataSet")
        .map(|(i, _)| i as u32)
        .collect();
    let mut rows: Vec<ObsRow> = vec![];
    let (mut key_cols, mut obs_cols) = (vec![], vec![]);
    let (mut total, mut series_count) = (0usize, 0usize);
    let mut series_table = Table {
        name: "Series".into(),
        columns: vec!["Key".into(), "Observations".into()],
        rows: vec![],
        total: None,
    };
    for &ds in &datasets {
        for &c in &doc.get(ds).children {
            let name = doc.get(c).name.as_str();
            let (key, obs): (Vec<(String, String)>, Vec<u32>) = match name {
                "Series" => {
                    series_count += 1;
                    let key = series_pairs(doc, c);
                    let obs = select(doc, c, "Obs");
                    if series_table.rows.len() < MAX_ROWS {
                        series_table.rows.push(Row {
                            cells: vec![
                                short(
                                    &key.iter()
                                        .map(|(k, v)| format!("{k}={v}"))
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                    400,
                                ),
                                obs.len().to_string(),
                            ],
                            source: Some(doc.get(c).span.clone()),
                        });
                    }
                    (key, obs)
                }
                "Obs" => (vec![], vec![c]),
                _ => continue,
            };
            for (k, _) in &key {
                add_column(&mut key_cols, k);
            }
            for o in obs {
                total += 1;
                if rows.len() >= MAX_ROWS {
                    continue;
                }
                let mut pairs = key.clone();
                for (k, v) in obs_pairs(doc, o) {
                    if !key_cols.contains(&k) {
                        add_column(&mut obs_cols, &k);
                    }
                    pairs.push((k, v));
                }
                rows.push((doc.get(o).span.clone(), pairs));
            }
        }
    }
    out.fact("Data", "Data sets", datasets.len().to_string(), None);
    out.fact("Data", "Series", series_count.to_string(), None);
    out.fact("Data", "Observations", total.to_string(), None);
    if rows.is_empty() {
        return;
    }
    // Series key first, then time and value, then the observation's other attributes.
    let mut columns = key_cols;
    for k in ["TIME_PERIOD", "OBS_VALUE"] {
        if obs_cols.iter().any(|c| c == k) {
            add_column(&mut columns, k);
        }
    }
    for k in &obs_cols {
        add_column(&mut columns, k);
    }
    let table = Table {
        name: "Observations".into(),
        rows: rows
            .into_iter()
            .map(|(span, pairs)| Row {
                cells: columns
                    .iter()
                    .map(|c| {
                        pairs
                            .iter()
                            .find(|(k, _)| k == c)
                            .map(|(_, v)| v.clone())
                            .unwrap_or_default()
                    })
                    .collect(),
                source: Some(span),
            })
            .collect(),
        columns,
        total: (total > MAX_ROWS).then_some(total as u64),
    };
    out.tables.push(table);
    if !series_table.rows.is_empty() {
        out.tables.push(series_table);
    }
    out.notes.push(
        "Codes are shown as written; their names come from the codelists of the data structure, which are not fetched"
            .into(),
    );
}

pub static DATA: XmlSpec = XmlSpec {
    family: "SDMX",
    kind: "SDMX-ML data message",
    matches: |root, ns| is_message(ns) && root != "Structure" && root.ends_with("Data"),
    version,
    facts: &[
        HEADER[0], HEADER[1], HEADER[2], HEADER[3], HEADER[4], HEADER[5], HEADER[6], HEADER[7],
        HEADER[8], HEADER[9], HEADER[10], HEADER[11], HEADER[12], HEADER[13],
    ],
    tables: &[],
    custom: Some(|doc, out| {
        out.kind = match doc.root().name.as_str() {
            "GenericData" | "GenericTimeSeriesData" => "SDMX-ML generic data message",
            "StructureSpecificData" | "StructureSpecificTimeSeriesData" => {
                "SDMX-ML structure-specific data message"
            }
            "CompactData" => "SDMX-ML compact data message",
            _ => "SDMX-ML data message",
        }
        .into();
        observations(doc, out);
    }),
};

pub static STRUCTURE: XmlSpec = XmlSpec {
    family: "SDMX",
    kind: "SDMX-ML structure message",
    matches: |root, ns| is_message(ns) && root == "Structure",
    version,
    facts: &[
        HEADER[0],
        HEADER[1],
        HEADER[2],
        HEADER[3],
        HEADER[4],
        HEADER[5],
        F("Content", "Dataflows", "count:**/Dataflows/Dataflow"),
        F(
            "Content",
            "Data structures",
            "count:**/DataStructures/DataStructure",
        ),
        F("Content", "Codelists", "count:**/Codelists/Codelist"),
        F("Content", "Concept schemes", "count:**/ConceptScheme"),
    ],
    tables: &[
        T {
            name: "Dataflows",
            rows: "**/Dataflows/Dataflow",
            columns: &[
                ("ID", "@id"),
                ("Agency", "@agencyID"),
                ("Version", "@version"),
                ("Name", "Name"),
                ("Data structure", "Structure/Ref/@id | Structure"),
            ],
        },
        T {
            name: "Data structures",
            rows: "**/DataStructures/DataStructure",
            columns: &[
                ("ID", "@id"),
                ("Agency", "@agencyID"),
                ("Version", "@version"),
                ("Name", "Name"),
                (
                    "Dimensions",
                    "count:DataStructureComponents/DimensionList/*",
                ),
                (
                    "Attributes",
                    "count:DataStructureComponents/AttributeList/Attribute",
                ),
            ],
        },
        T {
            name: "Dimensions",
            rows: "**/DataStructureComponents/DimensionList/*",
            columns: &[
                ("Data structure", "../../../@id"),
                ("ID", "@id"),
                ("Position", "@position"),
                ("Concept", "ConceptIdentity/Ref/@id | ConceptIdentity"),
                (
                    "Codelist",
                    "LocalRepresentation/Enumeration/Ref/@id | LocalRepresentation/Enumeration",
                ),
                ("Text type", "LocalRepresentation/TextFormat/@textType"),
            ],
        },
        T {
            name: "Attributes",
            rows: "**/DataStructureComponents/AttributeList/Attribute",
            columns: &[
                ("Data structure", "../../../@id"),
                ("ID", "@id"),
                ("Status", "@assignmentStatus | @usage"),
                ("Concept", "ConceptIdentity/Ref/@id | ConceptIdentity"),
                (
                    "Codelist",
                    "LocalRepresentation/Enumeration/Ref/@id | LocalRepresentation/Enumeration",
                ),
            ],
        },
        T {
            name: "Measures",
            rows: "**/DataStructureComponents/MeasureList/*",
            columns: &[
                ("Data structure", "../../../@id"),
                ("ID", "@id"),
                ("Concept", "ConceptIdentity/Ref/@id | ConceptIdentity"),
            ],
        },
        T {
            name: "Codelists",
            rows: "**/Codelists/Codelist",
            columns: &[
                ("ID", "@id"),
                ("Agency", "@agencyID"),
                ("Version", "@version"),
                ("Name", "Name"),
                ("Codes", "count:Code"),
            ],
        },
        T {
            name: "Codes",
            rows: "**/Codelists/Codelist/Code",
            columns: &[
                ("Codelist", "../@id"),
                ("Code", "@id"),
                ("Name", "Name"),
                ("Parent", "Parent/Ref/@id | Parent | @parentCode"),
            ],
        },
        T {
            name: "Concepts",
            rows: "**/ConceptScheme/Concept",
            columns: &[("Scheme", "../@id"), ("ID", "@id"), ("Name", "Name")],
        },
        // SDMX-ML 2.0 key families and code lists.
        T {
            name: "Dimensions",
            rows: "**/KeyFamily/Components/Dimension",
            columns: &[
                ("Key family", "../../@id"),
                ("Concept", "@conceptRef"),
                ("Codelist", "@codelist"),
            ],
        },
        T {
            name: "Codelists",
            rows: "**/CodeLists/CodeList",
            columns: &[
                ("ID", "@id"),
                ("Agency", "@agencyID"),
                ("Name", "Name"),
                ("Codes", "count:Code"),
            ],
        },
        T {
            name: "Codes",
            rows: "**/CodeLists/CodeList/Code",
            columns: &[
                ("Codelist", "../@id"),
                ("Code", "@value"),
                ("Name", "Description"),
            ],
        },
    ],
    custom: None,
};
