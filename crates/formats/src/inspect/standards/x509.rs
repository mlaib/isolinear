//! X.509 certificates (RFC 5280) in DER: identity, validity, public key, extensions and
//! fingerprints. Also the pieces the PEM, CMS and PKCS#12 handlers reuse (certificate
//! summaries, names, public keys, certificate signing requests). Signatures are not verified;
//! URLs in the certificate are shown as text and never fetched.
use super::der::{self, CONTEXT, INTEGER, OCTET_STRING, SEQUENCE, Shape, Tlv};
use super::hash::{colon_hex, sha1, sha256};
use super::util::clip;
use crate::inspect::{Handler, Input, Inspection, Probe, Row, Stage, State, Table};
use std::ops::Range;

/// Certificates given full detail (facts and extension tables) in one file.
pub const DETAILED: usize = 10;

/// What a certificate is, in one row.
#[derive(Clone, Debug, Default)]
pub struct Cert {
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
    pub not_before_secs: Option<i64>,
    pub not_after_secs: Option<i64>,
    pub key: String,
    pub sha1: String,
    pub sha256: String,
    pub sha256_raw: [u8; 32],
    pub sha1_raw: [u8; 20],
    /// Issuer name and serial number contents, and the subject key identifier, for matching
    /// CMS signer identifiers.
    pub issuer_der: Vec<u8>,
    pub serial_der: Vec<u8>,
    pub ski: Option<Vec<u8>>,
    pub range: Option<Range<u64>>,
}

pub const CERT_COLUMNS: [&str; 8] = [
    "#",
    "Subject",
    "Issuer",
    "Serial number",
    "Not before",
    "Not after",
    "Public key",
    "SHA-256 fingerprint",
];

impl Cert {
    pub fn row(&self, n: usize) -> Row {
        Row {
            cells: vec![
                n.to_string(),
                self.subject.clone(),
                self.issuer.clone(),
                self.serial.clone(),
                self.not_before.clone(),
                self.not_after.clone(),
                self.key.clone(),
                self.sha256.clone(),
            ],
            source: self.range.clone(),
        }
    }
}

/// A table of certificate summaries.
pub fn table(name: &str, certs: &[Cert]) -> Table {
    Table {
        name: name.into(),
        columns: CERT_COLUMNS.iter().map(|c| (*c).to_owned()).collect(),
        rows: certs
            .iter()
            .take(crate::inspect::MAX_ROWS)
            .enumerate()
            .map(|(i, c)| c.row(i + 1))
            .collect(),
        total: (certs.len() > crate::inspect::MAX_ROWS).then_some(certs.len() as u64),
    }
}

/// "C=LU, O=Example, CN=host" (attributes in the order of the file).
pub fn name_text(name: &Tlv) -> String {
    let mut parts = vec![];
    for rdn in name.children() {
        let mut same = vec![];
        for atv in rdn.children() {
            let (Some(t), Some(v)) = (atv.child(0), atv.child(1)) else {
                continue;
            };
            let key = t.oid().map(|o| der::oid_label(&o)).unwrap_or_default();
            let value = v.string().unwrap_or_else(|| v.describe());
            same.push(format!("{key}={value}"));
        }
        if !same.is_empty() {
            parts.push(same.join(" + "));
        }
    }
    clip(&parts.join(", "), 1000)
}

/// One attribute of a name ("CN"), if present.
pub fn name_part(name: &Tlv, oid: &str) -> Option<String> {
    name.children()
        .flat_map(|rdn| rdn.kids())
        .find(|atv| atv.child(0).and_then(|t| t.oid()).as_deref() == Some(oid))
        .and_then(|atv| atv.child(1))
        .and_then(|v| v.string())
}

fn bit_length(n: &[u8]) -> usize {
    let n = match n.iter().position(|&b| b != 0) {
        Some(i) => &n[i..],
        None => return 0,
    };
    (n.len() - 1) * 8 + (8 - n[0].leading_zeros() as usize)
}

fn curve_bits(oid: &str) -> Option<u32> {
    Some(match oid {
        "1.2.840.10045.3.1.1" => 192,
        "1.3.132.0.33" => 224,
        "1.2.840.10045.3.1.7" | "1.3.132.0.10" | "1.3.36.3.3.2.8.1.1.7" => 256,
        "1.3.132.0.34" | "1.3.36.3.3.2.8.1.1.11" => 384,
        "1.3.132.0.35" => 521,
        "1.3.36.3.3.2.8.1.1.13" => 512,
        _ => return None,
    })
}

