//! HL7 version 2 messages (pipe-delimited), recognized by `MSH` (or a batch `FHS`/`BHS`)
//! followed by its delimiters.
//!
//! MSH-1 is the field separator and MSH-2 the encoding characters (component, repetition,
//! escape, subcomponent). Segments end at CR, LF or CRLF (MLLP framing bytes are ignored);
//! escape sequences (\F\ \S\ \T\ \R\ \E\ \Xhh\ \.br\) are resolved for display. Each message
//! gets its MSH facts, patient facts from PID, the segments with numbered and named fields, and
//! an OBX results table. Batches (FHS/BHS … BTS/FTS, or consecutive MSH) are split into
//! messages and the batch counts compared. Nothing is checked against the HL7 v2 message
//! structures or tables.
use super::edi::{Rows, Tally, clip, content_start, decode, node};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};
use std::ops::Range;

pub struct Hl7;

/// Segments read; more are reported as skipped.
const MAX_SEGMENTS: usize = 1_000_000;
/// Fields per segment, repetitions per field and components per value that are read.
const MAX_FIELDS: usize = 300;
const MAX_REPS: usize = 100;
const MAX_COMPONENTS: usize = 100;

#[derive(Clone, Copy)]
struct Delims {
    field: u8,
    comp: u8,
    rep: u8,
    esc: Option<u8>,
    sub: Option<u8>,
}

/// Delimiters from a header segment (`MSH|^~\&|…`) at the start of `h`.
fn delims(h: &[u8]) -> Option<Delims> {
    let tag = h.get(..3)?;
    if !matches!(tag, b"MSH" | b"FHS" | b"BHS") {
        return None;
    }
    let field = *h.get(3)?;
    let ok = |b: u8| b.is_ascii_punctuation();
    if !ok(field) {
        return None;
    }
    let enc: Vec<u8> = h
        .iter()
        .skip(4)
        .take(5)
        .copied()
        .take_while(|&b| b != field && b != b'\r' && b != b'\n')
        .collect();
    if enc.len() < 2 || !enc.iter().all(|&b| ok(b)) || h.get(4 + enc.len()) != Some(&field) {
        return None;
    }
    let mut all = enc.clone();
    all.push(field);
    let mut sorted = all.clone();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() != all.len() {
        return None;
    }
    Some(Delims {
        field,
        comp: enc[0],
        rep: enc[1],
        esc: enc.get(2).copied(),
        sub: enc.get(3).copied(),
    })
}

impl Handler for Hl7 {
    fn name(&self) -> &'static str {
        "HL7 v2"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let h = skip_framing(probe.head);
        delims(&probe.head[h..]).is_some()
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let from = skip_framing(input.bytes);
        let d = delims(&input.bytes[from..]).ok_or("No HL7 header segment")?;
        inspect_bytes(input.bytes, from, d, cancelled)
    }
}

