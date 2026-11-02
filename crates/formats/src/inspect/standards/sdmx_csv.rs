//! SDMX-CSV data files (statistical observations, one per line). Version 1.0 starts with a
//! `DATAFLOW` column; version 2.0 with `STRUCTURE,STRUCTURE_ID` and an optional `ACTION`. Column
//! headers may carry labels (`FREQ: Frequency`). Without the data structure definition, which is
//! not in the file and never fetched, codes are shown as written and dimensions are the columns
//! before `OBS_VALUE`.
use crate::inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Row, Stage, State, Table};
use std::collections::{BTreeMap, HashSet};

/// Distinct values remembered per column (counted beyond, not listed).
const MAX_DISTINCT: usize = 10_000;
/// Series keys remembered to find duplicate observations.
const MAX_KEYS: usize = 2_000_000;
/// Columns read per record.
const MAX_COLUMNS: usize = 1_000;

pub struct SdmxCsv;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Version {
    V1,
    V2,
}

/// The code of a header cell: `FREQ: Frequency` -> `FREQ`.
fn code(cell: &str) -> &str {
    cell.split(':')
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .trim()
}

/// The version and delimiter of an SDMX-CSV header line.
fn header(line: &str) -> Option<(Version, u8)> {
    let line = line.trim_start_matches('\u{feff}');
    let delimiter = [b',', b';', b'\t']
        .into_iter()
        .find(|d| line.as_bytes().contains(d))?;
    let cells: Vec<&str> = line.split(delimiter as char).map(code).collect();
    let has = |c: &str| cells.contains(&c);
    match cells.as_slice() {
        ["STRUCTURE", "STRUCTURE_ID", ..] => Some((Version::V2, delimiter)),
        ["DATAFLOW", _, ..] if has("OBS_VALUE") || has("TIME_PERIOD") => {
            Some((Version::V1, delimiter))
        }
        _ => None,
    }
}

impl Handler for SdmxCsv {
    fn name(&self) -> &'static str {
        "SDMX-CSV"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if !matches!(probe.format, "csv" | "tsv") && !probe.ext_is(&["csv", "tsv", "txt", "sdmx"]) {
            return false;
        }
        probe
            .text()
            .and_then(|t| t.lines().next())
            .and_then(header)
            .is_some()
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let text = input.text()?;
        let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
        let (version, delimiter) = text
            .lines()
            .next()
            .and_then(header)
            .ok_or("Not an SDMX-CSV header")?;
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .delimiter(delimiter)
            .from_reader(&text.as_bytes()[bom..]);
        let mut record = csv::StringRecord::new();
        rdr.read_record(&mut record).map_err(|e| e.to_string())?;
        let columns: Vec<String> = record
            .iter()
            .take(MAX_COLUMNS)
            .map(|c| c.trim().to_owned())
            .collect();
        let codes: Vec<String> = columns.iter().map(|c| code(c).to_owned()).collect();
        let at = |c: &str| codes.iter().position(|x| x == c);
        let obs = at("OBS_VALUE");
        let time = at("TIME_PERIOD");
        let action = (version == Version::V2).then(|| at("ACTION")).flatten();
        let first_dim = match version {
            Version::V1 => 1,
            Version::V2 => 2 + usize::from(action == Some(2)),
        };
        let dims: Vec<usize> = (first_dim..obs.unwrap_or(codes.len())).collect();
        let attrs: Vec<usize> = obs.map_or(vec![], |o| (o + 1..codes.len()).collect());

