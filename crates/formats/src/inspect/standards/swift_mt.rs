//! SWIFT MT (FIN) messages: the header blocks `{1:}` to `{5:}` and the text block's fields
//! `:TAG:`, in files of one or several messages (separated by nothing, white space, `$` or the
//! SOH/ETX characters of PC-connect files), or as bare text blocks without headers, as banks
//! export them. Statements (MT940/MT942/MT950) have their own view in [`super::mt940`], which
//! uses this reader.
use super::common::{self, Capped, Dec, comma_decimal, lines_from, span, yymmdd};
use crate::inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Stage, State};
use std::ops::Range;

/// Messages read from one file at most.
const MAX_MESSAGES: usize = 100_000;
/// Fields kept per message.
const MAX_FIELDS: usize = 10_000;
/// Longest header or trailer block scanned for its closing brace.
const MAX_BLOCK: usize = 8192;
/// Messages whose header facts are listed one by one.
const FACT_MESSAGES: usize = 20;

#[derive(Clone, Debug)]
pub struct Field {
    /// "20", "32A", "61".
    pub tag: String,
    /// The value as written, lines joined with `\n`.
    pub value: String,
    pub span: Range<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Message {
    pub span: Range<usize>,
    pub block1: Option<(String, Range<usize>)>,
    pub block2: Option<(String, Range<usize>)>,
    /// Sub-blocks of the user header: (tag, value, span).
    pub block3: Vec<(String, String, Range<usize>)>,
    /// Sub-blocks of the trailer.
    pub block5: Vec<(String, String, Range<usize>)>,
    pub fields: Vec<Field>,
}

impl Message {
    /// "103", from the application header.
    pub fn mt(&self) -> Option<&str> {
        let b2 = &self.block2.as_ref()?.0;
        b2.get(1..4)
            .filter(|t| t.bytes().all(|c| c.is_ascii_digit()))
    }
    pub fn field(&self, tag: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.tag == tag)
    }
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub messages: Vec<Message>,
    /// Whether the messages had header blocks.
    pub with_headers: bool,
    pub problems: Vec<String>,
}

/// `:20:` → "20"; `:32A:` → "32A". Two digits and an optional capital letter.
pub fn field_tag(line: &[u8]) -> Option<(&str, usize)> {
    if line.first() != Some(&b':') {
        return None;
    }
    let d = line.get(1..3)?;
    if !d.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let (end, colon) = match line.get(3) {
        Some(b':') => (3, 3),
        Some(c) if c.is_ascii_uppercase() && line.get(4) == Some(&b':') => (4, 4),
        _ => return None,
    };
    let tag = std::str::from_utf8(line.get(1..end)?).ok()?;
    Some((tag, colon + 1))
}

/// Whether `head` starts with a SWIFT basic header block (after separators).
pub fn starts_with_header(head: &[u8]) -> bool {
    let h = skip_separators(head, 0);
    head.get(h..).is_some_and(|r| {
        r.starts_with(b"{1:F01") || r.starts_with(b"{1:F21") || r.starts_with(b"{1:A01")
    })
}

/// The message type in the first application header `{2:I940...` / `{2:O940...` of `head`.
pub fn first_mt(head: &[u8]) -> Option<&str> {
    let at = memchr::memmem::find(head, b"{2:")?;
    let t = head.get(at + 4..at + 7)?;
    matches!(head.get(at + 3), Some(b'I' | b'O'))
        .then(|| std::str::from_utf8(t).ok())
        .flatten()
        .filter(|t| t.bytes().all(|c| c.is_ascii_digit()))
}

/// Tags of the lines of `head` that start a field (bounded to the probe).
pub fn head_tags(head: &[u8]) -> Vec<&str> {
    lines_from(head, common::bom(head))
        .filter_map(|(_, l)| field_tag(l).map(|(t, _)| t))
        .take(1000)
        .collect()
}

fn skip_separators(b: &[u8], mut pos: usize) -> usize {
    while let Some(&c) = b.get(pos) {
        if c.is_ascii_whitespace() || matches!(c, 0x01 | 0x03 | b'$' | 0) {
            pos += 1;
        } else if pos == 0 && b.starts_with(&[0xEF, 0xBB, 0xBF]) {
            pos += 3;
        } else {
            break;
        }
    }
    pos
}

/// Read every message of the file.
pub fn parse(bytes: &[u8], cancelled: &dyn Fn() -> bool) -> Parsed {
    let start = skip_separators(bytes, 0);
    if bytes.get(start) == Some(&b'{') {
        parse_with_headers(bytes, start, cancelled)
    } else {
        parse_bare(bytes, start, cancelled)
    }
}

