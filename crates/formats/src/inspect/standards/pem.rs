//! PEM files (RFC 7468): every `-----BEGIN …-----` block listed with its type and size;
//! certificates, requests, public keys, CRLs and CMS decoded with the DER readers. Private keys
//! are shown only as "a private key of this type": their content is never displayed.
use super::der::{self, Tlv};
use super::util::{base64, bytes_text, span};
use super::{cms, x509};
use crate::inspect::{Handler, Input, Inspection, Probe, Row, Stage, State, Table};

pub struct Pem;

/// One block found in the text.
struct Block {
    label: String,
    /// The whole block, BEGIN line to END line.
    start: usize,
    end: usize,
    headers: Vec<(String, String)>,
    der: Option<Vec<u8>>,
    complete: bool,
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let rest = hay.get(from..)?;
    memchr::memmem::find(rest, needle).map(|i| i + from)
}

/// The label after "-----BEGIN " at `at`, when the line is a PEM boundary.
fn label_at(b: &[u8], at: usize) -> Option<(String, usize)> {
    let start = at + 11;
    let rest = b.get(start..)?;
    let close = memchr::memmem::find(&rest[..rest.len().min(100)], b"-----")?;
    let label = std::str::from_utf8(&rest[..close]).ok()?;
    if !label
        .bytes()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b' ' || c == b'-')
    {
        return None;
    }
    Some((label.to_owned(), start + close + 5))
}

fn blocks(b: &[u8]) -> Vec<Block> {
    let mut out = vec![];
    let mut pos = 0;
    while let Some(at) = find(b, b"-----BEGIN ", pos) {
        if out.len() >= crate::inspect::MAX_ROWS {
            break;
        }
        let Some((label, body_start)) = label_at(b, at) else {
            pos = at + 11;
            continue;
        };
        let end_marker = format!("-----END {label}-----");
        let (body_end, end, complete) = match find(b, end_marker.as_bytes(), body_start) {
            Some(e) => (e, e + end_marker.len(), true),
            None => {
                // Cut file: the body runs to the next BEGIN or the end.
                let e = find(b, b"-----BEGIN ", body_start).unwrap_or(b.len());
                (e, e, false)
            }
        };
        let body = &b[body_start..body_end];
        let mut headers = vec![];
        let mut data = Vec::with_capacity(body.len());
        for line in body.split(|&c| c == b'\n') {
            let text = String::from_utf8_lossy(line);
            let t = text.trim();
            if let Some((k, v)) = t.split_once(':')
                && data.is_empty()
                && !k.contains(' ')
            {
                headers.push((k.to_owned(), v.trim().to_owned()));
                continue;
            }
            data.extend_from_slice(t.as_bytes());
        }
        out.push(Block {
            label,
            start: at,
            end,
            headers,
            der: base64(&data, false),
            complete,
        });
        pos = end.max(at + 11);
    }
    out
}

fn is_private(label: &str) -> bool {
    label.contains("PRIVATE KEY")
}

