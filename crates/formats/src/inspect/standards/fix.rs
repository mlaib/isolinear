//! FIX protocol messages (tag=value): raw session captures with the SOH (0x01) delimiter, and
//! logs where SOH is written as `|` or `^A` and each line may start with a time stamp or other
//! prefix. Each message gets one row with its common fields; BodyLength (9) and CheckSum (10)
//! are recomputed, counting each written delimiter as the one SOH byte it stands for.
use super::common::{self, Capped, Tally, span};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};
use std::collections::BTreeMap;

/// Messages read from one file at most.
const MAX_MESSAGES: usize = 1_000_000;
/// Fields read per message.
const MAX_FIELDS: usize = 4096;
/// Bytes before a message searched for its log prefix.
const MAX_PREFIX: usize = 256;
/// Longest BeginString value accepted ("FIXT.1.1", "FIX.4.4", "FIX.5.0SP2").
const MAX_BEGIN: usize = 16;

pub struct Fix;

#[derive(Clone, Copy, PartialEq)]
enum Delim {
    Soh,
    Pipe,
    Caret,
}

impl Delim {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Soh => b"\x01",
            Self::Pipe => b"|",
            Self::Caret => b"^A",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Soh => "SOH (0x01)",
            Self::Pipe => "| (written for SOH)",
            Self::Caret => "^A (written for SOH)",
        }
    }
}

/// Start of a message at `at` ("8=FIX…"): its delimiter, when the BeginString is followed by one.
fn begin_at(b: &[u8], at: usize) -> Option<Delim> {
    // "58=FIX" is a Text field, not a message start.
    if at > 0 && b.get(at - 1).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let rest = b.get(at + 2..)?;
    let len = rest
        .iter()
        .take(MAX_BEGIN + 1)
        .position(|&c| c == 0x01 || c == b'|' || c == b'^' || c == b'\n' || c == b'\r')?;
    match rest.get(len..)? {
        r if r.starts_with(b"\x01") => Some(Delim::Soh),
        r if r.starts_with(b"|") => Some(Delim::Pipe),
        r if r.starts_with(b"^A") => Some(Delim::Caret),
        _ => None,
    }
}

