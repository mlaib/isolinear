//! SWIFT MT940 (customer statement), MT942 (interim transaction report) and MT950 (statement
//! message): statements with balances, statement lines `:61:` joined with the information
//! `:86:` that follows them, and the balance rule opening + credits − debits = closing. Files
//! come with SWIFT headers or, as banks usually export them (`.sta`), as bare text blocks; a file
//! may hold several statements, each starting with `:20:`.
use super::common::{self, Capped, Dec, Tally, span};
use super::swift_mt::{
    self, Balance, Field, FloorLimit, StatementLine, Totals, field_name, interpret,
};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};

/// Statements whose facts are listed one by one (the Statements table lists all).
const FACT_STATEMENTS: usize = 30;

pub struct Mt940;

impl Handler for Mt940 {
    fn name(&self) -> &'static str {
        "SWIFT MT940/MT942/MT950"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if swift_mt::starts_with_header(probe.head) {
            return matches!(swift_mt::first_mt(probe.head), Some("940" | "942" | "950"));
        }
        let tags = swift_mt::head_tags(probe.head);
        let has = |t: &str| tags.contains(&t);
        has("20") && has("25") && ["60F", "60M", "34F", "13D"].iter().any(|t| has(t))
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let parsed = swift_mt::parse(input.bytes, cancelled);
        let mut statements: Vec<Statement> = vec![];
        for m in &parsed.messages {
            let mut current: Option<Statement> = None;
            for f in &m.fields {
                if f.tag == "20" || current.is_none() {
                    if let Some(s) = current.take() {
                        statements.push(s);
                    }
                    current = Some(Statement {
                        mt: m.mt(),
                        fields: vec![],
                    });
                }
                if let Some(s) = current.as_mut() {
                    s.fields.push(f);
                }
            }
            statements.extend(current);
        }
        if statements.is_empty() {
            return Err("No statement was found".into());
        }
        let mt = parsed
            .messages
            .first()
            .and_then(|m| m.mt())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                let any = |t: &str| statements.iter().any(|s| s.get(t).is_some());
                if !any("60F") && !any("60M") && (any("34F") || any("13D")) {
                    "942".into()
                } else {
                    "940".into()
                }
            });
        let name = swift_mt::mt_name(&mt).unwrap_or("statement");
        let mut out = Inspection::new("SWIFT MT", &format!("MT{mt} — {name}"));
        if parsed.with_headers {
            out.version = Some("FIN".into());
            if let Some(m) = parsed.messages.first() {
                swift_mt::header_facts(m, &mut out, "Message");
            }
        }
        out.fact("File", "Statements", statements.len().to_string(), None);
        if parsed.messages.len() > 1 {
            out.fact("File", "Messages", parsed.messages.len().to_string(), None);
        }
        let mut problems = parsed.problems.clone();
        let mut bad_lines = 0usize;
        let mut tx = Capped::new(
            "Transactions",
            &[
                "Statement",
                "Value date",
                "Entry date",
                "D/C",
                "Funds code",
                "Amount",
                "Currency",
                "Type",
                "Customer reference",
                "Bank reference",
                "Supplementary details",
                "Information (86)",
                "As written (61)",
            ],
        );
        let mut table = Capped::new(
            "Statements",
            &[
                "Statement",
                "Reference",
                "Account",
                "Number",
                "Currency",
                "Opening",
                "Credits",
                "Debits",
                "Closing (declared)",
                "Closing (computed)",
                "Entries",
            ],
        );
        let mut balance = Tally::new("Closing balance (opening + credits − debits)");
        let mut debit_totals = Tally::new("Debit entries (90D): number and sum");
        let mut credit_totals = Tally::new("Credit entries (90C): number and sum");
        let mut continuity =
            Tally::new("Continuity (opening balance = previous closing balance, same account)");
        let mut previous: Option<(String, Balance)> = None;
        let many = statements.len() > 1;
        for (k, s) in statements.iter().enumerate() {
            if k % 1024 == 0 && cancelled() {
                problems.push("Cancelled".into());
                break;
            }
            let n = k + 1;
            let reference = s.value("20").unwrap_or_default();
            let account = s.value("25").or_else(|| s.value("25P")).unwrap_or_default();
            let group = if many {
                format!("Statement {n}")
            } else {
                "Statement".into()
            };
            let opening = s.balance(&["60F", "60M"]);
            let closing = s.balance(&["62F", "62M"]);
            let currency = opening
                .as_ref()
                .or(closing.as_ref())
                .map(|(b, _)| b.currency.clone())
                .or_else(|| {
                    s.get("34F")
                        .and_then(|f| FloorLimit::parse(&f.value))
                        .map(|f| f.currency)
                })
                .or_else(|| {
                    s.get("90D")
                        .or_else(|| s.get("90C"))
                        .and_then(|f| Totals::parse(&f.value))
                        .map(|t| t.currency)
                })
                .unwrap_or_default();
            // Statement lines, each with the `:86:` right after it.
            let mut lines: Vec<(Option<StatementLine>, &Field, Option<&Field>)> = vec![];
            let mut info: Vec<&Field> = vec![];
            for (i, f) in s.fields.iter().enumerate() {
                match f.tag.as_str() {
                    "61" => {
                        let next86 = s.fields.get(i + 1).copied().filter(|g| g.tag == "86");
                        lines.push((StatementLine::parse(&f.value), f, next86));
                    }
                    "86" if i == 0 || s.fields.get(i - 1).is_none_or(|p| p.tag != "61") => {
                        info.push(f)
                    }
                    _ => {}
                }
            }
            let (mut credits, mut debits) = (Dec::ZERO, Dec::ZERO);
            let (mut n_credits, mut n_debits) = (0u64, 0u64);
            let mut sums_ok = true;
            for (line, f61, f86) in &lines {
                match line {
                    Some(l) => {
                        let (sum, count) = if l.is_credit() {
                            (&mut credits, &mut n_credits)
                        } else {
                            (&mut debits, &mut n_debits)
                        };
                        *count += 1;
                        match sum.checked_add(l.amount) {
                            Some(v) => *sum = v,
                            None => sums_ok = false,
                        }
                    }
                    None => {
                        bad_lines += 1;
                        sums_ok = false;
                    }
                }
                if tx.full() {
                    tx.skip();
                    continue;
                }
                let info86 = f86.map(|g| g.value.clone()).unwrap_or_default();
                let end = f86.map(|g| g.span.end).unwrap_or(f61.span.end);
                let cells = match line {
                    Some(l) => vec![
                        n.to_string(),
                        l.value_date.clone(),
                        l.entry_date.clone(),
                        l.mark.clone(),
                        l.funds_code.clone(),
                        l.amount.format(2),
                        currency.clone(),
                        type_name(&l.kind),
                        l.customer_ref.clone(),
                        l.bank_ref.clone(),
                        l.supplementary.clone(),
                        info86,
                        f61.value.clone(),
                    ],
                    None => {
                        let mut c = vec![String::new(); 13];
                        c[0] = n.to_string();
                        c[11] = info86;
                        c[12] = f61.value.clone();
                        c
                    }
                };
                tx.push(cells, span(f61.span.start, end - f61.span.start));
            }
            let computed = match (&opening, sums_ok) {
                (Some((o, _)), true) => o
                    .signed()
                    .checked_add(credits)
                    .and_then(|v| v.checked_sub(debits)),
                _ => None,
            };
            if let (Some((o, _)), Some((c, _)), Some(comp)) = (&opening, &closing, computed) {
                let ok = comp == c.signed() && o.currency == c.currency;
                balance.add(ok, || {
                    let what = format!(
                        "opening {} + credits {} − debits {} = {}; declared closing {}{}",
                        o.signed().format(2),
                        credits.format(2),
                        debits.format(2),
                        comp.format(2),
                        c.signed().format(2),
                        if o.currency == c.currency {
                            String::new()
                        } else {
                            format!(" (currencies differ: {} and {})", o.currency, c.currency)
                        }
                    );
                    if many {
                        format!("statement {n} ({reference}): {what}")
                    } else {
                        what
                    }
                });
            }
            for (tag, tally, count, sum) in [
                ("90D", &mut debit_totals, n_debits, debits),
                ("90C", &mut credit_totals, n_credits, credits),
            ] {
                let Some(t) = s.get(tag).and_then(|f| Totals::parse(&f.value)) else {
                    continue;
                };
                if !sums_ok {
                    continue;
                }
                tally.add(t.count == count && t.amount == sum, || {
                    let what = format!(
                        "declared {} entries for {}, found {count} for {}",
                        t.count,
                        t.amount.format(2),
                        sum.format(2)
                    );
                    if many {
                        format!("statement {n}: {what}")
                    } else {
                        what
                    }
                });
            }
            if let (Some((prev_account, prev)), Some((o, _))) = (&previous, &opening)
                && *prev_account == account
            {
                continuity.add(
                    prev.signed() == o.signed() && prev.currency == o.currency,
                    || {
                        format!(
                            "statement {n}: opening {} {}, previous closing {} {}",
                            o.currency,
                            o.signed().format(2),
                            prev.currency,
                            prev.signed().format(2)
                        )
                    },
                );
            }
            previous = closing.as_ref().map(|(c, _)| (account.clone(), c.clone()));
            if !table.full() {
                let first = s.fields.first().map(|f| f.span.start).unwrap_or(0);
                let last = s.fields.last().map(|f| f.span.end).unwrap_or(first);
                table.push(
                    vec![
                        n.to_string(),
                        reference.clone(),
                        account.clone(),
                        s.get("28C")
                            .or_else(|| s.get("28"))
                            .map(|f| f.value.clone())
                            .unwrap_or_default(),
                        currency.clone(),
                        opening
                            .as_ref()
                            .map(|(b, _)| b.signed().format(2))
                            .unwrap_or_default(),
                        credits.format(2),
                        debits.format(2),
                        closing
                            .as_ref()
                            .map(|(b, _)| b.signed().format(2))
                            .unwrap_or_default(),
                        computed.map(|c| c.format(2)).unwrap_or_default(),
                        lines.len().to_string(),
                    ],
                    span(first, last - first),
                );
            } else {
                table.skip();
            }
            if k >= FACT_STATEMENTS {
                continue;
            }
            let mt = s.mt;
            let fact = |out: &mut Inspection, tag: &str, label: &str| {
                for f in s.fields.iter().filter(|f| f.tag == tag) {
                    let i = interpret(&f.tag, &f.value);
                    let v = if i.is_empty() { f.value.clone() } else { i };
                    out.fact(
                        &group,
                        label,
                        v,
                        span(f.span.start, f.span.end - f.span.start),
                    );
                }
            };
            fact(&mut out, "20", "Reference (20)");
            fact(&mut out, "21", "Related reference (21)");
            fact(&mut out, "25", "Account (25)");
            fact(&mut out, "25P", "Account (25P)");
            fact(&mut out, "28C", "Statement number (28C)");
            fact(&mut out, "28", "Statement number (28)");
            fact(&mut out, "13D", "Date/time (13D)");
            fact(&mut out, "34F", "Floor limit (34F)");
            fact(&mut out, "60F", "Opening balance (60F)");
            fact(&mut out, "60M", "Opening balance, intermediate (60M)");
            out.fact(&group, "Entries (61)", lines.len().to_string(), None);
            out.fact(
                &group,
                "Credits",
                format!("{n_credits}, {}", credits.format(2)),
                None,
            );
            out.fact(
                &group,
                "Debits",
                format!("{n_debits}, {}", debits.format(2)),
                None,
            );
            fact(&mut out, "62F", "Closing balance (62F)");
            fact(&mut out, "62M", "Closing balance, intermediate (62M)");
            fact(&mut out, "64", "Closing available balance (64)");
            fact(&mut out, "65", "Forward available balance (65)");
            fact(&mut out, "90D", "Debit entries (90D)");
            fact(&mut out, "90C", "Credit entries (90C)");
            for f in &info {
                out.fact(
                    &group,
                    field_name(mt, "86"),
                    f.value.clone(),
                    span(f.span.start, f.span.end - f.span.start),
                );
            }
        }
        if statements.len() > FACT_STATEMENTS {
            out.notes.push(format!(
                "Facts are listed for the first {FACT_STATEMENTS} statements; the Statements table lists all"
            ));
        }
        tx.finish(&mut out);
        table.finish(&mut out);
        swift_mt::fields_table(&parsed, &mut out);
        if bad_lines > 0 {
            problems.push(format!(
                "{bad_lines} statement line(s) (61) could not be read: they are listed as written and their statement's totals are not computed"
            ));
        }
        common::parsed(
            &mut out,
            "Statements, balances and statement lines read",
            &problems,
        );
        out.check(
            Stage::Schema,
            State::NotDone,
            "Field formats and mandatory fields are not checked against the MT940/MT942 rules",
        );
        let balances_checked = balance.failed() > 0
            || statements.iter().any(|s| {
                s.balance(&["60F", "60M"]).is_some() && s.balance(&["62F", "62M"]).is_some()
            });
        balance.report(&mut out);
        debit_totals.report(&mut out);
        credit_totals.report(&mut out);
        continuity.report(&mut out);
        if !balances_checked {
            out.check(
                Stage::Semantic,
                State::NotDone,
                "Closing balance: no statement has both an opening and a closing balance",
            );
        }
        out.notes.push(
            "C and RD (reversal of a debit) entries add to the balance; D and RC subtract".into(),
        );
        out.notes.push(
            "Information (86) is shown as written; bank-specific subfields (such as ?20…?29) are not split"
                .into(),
        );
        out.notes.push(
            "Years in SWIFT dates have two digits: 80–99 are read as 1980–1999, the others as 2000–2079"
                .into(),
        );
        Ok(out)
    }
}

