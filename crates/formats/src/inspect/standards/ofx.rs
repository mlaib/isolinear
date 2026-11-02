//! OFX (Open Financial Exchange) and Quicken's QFX: version 1 (an `OFXHEADER:100` header, then
//! SGML where value elements have no closing tag) and version 2 (XML with an `<?OFX ...?>`
//! instruction). One tolerant reader handles both: an element followed by text is a value, an
//! element followed by another tag is an aggregate closed by its end tag.
use super::common::{self, Capped, Dec, Tally, span};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};
use std::{collections::HashSet, ops::Range};

/// Elements read at most.
const MAX_ELEMENTS: usize = 4_000_000;
/// Deepest aggregate nesting accepted (deeper tags are read as values).
const MAX_DEPTH: usize = 64;
/// Statements whose facts are listed one by one.
const FACT_STATEMENTS: usize = 20;

pub struct Ofx;

fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

impl Handler for Ofx {
    fn name(&self) -> &'static str {
        "OFX"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let head = common::trimmed_head(probe.head);
        if head.starts_with(b"OFXHEADER:") {
            return true;
        }
        let start = head.get(..4096).unwrap_or(head);
        if memchr::memmem::find(start, b"<?OFX ").is_some() {
            return true;
        }
        probe.ext_is(&["ofx", "qfx", "qbo"]) && contains_ci(start, b"<OFX>")
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        let (header, body_at, sgml) = read_header(b);
        let doc = Doc::parse(b, body_at, cancelled)?;
        let root = doc
            .elements
            .iter()
            .position(|e| e.name == "OFX")
            .ok_or("No <OFX> element was found")?;
        let mut out = Inspection::new("OFX", "OFX document");
        let version = header
            .iter()
            .find(|(k, _)| k == "VERSION")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        out.version = Some(match (sgml, version.as_str()) {
            (true, "") => "1.x (SGML)".into(),
            (true, v) => format!("{} (SGML)", dotted(v)),
            (false, "") => "2.x (XML)".into(),
            (false, v) => format!("{} (XML)", dotted(v)),
        });
        for (k, v) in &header {
            out.fact("Header", k, v.clone(), None);
        }
        // Sign-on.
        if let Some(sonrs) = doc.find(root, "SONRS") {
            let f = |out: &mut Inspection, label: &str, path: &str| {
                if let Some((v, s)) = doc.value_at(sonrs, path) {
                    out.fact("Sign-on", label, v, s);
                }
            };
            if let Some((code, s)) = doc.value_at(sonrs, "STATUS/CODE") {
                out.fact("Sign-on", "Status", status(&code), s);
            }
            f(&mut out, "Status message", "STATUS/MESSAGE");
            f(&mut out, "Server date", "DTSERVER");
            f(&mut out, "Language", "LANGUAGE");
            f(&mut out, "Institution", "FI/ORG");
            f(&mut out, "Institution ID (FID)", "FI/FID");
            f(&mut out, "Intuit bank ID", "INTU.BID");
        }
        let statements: Vec<(usize, &'static str)> = doc
            .all(root)
            .into_iter()
            .filter_map(|i| match doc.elements.get(i).map(|e| e.name.as_str()) {
                Some("STMTRS") => Some((i, "Bank statement")),
                Some("CCSTMTRS") => Some((i, "Credit card statement")),
                Some("INVSTMTRS") => Some((i, "Investment statement")),
                _ => None,
            })
            .collect();
        let kinds: HashSet<&str> = statements.iter().map(|(_, k)| *k).collect();
        out.kind = match (statements.len(), kinds.iter().next()) {
            (0, _) => "OFX document (no statement)".into(),
            (1, Some(k)) => format!("OFX — {}", k.to_lowercase()),
            (n, Some(k)) if kinds.len() == 1 => format!("OFX — {} ({n})", k.to_lowercase()),
            (n, _) => format!("OFX — statements ({n})"),
        };
        let mut tx = Capped::new(
            "Transactions",
            &[
                "Statement",
                "Type",
                "Date posted",
                "Amount",
                "Currency",
                "FITID",
                "Name / payee",
                "Memo",
                "Check number",
                "Reference",
                "User date",
            ],
        );
        let mut unique = Tally::new("FITID unique within each statement");
        let mut amounts_ok = Tally::new("Transaction amounts are numbers");
        let many = statements.len() > 1;
        for (k, &(st, what)) in statements.iter().enumerate() {
            if k % 256 == 0 && cancelled() {
                break;
            }
            let n = k + 1;
            let group = if many {
                format!("Statement {n}")
            } else {
                "Statement".into()
            };
            let currency = doc.value_at(st, "CURDEF").map(|v| v.0).unwrap_or_default();
            if k < FACT_STATEMENTS {
                let f = |out: &mut Inspection, label: &str, path: &str| {
                    if let Some((v, s)) = doc.value_at(st, path) {
                        out.fact(&group, label, v, s);
                    }
                };
                out.fact(&group, "Type", what, doc.source(st));
                f(&mut out, "Currency", "CURDEF");
                f(&mut out, "Bank ID", "BANKACCTFROM/BANKID");
                f(&mut out, "Branch", "BANKACCTFROM/BRANCHID");
                f(&mut out, "Account", "BANKACCTFROM/ACCTID");
                f(&mut out, "Account", "CCACCTFROM/ACCTID");
                f(&mut out, "Account", "INVACCTFROM/ACCTID");
                f(&mut out, "Broker", "INVACCTFROM/BROKERID");
                f(&mut out, "Account type", "BANKACCTFROM/ACCTTYPE");
                for (label, path) in [
                    ("From", "BANKTRANLIST/DTSTART"),
                    ("To", "BANKTRANLIST/DTEND"),
                    ("From", "INVTRANLIST/DTSTART"),
                    ("To", "INVTRANLIST/DTEND"),
                    ("Ledger balance as of", "LEDGERBAL/DTASOF"),
                    ("Available balance as of", "AVAILBAL/DTASOF"),
                    ("Statement date", "DTASOF"),
                ] {
                    if let Some((v, s)) = doc.value_at(st, path) {
                        out.fact(&group, label, date(&v), s);
                    }
                }
                f(&mut out, "Ledger balance", "LEDGERBAL/BALAMT");
                f(&mut out, "Available balance", "AVAILBAL/BALAMT");
            }
            let trns: Vec<usize> = doc
                .all(st)
                .into_iter()
                .filter(|&i| doc.elements.get(i).is_some_and(|e| e.name == "STMTTRN"))
                .collect();
            if k < FACT_STATEMENTS {
                out.fact(
                    &group,
                    "Transactions",
                    trns.len().to_string(),
                    doc.source(st),
                );
            }
            let mut seen: HashSet<String> = HashSet::new();
            let mut duplicates = vec![];
            for &t in &trns {
                let v = |path: &str| doc.value_at(t, path).map(|v| v.0).unwrap_or_default();
                let fitid = v("FITID");
                if !fitid.is_empty() && !seen.insert(fitid.clone()) && duplicates.len() < 5 {
                    duplicates.push(fitid.clone());
                }
                let amount = v("TRNAMT");
                if !amount.is_empty() {
                    let ok = common::loose_decimal(&amount, true).is_some();
                    amounts_ok.add(ok, || format!("statement {n}: {amount:?}"));
                }
                if tx.full() {
                    tx.skip();
                    continue;
                }
                let ty = v("TRNTYPE");
                let name = {
                    let n1 = v("NAME");
                    if n1.is_empty() { v("PAYEE/NAME") } else { n1 }
                };
                let cur = {
                    let c = v("CURRENCY/CURSYM");
                    let c = if c.is_empty() {
                        v("ORIGCURRENCY/CURSYM")
                    } else {
                        c
                    };
                    if c.is_empty() { currency.clone() } else { c }
                };
                tx.push(
                    vec![
                        n.to_string(),
                        trn_type(&ty),
                        date(&v("DTPOSTED")),
                        amount,
                        cur,
                        fitid,
                        name,
                        v("MEMO"),
                        v("CHECKNUM"),
                        v("REFNUM"),
                        date(&v("DTUSER")),
                    ],
                    doc.source(t),
                );
            }
            if !trns.is_empty() {
                unique.add(duplicates.is_empty(), || {
                    if duplicates.is_empty() {
                        format!("statement {n}: {} transactions", trns.len())
                    } else {
                        format!("statement {n}: repeated {}", duplicates.join(", "))
                    }
                });
            }
            // Sum of the transactions, as information (OFX has no opening balance to check).
            if k < FACT_STATEMENTS && !trns.is_empty() {
                let mut sum = Some(Dec::ZERO);
                for &t in &trns {
                    let a = doc
                        .value_at(t, "TRNAMT")
                        .and_then(|(v, _)| common::loose_decimal(&v, true));
                    sum = match (sum, a) {
                        (Some(s), Some(a)) => s.checked_add(a),
                        _ => None,
                    };
                }
                if let Some(s) = sum {
                    out.fact(&group, "Sum of transactions", s.format(2), None);
                }
            }
        }
        if statements.len() > FACT_STATEMENTS {
            out.notes.push(format!(
                "Facts are listed for the first {FACT_STATEMENTS} statements"
            ));
        }
        tx.finish(&mut out);
        let mut problems = doc.problems.clone();
        if let Some(open) = doc.unclosed.last() {
            problems.push(format!(
                "The document ends before </{open}> is closed: what comes before is shown"
            ));
        }
        common::parsed(
            &mut out,
            if sgml {
                "Read as OFX 1 (SGML, value elements without end tags)"
            } else {
                "Read as OFX 2 (XML)"
            },
            &problems,
        );
        out.check(
            Stage::Schema,
            State::NotDone,
            "Not checked against the OFX DTD or schema",
        );
        unique.report(&mut out);
        amounts_ok.report(&mut out);
        out.notes.push(
            "OFX gives no opening balance, so the balances cannot be checked against the transactions"
                .into(),
        );
        if kinds.contains("Investment statement") {
            out.notes.push(
                "Investment statements: only the account, the period and the bank transactions (STMTTRN) are shown; buys, sells and positions are not listed"
                    .into(),
            );
        }
        Ok(out)
    }
}