impl Handler for Fix {
    fn name(&self) -> &'static str {
        "FIX"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let h = probe.head;
        memchr::memmem::find_iter(h, b"8=FIX").take(64).any(|at| {
            let Some(d) = begin_at(h, at) else {
                return false;
            };
            // BeginString, then BodyLength and MsgType.
            let rest = h.get(at..).unwrap_or_default();
            let Some(first) = memchr::memmem::find(rest, d.bytes()) else {
                return false;
            };
            let after = rest.get(first + d.bytes().len()..).unwrap_or_default();
            after.starts_with(b"9=") && memchr::memmem::find(after, b"35=").is_some_and(|i| i < 40)
        })
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        let mut msgs: Vec<Msg> = vec![];
        let mut problems = vec![];
        let mut pos = 0usize;
        let finder = memchr::memmem::Finder::new(b"8=FIX");
        while let Some(i) = finder.find(b.get(pos..).unwrap_or_default()) {
            let at = pos + i;
            pos = at + 5;
            if msgs.len().is_multiple_of(4096) && cancelled() {
                problems.push("Cancelled".into());
                break;
            }
            let Some(delim) = begin_at(b, at) else {
                continue;
            };
            if msgs.len() >= MAX_MESSAGES {
                problems.push(format!("Only the first {MAX_MESSAGES} messages were read"));
                break;
            }
            let prev_end = msgs.last().map(|m| m.end).unwrap_or(0);
            let m = read_message(b, at, prev_end, delim, &finder);
            pos = pos.max(m.end);
            msgs.push(m);
        }
        if msgs.is_empty() {
            return Err("No FIX message was found".into());
        }
        let begin = msgs
            .first()
            .and_then(|m| m.get("8"))
            .unwrap_or("")
            .to_owned();
        let mut out = Inspection::new("FIX", "FIX messages");
        out.version = Some(match msgs.first().and_then(|m| m.get("1128")) {
            Some(v) => format!("{begin} (application version {})", appl_ver(v)),
            None => begin.clone(),
        });
        let delim = msgs.first().map(|m| m.delim).unwrap_or(Delim::Soh);
        // Lines with no message (log noise).
        let other_lines = common::lines_from(b, 0)
            .take(10_000_000)
            .filter(|(_, l)| {
                !l.trim_ascii().is_empty() && memchr::memmem::find(l, b"8=FIX").is_none()
            })
            .count();
        let mut types: BTreeMap<String, u64> = BTreeMap::new();
        let mut sessions: BTreeMap<(String, String), u64> = BTreeMap::new();
        let mut table = Capped::new(
            "Messages",
            &[
                "#",
                "Log prefix",
                "Sending time (52)",
                "MsgType (35)",
                "Sender (49)",
                "Target (56)",
                "Seq (34)",
                "Symbol (55)",
                "Side (54)",
                "Order qty (38)",
                "Price (44)",
                "OrdType (40)",
                "OrdStatus (39)",
                "ExecType (150)",
                "ClOrdID (11)",
                "OrderID (37)",
                "Text (58)",
                "Fields",
            ],
        );
        let mut fields = Capped::new("Fields", &["#", "Tag", "Name", "Value", "Meaning"]);
        let mut body_len = Tally::new("BodyLength (9)");
        let mut checksum = Tally::new("CheckSum (10)");
        let mut truncated = 0usize;
        for (k, m) in msgs.iter().enumerate() {
            let n = k + 1;
            let ty = m.get("35").unwrap_or("");
            *types.entry(ty.to_owned()).or_default() += 1;
            *sessions
                .entry((
                    m.get("49").unwrap_or("").to_owned(),
                    m.get("56").unwrap_or("").to_owned(),
                ))
                .or_default() += 1;
            if !m.complete {
                truncated += 1;
            }
            if let (Some(declared), Some(computed)) = (m.get("9"), m.body_length()) {
                let ok = declared.trim().parse::<usize>().ok() == Some(computed);
                body_len.add(ok, || {
                    format!("message {n}: declared {declared}, computed {computed}")
                });
            }
            if let (Some(declared), Some(computed)) = (m.get("10"), m.checksum()) {
                let ok = declared.trim().parse::<u32>().ok() == Some(computed);
                checksum.add(ok, || {
                    format!("message {n}: declared {declared}, computed {computed:03}")
                });
            }
            if !table.full() {
                let g = |t: &str| m.get(t).unwrap_or("").to_owned();
                let e = |t: &str| m.get(t).map(|v| with_meaning(t, v)).unwrap_or_default();
                table.push(
                    vec![
                        n.to_string(),
                        m.prefix.clone(),
                        g("52"),
                        e("35"),
                        g("49"),
                        g("56"),
                        g("34"),
                        g("55"),
                        e("54"),
                        g("38"),
                        g("44"),
                        e("40"),
                        e("39"),
                        e("150"),
                        g("11"),
                        g("37"),
                        g("58"),
                        m.fields.len().to_string(),
                    ],
                    span(m.start, m.end - m.start),
                );
            } else {
                table.skip();
            }
            for f in &m.fields {
                if fields.full() {
                    fields.skip();
                    continue;
                }
                fields.push(
                    vec![
                        n.to_string(),
                        f.tag.clone(),
                        tag_name(&f.tag).to_owned(),
                        f.value.clone(),
                        meaning(&f.tag, &f.value).unwrap_or("").to_owned(),
                    ],
                    span(f.start, f.len),
                );
            }
        }
        let first_type = msgs.first().and_then(|m| m.get("35")).unwrap_or("");
        out.kind = if types.len() == 1 && msgs.len() == 1 {
            format!("FIX {} message", with_meaning("35", first_type))
        } else {
            format!("FIX messages ({})", msgs.len())
        };
        out.fact("Messages", "BeginString (8)", begin, None);
        if let Some(v) = msgs.first().and_then(|m| m.get("1128")) {
            out.fact("Messages", "ApplVerID (1128)", appl_ver(v), None);
        }
        out.fact("Messages", "Messages", msgs.len().to_string(), None);
        out.fact("Messages", "Delimiter", delim.name(), None);
        let by_type: Vec<String> = types
            .iter()
            .map(|(t, c)| format!("{}: {c}", with_meaning("35", t)))
            .collect();
        out.fact("Messages", "Message types", by_type.join(", "), None);
        let by_session: Vec<String> = sessions
            .iter()
            .take(50)
            .map(|((s, t), c)| format!("{s} → {t}: {c}"))
            .collect();
        out.fact(
            "Messages",
            "Sessions (49 → 56)",
            by_session.join(", "),
            None,
        );
        let times: Vec<&str> = msgs.iter().filter_map(|m| m.get("52")).collect();
        if let (Some(a), Some(z)) = (times.first(), times.last()) {
            out.fact("Messages", "First sending time", *a, None);
            out.fact("Messages", "Last sending time", *z, None);
        }
        if other_lines > 0 {
            out.fact("Messages", "Other lines", other_lines.to_string(), None);
        }
        table.finish(&mut out);
        fields.finish(&mut out);
        if truncated > 0 {
            problems.push(format!(
                "{truncated} message(s) end before their CheckSum (10) field"
            ));
        }
        common::parsed(&mut out, "Messages split into tag=value fields", &problems);
        out.check(
            Stage::Schema,
            State::NotDone,
            "Required fields, repeating groups and value types are not checked against a FIX data dictionary",
        );
        body_len.report(&mut out);
        checksum.report(&mut out);
        if delim != Delim::Soh {
            out.notes.push(format!(
                "The delimiter is written as {}: the checks count each one as a single SOH byte",
                if delim == Delim::Pipe { "|" } else { "^A" }
            ));
        }
        out.notes.push(
            "Repeating groups are shown as the flat sequence of their fields; tags outside the built-in dictionary are listed without a name"
                .into(),
        );
        Ok(out)
    }
}

