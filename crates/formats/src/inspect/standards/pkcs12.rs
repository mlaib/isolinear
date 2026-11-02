//! PKCS#12 / PFX key stores (RFC 7292): version, the safes and their protection, the bags
//! (certificates, keys, CRLs, secrets) and the integrity MAC's parameters. Password-protected
//! safes are listed but not opened (password entry is not implemented yet). Private keys are
//! never shown, decoded or copied, encrypted or not: only their kind.
use super::der::{self, OCTET_STRING, SEQUENCE, Shape, Tlv};
use super::hash::hex;
use super::util::bytes_text;
use super::x509::{self, Cert};
use crate::inspect::{Handler, Input, Inspection, OutlineNode, Probe, Row, Stage, State, Table};

const MAX_NESTING: usize = 4;

pub struct Pkcs12;
impl Handler for Pkcs12 {
    fn name(&self) -> &'static str {
        "PKCS#12"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        der::shape(probe.head) == Some(Shape::Pkcs12)
    }
    fn inspect(&self, input: &Input, _cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let top = der::root(input.bytes).map_err(|e| format!("Not readable as DER: {e}"))?;
        let mut out = Inspection::new("PKCS#12", "Key store (PFX)");
        let kids = top.kids_strict()?;
        let version = kids.first().and_then(|v| v.u64()).unwrap_or(0);
        out.version = Some(version.to_string());
        out.fact(
            "Store",
            "Version",
            version.to_string(),
            kids.first().map(|v| v.range()),
        );
        let mut st = State0::default();
        out.outline
            .push(node(0, "PFX", &format!("version {version}"), &top));
        if let Some(auth) = kids.get(1) {
            let oid = auth.child(0).and_then(|o| o.oid()).unwrap_or_default();
            out.fact(
                "Store",
                "Integrity mode",
                match oid.as_str() {
                    "1.2.840.113549.1.7.1" => "password (MAC)",
                    "1.2.840.113549.1.7.2" => "public key (signed)",
                    _ => "unknown",
                },
                Some(auth.range()),
            );
            let safes = auth
                .child(1)
                .filter(|c| c.context(0))
                .and_then(|c| c.inner());
            let safes = match oid.as_str() {
                "1.2.840.113549.1.7.1" => safes
                    .filter(|o| o.universal(OCTET_STRING))
                    .and_then(|o| o.inner()),
                _ => {
                    out.notes.push(
                        "The store is signed with a public key; its safes are not read.".into(),
                    );
                    None
                }
            };
            if let Some(safes) = safes {
                let mut types = vec![];
                for ci in safes.children() {
                    types.push(safe(&ci, &mut st));
                }
                out.fact("Store", "Safes", types.join(", "), Some(safes.range()));
            }
        }
        if let Some(mac) = kids.get(2).filter(|m| m.universal(SEQUENCE)) {
            let mk = mac.kids();
            let alg = mk
                .first()
                .and_then(|d| d.child(0))
                .and_then(|a| a.child(0))
                .and_then(|o| o.oid())
                .unwrap_or_default();
            out.fact(
                "Integrity",
                "MAC algorithm",
                der::oid_label(&alg),
                Some(mac.range()),
            );
            out.fact(
                "Integrity",
                "MAC iterations",
                mk.get(2).and_then(|i| i.u64()).unwrap_or(1).to_string(),
                mk.get(2).map(|i| i.range()),
            );
            if let Some(salt) = mk.get(1) {
                out.fact(
                    "Integrity",
                    "MAC salt",
                    bytes_text(salt.len as u64),
                    Some(salt.range()),
                );
            }
            out.check(
                Stage::Signature,
                State::NotDone,
                "The integrity MAC was not verified (it needs the password)",
            );
        }
        out.fact(
            "Contents",
            "Certificates (readable)",
            st.certs.len().to_string(),
            None,
        );
        out.fact("Contents", "Private keys", st.keys.to_string(), None);
        out.fact(
            "Contents",
            "Encrypted safes (need the password)",
            st.encrypted.to_string(),
            None,
        );
        for (i, c) in st.cert_tlvs.iter().enumerate().take(x509::DETAILED) {
            let _ = x509::cert_facts(c, &mut out, &format!("Certificate {}", i + 1));
        }
        if !st.bags.rows.is_empty() {
            st.bags.name = "Bags".into();
            st.bags.columns = ["#", "Bag", "Content", "Friendly name", "Local key ID"]
                .iter()
                .map(|c| (*c).to_owned())
                .collect();
            out.tables.push(std::mem::take(&mut st.bags));
        }
        if !st.certs.is_empty() {
            out.tables.push(x509::table("Certificates", &st.certs));
        }
        out.outline.append(&mut st.outline);
        match der::scan_errors(&top, 0) {
            None if st.encrypted == 0 => out.check(Stage::Parsed, State::Passed, "Read as DER; every safe was listed"),
            None => out.check(
                Stage::Parsed,
                State::Partial,
                "Read as DER; encrypted safes were listed but not opened (password entry is not implemented yet)",
            ),
            Some(e) => out.check(Stage::Parsed, State::Partial, format!("Damaged: {e}")),
        }
        out.not_validated();
        out.notes.push(
            "Password entry is not implemented yet: encrypted safes are listed with their protection, not opened."
                .into(),
        );
        out.notes.push(der::PRIVATE_NOTE.into());
        Ok(out)
    }
}

