//! UN/EDIFACT interchanges (ISO 9735), recognized by their first segment (`UNA` service string
//! advice, `UNB` interchange header or a bare `UNH` message header).
//!
//! The UNA advice (or the defaults `:+.? '`) gives the separators and the release character.
//! Envelopes (UNB/UNG/UNH and their trailers) are read and their counts and control references
//! compared. INVOIC, ORDERS and DESADV get a business view (parties, dates, references, line
//! items, totals, tax, packaging hierarchy); GS1 EANCOM subsets are labelled and their GLN, GTIN
//! and SSCC check digits verified. Other message types show their identity and segments.
//! Nothing is checked against the UN/EDIFACT directories or the EANCOM guides.
use super::edi::{
    Doc, Gs1, Level, Rows, Seg, Seps, Tally, coded, content_start, hierarchy, segments_view,
};
use crate::inspect::{
    Handler, Input, Inspection, Probe, Stage, State,
    spec::{compare_total, decimal_sum},
};

pub struct Edifact;

const DEFAULT: Seps = Seps {
    comp: b':',
    elem: b'+',
    rep: None,
    rel: Some(b'?'),
    term: b'\'',
};

/// Separators from a UNA service string advice (`UNA:+.? '`): component, element, decimal mark,
/// release, repetition (or reserved), terminator.
fn una(advice: &[u8]) -> Result<(Seps, u8), String> {
    let a = advice
        .get(3..9)
        .ok_or("The UNA service string advice is cut")?;
    let (comp, elem, dec, rel, rep, term) = (a[0], a[1], a[2], a[3], a[4], a[5]);
    let distinct = comp != elem && comp != term && elem != term;
    let punct = [comp, elem, term]
        .iter()
        .all(|b| b.is_ascii() && !b.is_ascii_alphanumeric());
    if !distinct || !punct {
        return Err("The UNA service string advice has unusable separators".into());
    }
    let unused = |b: u8| b == b' ' || b == comp || b == elem || b == term;
    Ok((
        Seps {
            comp,
            elem,
            rep: (!unused(rep) && rep != rel).then_some(rep),
            rel: (!unused(rel)).then_some(rel),
            term,
        },
        dec,
    ))
}

fn looks_like(h: &[u8]) -> bool {
    if h.starts_with(b"UNA") {
        let Ok((seps, _)) = una(h) else {
            return false;
        };
        let rest = h.get(9..).unwrap_or(&[]);
        let rest = &rest[content_start(rest)..];
        return (rest.starts_with(b"UNB") || rest.starts_with(b"UNH"))
            && rest.get(3) == Some(&seps.elem);
    }
    if let Some(rest) = h.strip_prefix(b"UNB+") {
        // Syntax identifier: four letters then the component separator ("UNOC:3").
        return rest.len() > 5
            && rest[..4].iter().all(u8::is_ascii_alphanumeric)
            && rest[4] == b':';
    }
    if let Some(rest) = h.strip_prefix(b"UNH+") {
        // Message reference, then the message type ("INVOIC:D:96A:UN").
        let Some(plus) = rest.iter().take(36).position(|&b| b == b'+') else {
            return false;
        };
        let ty = rest.get(plus + 1..plus + 8).unwrap_or(&[]);
        return ty.len() == 7 && ty[..6].iter().all(u8::is_ascii_uppercase) && ty[6] == b':';
    }
    false
}

impl Handler for Edifact {
    fn name(&self) -> &'static str {
        "UN/EDIFACT"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let h = &probe.head[content_start(probe.head)..];
        looks_like(h)
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let bytes = input.bytes;
        let from = content_start(bytes);
        let (seps, dec, start, una_seg) = if bytes[from..].starts_with(b"UNA") {
            let (s, d) = una(&bytes[from..])?;
            (s, d, from + 9, Some(from..from + 9))
        } else {
            (DEFAULT, b'.', from, None)
        };
        let doc = Doc::split(bytes, start, seps, cancelled)?;
        if doc.segs.is_empty() {
            return Err("No EDIFACT segments".into());
        }
        Ok(inspect_doc(&doc, dec, una_seg))
    }
}

// ---------------------------------------------------------------------------------------------
// Names

fn segment_name(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "UNA" => "Service string advice",
        "UNB" => "Interchange header",
        "UNG" => "Functional group header",
        "UNH" => "Message header",
        "UNT" => "Message trailer",
        "UNE" => "Functional group trailer",
        "UNZ" => "Interchange trailer",
        "UNS" => "Section control",
        "UGH" => "Anti-collision segment group header",
        "UGT" => "Anti-collision segment group trailer",
        "UCI" => "Interchange response",
        "UCM" => "Message/package response",
        "UCS" => "Segment error indication",
        "UCD" => "Data element error indication",
        "BGM" => "Beginning of message",
        "DTM" => "Date/time/period",
        "RFF" => "Reference",
        "NAD" => "Name and address",
        "CTA" => "Contact information",
        "COM" => "Communication contact",
        "CUX" => "Currencies",
        "PAT" => "Payment terms basis",
        "PCD" => "Percentage details",
        "ALC" => "Allowance or charge",
        "LIN" => "Line item",
        "PIA" => "Additional product id",
        "IMD" => "Item description",
        "QTY" => "Quantity",
        "PRI" => "Price details",
        "MOA" => "Monetary amount",
        "TAX" => "Duty/tax/fee details",
        "CNT" => "Control total",
        "FTX" => "Free text",
        "LOC" => "Place/location identification",
        "TDT" => "Details of transport",
        "TOD" => "Terms of delivery or transport",
        "CPS" => "Consignment packing sequence",
        "PAC" => "Package",
        "GIN" => "Goods identity number",
        "PCI" => "Package identification",
        "MEA" => "Measurements",
        "HAN" => "Handling instructions",
        "EQD" => "Equipment details",
        "ALI" => "Additional information",
        "GIR" => "Related identification numbers",
        "DOC" => "Document/message details",
        "SCC" => "Scheduling conditions",
        "CCI" => "Characteristic/class id",
        "RCS" => "Requirements and conditions",
        "TSR" => "Transport service requirements",
        "ERC" => "Application error information",
        "FII" => "Financial institution information",
        "RNG" => "Range details",
        "APR" => "Additional price information",
        "DGS" => "Dangerous goods",
        "QVR" => "Quantity variances",
        "STS" => "Status",
        "SEQ" => "Sequence details",
        "SGP" => "Split goods placement",
        "IDE" => "Identity",
        "PRC" => "Process identification",
        "PGI" => "Product group information",
        "EQN" => "Number of units",
        "GIS" => "General indicator",
        "INP" => "Parties and instruction",
        _ => return None,
    })
}

