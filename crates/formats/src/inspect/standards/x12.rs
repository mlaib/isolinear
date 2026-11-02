//! ANSI ASC X12 interchanges, recognized by the fixed-width `ISA` header.
//!
//! The ISA gives the delimiters: the element separator is its fourth byte, the component
//! separator is ISA16 and the segment terminator the byte after it (ISA11 is the repetition
//! separator from version 00402). ISA/GS/ST envelopes are read with their control numbers and
//! the version (GS08, ST03), and the SE/GE/IEA counts and control numbers compared. Views:
//! 810 invoice, 850 purchase order, 856 ship notice (HL hierarchy), 837 claims, 834 enrollment,
//! 835 remittance, 270/271 eligibility. Other transaction sets show their identity and
//! segments. Nothing is checked against the X12 standard or the HIPAA implementation guides.
use super::edi::{
    Doc, Gs1, Level, Rows, Seg, Seps, Tally, coded, content_start, date, hierarchy, negate,
    segments_view,
};
use crate::inspect::{
    Handler, Input, Inspection, Probe, Stage, State,
    spec::{compare_total, decimal_eq, decimal_sum},
};

pub struct X12;

/// Separators from the ISA header at the start of `h`.
fn isa(h: &[u8]) -> Option<Seps> {
    if !h.starts_with(b"ISA") {
        return None;
    }
    let elem = *h.get(3)?;
    if elem.is_ascii_alphanumeric() || elem.is_ascii_whitespace() || !elem.is_ascii() {
        return None;
    }
    // The 16th element separator starts ISA16; ISA is about 106 bytes, allow some slack.
    let mut seen = 0;
    let mut at = None;
    for (i, &b) in h.iter().enumerate().take(200).skip(3) {
        if b == elem {
            seen += 1;
            if seen == 16 {
                at = Some(i);
                break;
            }
        }
        if b == b'\n' && seen < 16 {
            return None;
        }
    }
    let at = at?;
    let comp = *h.get(at + 1)?;
    let term = *h.get(at + 2)?;
    if comp == elem || term == elem || comp == term || comp.is_ascii_alphanumeric() {
        return None;
    }
    if term.is_ascii_alphanumeric() || !term.is_ascii() {
        return None;
    }
    // ISA11 is the repetition separator from 00402 on; before, the standards identifier "U".
    let fields: Vec<&[u8]> = h[..at].split(|&b| b == elem).collect();
    let rep = fields.get(11).and_then(|f| {
        let version = fields.get(12).copied().unwrap_or(&[]);
        (f.len() == 1
            && !f[0].is_ascii_alphanumeric()
            && f[0] != comp
            && f[0] != term
            && version >= b"00402".as_slice())
        .then(|| f[0])
    });
    Some(Seps {
        elem,
        comp,
        rep,
        rel: None,
        term,
    })
}

impl Handler for X12 {
    fn name(&self) -> &'static str {
        "ANSI X12"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        isa(&probe.head[content_start(probe.head)..]).is_some()
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let from = content_start(input.bytes);
        let seps = isa(&input.bytes[from..]).ok_or("No usable ISA interchange header")?;
        let doc = Doc::split(input.bytes, from, seps, cancelled)?;
        if doc.segs.is_empty() {
            return Err("No X12 segments".into());
        }
        Ok(inspect_doc(&doc))
    }
}

// ---------------------------------------------------------------------------------------------
// Names

fn segment_name(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "ISA" => "Interchange control header",
        "IEA" => "Interchange control trailer",
        "GS" => "Functional group header",
        "GE" => "Functional group trailer",
        "ST" => "Transaction set header",
        "SE" => "Transaction set trailer",
        "TA1" => "Interchange acknowledgment",
        "BIG" => "Beginning segment for invoice",
        "BEG" => "Beginning segment for purchase order",
        "BSN" => "Beginning segment for ship notice",
        "BHT" => "Beginning of hierarchical transaction",
        "BGN" => "Beginning segment",
        "BPR" => "Financial information",
        "TRN" => "Trace",
        "N1" => "Party identification",
        "N2" => "Additional name information",
        "N3" => "Party location",
        "N4" => "Geographic location",
        "NM1" => "Individual or organizational name",
        "REF" => "Reference identification",
        "PER" => "Administrative communications contact",
        "DTM" => "Date/time reference",
        "DTP" => "Date or time period",
        "IT1" => "Baseline item data (invoice)",
        "PO1" => "Baseline item data",
        "PID" => "Product/item description",
        "TDS" => "Total monetary value summary",
        "CAD" => "Carrier detail",
        "ITD" => "Terms of sale",
        "CTT" => "Transaction totals",
        "AMT" => "Monetary amount",
        "SAC" => "Service, promotion, allowance or charge",
        "TXI" => "Tax information",
        "CUR" => "Currency",
        "FOB" => "F.O.B. related instructions",
        "HL" => "Hierarchical level",
        "LIN" => "Item identification",
        "SN1" => "Item detail (shipment)",
        "PRF" => "Purchase order reference",
        "TD1" => "Carrier details (quantity and weight)",
        "TD3" => "Carrier details (equipment)",
        "TD4" => "Carrier details (special handling)",
        "TD5" => "Carrier details (routing)",
        "MAN" => "Marks and numbers",
        "PKG" => "Marking, packaging, loading",
        "PO4" => "Item physical details",
        "MEA" => "Measurements",
        "CLM" => "Claim information",
        "HI" => "Health care information codes",
        "LX" => "Transaction set line number",
        "SV1" => "Professional service",
        "SV2" => "Institutional service line",
        "SV3" => "Dental service",
        "SBR" => "Subscriber information",
        "PAT" => "Patient information",
        "DMG" => "Demographic information",
        "PRV" => "Provider information",
        "CL1" => "Claim codes",
        "HCP" => "Health care pricing",
        "NTE" => "Note/special instruction",
        "PWK" => "Paperwork",
        "K3" => "File information",
        "INS" => "Member level detail",
        "HD" => "Health coverage",
        "COB" => "Coordination of benefits",
        "LS" => "Loop header",
        "LE" => "Loop trailer",
        "ACT" => "Account identification",
        "QTY" => "Quantity information",
        "CLP" => "Claim payment information",
        "SVC" => "Service payment information",
        "CAS" => "Claims adjustment",
        "PLB" => "Provider level adjustment",
        "LQ" => "Industry code identification",
        "MIA" => "Inpatient adjudication information",
        "MOA" => "Outpatient adjudication information",
        "EQ" => "Eligibility or benefit inquiry",
        "EB" => "Eligibility or benefit information",
        "AAA" => "Request validation",
        "MSG" => "Message text",
        "III" => "Information",
        "INV" => "Inventory detail",
        "AK1" => "Functional group response header",
        "AK2" => "Transaction set response header",
        "AK9" => "Functional group response trailer",
        "IK5" => "Implementation transaction set response trailer",
        "N9" => "Extended reference information",
        "SLN" => "Subline item detail",
        "ENT" => "Entity",
        _ => return None,
    })
}

fn set_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "810" => "invoice",
        "850" => "purchase order",
        "855" => "purchase order acknowledgment",
        "856" => "ship notice/manifest",
        "860" => "purchase order change request",
        "820" => "payment order/remittance advice",
        "824" => "application advice",
        "830" => "planning schedule",
        "846" => "inventory inquiry/advice",
        "852" => "product activity data",
        "862" => "shipping schedule",
        "864" => "text message",
        "875" => "grocery products purchase order",
        "880" => "grocery products invoice",
        "940" => "warehouse shipping order",
        "945" => "warehouse shipping advice",
        "204" => "motor carrier load tender",
        "210" => "motor carrier freight details and invoice",
        "214" => "transportation carrier shipment status",
        "997" => "functional acknowledgment",
        "999" => "implementation acknowledgment",
        "837" => "health care claim",
        "834" => "benefit enrollment and maintenance",
        "835" => "health care claim payment/advice",
        "270" => "eligibility, coverage or benefit inquiry",
        "271" => "eligibility, coverage or benefit information",
        "276" => "health care claim status request",
        "277" => "health care claim status notification",
        "278" => "health care services review",
        _ => return None,
    })
}

/// HIPAA and other implementation guides from GS08/ST03.
fn guide_name(v: &str) -> Option<&'static str> {
    let core = v.get(6..).unwrap_or("");
    Some(match core {
        "X222A1" | "X222" | "X098A1" | "X098" => "837 Professional",
        "X223A1" | "X223A2" | "X223A3" | "X223" | "X096A1" | "X096" => "837 Institutional",
        "X224A1" | "X224A2" | "X224A3" | "X224" | "X097A1" | "X097" => "837 Dental",
        "X220A1" | "X220" | "X095A1" | "X095" => "834 Benefit enrollment and maintenance",
        "X221A1" | "X221" | "X091A1" | "X091" => "835 Health care claim payment/advice",
        "X279A1" | "X279" | "X092A1" | "X092" => "270/271 Eligibility benefit inquiry and response",
        "X212" | "X093A1" | "X093" => "276/277 Claim status request and response",
        "X217" | "X094A1" | "X094" => "278 Services review",
        "X218" | "X061A1" | "X061" => "820 Payroll deducted and other group premium payment",
        "X231A1" | "X231" => "999 Implementation acknowledgment",
        "X214" => "277CA Claim acknowledgment",
        _ => return None,
    })
}

fn functional_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "IN" => "invoice",
        "PO" => "purchase order",
        "PR" => "purchase order acknowledgment",
        "SH" => "ship notice/manifest",
        "HC" => "health care claim",
        "BE" => "benefit enrollment and maintenance",
        "HP" => "health care claim payment/advice",
        "HS" => "eligibility inquiry",
        "HB" => "eligibility response",
        "HR" => "claim status request",
        "HN" => "claim status notification",
        "FA" => "functional acknowledgment",
        "RA" => "payment order/remittance advice",
        "QM" => "transportation status",
        "SM" => "motor carrier load tender",
        "IM" => "freight invoice",
        _ => return None,
    })
}

