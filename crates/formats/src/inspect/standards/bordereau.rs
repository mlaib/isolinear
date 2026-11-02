//! Insurance bordereaux: premium and claims listings exchanged between coverholders, brokers
//! and insurers (Lloyd's and company market), as CSV or Excel. A file is recognized when its
//! header row names several typical bordereau columns (policy number, insured, inception and
//! expiry, premiums, claim number, paid, reserve, currency, UMR…), or when a mapping file
//! `<name>.bordereau.toml` sits next to it:
//!
//! ```toml
//! [columns]
//! "Policy Ref" = "policy_number"
//! "GWP" = "gross_premium"
//! ```
//!
//! The column meanings are shown as facts, so the reader sees the interpretation. Premium and
//! claim amounts are added up exactly (by currency when there is a currency column), date
//! ranges are given, and values that are not amounts or dates are listed.
use super::doc_common::{self, SimpleToml, clip, push_row, sidecar, table};
use crate::inspect::{
    Handler, Input, Inspection, MAX_ROWS, Probe, Stage, State, spec::decimal_sum,
};
use std::ops::Range;
use std::path::{Path, PathBuf};

const SUFFIX: &str = ".bordereau.toml";
/// A row's cells and its bytes in the file (CSV).
type SourceRow = (Vec<String>, Option<Range<u64>>);
const MAX_COLUMNS: usize = 1000;
const MAX_PROBLEMS_LISTED: usize = 10_000;
/// Rows searched for the header (title rows may come first).
const HEADER_SEARCH: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Value {
    Amount,
    Date,
    Text,
}

struct Meaning {
    id: &'static str,
    label: &'static str,
    strong: bool,
    value: Value,
    names: &'static [&'static str],
}

const fn m(
    id: &'static str,
    label: &'static str,
    strong: bool,
    value: Value,
    names: &'static [&'static str],
) -> Meaning {
    Meaning {
        id,
        label,
        strong,
        value,
        names,
    }
}

