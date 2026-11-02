//! IATA Cargo-IMP messages, recognized by their standard message identifier line
//! (`FWB/16`, `FFM/8`, `FSU/14`, `FHL/4` …), optionally after SITA Type B address lines.
//!
//! Interpreted: FWB (air waybill: consignment, flights, routing, parties, charges declaration,
//! rate description, issue), FFM (flight manifest: flight, unloading points, ULDs and
//! consignments), FSU (status update: status lines with date, flight, airports and quantities)
//! and FHL (house manifest: master and house waybills). The layout read is the common one of
//! the recent versions; version-specific differences are not distinguished. Other Cargo-IMP
//! messages are recognized and their lines listed. Air waybill numbers are checked with their
//! check digit (serial number mod 7).
use super::edi::{Rows, Tally, clip, coded, content_start, decode};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State, spec::decimal_sum};
use std::ops::Range;

pub struct CargoImp;

/// Lines read; more are reported as skipped.
const MAX_LINES: usize = 1_000_000;

const MESSAGES: &[(&str, &str)] = &[
    ("FWB", "air waybill data"),
    ("FFM", "airline flight manifest"),
    ("FSU", "status update"),
    ("FHL", "consolidation list (house manifest)"),
    ("FZB", "house waybill data"),
    ("FMA", "acknowledgment"),
    ("FNA", "error notification"),
    ("FBL", "booking list"),
    ("FFR", "booking request"),
    ("FFA", "booking answer"),
    ("FSR", "status request"),
    ("FSA", "status answer"),
    ("FWR", "air waybill request"),
    ("FDA", "discrepancy advice"),
    ("FSB", "shipper's letter of instruction"),
    ("CSN", "customs status notification"),
    ("FRI", "freight release information"),
    ("FXF", "freight transfer"),
    ("FCC", "charges correction advice"),
];
const INTERPRETED: &[&str] = &["FWB", "FFM", "FSU", "FHL"];

fn message_name(id: &str) -> Option<&'static str> {
    MESSAGES.iter().find(|(m, _)| *m == id).map(|(_, n)| *n)
}

/// "FWB/16" -> ("FWB", "16").
fn identifier(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_end();
    let (id, version) = line.split_once('/')?;
    (message_name(id).is_some()
        && (1..=3).contains(&version.len())
        && version.bytes().all(|b| b.is_ascii_digit()))
    .then_some((id, version))
}

/// SITA Type B envelope lines before the message: priority and addresses ("QK LHRFMBA"), the
/// origin line (".LHRKKBA 121030") or a separator.
fn type_b(line: &str) -> bool {
    let b = line.as_bytes();
    (b.len() >= 3 && b[0] == b'Q' && b[1].is_ascii_uppercase() && b[2] == b' ')
        || line.starts_with('.')
        || line.starts_with('=')
        || (line.len() >= 7
            && line.split(' ').all(|w| {
                w.len() == 7
                    && w.bytes()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            }))
}

impl Handler for CargoImp {
    fn name(&self) -> &'static str {
        "IATA Cargo-IMP"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let h = &probe.head[content_start(probe.head)..];
        let Ok(text) = std::str::from_utf8(&h[..h.len().min(2048)])
            .or_else(|e| std::str::from_utf8(&h[..e.valid_up_to()]))
        else {
            return false;
        };
        let mut lines = text.lines().map(str::trim_end).filter(|l| !l.is_empty());
        for _ in 0..5 {
            let Some(l) = lines.next() else {
                return false;
            };
            if let Some((id, _)) = identifier(l) {
                // The next line must carry content: an air waybill or a flight.
                let next = lines.next().unwrap_or("");
                return !next.is_empty()
                    && (awb(next).is_some()
                        || next.starts_with("MBI/")
                        || (id == "FFM" && flight_line(next).is_some())
                        || !INTERPRETED.contains(&id));
            }
            if !type_b(l) {
                return false;
            }
        }
        false
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        inspect_bytes(input.bytes, cancelled)
    }
}

// ---------------------------------------------------------------------------------------------
// Pieces of lines

struct Awb<'a> {
    prefix: &'a str,
    serial: &'a str,
    origin: &'a str,
    dest: &'a str,
    rest: &'a str,
}