/// "102" → "1.0.2"; "220" → "2.2.0".
fn dotted(v: &str) -> String {
    if v.len() == 3 && v.bytes().all(|c| c.is_ascii_digit()) {
        v.chars()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(".")
    } else {
        v.to_owned()
    }
}

fn status(code: &str) -> String {
    let name = match code {
        "0" => "success",
        "2000" => "general error",
        "15000" => "must change user password",
        "15500" => "signon invalid",
        "15501" => "customer account already in use",
        "15502" => "user password lockout",
        _ => "",
    };
    if name.is_empty() {
        code.to_owned()
    } else {
        format!("{code} ({name})")
    }
}

fn trn_type(t: &str) -> String {
    let name = match t {
        "CREDIT" => "generic credit",
        "DEBIT" => "generic debit",
        "INT" => "interest",
        "DIV" => "dividend",
        "FEE" => "fee",
        "SRVCHG" => "service charge",
        "DEP" => "deposit",
        "ATM" => "ATM",
        "POS" => "point of sale",
        "XFER" => "transfer",
        "CHECK" => "check",
        "PAYMENT" => "electronic payment",
        "CASH" => "cash withdrawal",
        "DIRECTDEP" => "direct deposit",
        "DIRECTDEBIT" => "direct debit",
        "REPEATPMT" => "repeating payment",
        "HOLD" => "hold",
        "OTHER" => "other",
        _ => "",
    };
    if name.is_empty() {
        t.to_owned()
    } else {
        format!("{t} ({name})")
    }
}

