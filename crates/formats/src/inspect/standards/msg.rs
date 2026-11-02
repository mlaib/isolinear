//! Outlook messages (`.msg`, [MS-OXMSG]): a Compound File whose streams hold MAPI properties.
//! Variable-length values are `__substg1.0_IIIITTTT` streams (property id and type in hex),
//! fixed-size ones are 16-byte entries of `__properties_version1.0`; recipients and
//! attachments are sub-storages with the same layout. The compressed RTF body is not
//! decompressed; the plain or HTML body is shown.
use super::cfb::{self, Cfb, Kind};
use super::doc_common::{
    clip, decode_charset, filetime, human, is_picture, push_row, table, u16le, u32le, u64le,
    utf16le,
};
use super::mail;
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State, Visual};
use std::ops::Range;

const MAX_PICTURES: usize = 10;
const MAX_RECIPIENTS: usize = 10_000;
const MAX_ATTACHMENTS: usize = 10_000;

#[derive(Clone, Debug)]
enum Value {
    Text(String),
    Binary(Vec<u8>),
    Int(i64),
    Bool(bool),
    Float(f64),
    Time(String),
    Other(String),
}
impl Value {
    fn summary(&self) -> String {
        match self {
            Value::Text(s) => clip(s, 200),
            Value::Binary(b) => {
                let hex: Vec<String> = b.iter().take(16).map(|x| format!("{x:02X}")).collect();
                format!(
                    "{} bytes: {}{}",
                    b.len(),
                    hex.join(" "),
                    if b.len() > 16 { " …" } else { "" }
                )
            }
            Value::Int(i) => i.to_string(),
            Value::Bool(b) => if *b { "true" } else { "false" }.into(),
            Value::Float(f) => f.to_string(),
            Value::Time(t) => t.clone(),
            Value::Other(s) => s.clone(),
        }
    }
    fn text(&self) -> Option<String> {
        match self {
            Value::Text(s) => Some(s.clone()),
            Value::Time(t) => Some(t.clone()),
            Value::Int(_) | Value::Bool(_) | Value::Float(_) => Some(self.summary()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct Prop {
    id: u16,
    kind: u16,
    value: Value,
    source: Option<Range<u64>>,
}

fn type_name(t: u16) -> &'static str {
    match t {
        0x0002 => "Integer16",
        0x0003 => "Integer32",
        0x0004 => "Floating32",
        0x0005 => "Floating64",
        0x0006 => "Currency",
        0x0007 => "FloatingTime",
        0x000A => "ErrorCode",
        0x000B => "Boolean",
        0x000D => "Object",
        0x0014 => "Integer64",
        0x001E => "String8",
        0x001F => "String",
        0x0040 => "Time",
        0x0048 => "Guid",
        0x0102 => "Binary",
        t if t & 0x1000 != 0 => "Multiple values",
        _ => "Other",
    }
}

fn prop_name(id: u16) -> &'static str {
    match id {
        0x0017 => "Importance",
        0x001A => "MessageClass",
        0x0026 => "Priority",
        0x0036 => "Sensitivity",
        0x0037 => "Subject",
        0x0039 => "ClientSubmitTime",
        0x003D => "SubjectPrefix",
        0x0042 => "SentRepresentingName",
        0x0064 => "SentRepresentingAddressType",
        0x0065 => "SentRepresentingEmailAddress",
        0x0070 => "ConversationTopic",
        0x0071 => "ConversationIndex",
        0x007D => "TransportMessageHeaders",
        0x0C15 => "RecipientType",
        0x0C1A => "SenderName",
        0x0C1E => "SenderAddressType",
        0x0C1F => "SenderEmailAddress",
        0x0E02 => "DisplayBcc",
        0x0E03 => "DisplayCc",
        0x0E04 => "DisplayTo",
        0x0E06 => "MessageDeliveryTime",
        0x0E07 => "MessageFlags",
        0x0E08 => "MessageSize",
        0x0E1B => "HasAttachments",
        0x0E1D => "NormalizedSubject",
        0x0E20 => "AttachSize",
        0x0E21 => "AttachNumber",
        0x0FF9 => "RecordKey",
        0x1000 => "Body",
        0x1009 => "RtfCompressed",
        0x1013 => "Html",
        0x1035 => "InternetMessageId",
        0x1039 => "InternetReferences",
        0x1042 => "InReplyToId",
        0x3001 => "DisplayName",
        0x3002 => "AddressType",
        0x3003 => "EmailAddress",
        0x3007 => "CreationTime",
        0x3008 => "LastModificationTime",
        0x3701 => "AttachDataBinary",
        0x3703 => "AttachExtension",
        0x3704 => "AttachFilename",
        0x3705 => "AttachMethod",
        0x3707 => "AttachLongFilename",
        0x370E => "AttachMimeTag",
        0x3712 => "AttachContentId",
        0x3716 => "AttachContentDisposition",
        0x39FE => "SmtpAddress",
        0x3FDE => "InternetCodepage",
        0x3FFA => "LastModifierName",
        0x5D01 => "SenderSmtpAddress",
        0x5D02 => "SentRepresentingSmtpAddress",
        id if id >= 0x8000 => "(named property)",
        _ => "",
    }
}

/// The properties of one object (message, recipient, attachment): its substg streams and the
/// fixed-size entries of its properties stream (`header` bytes before the entries).
fn properties(cfb: &Cfb, storage: usize, header: usize) -> Vec<Prop> {
    let mut out = vec![];
    let span = |i: usize, len: usize| cfb.locator(i).range(0, len);
    for child in cfb.children(storage) {
        let e = &cfb.entries[child];
        if e.kind != Kind::Stream {
            continue;
        }
        if let Some(tag) = e.name.strip_prefix("__substg1.0_") {
            let Ok(tag) = u32::from_str_radix(tag.get(..8).unwrap_or(""), 16) else {
                continue;
            };
            let (id, kind) = ((tag >> 16) as u16, tag as u16);
            let Ok((bytes, _)) = cfb.read(child) else {
                continue;
            };
            let value = match kind {
                0x001F => Value::Text(utf16le(&bytes)),
                0x001E => Value::Text(match std::str::from_utf8(&bytes) {
                    Ok(s) => s.trim_end_matches('\0').to_owned(),
                    Err(_) => decode_charset(&bytes, "windows-1252")
                        .trim_end_matches('\0')
                        .to_owned(),
                }),
                0x0102 => Value::Binary(bytes.clone()),
                0x0048 if bytes.len() >= 16 => Value::Other(guid(&bytes)),
                t if t & 0x1000 != 0 => Value::Other(format!("{} bytes of values", bytes.len())),
                _ => Value::Binary(bytes.clone()),
            };
            out.push(Prop {
                id,
                kind,
                value,
                source: span(child, bytes.len()),
            });
        }
    }
    if let Some(p) = cfb.child(storage, "__properties_version1.0")
        && let Ok((bytes, _)) = cfb.read(p)
    {
        let locator = cfb.locator(p);
        let mut at = header;
        while at + 16 <= bytes.len() {
            let tag = u32le(&bytes, at);
            let (id, kind) = ((tag >> 16) as u16, tag as u16);
            let v = &bytes[at + 8..at + 16];
            let value = match kind {
                0x0002 => Some(Value::Int(i64::from(u16le(v, 0) as i16))),
                0x0003 | 0x000A => Some(Value::Int(i64::from(u32le(v, 0) as i32))),
                0x0014 => Some(Value::Int(u64le(v, 0) as i64)),
                0x000B => Some(Value::Bool(u16le(v, 0) != 0)),
                0x0004 => Some(Value::Float(f64::from(f32::from_bits(u32le(v, 0))))),
                0x0005 => Some(Value::Float(f64::from_bits(u64le(v, 0)))),
                0x0006 => Some(Value::Other(format!(
                    "{:.4}",
                    (u64le(v, 0) as i64) as f64 / 10_000.0
                ))),
                0x0040 => Some(
                    filetime(u64le(v, 0)).map_or(Value::Other("(not set)".into()), Value::Time),
                ),
                _ => None,
            };
            if let Some(value) = value
                && !out.iter().any(|p: &Prop| p.id == id && p.kind == kind)
            {
                out.push(Prop {
                    id,
                    kind,
                    value,
                    source: locator.range(at, 16),
                });
            }
            at += 16;
        }
    }
    out.sort_by_key(|p| p.id);
    out
}

fn guid(b: &[u8]) -> String {
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{}",
        u32le(b, 0),
        u16le(b, 4),
        u16le(b, 6),
        b[8],
        b[9],
        b[10..16]
            .iter()
            .map(|x| format!("{x:02X}"))
            .collect::<String>()
    )
}

fn get(props: &[Prop], id: u16) -> Option<&Prop> {
    props.iter().find(|p| p.id == id)
}
fn text(props: &[Prop], id: u16) -> Option<String> {
    get(props, id)
        .and_then(|p| p.value.text())
        .filter(|s| !s.is_empty())
}

pub struct OutlookMsg;
impl Handler for OutlookMsg {
    fn name(&self) -> &'static str {
        "Outlook message"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        probe.head.starts_with(cfb::MAGIC)
            && (cfb::head_has_name(probe.head, "__substg1.0_")
                || cfb::head_has_name(probe.head, "__properties_version1.0")
                || probe.ext_is(&["msg"]) && !cfb::head_has_name(probe.head, "Workbook"))
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let cfb = Cfb::open(input.bytes)?;
        let props = properties(&cfb, 0, 32);
        if props.is_empty() {
            return Err("The Compound File holds no Outlook message properties".into());
        }
        let mut out = Inspection::new("Microsoft Outlook", "Outlook message (.msg)");
        let class = text(&props, 0x001A).unwrap_or_default();
        if !class.is_empty() {
            out.kind = format!("Outlook item ({class})");
        }
        let fact = |out: &mut Inspection, group: &str, label: &str, id: u16| {
            if let Some(p) = get(&props, id)
                && let Some(v) = p.value.text()
            {
                out.fact(group, label, v, p.source.clone());
            }
        };
        fact(&mut out, "Message", "Subject", 0x0037);
        fact(&mut out, "Message", "Message class", 0x001A);
        fact(&mut out, "Message", "Sent (client submit time)", 0x0039);
        fact(&mut out, "Message", "Delivered", 0x0E06);
        fact(&mut out, "Message", "Created", 0x3007);
        fact(&mut out, "Message", "Message ID", 0x1035);
        fact(&mut out, "Message", "In reply to", 0x1042);
        fact(&mut out, "Message", "Conversation topic", 0x0070);
        fact(&mut out, "Message", "Size (declared)", 0x0E08);
        if let Some(i) = get(&props, 0x0017).and_then(|p| match p.value {
            Value::Int(i) => Some(i),
            _ => None,
        }) {
            let v = match i {
                0 => "low",
                2 => "high",
                _ => "normal",
            };
            out.fact(
                "Message",
                "Importance",
                v,
                get(&props, 0x0017).and_then(|p| p.source.clone()),
            );
        }
        fact(&mut out, "Addresses", "Sender name", 0x0C1A);
        let sender = get(&props, 0x5D01)
            .filter(|p| p.value.text().is_some_and(|s| !s.is_empty()))
            .or_else(|| get(&props, 0x0C1F));
        if let Some(p) = sender
            && let Some(v) = p.value.text()
        {
            out.fact("Addresses", "Sender address", v, p.source.clone());
        }
        fact(&mut out, "Addresses", "Sent on behalf of (name)", 0x0042);
        fact(&mut out, "Addresses", "Display To names", 0x0E04);
        fact(&mut out, "Addresses", "Display Cc names", 0x0E03);
        fact(&mut out, "Addresses", "Display Bcc names", 0x0E02);