/// "172-12345675LHRJFK/T5K120.5" -> prefix, serial, origin, destination and the rest.
fn awb(s: &str) -> Option<Awb<'_>> {
    let b = s.as_bytes();
    if b.len() < 12
        || !b[..3].iter().all(u8::is_ascii_digit)
        || b[3] != b'-'
        || !b[4..12].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let after = &s[12..];
    let ab = after.as_bytes();
    let (origin, dest, rest) = if ab.len() >= 6 && ab[..6].iter().all(u8::is_ascii_uppercase) {
        (&after[..3], &after[3..6], &after[6..])
    } else if ab.len() >= 3 && ab[..3].iter().all(u8::is_ascii_uppercase) {
        (&after[..3], "", &after[3..])
    } else {
        ("", "", after)
    };
    Some(Awb {
        prefix: &s[..3],
        serial: &s[4..12],
        origin,
        dest,
        rest,
    })
}

/// The check digit of an 8-digit serial number: the first seven digits mod 7.
fn awb_valid(serial: &str) -> bool {
    let (body, check) = serial.split_at(7.min(serial.len()));
    match (body.parse::<u32>(), check.parse::<u32>()) {
        (Ok(n), Ok(c)) => n % 7 == c,
        _ => false,
    }
}

#[derive(Default, Clone)]
struct Qty {
    code: String,
    pieces: String,
    weight: String,
    unit: String,
    volume: String,
}

/// Quantity detail "T5K120.5MC0.75": shipment description code, pieces, weight code and
/// weight, volume code and volume.
fn quantity(tok: &str) -> Option<Qty> {
    let b = tok.as_bytes();
    let code = *b.first()?;
    if !matches!(code, b'T' | b'P' | b'S' | b'D' | b'M') {
        return None;
    }
    let mut i = 1;
    let digits = |i: &mut usize| {
        let s = *i;
        while *i < b.len() && (b[*i].is_ascii_digit() || b[*i] == b'.') {
            *i += 1;
        }
        tok[s..*i].to_owned()
    };
    let pieces = digits(&mut i);
    if pieces.is_empty() {
        return None;
    }
    let mut q = Qty {
        code: (code as char).to_string(),
        pieces,
        ..Qty::default()
    };
    if i < b.len() && matches!(b[i], b'K' | b'L') {
        q.unit = if b[i] == b'K' { "kg" } else { "lb" }.into();
        i += 1;
        q.weight = digits(&mut i);
    }
    if let Some(code @ ("MC" | "CI" | "CF" | "CC")) = tok.get(i..i + 2) {
        let unit = match code {
            "MC" => "m³",
            "CI" => "in³",
            "CF" => "ft³",
            _ => "cm³",
        };
        i += 2;
        let v = digits(&mut i);
        q.volume = format!("{v} {unit}");
    }
    if tok.get(i..i + 2) == Some("DG") {
        i += 2;
        let _ = digits(&mut i);
    }
    (i == b.len()).then_some(q)
}

fn shipment_code(c: &str) -> &'static str {
    match c {
        "T" => "total consignment",
        "P" => "part consignment",
        "S" => "split consignment",
        "D" => "several shipments (partial)",
        "M" => "missing",
        _ => "",
    }
}

/// FFM flight line "1/BA0175/12NOV/LHR/GABCD": sequence, flight, date, airport, registration.
fn flight_line(l: &str) -> Option<Vec<&str>> {
    let p: Vec<&str> = l.split('/').collect();
    let ok = p.len() >= 4
        && p[0].bytes().all(|b| b.is_ascii_digit())
        && !p[0].is_empty()
        && is_flight(p[1])
        && p[3].len() == 3
        && p[3].bytes().all(|b| b.is_ascii_uppercase());
    ok.then_some(p)
}

/// "BA0175", "LH400A": a two-character carrier code then 1–4 digits and an optional suffix.
fn is_flight(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() >= 3
        && b.len() <= 8
        && b[..2].iter().all(u8::is_ascii_alphanumeric)
        && b[..2].iter().any(u8::is_ascii_uppercase)
        && b[2].is_ascii_digit()
        && b[2..]
            .iter()
            .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
}

/// "12NOV", "12NOV1030", "12NOV26".
fn is_date(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() >= 5
        && b[..2].iter().all(u8::is_ascii_digit)
        && b[2..5].iter().all(u8::is_ascii_uppercase)
        && b[5..].iter().all(u8::is_ascii_digit)
}