struct FieldAt {
    tag: String,
    value: String,
    start: usize,
    len: usize,
    /// Bytes of "tag=value" as written, for the checks.
    sum: u32,
}

struct Msg {
    start: usize,
    end: usize,
    delim: Delim,
    prefix: String,
    fields: Vec<FieldAt>,
    complete: bool,
}

impl Msg {
    fn get(&self, tag: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|f| f.tag == tag)
            .map(|f| f.value.as_str())
    }
    /// Bytes after BodyLength's delimiter up to CheckSum's field, each delimiter as one byte.
    fn body_length(&self) -> Option<usize> {
        let nine = self.fields.iter().position(|f| f.tag == "9")?;
        let ten = self.fields.iter().position(|f| f.tag == "10")?;
        let body = self.fields.get(nine + 1..ten)?;
        Some(body.iter().map(|f| f.len + 1).sum())
    }
    /// Sum of the bytes before CheckSum's field (delimiters as SOH), modulo 256.
    fn checksum(&self) -> Option<u32> {
        let ten = self.fields.iter().position(|f| f.tag == "10")?;
        let total: u64 = self
            .fields
            .get(..ten)?
            .iter()
            .map(|f| f.sum as u64 + 1)
            .sum();
        Some((total % 256) as u32)
    }
}