impl Handler for Pem {
    fn name(&self) -> &'static str {
        "PEM"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let h = probe.head;
        let mut pos = 0;
        while let Some(at) = find(h, b"-----BEGIN ", pos) {
            let line_start = at == 0 || h[at - 1] == b'\n' || h[at - 1] == b'\r';
            if line_start && label_at(h, at).is_some() {
                return true;
            }
            pos = at + 11;
        }
        false
    }
    fn inspect(&self, input: &Input, _cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let found = blocks(input.bytes);
        if found.is_empty() {
            return Err("No PEM block found".into());
        }
        let mut out = Inspection::new("PEM", "PEM file");
        let mut table = Table {
            name: "Blocks".into(),
            columns: ["#", "Type", "Size", "Protection", "Summary"]
                .iter()
                .map(|c| (*c).to_owned())
                .collect(),
            ..Table::default()
        };
        let mut certs = vec![];
        let mut detailed = 0;
        let mut damaged = 0;
        let mut privates = 0;
        let count = |l: &str| found.iter().filter(|b| b.label == l).count();
        let single =
            count("CERTIFICATE") + count("TRUSTED CERTIFICATE") + count("X509 CERTIFICATE") == 1;
        for (i, block) in found.iter().enumerate() {
            let n = i + 1;
            let src = span(block.start, block.end - block.start);
            let encrypted = block
                .headers
                .iter()
                .any(|(k, v)| k == "Proc-Type" && v.contains("ENCRYPTED"))
                || block.label == "ENCRYPTED PRIVATE KEY";
            let cipher = block
                .headers
                .iter()
                .find(|(k, _)| k == "DEK-Info")
                .map(|(_, v)| v.split(',').next().unwrap_or("").to_owned());
            let protection = match (&cipher, encrypted) {
                (Some(c), _) => format!("encrypted ({c})"),
                (None, true) => "encrypted".into(),
                _ => String::new(),
            };
            let size = block
                .der
                .as_ref()
                .map(|d| bytes_text(d.len() as u64))
                .unwrap_or_else(|| "not valid base64".into());
            if !block.complete || block.der.is_none() {
                damaged += 1;
            }
            let parsed = block
                .der
                .as_deref()
                .and_then(|d| der::parse(d, 0, d.len()).ok());
            let summary = if is_private(&block.label) {
                privates += 1;
                let kind = match (&parsed, encrypted && cipher.is_some()) {
                    // Only the structure's kind (algorithm identifier); never the content.
                    (Some(t), false) => der::private_key_kind(t),
                    _ => "Private key".into(),
                };
                format!("{kind} — private key, not shown")
            } else {
                match parsed {
                    Some(t) => {
                        // Sources inside the decoded DER are not file offsets: point at the block.
                        let (facts, tables, known) =
                            (out.facts.len(), out.tables.len(), certs.len());
                        let s = describe(
                            &block.label,
                            &t,
                            n,
                            &mut out,
                            &mut certs,
                            &mut detailed,
                            single,
                        );
                        for f in &mut out.facts[facts..] {
                            f.source = src.clone();
                        }
                        for t in &mut out.tables[tables..] {
                            for r in &mut t.rows {
                                r.source = src.clone();
                            }
                        }
                        for c in &mut certs[known..] {
                            c.range = src.clone();
                        }
                        s
                    }
                    None if block.label.starts_with("OPENSSH")
                        || block.label.starts_with("SSH2") =>
                    {
                        String::new()
                    }
                    None => "not readable".into(),
                }
            };
            table.rows.push(Row {
                cells: vec![
                    n.to_string(),
                    block.label.clone(),
                    size,
                    protection,
                    summary,
                ],
                source: src,
            });
        }
        out.kind = match (found.len(), found[0].label.as_str()) {
            (1, "CERTIFICATE") => "Certificate (PEM)".into(),
            (_, _) if count("CERTIFICATE") == found.len() => {
                format!(
                    "Certificate chain or bundle ({} certificates, PEM)",
                    found.len()
                )
            }
            (1, "CERTIFICATE REQUEST") | (1, "NEW CERTIFICATE REQUEST") => {
                "Certificate signing request (PEM)".into()
            }
            (1, "PUBLIC KEY") | (1, "RSA PUBLIC KEY") => "Public key (PEM)".into(),
            (1, l) if is_private(l) => "Private key (PEM)".into(),
            (1, "X509 CRL") => "Certificate revocation list (PEM)".into(),
            (1, "PKCS7") | (1, "CMS") => "CMS / PKCS#7 (PEM)".into(),
            (1, l) => format!("{} (PEM)", l.to_lowercase()),
            (n, _) => format!("PEM file with {n} blocks"),
        };
        out.fact("File", "Blocks", found.len().to_string(), None);
        if !certs.is_empty() {
            out.fact("File", "Certificates", certs.len().to_string(), None);
        }
        if privates > 0 {
            out.fact("File", "Private keys", privates.to_string(), None);
            out.notes.push(der::PRIVATE_NOTE.into());
        }
        out.tables.insert(0, table);
        if certs.len() > 1 {
            out.tables.push(x509::table("Certificates", &certs));
        }
        if damaged == 0 {
            out.check(Stage::Parsed, State::Passed, "Every block was read");
        } else {
            out.check(
                Stage::Parsed,
                State::Partial,
                format!("{damaged} block(s) are cut or not valid base64"),
            );
        }
        out.not_validated();
        let signed = found.iter().any(|b| {
            matches!(
                b.label.as_str(),
                "CERTIFICATE" | "CERTIFICATE REQUEST" | "X509 CRL" | "PKCS7" | "CMS"
            )
        });
        if signed && !out.checks.iter().any(|c| c.stage == Stage::Signature) {
            out.check(
                Stage::Signature,
                State::NotDone,
                "Signatures were not verified",
            );
        }
        out.notes.push(x509::NOTE.into());
        Ok(out)
    }
}

