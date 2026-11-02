//! BAI2 cash management balance reports (BAI, now X9.121): file `01`, group `02`, account `03`
//! with balances and summaries, transaction details `16`, continuations `88` (merged into the
//! record they continue), and the trailers `49`, `98`, `99` whose control totals and record
//! counts are compared with what the file contains. Amounts are whole minor units (cents).
use super::common::{self, Capped, Dec, Tally, lines_from, span, yymmdd};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};
use std::ops::Range;

/// Groups whose header facts are listed one by one.
const FACT_GROUPS: usize = 20;
/// Distributions read from a `D` funds type.
const MAX_DISTRIBUTIONS: usize = 100;
/// Continuation records merged into one record.
const MAX_CONTINUATIONS: usize = 1000;

pub struct Bai2;

impl Handler for Bai2 {
    fn name(&self) -> &'static str {
        "BAI2"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let head = common::trimmed_head(probe.head);
        if !head.starts_with(b"01,") {
            return false;
        }
        let mut lines = lines_from(head, 0).map(|(_, l)| l);
        let Some(first) = lines.next() else {
            return false;
        };
        // 01,sender,receiver,date,time,file id,record length,block size,version/
        first.iter().filter(|&&c| c == b',').count() >= 6
            && lines
                .take(50)
                .any(|l| l.starts_with(b"02,") || l.starts_with(b"99,"))
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let records = read_records(input.bytes, cancelled)?;
        let mut out = Inspection::new("BAI2", "BAI2 — cash management balance report");
        let mut r = Reader::new();
        for rec in &records.list {
            r.record(rec, &mut out);
        }
        r.finish(&mut out, records.problems)
    }
}

struct Record {
    code: String,
    /// Content after the record code, the trailing `/` removed, continuations merged.
    content: String,
    span: Range<usize>,
    /// Physical records (1 + continuations).
    physical: u64,
}

struct Records {
    list: Vec<Record>,
    problems: Vec<String>,
}

fn strip_slash(s: &str) -> &str {
    let s = s.trim_end();
    s.strip_suffix('/').unwrap_or(s)
}

fn read_records(bytes: &[u8], cancelled: &dyn Fn() -> bool) -> Result<Records, String> {
    let mut list: Vec<Record> = vec![];
    // Continuation contents of the last record, merged when the next record starts.
    let mut conts: Vec<String> = vec![];
    let mut orphans = 0usize;
    let mut problems = vec![];
    for (i, (at, line)) in lines_from(bytes, common::bom(bytes)).enumerate() {
        if i % 8192 == 0 && cancelled() {
            return Err("Cancelled".into());
        }
        let text = common::text(line);
        let t = text.trim();
        if t.is_empty() {
            continue;
        }
        let (code, rest) = t.split_once(',').unwrap_or((t, ""));
        let end = at + line.len();
        if code == "88" {
            match list.last_mut() {
                Some(last) if conts.len() < MAX_CONTINUATIONS => {
                    conts.push(strip_slash(rest).to_owned());
                    last.span.end = end;
                    last.physical += 1;
                }
                Some(last) => {
                    last.physical += 1;
                    last.span.end = end;
                }
                None => orphans += 1,
            }
            continue;
        }
        if let Some(last) = list.last_mut() {
            merge(last, std::mem::take(&mut conts));
        }
        list.push(Record {
            code: code.to_owned(),
            content: strip_slash(rest).to_owned(),
            span: at..end,
            physical: 1,
        });
    }
    if let Some(last) = list.last_mut() {
        merge(last, conts);
    }
    if orphans > 0 {
        problems.push(format!(
            "{orphans} continuation record(s) (88) before any record were ignored"
        ));
    }
    if list.is_empty() {
        return Err("No BAI2 record was found".into());
    }
    Ok(Records { list, problems })
}

/// Merge continuation records: further fields, or more text for a transaction detail whose
/// text field has started.
fn merge(rec: &mut Record, conts: Vec<String>) {
    if conts.is_empty() {
        return;
    }
    let text_started = rec.code == "16" && Detail::parse(&rec.content).is_some_and(|d| d.has_text);
    let sep = if text_started { " " } else { "," };
    for c in conts {
        rec.content.push_str(sep);
        rec.content.push_str(&c);
    }
}