fn status_name(c: &str) -> Option<&'static str> {
    Some(match c {
        "FOH" => "freight on hand",
        "RCS" => "received from shipper",
        "BKD" => "booked",
        "MAN" => "manifested",
        "PRE" => "prepared for loading",
        "DEP" => "departed",
        "ARR" => "arrived",
        "RCF" => "received from flight",
        "NFD" => "notified for delivery",
        "AWD" => "documents delivered",
        "DLV" => "delivered",
        "TFD" => "transferred to another airline",
        "RCT" => "received from another airline",
        "CCD" => "cleared by customs",
        "DIS" => "discrepancy",
        "CRC" => "reported to customs",
        "DDL" => "delivered to door",
        "TGC" => "transferred to customs",
        "AWR" => "documents received",
        "TRM" => "to be transferred",
        "DOC" => "documents",
        _ => return None,
    })
}

fn line_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "FLT" => "Flight bookings",
        "RTG" => "Routing",
        "SHP" => "Shipper",
        "CNE" => "Consignee",
        "AGT" => "Agent",
        "NFY" => "Also notify",
        "ACC" => "Accounting information",
        "CVD" => "Charge declarations",
        "RTD" => "Rate description",
        "OTH" => "Other charges",
        "PPD" => "Prepaid charge summary",
        "COL" => "Collect charge summary",
        "CER" => "Shipper's certification",
        "ISU" => "Carrier's execution",
        "OSI" => "Other service information",
        "SSR" => "Special service request",
        "SPH" => "Special handling",
        "REF" => "Sender reference",
        "OCI" => "Other customs information",
        "NAM" => "Name",
        "ADR" => "Address",
        "LOC" => "Location",
        "ULD" => "Unit load device",
        "DIM" => "Dimensions",
        "MBI" => "Master bill information",
        "HBS" => "House waybill summary",
        "TXT" => "Free text",
        "HTS" => "Harmonised tariff schedule",
        "LAST" => "Last part of the manifest",
        "CONT" => "Manifest continues",
        "COR" => "Customs origin",
        "SRI" => "Shipment reference information",
        "CDC" => "Charges in destination currency",
        "NOM" => "Nominated handling party",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------------
// Inspection

struct Line {
    text: String,
    start: usize,
    end: usize,
}