fn message_name(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "INVOIC" => "invoice",
        "ORDERS" => "purchase order",
        "ORDRSP" => "purchase order response",
        "ORDCHG" => "purchase order change request",
        "DESADV" => "despatch advice",
        "RECADV" => "receiving advice",
        "PRICAT" => "price/sales catalogue",
        "INVRPT" => "inventory report",
        "SLSRPT" => "sales data report",
        "REMADV" => "remittance advice",
        "PAYMUL" => "multiple payment order",
        "PAYORD" => "payment order",
        "BANSTA" => "banking status",
        "FINSTA" => "financial statement of an account",
        "CREADV" => "credit advice",
        "DEBADV" => "debit advice",
        "DIRDEB" => "direct debit",
        "CONTRL" => "syntax and service report",
        "APERAK" => "application error and acknowledgement",
        "IFTMIN" => "instruction (transport)",
        "IFTSTA" => "international multimodal status report",
        "IFCSUM" => "forwarding and consolidation summary",
        "IFTMBF" => "firm booking",
        "IFTMBC" => "booking confirmation",
        "COPRAR" => "container discharge/loading order",
        "CODECO" => "container gate-in/gate-out report",
        "COARRI" => "container discharge/loading report",
        "BAPLIE" => "bayplan/stowage plan",
        "CUSCAR" => "customs cargo report",
        "CUSDEC" => "customs declaration",
        "CUSRES" => "customs response",
        "DELFOR" => "delivery schedule",
        "DELJIT" => "delivery just in time",
        "PARTIN" => "party information",
        "QALITY" => "quality data",
        "MSCONS" => "metered services consumption report",
        "UTILMD" => "utilities master data",
        "PAXLST" => "passenger list",
        "REQOTE" => "request for quote",
        "QUOTES" => "quote",
        "INSDES" => "instruction to despatch",
        "GENRAL" => "general purpose message",
        "HANMOV" => "cargo/goods handling and movement",
        _ => return None,
    })
}

fn party_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "BY" => "Buyer",
        "SU" => "Supplier",
        "IV" => "Invoicee",
        "DP" => "Delivery party",
        "ST" => "Ship to",
        "SE" => "Seller",
        "PE" => "Payee",
        "PR" => "Payer",
        "II" => "Issuer of invoice",
        "CA" => "Carrier",
        "SF" => "Ship from",
        "CN" => "Consignee",
        "CZ" => "Consignor",
        "UC" => "Ultimate consignee",
        "UD" => "Ultimate customer",
        "OB" => "Ordered by",
        "FW" => "Freight forwarder",
        "LC" => "Party declaring the VAT",
        "SN" => "Store",
        "MF" => "Manufacturer",
        "MS" => "Document/message issuer",
        "MR" => "Message recipient",
        "AB" => "Buyer's agent",
        "DL" => "Factor",
        "PW" => "Pick up party",
        _ => return None,
    })
}

fn date_name(q: &str) -> Option<&'static str> {
    Some(match q {
        "137" => "Document date",
        "2" => "Delivery date requested",
        "3" => "Invoice date",
        "4" => "Order date",
        "10" => "Shipment date requested",
        "11" => "Despatch date",
        "13" => "Terms net due date",
        "17" => "Delivery date estimated",
        "35" => "Delivery date actual",
        "50" => "Goods receipt date",
        "63" => "Delivery date latest",
        "64" => "Delivery date earliest",
        "69" => "Delivery date promised",
        "132" => "Arrival date estimated",
        "140" => "Payment due date",
        "171" => "Reference date",
        "194" => "Start date",
        "206" => "End date",
        "263" => "Invoicing period",
        "324" => "Processing date/period",
        "454" => "Accounting value date",
        _ => return None,
    })
}

fn reference_name(q: &str) -> Option<&'static str> {
    Some(match q {
        "ON" => "Order number (buyer)",
        "VN" => "Order number (supplier)",
        "DQ" => "Delivery note number",
        "AAK" => "Despatch advice number",
        "AAU" => "Despatch note number",
        "VA" => "VAT registration number",
        "FC" => "Fiscal number",
        "GN" => "Government reference number",
        "XA" => "Company registration number",
        "CT" => "Contract number",
        "IV" => "Invoice number",
        "PL" => "Price list number",
        "BM" => "Bill of lading number",
        "CR" => "Customer reference number",
        "SRN" => "Shipment reference number",
        "ALO" => "Receiving advice number",
        "API" => "Additional party identification",
        "AAS" => "Transport document number",
        "CU" => "Consignor's reference",
        "ADE" => "Account number",
        "LI" => "Line item reference number",
        "ACD" => "Additional reference number",
        "AAN" => "Delivery schedule number",
        "UC" => "Ultimate customer's reference",
        _ => return None,
    })
}

fn amount_name(q: &str) -> Option<&'static str> {
    Some(match q {
        "77" => "Invoice amount",
        "79" => "Total line items amount",
        "125" => "Taxable amount",
        "176" => "Message total duty/tax/fee amount",
        "124" => "Tax amount",
        "203" => "Line item amount",
        "128" => "Total amount",
        "86" => "Message total monetary amount",
        "39" => "Invoice total amount",
        "9" => "Amount due/amount payable",
        "131" => "Total charges/allowances",
        "259" => "Total charges",
        "260" => "Total allowances",
        "204" => "Allowance amount",
        "23" => "Charge amount",
        "66" => "Goods item total",
        "150" => "Value added tax",
        "138" => "Payment discount amount",
        "113" => "Prepaid amount",
        "165" => "Adjustment amount",
        "8" => "Allowance or charge amount",
        "146" => "Unit price",
        _ => return None,
    })
}

fn quantity_name(q: &str) -> Option<&'static str> {
    Some(match q {
        "47" => "Invoiced quantity",
        "46" => "Delivered quantity",
        "21" => "Ordered quantity",
        "12" => "Despatched quantity",
        "59" => "Consumer units in traded unit",
        "113" => "Quantity to be delivered",
        "192" => "Free goods quantity",
        "61" => "Return quantity",
        "52" => "Quantity per pack",
        "1" => "Discrete quantity",
        "48" => "Received quantity",
        "194" => "Received and accepted",
        _ => return None,
    })
}