fn entity_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "BT" => "Bill-to party",
        "ST" => "Ship to",
        "SE" => "Selling party",
        "RE" => "Remit-to party",
        "BY" => "Buying party",
        "SF" => "Ship from",
        "VN" => "Vendor",
        "SU" => "Supplier",
        "PR" => "Payer",
        "PE" => "Payee",
        "SO" => "Sold to",
        "MF" => "Manufacturer",
        "CA" => "Carrier",
        "41" => "Submitter",
        "40" => "Receiver",
        "85" => "Billing provider",
        "87" => "Pay-to provider",
        "IL" => "Subscriber",
        "QC" => "Patient",
        "82" => "Rendering provider",
        "77" => "Service facility",
        "DN" => "Referring provider",
        "DQ" => "Supervising provider",
        "71" => "Attending provider",
        "72" => "Operating provider",
        "DK" => "Ordering provider",
        "P5" => "Plan sponsor",
        "IN" => "Insurer",
        "BO" => "Broker",
        "TV" => "Third party administrator",
        "1P" => "Provider",
        "FA" => "Facility",
        "03" => "Dependent",
        "2B" => "Third-party administrator",
        "36" => "Employer",
        "GP" => "Gateway provider",
        _ => return None,
    })
}

fn id_qualifier(code: &str) -> Option<&'static str> {
    Some(match code {
        "XX" => "NPI",
        "MI" => "member ID",
        "PI" => "payer ID",
        "46" => "ETIN",
        "24" => "EIN",
        "34" => "SSN",
        "FI" => "federal tax ID",
        "92" => "assigned by buyer",
        "91" => "assigned by seller",
        "1" => "DUNS",
        "9" => "DUNS+4",
        "UL" => "GLN",
        "ZZ" => "mutually defined",
        _ => return None,
    })
}

fn level_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "S" => "Shipment",
        "O" => "Order",
        "T" => "Tare",
        "P" => "Pack",
        "I" => "Item",
        "F" => "Component",
        "Q" => "Subline",
        "20" => "Information source / billing provider",
        "21" => "Information receiver",
        "22" => "Subscriber",
        "23" => "Dependent / patient",
        _ => return None,
    })
}

fn date_name(q: &str) -> Option<&'static str> {
    Some(match q {
        "002" => "Delivery requested",
        "003" => "Invoice date",
        "004" => "Purchase order date",
        "010" => "Requested ship",
        "011" => "Shipped",
        "017" => "Estimated delivery",
        "037" => "Ship not before",
        "038" => "Ship no later",
        "067" => "Current schedule delivery",
        "405" => "Production date",
        "472" => "Service date",
        "431" => "Onset of current illness",
        "434" => "Statement period",
        "435" => "Admission",
        "096" => "Discharge",
        "232" => "Claim statement period start",
        "233" => "Claim statement period end",
        "007" => "Effective",
        "303" => "Maintenance effective",
        "348" => "Benefit begin",
        "349" => "Benefit end",
        "356" => "Eligibility begin",
        "357" => "Eligibility end",
        "336" => "Employment begin",
        "291" => "Plan",
        "307" => "Eligibility",
        "102" => "Issue",
        _ => return None,
    })
}

/// Product ID qualifiers whose IDs are GS1 keys (UPC, EAN, GTIN-14, SSCC).
fn gs1_product(q: &str) -> Option<&'static str> {
    Some(match q {
        "UP" | "UA" | "UK" | "EN" | "UI" | "EO" => "GTIN",
        _ => return None,
    })
}