        let mut out = Inspection::new("SDMX", "Statistical data (SDMX-CSV)");
        out.version = Some(match version {
            Version::V1 => "SDMX-CSV 1.0".into(),
            Version::V2 => "SDMX-CSV 2.0".into(),
        });
        let mut table = Table {
            name: "Observations".into(),
            columns: columns.clone(),
            rows: vec![],
            total: None,
        };
        let mut distinct: Vec<BTreeMap<String, u64>> = vec![BTreeMap::new(); codes.len()];
        let mut more: Vec<bool> = vec![false; codes.len()];
        let mut keys: HashSet<String> = HashSet::new();
        let mut keys_full = false;
        let (mut count, mut ragged, mut not_numbers, mut missing, mut duplicates) =
            (0u64, 0u64, 0u64, 0u64, 0u64);
        let mut examples: Vec<String> = vec![];
        let (mut first_time, mut last_time): (Option<String>, Option<String>) = (None, None);
        let mut error = None;
        loop {
            if count % 65_536 == 0 && cancelled() {
                return Err("Cancelled".into());
            }
            let start = bom as u64 + rdr.position().byte();
            match rdr.read_record(&mut record) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    error = Some(format!("after {count} observations: {e}"));
                    break;
                }
            }
            let end = bom as u64 + rdr.position().byte();
            if record.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            count += 1;
            let line = record.position().map_or(0, |p| p.line());
            if record.len() != codes.len() {
                ragged += 1;
                if examples.len() < 10 {
                    examples.push(format!(
                        "line {line}: {} fields for {} columns",
                        record.len(),
                        codes.len()
                    ));
                }
            }
            let cell = |i: usize| record.get(i).unwrap_or("").trim();
            for (i, seen) in distinct.iter_mut().enumerate() {
                let v = cell(i);
                if let Some(n) = seen.get_mut(v) {
                    *n += 1;
                } else if seen.len() < MAX_DISTINCT {
                    seen.insert(v.to_owned(), 1);
                } else {
                    more[i] = true;
                }
            }
            if let Some(o) = obs {
                let v = cell(o);
                if v.is_empty() || v.eq_ignore_ascii_case("nan") {
                    missing += 1;
                } else if v.parse::<f64>().is_err() {
                    not_numbers += 1;
                    if examples.len() < 10 {
                        examples.push(format!("line {line}: OBS_VALUE \"{v}\" is not a number"));
                    }
                }
            }
            if let Some(t) = time {
                let v = cell(t);
                if !v.is_empty() {
                    if first_time.as_deref().is_none_or(|f| v < f) {
                        first_time = Some(v.to_owned());
                    }
                    if last_time.as_deref().is_none_or(|l| v > l) {
                        last_time = Some(v.to_owned());
                    }
                }
            }
            // One observation per series and period: the structure, the dimensions and the time.
            if obs.is_some() && !keys_full {
                let structure = match version {
                    Version::V1 => 0..1,
                    Version::V2 => 0..2,
                };
                let key: Vec<&str> = structure.chain(dims.iter().copied()).map(cell).collect();
                if keys.len() >= MAX_KEYS {
                    keys_full = true;
                } else if !keys.insert(key.join("\u{1f}")) && action.is_none_or(|a| cell(a) != "D")
                {
                    duplicates += 1;
                    if examples.len() < 10 {
                        examples.push(format!(
                            "line {line}: the same observation as an earlier line"
                        ));
                    }
                }
            }
            if table.rows.len() < MAX_ROWS {
                table.rows.push(Row {
                    cells: (0..codes.len()).map(|i| cell(i).to_owned()).collect(),
                    source: Some(start..end),
                });
            }
        }
        if count > table.rows.len() as u64 {
            table.total = Some(count);
        }

        // Dataset facts.
        let list = |i: usize| {
            let values: Vec<&str> = distinct[i]
                .keys()
                .map(String::as_str)
                .filter(|v| !v.is_empty())
                .collect();
            let shown: Vec<&str> = values.iter().take(12).copied().collect();
            let more = values.len() > shown.len() || more[i];
            format!("{}{}", shown.join(", "), if more { ", …" } else { "" })
        };
        match version {
            Version::V1 => out.fact("Dataset", "Dataflow", list(0), None),
            Version::V2 => {
                out.fact("Dataset", "Structure type", list(0), None);
                out.fact("Dataset", "Structure", list(1), None);
            }
        }
        out.fact("Dataset", "Observations", count.to_string(), None);
        if let (Some(a), Some(b)) = (&first_time, &last_time) {
            out.fact(
                "Dataset",
                "Time periods",
                if a == b {
                    a.clone()
                } else {
                    format!("{a} to {b}")
                },
                None,
            );
        }
        if obs.is_some() {
            out.fact("Dataset", "Missing values", missing.to_string(), None);
        }
        if let Some(a) = action {
            for (v, n) in &distinct[a] {
                let name = match v.as_str() {
                    "I" => "I (information)",
                    "A" => "A (append)",
                    "R" => "R (replace)",
                    "D" => "D (delete)",
                    "M" => "M (merge)",
                    "" => "(none)",
                    other => other,
                };
                out.fact("Actions", name, n.to_string(), None);
            }
        }
        let names = |idx: &[usize]| {
            idx.iter()
                .map(|&i| codes[i].clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.fact("Columns", "Dimensions", names(&dims), None);
        if let Some(o) = obs {
            out.fact("Columns", "Measure", codes[o].clone(), None);
        }
        out.fact("Columns", "Attributes", names(&attrs), None);
        for &i in &dims {
            let n = distinct[i].len();
            let label = columns[i]
                .split_once(':')
                .map(|(_, l)| l.trim())
                .filter(|l| !l.is_empty());
            out.fact(
                "Dimensions",
                &match label {
                    Some(l) => format!("{} ({l})", codes[i]),
                    None => codes[i].clone(),
                },
                format!(
                    "{}{} distinct: {}",
                    n,
                    if more[i] { "+" } else { "" },
                    list(i)
                ),
                None,
            );
        }

        // Checks.
        let (state, detail) = match (&error, ragged) {
            (Some(e), _) => (State::Partial, format!("Reading stopped {e}")),
            (None, 0) if input.complete => (State::Passed, format!("{count} observations")),
            (None, 0) => (State::Partial, "Only the start of the file was read".into()),
            (None, r) => (
                State::Partial,
                format!("{r} lines do not have one field per column"),
            ),
        };
        out.check(Stage::Parsed, state, detail);
        out.check(
            Stage::Schema,
            State::NotDone,
            "Not checked against the data structure definition (DSD), which is not in the file",
        );
        let semantic = if obs.is_none() {
            (
                State::NotDone,
                "No OBS_VALUE column: values were not checked".to_owned(),
            )
        } else if not_numbers + duplicates == 0 {
            (
                if keys_full {
                    State::Partial
                } else {
                    State::Passed
                },
                if keys_full {
                    "Observation values are numbers; duplicates were checked in the first 2 million lines"
                        .to_owned()
                } else {
                    "Observation values are numbers or missing; no observation appears twice"
                        .to_owned()
                },
            )
        } else {
            (
                State::Failed,
                format!(
                    "{not_numbers} values are not numbers; {duplicates} observations appear twice"
                ),
            )
        };
        out.check(Stage::Semantic, semantic.0, semantic.1);
        if !examples.is_empty() {
            out.notes
                .push(format!("Problems found: {}", examples.join("; ")));
        }
        out.notes.push(
            "Codes are shown as written; their names come from the data structure definition, which is not fetched"
                .into(),
        );
        out.tables.push(table);
        Ok(out)
    }
}