fn document_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "380" => "Commercial invoice",
        "381" => "Credit note",
        "383" => "Debit note",
        "384" => "Corrected invoice",
        "386" => "Prepayment invoice",
        "389" => "Self-billed invoice",
        "393" => "Factored invoice",
        "325" => "Proforma invoice",
        "220" => "Order",
        "221" => "Blanket order",
        "224" => "Rush order",
        "226" => "Call off order",
        "227" => "Consignment order",
        "351" => "Despatch advice",
        "231" => "Purchase order response",
        "230" => "Purchase order change",
        "632" => "Goods receipt",
        _ => return None,
    })
}

fn function_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "9" => "Original",
        "1" => "Cancellation",
        "5" => "Replace",
        "7" => "Duplicate",
        "31" => "Copy",
        "4" => "Change",
        "6" => "Confirmation",
        "43" => "Additional transmission",
        "29" => "Accepted without amendment",
        "27" => "Not accepted",
        _ => return None,
    })
}

fn qualifier_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "1" => "DUNS",
        "14" => "GS1 GLN",
        "30" => "ISO 6523",
        "ZZ" | "ZZZ" => "mutually defined",
        _ => return None,
    })
}

fn package_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "200" => "half pallet ISO 0",
        "201" => "pallet ISO 1",
        "202" => "pallet ISO 2",
        "CT" => "carton",
        "BX" => "box",
        "PK" => "package",
        "PX" => "pallet",
        "CS" => "case",
        "RO" => "roll",
        "BG" => "bag",
        _ => return None,
    })
}

/// DTM value with its format: 102 CCYYMMDD, 203 CCYYMMDDHHMM, 204 CCYYMMDDHHMMSS, 718 range.
fn dtm_value(value: &str, format: &str) -> String {
    match format {
        "102" | "203" | "204" => super::edi::date(value),
        "718" if value.len() == 17 && value.is_ascii() => {
            let (a, b) = (&value[..8], &value[9..]);
            if value.as_bytes()[8] == b'-' {
                format!("{value} ({} to {})", iso(a), iso(b))
            } else {
                value.to_owned()
            }
        }
        _ => value.to_owned(),
    }
}
fn iso(d: &str) -> String {
    if d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..8])
    } else {
        d.to_owned()
    }
}

// ---------------------------------------------------------------------------------------------
// Envelopes

struct Message {
    unh: usize,
    unt: Option<usize>,
    reference: String,
    ty: String,
    version: String,
    release: String,
    agency: String,
    assoc: String,
}
impl Message {
    fn directory(&self) -> String {
        match (self.version.as_str(), self.release.as_str()) {
            ("", "") => String::new(),
            (v, "") => v.to_owned(),
            (v, r) => format!("{v}.{r}"),
        }
    }
    fn eancom(&self) -> bool {
        self.assoc.starts_with("EAN")
    }
}