/// "20261031120000.000[-5:EST]" → "2026-10-31 12:00:00.000 [-5:EST]".
fn date(v: &str) -> String {
    let v = v.trim();
    let (main, tz) = match v.find('[') {
        Some(i) => (v.get(..i).unwrap_or(v), v.get(i..).unwrap_or("")),
        None => (v, ""),
    };
    let Some(d) = main.get(..8).and_then(common::yyyymmdd) else {
        return v.to_owned();
    };
    let rest = main.get(8..).unwrap_or("");
    let time = match (rest.get(0..2), rest.get(2..4), rest.get(4..)) {
        (Some(h), Some(m), Some(s)) if !s.is_empty() => format!(" {h}:{m}:{s}"),
        (Some(h), Some(m), _) => format!(" {h}:{m}"),
        _ => String::new(),
    };
    let tz = if tz.is_empty() {
        String::new()
    } else {
        format!(" {tz}")
    };
    format!("{d}{time}{tz}")
}

/// Header of an OFX 1 file (`KEY:VALUE` lines) or the `<?OFX ...?>` attributes of OFX 2: the
/// pairs, where the body starts, and whether it is SGML.
fn read_header(b: &[u8]) -> (Vec<(String, String)>, usize, bool) {
    let mut pairs = vec![];
    let start = common::bom(b);
    let head = common::trimmed_head(b);
    if head.starts_with(b"OFXHEADER:") {
        let mut body = b.len();
        for (at, line) in common::lines_from(b, start).take(100) {
            let t = common::text(line);
            let t = t.trim();
            if t.starts_with('<') {
                body = at;
                break;
            }
            if let Some((k, v)) = t.split_once(':') {
                pairs.push((k.trim().to_owned(), v.trim().to_owned()));
            }
        }
        if body == b.len() {
            body = memchr::memchr(b'<', b).unwrap_or(b.len());
        }
        return (pairs, body, true);
    }
    let window = b.get(..8192.min(b.len())).unwrap_or_default();
    if let Some(at) = memchr::memmem::find(window, b"<?OFX") {
        let end = memchr::memmem::find(window.get(at..).unwrap_or_default(), b"?>")
            .map(|e| at + e)
            .unwrap_or(at);
        let pi = common::text(window.get(at + 5..end).unwrap_or_default()).into_owned();
        for part in pi.split_whitespace() {
            if let Some((k, v)) = part.split_once('=') {
                pairs.push((k.to_owned(), v.trim_matches('"').to_owned()));
            }
        }
    }
    (pairs, start, false)
}