/// Text blocks without headers: fields line by line, a line `-` ends a message.
fn parse_bare(bytes: &[u8], start: usize, cancelled: &dyn Fn() -> bool) -> Parsed {
    let mut out = Parsed::default();
    let mut current = Message::default();
    let mut stray = 0usize;
    let mut capped = false;
    for (i, (at, line)) in lines_from(bytes, start).enumerate() {
        if i % 4096 == 0 && cancelled() {
            out.problems.push("Cancelled".into());
            break;
        }
        let trimmed = line.trim_ascii();
        if trimmed == b"-" || trimmed == b"-}" {
            if !current.fields.is_empty() {
                current.span.end = at + line.len();
                out.messages.push(std::mem::take(&mut current));
                if out.messages.len() >= MAX_MESSAGES {
                    capped = true;
                    break;
                }
            }
            continue;
        }
        if !add_line(&mut current, at, line) && !trimmed.is_empty() {
            stray += 1;
        }
    }
    if !current.fields.is_empty() {
        out.messages.push(current);
    }
    if stray > 0 {
        out.problems
            .push(format!("{stray} line(s) outside any field were ignored"));
    }
    if capped {
        out.problems
            .push(format!("Only the first {MAX_MESSAGES} messages were read"));
    }
    out
}

/// Add one line of a text block to `m`: a new field, or the continuation of the last one.
/// False when the line belongs to no field.
fn add_line(m: &mut Message, at: usize, line: &[u8]) -> bool {
    if line.trim_ascii().is_empty() {
        return false;
    }
    if let Some((tag, value_at)) = field_tag(line) {
        if m.fields.is_empty() {
            m.span.start = at;
        }
        m.span.end = at + line.len();
        if m.fields.len() < MAX_FIELDS {
            m.fields.push(Field {
                tag: tag.to_owned(),
                value: common::text(line.get(value_at..).unwrap_or_default()).into_owned(),
                span: at..at + line.len(),
            });
        }
        return true;
    }
    match m.fields.last_mut() {
        Some(f) if f.span.end <= at => {
            f.value.push('\n');
            f.value.push_str(&common::text(line));
            f.span.end = at + line.len();
            m.span.end = f.span.end;
            true
        }
        _ => false,
    }
}

fn parse_with_headers(bytes: &[u8], start: usize, cancelled: &dyn Fn() -> bool) -> Parsed {
    let mut out = Parsed {
        with_headers: true,
        ..Parsed::default()
    };
    let mut pos = start;
    let mut skipped = 0usize;
    let mut unterminated = 0usize;
    while pos < bytes.len() {
        if out.messages.len().is_multiple_of(1024) && cancelled() {
            out.problems.push("Cancelled".into());
            break;
        }
        if out.messages.len() >= MAX_MESSAGES {
            out.problems
                .push(format!("Only the first {MAX_MESSAGES} messages were read"));
            break;
        }
        pos = skip_separators(bytes, pos);
        if pos >= bytes.len() {
            break;
        }
        if bytes.get(pos) != Some(&b'{') {
            // Something between messages: skip to the next basic header.
            skipped += 1;
            match memchr::memmem::find(bytes.get(pos..).unwrap_or_default(), b"{1:") {
                Some(i) => {
                    pos += i;
                    continue;
                }
                None => break,
            }
        }
        let mut m = Message {
            span: pos..pos,
            ..Message::default()
        };
        loop {
            let p = skip_inline_space(bytes, pos);
            if bytes.get(p) != Some(&b'{') {
                break;
            }
            let Some((id, content_at)) = block_id(bytes, p) else {
                break;
            };
            if id == "1" && m.block1.is_some() {
                break;
            }
            if id == "4" {
                let content = content_at;
                let (end, next) = match find_block4_end(bytes, content) {
                    Some(e) => (e, e + 2),
                    None => {
                        unterminated += 1;
                        (bytes.len(), bytes.len())
                    }
                };
                let body = bytes.get(content..end).unwrap_or_default();
                let mut text_block = Message::default();
                for (at, line) in lines_from(body, 0) {
                    add_line(&mut text_block, content + at, line);
                }
                // A line "-" before the end is the terminator itself, handled above.
                m.fields = text_block.fields;
                pos = next;
            } else {
                let Some(end) = matching_brace(bytes, p) else {
                    // Header blocks are short: skip what was scanned and look for the next message.
                    unterminated += 1;
                    pos = (p + MAX_BLOCK).min(bytes.len());
                    break;
                };
                let inner = bytes.get(content_at..end).unwrap_or_default();
                let value = common::text(inner).into_owned();
                match id {
                    "1" => m.block1 = Some((value, content_at..end)),
                    "2" => m.block2 = Some((value, content_at..end)),
                    "3" => m.block3 = sub_blocks(bytes, content_at, end),
                    "5" => m.block5 = sub_blocks(bytes, content_at, end),
                    _ => {}
                }
                pos = end + 1;
            }
            m.span.end = pos.min(bytes.len());
        }
        if m.block1.is_none()
            && m.block2.is_none()
            && m.block3.is_empty()
            && m.block5.is_empty()
            && m.fields.is_empty()
        {
            // Not a block after all: skip the brace.
            skipped += 1;
            pos = pos.max(m.span.start + 1);
            continue;
        }
        out.messages.push(m);
    }
    if skipped > 0 {
        out.problems.push(format!(
            "{skipped} stretch(es) of text between messages were ignored"
        ));
    }
    if unterminated > 0 {
        out.problems.push(format!(
            "{unterminated} block(s) are not closed (the file may be cut)"
        ));
    }
    out
}