/// Skip a byte order mark, white space and MLLP start bytes (0x0B).
fn skip_framing(b: &[u8]) -> usize {
    let mut i = content_start(b);
    while b
        .get(i)
        .is_some_and(|&c| c == 0x0B || c.is_ascii_whitespace())
    {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------------------------
// Segments and fields

struct Segment {
    start: usize,
    end: usize,
    /// Field ranges; for MSH/FHS/BHS index 1 is the field separator itself.
    fields: Vec<(usize, usize)>,
}

struct Doc<'a> {
    bytes: &'a [u8],
    d: Delims,
    segs: Vec<Segment>,
    truncated: bool,
}

impl<'a> Doc<'a> {
    fn tag(&self, s: &Segment) -> &'a str {
        s.fields
            .first()
            .and_then(|&(a, b)| self.bytes.get(a..b))
            .and_then(|t| std::str::from_utf8(t).ok())
            .unwrap_or("?")
    }
    fn raw(&self, s: &Segment, n: usize) -> &'a [u8] {
        s.fields
            .get(n)
            .and_then(|&(a, b)| self.bytes.get(a..b))
            .unwrap_or(&[])
    }
    fn range(&self, s: &Segment, n: usize) -> Option<Range<u64>> {
        s.fields.get(n).map(|&(a, b)| a as u64..b as u64)
    }
    fn src(&self, s: &Segment) -> Option<Range<u64>> {
        Some(s.start as u64..s.end as u64)
    }
    fn text(&self, s: &Segment) -> String {
        decode(self.bytes.get(s.start..s.end).unwrap_or(&[]))
    }
    /// Resolve escape sequences in a piece of a field.
    fn unescape(&self, raw: &[u8]) -> String {
        let Some(esc) = self.d.esc else {
            return decode(raw);
        };
        if !raw.contains(&esc) {
            return decode(raw);
        }
        let mut out: Vec<u8> = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] != esc {
                out.push(raw[i]);
                i += 1;
                continue;
            }
            let Some(len) = raw[i + 1..].iter().position(|&b| b == esc) else {
                out.extend_from_slice(&raw[i..]);
                break;
            };
            let seq = &raw[i + 1..i + 1 + len];
            match seq {
                b"F" => out.push(self.d.field),
                b"S" => out.push(self.d.comp),
                b"R" => out.push(self.d.rep),
                b"E" => out.push(esc),
                b"T" => out.push(self.d.sub.unwrap_or(b'&')),
                b".br" => out.push(b'\n'),
                b"H" | b"N" => {}
                [b'X', hex @ ..] if hex.len() % 2 == 0 && hex.iter().all(u8::is_ascii_hexdigit) => {
                    for pair in hex.chunks(2) {
                        let s = std::str::from_utf8(pair).unwrap_or("00");
                        out.push(u8::from_str_radix(s, 16).unwrap_or(b'?'));
                    }
                }
                _ => out.extend_from_slice(&raw[i..i + len + 2]),
            }
            i += len + 2;
        }
        decode(&out)
    }
    fn split(raw: &[u8], sep: u8, max: usize) -> Vec<&[u8]> {
        raw.split(|&b| b == sep).take(max).collect()
    }
    /// Field `n` for display: repetitions, components and subcomponents as written, escapes
    /// resolved.
    fn field(&self, s: &Segment, n: usize) -> String {
        let raw = self.raw(s, n);
        if self.is_header(s) && n <= 2 {
            return decode(raw);
        }
        self.show(raw)
    }
    fn show(&self, raw: &[u8]) -> String {
        let reps = Self::split(raw, self.d.rep, MAX_REPS);
        let mut out = String::new();
        for (r, rep) in reps.iter().enumerate() {
            if r > 0 {
                out.push(self.d.rep as char);
            }
            for (c, comp) in Self::split(rep, self.d.comp, MAX_COMPONENTS)
                .iter()
                .enumerate()
            {
                if c > 0 {
                    out.push(self.d.comp as char);
                }
                match self.d.sub {
                    Some(sub) => {
                        for (k, part) in Self::split(comp, sub, MAX_COMPONENTS).iter().enumerate() {
                            if k > 0 {
                                out.push(sub as char);
                            }
                            out.push_str(&self.unescape(part));
                        }
                    }
                    None => out.push_str(&self.unescape(comp)),
                }
            }
        }
        out
    }
    /// Repetitions of field `n`, each as raw bytes.
    fn reps(&self, s: &Segment, n: usize) -> Vec<&'a [u8]> {
        Self::split(self.raw(s, n), self.d.rep, MAX_REPS)
    }
    /// Components of one repetition, unescaped (subcomponents kept as written).
    fn comps(&self, rep: &[u8]) -> Vec<String> {
        Self::split(rep, self.d.comp, MAX_COMPONENTS)
            .into_iter()
            .map(|c| self.show_sub(c))
            .collect()
    }
    fn show_sub(&self, c: &[u8]) -> String {
        match self.d.sub {
            Some(sub) => Self::split(c, sub, MAX_COMPONENTS)
                .iter()
                .map(|p| self.unescape(p))
                .collect::<Vec<_>>()
                .join(&(sub as char).to_string()),
            None => self.unescape(c),
        }
    }
    /// Component `c` (1-based) of the first repetition of field `n`.
    fn comp(&self, s: &Segment, n: usize, c: usize) -> String {
        let reps = self.reps(s, n);
        let first = reps.first().copied().unwrap_or(&[]);
        self.comps(first)
            .get(c.wrapping_sub(1))
            .cloned()
            .unwrap_or_default()
    }
    fn is_header(&self, s: &Segment) -> bool {
        matches!(self.tag(s), "MSH" | "FHS" | "BHS")
    }
}

fn split_segments<'a>(
    bytes: &'a [u8],
    from: usize,
    d: Delims,
    cancelled: &dyn Fn() -> bool,
) -> Result<Doc<'a>, String> {
    let mut segs = vec![];
    let mut truncated = false;
    let mut pos = from;
    let breaks = |b: u8| matches!(b, b'\r' | b'\n' | 0x0B | 0x1C);
    while pos < bytes.len() {
        while pos < bytes.len() && breaks(bytes[pos]) {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        if segs.len() >= MAX_SEGMENTS {
            truncated = true;
            break;
        }
        if segs.len() % 4096 == 4095 && cancelled() {
            return Err("Cancelled".into());
        }
        let start = pos;
        let end = bytes[pos..]
            .iter()
            .position(|&b| breaks(b))
            .map_or(bytes.len(), |p| pos + p);
        let line = &bytes[start..end];
        let mut fields = vec![];
        let mut fs = start;
        for (k, &b) in line.iter().enumerate() {
            if fields.len() >= MAX_FIELDS {
                break;
            }
            if b == d.field {
                fields.push((fs, start + k));
                fs = start + k + 1;
            }
        }
        if fields.len() < MAX_FIELDS {
            fields.push((fs, end));
        }
        // In header segments, field 1 is the separator itself: insert it.
        let tag = &bytes[start..fields[0].1];
        if matches!(tag, b"MSH" | b"FHS" | b"BHS") && fields.len() > 1 {
            let sep = fields[0].1;
            fields.insert(1, (sep, sep + 1));
        }
        segs.push(Segment { start, end, fields });
        pos = end;
    }
    Ok(Doc {
        bytes,
        d,
        segs,
        truncated,
    })
}