fn inspect_bytes(bytes: &[u8], cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let mut lines: Vec<Line> = vec![];
    let mut truncated = false;
    let mut pos = 0;
    for raw in bytes.split_inclusive(|&b| b == b'\n') {
        if lines.len() >= MAX_LINES {
            truncated = true;
            break;
        }
        if lines.len() % 4096 == 4095 && cancelled() {
            return Err("Cancelled".into());
        }
        let mut content = raw;
        while let [rest @ .., b'\n' | b'\r'] = content {
            content = rest;
        }
        let (start, end) = (pos, pos + content.len());
        pos += raw.len();
        let text = decode(content).trim_end().to_owned();
        if text.trim().is_empty() {
            continue;
        }
        lines.push(Line { text, start, end });
    }
    // Messages start at identifier lines.
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| identifier(l.text.as_str()).is_some())
        .map(|(i, _)| i)
        .collect();
    let Some(&first) = starts.first() else {
        return Err("No Cargo-IMP message identifier".into());
    };
    let (id, version) = identifier(lines[first].text.as_str()).unwrap_or(("", ""));
    let mut out = Inspection::new("IATA Cargo-IMP", "IATA Cargo-IMP message");
    let ids: Vec<&str> = {
        let mut v: Vec<&str> = vec![];
        for &s in &starts {
            if let Some((i, _)) = identifier(lines[s].text.as_str())
                && !v.contains(&i)
            {
                v.push(i);
            }
        }
        v
    };
    out.kind = if starts.len() == 1 {
        format!("{id} — {}", message_name(id).unwrap_or("Cargo-IMP message"))
    } else if ids.len() == 1 {
        format!(
            "{id} — {} messages ({})",
            starts.len(),
            message_name(id).unwrap_or("Cargo-IMP")
        )
    } else {
        format!("Cargo-IMP — {} messages ({})", starts.len(), ids.join(", "))
    };
    out.version = Some(format!("{id}/{version}"));

    let mut ctx = Ctx {
        out: &mut out,
        awbs: Tally::new("Air waybill check digit (serial mod 7)"),
        table: Rows::new(
            "Lines",
            &["#", "Message", "Identifier", "Meaning", "Content"],
        ),
        unread: 0,
        suffix: String::new(),
    };
    for (n, l) in lines.iter().enumerate().take(first) {
        ctx.table.push(
            vec![
                (n + 1).to_string(),
                String::new(),
                String::new(),
                "Type B envelope".into(),
                l.text.as_str().to_owned(),
            ],
            Some(l.start as u64..l.end as u64),
        );
    }
    let mut not_interpreted: Vec<String> = vec![];
    for (k, &s) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(lines.len());
        let msg = &lines[s..end];
        let (mid, mver) = identifier(msg[0].text.as_str()).unwrap_or(("", ""));
        ctx.suffix = if starts.len() > 1 {
            format!(" ({})", k + 1)
        } else {
            String::new()
        };
        if k < 1000 {
            let g = format!("Message{}", ctx.suffix);
            let src = Some(msg[0].start as u64..msg[0].end as u64);
            ctx.out
                .fact(&g, "Message", coded(mid, message_name(mid)), src.clone());
            ctx.out.fact(&g, "Version", mver.to_owned(), src);
        }
        for (j, l) in msg.iter().enumerate() {
            let ident = line_id(l.text.as_str());
            ctx.table.push(
                vec![
                    (s + j + 1).to_string(),
                    (k + 1).to_string(),
                    ident.to_owned(),
                    if j == 0 {
                        "Standard message identifier".into()
                    } else {
                        line_name(ident).unwrap_or("").to_owned()
                    },
                    clip(l.text.as_str(), super::edi::SHOWN),
                ],
                Some(l.start as u64..l.end as u64),
            );
        }
        let body = &msg[1..];
        match mid {
            "FWB" => fwb(&mut ctx, body),
            "FFM" => ffm(&mut ctx, body),
            "FSU" => fsu(&mut ctx, body),
            "FHL" => fhl(&mut ctx, body),
            other => {
                for l in body {
                    if let Some(a) = awb(l.text.as_str()) {
                        ctx.check_awb(&a);
                    }
                }
                if !not_interpreted.contains(&other.to_owned()) {
                    not_interpreted.push(other.to_owned());
                }
            }
        }
    }
    let unread = ctx.unread;
    let awbs = ctx.awbs;
    let table = ctx.table;
    let mut partial = vec![];
    if truncated {
        partial.push(format!("only the first {MAX_LINES} lines were read"));
    }
    if unread > 0 {
        partial.push(format!("{unread} lines were listed but not interpreted"));
    }
    if partial.is_empty() {
        out.check(
            Stage::Parsed,
            State::Passed,
            format!("{} lines read in {} messages", lines.len(), starts.len()),
        );
    } else {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("{} lines read; {}", lines.len(), partial.join("; ")),
        );
    }
    for m in &not_interpreted {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("{m} is recognized but not interpreted: its lines are listed"),
        );
    }
    out.not_validated();
    awbs.emit(&mut out);
    out.notes.push(
        "Interpreted messages: FWB (air waybill), FFM (flight manifest), FSU (status update) and FHL (house manifest), in the common line layout of their recent versions (for example FWB/16, FFM/8, FSU/14, FHL/4); version-specific differences are not distinguished"
            .into(),
    );
    out.notes.push(
        "Other Cargo-IMP messages (FZB, FMA, FNA, FBL, FFR, FFA, FSR, FSA, FWR …) are recognized and their lines listed. Lines are not validated against the Cargo-IMP manual"
            .into(),
    );
    table.emit(&mut out);
    for (n, l) in lines.iter().enumerate().take(crate::inspect::MAX_ITEMS) {
        let ident = line_id(l.text.as_str());
        super::edi::node(
            &mut out,
            usize::from(identifier(l.text.as_str()).is_none() && n >= first),
            match line_name(ident) {
                Some(nm) => format!("{ident} — {nm}"),
                None => ident.to_owned(),
            },
            clip(l.text.as_str(), super::edi::SHOWN),
            Some(l.start as u64..l.end as u64),
        );
    }
    Ok(out)
}

/// The line identifier: "RTD" for "RTD/1/P5", "/" for continuation lines, the AWB for
/// consignment lines.
fn line_id(l: &str) -> &str {
    if l.starts_with('/') {
        return "/";
    }
    if awb(l).is_some() {
        return "AWB";
    }
    let id = l.split('/').next().unwrap_or("");
    if id.len() <= 4 && id.bytes().all(|b| b.is_ascii_uppercase()) {
        id
    } else if l.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
        "#"
    } else {
        ""
    }
}