fn skip_inline_space(b: &[u8], mut pos: usize) -> usize {
    while matches!(b.get(pos), Some(b'\r' | b'\n' | b' ' | b'\t')) {
        pos += 1;
    }
    pos
}

/// `{4:` at `p`: ("4", offset after the colon). Block identifiers are 1–3 letters or digits.
fn block_id(b: &[u8], p: usize) -> Option<(&str, usize)> {
    let rest = b.get(p + 1..)?;
    let colon = rest.iter().take(4).position(|&c| c == b':')?;
    let id = rest.get(..colon)?;
    if id.is_empty() || !id.iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    Some((std::str::from_utf8(id).ok()?, p + 1 + colon + 1))
}

/// End of the text block: the `-}` that starts a line (or the block).
fn find_block4_end(b: &[u8], content: usize) -> Option<usize> {
    let body = b.get(content..)?;
    let mut from = 0;
    while let Some(i) = memchr::memmem::find(body.get(from..)?, b"-}") {
        let at = from + i;
        if at == 0 || matches!(body.get(at - 1), Some(b'\n' | b'\r')) {
            return Some(content + at);
        }
        from = at + 2;
    }
    None
}

/// The `}` closing the `{` at `p`, counting nested blocks (`{3:{108:X}}`).
fn matching_brace(b: &[u8], p: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &c) in b.get(p..)?.iter().take(MAX_BLOCK).enumerate() {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(p + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// `{108:REF}{121:UETR}` inside a block.
fn sub_blocks(b: &[u8], from: usize, to: usize) -> Vec<(String, String, Range<usize>)> {
    let mut out = vec![];
    let mut pos = from;
    while pos < to && out.len() < 100 {
        let Some(open) = b
            .get(pos..to)
            .and_then(|r| r.iter().position(|&c| c == b'{'))
        else {
            break;
        };
        let p = pos + open;
        let Some((tag, value_at)) = block_id(b, p) else {
            pos = p + 1;
            continue;
        };
        let end = matching_brace(b, p).unwrap_or(to).min(to);
        let value = common::text(b.get(value_at..end).unwrap_or_default()).into_owned();
        out.push((tag.to_owned(), value, p..(end + 1).min(to)));
        pos = end + 1;
    }
    out
}

// ----- names -----

pub fn mt_name(mt: &str) -> Option<&'static str> {
    Some(match mt {
        "101" => "Request for transfer",
        "103" => "Single customer credit transfer",
        "104" => "Direct debit and request for debit transfer",
        "105" => "EDIFACT envelope",
        "107" => "General direct debit message",
        "110" => "Advice of cheque(s)",
        "111" => "Request for stop payment of a cheque",
        "112" => "Status of a request for stop payment of a cheque",
        "190" => "Advice of charges, interest and other adjustments",
        "191" => "Request for payment of charges, interest and other expenses",
        "192" => "Request for cancellation",
        "195" => "Queries",
        "196" => "Answers",
        "198" => "Proprietary message",
        "199" => "Free format message",
        "200" => "Financial institution transfer for its own account",
        "201" => "Multiple financial institution transfer for its own account",
        "202" => "General financial institution transfer",
        "203" => "Multiple general financial institution transfer",
        "204" => "Financial markets direct debit message",
        "205" => "Financial institution transfer execution",
        "210" => "Notice to receive",
        "292" => "Request for cancellation",
        "295" => "Queries",
        "296" => "Answers",
        "299" => "Free format message",
        "300" => "Foreign exchange confirmation",
        "320" => "Fixed loan/deposit confirmation",
        "380" => "Foreign exchange order",
        "502" => "Order to buy or sell",
        "509" => "Trade status message",
        "515" => "Client confirmation of purchase or sale",
        "535" => "Statement of holdings",
        "536" => "Statement of transactions",
        "537" => "Statement of pending transactions",
        "540" => "Receive free",
        "541" => "Receive against payment",
        "542" => "Deliver free",
        "543" => "Deliver against payment",
        "544" => "Receive free confirmation",
        "545" => "Receive against payment confirmation",
        "546" => "Deliver free confirmation",
        "547" => "Deliver against payment confirmation",
        "548" => "Settlement status and processing advice",
        "564" => "Corporate action notification",
        "566" => "Corporate action confirmation",
        "700" => "Issue of a documentary credit",
        "707" => "Amendment to a documentary credit",
        "760" => "Guarantee / standby letter of credit",
        "799" => "Free format message",
        "900" => "Confirmation of debit",
        "910" => "Confirmation of credit",
        "920" => "Request message",
        "940" => "Customer statement message",
        "941" => "Balance report",
        "942" => "Interim transaction report",
        "950" => "Statement message",
        "970" => "Netting statement",
        "999" => "Free format message",
        _ => return None,
    })
}

/// Field names, by tag with its option letter, then by the two digits.
pub fn field_name(mt: Option<&str>, tag: &str) -> &'static str {
    let specific = match (mt, tag) {
        (Some("202" | "205" | "200"), "58A" | "58D") => Some("Beneficiary institution"),
        (Some("940" | "942" | "950"), "20") => Some("Transaction reference number"),
        (Some("900" | "910"), "32A") => Some("Value date, currency code, amount"),
        (Some("942"), "34F") => Some("Floor limit indicator"),
        _ => None,
    };
    if let Some(s) = specific {
        return s;
    }
    let exact = match tag {
        "13C" => "Time indication",
        "13D" => "Date/time indication",
        "20" => "Sender's reference",
        "20C" => "Reference",
        "21" => "Related reference",
        "21R" => "Customer specified reference",
        "23B" => "Bank operation code",
        "23E" => "Instruction code",
        "25" => "Account identification",
        "25P" => "Account identification (with BIC)",
        "26T" => "Transaction type code",
        "28" => "Statement number/page number",
        "28C" => "Statement number/sequence number",
        "28D" => "Message index/total",
        "30" => "Date",
        "30T" => "Trade date",
        "30V" => "Value date",
        "32A" => "Value date/currency/interbank settled amount",
        "32B" => "Currency/amount",
        "33B" => "Currency/instructed amount",
        "34F" => "Floor limit indicator",
        "36" => "Exchange rate",
        "50A" | "50F" | "50K" | "50" => "Ordering customer",
        "50C" | "50L" => "Instructing party",
        "50G" | "50H" => "Ordering customer",
        "51A" => "Sending institution",
        "52A" | "52B" | "52C" | "52D" => "Ordering institution",
        "53A" | "53B" | "53D" => "Sender's correspondent",
        "54A" | "54B" | "54D" => "Receiver's correspondent",
        "55A" | "55B" | "55D" => "Third reimbursement institution",
        "56A" | "56C" | "56D" => "Intermediary institution",
        "57A" | "57B" | "57C" | "57D" => "Account with institution",
        "58A" | "58D" => "Beneficiary institution",
        "59" | "59A" | "59F" => "Beneficiary customer",
        "60F" => "Opening balance (first)",
        "60M" => "Opening balance (intermediate)",
        "61" => "Statement line",
        "62F" => "Closing balance (booked, final)",
        "62M" => "Closing balance (booked, intermediate)",
        "64" => "Closing available balance",
        "65" => "Forward available balance",
        "70" => "Remittance information",
        "71A" => "Details of charges",
        "71F" => "Sender's charges",
        "71G" => "Receiver's charges",
        "72" => "Sender to receiver information",
        "75" => "Queries",
        "76" => "Answers",
        "77A" => "Narrative",
        "77B" => "Regulatory reporting",
        "77E" => "Proprietary message",
        "77T" => "Envelope contents",
        "79" => "Narrative",
        "86" => "Information to account owner",
        "90C" => "Number and sum of entries (credits)",
        "90D" => "Number and sum of entries (debits)",
        _ => "",
    };
    if !exact.is_empty() {
        return exact;
    }
    match tag.get(..2) {
        Some("50") => "Ordering customer",
        Some("52") => "Ordering institution",
        Some("53") => "Sender's correspondent",
        Some("54") => "Receiver's correspondent",
        Some("56") => "Intermediary institution",
        Some("57") => "Account with institution",
        Some("58") => "Beneficiary institution",
        Some("59") => "Beneficiary customer",
        _ => "",
    }
}

