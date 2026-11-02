//! CMS / PKCS#7 (RFC 5652), including CAdES signatures: content type, encapsulated or detached
//! content, certificates, signers with their signed and unsigned attributes, time-stamp tokens.
//! Digests that can be compared from the file alone are compared (message digest against the
//! encapsulated content, signing-certificate hash against the certificate, time-stamp imprint
//! against the signature value). Signatures themselves are never verified.
use super::der::{self, CONTEXT, OCTET_STRING, SEQUENCE, SET, Shape, Tlv};
use super::hash::{colon_hex, hex, sha1, sha256};
use super::util::{bytes_text, clip};
use super::x509::{self, Cert};
use crate::inspect::{Handler, Input, Inspection, Probe, Row, Stage, State, Table};

pub struct Cms;
impl Handler for Cms {
    fn name(&self) -> &'static str {
        "CMS / PKCS#7"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        der::shape(probe.head) == Some(Shape::Cms)
            && der::header(probe.head, 0)
                .is_ok_and(|(_, _, _, h, l)| (h as u64).saturating_add(l as u64) <= probe.size)
    }
    fn inspect(&self, input: &Input, _cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let top = der::root(input.bytes).map_err(|e| format!("Not readable as DER: {e}"))?;
        let mut out = Inspection::new("CMS / PKCS#7", "Cryptographic message");
        content_info(&top, &mut out, true)?;
        der::outline(&top, 0, &mut out.outline);
        x509::finish(&mut out, der::scan_errors(&top, 0));
        if !out.checks.iter().any(|c| c.stage == Stage::Signature) {
            out.check(
                Stage::Signature,
                State::NotDone,
                "Signatures were not verified",
            );
        }
        Ok(out)
    }
}

fn alg(t: Option<&Tlv>) -> (String, String) {
    let oid = t
        .and_then(|a| a.child(0))
        .and_then(|o| o.oid())
        .unwrap_or_default();
    (der::oid_label(&oid), oid)
}

/// A ContentInfo: sets the kind and adds what its content holds.
pub fn content_info(top: &Tlv, out: &mut Inspection, set_kind: bool) -> Result<(), String> {
    let oid = top
        .child(0)
        .and_then(|o| o.oid())
        .ok_or("no content type")?;
    let content = top
        .child(1)
        .filter(|c| c.context(0))
        .and_then(|c| c.inner());
    out.fact(
        "Message",
        "Content type",
        der::oid_label(&oid),
        top.child(0).map(|o| o.range()),
    );
    match (oid.as_str(), content) {
        ("1.2.840.113549.1.7.2", Some(sd)) => {
            let kind = signed_data(&sd, out)?;
            if set_kind {
                out.kind = kind;
            }
        }
        ("1.2.840.113549.1.7.3", Some(ed)) => {
            if set_kind {
                out.kind = "Enveloped (encrypted) data".into();
            }
            enveloped(&ed, out);
        }
        ("1.2.840.113549.1.7.1", Some(d)) => {
            if set_kind {
                out.kind = "Data".into();
            }
            out.fact("Message", "Data", bytes_text(d.len as u64), Some(d.range()));
        }
        _ => {
            if set_kind {
                out.kind = der::oid_label(&oid);
            }
            out.check(
                Stage::Parsed,
                State::Partial,
                "Only the content type of this message is shown",
            );
        }
    }
    Ok(())
}

fn digest_of(alg_oid: &str, data: &[u8]) -> Option<Vec<u8>> {
    match alg_oid {
        "2.16.840.1.101.3.4.2.1" => Some(sha256(data).to_vec()),
        "1.3.14.3.2.26" => Some(sha1(data).to_vec()),
        _ => None,
    }
}