fn inspect_doc(doc: &Doc, dec: u8, una_seg: Option<std::ops::Range<usize>>) -> Inspection {
    let mut out = Inspection::new("UN/EDIFACT", "UN/EDIFACT interchange");
    let mut partial: Vec<String> = vec![];

    // Walk the envelopes.
    let mut messages: Vec<Message> = vec![];
    let mut open: Option<usize> = None;
    let mut unt_count = Tally::new("UNT segment count");
    let mut unt_ref = Tally::new("UNT message reference matches UNH");
    let mut unz_count = Tally::new("UNZ count");
    let mut unz_ref = Tally::new("UNZ control reference matches UNB");
    let mut une_count = Tally::new("UNE message count");
    let mut une_ref = Tally::new("UNE group reference matches UNG");
    let mut unb: Option<usize> = None;
    let mut ung: Option<usize> = None;
    let mut msgs_in_ichg = 0usize;
    let mut groups_in_ichg = 0usize;
    let mut msgs_in_group = 0usize;
    let mut interchanges = 0usize;
    let mut outside = 0usize;
    let mut odd_tags = 0usize;
    for (i, s) in doc.segs.iter().enumerate() {
        let tag = doc.tag(s);
        if tag.len() != 3
            || !tag
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            odd_tags += 1;
        }
        match tag {
            "UNB" => {
                if unb.is_some() {
                    partial.push("an interchange has no UNZ trailer".into());
                }
                unb = Some(i);
                interchanges += 1;
                msgs_in_ichg = 0;
                groups_in_ichg = 0;
            }
            "UNG" => {
                ung = Some(i);
                groups_in_ichg += 1;
                msgs_in_group = 0;
            }
            "UNH" => {
                if let Some(m) = open.take() {
                    unt_count.fail(format!(
                        "message {} has no UNT trailer",
                        messages[m].reference
                    ));
                }
                let c = doc.comps(s, 2);
                let get = |k: usize| c.get(k).cloned().unwrap_or_default();
                messages.push(Message {
                    unh: i,
                    unt: None,
                    reference: doc.el(s, 1),
                    ty: get(0),
                    version: get(1),
                    release: get(2),
                    agency: get(3),
                    assoc: get(4),
                });
                open = Some(messages.len() - 1);
                msgs_in_ichg += 1;
                msgs_in_group += 1;
            }
            "UNT" => match open.take() {
                Some(m) => {
                    let msg = &mut messages[m];
                    msg.unt = Some(i);
                    let ctx = format!("message {}", msg.reference);
                    unt_count.compare(&ctx, &doc.el(s, 1), &(i - msg.unh + 1).to_string());
                    unt_ref.compare(&ctx, &doc.el(s, 2), &msg.reference);
                }
                None => outside += 1,
            },
            "UNE" => match ung.take() {
                Some(g) => {
                    let gref = doc.el(&doc.segs[g], 5);
                    let ctx = format!("group {gref}");
                    une_count.compare(&ctx, &doc.el(s, 1), &msgs_in_group.to_string());
                    une_ref.compare(&ctx, &doc.el(s, 2), &gref);
                }
                None => outside += 1,
            },
            "UNZ" => match unb.take() {
                Some(b) => {
                    let bref = doc.el(&doc.segs[b], 5);
                    let ctx = format!("interchange {bref}");
                    let n = if groups_in_ichg > 0 {
                        groups_in_ichg
                    } else {
                        msgs_in_ichg
                    };
                    unz_count.compare(&ctx, &doc.el(s, 1), &n.to_string());
                    unz_ref.compare(&ctx, &doc.el(s, 2), &bref);
                }
                None => outside += 1,
            },
            _ => {
                if open.is_none() {
                    outside += 1;
                }
            }
        }
    }
    if let Some(m) = open {
        unt_count.fail(format!(
            "message {} has no UNT trailer",
            messages[m].reference
        ));
        partial.push(format!(
            "message {} ends without UNT",
            messages[m].reference
        ));
    }
    if unb.is_some() {
        partial.push("the interchange ends without UNZ".into());
    }
    if ung.is_some() {
        partial.push("a functional group ends without UNE".into());
    }
    if outside > 0 {
        partial.push(format!(
            "{outside} segments outside any message or envelope"
        ));
    }
    if odd_tags > 0 {
        partial.push(format!("{odd_tags} segments without a valid tag"));
    }
    if doc.truncated {
        partial.push(format!(
            "only the first {} segments were read",
            super::edi::MAX_SEGMENTS
        ));
    }
    if doc.unterminated {
        partial.push("the last segment has no terminator (the file may be cut)".into());
    }

    // Kind and version.
    let mut types: Vec<&str> = vec![];
    for m in &messages {
        if !types.contains(&m.ty.as_str()) && types.len() < 20 {
            types.push(&m.ty);
        }
    }
    let eancom = messages.iter().any(Message::eancom);
    out.kind = match types.as_slice() {
        [] => "UN/EDIFACT interchange (no messages)".into(),
        [t] => {
            let base = match message_name(t) {
                Some(n) => format!("{t} — {n}"),
                None => format!("{t} — UN/EDIFACT message"),
            };
            if eancom {
                format!("{base} (GS1 EANCOM)")
            } else {
                base
            }
        }
        many => format!(
            "UN/EDIFACT interchange — {} messages ({})",
            messages.len(),
            many.join(", ")
        ),
    };
    if let Some(m) = messages.first() {
        let mut v = m.directory();
        if !m.agency.is_empty() {
            v.push_str(&format!(" ({})", m.agency));
        }
        if m.eancom() {
            v.push_str(&format!(", EANCOM subset {}", m.assoc));
        }
        out.version = Some(v.trim().to_owned()).filter(|v| !v.is_empty());
    }

    // Interchange facts.
    if let Some(r) = &una_seg {
        let s = doc.bytes.get(r.clone()).unwrap_or(&[]);
        out.fact(
            "Interchange",
            "Service string advice",
            super::edi::decode(s),
            Some(r.start as u64..r.end as u64),
        );
    }
    out.fact(
        "Interchange",
        "Separators",
        format!(
            "component {}  element {}  decimal {}  release {}  terminator {}",
            doc.seps.comp as char,
            doc.seps.elem as char,
            dec as char,
            doc.seps.rel.map(|b| b as char).unwrap_or('—'),
            doc.seps.term as char
        ),
        None,
    );
    if let Some(s) = doc.segs.iter().find(|s| doc.tag(s) == "UNB") {
        let src = doc.src(s);
        let syn = doc.comps(s, 1);
        out.fact(
            "Interchange",
            "Syntax",
            format!(
                "{} version {}",
                syn.first().cloned().unwrap_or_default(),
                syn.get(1).cloned().unwrap_or_default()
            ),
            src.clone(),
        );
        for (label, i) in [("Sender", 2), ("Recipient", 3)] {
            let c = doc.comps(s, i);
            let id = c.first().cloned().unwrap_or_default();
            let q = c.get(1).cloned().unwrap_or_default();
            let v = if q.is_empty() {
                id
            } else {
                format!("{id} (qualifier {})", coded(&q, qualifier_name(&q)))
            };
            out.fact("Interchange", label, v, src.clone());
        }
        let dt = doc.comps(s, 4);
        out.fact("Interchange", "Prepared", dt.join(" "), src.clone());
        out.fact(
            "Interchange",
            "Control reference",
            doc.el(s, 5),
            src.clone(),
        );
        out.fact(
            "Interchange",
            "Application reference",
            doc.el(s, 7),
            src.clone(),
        );
        if doc.el(s, 11) == "1" {
            out.fact("Interchange", "Test indicator", "1 (test)", src.clone());
        }
    }
    if interchanges > 1 {
        out.fact(
            "Interchange",
            "Interchanges in file",
            interchanges.to_string(),
            None,
        );
    }
    if let Some(s) = doc.segs.iter().find(|s| doc.tag(s) == "UNG") {
        let src = doc.src(s);
        out.fact("Group", "Message group", doc.el(s, 1), src.clone());
        out.fact("Group", "Sender", doc.comp(s, 2, 1), src.clone());
        out.fact("Group", "Recipient", doc.comp(s, 3, 1), src.clone());
        out.fact("Group", "Prepared", doc.comps(s, 4).join(" "), src.clone());
        out.fact("Group", "Group reference", doc.el(s, 5), src);
    }

    // Messages.
    let multi = messages.len() > 1;
    let mut table = Rows::new(
        "Messages",
        &[
            "Reference",
            "Type",
            "Directory",
            "Agency",
            "Association code",
            "Segments",
            "UNT declares",
        ],
    );
    let mut gs1 = Gs1::default();
    let mut lines = Lines::new();
    let mut packages = Rows::new(
        "Packages",
        &[
            "Package (CPS)",
            "Parent",
            "Packaging level",
            "Packages",
            "Package type",
            "SSCC",
            "Lines",
            "Message",
        ],
    );
    let mut taxes = Rows::new(
        "Tax summary",
        &[
            "Type",
            "Category",
            "Rate %",
            "Taxable amount",
            "Tax amount",
            "Message",
        ],
    );
    let mut levels: Vec<Level> = vec![];
    let mut no_view: Vec<String> = vec![];
    let mut checks = Checks::default();
    for (k, m) in messages.iter().enumerate() {
        let end = m.unt.unwrap_or(doc.segs.len() - 1).max(m.unh);
        let segs = &doc.segs[m.unh..=end];
        let (Some(first), Some(last)) = (segs.first(), segs.last()) else {
            continue;
        };
        table.push(
            vec![
                m.reference.clone(),
                m.ty.clone(),
                m.directory(),
                m.agency.clone(),
                m.assoc.clone(),
                segs.len().to_string(),
                m.unt.map(|u| doc.el(&doc.segs[u], 1)).unwrap_or_default(),
            ],
            doc.span(first, last),
        );
        let suffix = if multi {
            format!(" ({})", m.reference)
        } else {
            String::new()
        };
        if k < 1000 {
            let g = format!("Message{suffix}");
            let src = doc.src(first);
            out.fact(&g, "Message reference", m.reference.clone(), src.clone());
            out.fact(
                &g,
                "Message type",
                coded(&m.ty, message_name(&m.ty)),
                src.clone(),
            );
            out.fact(&g, "Directory", m.directory(), src.clone());
            out.fact(&g, "Controlling agency", m.agency.clone(), src.clone());
            if m.eancom() {
                out.fact(&g, "EANCOM subset", eancom_label(m), src.clone());
            } else {
                out.fact(&g, "Association code", m.assoc.clone(), src.clone());
            }
        }
        let kind = match m.ty.as_str() {
            "INVOIC" => Some(Kind::Invoice),
            "ORDERS" => Some(Kind::Order),
            "DESADV" => Some(Kind::Despatch),
            _ => None,
        };
        let mut ctx = Ctx {
            doc,
            out: &mut out,
            suffix,
            msg: m.reference.clone(),
            dec,
            gs1: &mut gs1,
            lines: &mut lines,
            packages: &mut packages,
            taxes: &mut taxes,
            levels: &mut levels,
            checks: &mut checks,
            details: k < 1000,
        };
        match kind {
            Some(kind) => commercial(&mut ctx, segs, kind),
            None => {
                identifiers(&mut ctx, segs);
                if !no_view.contains(&m.ty) && no_view.len() < 20 {
                    no_view.push(m.ty.clone());
                }
            }
        }
    }
    if messages.len() > 1000 {
        out.notes.push(
            "Facts are listed for the first 1000 messages; the tables cover all of them".into(),
        );
    }

    // Checks.
    if partial.is_empty() {
        out.check(
            Stage::Parsed,
            State::Passed,
            format!(
                "{} segments read in {} messages",
                doc.segs.len(),
                messages.len()
            ),
        );
    } else {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("{} segments read; {}", doc.segs.len(), partial.join("; ")),
        );
    }
    for t in &no_view {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("No business view for {t} messages: identity and segments only"),
        );
    }
    out.not_validated();
    for t in [unt_count, unt_ref, une_count, une_ref, unz_count, unz_ref] {
        t.emit(&mut out);
    }
    checks.emit(&mut out);
    gs1.emit(&mut out);

    // Notes.
    out.notes.push(
        "Envelopes, counts and control references are checked; segments are not validated against the UN/EDIFACT directory or an implementation guide"
            .into(),
    );
    out.notes.push(
        "Business views: INVOIC, ORDERS and DESADV (header, parties, references, lines, totals, tax, packaging); other message types show their identity and segments"
            .into(),
    );
    if eancom {
        out.notes.push(
            "GS1 EANCOM recognized by the UNH association code (EAN…); GLN, GTIN and SSCC check digits are verified, not the EANCOM subset rules"
                .into(),
        );
    }
    out.notes.push(
        "Values are shown as written, release characters removed; text is read as UTF-8, or as ISO 8859-1 when it is not UTF-8"
            .into(),
    );

    // Tables and outline.
    table.emit(&mut out);
    lines.emit(&mut out);
    packages.emit(&mut out);
    taxes.emit(&mut out);
    hierarchy(&mut out, "Packaging hierarchy (CPS)", &levels);
    if let Some(r) = una_seg {
        super::edi::node(
            &mut out,
            0,
            "UNA — Service string advice",
            super::edi::decode(doc.bytes.get(r.clone()).unwrap_or(&[])),
            Some(r.start as u64..r.end as u64),
        );
    }
    segments_view(
        doc,
        &mut out,
        &["UNB", "UNG", "UNH"],
        &["UNZ", "UNE", "UNT"],
        segment_name,
    );
    out
}