// ----- values -----

/// A balance field (`:60F:`, `:62F:`, `:64:`): D/C mark, date, currency, amount.
#[derive(Clone, Debug)]
pub struct Balance {
    pub credit: bool,
    pub date: String,
    pub currency: String,
    pub amount: Dec,
}
impl Balance {
    pub fn parse(v: &str) -> Option<Balance> {
        let v = v.lines().next()?.trim();
        let credit = match v.get(..1)? {
            "C" => true,
            "D" => false,
            _ => return None,
        };
        let date = yymmdd(v.get(1..7)?)?;
        let currency = currency(v.get(7..10)?)?;
        let amount = comma_decimal(v.get(10..)?)?;
        Some(Balance {
            credit,
            date,
            currency,
            amount,
        })
    }
    pub fn signed(&self) -> Dec {
        if self.credit {
            self.amount
        } else {
            self.amount.negated()
        }
    }
    pub fn readable(&self) -> String {
        format!(
            "{} {} {} ({})",
            self.date,
            self.currency,
            self.amount.format(2),
            if self.credit { "credit" } else { "debit" }
        )
    }
}

fn currency(s: &str) -> Option<String> {
    (s.len() == 3 && s.bytes().all(|c| c.is_ascii_uppercase())).then(|| s.to_owned())
}

