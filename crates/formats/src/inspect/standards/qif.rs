//! QIF (Quicken Interchange Format): sections introduced by `!Type:…` (or `!Account`), records
//! of one-letter fields ended by `^`. Bank, cash, credit card and asset/liability transactions
//! with their splits, investment transactions, and the account, category and class lists.
use super::common::{self, Capped, Dec, Tally, span};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};
use std::collections::BTreeMap;

/// Fields kept per record.
const MAX_FIELDS: usize = 1000;

pub struct Qif;

fn header_line(head: &[u8]) -> bool {
    let first = common::lines_from(head, 0)
        .next()
        .map(|(_, l)| l)
        .unwrap_or_default();
    let lower = first.to_ascii_lowercase();
    lower.starts_with(b"!type:")
        || lower.starts_with(b"!account")
        || lower.starts_with(b"!option:")
        || lower.starts_with(b"!clear:")
}

impl Handler for Qif {
    fn name(&self) -> &'static str {
        "QIF"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        header_line(common::trimmed_head(probe.head))
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        let mut out = Inspection::new("QIF", "QIF — Quicken interchange file");
        let mut section = Section::None;
        let mut section_name = String::new();
        let mut in_account_list = false;
        let mut account = String::new();
        let mut record: Vec<(u8, String, usize, usize)> = vec![];
        let mut record_start = 0usize;
        let mut problems = vec![];
        let mut unknown_sections: BTreeMap<String, u64> = BTreeMap::new();
        let mut skipped = 0u64;
        let mut tables = Tables::new();
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        let mut net: BTreeMap<String, Option<Dec>> = BTreeMap::new();
        let mut first_date = String::new();
        let mut last_date = String::new();
        let mut splits_check = Tally::new("Split amounts add up to the transaction amount");
        let mut bad_amounts = 0u64;
        let mut n_tx = 0u64;
        let mut flush = |record: &mut Vec<(u8, String, usize, usize)>,
                         start: usize,
                         end: usize,
                         section: Section,
                         section_name: &str,
                         in_account_list: bool,
                         account: &mut String| {
            if record.is_empty() {
                return;
            }
            let get = |c: u8| {
                record
                    .iter()
                    .find(|(k, ..)| *k == c)
                    .map(|(_, v, ..)| v.clone())
                    .unwrap_or_default()
            };
            let src = span(start, end - start);
            if in_account_list {
                *account = get(b'N');
                tables.accounts.push(
                    vec![get(b'N'), get(b'T'), get(b'D'), get(b'L'), get(b'$')],
                    src,
                );
            } else {
                match section {
                    Section::Cash | Section::Memorized => {
                        n_tx += 1;
                        let amount = {
                            let t = get(b'T');
                            if t.is_empty() { get(b'U') } else { t }
                        };
                        let date = get(b'D');
                        if !date.is_empty() {
                            if first_date.is_empty() {
                                first_date = date.clone();
                            }
                            last_date = date.clone();
                        }
                        *counts.entry(section_name.to_owned()).or_default() += 1;
                        let parsed = qif_amount(&amount);
                        if !amount.is_empty() && parsed.is_none() {
                            bad_amounts += 1;
                        }
                        let e = net
                            .entry(section_name.to_owned())
                            .or_insert(Some(Dec::ZERO));
                        *e = match (*e, parsed) {
                            (Some(s), Some(a)) => s.checked_add(a),
                            (s, None) if amount.is_empty() => s,
                            _ => None,
                        };
                        // Splits: S category, E memo, $ amount, % percent, in order.
                        let mut splits: Vec<[String; 4]> = vec![];
                        for (k, v, ..) in record.iter() {
                            match k {
                                b'S' => splits.push([
                                    v.clone(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                ]),
                                b'E' | b'$' | b'%' => {
                                    if splits.is_empty() {
                                        splits.push(Default::default());
                                    }
                                    if let Some(s) = splits.last_mut() {
                                        let i = match k {
                                            b'E' => 1,
                                            b'$' => 2,
                                            _ => 3,
                                        };
                                        if s[i].is_empty() {
                                            s[i] = v.clone();
                                        } else {
                                            splits.push(Default::default());
                                            if let Some(s) = splits.last_mut() {
                                                s[i] = v.clone();
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        if !splits.is_empty() {
                            let sum = splits.iter().try_fold(Dec::ZERO, |acc, s| {
                                if s[2].is_empty() {
                                    Some(acc)
                                } else {
                                    acc.checked_add(qif_amount(&s[2])?)
                                }
                            });
                            if let (Some(sum), Some(total)) = (sum, parsed) {
                                let n = n_tx;
                                splits_check.add(sum == total, || {
                                    format!(
                                        "transaction {n} ({date}): amount {}, splits {}",
                                        total.format(2),
                                        sum.format(2)
                                    )
                                });
                            }
                        }
                        let address: Vec<String> = record
                            .iter()
                            .filter(|(k, ..)| *k == b'A')
                            .map(|(_, v, ..)| v.clone())
                            .collect();
                        if !tables.transactions.full() {
                            tables.transactions.push(
                                vec![
                                    n_tx.to_string(),
                                    account.clone(),
                                    section_name.to_owned(),
                                    date,
                                    amount,
                                    get(b'P'),
                                    get(b'M'),
                                    get(b'L'),
                                    get(b'N'),
                                    cleared(&get(b'C')),
                                    address.join(", "),
                                    splits.len().to_string(),
                                ],
                                src.clone(),
                            );
                        } else {
                            tables.transactions.skip();
                        }
                        for s in splits {
                            if tables.splits.full() {
                                tables.splits.skip();
                                continue;
                            }
                            let [c, m, a, p] = s;
                            tables
                                .splits
                                .push(vec![n_tx.to_string(), c, m, a, p], src.clone());
                        }
                    }
                    Section::Invst => {
                        n_tx += 1;
                        *counts.entry(section_name.to_owned()).or_default() += 1;
                        let date = get(b'D');
                        if !date.is_empty() {
                            if first_date.is_empty() {
                                first_date = date.clone();
                            }
                            last_date = date.clone();
                        }
                        tables.invest.push(
                            vec![
                                n_tx.to_string(),
                                account.clone(),
                                date,
                                get(b'N'),
                                get(b'Y'),
                                get(b'Q'),
                                get(b'I'),
                                {
                                    let t = get(b'T');
                                    if t.is_empty() { get(b'U') } else { t }
                                },
                                get(b'O'),
                                get(b'L'),
                                get(b'P'),
                                get(b'M'),
                                cleared(&get(b'C')),
                            ],
                            src,
                        );
                    }
                    Section::Cat => tables.categories.push(
                        vec![
                            get(b'N'),
                            get(b'D'),
                            if record.iter().any(|(k, ..)| *k == b'I') {
                                "Income".into()
                            } else if record.iter().any(|(k, ..)| *k == b'E') {
                                "Expense".into()
                            } else {
                                String::new()
                            },
                            if record.iter().any(|(k, ..)| *k == b'T') {
                                "yes".into()
                            } else {
                                String::new()
                            },
                            get(b'R'),
                        ],
                        src,
                    ),
                    Section::Class => tables.classes.push(vec![get(b'N'), get(b'D')], src),
                    Section::Security => tables
                        .securities
                        .push(vec![get(b'N'), get(b'S'), get(b'T'), get(b'G')], src),
                    Section::Other | Section::None => skipped += 1,
                }
            }
            record.clear();
        };
        let mut last_end = 0usize;
        for (i, (at, line)) in common::lines_from(b, common::bom(b)).enumerate() {
            if i % 8192 == 0 && cancelled() {
                problems.push("Cancelled".into());
                break;
            }
            let text = common::text(line);
            let t = text.trim_end();
            if t.trim().is_empty() {
                continue;
            }
            let end = at + line.len();
            if t.starts_with('!') {
                flush(
                    &mut record,
                    record_start,
                    last_end,
                    section,
                    &section_name,
                    in_account_list,
                    &mut account,
                );
                let lower = t.to_ascii_lowercase();
                if lower.starts_with("!account") {
                    in_account_list = true;
                } else if let Some(ty) = lower.strip_prefix("!type:") {
                    in_account_list = false;
                    section_name = t.get(6..).unwrap_or("").trim().to_owned();
                    section = Section::of(ty.trim());
                    if section == Section::Other {
                        *unknown_sections.entry(section_name.clone()).or_default() += 1;
                    }
                }
                // !Option:AutoSwitch and !Clear:AutoSwitch only bracket the account list.
                last_end = end;
                continue;
            }
            if t.trim() == "^" {
                flush(
                    &mut record,
                    record_start,
                    end,
                    section,
                    &section_name,
                    in_account_list,
                    &mut account,
                );
                last_end = end;
                continue;
            }
            if record.is_empty() {
                record_start = at;
            }
            let lead = line.first().copied().unwrap_or(b' ');
            if record.len() < MAX_FIELDS {
                let value = t.get(1..).unwrap_or("").to_owned();
                record.push((lead, value, at, end));
            }
            last_end = end;
        }
        let unterminated = !record.is_empty();
        flush(
            &mut record,
            record_start,
            last_end,
            section,
            &section_name,
            in_account_list,
            &mut account,
        );
        if unterminated {
            problems.push("The last record has no ^ terminator (the file may be cut)".into());
        }
        if skipped > 0 {
            problems.push(format!(
                "{skipped} record(s) in sections that are not interpreted were skipped"
            ));
        }
        if bad_amounts > 0 {
            problems.push(format!("{bad_amounts} amount(s) are not numbers"));
        }
        let types: Vec<String> = counts.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        out.fact("File", "Sections", types.join(", "), None);
        out.fact("File", "Transactions", n_tx.to_string(), None);
        out.fact("File", "Accounts", tables.accounts.len().to_string(), None);
        if !first_date.is_empty() {
            out.fact("File", "First date (as written)", first_date, None);
            out.fact("File", "Last date (as written)", last_date, None);
        }
        for (k, v) in &net {
            if let Some(v) = v {
                out.fact("Net amounts", k, v.format(2), None);
            }
        }
        if !unknown_sections.is_empty() {
            let names: Vec<&str> = unknown_sections.keys().map(String::as_str).collect();
            out.notes
                .push(format!("Sections not interpreted: {}", names.join(", ")));
        }
        if let Some(name) = counts.keys().next()
            && counts.len() == 1
        {
            out.kind = format!("QIF — {}", section_label(name));
        }
        let Tables {
            transactions,
            splits,
            invest,
            accounts,
            categories,
            classes,
            securities,
        } = tables;
        transactions.finish(&mut out);
        splits.finish(&mut out);
        invest.finish(&mut out);
        accounts.finish(&mut out);
        categories.finish(&mut out);
        classes.finish(&mut out);
        securities.finish(&mut out);
        if out.tables.is_empty() && n_tx == 0 {
            problems.push("No record was found".into());
        }
        common::parsed(&mut out, "Sections and records read", &problems);
        out.check(
            Stage::Schema,
            State::NotDone,
            "QIF has no formal schema; field codes are interpreted as Quicken documents them",
        );
        splits_check.report(&mut out);
        out.notes.push(
            "Dates are shown as written: QIF does not say whether they are month/day or day/month"
                .into(),
        );
        Ok(out)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Section {
    None,
    /// Bank, Cash, CCard, Oth A, Oth L, Invoice: transactions with splits.
    Cash,
    Memorized,
    Invst,
    Cat,
    Class,
    Security,
    Other,
}

impl Section {
    fn of(lower: &str) -> Section {
        match lower {
            "bank" | "cash" | "ccard" | "oth a" | "oth l" | "invoice" => Section::Cash,
            "memorized" => Section::Memorized,
            "invst" | "port" => Section::Invst,
            "cat" => Section::Cat,
            "class" => Section::Class,
            "security" => Section::Security,
            _ => Section::Other,
        }
    }
}

fn section_label(name: &str) -> String {
    let l = match name.to_ascii_lowercase().as_str() {
        "bank" => "bank account",
        "cash" => "cash account",
        "ccard" => "credit card account",
        "oth a" => "asset account",
        "oth l" => "liability account",
        "invst" | "port" => "investment account",
        "invoice" => "invoices",
        "memorized" => "memorized transactions",
        "cat" => "category list",
        "class" => "class list",
        "security" => "securities",
        _ => "",
    };
    if l.is_empty() {
        name.to_owned()
    } else {
        format!("{l} ({name})")
    }
}

fn cleared(c: &str) -> String {
    match c.trim() {
        "" => String::new(),
        "*" | "c" => format!("{c} (cleared)"),
        "X" | "R" => format!("{c} (reconciled)"),
        other => other.to_owned(),
    }
}

/// QIF amounts use `,` for thousands ("1,234.56") in most files.
fn qif_amount(s: &str) -> Option<Dec> {
    common::loose_decimal(s, false)
}

struct Tables {
    transactions: Capped,
    splits: Capped,
    invest: Capped,
    accounts: Capped,
    categories: Capped,
    classes: Capped,
    securities: Capped,
}

impl Tables {
    fn new() -> Self {
        Self {
            transactions: Capped::new(
                "Transactions",
                &[
                    "#", "Account", "Type", "Date", "Amount", "Payee", "Memo", "Category",
                    "Number", "Cleared", "Address", "Splits",
                ],
            ),
            splits: Capped::new(
                "Splits",
                &["Transaction", "Category", "Memo", "Amount", "Percent"],
            ),
            invest: Capped::new(
                "Investment transactions",
                &[
                    "#",
                    "Account",
                    "Date",
                    "Action",
                    "Security",
                    "Quantity",
                    "Price",
                    "Amount",
                    "Commission",
                    "Transfer account",
                    "Text",
                    "Memo",
                    "Cleared",
                ],
            ),
            accounts: Capped::new(
                "Accounts",
                &["Name", "Type", "Description", "Credit limit", "Balance"],
            ),
            categories: Capped::new(
                "Categories",
                &[
                    "Name",
                    "Description",
                    "Income/expense",
                    "Tax related",
                    "Tax schedule",
                ],
            ),
            classes: Capped::new("Classes", &["Name", "Description"]),
            securities: Capped::new("Securities", &["Name", "Symbol", "Type", "Goal"]),
        }
    }
}