struct Ctx<'a> {
    out: &'a mut Inspection,
    awbs: Tally,
    table: Rows,
    unread: usize,
    suffix: String,
}
impl Ctx<'_> {
    fn fact(&mut self, group: &str, label: &str, value: impl Into<String>, l: &Line) {
        let g = format!("{group}{}", self.suffix);
        self.out
            .fact(&g, label, value, Some(l.start as u64..l.end as u64));
    }
    fn check_awb(&mut self, a: &Awb) {
        let n = format!("{}-{}", a.prefix, a.serial);
        let detail = format!("{n}: check digit {}", &a.serial[7..]);
        if awb_valid(a.serial) {
            self.awbs.pass(detail);
        } else {
            let expected = a.serial[..7].parse::<u32>().map(|v| v % 7).unwrap_or(0);
            self.awbs.fail(format!(
                "{n}: check digit {}, expected {expected}",
                &a.serial[7..]
            ));
        }
    }
    /// Consignment facts from an AWB line.
    fn consignment(&mut self, group: &str, l: &Line) -> Option<Qty> {
        let a = awb(l.text.as_str())?;
        self.check_awb(&a);
        self.fact(
            group,
            "Air waybill",
            format!("{}-{}", a.prefix, a.serial),
            l,
        );
        self.fact(group, "Origin", a.origin.to_owned(), l);
        self.fact(group, "Destination", a.dest.to_owned(), l);
        let q = a
            .rest
            .trim_start_matches('/')
            .split('/')
            .next()
            .and_then(quantity)?;
        self.fact(
            group,
            "Shipment",
            coded(&q.code, Some(shipment_code(&q.code))),
            l,
        );
        self.fact(group, "Pieces", q.pieces.clone(), l);
        if !q.weight.is_empty() {
            self.fact(group, "Weight", format!("{} {}", q.weight, q.unit), l);
        }
        self.fact(group, "Volume", q.volume.clone(), l);
        Some(q)
    }
}