/// "EUR1234,56" → ("EUR", 1234.56).
pub fn currency_amount(v: &str) -> Option<(String, Dec)> {
    let v = v.trim();
    Some((currency(v.get(..3)?)?, comma_decimal(v.get(3..)?)?))
}

/// A readable form of a field's value, when the field has a known layout ("" otherwise).
pub fn interpret(tag: &str, value: &str) -> String {
    let first = value.lines().next().unwrap_or("").trim();
    let r: Option<String> = match tag {
        "32A" => date_amount(first),
        "30" | "30V" | "30T" => yymmdd(first),
        "32B" | "33B" | "71F" | "71G" | "32M" | "33A" => {
            currency_amount(first).map(|(c, a)| format!("{c} {}", a.format(2)))
        }
        "60F" | "60M" | "62F" | "62M" | "64" | "65" => Balance::parse(first).map(|b| b.readable()),
        "90C" | "90D" => Totals::parse(first)
            .map(|t| format!("{} entries, {} {}", t.count, t.currency, t.amount.format(2))),
        "34F" => FloorLimit::parse(first).map(|f| {
            format!(
                "{} {}{}",
                f.currency,
                f.amount.format(2),
                match f.mark.as_str() {
                    "D" => " (debit)",
                    "C" => " (credit)",
                    _ => "",
                }
            )
        }),
        "13D" => date_time(first),
        "28C" | "28" => {
            let mut parts = first.split('/');
            let n = parts.next().and_then(|n| n.parse::<u64>().ok());
            let s = parts.next().and_then(|s| s.parse::<u64>().ok());
            match (n, s) {
                (Some(n), Some(s)) => Some(format!("statement {n}, sequence {s}")),
                (Some(n), None) => Some(format!("statement {n}")),
                _ => None,
            }
        }
        "36" => comma_decimal(first).map(|d| d.format(0)),
        "71A" => match first {
            "OUR" => Some("All charges paid by the ordering customer".into()),
            "SHA" => Some("Charges shared".into()),
            "BEN" => Some("All charges paid by the beneficiary".into()),
            _ => None,
        },
        "23B" => match first {
            "CRED" => Some("Normal credit transfer".into()),
            "CRTS" => Some("Test message".into()),
            "SPAY" => Some("SWIFTPay".into()),
            "SPRI" => Some("Priority".into()),
            "SSTD" => Some("Standard".into()),
            _ => None,
        },
        "61" => StatementLine::parse(value).map(|l| l.summary()),
        _ => None,
    };
    r.unwrap_or_default()
}

fn date_amount(v: &str) -> Option<String> {
    let date = yymmdd(v.get(..6)?)?;
    let (ccy, amt) = currency_amount(v.get(6..)?)?;
    Some(format!("{date}, {ccy} {}", amt.format(2)))
}

/// `:13D:` "2610311200+0100" as "2026-10-31 12:00 UTC+01:00".
pub fn date_time(v: &str) -> Option<String> {
    let date = yymmdd(v.get(..6)?)?;
    let time = common::hhmm(v.get(6..10)?)?;
    let sign = v.get(10..11).filter(|s| *s == "+" || *s == "-")?;
    let off = common::hhmm(v.get(11..15)?)?;
    Some(format!("{date} {time} UTC{sign}{off}"))
}

/// `:90D:`/`:90C:` "3EUR1234,56": number of entries, currency, sum.
pub struct Totals {
    pub count: u64,
    pub currency: String,
    pub amount: Dec,
}
impl Totals {
    pub fn parse(v: &str) -> Option<Totals> {
        let v = v.trim();
        let digits = v.bytes().take_while(u8::is_ascii_digit).count();
        let count = v.get(..digits)?.parse().ok()?;
        let (currency, amount) = currency_amount(v.get(digits..)?)?;
        Some(Totals {
            count,
            currency,
            amount,
        })
    }
}

/// `:34F:` "EURD100,": currency, optional D/C mark, amount.
pub struct FloorLimit {
    pub currency: String,
    pub mark: String,
    pub amount: Dec,
}
impl FloorLimit {
    pub fn parse(v: &str) -> Option<FloorLimit> {
        let v = v.trim();
        let currency = currency(v.get(..3)?)?;
        let (mark, amount) = match v.get(3..4)? {
            m @ ("D" | "C") => (m.to_owned(), v.get(4..)?),
            _ => (String::new(), v.get(3..)?),
        };
        Some(FloorLimit {
            currency,
            mark,
            amount: comma_decimal(amount)?,
        })
    }
}