struct Element {
    name: String,
    value: Option<String>,
    children: Vec<usize>,
    span: Range<usize>,
}

struct Doc {
    elements: Vec<Element>,
    /// Aggregates still open at the end of the file, outermost first.
    unclosed: Vec<String>,
    problems: Vec<String>,
}

impl Doc {
    fn parse(b: &[u8], from: usize, cancelled: &dyn Fn() -> bool) -> Result<Doc, String> {
        let mut elements: Vec<Element> = vec![];
        let mut stack: Vec<usize> = vec![];
        let mut problems = vec![];
        let mut pos = from;
        let mut steps = 0usize;
        while let Some(i) = b.get(pos..).and_then(|r| memchr::memchr(b'<', r)) {
            steps += 1;
            if steps.is_multiple_of(65536) && cancelled() {
                return Err("Cancelled".into());
            }
            if elements.len() >= MAX_ELEMENTS {
                problems.push(format!("Only the first {MAX_ELEMENTS} elements were read"));
                break;
            }
            let lt = pos + i;
            let rest = b.get(lt..).unwrap_or_default();
            // Comments, declarations and processing instructions.
            if rest.starts_with(b"<!--") {
                pos = memchr::memmem::find(rest, b"-->")
                    .map(|e| lt + e + 3)
                    .unwrap_or(b.len());
                continue;
            }
            if rest.starts_with(b"<?") || rest.starts_with(b"<!") {
                pos = memchr::memchr(b'>', rest)
                    .map(|e| lt + e + 1)
                    .unwrap_or(b.len());
                continue;
            }
            let Some(gt) = memchr::memchr(b'>', rest) else {
                break;
            };
            let tag = rest.get(1..gt).unwrap_or_default();
            let after = lt + gt + 1;
            if let Some(name) = tag.strip_prefix(b"/") {
                let name = common::text(name).trim().to_uppercase();
                if let Some(depth) = stack
                    .iter()
                    .rposition(|&e| elements.get(e).is_some_and(|el| el.name == name))
                {
                    for &e in stack.get(depth..).unwrap_or_default() {
                        if let Some(el) = elements.get_mut(e) {
                            el.span.end = after;
                        }
                    }
                    stack.truncate(depth);
                }
                pos = after;
                continue;
            }
            let self_closing = tag.ends_with(b"/");
            let tag = if self_closing {
                tag.get(..tag.len() - 1).unwrap_or_default()
            } else {
                tag
            };
            let name_len = tag
                .iter()
                .position(|c| c.is_ascii_whitespace())
                .unwrap_or(tag.len());
            let name = common::text(tag.get(..name_len).unwrap_or_default())
                .trim()
                .to_uppercase();
            if name.is_empty() {
                pos = after;
                continue;
            }
            let index = elements.len();
            if let Some(&parent) = stack.last()
                && let Some(p) = elements.get_mut(parent)
            {
                p.children.push(index);
            }
            // Text up to the next tag: a value when it is not blank.
            let text_end = b
                .get(after..)
                .and_then(|r| memchr::memchr(b'<', r))
                .map(|e| after + e)
                .unwrap_or(b.len());
            let text = common::text(b.get(after..text_end).unwrap_or_default());
            let text = text.trim();
            if self_closing {
                elements.push(Element {
                    name,
                    value: None,
                    children: vec![],
                    span: lt..after,
                });
                pos = after;
            } else if !text.is_empty() || stack.len() >= MAX_DEPTH {
                let mut end = text_end;
                // XML (and some SGML writers) close values too.
                let close = format!("</{name}>");
                let next = b.get(text_end..).unwrap_or_default();
                if next.len() >= close.len()
                    && next
                        .get(..close.len())
                        .is_some_and(|c| c.eq_ignore_ascii_case(close.as_bytes()))
                {
                    end = text_end + close.len();
                }
                elements.push(Element {
                    name,
                    value: Some(unescape(text)),
                    children: vec![],
                    span: lt..end,
                });
                pos = end;
            } else {
                elements.push(Element {
                    name,
                    value: None,
                    children: vec![],
                    span: lt..text_end,
                });
                stack.push(index);
                pos = after;
            }
        }
        let unclosed = stack
            .iter()
            .filter_map(|&e| elements.get(e).map(|el| el.name.clone()))
            .collect();
        for &e in &stack {
            if let Some(el) = elements.get_mut(e) {
                el.span.end = b.len();
            }
        }
        if elements.is_empty() {
            return Err("No OFX element was found".into());
        }
        Ok(Doc {
            elements,
            unclosed,
            problems,
        })
    }