/// FWB: the air waybill.
fn fwb(ctx: &mut Ctx, body: &[Line]) {
    let mut party: Option<String> = None;
    let mut rates = Rows::new(
        "Rate description",
        &[
            "Line",
            "Pieces",
            "Gross weight",
            "Rate class",
            "Commodity item",
            "Chargeable weight",
            "Rate/charge",
            "Total",
            "Goods",
        ],
    );
    let mut rate: Option<(Vec<String>, usize, usize)> = None;
    let flush = |rates: &mut Rows, rate: &mut Option<(Vec<String>, usize, usize)>| {
        if let Some((cells, a, b)) = rate.take() {
            rates.push(cells, Some(a as u64..b as u64));
        }
    };
    for (j, l) in body.iter().enumerate() {
        let t = l.text.as_str();
        let id = line_id(t);
        if j == 0 && id == "AWB" {
            ctx.consignment("Consignment", l);
            continue;
        }
        if id != "/" && id != "NAM" && id != "ADR" && id != "LOC" {
            party = None;
        }
        if id != "/" && id != "RTD" {
            flush(&mut rates, &mut rate);
        }
        let parts: Vec<&str> = t.split('/').collect();
        let part = |k: usize| parts.get(k).copied().unwrap_or("").trim().to_owned();
        match id {
            "FLT" => {
                let mut f = vec![];
                let mut k = 1;
                while k < parts.len() && f.len() < 20 {
                    let day = part(k + 1);
                    f.push(if day.is_empty() {
                        part(k)
                    } else {
                        format!("{} on day {day}", part(k))
                    });
                    k += 2;
                }
                ctx.fact("Consignment", "Flights", f.join(", "), l);
            }
            "RTG" => {
                let legs: Vec<String> = parts
                    .iter()
                    .skip(1)
                    .take(20)
                    .map(|p| {
                        if p.len() >= 5 && p.is_ascii() {
                            format!("to {} by {}", &p[..3], &p[3..])
                        } else {
                            (*p).to_owned()
                        }
                    })
                    .collect();
                ctx.fact("Consignment", "Routing", legs.join(", "), l);
            }
            "SHP" | "CNE" | "AGT" | "NFY" => {
                let g = line_name(id).unwrap_or("Party").to_owned();
                let account = parts
                    .iter()
                    .skip(1)
                    .find(|p| !p.is_empty())
                    .copied()
                    .unwrap_or("");
                ctx.fact(&g, "Account", account.to_owned(), l);
                party = Some(g);
            }
            "NAM" | "ADR" | "LOC" => {
                if let Some(g) = party.clone() {
                    let label = match id {
                        "NAM" => "Name",
                        "ADR" => "Address",
                        _ => "Place",
                    };
                    let v = parts[1..]
                        .iter()
                        .filter(|p| !p.is_empty())
                        .copied()
                        .collect::<Vec<_>>()
                        .join(", ");
                    ctx.fact(&g, label, v, l);
                }
            }
            "/" => {
                if let Some(g) = party.clone() {
                    let v: Vec<&str> = parts[1..]
                        .iter()
                        .filter(|p| !p.is_empty())
                        .copied()
                        .collect();
                    // Country, postal code and contacts ("/GB/EC1A1BB/TE/0123").
                    let label = if v
                        .first()
                        .is_some_and(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()))
                    {
                        "Country/postal code/contact"
                    } else {
                        "Name/address"
                    };
                    ctx.fact(&g, label, v.join(" "), l);
                } else if let Some((cells, _, b)) = rate.as_mut() {
                    *b = l.end;
                    rate_parts(cells, &parts[1..]);
                } else {
                    ctx.unread += 1;
                }
            }
            "CVD" => {
                ctx.fact("Charges", "Currency", part(1), l);
                ctx.fact("Charges", "Charge code", part(2), l);
                let pp = |v: String| match v.as_str() {
                    "PP" => "prepaid".to_owned(),
                    "CC" => "collect".to_owned(),
                    _ => v,
                };
                ctx.fact("Charges", "Weight/valuation", pp(part(3)), l);
                ctx.fact("Charges", "Other charges", pp(part(4)), l);
                ctx.fact("Charges", "Declared value for carriage", part(5), l);
                ctx.fact("Charges", "Declared value for customs", part(6), l);
                ctx.fact("Charges", "Insurance amount", part(7), l);
            }
            "RTD" => {
                let mut cells = vec![String::new(); 9];
                cells[0] = part(1);
                rate_parts(&mut cells, &parts[2.min(parts.len())..]);
                rate = Some((cells, l.start, l.end));
            }
            "OTH" => ctx.fact("Charges", "Other charges", parts[1..].join("/"), l),
            "PPD" | "COL" => {
                let label = if id == "PPD" { "Prepaid" } else { "Collect" };
                ctx.fact(
                    "Charges",
                    &format!("{label} summary"),
                    parts[1..].join("/"),
                    l,
                );
            }
            "ISU" => {
                ctx.fact("Issue", "Date", part(1), l);
                ctx.fact("Issue", "Place", part(2), l);
                ctx.fact("Issue", "Signature", part(3), l);
            }
            "CER" => ctx.fact("Issue", "Shipper's certification", part(1), l),
            "SPH" => ctx.fact("Consignment", "Special handling", parts[1..].join(" "), l),
            "SSR" | "OSI" | "ACC" | "REF" | "OCI" => {
                let label = line_name(id).unwrap_or(id);
                ctx.fact(
                    "Consignment",
                    label,
                    parts[1..].join(" ").trim().to_owned(),
                    l,
                );
            }
            _ => ctx.unread += 1,
        }
    }
    flush(&mut rates, &mut rate);
    rates.emit(ctx.out);
}

/// RTD elements: P pieces, K/L weight, C rate class, S commodity, W chargeable weight,
/// R rate, T total, NG/NC goods.
fn rate_parts(cells: &mut [String], parts: &[&str]) {
    let mut k = 0;
    while k < parts.len() {
        let p = parts[k];
        if p == "NG" || p == "NC" {
            if let Some(g) = parts.get(k + 1) {
                if !cells[8].is_empty() {
                    cells[8].push_str("; ");
                }
                cells[8].push_str(g);
            }
            k += 2;
            continue;
        }
        if let Some(c) = p.chars().next() {
            let v = &p[c.len_utf8()..];
            match c {
                'P' => cells[1] = v.to_owned(),
                'K' => cells[2] = format!("{v} kg"),
                'L' => cells[2] = format!("{v} lb"),
                'C' => cells[3] = v.to_owned(),
                'S' => cells[4] = v.to_owned(),
                'W' => cells[5] = v.to_owned(),
                'R' => cells[6] = v.to_owned(),
                'T' => cells[7] = v.to_owned(),
                _ => {}
            }
        }
        k += 1;
    }
}