/// SignedData: returns the kind ("CAdES / CMS signature", "Certificates only").
fn signed_data(sd: &Tlv, out: &mut Inspection) -> Result<String, String> {
    let kids = sd.kids_strict()?;
    let version = kids.first().and_then(|v| v.u64()).ok_or("no version")?;
    out.fact(
        "Signed data",
        "Version",
        version.to_string(),
        kids.first().map(|v| v.range()),
    );
    let digests: Vec<String> = kids
        .get(1)
        .map(|s| s.children().map(|a| alg(Some(&a)).0).collect())
        .unwrap_or_default();
    out.fact(
        "Signed data",
        "Digest algorithms",
        digests.join(", "),
        kids.get(1).map(|s| s.range()),
    );
    let encap = kids.get(2).filter(|e| e.universal(SEQUENCE));
    let e_type = encap
        .and_then(|e| e.child(0))
        .and_then(|o| o.oid())
        .unwrap_or_default();
    let e_content = encap
        .and_then(|e| e.child(1))
        .filter(|c| c.context(0))
        .and_then(|c| c.inner())
        .filter(|o| o.universal(OCTET_STRING));
    out.fact(
        "Signed data",
        "Encapsulated content type",
        der::oid_label(&e_type),
        encap.map(|e| e.range()),
    );
    out.fact(
        "Signed data",
        "Content",
        match &e_content {
            Some(c) => format!("present in the file ({})", bytes_text(c.len as u64)),
            None => "detached (not in this file)".into(),
        },
        e_content.map(|c| c.range()),
    );
    // Certificates ([0]) and CRLs ([1]), then the signer infos (the last SET).
    let mut certs: Vec<Cert> = vec![];
    let mut cert_tlvs = vec![];
    let mut crls = 0usize;
    for k in kids.iter().skip(3) {
        if k.context(0) {
            for c in k.children() {
                if c.universal(SEQUENCE)
                    && let Ok(s) = x509::summarize(&c)
                {
                    certs.push(s);
                    cert_tlvs.push(c);
                }
            }
        } else if k.context(1) {
            crls += k.children().count();
        }
    }
    let signers = kids.iter().skip(3).rev().find(|k| k.universal(SET));
    out.fact("Signed data", "Certificates", certs.len().to_string(), None);
    if crls > 0 {
        out.fact("Signed data", "CRLs", crls.to_string(), None);
    }
    for (i, c) in cert_tlvs.iter().enumerate().take(x509::DETAILED) {
        let _ = x509::cert_facts(c, out, &format!("Certificate {}", i + 1));
    }
    if !certs.is_empty() {
        out.tables.push(x509::table("Certificates", &certs));
    }
    let mut table = Table {
        name: "Signers".into(),
        columns: [
            "#",
            "Identified by",
            "Certificate",
            "Digest",
            "Signature algorithm",
            "Signing time",
            "Time-stamp",
        ]
        .iter()
        .map(|c| (*c).to_owned())
        .collect(),
        ..Table::default()
    };
    let mut count = 0;
    for (n, si) in signers
        .map(|s| s.kids())
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        count += 1;
        let row = signer(si, n + 1, &certs, &e_type, e_content.as_ref(), out);
        table.rows.push(row);
    }
    out.fact(
        "Signed data",
        "Signers",
        count.to_string(),
        signers.map(|s| s.range()),
    );
    if !table.rows.is_empty() {
        out.tables.push(table);
    }
    if count > 0 {
        out.check(
            Stage::Signature,
            State::NotDone,
            "The signature was not verified (nor the certificate chain or revocation)",
        );
    }
    Ok(if count == 0 {
        "Certificates only (degenerate signed data)".into()
    } else if e_type == "1.3.6.1.4.1.311.2.1.4" {
        "Authenticode signature".into()
    } else if e_type == "1.2.840.113549.1.9.16.1.4" {
        "Time-stamp token (RFC 3161)".into()
    } else if e_content.is_none() {
        "Signature (detached content)".into()
    } else {
        "Signed data (content enveloped in the signature)".into()
    })
}