/// Amount in minor units, with an optional sign; `None` when empty or not a number.
fn minor(s: &str) -> Option<i128> {
    let s = s.trim();
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if digits.is_empty() || digits.len() > 20 || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let v: i128 = digits.parse().ok()?;
    Some(if neg { -v } else { v })
}

/// Decimals of a currency's minor unit (ISO 4217), for showing amounts in cents as decimals.
fn decimals(currency: &str) -> u32 {
    match currency {
        "JPY" | "KRW" | "CLP" | "ISK" | "VND" | "XOF" | "XAF" | "XPF" | "PYG" | "UGX" | "RWF"
        | "KMF" | "GNF" | "DJF" | "VUV" => 0,
        "BHD" | "KWD" | "OMR" | "JOD" | "TND" | "IQD" | "LYD" => 3,
        _ => 2,
    }
}

fn show(units: i128, currency: &str) -> String {
    let d = decimals(currency);
    Dec::from_minor(units, d)
        .map(|v| v.format(d as usize))
        .unwrap_or_else(|| units.to_string())
}

fn funds_name(code: &str) -> &'static str {
    match code {
        "0" => "0 (immediate availability)",
        "1" => "1 (one-day availability)",
        "2" => "2 (two or more days)",
        "S" => "S (distributed availability)",
        "V" => "V (value dated)",
        "D" => "D (distributed availability, days)",
        "Z" => "Z (unknown)",
        _ => "",
    }
}

/// Fields that follow a funds type code: S has three amounts, V a date and a time, D a count
/// and as many (days, amount) pairs.
fn funds_extra(code: &str, fields: &[&str], at: usize) -> usize {
    match code.trim() {
        "S" => 3,
        "V" => 2,
        "D" => {
            let n = fields
                .get(at)
                .and_then(|n| n.trim().parse::<usize>().ok())
                .unwrap_or(0)
                .min(MAX_DISTRIBUTIONS);
            1 + 2 * n
        }
        _ => 0,
    }
}

/// A transaction detail (16).
struct Detail {
    code: String,
    amount: Option<i128>,
    funds: String,
    bank_ref: String,
    customer_ref: String,
    text: String,
    has_text: bool,
}

impl Detail {
    fn parse(content: &str) -> Option<Detail> {
        let f: Vec<&str> = content.split(',').collect();
        let code = f.first()?.trim().to_owned();
        let amount = f.get(1).and_then(|a| minor(a));
        let funds = f.get(2).map(|s| s.trim()).unwrap_or("").to_owned();
        let at = 3 + funds_extra(&funds, &f, 3);
        let bank_ref = f.get(at).map(|s| s.trim()).unwrap_or("").to_owned();
        let customer_ref = f.get(at + 1).map(|s| s.trim()).unwrap_or("").to_owned();
        let has_text = f.len() > at + 2;
        let text = f.get(at + 2..).map(|t| t.join(",")).unwrap_or_default();
        Some(Detail {
            code,
            amount,
            funds,
            bank_ref,
            customer_ref,
            text: text.trim().to_owned(),
            has_text,
        })
    }
}

/// Balances and summaries of an account identifier (03): (type code, amount, item count, funds).
fn summaries(fields: &[&str]) -> Vec<(String, Option<i128>, String, String)> {
    let mut out = vec![];
    let mut i = 2;
    while i < fields.len() && out.len() < 10_000 {
        let code = fields.get(i).map(|s| s.trim()).unwrap_or("");
        if code.is_empty() {
            break;
        }
        let amount = fields.get(i + 1).and_then(|a| minor(a));
        let count = fields.get(i + 2).map(|s| s.trim()).unwrap_or("").to_owned();
        let funds = fields.get(i + 3).map(|s| s.trim()).unwrap_or("").to_owned();
        out.push((code.to_owned(), amount, count, funds.clone()));
        i += 4 + funds_extra(&funds, fields, i + 4);
    }
    out
}