/// (algorithm, size) of a SubjectPublicKeyInfo.
pub fn public_key(spki: &Tlv) -> (String, String) {
    let Some(alg) = spki.child(0) else {
        return (String::new(), String::new());
    };
    let oid = alg.child(0).and_then(|o| o.oid()).unwrap_or_default();
    let params = alg.child(1);
    let key = spki.child(1);
    let name = der::oid_label(&oid);
    match oid.as_str() {
        "1.2.840.113549.1.1.1" | "1.2.840.113549.1.1.10" => {
            let n = key
                .and_then(|k| k.inner())
                .and_then(|seq| seq.child(0))
                .and_then(|n| n.uint_bytes());
            let e = key
                .and_then(|k| k.inner())
                .and_then(|seq| seq.child(1))
                .and_then(|e| e.u64());
            let mut size = n
                .map(|n| format!("{} bits", bit_length(n)))
                .unwrap_or_default();
            if let Some(e) = e {
                size.push_str(&format!(" (exponent {e})"));
            }
            ("RSA".into(), size)
        }
        "1.2.840.10045.2.1" => {
            let curve = params.and_then(|p| p.oid()).unwrap_or_default();
            let bits = curve_bits(&curve)
                .map(|b| format!("{b} bits"))
                .unwrap_or_default();
            (format!("EC, curve {}", der::oid_label(&curve)), bits)
        }
        "1.2.840.10040.4.1" => {
            let p = params.and_then(|p| p.child(0)).and_then(|p| p.uint_bytes());
            (
                "DSA".into(),
                p.map(|p| format!("{} bits", bit_length(p)))
                    .unwrap_or_default(),
            )
        }
        "1.3.101.112" => (name, "256 bits".into()),
        "1.3.101.113" => (name, "456 bits".into()),
        "1.3.101.110" => (name, "255 bits".into()),
        "1.3.101.111" => (name, "448 bits".into()),
        _ => (name, String::new()),
    }
}

fn key_text(spki: &Tlv) -> String {
    let (a, s) = public_key(spki);
    if s.is_empty() { a } else { format!("{a}, {s}") }
}

/// Readable time and its seconds.
fn time(t: Option<Tlv>) -> (String, Option<i64>) {
    match t.and_then(|t| t.time()) {
        Some((s, secs)) => (s, Some(secs)),
        None => (String::new(), None),
    }
}

/// The parts of a certificate, located.
struct Parts<'a> {
    tbs: Tlv<'a>,
    version: u64,
    serial: Option<Tlv<'a>>,
    inner_alg: Option<Tlv<'a>>,
    issuer: Option<Tlv<'a>>,
    validity: Option<Tlv<'a>>,
    subject: Option<Tlv<'a>>,
    spki: Option<Tlv<'a>>,
    extensions: Option<Tlv<'a>>,
    outer_alg: Option<Tlv<'a>>,
}

fn parts<'a>(cert: &Tlv<'a>) -> Result<Parts<'a>, String> {
    let kids = cert.kids_strict()?;
    let tbs = *kids.first().ok_or("empty certificate")?;
    if !tbs.universal(SEQUENCE) || !cert.universal(SEQUENCE) {
        return Err("not a certificate".into());
    }
    let t = tbs.kids_strict()?;
    let mut i = 0;
    let mut version = 1;
    if t.first().is_some_and(|v| v.context(0)) {
        version = t[0].inner().and_then(|v| v.u64()).map_or(1, |v| v + 1);
        i = 1;
    }
    let get = |k: usize| t.get(i + k).copied();
    let extensions = t[i..].iter().find(|e| e.context(3)).and_then(|e| e.inner());
    let serial = get(0).filter(|s| s.universal(INTEGER));
    if serial.is_none() {
        return Err("no serial number".into());
    }
    Ok(Parts {
        tbs,
        version,
        serial,
        inner_alg: get(1),
        issuer: get(2),
        validity: get(3),
        subject: get(4),
        spki: get(5),
        extensions,
        outer_alg: kids.get(1).copied(),
    })
}

fn alg_name(alg: Option<Tlv>) -> String {
    alg.and_then(|a| a.child(0))
        .and_then(|o| o.oid())
        .map(|o| der::oid_label(&o))
        .unwrap_or_default()
}