/// One SignerInfo: facts under "Signer n", checks, and a row.
fn signer(
    si: &Tlv,
    n: usize,
    certs: &[Cert],
    e_type: &str,
    e_content: Option<&Tlv>,
    out: &mut Inspection,
) -> Row {
    let g = format!("Signer {n}");
    let kids = si.kids();
    let mut i = 1;
    let sid = kids.get(1);
    let (who, cert) = match sid {
        Some(s) if s.universal(SEQUENCE) => {
            let issuer = s.child(0);
            let serial = s.child(1);
            let text = format!(
                "issuer {}, serial {}",
                issuer.map(|n| x509::name_text(&n)).unwrap_or_default(),
                serial.and_then(|s| s.int_text()).unwrap_or_default()
            );
            let found = certs.iter().position(|c| {
                Some(c.issuer_der.as_slice()) == issuer.map(|n| n.content())
                    && Some(c.serial_der.as_slice()) == serial.map(|s| s.content())
            });
            (text, found)
        }
        Some(s) if s.context(0) => {
            let ski = s.content();
            let found = certs.iter().position(|c| c.ski.as_deref() == Some(ski));
            (format!("subject key identifier {}", colon_hex(ski)), found)
        }
        _ => (String::new(), None),
    };
    i += 1;
    out.fact(&g, "Identified by", who.clone(), sid.map(|s| s.range()));
    let cert_text = cert
        .map(|c| format!("certificate {}: {}", c + 1, certs[c].subject))
        .unwrap_or_else(|| "not included in the file".into());
    out.fact(&g, "Signer certificate", cert_text.clone(), None);
    let digest = kids.get(i);
    let (digest_name, digest_oid) = alg(digest);
    out.fact(
        &g,
        "Digest algorithm",
        digest_name.clone(),
        digest.map(|d| d.range()),
    );
    i += 1;
    let signed = kids.get(i).filter(|k| k.context(0)).copied();
    if signed.is_some() {
        i += 1;
    }
    let sig_alg = kids.get(i);
    let (sig_name, _) = alg(sig_alg);
    out.fact(
        &g,
        "Signature algorithm",
        sig_name.clone(),
        sig_alg.map(|d| d.range()),
    );
    let signature = kids
        .get(i + 1)
        .filter(|s| s.universal(OCTET_STRING))
        .copied();
    if let Some(s) = &signature {
        out.fact(
            &g,
            "Signature value",
            bytes_text(s.len as u64),
            Some(s.range()),
        );
    }
    let unsigned = kids.iter().skip(i + 2).find(|k| k.context(1)).copied();
    let mut signing_time = String::new();
    let mut stamp = String::new();
    if let Some(attrs) = signed {
        out.fact(
            &g,
            "Signed attributes",
            attrs.children().count().to_string(),
            Some(attrs.range()),
        );
        for attr in attrs.children() {
            let Some(oid) = attr.child(0).and_then(|o| o.oid()) else {
                continue;
            };
            let value = attr.child(1).and_then(|s| s.child(0));
            let src = Some(attr.range());
            match oid.as_str() {
                "1.2.840.113549.1.9.3" => {
                    let ct = value.and_then(|v| v.oid()).unwrap_or_default();
                    out.fact(&g, "Content type (signed)", der::oid_label(&ct), src);
                    let same = ct == e_type;
                    out.check(
                        Stage::Semantic,
                        if same { State::Passed } else { State::Failed },
                        format!(
                            "{g}: the signed content type {} the encapsulated content type",
                            if same { "matches" } else { "differs from" }
                        ),
                    );
                }
                "1.2.840.113549.1.9.4" => {
                    let md = value.map(|v| v.content()).unwrap_or_default();
                    out.fact(&g, "Message digest", hex(md), src);
                    match e_content.map(|c| digest_of(&digest_oid, c.content())) {
                        Some(Some(d)) => {
                            let same = d == md;
                            out.check(
                                Stage::Semantic,
                                if same { State::Passed } else { State::Failed },
                                format!(
                                    "{g}: the message digest {} the encapsulated content",
                                    if same { "matches" } else { "does not match" }
                                ),
                            );
                        }
                        Some(None) => out.check(
                            Stage::Semantic,
                            State::NotDone,
                            format!("{g}: message digest not compared ({digest_name} is not implemented)"),
                        ),
                        None => out.check(
                            Stage::Semantic,
                            State::NotDone,
                            format!("{g}: message digest not compared (the content is detached)"),
                        ),
                    }
                }
                "1.2.840.113549.1.9.5" => {
                    signing_time = value
                        .and_then(|v| v.time())
                        .map(|t| t.0)
                        .unwrap_or_default();
                    out.fact(
                        &g,
                        "Signing time (claimed by the signer)",
                        signing_time.clone(),
                        src,
                    );
                }
                "1.2.840.113549.1.9.16.2.47" | "1.2.840.113549.1.9.16.2.12" => {
                    signing_certificate(&oid, value, cert.map(|c| &certs[c]), &g, src, out);
                }
                _ => out.fact(&g, &der::oid_label(&oid), "present", src),
            }
        }
    }
    if let Some(attrs) = unsigned {
        for attr in attrs.children() {
            let Some(oid) = attr.child(0).and_then(|o| o.oid()) else {
                continue;
            };
            let src = Some(attr.range());
            let value = attr.child(1).and_then(|s| s.child(0));
            if oid == "1.2.840.113549.1.9.16.2.14" || oid == "1.3.6.1.4.1.311.3.3.1" {
                if let Some(token) = value {
                    stamp = timestamp(&token, signature.as_ref(), &g, src.clone(), out);
                }
            } else {
                out.fact(
                    &g,
                    &format!("Unsigned {}", der::oid_label(&oid)),
                    "present",
                    src,
                );
            }
        }
    }
    Row {
        cells: vec![
            n.to_string(),
            who,
            cert_text,
            digest_name,
            sig_name,
            signing_time,
            stamp,
        ],
        source: Some(si.range()),
    }
}