/// Whether a type code is a balance, a credit or a debit, and its name when it is a common one.
fn type_class(code: &str) -> &'static str {
    match code.parse::<u32>() {
        Ok(1..=99) => "Balance",
        Ok(100..=399) => "Credit",
        Ok(400..=699) => "Debit",
        Ok(700..=799) => "Loan",
        Ok(890) => "Non-monetary",
        Ok(900..=999) => "Custom",
        _ => "",
    }
}

fn type_name(code: &str) -> &'static str {
    match code {
        "010" => "Opening ledger",
        "011" => "Average opening ledger MTD",
        "012" => "Average opening ledger YTD",
        "015" => "Closing ledger",
        "020" => "Average closing ledger MTD",
        "021" => "Average closing ledger previous month",
        "025" => "Average closing ledger YTD",
        "030" => "Current ledger",
        "037" => "ACH net position",
        "039" => "Opening available + total same-day ACH DTC deposit",
        "040" => "Opening available",
        "041" => "Average opening available MTD",
        "042" => "Average opening available YTD",
        "043" => "Average available previous month",
        "044" => "Disbursing opening available balance",
        "045" => "Closing available",
        "050" => "Average closing available MTD",
        "051" => "Average closing available last month",
        "054" => "Average closing available YTD last month",
        "055" => "Average closing available YTD",
        "056" => "Loan balance",
        "057" => "Total investment position",
        "059" => "Current available (CRS suppressed)",
        "060" => "Current available",
        "061" => "Average current available MTD",
        "062" => "Average current available YTD",
        "063" => "Total float",
        "065" => "Target balance",
        "066" => "Adjusted balance",
        "070" => "0-day float",
        "072" => "1-day float",
        "073" => "Float adjustment",
        "074" => "2 or more days float",
        "075" => "3 or more days float",
        "076" => "Adjustment to balances",
        "079" => "4-day float",
        "080" => "5-day float",
        "081" => "6-day float",
        "100" => "Total credits",
        "101" => "Total credit amount MTD",
        "105" => "Credits not detailed",
        "106" => "Deposits subject to float",
        "107" => "Total adjustment credits YTD",
        "108" => "Credit (any type)",
        "109" => "Current day total lockbox deposits",
        "110" => "Total lockbox deposits",
        "115" => "Lockbox deposit",
        "116" => "Item in lockbox deposit",
        "118" => "Lockbox adjustment credit",
        "120" => "EDI transaction credit",
        "121" => "EDI transaction credit",
        "130" => "Total concentration credits",
        "131" => "Total DTC credits",
        "135" => "DTC concentration credit",
        "140" => "Total ACH credits",
        "142" => "ACH credit received",
        "143" => "Item in ACH deposit",
        "145" => "ACH concentration credit",
        "146" => "Total bank card deposits",
        "147" => "Individual bank card deposit",
        "150" => "Total preauthorized payment credits",
        "155" => "Preauthorized draft credit",
        "160" => "Total ACH disbursing funding credits",
        "162" => "Corporate trade payment settlement",
        "163" => "Corporate trade payment credits",
        "164" => "Corporate trade payment credit",
        "165" => "Preauthorized ACH credit",
        "166" => "ACH settlement",
        "167" => "ACH settlement credits",
        "168" => "ACH return item or adjustment settlement",
        "169" => "Miscellaneous ACH credit",
        "170" => "Total other check deposits",
        "171" => "Individual loan deposit",
        "172" => "Deposit correction",
        "173" => "Bank-prepared deposit",
        "174" => "Other deposit",
        "175" => "Check deposit package",
        "176" => "Re-presented check deposit",
        "180" => "Total loan proceeds",
        "182" => "Total bank-prepared deposits",
        "184" => "Draft deposit",
        "185" => "Total miscellaneous deposits",
        "186" => "Cash letter credit",
        "187" => "Cash letter adjustment",
        "190" => "Total incoming money transfers",
        "191" => "Individual incoming internal money transfer",
        "195" => "Incoming money transfer",
        "196" => "Money transfer adjustment",
        "198" => "Compensation",
        "200" => "Total automatic transfer credits",
        "201" => "Individual automatic transfer credit",
        "205" => "Total book transfer credits",
        "206" => "Book transfer credit",
        "207" => "Total international money transfer credits",
        "208" => "Individual international money transfer credit",
        "210" => "Total international credits",
        "212" => "Foreign letter of credit",
        "213" => "Letter of credit",
        "214" => "Foreign exchange of credit",
        "216" => "Foreign remittance credit",
        "218" => "Foreign collection credit",
        "221" => "Foreign check purchase",
        "222" => "Foreign checks deposited",
        "224" => "Commission",
        "226" => "International money market trading",
        "227" => "Standing order",
        "229" => "Miscellaneous international credit",
        "230" => "Total security credits",
        "231" => "Total collection credits",
        "233" => "Securities sold",
        "236" => "Maturity of debt security",
        "238" => "Collection of dividends",
        "242" => "Collection of interest income",
        "244" => "Interest/matured principal payment",
        "250" => "Total checks posted and returned",
        "251" => "Total debit reversals",
        "252" => "Debit reversal",
        "254" => "Posting error correction credit",
        "255" => "Check posted and returned",
        "256" => "Total ACH return items",
        "257" => "Individual ACH return item",
        "258" => "ACH reversal credit",
        "260" => "Total rejected credits",
        "261" => "Individual rejected credit",
        "263" => "Overdraft",
        "266" => "Return item",
        "268" => "Return item adjustment",
        "270" => "Total ZBA credits",
        "271" => "Net zero-balance amount",
        "274" => "Cumulative ZBA or disbursement credits",
        "275" => "ZBA credit",
        "276" => "ZBA float adjustment",
        "277" => "ZBA credit transfer",
        "278" => "ZBA credit adjustment",
        "280" => "Total controlled disbursing credits",
        "281" => "Individual controlled disbursing credit",
        "295" => "ATM credit",
        "301" => "Commercial deposit",
        "306" => "Fed funds sold",
        "308" => "Trust credit",
        "319" => "Late deposit",
        "342" => "Broker deposit",
        "346" => "Sweep interest income",
        "347" => "Sweep principal sell",
        "350" => "Investment sold",
        "351" => "Individual investment sold",
        "353" => "Cash center credit",
        "354" => "Interest credit",
        "355" => "Investment interest",
        "356" => "Total credit adjustment",
        "357" => "Credit adjustment",
        "366" => "Currency and coin deposited",
        "390" => "Total miscellaneous credits",
        "391" => "Universal credit",
        "395" => "Check reversal",
        "397" => "Float adjustment",
        "398" => "Miscellaneous fee refund",
        "399" => "Miscellaneous credit",
        "400" => "Total debits",
        "401" => "Total debit amount MTD",
        "403" => "Today's total debits",
        "405" => "Total debit less wire transfers and charge-backs",
        "406" => "Debits not detailed",
        "408" => "Float adjustment",
        "409" => "Debit (any type)",
        "410" => "Total YTD adjustment",
        "412" => "Total debits (excluding returned items)",
        "415" => "Lockbox debit",
        "416" => "Total lockbox debits",
        "420" => "EDI transaction debit",
        "421" => "EDI transaction debit",
        "430" => "Total payable-through drafts",
        "435" => "Payable-through draft",
        "445" => "ACH concentration debit",
        "446" => "Total ACH disbursement funding debits",
        "447" => "ACH disbursement funding debit",
        "450" => "Total ACH debits",
        "451" => "ACH debit received",
        "452" => "Item in ACH disbursement or debit",
        "455" => "Preauthorized ACH debit",
        "462" => "Account holder initiated ACH debit",
        "463" => "Corporate trade payment debits",
        "464" => "Corporate trade payment debit",
        "465" => "Corporate trade payment settlement",
        "466" => "ACH settlement",
        "467" => "ACH settlement debits",
        "468" => "ACH return item or adjustment settlement",
        "469" => "Miscellaneous ACH debit",
        "470" => "Total check paid",
        "471" => "Total check paid, cumulative MTD",
        "472" => "Cumulative checks paid",
        "474" => "Certified check debit",
        "475" => "Check paid",
        "476" => "Federal Reserve Bank letter debit",
        "477" => "Bank originated debit",
        "478" => "List post debits",
        "479" => "List post debit",
        "480" => "Total loan payments",
        "481" => "Individual loan payment",
        "482" => "Total bank-originated debits",
        "484" => "Draft",
        "485" => "DTC debit",
        "486" => "Total cash letter debits",
        "487" => "Cash letter debit",
        "489" => "Cash letter adjustment",
        "490" => "Total outgoing money transfers",
        "491" => "Individual outgoing internal money transfer",
        "493" => "Customer terminal initiated money transfer",
        "495" => "Outgoing money transfer",
        "496" => "Money transfer adjustment",
        "498" => "Compensation",
        "500" => "Total automatic transfer debits",
        "501" => "Individual automatic transfer debit",
        "505" => "Total book transfer debits",
        "506" => "Book transfer debit",
        "507" => "Total international money transfer debits",
        "508" => "Individual international money transfer debit",
        "510" => "Total international debits",
        "512" => "Letter of credit debit",
        "514" => "Foreign exchange debit",
        "516" => "Foreign remittance debit",
        "518" => "Foreign collection debit",
        "522" => "Foreign checks paid",
        "524" => "Commission",
        "530" => "Total security debits",
        "531" => "Securities purchased",
        "550" => "Total deposited items returned",
        "555" => "Deposited item returned",
        "560" => "Total rejected debits",
        "561" => "Individual rejected debit",
        "564" => "Overdraft fee",
        "566" => "Return item",
        "567" => "Return item fee",
        "568" => "Return item adjustment",
        "570" => "Total ZBA debits",
        "574" => "Cumulative ZBA debits",
        "575" => "ZBA debit",
        "576" => "ZBA debit transfer",
        "577" => "ZBA debit adjustment",
        "578" => "Total controlled disbursing debits",
        "579" => "Individual controlled disbursing debit",
        "595" => "ATM debit",
        "597" => "ARP debit",
        "616" => "Federal Reserve Bank, commercial bank debit",
        "622" => "Broker debit",
        "627" => "Fed funds purchased",
        "629" => "Cash center debit",
        "630" => "Total debit adjustments",
        "631" => "Debit adjustment",
        "633" => "Trust debit",
        "650" => "Investments purchased",
        "651" => "Individual investment purchase",
        "654" => "Interest debit",
        "656" => "Sweep principal buy",
        "658" => "Principal payments debit",
        "661" => "Account analysis fee",
        "666" => "Currency and coin shipped",
        "690" => "Total miscellaneous debits",
        "691" => "Universal debit",
        "694" => "Deposit reversal",
        "695" => "Deposit correction debit",
        "696" => "Regular collection debit",
        "697" => "Cumulative debits",
        "698" => "Miscellaneous fees",
        "699" => "Miscellaneous debit",
        "701" => "Principal loan balance",
        "703" => "Available commitment amount",
        "705" => "Payment amount due",
        "707" => "Principal amount past due",
        "709" => "Interest amount past due",
        "720" => "Total loan payment",
        "721" => "Amount applied to interest",
        "722" => "Amount applied to principal",
        "760" => "Loan disbursement",
        "890" => "Contains non-monetary information",
        _ => "",
    }
}