/// A `:61:` statement line.
#[derive(Clone, Debug, Default)]
pub struct StatementLine {
    pub value_date: String,
    pub entry_date: String,
    /// D, C, RD, RC (and EC, ED for expected entries).
    pub mark: String,
    pub funds_code: String,
    pub amount: Dec,
    pub amount_written: String,
    /// "NTRF", "S103".
    pub kind: String,
    pub customer_ref: String,
    pub bank_ref: String,
    pub supplementary: String,
}

impl StatementLine {
    pub fn parse(value: &str) -> Option<StatementLine> {
        let mut lines = value.lines();
        let s = lines.next()?.trim_end();
        let supplementary = lines.collect::<Vec<_>>().join(" ");
        let b = s.as_bytes();
        let value_raw = s.get(..6)?;
        let value_date = yymmdd(value_raw)?;
        let mut i = 6;
        let mut entry_date = String::new();
        if b.get(6..10)
            .is_some_and(|d| d.iter().all(u8::is_ascii_digit))
        {
            let mmdd = s.get(6..10)?;
            entry_date = entry_with_year(value_raw, mmdd).unwrap_or_else(|| mmdd.to_owned());
            i = 10;
        }
        let mark = ["RC", "RD", "EC", "ED", "C", "D"]
            .into_iter()
            .find(|m| s.get(i..).is_some_and(|r| r.starts_with(m)))?;
        i += mark.len();
        let mut funds_code = String::new();
        if b.get(i).is_some_and(u8::is_ascii_alphabetic) {
            funds_code = s.get(i..i + 1)?.to_owned();
            i += 1;
        }
        let amount_len = b
            .get(i..)?
            .iter()
            .take(15)
            .take_while(|c| c.is_ascii_digit() || **c == b',')
            .count();
        let amount_written = s.get(i..i + amount_len)?.to_owned();
        let amount = comma_decimal(&amount_written)?;
        i += amount_len;
        let kind_end = (i + 4).min(s.len());
        let kind = s.get(i..kind_end).unwrap_or("").to_owned();
        let refs = s.get(kind_end..).unwrap_or("");
        let (customer_ref, bank_ref) = match refs.split_once("//") {
            Some((c, b)) => (c.to_owned(), b.to_owned()),
            None => (refs.to_owned(), String::new()),
        };
        Some(StatementLine {
            value_date,
            entry_date,
            mark: mark.to_owned(),
            funds_code,
            amount,
            amount_written,
            kind,
            customer_ref,
            bank_ref,
            supplementary,
        })
    }
    /// Credit entries add to the balance: C, RD (reversal of a debit) and EC.
    pub fn is_credit(&self) -> bool {
        matches!(self.mark.as_str(), "C" | "RD" | "EC")
    }
    pub fn signed(&self) -> Dec {
        if self.is_credit() {
            self.amount
        } else {
            self.amount.negated()
        }
    }
    pub fn mark_name(&self) -> &'static str {
        match self.mark.as_str() {
            "C" => "Credit",
            "D" => "Debit",
            "RC" => "Reversal of credit",
            "RD" => "Reversal of debit",
            "EC" => "Expected credit",
            "ED" => "Expected debit",
            _ => "",
        }
    }
    fn summary(&self) -> String {
        format!(
            "{} {}, value {}{}{}",
            self.mark_name(),
            self.amount.format(2),
            self.value_date,
            if self.kind.is_empty() { "" } else { ", " },
            self.kind
        )
    }
}

/// The entry date MMDD with the year of the value date (YYMMDD), across a year end.
fn entry_with_year(value: &str, mmdd: &str) -> Option<String> {
    let yy: i32 = value.get(..2)?.parse().ok()?;
    let vm: u32 = value.get(2..4)?.parse().ok()?;
    let em: u32 = mmdd.get(..2)?.parse().ok()?;
    let year = if yy >= 80 { 1900 + yy } else { 2000 + yy };
    let year = match (vm, em) {
        (12, 1) => year + 1,
        (1, 12) => year - 1,
        _ => year,
    };
    let d = format!("{:02}{mmdd}", year.rem_euclid(100));
    yymmdd(&d).map(|s| format!("{year}{}", s.get(4..).unwrap_or("")))
}

// ----- header facts -----

/// The BIC of a 12-character logical terminal address (BIC8, terminal code, branch).
fn bic_of(lt: &str) -> Option<String> {
    let bic8 = lt.get(..8)?;
    let branch = lt.get(9..12)?;
    Some(format!("{bic8}{branch}"))
}

pub struct Header {
    pub direction: &'static str,
    pub mt: String,
    pub sender: Option<String>,
    pub receiver: Option<String>,
    pub priority: Option<&'static str>,
}