/// FFM: flight, points of unloading, ULDs and the consignments on them.
fn ffm(ctx: &mut Ctx, body: &[Line]) {
    let mut table = Rows::new(
        "Consignments",
        &[
            "Unloading point",
            "ULD",
            "Air waybill",
            "Origin",
            "Destination",
            "Shipment",
            "Pieces",
            "Weight",
            "Volume",
            "Total pieces",
            "Description",
        ],
    );
    let mut unloading = String::new();
    let mut uld = String::new();
    let mut points: Vec<String> = vec![];
    let mut weights: Vec<String> = vec![];
    let mut units: Vec<String> = vec![];
    for (j, l) in body.iter().enumerate() {
        let t = l.text.as_str();
        if j == 0
            && let Some(p) = flight_line(t)
        {
            ctx.fact("Flight", "Message sequence", p[0].to_owned(), l);
            ctx.fact("Flight", "Flight", p[1].to_owned(), l);
            ctx.fact("Flight", "Date", p[2].to_owned(), l);
            ctx.fact("Flight", "Departure airport", p[3].to_owned(), l);
            ctx.fact(
                "Flight",
                "Aircraft registration",
                p.get(4).copied().unwrap_or("").to_owned(),
                l,
            );
            continue;
        }
        if let Some(a) = awb(t) {
            ctx.check_awb(&a);
            let toks: Vec<&str> = a.rest.trim_start_matches('/').split('/').collect();
            let q = toks.first().and_then(|x| quantity(x)).unwrap_or_default();
            let mut total = String::new();
            let mut desc = String::new();
            for tok in toks.iter().skip(1) {
                match quantity(tok) {
                    Some(tq) if tq.code == "T" && total.is_empty() => total = tq.pieces,
                    _ if desc.is_empty() && !tok.is_empty() => desc = (*tok).to_owned(),
                    _ => {}
                }
            }
            if !q.weight.is_empty() {
                weights.push(q.weight.clone());
                if !units.contains(&q.unit) {
                    units.push(q.unit.clone());
                }
            }
            table.push(
                vec![
                    unloading.clone(),
                    uld.clone(),
                    format!("{}-{}", a.prefix, a.serial),
                    a.origin.to_owned(),
                    a.dest.to_owned(),
                    coded(
                        &q.code,
                        Some(shipment_code(&q.code)).filter(|s| !s.is_empty()),
                    ),
                    q.pieces,
                    format!("{} {}", q.weight, q.unit).trim().to_owned(),
                    q.volume,
                    total,
                    desc,
                ],
                Some(l.start as u64..l.end as u64),
            );
            continue;
        }
        let id = line_id(t);
        match id {
            "ULD" => uld = t.get(4..).unwrap_or("").to_owned(),
            "LAST" => ctx.fact("Flight", "Manifest part", "last part", l),
            "CONT" => ctx.fact("Flight", "Manifest part", "continued in another message", l),
            "SSR" | "OSI" | "DIM" | "OCI" | "SCI" | "COR" | "/" => {}
            _ if t
                .get(..3)
                .is_some_and(|c| c.bytes().all(|b| b.is_ascii_uppercase()))
                && (t.len() == 3 || t.as_bytes()[3] == b'/') =>
            {
                unloading = t.get(..3).unwrap_or("").to_owned();
                uld.clear();
                if points.len() < 50 {
                    points.push(t.to_owned());
                }
            }
            _ => ctx.unread += 1,
        }
    }
    if let Some(first) = body.first() {
        ctx.fact("Flight", "Points of unloading", points.join(", "), first);
        ctx.fact("Flight", "Consignments", table.len().to_string(), first);
        if units.len() == 1
            && let Some(sum) = decimal_sum(weights.iter().map(String::as_str))
        {
            ctx.fact(
                "Flight",
                "Weight of the listed consignments (computed)",
                format!("{sum} {}", units[0]),
                first,
            );
        }
    }
    table.emit(ctx.out);
}