// ---------------------------------------------------------------------------------------------
// Names

fn segment_name(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "MSH" => "Message header",
        "EVN" => "Event type",
        "PID" => "Patient identification",
        "PD1" => "Patient additional demographic",
        "PV1" => "Patient visit",
        "PV2" => "Patient visit – additional information",
        "NK1" => "Next of kin / associated parties",
        "AL1" => "Patient allergy information",
        "DG1" => "Diagnosis",
        "PR1" => "Procedures",
        "IN1" => "Insurance",
        "IN2" => "Insurance additional information",
        "GT1" => "Guarantor",
        "ORC" => "Common order",
        "OBR" => "Observation request",
        "OBX" => "Observation/result",
        "NTE" => "Notes and comments",
        "SPM" => "Specimen",
        "MSA" => "Message acknowledgment",
        "ERR" => "Error",
        "MRG" => "Merge patient information",
        "RXO" => "Pharmacy/treatment order",
        "RXE" => "Pharmacy/treatment encoded order",
        "RXA" => "Pharmacy/treatment administration",
        "RXR" => "Pharmacy/treatment route",
        "SCH" => "Scheduling activity information",
        "AIS" => "Appointment information – service",
        "TXA" => "Transcription document header",
        "ROL" => "Role",
        "FHS" => "File header",
        "FTS" => "File trailer",
        "BHS" => "Batch header",
        "BTS" => "Batch trailer",
        "QRD" => "Original-style query definition",
        "QPD" => "Query parameter definition",
        "TQ1" => "Timing/quantity",
        "ZDS" | "ZPI" | "ZPV" => "Site-defined segment",
        _ => return None,
    })
}