#[derive(Default)]
struct State0<'a> {
    bags: Table,
    certs: Vec<Cert>,
    cert_tlvs: Vec<Tlv<'a>>,
    keys: usize,
    encrypted: usize,
    outline: Vec<OutlineNode>,
}

fn node(depth: u16, label: &str, value: &str, t: &Tlv) -> OutlineNode {
    OutlineNode {
        depth,
        label: label.into(),
        value: value.into(),
        source: Some(t.range()),
    }
}

/// Encryption parameters, readable: "PBES2 (PBKDF2, hmacWithSHA256, 2048 iterations, aes256-CBC)".
pub fn protection(alg: Option<&Tlv>) -> String {
    let Some(alg) = alg else {
        return String::new();
    };
    let oid = alg.child(0).and_then(|o| o.oid()).unwrap_or_default();
    let params = alg.child(1);
    if oid == "1.2.840.113549.1.5.13" {
        let kdf = params.and_then(|p| p.child(0));
        let kdf_params = kdf.and_then(|k| k.child(1));
        let iterations = kdf_params
            .and_then(|p| p.kids().into_iter().find(|k| k.universal(der::INTEGER)))
            .and_then(|i| i.u64());
        let prf = kdf_params
            .and_then(|p| p.kids().into_iter().find(|k| k.universal(SEQUENCE)))
            .and_then(|a| a.child(0))
            .and_then(|o| o.oid())
            .map(|o| der::oid_label(&o))
            .unwrap_or_else(|| "hmacWithSHA1".into());
        let kdf_name = kdf
            .and_then(|k| k.child(0))
            .and_then(|o| o.oid())
            .map(|o| der::oid_label(&o))
            .unwrap_or_default();
        let cipher = params
            .and_then(|p| p.child(1))
            .and_then(|e| e.child(0))
            .and_then(|o| o.oid())
            .map(|o| der::oid_label(&o))
            .unwrap_or_default();
        let it = iterations
            .map(|i| format!(", {i} iterations"))
            .unwrap_or_default();
        return format!("PBES2 ({kdf_name}, {prf}{it}, {cipher})");
    }
    let iterations = params.and_then(|p| p.child(1)).and_then(|i| i.u64());
    match iterations {
        Some(i) => format!("{}, {i} iterations", der::oid_label(&oid)),
        None => der::oid_label(&oid),
    }
}

