//! The French Fichier des Écritures Comptables (FEC, article A47 A-1 of the Livre des procédures
//! fiscales): one line per accounting entry line, 18 columns separated by `|` or a tab, amounts
//! with a comma, dates as YYYYMMDD. Identifiers (account numbers, entry numbers) are kept exactly
//! as written. Each entry must balance (debits = credits), and so must the whole file.
use super::common::{self, Capped, Dec, Legacy, Tally, span};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};
use std::collections::{BTreeMap, HashMap};

/// The 18 columns, in the order the regulation gives them.
pub const COLUMNS: [&str; 18] = [
    "JournalCode",
    "JournalLib",
    "EcritureNum",
    "EcritureDate",
    "CompteNum",
    "CompteLib",
    "CompAuxNum",
    "CompAuxLib",
    "PieceRef",
    "PieceDate",
    "EcritureLib",
    "Debit",
    "Credit",
    "EcritureLet",
    "DateLet",
    "ValidDate",
    "Montantdevise",
    "Idevise",
];
/// Columns that must not be empty.
const REQUIRED: [&str; 10] = [
    "JournalCode",
    "JournalLib",
    "EcritureNum",
    "EcritureDate",
    "CompteNum",
    "CompteLib",
    "PieceRef",
    "PieceDate",
    "EcritureLib",
    "ValidDate",
];
/// Problem lines listed by number in a check.
const SHOWN: usize = 5;

pub struct Fec;

/// The header's separator, when the first line is an FEC header.
fn header_separator(line: &[u8]) -> Option<u8> {
    let lower = line.to_ascii_lowercase();
    if !lower.starts_with(b"journalcode") {
        return None;
    }
    let sep = *line.get(b"journalcode".len())?;
    if sep != b'|' && sep != b'\t' {
        return None;
    }
    let has = |name: &[u8]| lower.split(|&c| c == sep).any(|c| c.trim_ascii() == name);
    (has(b"journallib") && has(b"ecriturenum")).then_some(sep)
}