    /// The element and all its descendants, in document order (bounded by the element count).
    fn all(&self, from: usize) -> Vec<usize> {
        let mut out = vec![];
        let mut stack = vec![from];
        while let Some(e) = stack.pop() {
            out.push(e);
            if let Some(el) = self.elements.get(e) {
                stack.extend(el.children.iter().rev());
            }
            if out.len() > MAX_ELEMENTS {
                break;
            }
        }
        out
    }

    /// The first descendant called `name`.
    fn find(&self, from: usize, name: &str) -> Option<usize> {
        self.all(from)
            .into_iter()
            .find(|&e| self.elements.get(e).is_some_and(|el| el.name == name))
    }

    /// The value at a path of child names (`BANKACCTFROM/ACCTID`), with its source.
    fn value_at(&self, from: usize, path: &str) -> Option<(String, Option<Range<u64>>)> {
        let mut current = from;
        for step in path.split('/') {
            current = self
                .elements
                .get(current)?
                .children
                .iter()
                .copied()
                .find(|&c| self.elements.get(c).is_some_and(|el| el.name == step))?;
        }
        let el = self.elements.get(current)?;
        let v = el.value.clone()?;
        Some((v, span(el.span.start, el.span.end - el.span.start)))
    }

    fn source(&self, e: usize) -> Option<Range<u64>> {
        let el = self.elements.get(e)?;
        span(el.span.start, el.span.end.saturating_sub(el.span.start))
    }
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(rest.get(..i).unwrap_or(""));
        let tail = rest.get(i..).unwrap_or("");
        let end = tail.bytes().take(12).position(|c| c == b';');
        let entity = end.and_then(|e| tail.get(1..e));
        let decoded = match entity {
            Some("amp") => Some('&'),
            Some("lt") => Some('<'),
            Some("gt") => Some('>'),
            Some("quot") => Some('"'),
            Some("apos") => Some('\''),
            Some(n) if n.starts_with("#x") => u32::from_str_radix(n.get(2..).unwrap_or(""), 16)
                .ok()
                .and_then(char::from_u32),
            Some(n) if n.starts_with('#') => n
                .get(1..)
                .and_then(|d| d.parse().ok())
                .and_then(char::from_u32),
            _ => None,
        };
        match (decoded, end) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = tail.get(e + 1..).unwrap_or("");
            }
            _ => {
                out.push('&');
                rest = tail.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
    out
}