const MSH: &[&str] = &[
    "Field separator",
    "Encoding characters",
    "Sending application",
    "Sending facility",
    "Receiving application",
    "Receiving facility",
    "Date/time of message",
    "Security",
    "Message type",
    "Message control ID",
    "Processing ID",
    "Version ID",
    "Sequence number",
    "Continuation pointer",
    "Accept acknowledgment type",
    "Application acknowledgment type",
    "Country code",
    "Character set",
    "Principal language of message",
    "Alternate character set handling scheme",
    "Message profile identifier",
];
const EVN: &[&str] = &[
    "Event type code",
    "Recorded date/time",
    "Date/time planned event",
    "Event reason code",
    "Operator ID",
    "Event occurred",
    "Event facility",
];
const PID: &[&str] = &[
    "Set ID",
    "Patient ID",
    "Patient identifier list",
    "Alternate patient ID",
    "Patient name",
    "Mother's maiden name",
    "Date/time of birth",
    "Administrative sex",
    "Patient alias",
    "Race",
    "Patient address",
    "County code",
    "Phone number – home",
    "Phone number – business",
    "Primary language",
    "Marital status",
    "Religion",
    "Patient account number",
    "SSN number",
    "Driver's license number",
    "Mother's identifier",
    "Ethnic group",
    "Birth place",
    "Multiple birth indicator",
    "Birth order",
    "Citizenship",
    "Veterans military status",
    "Nationality",
    "Patient death date and time",
    "Patient death indicator",
    "Identity unknown indicator",
    "Identity reliability code",
    "Last update date/time",
    "Last update facility",
    "Species code",
    "Breed code",
    "Strain",
    "Production class code",
    "Tribal citizenship",
];
const PV1: &[&str] = &[
    "Set ID",
    "Patient class",
    "Assigned patient location",
    "Admission type",
    "Preadmit number",
    "Prior patient location",
    "Attending doctor",
    "Referring doctor",
    "Consulting doctor",
    "Hospital service",
    "Temporary location",
    "Preadmit test indicator",
    "Re-admission indicator",
    "Admit source",
    "Ambulatory status",
    "VIP indicator",
    "Admitting doctor",
    "Patient type",
    "Visit number",
    "Financial class",
    "Charge price indicator",
    "Courtesy code",
    "Credit rating",
    "Contract code",
    "Contract effective date",
    "Contract amount",
    "Contract period",
    "Interest code",
    "Transfer to bad debt code",
    "Transfer to bad debt date",
    "Bad debt agency code",
    "Bad debt transfer amount",
    "Bad debt recovery amount",
    "Delete account indicator",
    "Delete account date",
    "Discharge disposition",
    "Discharged to location",
    "Diet type",
    "Servicing facility",
    "Bed status",
    "Account status",
    "Pending location",
    "Prior temporary location",
    "Admit date/time",
    "Discharge date/time",
    "Current patient balance",
    "Total charges",
    "Total adjustments",
    "Total payments",
    "Alternate visit ID",
    "Visit indicator",
    "Other healthcare provider",
];
const ORC: &[&str] = &[
    "Order control",
    "Placer order number",
    "Filler order number",
    "Placer group number",
    "Order status",
    "Response flag",
    "Quantity/timing",
    "Parent order",
    "Date/time of transaction",
    "Entered by",
    "Verified by",
    "Ordering provider",
    "Enterer's location",
    "Call back phone number",
    "Order effective date/time",
    "Order control code reason",
    "Entering organization",
    "Entering device",
    "Action by",
    "Advanced beneficiary notice code",
    "Ordering facility name",
    "Ordering facility address",
    "Ordering facility phone number",
    "Ordering provider address",
    "Order status modifier",
];
const OBR: &[&str] = &[
    "Set ID",
    "Placer order number",
    "Filler order number",
    "Universal service identifier",
    "Priority",
    "Requested date/time",
    "Observation date/time",
    "Observation end date/time",
    "Collection volume",
    "Collector identifier",
    "Specimen action code",
    "Danger code",
    "Relevant clinical information",
    "Specimen received date/time",
    "Specimen source",
    "Ordering provider",
    "Order callback phone number",
    "Placer field 1",
    "Placer field 2",
    "Filler field 1",
    "Filler field 2",
    "Results rpt/status chng – date/time",
    "Charge to practice",
    "Diagnostic serv sect ID",
    "Result status",
    "Parent result",
    "Quantity/timing",
    "Result copies to",
    "Parent",
    "Transportation mode",
    "Reason for study",
    "Principal result interpreter",
    "Assistant result interpreter",
    "Technician",
    "Transcriptionist",
    "Scheduled date/time",
    "Number of sample containers",
    "Transport logistics of collected sample",
    "Collector's comment",
    "Transport arrangement responsibility",
    "Transport arranged",
    "Escort required",
    "Planned patient transport comment",
];
const OBX: &[&str] = &[
    "Set ID",
    "Value type",
    "Observation identifier",
    "Observation sub-ID",
    "Observation value",
    "Units",
    "References range",
    "Abnormal flags",
    "Probability",
    "Nature of abnormal test",
    "Observation result status",
    "Effective date of reference range",
    "User defined access checks",
    "Date/time of the observation",
    "Producer's ID",
    "Responsible observer",
    "Observation method",
    "Equipment instance identifier",
    "Date/time of the analysis",
];
const NK1: &[&str] = &[
    "Set ID",
    "Name",
    "Relationship",
    "Address",
    "Phone number",
    "Business phone number",
    "Contact role",
    "Start date",
    "End date",
    "Next of kin job title",
    "Next of kin job code/class",
    "Next of kin employee number",
    "Organization name",
    "Marital status",
    "Administrative sex",
    "Date/time of birth",
];
const AL1: &[&str] = &[
    "Set ID",
    "Allergen type code",
    "Allergen code/mnemonic/description",
    "Allergy severity code",
    "Allergy reaction code",
    "Identification date",
];
const DG1: &[&str] = &[
    "Set ID",
    "Diagnosis coding method",
    "Diagnosis code",
    "Diagnosis description",
    "Diagnosis date/time",
    "Diagnosis type",
    "Major diagnostic category",
    "Diagnostic related group",
    "DRG approval indicator",
    "DRG grouper review code",
    "Outlier type",
    "Outlier days",
    "Outlier cost",
    "Grouper version and type",
    "Diagnosis priority",
    "Diagnosing clinician",
    "Diagnosis classification",
    "Confidential indicator",
    "Attestation date/time",
];
const IN1: &[&str] = &[
    "Set ID",
    "Insurance plan ID",
    "Insurance company ID",
    "Insurance company name",
    "Insurance company address",
    "Insurance co contact person",
    "Insurance co phone number",
    "Group number",
    "Group name",
    "Insured's group emp ID",
    "Insured's group emp name",
    "Plan effective date",
    "Plan expiration date",
    "Authorization information",
    "Plan type",
    "Name of insured",
    "Insured's relationship to patient",
    "Insured's date of birth",
    "Insured's address",
    "Assignment of benefits",
    "Coordination of benefits",
    "Coord of ben. priority",
    "Notice of admission flag",
    "Notice of admission date",
    "Report of eligibility flag",
    "Report of eligibility date",
    "Release information code",
    "Pre-admit cert (PAC)",
    "Verification date/time",
    "Verification by",
    "Type of agreement code",
    "Billing status",
    "Lifetime reserve days",
    "Delay before L.R. day",
    "Company plan code",
    "Policy number",
    "Policy deductible",
    "Policy limit – amount",
    "Policy limit – days",
    "Room rate – semi-private",
    "Room rate – private",
    "Insured's employment status",
    "Insured's administrative sex",
    "Insured's employer's address",
    "Verification status",
    "Prior insurance plan ID",
    "Coverage type",
    "Handicap",
    "Insured's ID number",
];
const MSA: &[&str] = &[
    "Acknowledgment code",
    "Message control ID",
    "Text message",
    "Expected sequence number",
    "Delayed acknowledgment type",
    "Error condition",
];

fn field_name(tag: &str, n: usize) -> Option<&'static str> {
    let names = match tag {
        "MSH" => MSH,
        "EVN" => EVN,
        "PID" => PID,
        "PV1" => PV1,
        "ORC" => ORC,
        "OBR" => OBR,
        "OBX" => OBX,
        "NK1" => NK1,
        "AL1" => AL1,
        "DG1" => DG1,
        "IN1" => IN1,
        "MSA" => MSA,
        _ => return None,
    };
    names.get(n.checked_sub(1)?).copied()
}