/// Product/service ID pairs (qualifier, ID) from element `from` on, as "UP 012345678905".
fn id_pairs(doc: &Doc, s: &Seg, from: usize) -> Vec<(String, String)> {
    let mut v = vec![];
    let mut i = from;
    while i < doc.count(s) && v.len() < 15 {
        let (q, id) = (doc.el(s, i), doc.el(s, i + 1));
        if !id.is_empty() {
            v.push((q, id));
        }
        i += 2;
    }
    v
}
fn show_pairs(p: &[(String, String)]) -> String {
    p.iter()
        .map(|(q, id)| format!("{q} {id}").trim().to_owned())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A person or organisation from NM1: "SMITH, JOHN A" style kept as "JOHN A SMITH".
fn nm1_name(doc: &Doc, s: &Seg) -> String {
    let last = doc.el(s, 3);
    let parts: Vec<String> = [doc.el(s, 4), doc.el(s, 5), last, doc.el(s, 7)]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect();
    parts.join(" ")
}
fn nm1_id(doc: &Doc, s: &Seg) -> String {
    let (q, id) = (doc.el(s, 8), doc.el(s, 9));
    if id.is_empty() {
        String::new()
    } else if q.is_empty() {
        id
    } else {
        format!("{id} ({})", coded(&q, id_qualifier(&q)))
    }
}

/// "123456" with two implied decimals (X12 type N2) -> "123456 (1234.56)".
fn implied2(raw: &str) -> String {
    let r = raw.trim();
    let (neg, d) = match r.strip_prefix('-') {
        Some(d) => ("-", d),
        None => ("", r),
    };
    if d.is_empty() || !d.bytes().all(|b| b.is_ascii_digit()) {
        return raw.to_owned();
    }
    let d = format!("{d:0>3}");
    let (int, frac) = d.split_at(d.len() - 2);
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    format!("{r} ({neg}{int}.{frac}, two implied decimals)")
}

/// ISA09 YYMMDD and ISA10 HHMM, as written.
fn yymmdd(d: &str, t: &str) -> String {
    format!("{d} {t}").trim().to_owned()
}

// ---------------------------------------------------------------------------------------------
// Envelopes

struct Set {
    st: usize,
    se: Option<usize>,
    id: String,
    control: String,
    guide: String,
    group_version: String,
}

fn inspect_doc(doc: &Doc) -> Inspection {
    let mut out = Inspection::new("ANSI X12", "ANSI X12 interchange");
    let mut partial: Vec<String> = vec![];
    let mut sets: Vec<Set> = vec![];
    let mut se_count = Tally::new("SE segment count");
    let mut se_ref = Tally::new("SE control number matches ST02");
    let mut ge_count = Tally::new("GE transaction set count");
    let mut ge_ref = Tally::new("GE control number matches GS06");
    let mut iea_count = Tally::new("IEA functional group count");
    let mut iea_ref = Tally::new("IEA control number matches ISA13");
    let mut isa: Option<usize> = None;
    let mut gs: Option<usize> = None;
    let mut open: Option<usize> = None;
    let mut groups = 0usize;
    let mut sets_in_group = 0usize;
    let mut interchanges = 0usize;
    let mut outside = 0usize;
    let mut odd = 0usize;
    let mut version = String::new();
    for (i, s) in doc.segs.iter().enumerate() {
        let tag = doc.tag(s);
        if !(2..=3).contains(&tag.len())
            || !tag
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            odd += 1;
        }
        match tag {
            "ISA" => {
                if isa.is_some() {
                    partial.push("an interchange has no IEA trailer".into());
                }
                isa = Some(i);
                interchanges += 1;
                groups = 0;
            }
            "GS" => {
                if gs.is_some() {
                    partial.push("a functional group has no GE trailer".into());
                }
                gs = Some(i);
                groups += 1;
                sets_in_group = 0;
                version = doc.el(s, 8);
            }
            "ST" => {
                if let Some(t) = open.take() {
                    se_count.fail(format!(
                        "transaction set {} has no SE trailer",
                        sets[t].control
                    ));
                }
                sets.push(Set {
                    st: i,
                    se: None,
                    id: doc.el(s, 1),
                    control: doc.el(s, 2),
                    guide: doc.el(s, 3),
                    group_version: version.clone(),
                });
                open = Some(sets.len() - 1);
                sets_in_group += 1;
            }
            "SE" => match open.take() {
                Some(t) => {
                    let set = &mut sets[t];
                    set.se = Some(i);
                    let ctx = format!("transaction set {}", set.control);
                    se_count.compare(&ctx, &doc.el(s, 1), &(i - set.st + 1).to_string());
                    se_ref.compare(&ctx, &doc.el(s, 2), &set.control);
                }
                None => outside += 1,
            },
            "GE" => match gs.take() {
                Some(g) => {
                    let gref = doc.el(&doc.segs[g], 6);
                    let ctx = format!("group {gref}");
                    ge_count.compare(&ctx, &doc.el(s, 1), &sets_in_group.to_string());
                    ge_ref.compare(&ctx, &doc.el(s, 2), &gref);
                }
                None => outside += 1,
            },
            "IEA" => match isa.take() {
                Some(x) => {
                    let xref = doc.el(&doc.segs[x], 13);
                    let ctx = format!("interchange {xref}");
                    iea_count.compare(&ctx, &doc.el(s, 1), &groups.to_string());
                    iea_ref.compare(&ctx, &doc.el(s, 2), &xref);
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
    if let Some(t) = open {
        se_count.fail(format!(
            "transaction set {} has no SE trailer",
            sets[t].control
        ));
        partial.push(format!(
            "transaction set {} ends without SE",
            sets[t].control
        ));
    }
    if gs.is_some() {
        partial.push("the functional group ends without GE".into());
    }
    if isa.is_some() {
        partial.push("the interchange ends without IEA".into());
    }
    if outside > 0 {
        partial.push(format!(
            "{outside} segments outside any transaction set or envelope"
        ));
    }
    if odd > 0 {
        partial.push(format!("{odd} segments without a valid tag"));
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
    let mut ids: Vec<&str> = vec![];
    for t in &sets {
        if !ids.contains(&t.id.as_str()) && ids.len() < 20 {
            ids.push(&t.id);
        }
    }
    let guide_of = |t: &Set| {
        if !t.guide.is_empty() {
            t.guide.clone()
        } else {
            t.group_version.clone()
        }
    };
    out.kind = match ids.as_slice() {
        [] => "ANSI X12 interchange (no transaction sets)".into(),
        [id] => {
            let first = &sets[0];
            let g = guide_of(first);
            let mut k = match set_name(id) {
                Some(n) => format!("{id} — {n}"),
                None => format!("{id} — X12 transaction set"),
            };
            if *id == "837" {
                let variant = guide_name(&g)
                    .and_then(|n| n.strip_prefix("837 "))
                    .map(str::to_owned)
                    .or_else(|| claim_variant(doc, first));
                if let Some(v) = variant {
                    k.push_str(&format!(" ({v})"));
                }
            }
            k
        }
        many => format!(
            "ANSI X12 interchange — {} transaction sets ({})",
            sets.len(),
            many.join(", ")
        ),
    };
    if let Some(t) = sets.first() {
        let g = guide_of(t);
        out.version = Some(match guide_name(&g) {
            Some(n) => format!("{g} ({n})"),
            None => g,
        })
        .filter(|v| !v.is_empty());
    }
    if out.version.is_none()
        && let Some(s) = doc.segs.first()
    {
        out.version = Some(doc.el(s, 12)).filter(|v| !v.is_empty());
    }

    // Envelope facts.
    out.fact(
        "Interchange",
        "Separators",
        format!(
            "element {}  component {}  repetition {}  terminator {}",
            doc.seps.elem as char,
            doc.seps.comp as char,
            doc.seps.rep.map(|b| b as char).unwrap_or('—'),
            match doc.seps.term {
                b'\n' => "line feed".to_owned(),
                b'\r' => "carriage return".to_owned(),
                t => (t as char).to_string(),
            }
        ),
        None,
    );
    if let Some(s) = doc.segs.iter().find(|s| doc.tag(s) == "ISA") {
        let src = doc.src(s);
        let f = |i| doc.el(s, i).trim().to_owned();
        out.fact(
            "Interchange",
            "Sender",
            format!("{} (qualifier {})", f(6), f(5)),
            src.clone(),
        );
        out.fact(
            "Interchange",
            "Receiver",
            format!("{} (qualifier {})", f(8), f(7)),
            src.clone(),
        );
        out.fact(
            "Interchange",
            "Date/time",
            yymmdd(&f(9), &f(10)),
            src.clone(),
        );
        out.fact("Interchange", "Version", f(12), src.clone());
        out.fact("Interchange", "Control number", f(13), src.clone());
        out.fact(
            "Interchange",
            "Acknowledgment requested",
            f(14),
            src.clone(),
        );
        let usage = f(15);
        let u = match usage.as_str() {
            "P" => "P (production)",
            "T" => "T (test)",
            "I" => "I (information)",
            _ => usage.as_str(),
        };
        out.fact("Interchange", "Usage", u.to_owned(), src);
    }
    if interchanges > 1 {
        out.fact(
            "Interchange",
            "Interchanges in file",
            interchanges.to_string(),
            None,
        );
    }
    if let Some(s) = doc.segs.iter().find(|s| doc.tag(s) == "GS") {
        let src = doc.src(s);
        let code = doc.el(s, 1);
        out.fact(
            "Functional group",
            "Functional identifier",
            coded(&code, functional_name(&code)),
            src.clone(),
        );
        out.fact(
            "Functional group",
            "Application sender",
            doc.el(s, 2),
            src.clone(),
        );
        out.fact(
            "Functional group",
            "Application receiver",
            doc.el(s, 3),
            src.clone(),
        );
        out.fact(
            "Functional group",
            "Date/time",
            format!("{} {}", date(&doc.el(s, 4)), doc.el(s, 5))
                .trim()
                .to_owned(),
            src.clone(),
        );
        out.fact(
            "Functional group",
            "Control number",
            doc.el(s, 6),
            src.clone(),
        );
        let v = doc.el(s, 8);
        out.fact(
            "Functional group",
            "Version",
            coded(&v, guide_name(&v)),
            src,
        );
    }

    // Transaction sets.
    let multi = sets.len() > 1;
    let mut table = Rows::new(
        "Transaction sets",
        &[
            "Control number",
            "Set",
            "Name",
            "Implementation guide",
            "Segments",
            "SE declares",
        ],
    );
    let mut views = Views::new();
    let mut no_view: Vec<String> = vec![];
    for (k, t) in sets.iter().enumerate() {
        let end = t.se.unwrap_or(doc.segs.len() - 1).max(t.st);
        let segs = &doc.segs[t.st..=end];
        let (Some(first), Some(last)) = (segs.first(), segs.last()) else {
            continue;
        };
        let g = guide_of(t);
        table.push(
            vec![
                t.control.clone(),
                t.id.clone(),
                set_name(&t.id).unwrap_or("").to_owned(),
                coded(&g, guide_name(&g)),
                segs.len().to_string(),
                t.se.map(|e| doc.el(&doc.segs[e], 1)).unwrap_or_default(),
            ],
            doc.span(first, last),
        );
        let details = k < 1000;
        let suffix = if multi {
            format!(" ({})", t.control)
        } else {
            String::new()
        };
        if details {
            let grp = format!("Transaction set{suffix}");
            let src = doc.src(first);
            out.fact(
                &grp,
                "Transaction set",
                coded(&t.id, set_name(&t.id)),
                src.clone(),
            );
            out.fact(&grp, "Control number", t.control.clone(), src.clone());
            out.fact(&grp, "Implementation guide", coded(&g, guide_name(&g)), src);
        }
        let mut ctx = Ctx {
            doc,
            out: &mut out,
            suffix,
            set: t.control.clone(),
            details,
            v: &mut views,
        };
        match t.id.as_str() {
            "810" => order_like(&mut ctx, segs, "IT1"),
            "850" => order_like(&mut ctx, segs, "PO1"),
            "856" => ship_notice(&mut ctx, segs),
            "837" => claims(&mut ctx, segs),
            "834" => enrollment(&mut ctx, segs),
            "835" => remittance(&mut ctx, segs),
            "270" | "271" => eligibility(&mut ctx, segs),
            other => {
                if !no_view.contains(&other.to_owned()) && no_view.len() < 20 {
                    no_view.push(other.to_owned());
                }
            }
        }
    }
    if sets.len() > 1000 {
        out.notes.push(
            "Facts are listed for the first 1000 transaction sets; the tables cover all of them"
                .into(),
        );
    }

    if partial.is_empty() {
        out.check(
            Stage::Parsed,
            State::Passed,
            format!(
                "{} segments read in {} transaction sets",
                doc.segs.len(),
                sets.len()
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
            format!("No business view for transaction set {t}: identity and segments only"),
        );
    }
    out.not_validated();
    for t in [se_count, se_ref, ge_count, ge_ref, iea_count, iea_ref] {
        t.emit(&mut out);
    }
    out.notes.push(
        "Envelopes, counts and control numbers are checked; segments are not validated against the X12 standard or the implementation guides"
            .into(),
    );
    out.notes.push(
        "Business views: 810, 850, 856 (HL hierarchy), 837, 834, 835, 270/271; other transaction sets show their identity and segments. Fixed-width ISA values are shown without their padding"
            .into(),
    );
    table.emit(&mut out);
    views.emit(&mut out);
    segments_view(
        doc,
        &mut out,
        &["ISA", "GS", "ST"],
        &["IEA", "GE", "SE"],
        segment_name,
    );
    out
}

/// 837 variant from the service lines when the guide is not given.
fn claim_variant(doc: &Doc, set: &Set) -> Option<String> {
    let end = set.se.unwrap_or(doc.segs.len().saturating_sub(1));
    doc.segs.get(set.st..=end)?.iter().find_map(|s| {
        match doc.tag(s) {
            "SV1" => Some("Professional"),
            "SV2" => Some("Institutional"),
            "SV3" => Some("Dental"),
            _ => None,
        }
        .map(str::to_owned)
    })
}

// ---------------------------------------------------------------------------------------------
// Views

struct Views {
    lines: Rows,
    levels: Vec<Level>,
    hl: Rows,
    claims: Rows,
    services: Rows,
    diagnoses: Rows,
    members: Rows,
    coverages: Rows,
    payments: Rows,
    adjustments: Rows,
    provider_adj: Rows,
    benefits: Rows,
    inquiries: Rows,
    gs1: Gs1,
    tallies: Vec<Tally>,
    sums: Vec<(String, Option<String>, Option<String>)>,
}
impl Views {
    fn new() -> Self {
        Self {
            lines: Rows::new(
                "Line items",
                &[
                    "Line",
                    "Product IDs",
                    "Description",
                    "Quantity",
                    "Unit",
                    "Unit price",
                    "Transaction set",
                ],
            ),
            levels: vec![],
            hl: Rows::new(
                "Hierarchy",
                &[
                    "HL",
                    "Parent",
                    "Level",
                    "Purchase order",
                    "Package marks",
                    "Product IDs",
                    "Quantity shipped",
                    "Unit",
                    "Transaction set",
                ],
            ),
            claims: Rows::new(
                "Claims",
                &[
                    "Claim ID",
                    "Patient",
                    "Subscriber",
                    "Member ID",
                    "Payer",
                    "Total charge",
                    "Facility/place of service",
                    "Frequency",
                    "Diagnoses",
                    "Service lines",
                    "Transaction set",
                ],
            ),
            services: Rows::new(
                "Service lines",
                &[
                    "Claim ID",
                    "Line",
                    "Procedure",
                    "Modifiers",
                    "Revenue code",
                    "Charge",
                    "Paid",
                    "Units",
                    "Service date",
                    "Diagnosis pointers",
                ],
            ),
            diagnoses: Rows::new(
                "Diagnosis codes",
                &["Claim ID", "Position", "Qualifier", "Code"],
            ),
            members: Rows::new(
                "Members",
                &[
                    "Subscriber",
                    "Relationship",
                    "Maintenance",
                    "Reason",
                    "Benefit status",
                    "Name",
                    "Member ID",
                    "Birth date",
                    "Gender",
                    "Coverages",
                    "Transaction set",
                ],
            ),
            coverages: Rows::new(
                "Coverages",
                &[
                    "Member",
                    "Maintenance",
                    "Insurance line",
                    "Plan",
                    "Coverage level",
                    "Begin",
                    "End",
                ],
            ),
            payments: Rows::new(
                "Claim payments",
                &[
                    "Claim ID",
                    "Status",
                    "Charge",
                    "Paid",
                    "Patient responsibility",
                    "Patient",
                    "Payer claim number",
                    "Filing indicator",
                    "Service lines",
                ],
            ),
            adjustments: Rows::new(
                "Adjustments",
                &[
                    "Claim ID",
                    "Level",
                    "Group",
                    "Reason code",
                    "Amount",
                    "Quantity",
                ],
            ),
            provider_adj: Rows::new(
                "Provider adjustments",
                &["Provider", "Fiscal period", "Reason", "Reference", "Amount"],
            ),
            benefits: Rows::new(
                "Benefits",
                &[
                    "Person",
                    "Information",
                    "Coverage level",
                    "Service types",
                    "Insurance type",
                    "Plan",
                    "Time period",
                    "Amount",
                    "Percent",
                    "Quantity",
                    "In network",
                ],
            ),
            inquiries: Rows::new(
                "Eligibility inquiries",
                &[
                    "Person",
                    "Level",
                    "Member ID",
                    "Birth date",
                    "Service types",
                    "Trace",
                ],
            ),
            gs1: Gs1::default(),
            tallies: vec![],
            sums: vec![],
        }
    }
    fn tally(&mut self, what: &str) -> &mut Tally {
        let i = match self.tallies.iter().position(|t| t.what() == what) {
            Some(i) => i,
            None => {
                self.tallies.push(Tally::new(what));
                self.tallies.len() - 1
            }
        };
        &mut self.tallies[i]
    }
    fn emit(self, out: &mut Inspection) {
        for t in self.tallies {
            t.emit(out);
        }
        for (what, d, c) in self.sums {
            compare_total(out, &what, d, c);
        }
        self.gs1.emit(out);
        hierarchy(out, "HL hierarchy", &self.levels);
        for r in [
            self.lines,
            self.hl,
            self.claims,
            self.services,
            self.diagnoses,
            self.members,
            self.coverages,
            self.payments,
            self.adjustments,
            self.provider_adj,
            self.inquiries,
            self.benefits,
        ] {
            r.emit(out);
        }
    }
}

struct Ctx<'a, 'd> {
    doc: &'a Doc<'d>,
    out: &'a mut Inspection,
    suffix: String,
    set: String,
    details: bool,
    v: &'a mut Views,
}
impl Ctx<'_, '_> {
    fn fact(&mut self, group: &str, label: &str, value: impl Into<String>, s: &Seg) {
        if self.details {
            let g = format!("{group}{}", self.suffix);
            self.out.fact(&g, label, value, self.doc.src(s));
        }
    }
    /// N1/NM1 party facts with the N2/N3/N4/REF/PER that follow (from `i`).
    fn party(&mut self, segs: &[Seg], i: usize) -> usize {
        let doc = self.doc;
        let s = &segs[i];
        let tag = doc.tag(s);
        let code = doc.el(s, 1);
        let group = format!("{} ({code})", entity_name(&code).unwrap_or("Party"));
        if tag == "N1" {
            self.fact(&group, "Name", doc.el(s, 2), s);
            let (q, id) = (doc.el(s, 3), doc.el(s, 4));
            if !id.is_empty() {
                self.fact(
                    &group,
                    "ID",
                    format!("{id} ({})", coded(&q, id_qualifier(&q))),
                    s,
                );
                if q == "UL" {
                    self.v
                        .gs1
                        .add("GLN", &id, &format!("N1*{code}"), doc.src(s));
                }
            }
        } else {
            self.fact(&group, "Name", nm1_name(doc, s), s);
            self.fact(&group, "ID", nm1_id(doc, s), s);
        }
        let mut j = i + 1;
        while let Some(n) = segs.get(j) {
            match doc.tag(n) {
                "N2" => self.fact(&group, "Additional name", doc.el(n, 1), n),
                "N3" => {
                    let a = [doc.el(n, 1), doc.el(n, 2)]
                        .into_iter()
                        .filter(|x| !x.is_empty())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.fact(&group, "Address", a, n);
                }
                "N4" => {
                    let a = [doc.el(n, 1), doc.el(n, 2), doc.el(n, 3), doc.el(n, 4)]
                        .into_iter()
                        .filter(|x| !x.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    self.fact(&group, "City/state/postal code", a, n);
                }
                "REF" => {
                    let q = doc.el(n, 1);
                    self.fact(&group, &format!("Reference {q}"), doc.el(n, 2), n);
                }
                "PER" => {
                    let c = [doc.el(n, 2), doc.el(n, 4)]
                        .into_iter()
                        .filter(|x| !x.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    self.fact(&group, "Contact", c, n);
                }
                _ => break,
            }
            j += 1;
        }
        j
    }
}

fn first_date(doc: &Doc, s: &Seg) -> String {
    // DTM: qualifier, date, time; DTP: qualifier, format, value.
    match doc.tag(s) {
        "DTP" => {
            let v = doc.el(s, 3);
            if doc.el(s, 2) == "RD8" && v.len() == 17 {
                v
            } else {
                date(&v)
            }
        }
        _ => {
            let t = doc.el(s, 3);
            let d = date(&doc.el(s, 2));
            if t.is_empty() { d } else { format!("{d} {t}") }
        }
    }
}

/// 810 invoice and 850 purchase order: header, parties, dates, lines and totals.
fn order_like(ctx: &mut Ctx, segs: &[Seg], line_tag: &str) {
    let doc = ctx.doc;
    let invoice = line_tag == "IT1";
    let group = if invoice { "Invoice" } else { "Purchase order" };
    let mut lines = 0usize;
    let mut current: Option<(Vec<String>, usize, usize)> = None;
    let mut declared: Option<String> = None;
    let mut i = 0;
    let flush = |ctx: &mut Ctx, cur: &mut Option<(Vec<String>, usize, usize)>| {
        if let Some((mut cells, a, b)) = cur.take() {
            cells.push(ctx.set.clone());
            let src = ctx.doc.span(&segs[a], &segs[b]);
            ctx.v.lines.push(cells, src);
        }
    };
    while i < segs.len() {
        let s = &segs[i];
        let tag = doc.tag(s);
        // A line ends at the next line or at the summary.
        if tag == line_tag || matches!(tag, "CTT" | "TDS" | "SE") {
            flush(ctx, &mut current);
        }
        match tag {
            "BIG" => {
                ctx.fact(group, "Invoice date", date(&doc.el(s, 1)), s);
                ctx.fact(group, "Invoice number", doc.el(s, 2), s);
                ctx.fact(group, "Purchase order date", date(&doc.el(s, 3)), s);
                ctx.fact(group, "Purchase order number", doc.el(s, 4), s);
                ctx.fact(group, "Transaction type", doc.el(s, 7), s);
            }
            "BEG" => {
                let purpose = doc.el(s, 1);
                let p = match purpose.as_str() {
                    "00" => Some("original"),
                    "01" => Some("cancellation"),
                    "05" => Some("replace"),
                    "06" => Some("confirmation"),
                    "07" => Some("duplicate"),
                    _ => None,
                };
                ctx.fact(group, "Purpose", coded(&purpose, p), s);
                let ty = doc.el(s, 2);
                let t = match ty.as_str() {
                    "SA" => Some("stand-alone order"),
                    "NE" => Some("new order"),
                    "BK" => Some("blanket order"),
                    "RL" => Some("release"),
                    "DS" => Some("drop ship"),
                    _ => None,
                };
                ctx.fact(group, "Order type", coded(&ty, t), s);
                ctx.fact(group, "Purchase order number", doc.el(s, 3), s);
                ctx.fact(group, "Release number", doc.el(s, 4), s);
                ctx.fact(group, "Order date", date(&doc.el(s, 5)), s);
            }
            "CUR" => ctx.fact(group, "Currency", doc.el(s, 2), s),
            "REF" if current.is_none() => {
                let q = doc.el(s, 1);
                ctx.fact(group, &format!("Reference {q}"), doc.el(s, 2), s);
            }
            "DTM" if current.is_none() => {
                let q = doc.el(s, 1);
                let label = date_name(&q)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Date ({q})"));
                ctx.fact(group, &label, first_date(doc, s), s);
            }
            "ITD" => {
                let net = doc.el(s, 7);
                if !net.is_empty() {
                    ctx.fact(group, "Terms net days", net, s);
                }
                ctx.fact(group, "Terms", doc.el(s, 12), s);
            }
            "N1" if current.is_none() => {
                let next = ctx.party(segs, i);
                i = next;
                continue;
            }
            "TDS" => {
                ctx.fact("Totals", "Total invoice amount", implied2(&doc.el(s, 1)), s);
            }
            "AMT" => {
                let q = doc.el(s, 1);
                let label = if q == "TT" {
                    "Total transaction amount (AMT TT)".to_owned()
                } else {
                    format!("Amount ({q})")
                };
                ctx.fact("Totals", &label, doc.el(s, 2), s);
            }
            "TXI" if current.is_none() => {
                ctx.fact("Totals", &format!("Tax {}", doc.el(s, 1)), doc.el(s, 2), s);
            }
            "CTT" => {
                declared = Some(doc.el(s, 1));
                ctx.fact("Totals", "Number of line items (CTT01)", doc.el(s, 1), s);
            }
            t if t == line_tag => {
                lines += 1;
                let pairs = id_pairs(doc, s, 6);
                for (q, id) in &pairs {
                    if let Some(kind) = gs1_product(q) {
                        ctx.v
                            .gs1
                            .add(kind, id, &format!("{t} {}", doc.el(s, 1)), doc.src(s));
                    }
                }
                current = Some((
                    vec![
                        doc.el(s, 1),
                        show_pairs(&pairs),
                        String::new(),
                        doc.el(s, 2),
                        doc.el(s, 3),
                        doc.el(s, 4),
                    ],
                    i,
                    i,
                ));
            }
            "PID" => {
                if let Some((cells, _, b)) = current.as_mut() {
                    *b = i;
                    let d = doc.el(s, 5);
                    if !d.is_empty() {
                        if !cells[2].is_empty() {
                            cells[2].push_str("; ");
                        }
                        cells[2].push_str(&d);
                    }
                }
            }
            _ => {
                if let Some((_, _, b)) = current.as_mut() {
                    *b = i;
                }
            }
        }
        i += 1;
    }
    flush(ctx, &mut current);
    ctx.fact(group, "Line items", lines.to_string(), &segs[0]);
    if let Some(d) = declared {
        let set = format!("transaction set {}", ctx.set);
        ctx.v
            .tally(&format!("Line item count (CTT01 vs {line_tag})"))
            .compare(&set, &d, &lines.to_string());
    }
}

/// 856 ship notice: BSN, the HL hierarchy (shipment, order, tare, pack, item) with what each
/// level carries.
fn ship_notice(ctx: &mut Ctx, segs: &[Seg]) {
    let doc = ctx.doc;
    struct Hl {
        id: String,
        parent: String,
        level: String,
        po: String,
        marks: Vec<String>,
        ids: Vec<(String, String)>,
        qty: String,
        unit: String,
        start: usize,
        end: usize,
    }
    let mut hls: Vec<Hl> = vec![];
    let mut parents = Tally::new("HL parent references an earlier level");
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (i, s) in segs.iter().enumerate() {
        let tag = doc.tag(s);
        if let Some(h) = hls.last_mut()
            && tag != "HL"
            && tag != "SE"
            && tag != "CTT"
        {
            h.end = i;
        }
        match tag {
            "BSN" => {
                ctx.fact("Shipment", "Shipment ID", doc.el(s, 2), s);
                ctx.fact("Shipment", "Date", date(&doc.el(s, 3)), s);
                ctx.fact("Shipment", "Time", doc.el(s, 4), s);
                let st = doc.el(s, 5);
                let n = match st.as_str() {
                    "0001" => Some("shipment, order, packaging, item"),
                    "0002" => Some("shipment, order, item, packaging"),
                    "0004" => Some("shipment, order, item"),
                    _ => None,
                };
                ctx.fact("Shipment", "Hierarchical structure", coded(&st, n), s);
            }
            "HL" => {
                let id = doc.el(s, 1);
                let parent = doc.el(s, 2);
                if !parent.is_empty() {
                    if seen.contains(parent.as_str()) {
                        parents.compare(&format!("HL {id}"), &parent, &parent);
                    } else {
                        parents.fail(format!("HL {id} refers to unknown parent {parent}"));
                    }
                }
                seen.insert(id.clone());
                if hls.len() < crate::inspect::MAX_ROWS {
                    hls.push(Hl {
                        id,
                        parent,
                        level: doc.el(s, 3),
                        po: String::new(),
                        marks: vec![],
                        ids: vec![],
                        qty: String::new(),
                        unit: String::new(),
                        start: i,
                        end: i,
                    });
                }
            }
            "PRF" => {
                if let Some(h) = hls.last_mut() {
                    h.po = doc.el(s, 1);
                }
            }
            "MAN" => {
                let (q, v) = (doc.el(s, 1), doc.el(s, 2));
                if q == "GM" {
                    // SSCC, sometimes written with its application identifier "00".
                    let sscc = if v.len() == 20 && v.starts_with("00") {
                        &v[2..]
                    } else {
                        &v
                    };
                    ctx.v.gs1.add("SSCC", sscc, "MAN*GM", doc.src(s));
                }
                if let Some(h) = hls.last_mut()
                    && h.marks.len() < 20
                {
                    h.marks.push(format!("{q} {v}"));
                }
            }
            "LIN" => {
                let pairs = id_pairs(doc, s, 2);
                for (q, id) in &pairs {
                    if let Some(kind) = gs1_product(q) {
                        ctx.v.gs1.add(kind, id, "LIN", doc.src(s));
                    }
                }
                if let Some(h) = hls.last_mut() {
                    h.ids.extend(pairs);
                }
            }
            "SN1" => {
                if let Some(h) = hls.last_mut() {
                    h.qty = doc.el(s, 2);
                    h.unit = doc.el(s, 3);
                }
            }
            "TD1" => {
                let w = [doc.el(s, 7), doc.el(s, 8)].join(" ").trim().to_owned();
                if !w.is_empty() {
                    ctx.fact("Shipment", "Weight", w, s);
                }
                let pk = [doc.el(s, 2), doc.el(s, 1)]
                    .join(" × ")
                    .trim_matches([' ', '×'])
                    .to_owned();
                ctx.fact("Shipment", "Packaging", pk, s);
            }
            "TD5" => {
                ctx.fact("Shipment", "Carrier", doc.el(s, 3), s);
                ctx.fact("Shipment", "Routing", doc.el(s, 5), s);
            }
            "REF" => {
                let q = doc.el(s, 1);
                let label = match q.as_str() {
                    "BM" => "Bill of lading".to_owned(),
                    "CN" => "Carrier PRO number".to_owned(),
                    _ => format!("Reference {q}"),
                };
                ctx.fact("Shipment", &label, doc.el(s, 2), s);
            }
            "DTM" => {
                let q = doc.el(s, 1);
                let label = date_name(&q)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Date ({q})"));
                ctx.fact("Shipment", &label, first_date(doc, s), s);
            }
            "N1" => {
                ctx.party(segs, i);
            }
            _ => {}
        }
    }
    parents.emit(ctx.out);
    // Rows and levels, with the order and package inherited from ancestors.
    let index: std::collections::HashMap<&str, usize> = hls
        .iter()
        .enumerate()
        .map(|(k, h)| (h.id.as_str(), k))
        .collect();
    let ancestor = |mut k: usize, f: &dyn Fn(&Hl) -> Option<String>| -> String {
        for _ in 0..64 {
            if let Some(v) = f(&hls[k]) {
                return v;
            }
            match index.get(hls[k].parent.as_str()) {
                Some(&p) if p != k => k = p,
                _ => break,
            }
        }
        String::new()
    };
    for (k, h) in hls.iter().enumerate() {
        let po = ancestor(k, &|h| (!h.po.is_empty()).then(|| h.po.clone()));
        let marks = ancestor(k, &|h| (!h.marks.is_empty()).then(|| h.marks.join(", ")));
        let src = ctx.doc.span(&segs[h.start], &segs[h.end]);
        let level = coded(&h.level, level_name(&h.level));
        let mut value = vec![];
        if !h.po.is_empty() {
            value.push(format!("PO {}", h.po));
        }
        if !h.marks.is_empty() {
            value.push(h.marks.join(", "));
        }
        if !h.ids.is_empty() {
            value.push(show_pairs(&h.ids));
        }
        if !h.qty.is_empty() {
            value.push(format!("× {} {}", h.qty, h.unit).trim().to_owned());
        }
        let label = match level_name(&h.level) {
            Some(n) => format!("HL {} — {n}", h.id),
            None => format!("HL {} — level {}", h.id, h.level),
        };
        let label = if ctx.suffix.is_empty() {
            label
        } else {
            format!("{label} (set {})", ctx.set)
        };
        ctx.v.levels.push(Level {
            id: format!("{}\u{1}{}", ctx.set, h.id),
            parent: if h.parent.is_empty() {
                String::new()
            } else {
                format!("{}\u{1}{}", ctx.set, h.parent)
            },
            label,
            value: value.join("; "),
            source: src.clone(),
        });
        ctx.v.hl.push(
            vec![
                h.id.clone(),
                h.parent.clone(),
                level,
                po,
                marks,
                show_pairs(&h.ids),
                h.qty.clone(),
                h.unit.clone(),
                ctx.set.clone(),
            ],
            src,
        );
    }
    let items = hls.iter().filter(|h| h.level == "I").count();
    let packs = hls
        .iter()
        .filter(|h| h.level == "P" || h.level == "T")
        .count();
    ctx.fact(
        "Shipment",
        "Hierarchical levels",
        hls.len().to_string(),
        &segs[0],
    );
    ctx.fact("Shipment", "Packs/tares", packs.to_string(), &segs[0]);
    ctx.fact("Shipment", "Items", items.to_string(), &segs[0]);
}

/// Claim being read from an 837.
struct Claim {
    id: String,
    charge: String,
    facility: String,
    frequency: String,
    diagnoses: Vec<String>,
    lines: Vec<String>,
    start: usize,
    end: usize,
    patient: String,
    subscriber: String,
    member: String,
    payer: String,
}

/// 837 claims: billing provider, subscriber, patient, claims, diagnoses and service lines.
fn claims(ctx: &mut Ctx, segs: &[Seg]) {
    let doc = ctx.doc;
    let mut subscriber = String::new();
    let mut member = String::new();
    let mut patient = String::new();
    let mut payer = String::new();
    let mut in_patient_level = false;
    let mut claim: Option<Claim> = None;
    let mut line: Option<(Vec<String>, usize, usize)> = None;
    let mut count = 0usize;
    let flush_line = |ctx: &mut Ctx, line: &mut Option<(Vec<String>, usize, usize)>| {
        if let Some((cells, a, b)) = line.take() {
            let src = ctx.doc.span(&segs[a], &segs[b]);
            ctx.v.services.push(cells, src);
        }
    };
    let flush_claim = |ctx: &mut Ctx, claim: &mut Option<Claim>| {
        if let Some(c) = claim.take() {
            let src = ctx.doc.span(&segs[c.start], &segs[c.end]);
            let what = "Claim total charge (CLM02) = sum of service line charges";
            if !c.lines.is_empty() {
                let sum = decimal_sum(c.lines.iter().map(String::as_str));
                let detail = format!(
                    "claim {}: declared {}, lines add up to {}",
                    c.id,
                    c.charge,
                    sum.clone().unwrap_or_else(|| "?".into())
                );
                let t = ctx.v.tally(what);
                match sum {
                    Some(s) if decimal_eq(&c.charge, &s) => t.pass(detail),
                    _ => t.fail(detail),
                }
            }
            ctx.v.claims.push(
                vec![
                    c.id,
                    c.patient,
                    c.subscriber,
                    c.member,
                    c.payer,
                    c.charge,
                    c.facility,
                    c.frequency,
                    c.diagnoses.join(", "),
                    c.lines.len().to_string(),
                    ctx.set.clone(),
                ],
                src,
            );
        }
    };
    let mut i = 0;
    while i < segs.len() {
        let s = &segs[i];
        let tag = doc.tag(s);
        if matches!(tag, "LX" | "CLM" | "HL" | "SE") {
            flush_line(ctx, &mut line);
        }
        if matches!(tag, "CLM" | "HL" | "SE") {
            flush_claim(ctx, &mut claim);
        }
        if let Some(c) = claim.as_mut() {
            c.end = i;
        }
        if let Some((_, _, b)) = line.as_mut() {
            *b = i;
        }
        match tag {
            "BHT" => {
                ctx.fact("Claim batch", "Reference", doc.el(s, 3), s);
                ctx.fact("Claim batch", "Created", date(&doc.el(s, 4)), s);
                let t = doc.el(s, 6);
                let n = match t.as_str() {
                    "CH" => Some("chargeable"),
                    "RP" => Some("reporting"),
                    "31" => Some("subrogation demand"),
                    _ => None,
                };
                ctx.fact("Claim batch", "Claim type", coded(&t, n), s);
            }
            "HL" => {
                in_patient_level = doc.el(s, 3) == "23";
                if doc.el(s, 3) == "22" {
                    subscriber.clear();
                    member.clear();
                    patient.clear();
                }
            }
            "SBR" => {
                let rel = doc.el(s, 2);
                ctx.fact("Subscriber", "Payer responsibility", doc.el(s, 1), s);
                ctx.fact("Subscriber", "Relationship to patient", rel, s);
                ctx.fact("Subscriber", "Group number", doc.el(s, 3), s);
                ctx.fact("Subscriber", "Claim filing indicator", doc.el(s, 9), s);
            }
            "NM1" => {
                let code = doc.el(s, 1);
                match code.as_str() {
                    "IL" => {
                        subscriber = nm1_name(doc, s);
                        member = doc.el(s, 9);
                        patient = subscriber.clone();
                    }
                    "QC" => patient = nm1_name(doc, s),
                    "PR" => payer = nm1_name(doc, s),
                    _ => {}
                }
                if claim.is_none() && line.is_none() {
                    let next = ctx.party(segs, i);
                    i = next;
                    continue;
                }
            }
            "DMG" if claim.is_none() => {
                let who = if in_patient_level {
                    "Patient (QC)"
                } else {
                    "Subscriber (IL)"
                };
                ctx.fact(who, "Birth date", date(&doc.el(s, 2)), s);
                ctx.fact(who, "Gender", doc.el(s, 3), s);
            }
            "CLM" => {
                count += 1;
                let c = doc.comps(s, 5);
                let facility = match (c.first(), c.get(1)) {
                    (Some(code), Some(q)) if q == "B" => format!("{code} (place of service)"),
                    (Some(code), Some(q)) if q == "A" => format!("{code} (facility type)"),
                    (Some(code), _) => code.clone(),
                    _ => String::new(),
                };
                let freq = c.get(2).cloned().unwrap_or_default();
                let f = match freq.as_str() {
                    "1" => Some("original"),
                    "7" => Some("replacement"),
                    "8" => Some("void"),
                    _ => None,
                };
                claim = Some(Claim {
                    id: doc.el(s, 1),
                    charge: doc.el(s, 2),
                    facility,
                    frequency: coded(&freq, f),
                    diagnoses: vec![],
                    lines: vec![],
                    start: i,
                    end: i,
                    patient: patient.clone(),
                    subscriber: subscriber.clone(),
                    member: member.clone(),
                    payer: payer.clone(),
                });
            }
            "HI" => {
                let cid = claim.as_ref().map(|c| c.id.clone()).unwrap_or_default();
                for k in 1..=doc.count(s).min(12) {
                    let c = doc.comps(s, k);
                    let (q, code) = (
                        c.first().cloned().unwrap_or_default(),
                        c.get(1).cloned().unwrap_or_default(),
                    );
                    if code.is_empty() {
                        continue;
                    }
                    let qn = match q.as_str() {
                        "ABK" => Some("principal diagnosis (ICD-10-CM)"),
                        "ABF" => Some("other diagnosis (ICD-10-CM)"),
                        "ABJ" => Some("admitting diagnosis (ICD-10-CM)"),
                        "APR" => Some("patient's reason for visit (ICD-10-CM)"),
                        "ABN" => Some("external cause of injury (ICD-10-CM)"),
                        "BK" => Some("principal diagnosis (ICD-9-CM)"),
                        "BF" => Some("other diagnosis (ICD-9-CM)"),
                        "BBR" => Some("principal procedure (ICD-10-PCS)"),
                        "BBQ" => Some("other procedure (ICD-10-PCS)"),
                        "BE" => Some("value code"),
                        "BG" => Some("condition code"),
                        "BH" => Some("occurrence code"),
                        "DR" => Some("DRG"),
                        _ => None,
                    };
                    if let Some(c) = claim.as_mut()
                        && matches!(q.as_str(), "ABK" | "ABF" | "ABJ" | "APR" | "BK" | "BF")
                        && c.diagnoses.len() < 24
                    {
                        c.diagnoses.push(code.clone());
                    }
                    ctx.v.diagnoses.push(
                        vec![cid.clone(), k.to_string(), coded(&q, qn), code],
                        doc.src(s),
                    );
                }
            }
            "LX" => {
                line = Some((
                    vec![
                        claim.as_ref().map(|c| c.id.clone()).unwrap_or_default(),
                        doc.el(s, 1),
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                    ],
                    i,
                    i,
                ));
            }
            "SV1" | "SV2" | "SV3" => {
                if line.is_none() {
                    line = Some((
                        vec![
                            claim.as_ref().map(|c| c.id.clone()).unwrap_or_default(),
                            String::new(),
                            String::new(),
                            String::new(),
                            String::new(),
                            String::new(),
                            String::new(),
                            String::new(),
                            String::new(),
                            String::new(),
                        ],
                        i,
                        i,
                    ));
                }
                let (proc_el, charge_el, units_el) = match tag {
                    "SV1" => (1, 2, 4),
                    "SV2" => (2, 3, 5),
                    _ => (1, 2, 6),
                };
                let p = doc.comps(s, proc_el);
                let procedure = match (p.first(), p.get(1)) {
                    (Some(q), Some(code)) if !code.is_empty() => format!("{q}:{code}"),
                    _ => String::new(),
                };
                let mods: Vec<String> = p
                    .iter()
                    .skip(2)
                    .take(4)
                    .filter(|m| !m.is_empty())
                    .cloned()
                    .collect();
                let charge = doc.el(s, charge_el);
                if let Some(c) = claim.as_mut()
                    && !charge.is_empty()
                {
                    c.lines.push(charge.clone());
                }
                if let Some((cells, _, _)) = line.as_mut() {
                    cells[2] = procedure;
                    cells[3] = mods.join(" ");
                    if tag == "SV2" {
                        cells[4] = doc.el(s, 1);
                    }
                    cells[5] = charge;
                    cells[7] = doc.el(s, units_el);
                    if tag == "SV1" {
                        cells[9] = doc
                            .comps(s, 7)
                            .into_iter()
                            .filter(|x| !x.is_empty())
                            .collect::<Vec<_>>()
                            .join(",");
                    }
                }
            }
            "DTP" => {
                let q = doc.el(s, 1);
                if let Some((cells, _, _)) = line.as_mut()
                    && q == "472"
                {
                    cells[8] = first_date(doc, s);
                } else if let Some(c) = claim.as_ref() {
                    let label = date_name(&q)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Date ({q})"));
                    let g = format!("Claim {}", c.id);
                    if count <= 50 {
                        ctx.fact(&g, &label, first_date(doc, s), s);
                    }
                }
            }
            "REF" if claim.is_some() && line.is_none() && count <= 50 => {
                let g = format!(
                    "Claim {}",
                    claim.as_ref().map(|c| c.id.as_str()).unwrap_or("")
                );
                let q = doc.el(s, 1);
                let label = match q.as_str() {
                    "G1" => "Prior authorization".to_owned(),
                    "D9" => "Claim identifier for transmission intermediaries".to_owned(),
                    "EA" => "Medical record number".to_owned(),
                    "F8" => "Payer claim control number".to_owned(),
                    _ => format!("Reference {q}"),
                };
                ctx.fact(&g, &label, doc.el(s, 2), s);
            }
            _ => {}
        }
        i += 1;
    }
    flush_line(ctx, &mut line);
    flush_claim(ctx, &mut claim);
    ctx.fact("Claim batch", "Claims", count.to_string(), &segs[0]);
}

/// 834 enrollment: sponsor, insurer, members and their coverages.
fn enrollment(ctx: &mut Ctx, segs: &[Seg]) {
    let doc = ctx.doc;
    struct Member {
        cells: Vec<String>,
        coverages: Vec<String>,
        start: usize,
        end: usize,
    }
    let mut member: Option<Member> = None;
    let mut coverage: Option<(Vec<String>, usize, usize)> = None;
    let mut count = 0usize;
    let flush_cov =
        |ctx: &mut Ctx, cov: &mut Option<(Vec<String>, usize, usize)>, m: &mut Option<Member>| {
            if let Some((cells, a, b)) = cov.take() {
                if let Some(m) = m.as_mut()
                    && m.coverages.len() < 20
                {
                    let mut d = format!("{} {} ({})", cells[2], cells[3], cells[4]);
                    if !cells[5].is_empty() {
                        d.push_str(&format!(" from {}", cells[5]));
                    }
                    m.coverages.push(d.replace("  ", " ").replace(" ()", ""));
                }
                let src = ctx.doc.span(&segs[a], &segs[b]);
                ctx.v.coverages.push(cells, src);
            }
        };
    let flush_member = |ctx: &mut Ctx, m: &mut Option<Member>| {
        if let Some(mut m) = m.take() {
            m.cells[9] = m.coverages.join("; ");
            m.cells.push(ctx.set.clone());
            let src = ctx.doc.span(&segs[m.start], &segs[m.end]);
            ctx.v.members.push(m.cells, src);
        }
    };
    let mut i = 0;
    while i < segs.len() {
        let s = &segs[i];
        let tag = doc.tag(s);
        if matches!(tag, "HD" | "INS" | "SE") {
            flush_cov(ctx, &mut coverage, &mut member);
        }
        if matches!(tag, "INS" | "SE") {
            flush_member(ctx, &mut member);
        }
        if let Some(m) = member.as_mut() {
            m.end = i;
        }
        if let Some((_, _, b)) = coverage.as_mut() {
            *b = i;
        }
        match tag {
            "BGN" => {
                let p = doc.el(s, 1);
                let pn = match p.as_str() {
                    "00" => Some("original"),
                    "15" => Some("re-submission"),
                    "22" => Some("information copy"),
                    _ => None,
                };
                ctx.fact("Enrollment", "Purpose", coded(&p, pn), s);
                ctx.fact("Enrollment", "Reference", doc.el(s, 2), s);
                ctx.fact("Enrollment", "Created", date(&doc.el(s, 3)), s);
                let a = doc.el(s, 8);
                let an = match a.as_str() {
                    "2" => Some("change (update)"),
                    "4" => Some("verify"),
                    "RX" => Some("replace"),
                    _ => None,
                };
                ctx.fact("Enrollment", "Action", coded(&a, an), s);
            }
            "REF" if member.is_none() => {
                let q = doc.el(s, 1);
                let label = if q == "38" {
                    "Master policy number".to_owned()
                } else {
                    format!("Reference {q}")
                };
                ctx.fact("Enrollment", &label, doc.el(s, 2), s);
            }
            "DTP" if member.is_none() => {
                let q = doc.el(s, 1);
                let label = date_name(&q)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Date ({q})"));
                ctx.fact("Enrollment", &label, first_date(doc, s), s);
            }
            "N1" if member.is_none() => {
                let next = ctx.party(segs, i);
                i = next;
                continue;
            }
            "INS" => {
                count += 1;
                let rel = doc.el(s, 2);
                let reln = match rel.as_str() {
                    "18" => Some("self"),
                    "01" => Some("spouse"),
                    "19" => Some("child"),
                    "15" => Some("ward"),
                    "53" => Some("life partner"),
                    "G8" => Some("other relationship"),
                    _ => None,
                };
                let mt = doc.el(s, 3);
                let mtn = match mt.as_str() {
                    "001" => Some("change"),
                    "021" => Some("addition"),
                    "024" => Some("cancellation or termination"),
                    "025" => Some("reinstatement"),
                    "030" => Some("audit or compare"),
                    _ => None,
                };
                let bs = doc.el(s, 5);
                let bsn = match bs.as_str() {
                    "A" => Some("active"),
                    "C" => Some("COBRA"),
                    "S" => Some("surviving insured"),
                    "T" => Some("tax equity and fiscal responsibility act"),
                    _ => None,
                };
                member = Some(Member {
                    cells: vec![
                        doc.el(s, 1),
                        coded(&rel, reln),
                        coded(&mt, mtn),
                        doc.el(s, 4),
                        coded(&bs, bsn),
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                    ],
                    coverages: vec![],
                    start: i,
                    end: i,
                });
            }
            "REF" => {
                if let Some(m) = member.as_mut()
                    && doc.el(s, 1) == "0F"
                {
                    m.cells[6] = doc.el(s, 2);
                }
            }
            "NM1" => {
                if let Some(m) = member.as_mut()
                    && matches!(doc.el(s, 1).as_str(), "IL" | "74")
                {
                    m.cells[5] = nm1_name(doc, s);
                    if m.cells[6].is_empty() {
                        m.cells[6] = doc.el(s, 9);
                    }
                }
            }
            "DMG" => {
                if let Some(m) = member.as_mut() {
                    m.cells[7] = date(&doc.el(s, 2));
                    m.cells[8] = doc.el(s, 3);
                }
            }
            "HD" => {
                let level = doc.el(s, 5);
                let ln = match level.as_str() {
                    "EMP" => Some("employee only"),
                    "ESP" => Some("employee and spouse"),
                    "ECH" => Some("employee and children"),
                    "FAM" => Some("family"),
                    "IND" => Some("individual"),
                    "CHD" => Some("children only"),
                    "E1D" => Some("employee and one dependent"),
                    "SPC" => Some("spouse and children"),
                    "SPO" => Some("spouse only"),
                    _ => None,
                };
                let line = doc.el(s, 3);
                let lnn = match line.as_str() {
                    "HLT" => Some("health"),
                    "DEN" => Some("dental"),
                    "VIS" => Some("vision"),
                    "HMO" => Some("HMO"),
                    "PPO" => Some("PPO"),
                    "LIF" => Some("life"),
                    "MM" => Some("major medical"),
                    "PDG" => Some("prescription drug"),
                    _ => None,
                };
                coverage = Some((
                    vec![
                        member
                            .as_ref()
                            .map(|m| m.cells[5].clone())
                            .unwrap_or_default(),
                        doc.el(s, 1),
                        coded(&line, lnn),
                        doc.el(s, 4),
                        coded(&level, ln),
                        String::new(),
                        String::new(),
                    ],
                    i,
                    i,
                ));
                if let Some((c, _, _)) = coverage.as_mut()
                    && c[0].is_empty()
                {
                    c[0] = format!("member {count}");
                }
            }
            "DTP" => {
                let q = doc.el(s, 1);
                if let Some((cells, _, _)) = coverage.as_mut() {
                    match q.as_str() {
                        "348" => cells[5] = first_date(doc, s),
                        "349" => cells[6] = first_date(doc, s),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    flush_cov(ctx, &mut coverage, &mut member);
    flush_member(ctx, &mut member);
    ctx.fact("Enrollment", "Members", count.to_string(), &segs[0]);
}

/// 835 remittance: payment, payer and payee, claim payments, service lines, adjustments and
/// the balancing of payments with adjustments.
fn remittance(ctx: &mut Ctx, segs: &[Seg]) {
    let doc = ctx.doc;
    let mut total: Option<(String, usize)> = None;
    let mut claim_paid: Vec<String> = vec![];
    let mut plb: Vec<String> = vec![];
    struct ClaimPay {
        cells: Vec<String>,
        paid: String,
        lines_paid: Vec<String>,
        start: usize,
        end: usize,
    }
    let mut claim: Option<ClaimPay> = None;
    // Service line: cells, charge, adjustments, paid, start, end.
    let mut svc: Option<(Vec<String>, Vec<String>, usize, usize)> = None;
    let mut count = 0usize;
    let flush_svc = |ctx: &mut Ctx, svc: &mut Option<(Vec<String>, Vec<String>, usize, usize)>| {
        if let Some((cells, adj, a, b)) = svc.take() {
            let (charge, paid) = (cells[5].clone(), cells[6].clone());
            let mut values = vec![charge.clone()];
            values.extend(adj.iter().map(|x| negate(x)));
            let computed = decimal_sum(values.iter().map(String::as_str));
            let t = ctx
                .v
                .tally("Service line balance (charge − adjustments = paid)");
            let ok = computed.as_deref().is_some_and(|c| decimal_eq(c, &paid));
            let detail = format!(
                "claim {} {}: charge {charge} − adjustments = {}, paid {paid}",
                cells[0],
                cells[2],
                computed.unwrap_or_else(|| "?".into())
            );
            if ok {
                t.pass(detail)
            } else {
                t.fail(detail)
            }
            let src = ctx.doc.span(&segs[a], &segs[b]);
            ctx.v.services.push(cells, src);
        }
    };
    let flush_claim = |ctx: &mut Ctx, claim: &mut Option<ClaimPay>| {
        if let Some(mut c) = claim.take() {
            if !c.lines_paid.is_empty() {
                let sum = decimal_sum(c.lines_paid.iter().map(String::as_str));
                let t = ctx
                    .v
                    .tally("Claim payment (CLP04) = sum of service line payments");
                let ok = sum.as_deref().is_some_and(|s| decimal_eq(s, &c.paid));
                let detail = format!(
                    "claim {}: paid {}, service lines {}",
                    c.cells[0],
                    c.paid,
                    sum.unwrap_or_else(|| "?".into())
                );
                if ok { t.pass(detail) } else { t.fail(detail) }
            }
            c.cells.push(c.lines_paid.len().to_string());
            let src = ctx.doc.span(&segs[c.start], &segs[c.end]);
            ctx.v.payments.push(c.cells, src);
        }
    };
    let mut i = 0;
    while i < segs.len() {
        let s = &segs[i];
        let tag = doc.tag(s);
        if matches!(tag, "SVC" | "CLP" | "LX" | "PLB" | "SE") {
            flush_svc(ctx, &mut svc);
        }
        if matches!(tag, "CLP" | "LX" | "PLB" | "SE") {
            flush_claim(ctx, &mut claim);
        }
        if let Some(c) = claim.as_mut() {
            c.end = i;
        }
        if let Some((_, _, _, b)) = svc.as_mut() {
            *b = i;
        }
        match tag {
            "BPR" => {
                let h = doc.el(s, 1);
                let hn = match h.as_str() {
                    "C" => Some("payment accompanies remittance advice"),
                    "D" => Some("make payment only"),
                    "H" => Some("notification only"),
                    "I" => Some("remittance information only"),
                    "P" => Some("prenotification of future transfers"),
                    "X" => Some("split payment and remittance"),
                    _ => None,
                };
                ctx.fact("Payment", "Transaction handling", coded(&h, hn), s);
                ctx.fact("Payment", "Total payment amount", doc.el(s, 2), s);
                ctx.fact("Payment", "Credit/debit", doc.el(s, 3), s);
                let m = doc.el(s, 4);
                let mn = match m.as_str() {
                    "ACH" => Some("automated clearing house"),
                    "BOP" => Some("financial institution option"),
                    "CHK" => Some("check"),
                    "FWT" => Some("federal reserve wire transfer"),
                    "NON" => Some("non-payment data"),
                    _ => None,
                };
                ctx.fact("Payment", "Payment method", coded(&m, mn), s);
                ctx.fact("Payment", "Payment date", date(&doc.el(s, 16)), s);
                total = Some((doc.el(s, 2), i));
            }
            "TRN" => {
                ctx.fact("Payment", "Check/EFT trace number", doc.el(s, 2), s);
                ctx.fact("Payment", "Payer identifier", doc.el(s, 3), s);
            }
            "DTM" if claim.is_none() => {
                let q = doc.el(s, 1);
                let label = date_name(&q)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Date ({q})"));
                ctx.fact("Payment", &label, first_date(doc, s), s);
            }
            "N1" if claim.is_none() => {
                let next = ctx.party(segs, i);
                i = next;
                continue;
            }
            "CLP" => {
                count += 1;
                let st = doc.el(s, 2);
                let stn = match st.as_str() {
                    "1" => Some("processed as primary"),
                    "2" => Some("processed as secondary"),
                    "3" => Some("processed as tertiary"),
                    "4" => Some("denied"),
                    "19" => Some("processed as primary, forwarded"),
                    "20" => Some("processed as secondary, forwarded"),
                    "21" => Some("processed as tertiary, forwarded"),
                    "22" => Some("reversal of previous payment"),
                    "23" => Some("not our claim, forwarded"),
                    "25" => Some("predetermination pricing only"),
                    _ => None,
                };
                let paid = doc.el(s, 4);
                claim_paid.push(paid.clone());
                claim = Some(ClaimPay {
                    cells: vec![
                        doc.el(s, 1),
                        coded(&st, stn),
                        doc.el(s, 3),
                        paid.clone(),
                        doc.el(s, 5),
                        String::new(),
                        doc.el(s, 7),
                        doc.el(s, 6),
                    ],
                    paid,
                    lines_paid: vec![],
                    start: i,
                    end: i,
                });
            }
            "NM1" => {
                if let Some(c) = claim.as_mut()
                    && doc.el(s, 1) == "QC"
                {
                    c.cells[5] = nm1_name(doc, s);
                }
            }
            "SVC" => {
                let p = doc.comps(s, 1);
                let procedure = match (p.first(), p.get(1)) {
                    (Some(q), Some(code)) if !code.is_empty() => format!("{q}:{code}"),
                    _ => String::new(),
                };
                let mods: Vec<String> = p
                    .iter()
                    .skip(2)
                    .take(4)
                    .filter(|m| !m.is_empty())
                    .cloned()
                    .collect();
                let paid = doc.el(s, 3);
                let cid = claim
                    .as_ref()
                    .map(|c| c.cells[0].clone())
                    .unwrap_or_default();
                if let Some(c) = claim.as_mut() {
                    c.lines_paid.push(paid.clone());
                }
                svc = Some((
                    vec![
                        cid,
                        String::new(),
                        procedure,
                        mods.join(" "),
                        doc.el(s, 4),
                        doc.el(s, 2),
                        paid,
                        doc.el(s, 5),
                        String::new(),
                        String::new(),
                    ],
                    vec![],
                    i,
                    i,
                ));
            }
            "DTM" => {
                if let Some((cells, _, _, _)) = svc.as_mut()
                    && doc.el(s, 1) == "472"
                {
                    cells[8] = first_date(doc, s);
                }
            }
            "CAS" => {
                let group = doc.el(s, 1);
                let gn = match group.as_str() {
                    "CO" => Some("contractual obligations"),
                    "CR" => Some("corrections and reversals"),
                    "OA" => Some("other adjustments"),
                    "PI" => Some("payer initiated reductions"),
                    "PR" => Some("patient responsibility"),
                    _ => None,
                };
                let cid = claim
                    .as_ref()
                    .map(|c| c.cells[0].clone())
                    .unwrap_or_default();
                let level = match svc.as_ref() {
                    Some((cells, _, _, _)) => format!("service {}", cells[2]),
                    None => "claim".to_owned(),
                };
                let mut k = 2;
                while k < doc.count(s) && k <= 17 {
                    let (reason, amount, qty) = (doc.el(s, k), doc.el(s, k + 1), doc.el(s, k + 2));
                    if !reason.is_empty() || !amount.is_empty() {
                        if let Some((_, adj, _, _)) = svc.as_mut()
                            && !amount.is_empty()
                        {
                            adj.push(amount.clone());
                        }
                        ctx.v.adjustments.push(
                            vec![
                                cid.clone(),
                                level.clone(),
                                coded(&group, gn),
                                reason,
                                amount,
                                qty,
                            ],
                            doc.src(s),
                        );
                    }
                    k += 3;
                }
            }
            "PLB" => {
                let mut k = 3;
                while k < doc.count(s) && k <= 13 {
                    let reason = doc.comps(s, k);
                    let amount = doc.el(s, k + 1);
                    if !amount.is_empty() {
                        plb.push(amount.clone());
                    }
                    ctx.v.provider_adj.push(
                        vec![
                            doc.el(s, 1),
                            date(&doc.el(s, 2)),
                            reason.first().cloned().unwrap_or_default(),
                            reason.get(1).cloned().unwrap_or_default(),
                            amount,
                        ],
                        doc.src(s),
                    );
                    k += 2;
                }
            }
            _ => {}
        }
        i += 1;
    }
    flush_svc(ctx, &mut svc);
    flush_claim(ctx, &mut claim);
    ctx.fact("Payment", "Claims", count.to_string(), &segs[0]);
    if let Some((t, _)) = total {
        let mut values = claim_paid;
        values.extend(plb.iter().map(|x| negate(x)));
        ctx.v.sums.push((
            format!(
                "Total payment (BPR02) = claim payments − provider adjustments, transaction set {}",
                ctx.set
            ),
            Some(t),
            decimal_sum(values.iter().map(String::as_str)),
        ));
    }
}

/// 270/271 eligibility: the parties at each HL level, inquiries (EQ) and benefits (EB).
fn eligibility(ctx: &mut Ctx, segs: &[Seg]) {
    let doc = ctx.doc;
    let mut person = String::new();
    let mut person_id = String::new();
    let mut level = String::new();
    let mut dob = String::new();
    let mut trace = String::new();
    for s in segs {
        match doc.tag(s) {
            "BHT" => {
                let p = doc.el(s, 2);
                let pn = match p.as_str() {
                    "13" => Some("request"),
                    "11" => Some("response"),
                    "01" => Some("cancellation"),
                    _ => None,
                };
                ctx.fact("Eligibility", "Purpose", coded(&p, pn), s);
                ctx.fact("Eligibility", "Reference", doc.el(s, 3), s);
                ctx.fact("Eligibility", "Created", date(&doc.el(s, 4)), s);
            }
            "HL" => {
                let code = doc.el(s, 3);
                level = coded(&code, level_name(&code));
                if matches!(code.as_str(), "22" | "23") {
                    person.clear();
                    person_id.clear();
                    dob.clear();
                    trace.clear();
                }
            }
            "NM1" => {
                let code = doc.el(s, 1);
                let g = format!("{} ({code})", entity_name(&code).unwrap_or("Party"));
                let name = nm1_name(doc, s);
                if matches!(code.as_str(), "IL" | "03") {
                    person = name.clone();
                    person_id = doc.el(s, 9);
                }
                ctx.fact(&g, "Name", name, s);
                ctx.fact(&g, "ID", nm1_id(doc, s), s);
            }
            "TRN" => trace = doc.el(s, 2),
            "DMG" => {
                dob = date(&doc.el(s, 2));
                ctx.fact("Subscriber (IL)", "Birth date", dob.clone(), s);
                ctx.fact("Subscriber (IL)", "Gender", doc.el(s, 3), s);
            }
            "EQ" => {
                let types = doc.reps(s, 1).join(", ");
                ctx.v.inquiries.push(
                    vec![
                        person.clone(),
                        level.clone(),
                        person_id.clone(),
                        dob.clone(),
                        types,
                        trace.clone(),
                    ],
                    doc.src(s),
                );
            }
            "EB" => {
                let info = doc.el(s, 1);
                let n = match info.as_str() {
                    "1" => Some("active coverage"),
                    "2" => Some("active, full risk capitation"),
                    "3" => Some("active, services capitated"),
                    "5" => Some("active, pending investigation"),
                    "6" => Some("inactive"),
                    "7" => Some("inactive, pending eligibility update"),
                    "8" => Some("inactive, pending investigation"),
                    "A" => Some("co-insurance"),
                    "B" => Some("co-payment"),
                    "C" => Some("deductible"),
                    "CB" => Some("coverage basis"),
                    "D" => Some("benefit description"),
                    "E" => Some("exclusions"),
                    "F" => Some("limitations"),
                    "G" => Some("out of pocket (stop loss)"),
                    "H" => Some("unlimited"),
                    "I" => Some("non-covered"),
                    "J" => Some("cost containment"),
                    "L" => Some("primary care provider"),
                    "MC" => Some("managed care coordinator"),
                    "R" => Some("other or additional payer"),
                    "U" => Some("contact following entity"),
                    "V" => Some("cannot process"),
                    _ => None,
                };
                ctx.v.benefits.push(
                    vec![
                        person.clone(),
                        coded(&info, n),
                        doc.el(s, 2),
                        doc.reps(s, 3).join(", "),
                        doc.el(s, 4),
                        doc.el(s, 5),
                        doc.el(s, 6),
                        doc.el(s, 7),
                        doc.el(s, 8),
                        [doc.el(s, 10), doc.el(s, 9)].join(" ").trim().to_owned(),
                        doc.el(s, 12),
                    ],
                    doc.src(s),
                );
            }
            "AAA" => {
                let reason = doc.el(s, 3);
                let rn = match reason.as_str() {
                    "42" => Some("unable to respond at current time"),
                    "43" => Some("invalid/missing provider identification"),
                    "58" => Some("invalid/missing date of birth"),
                    "72" => Some("invalid/missing subscriber/insured ID"),
                    "73" => Some("invalid/missing subscriber/insured name"),
                    "75" => Some("subscriber/insured not found"),
                    "76" => Some("duplicate subscriber/insured ID number"),
                    _ => None,
                };
                ctx.fact(
                    "Rejections",
                    &format!("Request rejected ({})", level),
                    format!(
                        "valid request {}, reason {}",
                        doc.el(s, 1),
                        coded(&reason, rn)
                    ),
                    s,
                );
            }
            _ => {}
        }
    }
}