static MEANINGS: &[Meaning] = &[
    m(
        "policy_number",
        "Policy number",
        true,
        Value::Text,
        &[
            "policy number",
            "policy no",
            "policy ref",
            "policy reference",
            "policy id",
            "certificate number",
            "certificate no",
            "certificate ref",
            "certificate reference",
            "unique risk reference",
            "risk reference",
        ],
    ),
    m(
        "insured",
        "Insured",
        true,
        Value::Text,
        &[
            "insured",
            "insured name",
            "name of insured",
            "policyholder",
            "policy holder",
            "assured",
            "assured name",
            "insured party",
        ],
    ),
    m(
        "inception",
        "Inception date",
        true,
        Value::Date,
        &[
            "inception",
            "inception date",
            "effective date",
            "start date",
            "period from",
            "risk inception",
            "risk inception date",
            "cover start",
            "cover start date",
            "policy start date",
        ],
    ),
    m(
        "expiry",
        "Expiry date",
        true,
        Value::Date,
        &[
            "expiry",
            "expiry date",
            "expiration date",
            "end date",
            "period to",
            "risk expiry",
            "risk expiry date",
            "cover end",
            "cover end date",
            "policy end date",
        ],
    ),
    m(
        "period",
        "Reporting period",
        false,
        Value::Text,
        &[
            "reporting period",
            "bordereau period",
            "period",
            "reporting month",
            "accounting period",
        ],
    ),
    m(
        "gross_premium",
        "Gross premium",
        true,
        Value::Amount,
        &[
            "gross premium",
            "gwp",
            "gross written premium",
            "premium gross",
            "gross premium written",
            "original gross premium",
        ],
    ),
    m(
        "net_premium",
        "Net premium",
        true,
        Value::Amount,
        &[
            "net premium",
            "nwp",
            "net written premium",
            "premium net",
            "net premium to underwriters",
            "net to underwriters",
        ],
    ),
    m(
        "premium",
        "Premium",
        true,
        Value::Amount,
        &[
            "premium",
            "written premium",
            "premium amount",
            "total premium",
            "annual premium",
        ],
    ),
    m(
        "commission",
        "Commission",
        false,
        Value::Amount,
        &[
            "commission",
            "brokerage",
            "commission amount",
            "broker commission",
            "coverholder commission",
        ],
    ),
    m(
        "tax",
        "Premium tax",
        false,
        Value::Amount,
        &[
            "ipt",
            "premium tax",
            "insurance premium tax",
            "tax",
            "taxes",
        ],
    ),
    m(
        "claim_number",
        "Claim number",
        true,
        Value::Text,
        &[
            "claim number",
            "claim no",
            "claim ref",
            "claim reference",
            "claim id",
            "claims reference",
        ],
    ),
    m(
        "loss_date",
        "Date of loss",
        true,
        Value::Date,
        &["date of loss", "loss date", "loss occurrence date"],
    ),
    m(
        "notified",
        "Date notified",
        false,
        Value::Date,
        &["date notified", "notification date", "date reported"],
    ),
    m(
        "paid",
        "Paid",
        true,
        Value::Amount,
        &[
            "paid",
            "paid amount",
            "claims paid",
            "paid to date",
            "total paid",
            "indemnity paid",
            "amount paid",
            "paid this period",
        ],
    ),
    m(
        "reserve",
        "Outstanding reserve",
        true,
        Value::Amount,
        &[
            "reserve",
            "reserves",
            "outstanding",
            "outstanding reserve",
            "os reserve",
            "outstanding amount",
            "outstanding claims",
        ],
    ),
    m(
        "incurred",
        "Incurred",
        true,
        Value::Amount,
        &["incurred", "total incurred", "incurred amount"],
    ),
    m(
        "currency",
        "Currency",
        false,
        Value::Text,
        &[
            "currency",
            "ccy",
            "settlement currency",
            "original currency",
            "currency code",
        ],
    ),
    m(
        "line_of_business",
        "Line of business",
        false,
        Value::Text,
        &[
            "line of business",
            "lob",
            "class of business",
            "class",
            "risk code",
            "section",
        ],
    ),
    m(
        "umr",
        "Unique market reference (UMR)",
        true,
        Value::Text,
        &[
            "umr",
            "unique market reference",
            "binder ref",
            "binder reference",
            "binding authority",
            "binding authority reference",
            "contract reference",
            "agreement number",
        ],
    ),
    m(
        "coverholder",
        "Coverholder",
        true,
        Value::Text,
        &["coverholder", "cover holder", "coverholder name"],
    ),
    m(
        "sum_insured",
        "Sum insured",
        false,
        Value::Amount,
        &[
            "sum insured",
            "total sum insured",
            "tsi",
            "limit",
            "limit of indemnity",
        ],
    ),
    m(
        "country",
        "Risk country",
        false,
        Value::Text,
        &["country", "risk country", "risk location", "territory"],
    ),
    m(
        "transaction_type",
        "Transaction type",
        false,
        Value::Text,
        &["transaction type", "transaction"],
    ),
    m(
        "status",
        "Claim status",
        false,
        Value::Text,
        &["claim status", "status"],
    ),
    m(
        "year_of_account",
        "Year of account",
        true,
        Value::Text,
        &["year of account", "yoa", "underwriting year"],
    ),
];

/// A header as compared: lower case, words only, a bracketed note and a trailing currency
/// code or "amount" dropped ("Gross Premium (GBP)" → "gross premium").
fn normalize(header: &str) -> String {
    let base = header.split(['(', '[']).next().unwrap_or(header);
    let words: Vec<String> = base
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut words = words;
    while words.len() > 1 {
        let last = words.last().map(String::as_str).unwrap_or("");
        let currency = last.len() == 3
            && [
                "gbp", "eur", "usd", "chf", "cad", "aud", "jpy", "orig", "sett",
            ]
            .contains(&last);
        if currency || matches!(last, "amount" | "amt" | "value" | "total") && words.len() > 2 {
            words.pop();
        } else {
            break;
        }
    }
    words.join(" ")
}