/// "20261101103000" -> "20261101103000 (2026-11-01 10:30:00)"; HL7 TS/DTM values may carry
/// fractions and a zone ("+0100"), which are kept as written.
fn ts(raw: &str) -> String {
    let digits: String = raw.chars().take_while(char::is_ascii_digit).collect();
    let d = &digits;
    let shown = match d.len() {
        8 => format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..8]),
        12 => format!(
            "{}-{}-{} {}:{}",
            &d[..4],
            &d[4..6],
            &d[6..8],
            &d[8..10],
            &d[10..12]
        ),
        n if n >= 14 => format!(
            "{}-{}-{} {}:{}:{}",
            &d[..4],
            &d[4..6],
            &d[6..8],
            &d[8..10],
            &d[10..12],
            &d[12..14]
        ),
        _ => return raw.to_owned(),
    };
    let month_ok = matches!(d[4..6].parse::<u8>(), Ok(1..=12));
    if !month_ok {
        return raw.to_owned();
    }
    format!("{raw} ({shown})")
}

/// XPN/XCN name "FAMILY^GIVEN^MIDDLE^SUFFIX^PREFIX" as "PREFIX GIVEN MIDDLE FAMILY SUFFIX".
fn person(c: &[String], offset: usize) -> String {
    let g = |k: usize| c.get(k + offset).cloned().unwrap_or_default();
    [g(4), g(1), g(2), g(0), g(3)]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------------------------
// Inspection

fn inspect_bytes(
    bytes: &[u8],
    from: usize,
    d: Delims,
    cancelled: &dyn Fn() -> bool,
) -> Result<Inspection, String> {
    let doc = split_segments(bytes, from, d, cancelled)?;
    let mut out = Inspection::new("HL7 v2", "HL7 v2 message");
    let mut partial: Vec<String> = vec![];

    // Messages start at MSH; batch envelopes around them.
    let mut starts: Vec<usize> = vec![];
    let mut bhs = 0usize;
    let mut fhs = false;
    let mut bts = Tally::new("BTS batch message count");
    let mut fts = Tally::new("FTS file batch count");
    let mut in_batch = 0usize;
    let mut odd = 0usize;
    for (i, s) in doc.segs.iter().enumerate() {
        let tag = doc.tag(s);
        if tag.len() != 3
            || !tag
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            odd += 1;
        }
        match tag {
            "MSH" => {
                starts.push(i);
                in_batch += 1;
            }
            "FHS" => fhs = true,
            "BHS" => {
                bhs += 1;
                in_batch = 0;
            }
            "BTS" => bts.compare(
                &format!("batch {bhs}"),
                &doc.field(s, 1),
                &in_batch.to_string(),
            ),
            "FTS" => fts.compare("file", &doc.field(s, 1), &bhs.to_string()),
            _ => {}
        }
    }
    if odd > 0 {
        partial.push(format!("{odd} segments without a valid tag"));
    }
    if doc.truncated {
        partial.push(format!("only the first {MAX_SEGMENTS} segments were read"));
    }
    if starts.is_empty() {
        partial.push("no MSH message header".into());
    }

    let first = starts.first().map(|&i| &doc.segs[i]);
    let types: Vec<String> = {
        let mut t: Vec<String> = vec![];
        for &i in &starts {
            let s = &doc.segs[i];
            let ty = [doc.comp(s, 9, 1), doc.comp(s, 9, 2)]
                .into_iter()
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
                .join("^");
            if !t.contains(&ty) && t.len() < 20 {
                t.push(ty);
            }
        }
        t
    };
    out.kind = match (starts.len(), types.as_slice()) {
        (0, _) => "HL7 v2 batch (no messages)".into(),
        (1, [t]) => format!("{t} — {}", event_name(t).unwrap_or("HL7 v2 message")),
        (n, [t]) => format!("{t} — {n} messages ({})", event_name(t).unwrap_or("HL7 v2")),
        (n, many) => format!("HL7 v2 — {n} messages ({})", many.join(", ")),
    };
    if let Some(s) = first {
        out.version = Some(doc.comp(s, 12, 1)).filter(|v| !v.is_empty());
    }
    out.fact(
        "Encoding",
        "Delimiters",
        format!(
            "field {}  component {}  repetition {}  escape {}  subcomponent {}",
            d.field as char,
            d.comp as char,
            d.rep as char,
            d.esc.map(|b| b as char).unwrap_or('—'),
            d.sub.map(|b| b as char).unwrap_or('—'),
        ),
        None,
    );
    if fhs || bhs > 0 {
        out.fact(
            "Batch",
            "Envelope",
            format!("{}{} batches", if fhs { "file header, " } else { "" }, bhs),
            None,
        );
    }
    if starts.len() > 1 {
        out.fact("Batch", "Messages", starts.len().to_string(), None);
    }

    let mut messages = Rows::new(
        "Messages",
        &[
            "#",
            "Message type",
            "Control ID",
            "Date/time",
            "Sending application",
            "Receiving application",
            "Patient",
            "Segments",
        ],
    );
    let mut results = Rows::new(
        "Results (OBX)",
        &[
            "Message",
            "Set ID",
            "Value type",
            "Observation ID",
            "Observation",
            "Value",
            "Units",
            "Reference range",
            "Abnormal flags",
            "Status",
            "Observed",
        ],
    );
    let mut segments = Rows::new(
        "Segments",
        &["#", "Message", "Segment", "Name", "Fields", "Content"],
    );
    let multi = starts.len() > 1;
    let mut full = false;
    let mut msg_no = 0usize;
    for (i, s) in doc.segs.iter().enumerate() {
        let tag = doc.tag(s);
        if tag == "MSH" {
            msg_no += 1;
            // A message ends at the next message or batch/file envelope segment.
            let end = doc.segs[i + 1..]
                .iter()
                .position(|s| matches!(doc.tag(s), "MSH" | "BHS" | "BTS" | "FHS" | "FTS"))
                .map_or(doc.segs.len(), |p| i + 1 + p);
            message_facts(
                &doc,
                &doc.segs[i..end],
                msg_no,
                multi,
                &mut out,
                &mut messages,
            );
        }
        let used = s.fields.len().saturating_sub(1);
        segments.push(
            vec![
                (i + 1).to_string(),
                if msg_no > 0 {
                    msg_no.to_string()
                } else {
                    String::new()
                },
                tag.to_owned(),
                segment_name(tag).unwrap_or("").to_owned(),
                used.to_string(),
                clip(&doc.text(s), super::edi::SHOWN),
            ],
            doc.src(s),
        );
        if tag == "OBX" {
            let id = doc.comp(s, 3, 1);
            let text = doc.comp(s, 3, 2);
            results.push(
                vec![
                    msg_no.to_string(),
                    doc.field(s, 1),
                    doc.field(s, 2),
                    id,
                    text,
                    doc.field(s, 5),
                    doc.comp(s, 6, 1),
                    doc.field(s, 7),
                    doc.field(s, 8),
                    doc.field(s, 11),
                    ts(&doc.field(s, 14)),
                ],
                doc.src(s),
            );
        }
        if !full {
            full = !segment_outline(&doc, s, multi, msg_no, &mut out);
        }
    }
    if full {
        out.notes.push(format!(
            "The outline lists the first {} segments and fields",
            crate::inspect::MAX_ITEMS
        ));
    }

    if partial.is_empty() {
        out.check(
            Stage::Parsed,
            State::Passed,
            format!(
                "{} segments read in {} messages",
                doc.segs.len(),
                starts.len()
            ),
        );
    } else {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("{} segments read; {}", doc.segs.len(), partial.join("; ")),
        );
    }
    out.not_validated();
    bts.emit(&mut out);
    fts.emit(&mut out);
    out.notes.push(
        "Segments and fields are read by position; field names follow HL7 v2.5 for MSH, EVN, PID, PV1, ORC, OBR, OBX, NK1, AL1, DG1, IN1 and MSA. Message structures, data types and tables are not validated"
            .into(),
    );
    out.notes.push(
        "Escape sequences are resolved for display (\\F\\ \\S\\ \\T\\ \\R\\ \\E\\ \\Xhh\\ \\.br\\); character set switches (\\C…\\, \\M…\\) are kept as written"
            .into(),
    );
    messages.emit(&mut out);
    results.emit(&mut out);
    segments.emit(&mut out);
    Ok(out)
}