/// One-row summary of a certificate element.
pub fn summarize(cert: &Tlv) -> Result<Cert, String> {
    let p = parts(cert)?;
    let (nb, nbs) = time(p.validity.and_then(|v| v.child(0)));
    let (na, nas) = time(p.validity.and_then(|v| v.child(1)));
    let digest = sha256(cert.bytes());
    Ok(Cert {
        subject: p.subject.map(|n| name_text(&n)).unwrap_or_default(),
        issuer: p.issuer.map(|n| name_text(&n)).unwrap_or_default(),
        serial: p.serial.and_then(|s| s.int_text()).unwrap_or_default(),
        not_before: nb,
        not_after: na,
        not_before_secs: nbs,
        not_after_secs: nas,
        key: p.spki.map(|k| key_text(&k)).unwrap_or_default(),
        sha1: colon_hex(&sha1(cert.bytes())),
        sha256: colon_hex(&digest),
        sha256_raw: digest,
        sha1_raw: sha1(cert.bytes()),
        issuer_der: p.issuer.map(|n| n.content().to_vec()).unwrap_or_default(),
        serial_der: p.serial.map(|n| n.content().to_vec()).unwrap_or_default(),
        ski: p.extensions.and_then(|e| {
            e.children()
                .find(|x| x.child(0).and_then(|o| o.oid()).as_deref() == Some("2.5.29.14"))
                .and_then(|x| x.kids().into_iter().find(|v| v.universal(OCTET_STRING)))
                .and_then(|v| v.inner())
                .map(|k| k.content().to_vec())
        }),
        range: Some(cert.range()),
    })
}

/// Seconds since the Unix epoch on this computer's clock.
pub fn now() -> Option<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
}

/// Whether `now` falls inside the validity period: (state, explanation).
pub fn validity_status(now: i64, not_before: i64, not_after: i64) -> (State, String) {
    if now < not_before {
        (State::Failed, "Not yet valid".into())
    } else if now > not_after {
        (State::Failed, "Expired".into())
    } else {
        let days = (not_after - now) / 86_400;
        (State::Passed, format!("Valid now ({days} days left)"))
    }
}

/// Detailed facts and the extension table for one certificate, under `group`.
pub fn cert_facts(cert: &Tlv, out: &mut Inspection, group: &str) -> Result<Cert, String> {
    let p = parts(cert)?;
    let summary = summarize(cert)?;
    let g = group;
    out.fact(g, "Version", format!("v{}", p.version), Some(p.tbs.range()));
    out.fact(
        g,
        "Serial number",
        summary.serial.clone(),
        p.serial.map(|s| s.range()),
    );
    out.fact(
        g,
        "Subject",
        summary.subject.clone(),
        p.subject.map(|s| s.range()),
    );
    if let Some(cn) = p.subject.and_then(|s| name_part(&s, "2.5.4.3")) {
        out.fact(g, "Subject common name", cn, None);
    }
    out.fact(
        g,
        "Issuer",
        summary.issuer.clone(),
        p.issuer.map(|s| s.range()),
    );
    if p.subject.map(|s| s.content()) == p.issuer.map(|s| s.content()) {
        out.fact(
            g,
            "Self-issued",
            "Yes (subject and issuer are the same)",
            None,
        );
    }
    out.fact(
        g,
        "Signature algorithm",
        alg_name(p.outer_alg),
        p.outer_alg.map(|a| a.range()),
    );
    let vr = p.validity.map(|v| v.range());
    out.fact(g, "Not before", summary.not_before.clone(), vr.clone());
    out.fact(g, "Not after", summary.not_after.clone(), vr);
    if let (Some(nb), Some(na), Some(now)) =
        (summary.not_before_secs, summary.not_after_secs, now())
    {
        let (state, text) = validity_status(now, nb, na);
        out.fact(
            g,
            "Status",
            format!("{text} (on this computer's clock)"),
            None,
        );
        out.check(
            Stage::Semantic,
            state,
            format!("{g}: validity period compared with this computer's clock — {text}"),
        );
    }
    if let Some(spki) = p.spki {
        let (alg, size) = public_key(&spki);
        out.fact(g, "Public key algorithm", alg, Some(spki.range()));
        out.fact(g, "Public key size", size, None);
    }
    out.fact(
        g,
        "SHA-256 fingerprint",
        summary.sha256.clone(),
        Some(cert.range()),
    );
    out.fact(
        g,
        "SHA-1 fingerprint",
        summary.sha1.clone(),
        Some(cert.range()),
    );
    let same = p.inner_alg.map(|a| a.content()) == p.outer_alg.map(|a| a.content());
    out.check(
        Stage::Semantic,
        if same { State::Passed } else { State::Failed },
        format!(
            "{g}: the signature algorithm inside the signed part {} the one outside",
            if same { "matches" } else { "differs from" }
        ),
    );
    if let Some(exts) = p.extensions {
        let mut table = Table {
            name: if group == "Certificate" {
                "Extensions".into()
            } else {
                format!("Extensions ({group})")
            },
            columns: vec!["Extension".into(), "Critical".into(), "Value".into()],
            ..Table::default()
        };
        for ext in exts.children() {
            let Some(oid) = ext.child(0).and_then(|o| o.oid()) else {
                continue;
            };
            let kids = ext.kids();
            let critical = kids.get(1).and_then(|c| c.boolean()).unwrap_or(false);
            let value = kids.iter().find(|k| k.universal(OCTET_STRING));
            let text = value.map(|v| extension_value(&oid, v)).unwrap_or_default();
            if let Some(label) = extension_label(&oid) {
                out.fact(g, label, text.clone(), Some(ext.range()));
            }
            table.rows.push(Row {
                cells: vec![
                    der::oid_label(&oid),
                    if critical { "yes" } else { "no" }.into(),
                    text,
                ],
                source: Some(ext.range()),
            });
        }
        if !table.rows.is_empty() {
            out.tables.push(table);
        }
    }
    Ok(summary)
}