/// One ContentInfo of the authenticated safe; returns its kind.
fn safe<'a>(ci: &Tlv<'a>, st: &mut State0<'a>) -> String {
    let oid = ci.child(0).and_then(|o| o.oid()).unwrap_or_default();
    let content = ci.child(1).filter(|c| c.context(0)).and_then(|c| c.inner());
    match oid.as_str() {
        "1.2.840.113549.1.7.1" => {
            st.outline.push(node(1, "Safe", "not encrypted", ci));
            if let Some(contents) = content
                .filter(|o| o.universal(OCTET_STRING))
                .and_then(|o| o.inner())
            {
                bags(&contents, st, 2);
            }
            "data".into()
        }
        "1.2.840.113549.1.7.6" => {
            st.encrypted += 1;
            let eci = content.and_then(|e| e.child(1));
            let alg = eci.and_then(|e| e.child(1));
            let how = protection(alg.as_ref());
            st.outline.push(node(
                1,
                "Safe",
                &format!("encrypted — needs the password ({how})"),
                ci,
            ));
            let n = st.bags.rows.len() + 1;
            st.bags.rows.push(Row {
                cells: vec![
                    n.to_string(),
                    "Encrypted safe".into(),
                    format!("encrypted — needs the password ({how}); usually the certificates"),
                    String::new(),
                    String::new(),
                ],
                source: Some(ci.range()),
            });
            "encrypted data".into()
        }
        "1.2.840.113549.1.7.3" => {
            st.encrypted += 1;
            st.outline
                .push(node(1, "Safe", "encrypted with a public key", ci));
            "enveloped data".into()
        }
        other => {
            st.outline.push(node(1, "Safe", &der::oid_label(other), ci));
            der::oid_label(other)
        }
    }
}

fn bags<'a>(contents: &Tlv<'a>, st: &mut State0<'a>, depth: u16) {
    if usize::from(depth) > MAX_NESTING + 1 {
        return;
    }
    for bag in contents.children() {
        let kids = bag.kids();
        let oid = kids.first().and_then(|o| o.oid()).unwrap_or_default();
        let value = kids.get(1).filter(|v| v.context(0)).and_then(|v| v.inner());
        let mut friendly = String::new();
        let mut key_id = String::new();
        if let Some(attrs) = kids.get(2) {
            for a in attrs.children() {
                let v = a.child(1).and_then(|s| s.child(0));
                match a.child(0).and_then(|o| o.oid()).as_deref() {
                    Some("1.2.840.113549.1.9.20") => {
                        friendly = v.and_then(|v| v.string()).unwrap_or_default()
                    }
                    Some("1.2.840.113549.1.9.21") => {
                        key_id = v.map(|v| hex(v.content())).unwrap_or_default()
                    }
                    _ => {}
                }
            }
        }
        let (name, content) = match oid.as_str() {
            "1.2.840.113549.1.12.10.1.3" => {
                let cert = value
                    .and_then(|cb| cb.child(1))
                    .filter(|c| c.context(0))
                    .and_then(|c| c.inner())
                    .and_then(|o| o.inner());
                match cert.map(|c| (x509::summarize(&c), c)) {
                    Some((Ok(s), c)) => {
                        let text = format!("certificate: {}", s.subject);
                        st.certs.push(s);
                        st.cert_tlvs.push(c);
                        ("Certificate", text)
                    }
                    _ => ("Certificate", "not readable".into()),
                }
            }
            "1.2.840.113549.1.12.10.1.1" => {
                st.keys += 1;
                let kind = value.map(|v| der::private_key_kind(&v)).unwrap_or_default();
                ("Private key", format!("{kind}, not encrypted — not shown"))
            }
            "1.2.840.113549.1.12.10.1.2" => {
                st.keys += 1;
                let how = protection(value.and_then(|v| v.child(0)).as_ref());
                ("Private key", format!("encrypted ({how}) — not shown"))
            }
            "1.2.840.113549.1.12.10.1.4" => ("CRL", "present".into()),
            "1.2.840.113549.1.12.10.1.5" => ("Secret", "present — not shown".into()),
            "1.2.840.113549.1.12.10.1.6" => {
                if let Some(v) = value {
                    bags(&v, st, depth + 1);
                }
                ("Nested safe contents", String::new())
            }
            _ => ("Unknown bag", der::oid_label(&oid)),
        };
        st.outline.push(node(depth, name, &content, &bag));
        let n = st.bags.rows.len() + 1;
        if st.bags.rows.len() < crate::inspect::MAX_ROWS {
            st.bags.rows.push(Row {
                cells: vec![n.to_string(), name.into(), content, friendly, key_id],
                source: Some(bag.range()),
            });
        }
    }
}