/// ESS signing-certificate (v1: SHA-1, v2: SHA-256 by default) compared with the signer's
/// certificate when it is in the file.
fn signing_certificate(
    oid: &str,
    value: Option<Tlv>,
    cert: Option<&Cert>,
    g: &str,
    src: Option<std::ops::Range<u64>>,
    out: &mut Inspection,
) {
    let v2 = oid.ends_with(".47");
    let Some(first) = value.and_then(|v| v.child(0)).and_then(|ids| ids.child(0)) else {
        return;
    };
    let kids = first.kids();
    let (hash_alg, hash) = match kids.first() {
        Some(a) if a.universal(SEQUENCE) => (
            a.child(0).and_then(|o| o.oid()).unwrap_or_default(),
            kids.get(1).map(|h| h.content()).unwrap_or_default(),
        ),
        Some(h) => (
            if v2 {
                "2.16.840.1.101.3.4.2.1"
            } else {
                "1.3.14.3.2.26"
            }
            .to_owned(),
            h.content(),
        ),
        None => return,
    };
    let label = if v2 {
        "Signing certificate v2 (CAdES)"
    } else {
        "Signing certificate (ESS)"
    };
    out.fact(
        g,
        label,
        format!("{} {}", der::oid_label(&hash_alg), hex(hash)),
        src,
    );
    let Some(cert) = cert else {
        return;
    };
    let expected: Option<&[u8]> = match hash_alg.as_str() {
        "2.16.840.1.101.3.4.2.1" => Some(&cert.sha256_raw),
        "1.3.14.3.2.26" => Some(&cert.sha1_raw),
        _ => None,
    };
    if let Some(e) = expected {
        let same = e == hash;
        out.check(
            Stage::Semantic,
            if same { State::Passed } else { State::Failed },
            format!(
                "{g}: the signing-certificate hash {} the signer's certificate",
                if same { "matches" } else { "does not match" }
            ),
        );
    }
}