/// FSU: the consignment and its status lines.
fn fsu(ctx: &mut Ctx, body: &[Line]) {
    let mut table = Rows::new(
        "Status events",
        &[
            "Status",
            "Meaning",
            "Date/time",
            "Flight",
            "Airports",
            "Pieces",
            "Weight",
            "Details",
        ],
    );
    for (j, l) in body.iter().enumerate() {
        let t = l.text.as_str();
        if j == 0 && awb(t).is_some() {
            ctx.consignment("Consignment", l);
            continue;
        }
        let parts: Vec<&str> = t.split('/').collect();
        let code = parts[0];
        if let Some(name) = status_name(code) {
            let mut when = vec![];
            let mut flight = String::new();
            let mut airports = vec![];
            let mut q = Qty::default();
            let mut details = vec![];
            for p in parts.iter().skip(1) {
                let p = p.trim();
                if p.is_empty() {
                    continue;
                }
                if is_date(p) {
                    when.push(p.to_owned());
                } else if flight.is_empty() && is_flight(p) {
                    flight = p.to_owned();
                } else if (p.len() == 3 || p.len() == 6)
                    && p.bytes().all(|b| b.is_ascii_uppercase())
                {
                    airports.push(p[..3].to_owned());
                    if p.len() == 6 {
                        airports.push(p[3..].to_owned());
                    }
                } else if let Some(x) = quantity(p).filter(|_| q.pieces.is_empty()) {
                    q = x;
                } else if details.len() < 10 {
                    details.push(p.to_owned());
                }
            }
            table.push(
                vec![
                    code.to_owned(),
                    name.to_owned(),
                    when.join(" "),
                    flight,
                    airports.join(" → "),
                    q.pieces,
                    format!("{} {}", q.weight, q.unit).trim().to_owned(),
                    details.join(" / "),
                ],
                Some(l.start as u64..l.end as u64),
            );
        } else if !matches!(code, "OSI" | "SSR" | "OCI" | "REF") && !t.starts_with('/') {
            ctx.unread += 1;
        }
    }
    if let (Some(first), Some(last)) = (body.first(), table.table.rows.last()) {
        let latest = format!("{} ({})", last.cells[0], last.cells[1]);
        ctx.fact("Consignment", "Latest status", latest, first);
    }
    table.emit(ctx.out);
}

/// FHL: master waybill and its house waybills.
fn fhl(ctx: &mut Ctx, body: &[Line]) {
    let mut table = Rows::new(
        "House waybills",
        &[
            "House waybill",
            "Origin",
            "Destination",
            "Pieces",
            "Weight",
            "SLAC",
            "Description",
        ],
    );
    let mut master: Option<(Qty, Range<u64>)> = None;
    let mut pieces: Vec<String> = vec![];
    for l in body {
        let t = l.text.as_str();
        let parts: Vec<&str> = t.split('/').collect();
        match parts[0] {
            "MBI" => {
                let rest = t.get(4..).unwrap_or("");
                let line = Line {
                    text: rest.to_owned(),
                    start: l.start + 4,
                    end: l.end,
                };
                if let Some(q) = ctx.consignment("Master waybill", &line) {
                    master = Some((q, l.start as u64..l.end as u64));
                }
            }
            "HBS" => {
                let get = |k: usize| parts.get(k).copied().unwrap_or("").to_owned();
                let od = get(2);
                let (o, d) = if od.len() == 6 && od.is_ascii() {
                    (od[..3].to_owned(), od[3..].to_owned())
                } else {
                    (od.clone(), String::new())
                };
                let w = get(4);
                let weight = match w.chars().next() {
                    Some('K') => format!("{} kg", &w[1..]),
                    Some('L') => format!("{} lb", &w[1..]),
                    _ => w,
                };
                let p = get(3);
                if !p.is_empty() {
                    pieces.push(p.clone());
                }
                table.push(
                    vec![get(1), o, d, p, weight, get(5), get(6)],
                    Some(l.start as u64..l.end as u64),
                );
            }
            "TXT" | "HTS" | "OCI" | "SHP" | "CNE" | "CVD" | "NAM" | "ADR" | "LOC" => {}
            _ if t.starts_with('/') => {}
            _ => ctx.unread += 1,
        }
    }
    if let Some(first) = body.first() {
        ctx.fact(
            "Master waybill",
            "House waybills",
            table.len().to_string(),
            first,
        );
    }
    if let Some((q, _)) = master
        && !pieces.is_empty()
    {
        let sum = decimal_sum(pieces.iter().map(String::as_str));
        let mut t = Tally::new("House waybill pieces add up to the master waybill's");
        t.compare("FHL", &q.pieces, &sum.unwrap_or_else(|| "?".into()));
        t.emit(ctx.out);
    }
    table.emit(ctx.out);
}