/// Read one message from `at`; it ends after CheckSum (10), at the next message, or (for `|`
/// and `^A` logs) at the end of the line.
fn read_message(
    b: &[u8],
    at: usize,
    prev_end: usize,
    delim: Delim,
    finder: &memchr::memmem::Finder,
) -> Msg {
    // The prefix: what precedes the message on its line, after the previous message.
    let window = at.saturating_sub(MAX_PREFIX).max(prev_end.min(at));
    let line_start = b
        .get(window..at)
        .and_then(|h| memchr::memrchr2(b'\n', b'\r', h).map(|i| window + i + 1))
        .unwrap_or(window);
    let prefix = b.get(line_start..at).unwrap_or_default();
    let prefix = if delim == Delim::Soh && prefix.contains(&0x01) {
        String::new()
    } else {
        let p = common::text(prefix);
        let p = p.trim();
        p.chars().take(80).collect()
    };
    let next = finder
        .find_iter(b.get(at + 5..).unwrap_or_default())
        .map(|i| at + 5 + i)
        .find(|&i| begin_at(b, i).is_some())
        .unwrap_or(b.len());
    let line_end = if delim == Delim::Soh {
        next
    } else {
        b.get(at..next)
            .and_then(|r| memchr::memchr2(b'\n', b'\r', r))
            .map(|i| at + i)
            .unwrap_or(next)
    };
    let limit = next.min(line_end);
    let d = delim.bytes();
    let mut fields = vec![];
    let mut pos = at;
    let mut complete = false;
    while pos < limit && fields.len() < MAX_FIELDS {
        let rest = b.get(pos..limit).unwrap_or_default();
        let len = memchr::memmem::find(rest, d).unwrap_or(rest.len());
        let raw = rest.get(..len).unwrap_or_default();
        let (tag, value) = match memchr::memchr(b'=', raw) {
            Some(eq) => (
                raw.get(..eq).unwrap_or_default(),
                raw.get(eq + 1..).unwrap_or_default(),
            ),
            None => (raw, &b""[..]),
        };
        let is_ten = tag == b"10";
        // The checksum ends at its delimiter, or (unterminated) after its three digits.
        let (raw, value) = if is_ten && len == rest.len() {
            let digits = value.iter().take_while(|c| c.is_ascii_digit()).count();
            (
                raw.get(..3 + digits).unwrap_or(raw),
                value.get(..digits).unwrap_or(value),
            )
        } else {
            (raw, value)
        };
        if !raw.is_empty() {
            fields.push(FieldAt {
                tag: common::text(tag).trim().to_owned(),
                value: common::text(value).into_owned(),
                start: pos,
                len: raw.len(),
                sum: raw.iter().map(|&c| c as u32).sum(),
            });
        }
        if is_ten {
            complete = true;
            break;
        }
        pos += len + if len < rest.len() { d.len() } else { 0 };
    }
    let end = fields
        .last()
        .map(|f| f.start + f.len)
        .unwrap_or(at + 5)
        .max(at + 5)
        .min(b.len());
    Msg {
        start: at,
        end,
        delim,
        prefix,
        fields,
        complete,
    }
}

fn appl_ver(v: &str) -> String {
    let name = match v {
        "2" => "FIX 4.0",
        "3" => "FIX 4.1",
        "4" => "FIX 4.2",
        "5" => "FIX 4.3",
        "6" => "FIX 4.4",
        "7" => "FIX 5.0",
        "8" => "FIX 5.0 SP1",
        "9" => "FIX 5.0 SP2",
        _ => "",
    };
    if name.is_empty() {
        v.to_owned()
    } else {
        format!("{v} ({name})")
    }
}

fn with_meaning(tag: &str, v: &str) -> String {
    match meaning(tag, v) {
        Some(m) => format!("{v} — {m}"),
        None => v.to_owned(),
    }
}