/// Facts for a non-private block; returns its one-line summary.
fn describe(
    label: &str,
    t: &Tlv,
    n: usize,
    out: &mut Inspection,
    certs: &mut Vec<x509::Cert>,
    detailed: &mut usize,
    single: bool,
) -> String {
    match label {
        "CERTIFICATE" | "X509 CERTIFICATE" | "TRUSTED CERTIFICATE" => {
            let group = if single {
                "Certificate".to_owned()
            } else {
                format!("Certificate {}", certs.len() + 1)
            };
            let summary = if *detailed < x509::DETAILED {
                *detailed += 1;
                x509::cert_facts(t, out, &group)
            } else {
                x509::summarize(t)
            };
            match summary {
                Ok(s) => {
                    let text = format!("{} (issued by {})", s.subject, s.issuer);
                    certs.push(s);
                    text
                }
                Err(e) => format!("not readable: {e}"),
            }
        }
        "CERTIFICATE REQUEST" | "NEW CERTIFICATE REQUEST" => {
            let group = format!("Request {n}");
            match x509::csr_facts(t, out, &group) {
                Ok(()) => {
                    out.check(
                        Stage::Signature,
                        State::NotDone,
                        format!("{group}: the request's signature was not verified"),
                    );
                    out.facts
                        .iter()
                        .find(|f| f.group == group && f.label == "Subject")
                        .map(|f| f.value.clone())
                        .unwrap_or_default()
                }
                Err(e) => format!("not readable: {e}"),
            }
        }
        "PUBLIC KEY" => {
            let (alg, size) = x509::public_key(t);
            let g = format!("Public key {n}");
            out.fact(&g, "Algorithm", alg.clone(), None);
            out.fact(&g, "Size", size.clone(), None);
            format!("{alg} {size}").trim().to_owned()
        }
        "RSA PUBLIC KEY" => {
            let bits = t
                .child(0)
                .and_then(|n| n.uint_bytes())
                .map(|n| {
                    let lead = n.iter().position(|&b| b != 0).unwrap_or(n.len());
                    let n = &n[lead..];
                    n.first()
                        .map(|f| (n.len() - 1) * 8 + (8 - f.leading_zeros() as usize))
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            let g = format!("Public key {n}");
            out.fact(&g, "Algorithm", "RSA (PKCS#1)", None);
            out.fact(&g, "Size", format!("{bits} bits"), None);
            format!("RSA {bits} bits")
        }
        "X509 CRL" => x509::crl_facts(t, out, &format!("CRL {n}")),
        "PKCS7" | "CMS" => {
            let _ = cms::content_info(t, out, false);
            "CMS message".into()
        }
        "EC PARAMETERS" => t.oid().map(|o| der::oid_label(&o)).unwrap_or_default(),
        _ => String::new(),
    }
}