fn event_name(t: &str) -> Option<&'static str> {
    Some(match t {
        "ADT^A01" => "admit/visit notification",
        "ADT^A02" => "transfer a patient",
        "ADT^A03" => "discharge/end visit",
        "ADT^A04" => "register a patient",
        "ADT^A05" => "pre-admit a patient",
        "ADT^A08" => "update patient information",
        "ADT^A11" => "cancel admit/visit notification",
        "ADT^A13" => "cancel discharge/end visit",
        "ADT^A28" => "add person information",
        "ADT^A31" => "update person information",
        "ADT^A40" => "merge patient – patient identifier list",
        "ORU^R01" => "unsolicited observation result",
        "ORM^O01" => "general order",
        "OML^O21" => "laboratory order",
        "ORL^O22" => "general laboratory order response",
        "SIU^S12" => "notification of new appointment booking",
        "SIU^S14" => "notification of appointment modification",
        "SIU^S15" => "notification of appointment cancellation",
        "MDM^T02" => "original document notification and content",
        "DFT^P03" => "post detail financial transaction",
        "VXU^V04" => "unsolicited vaccination record update",
        "RDE^O11" => "pharmacy/treatment encoded order",
        "QBP^Q11" => "query by parameter",
        "RSP^K11" => "segment pattern response",
        "BAR^P01" => "add patient accounts",
        _ if t.starts_with("ACK") => "general acknowledgment",
        _ if t.starts_with("ADT") => "patient administration",
        _ if t.starts_with("ORU") => "observation result",
        _ if t.starts_with("ORM") => "order",
        _ if t.starts_with("SIU") => "scheduling",
        _ => return None,
    })
}