fn extension_label(oid: &str) -> Option<&'static str> {
    Some(match oid {
        "2.5.29.19" => "Basic constraints",
        "2.5.29.15" => "Key usage",
        "2.5.29.37" => "Extended key usage",
        "2.5.29.17" => "Subject alternative names",
        "2.5.29.14" => "Subject key identifier",
        "2.5.29.35" => "Authority key identifier",
        "2.5.29.31" => "CRL distribution points",
        "1.3.6.1.5.5.7.1.1" => "Authority information access",
        "2.5.29.32" => "Certificate policies",
        _ => return None,
    })
}

const KEY_USAGE: [&str; 9] = [
    "digital signature",
    "non-repudiation",
    "key encipherment",
    "data encipherment",
    "key agreement",
    "certificate signing",
    "CRL signing",
    "encipher only",
    "decipher only",
];

/// A GeneralName as text: "DNS:example.org", "IP:192.0.2.1", "URI:http://…".
pub fn general_name(n: &Tlv) -> String {
    if n.class != CONTEXT {
        return n.describe();
    }
    let c = n.content();
    let text = || super::util::printable(&String::from_utf8_lossy(c));
    match n.tag {
        0 => {
            let oid = n.child(0).and_then(|o| o.oid()).unwrap_or_default();
            let value = n
                .child(1)
                .and_then(|v| v.inner())
                .and_then(|v| v.string())
                .unwrap_or_default();
            match oid.as_str() {
                "1.3.6.1.4.1.311.20.2.3" => format!("UPN:{value}"),
                _ => format!("otherName:{oid} {value}").trim_end().to_owned(),
            }
        }
        1 => format!("email:{}", text()),
        2 => format!("DNS:{}", text()),
        4 => format!(
            "DirName:{}",
            n.inner().map(|i| name_text(&i)).unwrap_or_default()
        ),
        6 => format!("URI:{}", text()),
        7 => match c.len() {
            4 => format!("IP:{}", std::net::Ipv4Addr::new(c[0], c[1], c[2], c[3])),
            16 => {
                let mut a = [0u8; 16];
                a.copy_from_slice(c);
                format!("IP:{}", std::net::Ipv6Addr::from(a))
            }
            _ => format!("IP:{}", super::hash::hex(c)),
        },
        8 => {
            let mut fake = vec![0x06, c.len().min(127) as u8];
            fake.extend_from_slice(&c[..c.len().min(127)]);
            let oid = der::root(&fake)
                .ok()
                .and_then(|t| t.oid())
                .unwrap_or_default();
            format!("RID:{oid}")
        }
        t => format!("[{t}] {} bytes", c.len()),
    }
}