/// Names of the enumerated values of MsgType, Side, OrdType, OrdStatus, ExecType, TimeInForce
/// and a few session fields.
pub fn meaning(tag: &str, v: &str) -> Option<&'static str> {
    Some(match (tag, v) {
        ("35", "0") => "Heartbeat",
        ("35", "1") => "TestRequest",
        ("35", "2") => "ResendRequest",
        ("35", "3") => "Reject",
        ("35", "4") => "SequenceReset",
        ("35", "5") => "Logout",
        ("35", "6") => "IOI",
        ("35", "7") => "Advertisement",
        ("35", "8") => "ExecutionReport",
        ("35", "9") => "OrderCancelReject",
        ("35", "A") => "Logon",
        ("35", "B") => "News",
        ("35", "C") => "Email",
        ("35", "D") => "NewOrderSingle",
        ("35", "E") => "NewOrderList",
        ("35", "F") => "OrderCancelRequest",
        ("35", "G") => "OrderCancelReplaceRequest",
        ("35", "H") => "OrderStatusRequest",
        ("35", "J") => "AllocationInstruction",
        ("35", "K") => "ListCancelRequest",
        ("35", "L") => "ListExecute",
        ("35", "M") => "ListStatusRequest",
        ("35", "N") => "ListStatus",
        ("35", "P") => "AllocationInstructionAck",
        ("35", "Q") => "DontKnowTrade",
        ("35", "R") => "QuoteRequest",
        ("35", "S") => "Quote",
        ("35", "T") => "SettlementInstructions",
        ("35", "V") => "MarketDataRequest",
        ("35", "W") => "MarketDataSnapshotFullRefresh",
        ("35", "X") => "MarketDataIncrementalRefresh",
        ("35", "Y") => "MarketDataRequestReject",
        ("35", "Z") => "QuoteCancel",
        ("35", "a") => "QuoteStatusRequest",
        ("35", "b") => "MassQuoteAcknowledgement",
        ("35", "c") => "SecurityDefinitionRequest",
        ("35", "d") => "SecurityDefinition",
        ("35", "e") => "SecurityStatusRequest",
        ("35", "f") => "SecurityStatus",
        ("35", "g") => "TradingSessionStatusRequest",
        ("35", "h") => "TradingSessionStatus",
        ("35", "i") => "MassQuote",
        ("35", "j") => "BusinessMessageReject",
        ("35", "k") => "BidRequest",
        ("35", "l") => "BidResponse",
        ("35", "m") => "ListStrikePrice",
        ("35", "n") => "XMLnonFIX",
        ("35", "o") => "RegistrationInstructions",
        ("35", "p") => "RegistrationInstructionsResponse",
        ("35", "q") => "OrderMassCancelRequest",
        ("35", "r") => "OrderMassCancelReport",
        ("35", "s") => "NewOrderCross",
        ("35", "t") => "CrossOrderCancelReplaceRequest",
        ("35", "u") => "CrossOrderCancelRequest",
        ("35", "v") => "SecurityTypeRequest",
        ("35", "w") => "SecurityTypes",
        ("35", "x") => "SecurityListRequest",
        ("35", "y") => "SecurityList",
        ("35", "z") => "DerivativeSecurityListRequest",
        ("35", "AA") => "DerivativeSecurityList",
        ("35", "AB") => "NewOrderMultileg",
        ("35", "AC") => "MultilegOrderCancelReplace",
        ("35", "AD") => "TradeCaptureReportRequest",
        ("35", "AE") => "TradeCaptureReport",
        ("35", "AF") => "OrderMassStatusRequest",
        ("35", "AG") => "QuoteRequestReject",
        ("35", "AH") => "RFQRequest",
        ("35", "AI") => "QuoteStatusReport",
        ("35", "AJ") => "QuoteResponse",
        ("35", "AK") => "Confirmation",
        ("35", "AL") => "PositionMaintenanceRequest",
        ("35", "AM") => "PositionMaintenanceReport",
        ("35", "AN") => "RequestForPositions",
        ("35", "AO") => "RequestForPositionsAck",
        ("35", "AP") => "PositionReport",
        ("35", "AQ") => "TradeCaptureReportRequestAck",
        ("35", "AR") => "TradeCaptureReportAck",
        ("35", "AS") => "AllocationReport",
        ("35", "AT") => "AllocationReportAck",
        ("35", "AU") => "ConfirmationAck",
        ("35", "AV") => "SettlementInstructionRequest",
        ("35", "AW") => "AssignmentReport",
        ("35", "AX") => "CollateralRequest",
        ("35", "AY") => "CollateralAssignment",
        ("35", "AZ") => "CollateralResponse",
        ("35", "BA") => "CollateralReport",
        ("35", "BB") => "CollateralInquiry",
        ("35", "BC") => "NetworkCounterpartySystemStatusRequest",
        ("35", "BD") => "NetworkCounterpartySystemStatusResponse",
        ("35", "BE") => "UserRequest",
        ("35", "BF") => "UserResponse",
        ("35", "BG") => "CollateralInquiryAck",
        ("35", "BH") => "ConfirmationRequest",
        ("35", "BI") => "TradingSessionListRequest",
        ("35", "BJ") => "TradingSessionList",
        ("35", "BK") => "SecurityListUpdateReport",
        ("35", "BL") => "AdjustedPositionReport",
        ("35", "BM") => "AllocationInstructionAlert",
        ("35", "BN") => "ExecutionAcknowledgement",
        ("35", "BO") => "ContraryIntentionReport",
        ("35", "BP") => "SecurityDefinitionUpdateReport",
        ("54", "1") => "Buy",
        ("54", "2") => "Sell",
        ("54", "3") => "Buy minus",
        ("54", "4") => "Sell plus",
        ("54", "5") => "Sell short",
        ("54", "6") => "Sell short exempt",
        ("54", "7") => "Undisclosed",
        ("54", "8") => "Cross",
        ("54", "9") => "Cross short",
        ("54", "A") => "Cross short exempt",
        ("54", "B") => "As defined",
        ("54", "C") => "Opposite",
        ("54", "D") => "Subscribe",
        ("54", "E") => "Redeem",
        ("54", "F") => "Lend",
        ("54", "G") => "Borrow",
        ("40", "1") => "Market",
        ("40", "2") => "Limit",
        ("40", "3") => "Stop",
        ("40", "4") => "Stop limit",
        ("40", "5") => "Market on close",
        ("40", "6") => "With or without",
        ("40", "7") => "Limit or better",
        ("40", "8") => "Limit with or without",
        ("40", "9") => "On basis",
        ("40", "A") => "On close",
        ("40", "B") => "Limit on close",
        ("40", "C") => "Forex market",
        ("40", "D") => "Previously quoted",
        ("40", "E") => "Previously indicated",
        ("40", "F") => "Forex limit",
        ("40", "G") => "Forex swap",
        ("40", "H") => "Forex previously quoted",
        ("40", "I") => "Funari",
        ("40", "J") => "Market if touched",
        ("40", "K") => "Market with leftover as limit",
        ("40", "L") => "Previous fund valuation point",
        ("40", "M") => "Next fund valuation point",
        ("40", "P") => "Pegged",
        ("40", "Q") => "Counter-order selection",
        ("39", "0") => "New",
        ("39", "1") => "Partially filled",
        ("39", "2") => "Filled",
        ("39", "3") => "Done for day",
        ("39", "4") => "Canceled",
        ("39", "5") => "Replaced",
        ("39", "6") => "Pending cancel",
        ("39", "7") => "Stopped",
        ("39", "8") => "Rejected",
        ("39", "9") => "Suspended",
        ("39", "A") => "Pending new",
        ("39", "B") => "Calculated",
        ("39", "C") => "Expired",
        ("39", "D") => "Accepted for bidding",
        ("39", "E") => "Pending replace",
        ("150", "0") => "New",
        ("150", "1") => "Partial fill (FIX 4.2)",
        ("150", "2") => "Fill (FIX 4.2)",
        ("150", "3") => "Done for day",
        ("150", "4") => "Canceled",
        ("150", "5") => "Replaced",
        ("150", "6") => "Pending cancel",
        ("150", "7") => "Stopped",
        ("150", "8") => "Rejected",
        ("150", "9") => "Suspended",
        ("150", "A") => "Pending new",
        ("150", "B") => "Calculated",
        ("150", "C") => "Expired",
        ("150", "D") => "Restated",
        ("150", "E") => "Pending replace",
        ("150", "F") => "Trade",
        ("150", "G") => "Trade correct",
        ("150", "H") => "Trade cancel",
        ("150", "I") => "Order status",
        ("150", "J") => "Trade in a clearing hold",
        ("150", "K") => "Trade released to clearing",
        ("150", "L") => "Triggered or activated by system",
        ("59", "0") => "Day",
        ("59", "1") => "Good till cancel",
        ("59", "2") => "At the opening",
        ("59", "3") => "Immediate or cancel",
        ("59", "4") => "Fill or kill",
        ("59", "5") => "Good till crossing",
        ("59", "6") => "Good till date",
        ("59", "7") => "At the close",
        ("59", "8") => "Good through crossing",
        ("59", "9") => "At crossing",
        ("20", "0") => "New",
        ("20", "1") => "Cancel",
        ("20", "2") => "Correct",
        ("20", "3") => "Status",
        ("21", "1") => "Automated, private, no broker intervention",
        ("21", "2") => "Automated, public, broker intervention OK",
        ("21", "3") => "Manual order, best execution",
        ("98", "0") => "None / other",
        ("43" | "97" | "123" | "141", "Y") => "Yes",
        ("43" | "97" | "123" | "141", "N") => "No",
        ("373", "0") => "Invalid tag number",
        ("373", "1") => "Required tag missing",
        ("373", "2") => "Tag not defined for this message type",
        ("373", "3") => "Undefined tag",
        ("373", "4") => "Tag specified without a value",
        ("373", "5") => "Value is incorrect (out of range) for this tag",
        ("373", "6") => "Incorrect data format for value",
        ("373", "9") => "CompID problem",
        ("373", "10") => "SendingTime accuracy problem",
        ("373", "11") => "Invalid MsgType",
        ("373", "13") => "Tag appears more than once",
        ("373", "99") => "Other",
        _ => return None,
    })
}