        // Transport headers, when the message came through the Internet.
        if let Some(h) = get(&props, 0x007D).and_then(|p| match &p.value {
            Value::Text(s) => Some(s.clone()),
            _ => None,
        }) {
            let (headers, _) = mail::parse_headers(h.as_bytes());
            let received = headers
                .iter()
                .filter(|x| x.name.eq_ignore_ascii_case("Received"))
                .count();
            let src = get(&props, 0x007D).and_then(|p| p.source.clone());
            out.fact(
                "Transport",
                "Received hops",
                received.to_string(),
                src.clone(),
            );
            if let Some(a) = mail::header(&headers, "Authentication-Results") {
                out.fact(
                    "Transport",
                    "Authentication results",
                    clip(&a.value, 300),
                    src.clone(),
                );
            }
            if let Some(r) = mail::header(&headers, "Return-Path") {
                out.fact(
                    "Transport",
                    "Return-Path address",
                    r.value.clone(),
                    src.clone(),
                );
            }
            out.fact(
                "Transport",
                "Transport header fields",
                headers.len().to_string(),
                src,
            );
        }

        // Body.
        let body = text(&props, 0x1000).map(|b| (b, false)).or_else(|| {
            get(&props, 0x1013).map(|p| {
                let html = match &p.value {
                    Value::Text(s) => s.clone(),
                    Value::Binary(b) => String::from_utf8_lossy(b).into_owned(),
                    other => other.summary(),
                };
                (mail::html_to_text(&html), true)
            })
        });
        match body {
            Some((b, html)) => {
                let src =
                    get(&props, if html { 0x1013 } else { 0x1000 }).and_then(|p| p.source.clone());
                out.fact(
                    "Content",
                    "Body",
                    if html {
                        "HTML, shown as plain text (tags removed)"
                    } else {
                        "plain text"
                    },
                    src.clone(),
                );
                let mut t = table("Body text", &["Text"]);
                for line in b.lines().take(20_000) {
                    push_row(&mut t, vec![line.trim_end().to_owned()], src.clone());
                }
                out.tables.push(t);
            }
            None if get(&props, 0x1009).is_some() => out.notes.push(
                "The body is stored only as compressed RTF, which Isolinear does not decompress"
                    .into(),
            ),
            None => {}
        }