/// Every GeneralName found under `t` (bounded depth), as text.
fn names_under(t: &Tlv, depth: usize, out: &mut Vec<String>) {
    if depth > 8 || out.len() > 200 {
        return;
    }
    if t.class == CONTEXT && matches!(t.tag, 1 | 2 | 6 | 7) && !t.constructed {
        out.push(general_name(t));
        return;
    }
    if t.constructed {
        for c in t.children() {
            names_under(&c, depth + 1, out);
        }
    }
}

fn extension_value(oid: &str, value: &Tlv) -> String {
    let Some(inner) = value.inner() else {
        return format!("{} bytes", value.len);
    };
    let kids = inner.kids();
    let r = match oid {
        "2.5.29.19" => {
            let ca = kids.iter().find_map(|k| k.boolean()).unwrap_or(false);
            let path = kids
                .iter()
                .find(|k| k.universal(INTEGER))
                .and_then(|k| k.u64());
            let mut s = format!("CA: {}", if ca { "yes" } else { "no" });
            if let Some(p) = path {
                s.push_str(&format!(", path length {p}"));
            }
            s
        }
        "2.5.29.15" => match inner.bits() {
            Some((_, bytes)) => KEY_USAGE
                .iter()
                .enumerate()
                .filter(|(i, _)| bytes.get(i / 8).is_some_and(|b| b & (0x80 >> (i % 8)) != 0))
                .map(|(_, n)| *n)
                .collect::<Vec<_>>()
                .join(", "),
            None => String::new(),
        },
        "2.5.29.37" | "2.5.29.32" => kids
            .iter()
            .filter_map(|k| {
                if k.universal(SEQUENCE) {
                    k.child(0).and_then(|o| o.oid())
                } else {
                    k.oid()
                }
            })
            .map(|o| der::oid_label(&o))
            .collect::<Vec<_>>()
            .join(", "),
        "2.5.29.17" | "2.5.29.18" => kids.iter().map(general_name).collect::<Vec<_>>().join(", "),
        "2.5.29.14" => colon_hex(inner.content()),
        "2.5.29.35" => {
            let mut parts = vec![];
            for k in &kids {
                match k.tag {
                    0 if k.class == CONTEXT => {
                        parts.push(format!("key ID {}", colon_hex(k.content())))
                    }
                    1 if k.class == CONTEXT => {
                        let mut names = vec![];
                        names_under(k, 0, &mut names);
                        for c in k.children() {
                            if c.context(4) {
                                names.push(general_name(&c));
                            }
                        }
                        parts.push(format!("issuer {}", names.join(", ")));
                    }
                    2 if k.class == CONTEXT => {
                        parts.push(format!("serial {}", colon_hex(k.content()).to_lowercase()))
                    }
                    _ => {}
                }
            }
            parts.join("; ")
        }
        "2.5.29.31" | "2.5.29.46" => {
            let mut names = vec![];
            names_under(&inner, 0, &mut names);
            names.join(", ")
        }
        "1.3.6.1.5.5.7.1.1" | "1.3.6.1.5.5.7.1.11" => kids
            .iter()
            .filter_map(|ad| {
                let method = ad.child(0)?.oid()?;
                let loc = ad.child(1)?;
                Some(format!(
                    "{} — {}",
                    der::oid_label(&method),
                    general_name(&loc)
                ))
            })
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    };
    if r.is_empty() {
        format!("{} bytes", value.len)
    } else {
        clip(&r, 4000)
    }
}