fn describe(code: &str) -> String {
    match (type_name(code), type_class(code)) {
        ("", "") => String::new(),
        ("", class) => format!("{class} (custom or uncommon code)"),
        (name, _) => name.to_owned(),
    }
}

fn group_status(s: &str) -> &'static str {
    match s.trim() {
        "1" => "1 (update)",
        "2" => "2 (deletion)",
        "3" => "3 (correction)",
        "4" => "4 (test only)",
        _ => "",
    }
}

fn as_of_modifier(s: &str) -> &'static str {
    match s.trim() {
        "1" => "1 (interim previous-day data)",
        "2" => "2 (final previous-day data)",
        "3" => "3 (interim same-day data)",
        "4" => "4 (final same-day data)",
        _ => "",
    }
}

fn date_time(date: &str, time: &str) -> String {
    let d = yymmdd(date.trim()).unwrap_or_else(|| date.trim().to_owned());
    let t = time.trim();
    match t {
        "" => d,
        "2400" | "9999" => format!("{d} end of day ({t})"),
        _ => format!("{d} {}", common::hhmm(t).unwrap_or_else(|| t.to_owned())),
    }
}

struct Account {
    number: String,
    currency: String,
    sum: Option<i128>,
    records: u64,
    details: u64,
}

struct Group {
    number: usize,
    currency: String,
    records: u64,
    accounts: u64,
    /// Sum of the declared account control totals (49).
    sum49: Option<i128>,
}