fn eancom_label(m: &Message) -> String {
    let release = match m.release.as_str() {
        "96A" => Some("EANCOM 1997"),
        "01B" => Some("EANCOM 2002"),
        _ => None,
    };
    match release {
        Some(r) => format!("{} on directory {} ({r})", m.assoc, m.directory()),
        None => format!("{} on directory {}", m.assoc, m.directory()),
    }
}

// ---------------------------------------------------------------------------------------------
// Business views

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Invoice,
    Order,
    Despatch,
}
impl Kind {
    fn group(self) -> &'static str {
        match self {
            Self::Invoice => "Invoice",
            Self::Order => "Order",
            Self::Despatch => "Despatch advice",
        }
    }
    /// The quantity qualifier that is the line's main quantity.
    fn quantity(self) -> &'static str {
        match self {
            Self::Invoice => "47",
            Self::Order => "21",
            Self::Despatch => "12",
        }
    }
}

struct Ctx<'a, 'd> {
    doc: &'a Doc<'d>,
    out: &'a mut Inspection,
    suffix: String,
    msg: String,
    dec: u8,
    gs1: &'a mut Gs1,
    lines: &'a mut Lines,
    packages: &'a mut Rows,
    taxes: &'a mut Rows,
    levels: &'a mut Vec<Level>,
    checks: &'a mut Checks,
    /// Facts are written (false past the first messages of a big interchange).
    details: bool,
}
impl Ctx<'_, '_> {
    fn fact(&mut self, group: &str, label: &str, value: impl Into<String>, s: &Seg) {
        if self.details {
            let g = format!("{group}{}", self.suffix);
            self.out.fact(&g, label, value, self.doc.src(s));
        }
    }
    /// An amount with the decimal mark normalised to '.' for arithmetic.
    fn number(&self, v: &str) -> String {
        if self.dec == b',' {
            v.replace(',', ".")
        } else {
            v.to_owned()
        }
    }
}

#[derive(Default)]
struct Checks {
    line_counts: Option<Tally>,
    line_sums: Vec<(String, Option<String>, Option<String>)>,
}
impl Checks {
    fn emit(self, out: &mut Inspection) {
        if let Some(t) = self.line_counts {
            t.emit(out);
        }
        for (what, declared, computed) in self.line_sums {
            compare_total(out, &what, declared, computed);
        }
    }
}

struct Lines {
    rows: Rows,
}
impl Lines {
    fn new() -> Self {
        Self {
            rows: Rows::new(
                "Line items",
                &[
                    "Line",
                    "Product ID",
                    "ID type",
                    "Other IDs",
                    "Description",
                    "Quantity",
                    "Unit",
                    "Other quantities",
                    "Price",
                    "Line amount",
                    "Tax %",
                    "Package (CPS)",
                    "Message",
                ],
            ),
        }
    }
    fn emit(self, out: &mut Inspection) {
        self.rows.emit(out);
    }
}