/// Names of the most common tags (FIX 4.2–5.0).
pub fn tag_name(tag: &str) -> &'static str {
    match tag {
        "1" => "Account",
        "6" => "AvgPx",
        "7" => "BeginSeqNo",
        "8" => "BeginString",
        "9" => "BodyLength",
        "10" => "CheckSum",
        "11" => "ClOrdID",
        "12" => "Commission",
        "13" => "CommType",
        "14" => "CumQty",
        "15" => "Currency",
        "16" => "EndSeqNo",
        "17" => "ExecID",
        "18" => "ExecInst",
        "19" => "ExecRefID",
        "20" => "ExecTransType",
        "21" => "HandlInst",
        "22" => "SecurityIDSource",
        "23" => "IOIID",
        "25" => "IOIQltyInd",
        "26" => "IOIRefID",
        "27" => "IOIQty",
        "28" => "IOITransType",
        "29" => "LastCapacity",
        "30" => "LastMkt",
        "31" => "LastPx",
        "32" => "LastQty",
        "33" => "NoLinesOfText",
        "34" => "MsgSeqNum",
        "35" => "MsgType",
        "36" => "NewSeqNo",
        "37" => "OrderID",
        "38" => "OrderQty",
        "39" => "OrdStatus",
        "40" => "OrdType",
        "41" => "OrigClOrdID",
        "43" => "PossDupFlag",
        "44" => "Price",
        "45" => "RefSeqNum",
        "48" => "SecurityID",
        "49" => "SenderCompID",
        "50" => "SenderSubID",
        "52" => "SendingTime",
        "53" => "Quantity",
        "54" => "Side",
        "55" => "Symbol",
        "56" => "TargetCompID",
        "57" => "TargetSubID",
        "58" => "Text",
        "59" => "TimeInForce",
        "60" => "TransactTime",
        "61" => "Urgency",
        "62" => "ValidUntilTime",
        "63" => "SettlType",
        "64" => "SettlDate",
        "65" => "SymbolSfx",
        "66" => "ListID",
        "67" => "ListSeqNo",
        "68" => "TotNoOrders",
        "69" => "ListExecInst",
        "70" => "AllocID",
        "71" => "AllocTransType",
        "72" => "RefAllocID",
        "73" => "NoOrders",
        "74" => "AvgPxPrecision",
        "75" => "TradeDate",
        "77" => "PositionEffect",
        "78" => "NoAllocs",
        "79" => "AllocAccount",
        "80" => "AllocQty",
        "81" => "ProcessCode",
        "97" => "PossResend",
        "98" => "EncryptMethod",
        "99" => "StopPx",
        "100" => "ExDestination",
        "102" => "CxlRejReason",
        "103" => "OrdRejReason",
        "104" => "IOIQualifier",
        "106" => "Issuer",
        "107" => "SecurityDesc",
        "108" => "HeartBtInt",
        "110" => "MinQty",
        "111" => "MaxFloor",
        "112" => "TestReqID",
        "113" => "ReportToExch",
        "114" => "LocateReqd",
        "115" => "OnBehalfOfCompID",
        "116" => "OnBehalfOfSubID",
        "117" => "QuoteID",
        "118" => "NetMoney",
        "119" => "SettlCurrAmt",
        "120" => "SettlCurrency",
        "122" => "OrigSendingTime",
        "123" => "GapFillFlag",
        "126" => "ExpireTime",
        "128" => "DeliverToCompID",
        "129" => "DeliverToSubID",
        "131" => "QuoteReqID",
        "132" => "BidPx",
        "133" => "OfferPx",
        "134" => "BidSize",
        "135" => "OfferSize",
        "136" => "NoMiscFees",
        "137" => "MiscFeeAmt",
        "138" => "MiscFeeCurr",
        "139" => "MiscFeeType",
        "140" => "PrevClosePx",
        "141" => "ResetSeqNumFlag",
        "142" => "SenderLocationID",
        "143" => "TargetLocationID",
        "146" => "NoRelatedSym",
        "148" => "Headline",
        "150" => "ExecType",
        "151" => "LeavesQty",
        "152" => "CashOrderQty",
        "153" => "AllocAvgPx",
        "154" => "AllocNetMoney",
        "155" => "SettlCurrFxRate",
        "156" => "SettlCurrFxRateCalc",
        "167" => "SecurityType",
        "168" => "EffectiveTime",
        "192" => "OrderQty2",
        "193" => "SettlDate2",
        "198" => "SecondaryOrderID",
        "200" => "MaturityMonthYear",
        "201" => "PutOrCall",
        "202" => "StrikePrice",
        "207" => "SecurityExchange",
        "211" => "PegOffsetValue",
        "223" => "CouponRate",
        "231" => "ContractMultiplier",
        "262" => "MDReqID",
        "263" => "SubscriptionRequestType",
        "264" => "MarketDepth",
        "265" => "MDUpdateType",
        "267" => "NoMDEntryTypes",
        "268" => "NoMDEntries",
        "269" => "MDEntryType",
        "270" => "MDEntryPx",
        "271" => "MDEntrySize",
        "272" => "MDEntryDate",
        "273" => "MDEntryTime",
        "279" => "MDUpdateAction",
        "281" => "MDReqRejReason",
        "299" => "QuoteEntryID",
        "336" => "TradingSessionID",
        "369" => "LastMsgSeqNumProcessed",
        "371" => "RefTagID",
        "372" => "RefMsgType",
        "373" => "SessionRejectReason",
        "378" => "ExecRestatementReason",
        "379" => "BusinessRejectRefID",
        "380" => "BusinessRejectReason",
        "382" => "NoContraBrokers",
        "383" => "MaxMessageSize",
        "423" => "PriceType",
        "434" => "CxlRejResponseTo",
        "447" => "PartyIDSource",
        "448" => "PartyID",
        "452" => "PartyRole",
        "453" => "NoPartyIDs",
        "455" => "SecurityAltID",
        "460" => "Product",
        "461" => "CFICode",
        "487" => "TradeReportTransType",
        "528" => "OrderCapacity",
        "529" => "OrderRestrictions",
        "537" => "QuoteType",
        "541" => "MaturityDate",
        "553" => "Username",
        "554" => "Password",
        "555" => "NoLegs",
        "571" => "TradeReportID",
        "581" => "AccountType",
        "600" => "LegSymbol",
        "636" => "WorkingIndicator",
        "662" => "BenchmarkPrice",
        "715" => "ClearingBusinessDate",
        "789" => "NextExpectedMsgSeqNum",
        "828" => "TrdType",
        "851" => "LastLiquidityInd",
        "1003" => "TradeID",
        "1028" => "ManualOrderIndicator",
        "1057" => "AggressorIndicator",
        "1128" => "ApplVerID",
        "1137" => "DefaultApplVerID",
        "1300" => "MarketSegmentID",
        _ => "",
    }
}