struct Reader {
    file_records: u64,
    file_open: bool,
    sum98: Option<i128>,
    groups_seen: u64,
    group: Option<Group>,
    account: Option<Account>,
    accounts_total: u64,
    details_total: u64,
    problems: Vec<String>,
    unexpected: u64,
    unknown: u64,
    balances: Capped,
    transactions: Capped,
    account_totals: Tally,
    account_counts: Tally,
    group_totals: Tally,
    group_counts: Tally,
    file_totals: Tally,
    file_counts: Tally,
    saw_99: bool,
}

fn src(r: &Range<usize>) -> Option<Range<u64>> {
    span(r.start, r.end - r.start)
}

fn add(sum: Option<i128>, v: i128) -> Option<i128> {
    sum.and_then(|s| s.checked_add(v))
}

impl Reader {
    fn new() -> Self {
        Self {
            file_records: 0,
            file_open: false,
            sum98: Some(0),
            groups_seen: 0,
            group: None,
            account: None,
            accounts_total: 0,
            details_total: 0,
            problems: vec![],
            unexpected: 0,
            unknown: 0,
            balances: Capped::new(
                "Balances and summaries",
                &[
                    "Group",
                    "Account",
                    "Currency",
                    "Type code",
                    "Description",
                    "Amount",
                    "Item count",
                    "Funds type",
                ],
            ),
            transactions: Capped::new(
                "Transactions",
                &[
                    "Group",
                    "Account",
                    "Currency",
                    "Type code",
                    "Description",
                    "Credit/debit",
                    "Amount",
                    "Funds type",
                    "Bank reference",
                    "Customer reference",
                    "Text",
                ],
            ),
            account_totals: Tally::new("Account control totals (49)"),
            account_counts: Tally::new("Account record counts (49)"),
            group_totals: Tally::new("Group control totals (98)"),
            group_counts: Tally::new("Group account and record counts (98)"),
            file_totals: Tally::new("File control total (99)"),
            file_counts: Tally::new("File group and record counts (99)"),
            saw_99: false,
        }
    }