#[derive(Default)]
struct Line {
    no: String,
    id: String,
    id_type: String,
    other_ids: Vec<String>,
    description: String,
    qty: String,
    unit: String,
    other_qty: Vec<String>,
    price: String,
    amount: Option<String>,
    tax: String,
    cps: String,
}

#[derive(Default)]
struct Package {
    id: String,
    parent: String,
    level: String,
    count: String,
    ty: String,
    sscc: Vec<String>,
    lines: usize,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Section {
    Header,
    Detail,
    Summary,
}

fn is_gs1_item_type(t: &str) -> bool {
    matches!(t, "SRV" | "EN" | "UP" | "SRS")
}

/// GS1 identifiers in messages without a business view.
fn identifiers(ctx: &mut Ctx, segs: &[Seg]) {
    let doc = ctx.doc;
    for s in segs {
        match doc.tag(s) {
            "NAD" => {
                if doc.comp(s, 2, 3) == "9" {
                    let place = format!("NAD+{}", doc.el(s, 1));
                    ctx.gs1.add("GLN", &doc.comp(s, 2, 1), &place, doc.src(s));
                }
            }
            "LIN" => {
                if is_gs1_item_type(&doc.comp(s, 3, 2)) {
                    ctx.gs1.add("GTIN", &doc.comp(s, 3, 1), "LIN", doc.src(s));
                }
            }
            "GIN" if doc.el(s, 1) == "BJ" => {
                for i in 2..=doc.count(s).min(6) {
                    ctx.gs1
                        .add("SSCC", &doc.comp(s, i, 1), "GIN+BJ", doc.src(s));
                }
            }
            _ => {}
        }
    }
}

fn commercial(ctx: &mut Ctx, segs: &[Seg], kind: Kind) {
    let doc = ctx.doc;
    let group = kind.group();
    let mut section = Section::Header;
    let mut party: Option<String> = None;
    let mut line: Option<(Line, usize, usize)> = None;
    let mut pkg: Option<Package> = None;
    let mut tax_row: Option<(Vec<String>, usize, usize)> = None;
    let mut line_count = 0usize;
    let mut line_amounts: Vec<String> = vec![];
    let mut all_lines_have_amount = true;
    let mut declared_lines: Option<(String, usize)> = None;
    let mut total_79: Option<String> = None;
    let mut tax_amounts: Vec<String> = vec![];
    let mut total_176: Option<String> = None;
    let mut free_text = 0;
    let mut last_rff: Option<String> = None;

    let flush_line = |ctx: &mut Ctx,
                      line: &mut Option<(Line, usize, usize)>,
                      pkg: &mut Option<Package>,
                      amounts: &mut Vec<String>,
                      all: &mut bool| {
        if let Some((l, a, b)) = line.take() {
            match &l.amount {
                Some(v) => amounts.push(ctx.number(v)),
                None => *all = false,
            }
            if let Some(p) = pkg.as_mut() {
                p.lines += 1;
            }
            let src = ctx.doc.span(&segs[a], &segs[b]);
            ctx.lines.rows.push(
                vec![
                    l.no,
                    l.id,
                    l.id_type,
                    l.other_ids.join(", "),
                    l.description,
                    l.qty,
                    l.unit,
                    l.other_qty.join(", "),
                    l.price,
                    l.amount.unwrap_or_default(),
                    l.tax,
                    l.cps,
                    ctx.msg.clone(),
                ],
                src,
            );
        }
    };
    let flush_pkg = |ctx: &mut Ctx, pkg: &mut Option<Package>| {
        if let Some(p) = pkg.take() {
            let src = ctx.doc.span(&segs[p.start], &segs[p.end]);
            let ty = coded(&p.ty, package_name(&p.ty));
            let mut value = vec![];
            if !p.count.is_empty() || !p.ty.is_empty() {
                value.push(format!("{} × {}", p.count, ty).trim().to_owned());
            }
            if !p.sscc.is_empty() {
                value.push(format!("SSCC {}", p.sscc.join(", ")));
            }
            if p.lines > 0 {
                value.push(format!("{} lines", p.lines));
            }
            let label = if ctx.suffix.is_empty() {
                format!("CPS {}", p.id)
            } else {
                format!("CPS {} — message {}", p.id, ctx.msg)
            };
            ctx.levels.push(Level {
                id: format!("{}\u{1}{}", ctx.msg, p.id),
                parent: if p.parent.is_empty() {
                    String::new()
                } else {
                    format!("{}\u{1}{}", ctx.msg, p.parent)
                },
                label,
                value: value.join("; "),
                source: src.clone(),
            });
            ctx.packages.push(
                vec![
                    p.id,
                    p.parent,
                    p.level,
                    p.count,
                    ty,
                    p.sscc.join(", "),
                    p.lines.to_string(),
                    ctx.msg.clone(),
                ],
                src,
            );
        }
    };

    for (i, s) in segs.iter().enumerate() {
        let tag = doc.tag(s);
        // Close a line group at the next line, package, section or trailer.
        if matches!(tag, "LIN" | "CPS" | "UNS" | "CNT" | "UNT") {
            flush_line(
                ctx,
                &mut line,
                &mut pkg,
                &mut line_amounts,
                &mut all_lines_have_amount,
            );
        }
        if matches!(tag, "CPS" | "UNS" | "CNT" | "UNT") {
            flush_pkg(ctx, &mut pkg);
        }
        if let Some(p) = pkg.as_mut() {
            p.end = i;
        }
        if tag != "MOA"
            && let Some((row, a, b)) = tax_row.take()
        {
            let mut row = row;
            row.push(ctx.msg.clone());
            let src = doc.span(&segs[a], &segs[b]);
            ctx.taxes.push(row, src);
        }
        if !matches!(tag, "RFF" | "CTA" | "COM" | "LOC" | "FII" | "DTM" | "NAD") {
            party = None;
        }
        match tag {
            "BGM" => {
                let code = doc.comp(s, 1, 1);
                ctx.fact(
                    group,
                    "Document type",
                    coded(&code, document_name(&code)),
                    s,
                );
                let number = match doc.comp(s, 2, 1) {
                    n if n.is_empty() => doc.el(s, 2),
                    n => n,
                };
                let label = match kind {
                    Kind::Invoice => "Invoice number",
                    Kind::Order => "Order number",
                    Kind::Despatch => "Despatch advice number",
                };
                ctx.fact(group, label, number, s);
                let f = doc.comp(s, 3, 1);
                ctx.fact(group, "Message function", coded(&f, function_name(&f)), s);
            }
            "DTM" => {
                let c = doc.comps(s, 1);
                let q = c.first().cloned().unwrap_or_default();
                let v = c.get(1).cloned().unwrap_or_default();
                let fmt = c.get(2).cloned().unwrap_or_default();
                let value = dtm_value(&v, &fmt);
                if let Some((_, _, b)) = line.as_mut() {
                    *b = i;
                } else if let Some(r) = &last_rff {
                    let r = r.clone();
                    ctx.fact(group, &format!("{r} date"), value, s);
                } else if section == Section::Header {
                    let label = match date_name(&q) {
                        Some(n) => n.to_owned(),
                        None => format!("Date (qualifier {q})"),
                    };
                    ctx.fact(group, &label, value, s);
                }
            }
            "RFF" => {
                let q = doc.comp(s, 1, 1);
                let v = doc.comp(s, 1, 2);
                let label = match reference_name(&q) {
                    Some(n) => n.to_owned(),
                    None => format!("Reference ({q})"),
                };
                if let Some((l, _, b)) = line.as_mut() {
                    *b = i;
                    l.other_ids.push(format!("{q} {v}"));
                } else if let Some(p) = &party {
                    let p = p.clone();
                    ctx.fact(&p, &label, v, s);
                } else if section == Section::Header {
                    ctx.fact(group, &label, v, s);
                }
            }
            "NAD" => {
                let q = doc.el(s, 1);
                let g = format!("{} ({q})", party_name(&q).unwrap_or("Party"));
                let id = doc.comp(s, 2, 1);
                let agency = doc.comp(s, 2, 3);
                if agency == "9" {
                    ctx.gs1.add("GLN", &id, &format!("NAD+{q}"), doc.src(s));
                    ctx.fact(&g, "GLN", id, s);
                } else if !id.is_empty() {
                    let v = if agency.is_empty() {
                        id
                    } else {
                        format!("{id} (agency {agency})")
                    };
                    ctx.fact(&g, "Party ID", v, s);
                }
                let mut name = doc.comps(s, 4);
                name.retain(|c| !c.is_empty());
                let name = if name.is_empty() {
                    doc.comps(s, 3)
                        .into_iter()
                        .filter(|c| !c.is_empty())
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    name.join(" ")
                };
                ctx.fact(&g, "Name", name, s);
                let street: Vec<String> = doc
                    .comps(s, 5)
                    .into_iter()
                    .filter(|c| !c.is_empty())
                    .collect();
                ctx.fact(&g, "Street", street.join(", "), s);
                let city = [doc.el(s, 8), doc.el(s, 6)]
                    .into_iter()
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                ctx.fact(&g, "City", city, s);
                ctx.fact(&g, "Country", doc.el(s, 9), s);
                party = Some(g);
                if let Some((_, _, b)) = line.as_mut() {
                    *b = i;
                }
            }
            "CTA" => {
                if let Some(p) = party.clone() {
                    ctx.fact(&p, "Contact", doc.comp(s, 2, 2), s);
                }
            }
            "COM" => {
                if let Some(p) = party.clone() {
                    let label = match doc.comp(s, 1, 2).as_str() {
                        "TE" => "Telephone",
                        "EM" => "E-mail",
                        "FX" => "Fax",
                        _ => "Communication",
                    };
                    ctx.fact(&p, label, doc.comp(s, 1, 1), s);
                }
            }
            "CUX" => {
                let c = doc.comp(s, 1, 2);
                ctx.fact(group, "Currency", c, s);
                let c2 = doc.comp(s, 2, 2);
                ctx.fact(group, "Second currency", c2, s);
            }
            "TDT" => {
                let mode = doc.comp(s, 3, 1);
                let carrier = doc.comp(s, 5, 1);
                let mut v = vec![];
                if !doc.el(s, 2).is_empty() {
                    v.push(format!("conveyance {}", doc.el(s, 2)));
                }
                if !mode.is_empty() {
                    v.push(format!("mode {mode}"));
                }
                if !carrier.is_empty() {
                    v.push(format!("carrier {carrier}"));
                }
                ctx.fact(group, "Transport", v.join(", "), s);
            }
            "FTX" if section == Section::Header && free_text < 5 => {
                free_text += 1;
                let text: Vec<String> = doc
                    .comps(s, 4)
                    .into_iter()
                    .filter(|c| !c.is_empty())
                    .collect();
                ctx.fact(
                    group,
                    &format!("Free text {free_text} ({})", doc.el(s, 1)),
                    text.join(" "),
                    s,
                );
            }
            "CPS" => {
                section = Section::Detail;
                pkg = Some(Package {
                    id: doc.el(s, 1),
                    parent: doc.el(s, 2),
                    level: doc.comp(s, 3, 1),
                    start: i,
                    end: i,
                    ..Package::default()
                });
            }
            "PAC" => {
                if let Some(p) = pkg.as_mut() {
                    p.count = doc.el(s, 1);
                    p.ty = doc.comp(s, 3, 1);
                }
            }
            "GIN" => {
                let q = doc.el(s, 1);
                for k in 2..=doc.count(s).min(6) {
                    let v = doc.comp(s, k, 1);
                    if v.is_empty() {
                        continue;
                    }
                    if q == "BJ" {
                        ctx.gs1.add("SSCC", &v, "GIN+BJ", doc.src(s));
                        match (line.as_mut(), pkg.as_mut()) {
                            (Some((l, _, b)), _) => {
                                *b = i;
                                l.other_ids.push(format!("SSCC {v}"));
                            }
                            (None, Some(p)) => {
                                if p.sscc.len() < 100 {
                                    p.sscc.push(v)
                                }
                            }
                            _ => {}
                        }
                    } else if let Some((l, _, b)) = line.as_mut() {
                        *b = i;
                        l.other_ids.push(format!("{q} {v}"));
                    }
                }
            }
            "LIN" => {
                section = Section::Detail;
                line_count += 1;
                let id = doc.comp(s, 3, 1);
                let ty = doc.comp(s, 3, 2);
                if is_gs1_item_type(&ty) {
                    ctx.gs1
                        .add("GTIN", &id, &format!("LIN {}", doc.el(s, 1)), doc.src(s));
                }
                let l = Line {
                    no: doc.el(s, 1),
                    id,
                    id_type: ty,
                    cps: pkg.as_ref().map(|p| p.id.clone()).unwrap_or_default(),
                    ..Line::default()
                };
                line = Some((l, i, i));
            }
            "PIA" => {
                if let Some((l, _, b)) = line.as_mut() {
                    *b = i;
                    for k in 2..=doc.count(s).min(6) {
                        let id = doc.comp(s, k, 1);
                        let ty = doc.comp(s, k, 2);
                        if id.is_empty() {
                            continue;
                        }
                        if is_gs1_item_type(&ty) {
                            ctx.gs1.add("GTIN", &id, "PIA", doc.src(s));
                        }
                        if l.other_ids.len() < 20 {
                            l.other_ids.push(format!("{ty} {id}").trim().to_owned());
                        }
                    }
                }
            }
            "IMD" => {
                if let Some((l, _, b)) = line.as_mut() {
                    *b = i;
                    let c = doc.comps(s, 3);
                    let text: Vec<&String> = c.iter().skip(3).filter(|x| !x.is_empty()).collect();
                    let text = text.into_iter().cloned().collect::<Vec<_>>().join(" ");
                    if !text.is_empty() {
                        if !l.description.is_empty() {
                            l.description.push_str("; ");
                        }
                        l.description.push_str(&text);
                    }
                }
            }
            "QTY" => {
                if let Some((l, _, b)) = line.as_mut() {
                    *b = i;
                    let q = doc.comp(s, 1, 1);
                    let v = doc.comp(s, 1, 2);
                    let u = doc.comp(s, 1, 3);
                    if q == kind.quantity() || (l.qty.is_empty() && l.other_qty.is_empty()) {
                        if !l.qty.is_empty() {
                            l.other_qty.push(l.qty.clone());
                        }
                        l.qty = v;
                        l.unit = u;
                    } else if l.other_qty.len() < 10 {
                        let n = quantity_name(&q).unwrap_or("quantity");
                        l.other_qty
                            .push(format!("{n} ({q}): {v} {u}").trim().to_owned());
                    }
                }
            }
            "PRI" => {
                if let Some((l, _, b)) = line.as_mut() {
                    *b = i;
                    let q = doc.comp(s, 1, 1);
                    let v = doc.comp(s, 1, 2);
                    if l.price.is_empty() || q == "AAA" {
                        l.price = if q.is_empty() {
                            v
                        } else {
                            format!("{v} ({q})")
                        };
                    }
                }
            }
            "TAX" => {
                let rate = doc.comp(s, 5, 4);
                if let Some((l, _, b)) = line.as_mut() {
                    *b = i;
                    l.tax = rate;
                } else if section == Section::Summary || section == Section::Header {
                    tax_row = Some((
                        vec![
                            doc.comp(s, 2, 1),
                            doc.el(s, 6),
                            rate,
                            String::new(),
                            String::new(),
                        ],
                        i,
                        i,
                    ));
                }
            }
            "MOA" => {
                let q = doc.comp(s, 1, 1);
                let v = doc.comp(s, 1, 2);
                let cur = doc.comp(s, 1, 3);
                if let Some((row, _, b)) = tax_row.as_mut() {
                    *b = i;
                    match q.as_str() {
                        "125" => row[3] = v.clone(),
                        "124" | "150" => {
                            row[4] = v.clone();
                            tax_amounts.push(ctx.number(&v));
                        }
                        _ => {}
                    }
                } else if let Some((l, _, b)) = line.as_mut() {
                    *b = i;
                    if q == "203" || (q == "66" && l.amount.is_none()) {
                        l.amount = Some(v);
                    }
                    continue;
                }
                if section == Section::Summary || (section == Section::Header && line.is_none()) {
                    let label = match amount_name(&q) {
                        Some(n) => format!("{n} ({q})"),
                        None => format!("Amount ({q})"),
                    };
                    let shown = if cur.is_empty() {
                        v.clone()
                    } else {
                        format!("{v} {cur}")
                    };
                    ctx.fact("Totals", &label, shown, s);
                    match q.as_str() {
                        "79" => total_79 = Some(ctx.number(&v)),
                        "176" => total_176 = Some(ctx.number(&v)),
                        _ => {}
                    }
                }
            }
            "UNS" => {
                section = Section::Summary;
            }
            "CNT" => {
                let q = doc.comp(s, 1, 1);
                let v = doc.comp(s, 1, 2);
                if q == "2" {
                    declared_lines = Some((v, i));
                } else {
                    ctx.fact("Totals", &format!("Control total ({q})"), v, s);
                }
            }
            _ => {
                if let Some((_, _, b)) = line.as_mut() {
                    *b = i;
                }
            }
        }
        if tag == "RFF" {
            last_rff = reference_name(&doc.comp(s, 1, 1))
                .map(str::to_owned)
                .or_else(|| Some(format!("Reference ({})", doc.comp(s, 1, 1))));
        } else if tag != "DTM" {
            last_rff = None;
        }
    }
    flush_line(
        ctx,
        &mut line,
        &mut pkg,
        &mut line_amounts,
        &mut all_lines_have_amount,
    );
    flush_pkg(ctx, &mut pkg);
    if let Some((mut row, a, b)) = tax_row.take() {
        row.push(ctx.msg.clone());
        let src = doc.span(&segs[a], &segs[b]);
        ctx.taxes.push(row, src);
    }
    if ctx.details {
        let g = format!("{group}{}", ctx.suffix);
        ctx.out.fact(&g, "Line items", line_count.to_string(), None);
    }
    if let Some((declared, at)) = declared_lines {
        let ctx_name = format!("message {}", ctx.msg);
        let t = ctx
            .checks
            .line_counts
            .get_or_insert_with(|| Tally::new("Line item count (CNT 2)"));
        t.compare(&ctx_name, &declared, &line_count.to_string());
        if ctx.details {
            let g = format!("Totals{}", ctx.suffix);
            ctx.out.fact(
                &g,
                "Number of line items (CNT 2)",
                declared,
                doc.src(&segs[at]),
            );
        }
    }
    if kind == Kind::Invoice {
        if let Some(t) = total_79
            && all_lines_have_amount
            && line_count > 0
        {
            ctx.checks.line_sums.push((
                format!(
                    "Total line items amount (MOA 79) = sum of line amounts (MOA 203), message {}",
                    ctx.msg
                ),
                Some(t),
                decimal_sum(line_amounts.iter().map(String::as_str)),
            ));
        }
        if let Some(t) = total_176
            && !tax_amounts.is_empty()
        {
            ctx.checks.line_sums.push((
                format!(
                    "Total tax (MOA 176) = sum of tax amounts in the tax summary, message {}",
                    ctx.msg
                ),
                Some(t),
                decimal_sum(tax_amounts.iter().map(String::as_str)),
            ));
        }
    }
}