/// What blocks 1 and 2 say, as facts in `group`; the header summary is returned.
pub fn header_facts(m: &Message, out: &mut Inspection, group: &str) -> Header {
    let mut h = Header {
        direction: "",
        mt: m.mt().unwrap_or("").to_owned(),
        sender: None,
        receiver: None,
        priority: None,
    };
    let src = |r: &Range<usize>| span(r.start, r.end - r.start);
    let mut own_lt: Option<String> = None;
    if let Some((b1, r)) = &m.block1 {
        let app = match b1.get(..1) {
            Some("F") => "F (FIN)",
            Some("A") => "A (general purpose)",
            Some("L") => "L (login)",
            _ => "",
        };
        out.fact(group, "Application", app, src(r));
        out.fact(group, "Service", b1.get(1..3).unwrap_or(""), src(r));
        own_lt = b1.get(3..15).map(str::to_owned);
        out.fact(
            group,
            "Session / sequence",
            match (b1.get(15..19), b1.get(19..25)) {
                (Some(s), Some(q)) => format!("{s} / {q}"),
                _ => String::new(),
            },
            src(r),
        );
    }
    if let Some((b2, r)) = &m.block2 {
        let pri = |c: Option<&str>| match c {
            Some("N") => Some("N (normal)"),
            Some("U") => Some("U (urgent)"),
            Some("S") => Some("S (system)"),
            _ => None,
        };
        match b2.get(..1) {
            Some("I") => {
                h.direction = "Input (sent to SWIFT)";
                h.sender = own_lt.as_deref().and_then(bic_of);
                h.receiver = b2.get(4..16).and_then(bic_of);
                h.priority = pri(b2.get(16..17));
            }
            Some("O") => {
                h.direction = "Output (delivered by SWIFT)";
                h.receiver = own_lt.as_deref().and_then(bic_of);
                h.sender = b2.get(14..26).and_then(bic_of);
                h.priority = pri(b2.get(46..47));
                if let (Some(d), Some(t)) = (b2.get(36..42), b2.get(42..46))
                    && let (Some(d), Some(t)) = (yymmdd(d), common::hhmm(t))
                {
                    out.fact(group, "Output date", format!("{d} {t}"), src(r));
                }
                if let Some(mir) = b2.get(8..36) {
                    out.fact(group, "Message input reference", mir, src(r));
                }
            }
            _ => {}
        }
        out.fact(group, "Direction", h.direction, src(r));
        let name = mt_name(&h.mt).unwrap_or("");
        out.fact(
            group,
            "Message type",
            if name.is_empty() {
                format!("MT{}", h.mt)
            } else {
                format!("MT{} — {name}", h.mt)
            },
            src(r),
        );
        out.fact(
            group,
            "Sender (BIC)",
            h.sender.clone().unwrap_or_default(),
            src(r),
        );
        out.fact(
            group,
            "Receiver (BIC)",
            h.receiver.clone().unwrap_or_default(),
            src(r),
        );
        out.fact(group, "Priority", h.priority.unwrap_or(""), src(r));
    } else if let Some(lt) = &own_lt {
        out.fact(
            group,
            "Logical terminal (BIC)",
            bic_of(lt).unwrap_or_default(),
            m.block1.as_ref().and_then(|(_, r)| src(r)),
        );
    }
    for (tag, value, r) in &m.block3 {
        let label = match tag.as_str() {
            "103" => "Service identifier",
            "108" => "Message user reference",
            "111" => "Service type identifier",
            "113" => "Banking priority",
            "119" => "Validation flag",
            "121" => "UETR",
            "165" => "Payment release information",
            "433" => "Sanctions screening",
            "434" => "Payment controls",
            _ => "",
        };
        if !label.is_empty() {
            out.fact(group, label, value.clone(), src(r));
        } else {
            out.fact(
                group,
                &format!("User header {{{tag}:}}"),
                value.clone(),
                src(r),
            );
        }
    }
    for (tag, value, r) in &m.block5 {
        let label = match tag.as_str() {
            "CHK" => "Checksum",
            "MAC" => "Authentication code",
            "PDE" => "Possible duplicate emission",
            "PDM" => "Possible duplicate message",
            "DLM" => "Delayed message",
            "TNG" => "Training",
            "MRF" => "Message reference",
            "SYS" => "System originated message",
            _ => "",
        };
        let label = if label.is_empty() {
            format!("Trailer {{{tag}:}}")
        } else {
            label.to_owned()
        };
        // Present-but-empty trailers (`{PDE:}`) still mean something.
        let v = if value.is_empty() { "present" } else { value };
        out.fact(group, &label, v, src(r));
    }
    h
}

/// The table of every field of every message.
pub fn fields_table(parsed: &Parsed, out: &mut Inspection) {
    let many = parsed.messages.len() > 1;
    let mut cols = vec![];
    if many {
        cols.push("Message");
    }
    cols.extend(["Tag", "Field", "Value", "Interpreted"]);
    let mut t = Capped::new("Fields", &cols);
    for (n, m) in parsed.messages.iter().enumerate() {
        for f in &m.fields {
            if t.full() {
                t.skip();
                continue;
            }
            let mut cells = vec![];
            if many {
                cells.push((n + 1).to_string());
            }
            cells.push(f.tag.clone());
            cells.push(field_name(m.mt(), &f.tag).to_owned());
            cells.push(f.value.clone());
            cells.push(interpret(&f.tag, &f.value));
            t.push(cells, span(f.span.start, f.span.end - f.span.start));
        }
    }
    t.finish(out);
}