/// Facts of one message (MSH, EVN, PID, PV1) and its row in the messages table.
fn message_facts(
    doc: &Doc,
    segs: &[Segment],
    n: usize,
    multi: bool,
    out: &mut Inspection,
    table: &mut Rows,
) {
    let Some(msh) = segs.first() else {
        return;
    };
    let suffix = if multi {
        format!(" {n}")
    } else {
        String::new()
    };
    let details = n <= 1000;
    let g = format!("Message{suffix}");
    let ty = doc.field(msh, 9);
    let ctl = doc.field(msh, 10);
    let when = doc.field(msh, 7);
    let app = |s: &Segment, f: usize| {
        let c = doc.comp(s, f, 1);
        if c.is_empty() { doc.field(s, f) } else { c }
    };
    let mut patient = String::new();
    if details {
        let f = |out: &mut Inspection, label: &str, k: usize, v: String| {
            out.fact(&g, label, v, doc.range(msh, k));
        };
        f(out, "Sending application", 3, doc.field(msh, 3));
        f(out, "Sending facility", 4, doc.field(msh, 4));
        f(out, "Receiving application", 5, doc.field(msh, 5));
        f(out, "Receiving facility", 6, doc.field(msh, 6));
        f(out, "Date/time of message", 7, ts(&when));
        let event = [doc.comp(msh, 9, 1), doc.comp(msh, 9, 2)]
            .into_iter()
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
            .join("^");
        let shown = match event_name(&event) {
            Some(nm) => format!("{ty} ({nm})"),
            None => ty.clone(),
        };
        f(out, "Message type", 9, shown);
        f(out, "Message control ID", 10, ctl.clone());
        let pid = doc.comp(msh, 11, 1);
        let p = match pid.as_str() {
            "P" => "P (production)".to_owned(),
            "T" => "T (training)".to_owned(),
            "D" => "D (debugging)".to_owned(),
            _ => doc.field(msh, 11),
        };
        f(out, "Processing ID", 11, p);
        f(out, "Version", 12, doc.field(msh, 12));
        f(out, "Character set", 18, doc.field(msh, 18));
    }
    for s in segs.iter().skip(1) {
        match doc.tag(s) {
            "EVN" if details => {
                out.fact(&g, "Event type", doc.field(s, 1), doc.range(s, 1));
                out.fact(&g, "Event recorded", ts(&doc.field(s, 2)), doc.range(s, 2));
            }
            "PID" if patient.is_empty() => {
                let name_rep = doc.reps(s, 5).first().copied().unwrap_or(&[]);
                let name = person(&doc.comps(name_rep), 0);
                patient = name.clone();
                if !details {
                    continue;
                }
                let pg = format!("Patient{suffix}");
                let ids: Vec<String> = doc
                    .reps(s, 3)
                    .iter()
                    .take(20)
                    .map(|r| {
                        let c = doc.comps(r);
                        let id = c.first().cloned().unwrap_or_default();
                        let auth = c.get(3).cloned().unwrap_or_default();
                        let ty = c.get(4).cloned().unwrap_or_default();
                        let extra: Vec<String> =
                            [ty, auth].into_iter().filter(|x| !x.is_empty()).collect();
                        if extra.is_empty() {
                            id
                        } else {
                            format!("{id} ({})", extra.join(", "))
                        }
                    })
                    .filter(|x| !x.is_empty())
                    .collect();
                out.fact(&pg, "Identifiers", ids.join("; "), doc.range(s, 3));
                out.fact(&pg, "Name", name, doc.range(s, 5));
                out.fact(&pg, "Birth date", ts(&doc.field(s, 7)), doc.range(s, 7));
                let sex = doc.field(s, 8);
                let sx = match sex.as_str() {
                    "F" => "F (female)".to_owned(),
                    "M" => "M (male)".to_owned(),
                    "O" => "O (other)".to_owned(),
                    "U" => "U (unknown)".to_owned(),
                    "A" => "A (ambiguous)".to_owned(),
                    "N" => "N (not applicable)".to_owned(),
                    _ => sex,
                };
                out.fact(&pg, "Administrative sex", sx, doc.range(s, 8));
                let addr = doc.reps(s, 11).first().copied().unwrap_or(&[]);
                let a = doc.comps(addr);
                let g2 = |k: usize| a.get(k).cloned().unwrap_or_default();
                let address = [
                    g2(0),
                    g2(1),
                    format!("{} {}", g2(4), g2(2)).trim().to_owned(),
                    g2(3),
                    g2(5),
                ]
                .into_iter()
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
                out.fact(&pg, "Address", address, doc.range(s, 11));
                out.fact(&pg, "Phone (home)", doc.comp(s, 13, 1), doc.range(s, 13));
                out.fact(&pg, "Account number", doc.comp(s, 18, 1), doc.range(s, 18));
                let death = doc.field(s, 29);
                if !death.is_empty() {
                    out.fact(&pg, "Death date/time", ts(&death), doc.range(s, 29));
                }
            }
            "PV1" if details => {
                let vg = format!("Visit{suffix}");
                let class = doc.field(s, 2);
                let cl = match class.as_str() {
                    "I" => "I (inpatient)".to_owned(),
                    "O" => "O (outpatient)".to_owned(),
                    "E" => "E (emergency)".to_owned(),
                    "P" => "P (preadmit)".to_owned(),
                    "R" => "R (recurring patient)".to_owned(),
                    "B" => "B (obstetrics)".to_owned(),
                    _ => class,
                };
                out.fact(&vg, "Patient class", cl, doc.range(s, 2));
                let loc = doc.reps(s, 3).first().copied().unwrap_or(&[]);
                let l = doc.comps(loc);
                let location = [
                    l.first().cloned().unwrap_or_default(),
                    l.get(1)
                        .map(|r| format!("room {r}"))
                        .filter(|_| l.get(1).is_some_and(|r| !r.is_empty()))
                        .unwrap_or_default(),
                    l.get(2)
                        .map(|b| format!("bed {b}"))
                        .filter(|_| l.get(2).is_some_and(|b| !b.is_empty()))
                        .unwrap_or_default(),
                    l.get(3).cloned().unwrap_or_default(),
                ]
                .into_iter()
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
                out.fact(&vg, "Location", location, doc.range(s, 3));
                let att = doc.reps(s, 7).first().copied().unwrap_or(&[]);
                let a = doc.comps(att);
                let who = person(&a, 1);
                let id = a.first().cloned().unwrap_or_default();
                let shown = match (who.is_empty(), id.is_empty()) {
                    (false, false) => format!("{who} ({id})"),
                    (false, true) => who,
                    _ => id,
                };
                out.fact(&vg, "Attending doctor", shown, doc.range(s, 7));
                out.fact(&vg, "Visit number", doc.comp(s, 19, 1), doc.range(s, 19));
                out.fact(
                    &vg,
                    "Admit date/time",
                    ts(&doc.field(s, 44)),
                    doc.range(s, 44),
                );
                out.fact(
                    &vg,
                    "Discharge date/time",
                    ts(&doc.field(s, 45)),
                    doc.range(s, 45),
                );
            }
            "MSA" if details => {
                out.fact(&g, "Acknowledgment code", doc.field(s, 1), doc.range(s, 1));
                out.fact(
                    &g,
                    "Acknowledged control ID",
                    doc.field(s, 2),
                    doc.range(s, 2),
                );
                out.fact(&g, "Acknowledgment text", doc.field(s, 3), doc.range(s, 3));
            }
            _ => {}
        }
    }
    let last = segs.last().unwrap_or(msh);
    table.push(
        vec![
            n.to_string(),
            ty,
            ctl,
            ts(&when),
            app(msh, 3),
            app(msh, 5),
            patient,
            segs.len().to_string(),
        ],
        Some(msh.start as u64..last.end as u64),
    );
}