/// An RFC 3161 time-stamp token: its time, authority and imprint, compared with the signature.
fn timestamp(
    token: &Tlv,
    signature: Option<&Tlv>,
    g: &str,
    src: Option<std::ops::Range<u64>>,
    out: &mut Inspection,
) -> String {
    // ContentInfo { signedData { ..., encapContentInfo { id-ct-TSTInfo, [0] OCTET STRING } } }
    let sd = token
        .child(1)
        .filter(|c| c.context(0))
        .and_then(|c| c.inner());
    let tst = sd
        .and_then(|sd| sd.children().find(|k| k.universal(SEQUENCE)))
        .filter(|e| {
            e.child(0).and_then(|o| o.oid()).as_deref() == Some("1.2.840.113549.1.9.16.1.4")
        })
        .and_then(|e| e.child(1))
        .and_then(|c| c.inner())
        .and_then(|o| o.inner());
    let Some(tst) = tst else {
        out.fact(g, "Time-stamp token", "present (not readable)", src);
        return "present".into();
    };
    let k = tst.kids();
    let policy = k.get(1).and_then(|p| p.oid()).unwrap_or_default();
    let imprint = k.get(2);
    let gen_time = k
        .iter()
        .find_map(|t| t.time())
        .map(|t| t.0)
        .unwrap_or_default();
    let tsa = k
        .iter()
        .find(|t| t.context(0))
        .and_then(|t| t.inner())
        .map(|n| x509::general_name(&n))
        .unwrap_or_default();
    out.fact(g, "Time-stamp time", gen_time.clone(), src.clone());
    out.fact(g, "Time-stamp authority", tsa.clone(), None);
    out.fact(g, "Time-stamp policy", policy, None);
    if let (Some(imp), Some(sig)) = (imprint, signature) {
        let alg_oid = imp
            .child(0)
            .and_then(|a| a.child(0))
            .and_then(|o| o.oid())
            .unwrap_or_default();
        let value = imp.child(1).map(|v| v.content()).unwrap_or_default();
        if let Some(d) = digest_of(&alg_oid, sig.content()) {
            let same = d == value;
            out.check(
                Stage::Semantic,
                if same { State::Passed } else { State::Failed },
                format!(
                    "{g}: the time-stamp imprint {} the signature value",
                    if same { "matches" } else { "does not match" }
                ),
            );
        }
    }
    out.check(
        Stage::Signature,
        State::NotDone,
        format!("{g}: the time-stamp token's signature was not verified"),
    );
    clip(format!("{gen_time} {tsa}").trim(), 300)
}

/// EnvelopedData: recipients and the content encryption algorithm (nothing is decrypted).
fn enveloped(ed: &Tlv, out: &mut Inspection) {
    let kids = ed.kids();
    let recipients = kids.iter().find(|k| k.universal(SET));
    let enc = kids
        .iter()
        .find(|k| k.universal(SEQUENCE) && k.child(0).and_then(|o| o.oid()).is_some());
    let mut table = Table {
        name: "Recipients".into(),
        columns: vec![
            "#".into(),
            "Kind".into(),
            "Identified by".into(),
            "Key encryption".into(),
        ],
        ..Table::default()
    };
    for (n, r) in recipients
        .map(|r| r.kids())
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let (kind, who, alg_name) = if r.universal(SEQUENCE) {
            let rk = r.kids();
            let rid = rk.get(1);
            let who = match rid {
                Some(s) if s.universal(SEQUENCE) => format!(
                    "issuer {}, serial {}",
                    s.child(0).map(|n| x509::name_text(&n)).unwrap_or_default(),
                    s.child(1).and_then(|v| v.int_text()).unwrap_or_default()
                ),
                Some(s) if s.context(0) => {
                    format!("subject key identifier {}", colon_hex(s.content()))
                }
                _ => String::new(),
            };
            ("key transport", who, alg(rk.get(2)).0)
        } else {
            let kind = match (r.class, r.tag) {
                (CONTEXT, 1) => "key agreement",
                (CONTEXT, 2) => "key encryption key",
                (CONTEXT, 3) => "password",
                (CONTEXT, 4) => "other",
                _ => "unknown",
            };
            (kind, String::new(), String::new())
        };
        table.rows.push(Row {
            cells: vec![(n + 1).to_string(), kind.into(), who, alg_name],
            source: Some(r.range()),
        });
    }
    out.fact(
        "Enveloped data",
        "Recipients",
        table.rows.len().to_string(),
        recipients.map(|r| r.range()),
    );
    if let Some(e) = enc {
        let ek = e.kids();
        out.fact(
            "Enveloped data",
            "Content type",
            ek.first()
                .and_then(|o| o.oid())
                .map(|o| der::oid_label(&o))
                .unwrap_or_default(),
            Some(e.range()),
        );
        out.fact(
            "Enveloped data",
            "Content encryption",
            alg(ek.get(1)).0,
            ek.get(1).map(|a| a.range()),
        );
        if let Some(c) = ek.get(2) {
            out.fact(
                "Enveloped data",
                "Encrypted content",
                bytes_text(c.len as u64),
                Some(c.range()),
            );
        }
    }
    if !table.rows.is_empty() {
        out.tables.push(table);
    }
    out.notes
        .push("The content is encrypted; Isolinear does not decrypt it.".into());
}