pub struct SwiftMt;

impl Handler for SwiftMt {
    fn name(&self) -> &'static str {
        "SWIFT MT"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if starts_with_header(probe.head) {
            return true;
        }
        // A bare text block: the first line is `:20:`, and more fields follow.
        let head = common::trimmed_head(probe.head);
        if !head.starts_with(b":20:") {
            return false;
        }
        let tags = head_tags(probe.head);
        tags.len() >= 3
            && (probe.ext_is(&["mt", "fin", "txt", "swift", "sta", "dat"]) || tags.len() >= 4)
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let parsed = parse(input.bytes, cancelled);
        if parsed.messages.is_empty() {
            return Err("No SWIFT MT message was found".into());
        }
        let mut out = Inspection::new("SWIFT MT", "SWIFT MT message");
        let first_mt = parsed.messages.first().and_then(|m| m.mt()).unwrap_or("");
        let same_type = parsed
            .messages
            .iter()
            .all(|m| m.mt().unwrap_or("") == first_mt);
        out.kind = match (parsed.messages.len(), first_mt) {
            (1, "") => "SWIFT MT text block (no headers)".into(),
            (n, mt) if !mt.is_empty() && same_type => {
                let name = mt_name(mt).unwrap_or("message");
                if n == 1 {
                    format!("MT{mt} — {name}")
                } else {
                    format!("MT{mt} — {name} ({n} messages)")
                }
            }
            (n, _) => format!("SWIFT MT messages ({n})"),
        };
        if parsed.with_headers {
            out.version = Some("FIN".into());
        }
        let many = parsed.messages.len() > 1;
        out.fact("File", "Messages", parsed.messages.len().to_string(), None);
        let mut summary = Capped::new(
            "Messages",
            &[
                "Message",
                "Type",
                "Sender",
                "Receiver",
                "Reference (20)",
                "Amount (32A/32B)",
                "Fields",
            ],
        );
        for (n, m) in parsed.messages.iter().enumerate() {
            if n % 1024 == 0 && cancelled() {
                break;
            }
            let group = if many {
                format!("Message {}", n + 1)
            } else {
                "Message".into()
            };
            let h = if n < FACT_MESSAGES {
                header_facts(m, &mut out, &group)
            } else {
                header_facts(m, &mut Inspection::default(), &group)
            };
            if n < FACT_MESSAGES {
                for tag in ["20", "21", "32A", "33B", "71A"] {
                    if let Some(f) = m.field(tag) {
                        let i = interpret(tag, &f.value);
                        let v = if i.is_empty() {
                            f.value.clone()
                        } else {
                            format!("{i} (as written: {})", f.value)
                        };
                        out.fact(
                            &group,
                            field_name(m.mt(), tag),
                            v,
                            span(f.span.start, f.span.end - f.span.start),
                        );
                    }
                }
            }
            if many && !summary.full() {
                let amount = m
                    .field("32A")
                    .or_else(|| m.field("32B"))
                    .map(|f| interpret(&f.tag, &f.value))
                    .unwrap_or_default();
                summary.push(
                    vec![
                        (n + 1).to_string(),
                        if h.mt.is_empty() {
                            String::new()
                        } else {
                            format!("MT{}", h.mt)
                        },
                        h.sender.unwrap_or_default(),
                        h.receiver.unwrap_or_default(),
                        m.field("20").map(|f| f.value.clone()).unwrap_or_default(),
                        amount,
                        m.fields.len().to_string(),
                    ],
                    span(m.span.start, m.span.end.saturating_sub(m.span.start)),
                );
            }
        }
        if parsed.messages.len() > FACT_MESSAGES {
            out.notes.push(format!(
                "Header facts are listed for the first {FACT_MESSAGES} messages; the Messages table lists all"
            ));
        }
        summary.finish(&mut out);
        fields_table(&parsed, &mut out);
        let total_fields: usize = parsed.messages.iter().map(|m| m.fields.len()).sum();
        if total_fields > MAX_ROWS {
            out.notes
                .push(format!("Only the first {MAX_ROWS} fields are listed"));
        }
        common::parsed(
            &mut out,
            "Header blocks and text-block fields read",
            &parsed.problems,
        );
        out.check(
            Stage::Schema,
            State::NotDone,
            "Field formats, mandatory fields and network validated rules are not checked",
        );
        out.notes.push(
            "Years in SWIFT dates have two digits: 80–99 are read as 1980–1999, the others as 2000–2079"
                .into(),
        );
        out.notes.push(
            "Amounts use a comma as the decimal separator; the interpreted column rewrites them with a point, the value column keeps them as written"
                .into(),
        );
        Ok(out)
    }
}