/// One segment in the outline: the segment, its non-empty fields (`PID-5 Patient name`), their
/// repetitions and components. Returns false when the outline is full.
fn segment_outline(doc: &Doc, s: &Segment, multi: bool, msg: usize, out: &mut Inspection) -> bool {
    let tag = doc.tag(s);
    let base = usize::from(multi);
    if multi
        && tag == "MSH"
        && !node(
            out,
            0,
            format!("Message {msg}"),
            doc.field(s, 9),
            doc.src(s),
        )
    {
        return false;
    }
    let label = match segment_name(tag) {
        Some(n) => format!("{tag} — {n}"),
        None => tag.to_owned(),
    };
    let depth = if multi && tag != "MSH" && msg == 0 {
        0
    } else {
        base
    };
    if !node(
        out,
        depth,
        label,
        clip(&doc.text(s), super::edi::SHOWN),
        doc.src(s),
    ) {
        return false;
    }
    let header = doc.is_header(s);
    for n in 1..s.fields.len() {
        let raw = doc.raw(s, n);
        if raw.is_empty() {
            continue;
        }
        let name = field_name(tag, n);
        let label = match name {
            Some(nm) => format!("{tag}-{n} {nm}"),
            None => format!("{tag}-{n}"),
        };
        let value = clip(&doc.field(s, n), super::edi::SHOWN);
        if !node(out, depth + 1, label, value, doc.range(s, n)) {
            return false;
        }
        if header && n <= 2 {
            continue;
        }
        let reps = doc.reps(s, n);
        for (r, rep) in reps.iter().enumerate() {
            let comps = doc.comps(rep);
            let deeper = if reps.len() > 1 {
                let l = format!("{tag}-{n}[{}]", r + 1);
                if !node(
                    out,
                    depth + 2,
                    l,
                    clip(&doc.show(rep), super::edi::SHOWN),
                    None,
                ) {
                    return false;
                }
                depth + 3
            } else {
                depth + 2
            };
            if comps.len() > 1 {
                for (c, v) in comps.iter().enumerate() {
                    if v.is_empty() {
                        continue;
                    }
                    if !node(
                        out,
                        deeper,
                        format!("{tag}-{n}.{}", c + 1),
                        clip(v, super::edi::SHOWN),
                        None,
                    ) {
                        return false;
                    }
                }
            }
        }
    }
    true
}