struct Statement<'a> {
    mt: Option<&'a str>,
    fields: Vec<&'a Field>,
}

impl Statement<'_> {
    fn get(&self, tag: &str) -> Option<&Field> {
        self.fields.iter().copied().find(|f| f.tag == tag)
    }
    fn value(&self, tag: &str) -> Option<String> {
        self.get(tag).map(|f| f.value.clone())
    }
    fn balance(&self, tags: &[&str]) -> Option<(Balance, &Field)> {
        tags.iter()
            .find_map(|t| self.get(t))
            .and_then(|f| Balance::parse(&f.value).map(|b| (b, f)))
    }
}

/// "NTRF" → "NTRF (transfer)".
fn type_name(code: &str) -> String {
    let name = match code.get(1..) {
        _ if code.starts_with('S') && code.len() == 4 => "SWIFT MT",
        Some("BOE") => "bill of exchange",
        Some("BRF") => "brokerage fee",
        Some("CHG") => "charges",
        Some("CHK") => "cheque",
        Some("CLR") => "cash letters/cheques remittance",
        Some("CMI") | Some("CMN") | Some("CMP") | Some("CMS") | Some("CMT") | Some("CMZ") => {
            "cash management"
        }
        Some("COL") => "collections",
        Some("COM") => "commission",
        Some("DCR") => "documentary credit",
        Some("DDT") => "direct debit",
        Some("DIS") => "gross amount",
        Some("DIV") => "dividends",
        Some("EQA") => "equivalent amount",
        Some("EXT") => "external transfer",
        Some("FEX") => "foreign exchange",
        Some("INT") => "interest",
        Some("LBX") => "lockbox",
        Some("LDP") => "loan deposit",
        Some("MSC") => "miscellaneous",
        Some("RTI") => "returned item",
        Some("SEC") => "securities",
        Some("STO") => "standing order",
        Some("TCK") => "travellers cheques",
        Some("TRF") => "transfer",
        Some("VDA") => "value date adjustment",
        Some("WAR") => "warrant",
        _ => "",
    };
    if name.is_empty() || code.is_empty() {
        code.to_owned()
    } else if name == "SWIFT MT" {
        format!("{code} (from MT{})", code.get(1..).unwrap_or(""))
    } else {
        format!("{code} ({name})")
    }
}