impl Handler for Fec {
    fn name(&self) -> &'static str {
        "FEC"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let head = probe
            .head
            .get(common::bom(probe.head)..)
            .unwrap_or_default();
        let first = common::lines_from(head, 0)
            .next()
            .map(|(_, l)| l)
            .unwrap_or_default();
        header_separator(first).is_some()
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        let mut lines = common::lines_from(b, common::bom(b));
        let (_, header) = lines.next().ok_or("Empty file")?;
        let sep = header_separator(header).ok_or("No FEC header line")?;
        let dec = |f: &[u8]| common::decode(f, Legacy::Latin9).trim().to_owned();
        let names: Vec<String> = header.split(|&c| c == sep).map(dec).collect();
        let col = |name: &str| names.iter().position(|n| n.eq_ignore_ascii_case(name));
        let i_journal = col("JournalCode");
        let i_journal_lib = col("JournalLib");
        let i_num = col("EcritureNum");
        let i_date = col("EcritureDate");
        let i_account = col("CompteNum");
        let i_debit = col("Debit");
        let i_credit = col("Credit");
        let i_amount = col("Montant");
        let i_sense = col("Sens");
        let i_currency = col("Idevise");
        let signed_layout = i_debit.is_none() && i_amount.is_some() && i_sense.is_some();
        let mut out = Inspection::new("FEC", "FEC — Fichier des écritures comptables");
        out.version = Some(if signed_layout {
            "Article A47 A-1 LPF (Montant/Sens layout)".into()
        } else {
            "Article A47 A-1 LPF".into()
        });
        let cols: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut table = Capped::new("Entries", &cols);
        let mut entries: HashMap<(String, String), EntryTotals> = HashMap::new();
        let mut order: Vec<(String, String)> = vec![];
        let mut journals: BTreeMap<String, JournalTotals> = BTreeMap::new();
        let (mut total_debit, mut total_credit) = (Some(Dec::ZERO), Some(Dec::ZERO));
        let mut first_date: Option<String> = None;
        let mut last_date: Option<String> = None;
        let mut accounts: std::collections::HashSet<String> = Default::default();
        let mut currencies: std::collections::BTreeSet<String> = Default::default();
        let mut wrong_width: Vec<usize> = vec![];
        let mut wrong_width_count = 0usize;
        let mut bad_amount: Vec<usize> = vec![];
        let mut bad_amount_count = 0usize;
        let mut bad_date: Vec<usize> = vec![];
        let mut bad_date_count = 0usize;
        let mut missing: Vec<usize> = vec![];
        let mut missing_count = 0usize;
        let mut problems = vec![];
        let date_cols: Vec<(usize, bool)> = [
            ("EcritureDate", true),
            ("PieceDate", true),
            ("ValidDate", true),
            ("DateLet", false),
        ]
        .iter()
        .filter_map(|(n, req)| col(n).map(|i| (i, *req)))
        .collect();
        let required: Vec<usize> = REQUIRED.iter().filter_map(|n| col(n)).collect();
        let note = |list: &mut Vec<usize>, count: &mut usize, line: usize| {
            *count += 1;
            if list.len() < SHOWN {
                list.push(line);
            }
        };
        let mut lines_read = 0usize;
        for (i, (at, line)) in lines.enumerate() {
            if i % 8192 == 0 && cancelled() {
                problems.push("Cancelled".into());
                break;
            }
            if line.trim_ascii().is_empty() {
                continue;
            }
            let line_no = i + 2;
            let raw: Vec<&[u8]> = line.split(|&c| c == sep).collect();
            if raw.len() != names.len() {
                note(&mut wrong_width, &mut wrong_width_count, line_no);
                continue;
            }
            lines_read += 1;
            let cells: Vec<String> = raw
                .iter()
                .map(|f| common::decode(f, Legacy::Latin9).into_owned())
                .collect();
            let get =
                |i: Option<usize>| i.and_then(|i| cells.get(i)).map(|s| s.trim()).unwrap_or("");
            // Amounts.
            let (debit, credit) = if signed_layout {
                let a = parse_amount(get(i_amount));
                match (a, get(i_sense).to_ascii_uppercase().as_str()) {
                    (Some(a), "D" | "+1" | "1") => (Some(a), Some(Dec::ZERO)),
                    (Some(a), "C" | "-1") => (Some(Dec::ZERO), Some(a)),
                    _ => (None, None),
                }
            } else {
                (parse_amount(get(i_debit)), parse_amount(get(i_credit)))
            };
            let (Some(debit), Some(credit)) = (debit, credit) else {
                note(&mut bad_amount, &mut bad_amount_count, line_no);
                if table.full() {
                    table.skip();
                } else {
                    table.push(cells.clone(), span(at, line.len()));
                }
                continue;
            };
            for &(c, req) in &date_cols {
                let v = cells.get(c).map(|s| s.trim()).unwrap_or("");
                if (req || !v.is_empty()) && common::yyyymmdd(v).is_none() {
                    note(&mut bad_date, &mut bad_date_count, line_no);
                    break;
                }
            }
            if required
                .iter()
                .any(|&c| cells.get(c).is_none_or(|v| v.trim().is_empty()))
            {
                note(&mut missing, &mut missing_count, line_no);
            }
            let journal = get(i_journal).to_owned();
            let key = (journal.clone(), get(i_num).to_owned());
            let e = entries.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                EntryTotals {
                    debit: Some(Dec::ZERO),
                    credit: Some(Dec::ZERO),
                    first_line: line_no,
                }
            });
            e.debit = e.debit.and_then(|d| d.checked_add(debit));
            e.credit = e.credit.and_then(|c| c.checked_add(credit));
            total_debit = total_debit.and_then(|d| d.checked_add(debit));
            total_credit = total_credit.and_then(|c| c.checked_add(credit));
            let j = journals.entry(journal).or_insert_with(|| JournalTotals {
                label: get(i_journal_lib).to_owned(),
                entries: 0,
                lines: 0,
                debit: Some(Dec::ZERO),
                credit: Some(Dec::ZERO),
            });
            j.lines += 1;
            j.debit = j.debit.and_then(|d| d.checked_add(debit));
            j.credit = j.credit.and_then(|c| c.checked_add(credit));
            let date = get(i_date);
            if common::yyyymmdd(date).is_some() {
                if first_date.as_deref().is_none_or(|f| date < f) {
                    first_date = Some(date.to_owned());
                }
                if last_date.as_deref().is_none_or(|l| date > l) {
                    last_date = Some(date.to_owned());
                }
            }
            if accounts.len() < 1_000_000 {
                accounts.insert(get(i_account).to_owned());
            }
            let cur = get(i_currency);
            if !cur.is_empty() && currencies.len() < 100 {
                currencies.insert(cur.to_owned());
            }
            if table.full() {
                table.skip();
            } else {
                table.push(cells, span(at, line.len()));
            }
        }
        for (j, _) in &order {
            if let Some(t) = journals.get_mut(j) {
                t.entries += 1;
            }
        }
        // Facts.
        out.fact(
            "File",
            "Separator",
            if sep == b'|' { "| (pipe)" } else { "tab" },
            None,
        );
        if let Some((siren, closing)) = file_name_parts(input.path) {
            out.fact("File", "SIREN (from the file name)", siren, None);
            out.fact(
                "File",
                "Closing date (from the file name)",
                common::yyyymmdd(&closing).unwrap_or(closing),
                None,
            );
        }
        out.fact("Entries", "Entries", order.len().to_string(), None);
        out.fact("Entries", "Lines", lines_read.to_string(), None);
        if let (Some(a), Some(z)) = (&first_date, &last_date) {
            out.fact(
                "Entries",
                "Period (entry dates)",
                format!(
                    "{} to {}",
                    common::yyyymmdd(a).unwrap_or_default(),
                    common::yyyymmdd(z).unwrap_or_default()
                ),
                None,
            );
        }
        out.fact("Entries", "Accounts used", accounts.len().to_string(), None);
        if let Some(d) = total_debit {
            out.fact("Entries", "Total debit", d.format(2), None);
        }
        if let Some(c) = total_credit {
            out.fact("Entries", "Total credit", c.format(2), None);
        }
        if !currencies.is_empty() {
            out.fact(
                "Entries",
                "Currencies (Idevise)",
                currencies.iter().cloned().collect::<Vec<_>>().join(", "),
                None,
            );
        }
        let list: Vec<String> = journals
            .iter()
            .take(100)
            .map(|(code, j)| {
                if j.label.is_empty() {
                    format!("{code} ({})", j.entries)
                } else {
                    format!("{code} {} ({})", j.label, j.entries)
                }
            })
            .collect();
        out.fact("Entries", "Journals", list.join(", "), None);
        let mut jt = Capped::new(
            "Journals",
            &[
                "JournalCode",
                "JournalLib",
                "Entries",
                "Lines",
                "Debit",
                "Credit",
            ],
        );
        for (code, j) in &journals {
            jt.push(
                vec![
                    code.clone(),
                    j.label.clone(),
                    j.entries.to_string(),
                    j.lines.to_string(),
                    j.debit.map(|d| d.format(2)).unwrap_or_default(),
                    j.credit.map(|c| c.format(2)).unwrap_or_default(),
                ],
                None,
            );
        }
        table.finish(&mut out);
        jt.finish(&mut out);
        // Checks.
        let lines_note = |what: &str, list: &[usize], count: usize| {
            let shown: Vec<String> = list.iter().map(usize::to_string).collect();
            format!(
                "{count} {what} (line{} {}{})",
                if list.len() > 1 { "s" } else { "" },
                shown.join(", "),
                if count > list.len() { ", …" } else { "" }
            )
        };
        if wrong_width_count > 0 {
            problems.push(format!(
                "{} were skipped",
                lines_note(
                    &format!("line(s) without exactly {} fields", names.len()),
                    &wrong_width,
                    wrong_width_count
                )
            ));
        }
        if bad_amount_count > 0 {
            problems.push(format!(
                "{} are listed but left out of the totals",
                lines_note(
                    "line(s) with an unreadable amount",
                    &bad_amount,
                    bad_amount_count
                )
            ));
        }
        common::parsed(&mut out, "Entry lines read", &problems);
        let standard = names.len() >= 18
            && names
                .iter()
                .zip(COLUMNS.iter())
                .all(|(a, b)| a.eq_ignore_ascii_case(b));
        let mut schema_issues = vec![];
        if !standard && !signed_layout {
            let missing_cols: Vec<&str> = COLUMNS
                .iter()
                .copied()
                .filter(|c| col(c).is_none())
                .collect();
            schema_issues.push(if missing_cols.is_empty() {
                "the columns are not in the regulatory order".to_owned()
            } else {
                format!("missing columns: {}", missing_cols.join(", "))
            });
        }
        if bad_date_count > 0 {
            schema_issues.push(lines_note(
                "line(s) with a date that is not YYYYMMDD",
                &bad_date,
                bad_date_count,
            ));
        }
        if missing_count > 0 {
            schema_issues.push(lines_note(
                "line(s) with an empty required field",
                &missing,
                missing_count,
            ));
        }
        if bad_amount_count > 0 {
            schema_issues.push(format!(
                "{bad_amount_count} line(s) with an unreadable amount"
            ));
        }
        if schema_issues.is_empty() {
            out.check(
                Stage::Schema,
                State::Partial,
                "Column names and order, YYYYMMDD dates, amounts and required fields checked; the account plan, entry numbering and file encoding are not",
            );
        } else {
            out.check(Stage::Schema, State::Failed, schema_issues.join("; "));
        }
        let mut balanced = Tally::new("Entries balanced (debits = credits)");
        for key in &order {
            let Some(e) = entries.get(key) else {
                continue;
            };
            let (Some(d), Some(c)) = (e.debit, e.credit) else {
                continue;
            };
            balanced.add(d == c, || {
                format!(
                    "{} {} (line {}): debit {}, credit {}",
                    key.0,
                    key.1,
                    e.first_line,
                    d.format(2),
                    c.format(2)
                )
            });
        }
        balanced.report(&mut out);
        if let (Some(d), Some(c)) = (total_debit, total_credit)
            && lines_read > 0
        {
            out.check(
                Stage::Semantic,
                if d == c { State::Passed } else { State::Failed },
                format!(
                    "File balanced: total debit {}, total credit {}",
                    d.format(2),
                    c.format(2)
                ),
            );
        }
        out.notes.push(
            "Entries are identified by journal code and entry number; values are shown as written"
                .into(),
        );
        out.notes.push(
            "Text that is not UTF-8 is read as ISO 8859-15, the encoding the regulation names"
                .into(),
        );
        Ok(out)
    }
}

struct EntryTotals {
    debit: Option<Dec>,
    credit: Option<Dec>,
    first_line: usize,
}

struct JournalTotals {
    label: String,
    entries: u64,
    lines: u64,
    debit: Option<Dec>,
    credit: Option<Dec>,
}

/// An FEC amount: comma (or point) decimal, optional sign and spaces; empty is zero.
fn parse_amount(s: &str) -> Option<Dec> {
    if s.trim().is_empty() {
        return Some(Dec::ZERO);
    }
    common::loose_decimal(s, true)
}

/// "123456789FEC20251231" in the file name: SIREN and closing date.
fn file_name_parts(path: &std::path::Path) -> Option<(String, String)> {
    let stem = path.file_stem()?.to_string_lossy().to_uppercase();
    let at = stem.find("FEC")?;
    let siren = stem.get(at.checked_sub(9)?..at)?;
    let date = stem.get(at + 3..at + 11)?;
    (siren.bytes().all(|c| c.is_ascii_digit()) && date.bytes().all(|c| c.is_ascii_digit()))
        .then(|| (siren.to_owned(), date.to_owned()))
}