fn meaning_of(header: &str) -> Option<&'static Meaning> {
    let n = normalize(header);
    MEANINGS.iter().find(|m| m.names.contains(&n.as_str()))
}

/// Columns mapped to meanings, each meaning once (the first column wins).
fn detect(headers: &[String]) -> Vec<(usize, &'static Meaning)> {
    let mut out: Vec<(usize, &'static Meaning)> = vec![];
    for (i, h) in headers.iter().enumerate().take(MAX_COLUMNS) {
        if let Some(m) = meaning_of(h)
            && !out.iter().any(|(_, x)| x.id == m.id)
        {
            out.push((i, m));
        }
    }
    out
}

/// Several typical columns, at least three of them strong ones.
fn convincing(mapping: &[(usize, &Meaning)]) -> bool {
    mapping.len() >= 4 && mapping.iter().filter(|(_, m)| m.strong).count() >= 3
}

fn mapping_path(path: &Path) -> Option<PathBuf> {
    sidecar(path, SUFFIX)
}

/// The mapping file's columns: header text → meaning.
fn read_mapping(path: &Path) -> Result<Vec<(String, &'static Meaning)>, String> {
    let source = doc_common::read_sidecar(path)?;
    let toml = SimpleToml::parse(&source)?;
    let mut out = vec![];
    for (header, _, pairs) in &toml.sections {
        if header != "columns" {
            continue;
        }
        for (column, value) in pairs {
            let want = value
                .as_str()
                .ok_or_else(|| format!("{column:?} must map to a text value"))?;
            let m = MEANINGS
                .iter()
                .find(|m| m.id == want || m.label.eq_ignore_ascii_case(want))
                .ok_or_else(|| {
                    format!(
                        "{want:?} is not a known meaning (known: {})",
                        MEANINGS.iter().map(|m| m.id).collect::<Vec<_>>().join(", ")
                    )
                })?;
            out.push((column.clone(), m));
        }
    }
    if out.is_empty() {
        return Err("the mapping file has no [columns] entries".into());
    }
    Ok(out)
}

/// The best header row among the first rows, and its mapping.
fn header_row(
    rows: &[(Vec<String>, Option<Range<u64>>)],
) -> Option<(usize, Vec<(usize, &'static Meaning)>)> {
    rows.iter()
        .take(HEADER_SEARCH)
        .enumerate()
        .map(|(i, (cells, _))| (i, detect(cells)))
        .max_by_key(|(i, m)| (m.len(), std::cmp::Reverse(*i)))
}

// ----- reading the rows -----

fn csv_rows(bytes: &[u8], cancelled: &dyn Fn() -> bool) -> Result<Vec<SourceRow>, String> {
    let bom = if bytes.starts_with(b"\xEF\xBB\xBF") {
        3
    } else {
        0
    };
    let data = &bytes[bom..];
    let dialect = crate::csv::sniff(&data[..data.len().min(crate::inspect::PROBE)], None);
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(dialect.delimiter)
        .quote(dialect.quote)
        .from_reader(data);
    let mut out = vec![];
    let mut record = csv::ByteRecord::new();
    loop {
        if out.len() % 65_536 == 0 && cancelled() {
            return Err("Cancelled".into());
        }
        let start = rdr.position().byte();
        match rdr.read_byte_record(&mut record) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => return Err(format!("CSV error after {} rows: {e}", out.len())),
        }
        let end = rdr.position().byte();
        let cells: Vec<String> = record
            .iter()
            .take(MAX_COLUMNS)
            .map(|f| String::from_utf8_lossy(f).trim().to_owned())
            .collect();
        out.push((cells, Some(start + bom as u64..end + bom as u64)));
        if out.len() > MAX_ROWS + HEADER_SEARCH {
            break;
        }
    }
    Ok(out)
}

fn xlsx_rows(
    bytes: &[u8],
    cancelled: &dyn Fn() -> bool,
) -> Result<(String, Vec<SourceRow>, bool), String> {
    let (sheet, rows, cut) = crate::table::xlsx::sheet_rows(bytes, cancelled)?;
    let out = rows
        .into_iter()
        .take(MAX_ROWS + HEADER_SEARCH)
        .map(|(_, cells)| {
            let width = cells
                .last()
                .map_or(0, |c| c.0 as usize + 1)
                .min(MAX_COLUMNS);
            let mut dense = vec![String::new(); width];
            for (c, t) in cells {
                if (c as usize) < width {
                    dense[c as usize] = t.trim().to_owned();
                }
            }
            (dense, None)
        })
        .collect();
    Ok((sheet, out, cut))
}

/// Strings of a workbook's shared strings and first sheet that lie in the probe (for a cheap look
/// at the header row).
fn xlsx_head_strings(head: &[u8]) -> Vec<String> {
    let mut out = vec![];
    for (name, method, data) in doc_common::head_entries(head) {
        let n = name.to_ascii_lowercase();
        if !(n == "xl/sharedstrings.xml" || n == "xl/worksheets/sheet1.xml") || data.is_empty() {
            continue;
        }
        let xml = match method {
            0 => data.to_vec(),
            8 => match doc_common::inflate(data, 1024 * 1024, true) {
                Ok(x) => x,
                Err(_) => continue,
            },
            _ => continue,
        };
        let text = String::from_utf8_lossy(&xml);
        for piece in text.split("<t").skip(1).take(10_000) {
            if let Some(start) = piece.find('>')
                && let Some(end) = piece[start..].find("</t>")
            {
                out.push(piece[start + 1..start + end].replace("&amp;", "&"));
            }
        }
    }
    out
}

pub struct Bordereau;
impl Handler for Bordereau {
    fn name(&self) -> &'static str {
        "Insurance bordereau"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let csv = probe.ext_is(&["csv"]) || matches!(probe.format, "csv" | "tsv");
        let xlsx = probe.ext_is(&["xlsx", "xlsm"]) && probe.head.starts_with(b"PK\x03\x04");
        if !csv && !xlsx {
            return false;
        }
        if mapping_path(probe.path).is_some() {
            return true;
        }
        if xlsx {
            return convincing(&detect(&xlsx_head_strings(probe.head)));
        }
        // The probe may end in the middle of a row; only whole rows are looked at.
        let head = match probe.head.iter().rposition(|&b| b == b'\n') {
            Some(i) => &probe.head[..i + 1],
            None => probe.head,
        };
        csv_rows(head, &|| false)
            .ok()
            .and_then(|rows| header_row(&rows))
            .is_some_and(|(_, m)| convincing(&m))
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let is_xlsx = input.bytes.starts_with(b"PK\x03\x04");
        let (sheet, rows, cut) = if is_xlsx {
            let (s, r, c) = xlsx_rows(input.bytes, cancelled)?;
            (Some(s), r, c)
        } else {
            (None, csv_rows(input.bytes, cancelled)?, false)
        };
        let sidecar = mapping_path(input.path);
        let (header_index, mapping, from_file) = match &sidecar {
            Some(p) => {
                let wanted = read_mapping(p)
                    .map_err(|e| format!("The mapping file {} cannot be used: {e}", p.display()))?;
                // The header row is the first row that names a mapped column.
                let index = rows
                    .iter()
                    .take(HEADER_SEARCH)
                    .position(|(cells, _)| {
                        wanted
                            .iter()
                            .any(|(c, _)| cells.iter().any(|h| h.eq_ignore_ascii_case(c)))
                    })
                    .ok_or("No row among the first ten names a column of the mapping file")?;
                let headers = &rows[index].0;
                let mut mapping = vec![];
                let mut missing = vec![];
                for (column, m) in wanted {
                    match headers.iter().position(|h| h.eq_ignore_ascii_case(&column)) {
                        Some(i) => mapping.push((i, m)),
                        None => missing.push(column),
                    }
                }
                (index, mapping, Some((p.clone(), missing)))
            }
            None => {
                let (i, m) = header_row(&rows).ok_or("The file has no rows")?;
                if !convincing(&m) {
                    return Err("The header row does not name enough bordereau columns".into());
                }
                (i, m, None)
            }
        };
        let headers = rows[header_index].0.clone();
        let has = |id: &str| mapping.iter().any(|(_, m)| m.id == id);
        let claims = has("claim_number") || has("paid") || has("reserve") || has("incurred");
        let premium = has("gross_premium") || has("net_premium") || has("premium");
        let kind = match (premium, claims) {
            (true, true) => "Premium and claims bordereau",
            (false, true) => "Claims bordereau",
            _ => "Premium bordereau",
        };
        let mut out = Inspection::new("Insurance bordereau", kind);
        out.fact(
            "File",
            "Format",
            if is_xlsx { "Excel workbook" } else { "CSV" },
            None,
        );
        if let Some(s) = &sheet {
            out.fact("File", "Sheet", s.clone(), None);
        }
        out.fact(
            "File",
            "Header row",
            (header_index + 1).to_string(),
            rows[header_index].1.clone(),
        );
        let source = match &from_file {
            Some((p, _)) => format!(
                "from {}",
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            None => "detected from the header".into(),
        };
        for (i, m) in &mapping {
            out.fact(
                "Interpretation",
                &headers[*i],
                format!("{} ({source})", m.label),
                rows[header_index].1.clone(),
            );
        }
        let unmapped: Vec<&str> = headers
            .iter()
            .enumerate()
            .filter(|(i, h)| !h.is_empty() && !mapping.iter().any(|(c, _)| c == i))
            .map(|(_, h)| h.as_str())
            .collect();
        if !unmapped.is_empty() {
            out.fact(
                "Interpretation",
                "Columns without a meaning",
                clip(&unmapped.join(", "), 500),
                None,
            );
        }
        if let Some((_, missing)) = &from_file
            && !missing.is_empty()
        {
            out.notes.push(format!(
                "The mapping file names columns the file does not have: {}",
                missing.join(", ")
            ));
        }

        let mut data = table(
            "Rows",
            &headers.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        data.columns.insert(0, "Row".into());
        let mut problems = table("Problems", &["Row", "Column", "Value", "Problem"]);
        let mut problem_count = 0usize;
        let currency_col = mapping
            .iter()
            .find(|(_, m)| m.id == "currency")
            .map(|(c, _)| *c);
        // (meaning id, currency) → values
        let mut amounts: Vec<(&'static str, &'static str, String, Vec<String>)> = vec![];
        let mut dates: Vec<(&'static str, String, String)> = vec![];
        let mut ambiguous = 0usize;
        let mut row_count = 0usize;
        for (n, (cells, src)) in rows.iter().enumerate().skip(header_index + 1) {
            if cells.iter().all(String::is_empty) {
                continue;
            }
            row_count += 1;
            let line = (n + 1).to_string();
            let mut problem =
                |out: &mut crate::inspect::Table, column: &str, value: &str, what: String| {
                    problem_count += 1;
                    if out.rows.len() < MAX_PROBLEMS_LISTED {
                        push_row(
                            out,
                            vec![line.clone(), column.to_owned(), value.to_owned(), what],
                            src.clone(),
                        );
                    }
                };
            let currency = currency_col
                .and_then(|c| cells.get(c))
                .map(|c| c.to_uppercase())
                .unwrap_or_default();
            let mut inception = None;
            let mut expiry = None;
            for (c, m) in &mapping {
                let value = cells.get(*c).map(String::as_str).unwrap_or("");
                if value.is_empty() {
                    continue;
                }
                match m.value {
                    Value::Amount => match doc_common::parse_amount(value) {
                        Some(v) => {
                            match amounts.iter_mut().find(|a| a.0 == m.id && a.2 == currency) {
                                Some(a) => a.3.push(v),
                                None => amounts.push((m.id, m.label, currency.clone(), vec![v])),
                            }
                        }
                        None => problem(
                            &mut problems,
                            &headers[*c],
                            value,
                            format!("{} is not an amount", m.label),
                        ),
                    },
                    Value::Date => match doc_common::parse_date(value) {
                        Some(d) => {
                            let nums: Vec<u32> = value
                                .split(['/', '.', '-'])
                                .filter_map(|p| p.trim().parse().ok())
                                .collect();
                            if nums.len() == 3
                                && nums[0] <= 12
                                && nums[1] <= 12
                                && nums[0] != nums[1]
                                && value.contains('/')
                            {
                                ambiguous += 1;
                            }
                            match m.id {
                                "inception" => inception = Some(d.clone()),
                                "expiry" => expiry = Some(d.clone()),
                                _ => {}
                            }
                            match dates.iter_mut().find(|x| x.0 == m.label) {
                                Some(x) => {
                                    if d < x.1 {
                                        x.1 = d.clone();
                                    }
                                    if d > x.2 {
                                        x.2 = d;
                                    }
                                }
                                None => dates.push((m.label, d.clone(), d)),
                            }
                        }
                        None => problem(
                            &mut problems,
                            &headers[*c],
                            value,
                            format!("{} is not a date", m.label),
                        ),
                    },
                    Value::Text => {}
                }
            }
            if let (Some(i), Some(e)) = (&inception, &expiry)
                && i > e
            {
                problem(
                    &mut problems,
                    "Inception/expiry",
                    &format!("{i} / {e}"),
                    "inception is after expiry".into(),
                );
            }
            let mut row = vec![line.clone()];
            row.extend(cells.iter().cloned());
            row.resize(data.columns.len(), String::new());
            push_row(&mut data, row, src.clone());
            if row_count.is_multiple_of(65_536) && cancelled() {
                return Err("Cancelled".into());
            }
        }
        out.fact("Totals", "Rows", row_count.to_string(), None);
        for (id, label, currency, values) in &amounts {
            let _ = id;
            if let Some(sum) = decimal_sum(values.iter().map(String::as_str)) {
                let label = if currency.is_empty() {
                    format!("{label} total")
                } else {
                    format!("{label} total ({currency})")
                };
                out.fact("Totals", &label, sum, None);
            }
        }
        for (label, from, to) in &dates {
            out.fact(
                "Dates",
                label,
                if from == to {
                    from.clone()
                } else {
                    format!("{from} to {to}")
                },
                None,
            );
        }
        out.tables.push(data);
        out.check(
            Stage::Parsed,
            if cut { State::Partial } else { State::Passed },
            if cut {
                "Only the first rows of the sheet were read (cell budget or damage)".to_owned()
            } else {
                format!(
                    "{row_count} rows read; {} columns interpreted",
                    mapping.len()
                )
            },
        );
        out.check(
            Stage::Semantic,
            if problem_count == 0 { State::Passed } else { State::Failed },
            if problem_count == 0 {
                "Every amount and date in the interpreted columns could be read".to_owned()
            } else {
                format!("{problem_count} values are not readable amounts or dates, or dates are out of order")
            },
        );
        out.check(Stage::Schema, State::NotDone, "Not checked against a bordereau reporting standard (such as Lloyd's Coverholder Reporting Standards)");
        if problem_count > 0 {
            out.tables.push(problems);
        }
        if ambiguous > 0 {
            out.notes.push(format!("{ambiguous} dates such as 03/04/2026 could be day-first or month-first; they were read day first"));
        }
        if from_file.is_none() {
            out.notes.push("Column meanings were inferred from the header names; check the Interpretation facts, or write a .bordereau.toml mapping file next to the file to set them".into());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn header_detection() {
        let h = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let m = detect(&h(&[
            "Policy Ref",
            "Insured Name",
            "Inception Date",
            "Expiry Date",
            "Gross Premium (GBP)",
            "Currency",
        ]));
        assert_eq!(m.len(), 6);
        assert!(convincing(&m));
        assert_eq!(normalize("Gross Premium (GBP)"), "gross premium");
        assert_eq!(normalize("Paid Amount"), "paid amount");
        // An ordinary table is not a bordereau.
        assert!(!convincing(&detect(&h(&["id", "name", "zip", "amount"]))));
        assert!(!convincing(&detect(&h(&[
            "Date",
            "Description",
            "Amount",
            "Currency",
            "Status"
        ]))));
    }
}