    fn record(&mut self, rec: &Record, out: &mut Inspection) {
        let f: Vec<&str> = rec.content.split(',').collect();
        let get = |i: usize| f.get(i).map(|s| s.trim()).unwrap_or("");
        // Every physical record counts toward the open account, group and file.
        if rec.code == "01" {
            self.file_records = 0;
        }
        self.file_records += rec.physical;
        if let Some(g) = self.group.as_mut() {
            g.records += rec.physical;
        }
        if let Some(a) = self.account.as_mut() {
            a.records += rec.physical;
        }
        match rec.code.as_str() {
            "01" => {
                if self.file_open {
                    self.unexpected += 1;
                }
                self.file_open = true;
                let s = src(&rec.span);
                out.fact("File", "Sender", get(0), s.clone());
                out.fact("File", "Receiver", get(1), s.clone());
                out.fact("File", "Created", date_time(get(2), get(3)), s.clone());
                out.fact("File", "File ID", get(4), s.clone());
                out.fact("File", "Physical record length", get(5), s.clone());
                out.fact("File", "Block size", get(6), s.clone());
                out.fact("File", "Version", get(7), s);
                if !get(7).is_empty() {
                    out.version = Some(format!("BAI version {}", get(7)));
                }
            }
            "02" => {
                if self.group.is_some() {
                    self.unexpected += 1;
                }
                self.groups_seen += 1;
                let number = self.groups_seen as usize;
                let currency = get(5).to_owned();
                if number <= FACT_GROUPS {
                    let g = format!("Group {number}");
                    let s = src(&rec.span);
                    out.fact(&g, "Ultimate receiver", get(0), s.clone());
                    out.fact(&g, "Originator", get(1), s.clone());
                    out.fact(&g, "Status", group_status(get(2)), s.clone());
                    out.fact(&g, "As of", date_time(get(3), get(4)), s.clone());
                    out.fact(&g, "Currency", currency.clone(), s.clone());
                    out.fact(&g, "As-of date modifier", as_of_modifier(get(6)), s);
                }
                self.group = Some(Group {
                    number,
                    currency,
                    records: rec.physical,
                    accounts: 0,
                    sum49: Some(0),
                });
            }
            "03" => {
                if self.account.is_some() {
                    self.unexpected += 1;
                }
                let group_currency = self
                    .group
                    .as_ref()
                    .map(|g| g.currency.clone())
                    .unwrap_or_default();
                let currency = if get(1).is_empty() {
                    group_currency
                } else {
                    get(1).to_owned()
                };
                let group = self.group.as_ref().map(|g| g.number).unwrap_or(0);
                if let Some(g) = self.group.as_mut() {
                    g.accounts += 1;
                }
                self.accounts_total += 1;
                let mut sum = Some(0i128);
                for (code, amount, count, funds) in summaries(&f) {
                    if let Some(a) = amount {
                        sum = add(sum, a);
                    }
                    if self.balances.full() {
                        self.balances.skip();
                        continue;
                    }
                    self.balances.push(
                        vec![
                            group.to_string(),
                            get(0).to_owned(),
                            currency.clone(),
                            code.clone(),
                            describe(&code),
                            amount.map(|a| show(a, &currency)).unwrap_or_default(),
                            count,
                            funds_name(&funds).to_owned(),
                        ],
                        src(&rec.span),
                    );
                }
                self.account = Some(Account {
                    number: get(0).to_owned(),
                    currency,
                    sum,
                    records: rec.physical,
                    details: 0,
                });
            }
            "16" => {
                let Some(d) = Detail::parse(&rec.content) else {
                    self.unknown += 1;
                    return;
                };
                self.details_total += 1;
                let group = self.group.as_ref().map(|g| g.number).unwrap_or(0);
                let (number, currency) = match self.account.as_mut() {
                    Some(a) => {
                        a.details += 1;
                        if let Some(v) = d.amount {
                            a.sum = add(a.sum, v);
                        }
                        (a.number.clone(), a.currency.clone())
                    }
                    None => {
                        self.unexpected += 1;
                        (String::new(), String::new())
                    }
                };
                if self.transactions.full() {
                    self.transactions.skip();
                    return;
                }
                self.transactions.push(
                    vec![
                        group.to_string(),
                        number,
                        currency.clone(),
                        d.code.clone(),
                        describe(&d.code),
                        type_class(&d.code).to_owned(),
                        d.amount.map(|a| show(a, &currency)).unwrap_or_default(),
                        funds_name(&d.funds).to_owned(),
                        d.bank_ref,
                        d.customer_ref,
                        d.text,
                    ],
                    src(&rec.span),
                );
            }
            "49" => {
                let Some(a) = self.account.take() else {
                    self.unexpected += 1;
                    return;
                };
                let declared = minor(get(0));
                let ccy = a.currency.clone();
                if let Some(d) = declared
                    && let Some(sum) = a.sum
                {
                    self.account_totals.add(d == sum, || {
                        format!(
                            "account {}: declared {}, computed {}",
                            a.number,
                            show(d, &ccy),
                            show(sum, &ccy)
                        )
                    });
                }
                if let Ok(n) = get(1).parse::<u64>() {
                    self.account_counts.add(n == a.records, || {
                        format!(
                            "account {}: declared {n} records, counted {}",
                            a.number, a.records
                        )
                    });
                }
                if let Some(g) = self.group.as_mut() {
                    g.sum49 = match declared {
                        Some(d) => add(g.sum49, d),
                        None => None,
                    };
                }
            }
            "98" => {
                if self.account.take().is_some() {
                    self.problems
                        .push("An account has no trailer (49) before its group trailer".into());
                }
                let Some(g) = self.group.take() else {
                    self.unexpected += 1;
                    return;
                };
                let declared = minor(get(0));
                if let (Some(d), Some(sum)) = (declared, g.sum49) {
                    self.group_totals.add(d == sum, || {
                        format!(
                            "group {}: declared {}, sum of account totals {}",
                            g.number,
                            show(d, &g.currency),
                            show(sum, &g.currency)
                        )
                    });
                }
                if let (Ok(na), Ok(nr)) = (get(1).parse::<u64>(), get(2).parse::<u64>()) {
                    self.group_counts
                        .add(na == g.accounts && nr == g.records, || {
                            format!(
                                "group {}: declared {na} accounts and {nr} records, counted {} and {}",
                                g.number, g.accounts, g.records
                            )
                        });
                }
                self.sum98 = match declared {
                    Some(d) => add(self.sum98, d),
                    None => None,
                };
            }
            "99" => {
                self.saw_99 = true;
                if self.group.take().is_some() {
                    self.problems
                        .push("A group has no trailer (98) before the file trailer".into());
                }
                let declared = minor(get(0));
                if let (Some(d), Some(sum)) = (declared, self.sum98) {
                    self.file_totals.add(d == sum, || {
                        format!(
                            "declared {}, sum of group totals {} (in minor units)",
                            d, sum
                        )
                    });
                }
                if let (Ok(ng), Ok(nr)) = (get(1).parse::<u64>(), get(2).parse::<u64>()) {
                    let (seen, records) = (self.groups_seen, self.file_records);
                    self.file_counts.add(ng == seen && nr == records, || {
                        format!(
                            "declared {ng} groups and {nr} records, counted {seen} and {records}"
                        )
                    });
                }
                self.file_open = false;
            }
            _ => self.unknown += 1,
        }
    }