/// Basic fields of a PKCS#10 certificate signing request.
pub fn csr_facts(top: &Tlv, out: &mut Inspection, group: &str) -> Result<(), String> {
    let info = top
        .child(0)
        .filter(|i| i.universal(SEQUENCE))
        .ok_or("no request info")?;
    let k = info.kids_strict()?;
    let version = k.first().and_then(|v| v.u64()).ok_or("no version")?;
    out.fact(
        group,
        "Version",
        format!("v{}", version + 1),
        Some(info.range()),
    );
    if let Some(subject) = k.get(1) {
        out.fact(group, "Subject", name_text(subject), Some(subject.range()));
    }
    if let Some(spki) = k.get(2) {
        let (alg, size) = public_key(spki);
        out.fact(group, "Public key algorithm", alg, Some(spki.range()));
        out.fact(group, "Public key size", size, None);
    }
    if let Some(attrs) = k.get(3).filter(|a| a.context(0)) {
        for attr in attrs.children() {
            let Some(oid) = attr.child(0).and_then(|o| o.oid()) else {
                continue;
            };
            let values = attr.child(1);
            if oid == "1.2.840.113549.1.9.14" {
                // extensionRequest: the requested extensions, read like a certificate's.
                let Some(exts) = values.and_then(|v| v.child(0)) else {
                    continue;
                };
                for ext in exts.children() {
                    let Some(eo) = ext.child(0).and_then(|o| o.oid()) else {
                        continue;
                    };
                    let v = ext.kids().into_iter().find(|x| x.universal(OCTET_STRING));
                    let text = v.map(|v| extension_value(&eo, &v)).unwrap_or_default();
                    let label = format!("Requested {}", der::oid_label(&eo));
                    out.fact(group, &label, text, Some(ext.range()));
                }
            } else {
                out.fact(group, &der::oid_label(&oid), "present", Some(attr.range()));
            }
        }
    }
    let alg = top.child(1);
    out.fact(
        group,
        "Signature algorithm",
        alg_name(alg),
        alg.map(|a| a.range()),
    );
    Ok(())
}

/// X.509 certificates in DER (`.der`, `.cer`, `.crt`).
pub struct X509;
impl Handler for X509 {
    fn name(&self) -> &'static str {
        "X.509 certificate"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        der::shape(probe.head) == Some(Shape::Certificate)
            && der::header(probe.head, 0)
                .is_ok_and(|(_, _, _, h, l)| (h as u64).saturating_add(l as u64) <= probe.size)
    }
    fn inspect(&self, input: &Input, _cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let top = der::root(input.bytes).map_err(|e| format!("Not readable as DER: {e}"))?;
        let mut out = Inspection::new("X.509", "Certificate (DER)");
        cert_facts(&top, &mut out, "Certificate")?;
        out.version = out
            .facts
            .iter()
            .find(|f| f.label == "Version")
            .map(|f| f.value.clone());
        der::outline(&top, 0, &mut out.outline);
        finish(&mut out, der::scan_errors(&top, 0));
        out.check(
            Stage::Signature,
            State::NotDone,
            "The certificate's signature was not verified",
        );
        Ok(out)
    }
}

/// Parsed check, schema not done, and the notes every certificate view carries.
pub fn finish(out: &mut Inspection, error: Option<String>) {
    match error {
        None => out.check(
            Stage::Parsed,
            State::Passed,
            "Read as DER; the fields shown were decoded",
        ),
        Some(e) => out.check(Stage::Parsed, State::Partial, format!("Damaged: {e}")),
    }
    out.not_validated();
    out.notes.push(NOTE.into());
}

pub const NOTE: &str = "Signatures and certificate chains are not verified. Validity is compared \
with this computer's clock. Addresses in the certificate (CRL, OCSP, CA issuers) are shown as \
text and never contacted.";

/// Issuer, update times and revoked count of a certificate revocation list; returns a summary.
pub fn crl_facts(top: &Tlv, out: &mut Inspection, group: &str) -> String {
    let Some(tbs) = top.child(0).filter(|t| t.universal(SEQUENCE)) else {
        return "not readable".into();
    };
    let k = tbs.kids();
    let i = usize::from(k.first().is_some_and(|v| v.universal(INTEGER)));
    let issuer = k.get(i + 1).map(|n| name_text(n)).unwrap_or_default();
    let this_update = k
        .get(i + 2)
        .and_then(|t| t.time())
        .map(|t| t.0)
        .unwrap_or_default();
    let next_update = k
        .get(i + 3)
        .and_then(|t| t.time())
        .map(|t| t.0)
        .unwrap_or_default();
    let revoked = k
        .iter()
        .skip(i + 3)
        .find(|r| r.universal(SEQUENCE))
        .map(|r| r.children().count())
        .unwrap_or(0);
    out.fact(group, "Issuer", issuer.clone(), Some(tbs.range()));
    out.fact(group, "This update", this_update, None);
    out.fact(group, "Next update", next_update, None);
    out.fact(group, "Revoked certificates", revoked.to_string(), None);
    out.fact(group, "Signature algorithm", alg_name(top.child(1)), None);
    format!("CRL by {issuer}, {revoked} revoked")
}