        // Recipients and attachments.
        let mut recipients = table(
            "Recipients",
            &["Type", "Display name", "Email address", "Address type"],
        );
        let mut attachments = table(
            "Attachments",
            &["Filename", "MIME type", "Size", "Method", "Content ID"],
        );
        let mut all = table("Properties", &["Scope", "Tag", "Name", "Type", "Value"]);
        let add_props = |scope: &str, list: &[Prop], all: &mut crate::inspect::Table| {
            for p in list {
                push_row(
                    all,
                    vec![
                        scope.to_owned(),
                        format!("0x{:04X}{:04X}", p.id, p.kind),
                        prop_name(p.id).to_owned(),
                        type_name(p.kind).to_owned(),
                        p.value.summary(),
                    ],
                    p.source.clone(),
                );
            }
        };
        add_props("Message", &props, &mut all);
        let mut pictures = 0usize;
        let mut signed = class.to_ascii_uppercase().contains("SMIME");
        let mut embedded = 0usize;
        for child in cfb.children(0) {
            if cancelled() {
                return Err("Cancelled".into());
            }
            let name = cfb.entries[child].name.clone();
            if cfb.entries[child].kind != Kind::Storage {
                continue;
            }
            if name.starts_with("__recip_version1.0_") && recipients.rows.len() < MAX_RECIPIENTS {
                let p = properties(&cfb, child, 8);
                let kind = match get(&p, 0x0C15).map(|x| &x.value) {
                    Some(Value::Int(1)) => "To",
                    Some(Value::Int(2)) => "Cc",
                    Some(Value::Int(3)) => "Bcc",
                    _ => "",
                };
                let address = text(&p, 0x39FE)
                    .or_else(|| text(&p, 0x3003))
                    .unwrap_or_default();
                push_row(
                    &mut recipients,
                    vec![
                        kind.into(),
                        text(&p, 0x3001).unwrap_or_default(),
                        address,
                        text(&p, 0x3002).unwrap_or_default(),
                    ],
                    p.first().and_then(|x| x.source.clone()),
                );
                add_props(
                    &format!("Recipient {}", recipients.rows.len()),
                    &p,
                    &mut all,
                );
            } else if name.starts_with("__attach_version1.0_")
                && attachments.rows.len() < MAX_ATTACHMENTS
            {
                let p = properties(&cfb, child, 8);
                let filename = text(&p, 0x3707)
                    .or_else(|| text(&p, 0x3704))
                    .or_else(|| text(&p, 0x3001))
                    .unwrap_or_default();
                let mime = text(&p, 0x370E).unwrap_or_default();
                let data = get(&p, 0x3701).and_then(|x| match &x.value {
                    Value::Binary(b) => Some(b),
                    _ => None,
                });
                let method = match get(&p, 0x3705).map(|x| &x.value) {
                    Some(Value::Int(1)) => "by value",
                    Some(Value::Int(2..=4)) => "by reference",
                    Some(Value::Int(5)) => "embedded message",
                    Some(Value::Int(6)) => "OLE object",
                    _ => "",
                };
                if method == "embedded message"
                    || cfb.child(child, "__substg1.0_3701000D").is_some()
                {
                    embedded += 1;
                }
                if filename.to_ascii_lowercase().ends_with(".p7m") || mime.contains("pkcs7") {
                    signed = true;
                }
                let size = match data {
                    Some(b) => b.len().to_string(),
                    None => text(&p, 0x0E20)
                        .map(|s| format!("{s} (declared)"))
                        .unwrap_or_default(),
                };
                if let Some(b) = data
                    && pictures < MAX_PICTURES
                    && is_picture(b)
                {
                    pictures += 1;
                    out.visuals.push(Visual::Picture {
                        bytes: b.clone(),
                        caption: filename.clone(),
                    });
                }
                push_row(
                    &mut attachments,
                    vec![
                        filename,
                        mime,
                        size,
                        method.into(),
                        text(&p, 0x3712).unwrap_or_default(),
                    ],
                    get(&p, 0x3701).and_then(|x| x.source.clone()),
                );
                add_props(
                    &format!("Attachment {}", attachments.rows.len()),
                    &p,
                    &mut all,
                );
            }
        }
        out.fact(
            "Content",
            "Recipients",
            recipients.rows.len().to_string(),
            None,
        );
        out.fact(
            "Content",
            "Attachments",
            attachments.rows.len().to_string(),
            None,
        );
        out.fact("Content", "Properties", all.rows.len().to_string(), None);
        if !recipients.rows.is_empty() {
            out.tables.push(recipients);
        }
        if !attachments.rows.is_empty() {
            out.tables.push(attachments);
        }
        out.tables.push(all);
        if embedded > 0 {
            out.notes.push(format!(
                "{embedded} attachments are embedded messages; save them out to inspect them"
            ));
        }
        if cfb.warnings.is_empty() {
            out.check(
                Stage::Parsed,
                State::Passed,
                "Compound File and MAPI properties read",
            );
        } else {
            out.check(
                Stage::Parsed,
                State::Partial,
                format!("The Compound File is damaged: {}", cfb.warnings.join("; ")),
            );
        }
        out.not_validated();
        if signed {
            out.check(
                Stage::Signature,
                State::NotDone,
                "The S/MIME signature was not verified",
            );
        }
        out.notes.push(format!(
            "Container: Compound File version {}, {} directory entries, {}",
            cfb.version,
            cfb.entries.len(),
            human(input.bytes.len() as u64)
        ));
        Ok(out)
    }
}