    fn finish(
        mut self,
        out: &mut Inspection,
        mut problems: Vec<String>,
    ) -> Result<Inspection, String> {
        if self.account.is_some() {
            self.problems
                .push("The last account has no trailer (49)".into());
        }
        if self.group.is_some() {
            self.problems
                .push("The last group has no trailer (98)".into());
        }
        if !self.saw_99 {
            self.problems
                .push("The file trailer (99) is missing: the file may be cut".into());
        }
        if self.unknown > 0 {
            self.problems.push(format!(
                "{} record(s) of an unknown type were ignored",
                self.unknown
            ));
        }
        if self.unexpected > 0 {
            self.problems.push(format!(
                "{} record(s) appear out of the 01/02/03/16/49/98/99 order",
                self.unexpected
            ));
        }
        problems.append(&mut self.problems);
        out.fact("Totals", "Groups", self.groups_seen.to_string(), None);
        out.fact("Totals", "Accounts", self.accounts_total.to_string(), None);
        out.fact(
            "Totals",
            "Transactions",
            self.details_total.to_string(),
            None,
        );
        if self.groups_seen as usize > FACT_GROUPS {
            out.notes.push(format!(
                "Group facts are listed for the first {FACT_GROUPS} groups"
            ));
        }
        self.balances.finish(out);
        self.transactions.finish(out);
        common::parsed(out, "Records read; continuations (88) merged", &problems);
        out.check(
            Stage::Schema,
            State::NotDone,
            "Field formats are not checked against the BAI2 specification",
        );
        for t in [
            self.account_totals,
            self.account_counts,
            self.group_totals,
            self.group_counts,
            self.file_totals,
            self.file_counts,
        ] {
            t.report(out);
        }
        out.notes.push(
            "Amounts are written in minor units without a decimal point; they are shown with the currency's decimals (2 unless the currency has 0 or 3)"
                .into(),
        );
        out.notes.push(
            "Control totals are the sums of every amount (balances, summaries and details) as BAI2 defines them; record counts include continuation records"
                .into(),
        );
        Ok(std::mem::take(out))
    }
}
