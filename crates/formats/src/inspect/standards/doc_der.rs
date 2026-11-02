//! Just enough DER to describe X.509 certificates and CMS signatures for display: who signed
//! (subject and issuer names), validity dates, the signing time and digest algorithm. Nothing
//! here verifies anything; lengths are checked, and only fixed paths into the structures are
//! followed (no recursion driven by the data).

#[derive(Clone, Copy, Debug)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub content: &'a [u8],
    /// The whole element, header included.
    pub raw: &'a [u8],
}

/// One element at the start of `b`, and what follows it.
pub fn read(b: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    let tag = *b.first()?;
    if tag & 0x1F == 0x1F {
        return None;
    }
    let first = *b.get(1)?;
    let (len, header) = if first < 0x80 {
        (first as usize, 2)
    } else {
        let n = (first & 0x7F) as usize;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *b.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let end = header.checked_add(len)?;
    let content = b.get(header..end)?;
    Some((
        Tlv {
            tag,
            content,
            raw: &b[..end],
        },
        &b[end..],
    ))
}

/// The elements inside a constructed element.
pub fn children(b: &[u8]) -> Vec<Tlv<'_>> {
    let mut out = vec![];
    let mut rest = b;
    while !rest.is_empty() && out.len() < 10_000 {
        match read(rest) {
            Some((t, r)) => {
                out.push(t);
                rest = r;
            }
            None => break,
        }
    }
    out
}

/// Dotted text of an OID's content bytes.
pub fn oid(b: &[u8]) -> String {
    let mut parts: Vec<u64> = vec![];
    let mut v = 0u64;
    for (i, &c) in b.iter().enumerate() {
        v = (v << 7) | u64::from(c & 0x7F);
        if c & 0x80 == 0 {
            if parts.is_empty() {
                let first = (v / 40).min(2);
                parts.push(first);
                parts.push(v - first * 40);
            } else {
                parts.push(v);
            }
            v = 0;
        }
        if i > 64 {
            break;
        }
    }
    parts
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

fn oid_name(dotted: &str) -> &str {
    match dotted {
        "2.5.4.3" => "CN",
        "2.5.4.4" => "SN",
        "2.5.4.5" => "serialNumber",
        "2.5.4.6" => "C",
        "2.5.4.7" => "L",
        "2.5.4.8" => "ST",
        "2.5.4.10" => "O",
        "2.5.4.11" => "OU",
        "2.5.4.12" => "title",
        "2.5.4.42" => "GN",
        "2.5.4.97" => "organizationIdentifier",
        "1.2.840.113549.1.9.1" => "E",
        "1.3.14.3.2.26" => "SHA-1",
        "2.16.840.1.101.3.4.2.1" => "SHA-256",
        "2.16.840.1.101.3.4.2.2" => "SHA-384",
        "2.16.840.1.101.3.4.2.3" => "SHA-512",
        "1.2.840.113549.1.1.11" => "RSA with SHA-256",
        "1.2.840.113549.1.1.12" => "RSA with SHA-384",
        "1.2.840.113549.1.1.13" => "RSA with SHA-512",
        "1.2.840.113549.1.1.10" => "RSA-PSS",
        "1.2.840.113549.1.1.1" => "RSA",
        "1.2.840.10045.4.3.2" => "ECDSA with SHA-256",
        "1.2.840.10045.4.3.3" => "ECDSA with SHA-384",
        "1.2.840.10045.4.3.4" => "ECDSA with SHA-512",
        other => other,
    }
}

/// A directory string (UTF8, Printable, IA5, T61, BMP).
fn string(t: &Tlv) -> String {
    match t.tag {
        0x1E => {
            let units: Vec<u16> = t
                .content
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        0x14 => t.content.iter().map(|&b| b as char).collect(),
        _ => String::from_utf8_lossy(t.content).into_owned(),
    }
}

/// "CN=Ann Doe, O=Example, C=LU" from a Name.
pub fn name(b: &[u8]) -> String {
    let mut parts = vec![];
    for set in children(b) {
        for attr in children(set.content) {
            let kids = children(attr.content);
            if let [o, v, ..] = kids.as_slice()
                && o.tag == 0x06
            {
                parts.push(format!("{}={}", oid_name(&oid(o.content)), string(v)));
            }
        }
    }
    parts.join(", ")
}

/// UTCTime or GeneralizedTime as `YYYY-MM-DD HH:MM:SS UTC`.
pub fn time(t: &Tlv) -> Option<String> {
    let s = std::str::from_utf8(t.content).ok()?;
    let (y, rest) = match t.tag {
        0x17 => {
            let yy: i32 = s.get(..2)?.parse().ok()?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, s.get(2..)?)
        }
        0x18 => (s.get(..4)?.parse().ok()?, s.get(4..)?),
        _ => return None,
    };
    let n = |i: usize| rest.get(i..i + 2).and_then(|x| x.parse::<u32>().ok());
    Some(format!(
        "{y:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        n(0)?,
        n(2)?,
        n(4)?,
        n(6)?,
        n(8).unwrap_or(0)
    ))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Certificate {
    pub subject: String,
    pub issuer: String,
    pub serial: Vec<u8>,
    pub not_before: String,
    pub not_after: String,
}

/// The names and validity of a DER certificate.
pub fn certificate(der: &[u8]) -> Option<Certificate> {
    let (cert, _) = read(der)?;
    let tbs = *children(cert.content).first()?;
    let mut fields = children(tbs.content).into_iter();
    let mut f = fields.next()?;
    if f.tag == 0xA0 {
        f = fields.next()?;
    }
    let serial = f.content.to_vec();
    let _alg = fields.next()?;
    let issuer = fields.next()?;
    let validity = fields.next()?;
    let subject = fields.next()?;
    let v = children(validity.content);
    Some(Certificate {
        subject: name(subject.content),
        issuer: name(issuer.content),
        serial,
        not_before: v.first().and_then(time).unwrap_or_default(),
        not_after: v.get(1).and_then(time).unwrap_or_default(),
    })
}

#[derive(Clone, Debug, Default)]
pub struct Cms {
    pub signer: Option<Certificate>,
    pub certificates: usize,
    pub signing_time: Option<String>,
    pub digest: Option<String>,
    pub signature_algorithm: Option<String>,
    /// An RFC 3161 timestamp token is attached to the signature.
    pub timestamp: bool,
    /// The content is itself a timestamp token (a document timestamp).
    pub is_timestamp_token: bool,
}

/// What a CMS SignedData blob (PKCS#7) says about its signer.
pub fn cms(der: &[u8]) -> Option<Cms> {
    let (info, _) = read(der)?;
    let kids = children(info.content);
    let (oid_t, explicit) = (kids.first()?, kids.get(1)?);
    if oid(oid_t.content) != "1.2.840.113549.1.7.2" {
        return None;
    }
    let (signed, _) = read(explicit.content)?;
    let parts = children(signed.content);
    let mut out = Cms::default();
    let mut certs = vec![];
    if let Some(encap) = parts.get(2) {
        let e = children(encap.content);
        out.is_timestamp_token = e
            .first()
            .is_some_and(|o| oid(o.content) == "1.2.840.113549.1.9.16.1.4");
    }
    for p in &parts[3.min(parts.len())..] {
        if p.tag == 0xA0 {
            certs = children(p.content)
                .iter()
                .filter_map(|c| certificate(c.raw))
                .collect();
        }
    }
    out.certificates = certs.len();
    let signer_infos = parts.iter().rev().find(|p| p.tag == 0x31)?;
    let si = *children(signer_infos.content).first()?;
    let fields = children(si.content);
    // sid: IssuerAndSerialNumber (a SEQUENCE) or [0] subjectKeyIdentifier.
    if let Some(sid) = fields.get(1) {
        if sid.tag == 0x30 {
            let ias = children(sid.content);
            if let (Some(iss), Some(ser)) = (ias.first(), ias.get(1)) {
                let issuer = name(iss.content);
                out.signer = certs
                    .iter()
                    .find(|c| c.issuer == issuer && c.serial == ser.content)
                    .cloned();
            }
        }
        if out.signer.is_none() {
            out.signer = certs.first().cloned();
        }
    }
    if let Some(d) = fields.get(2) {
        out.digest = children(d.content)
            .first()
            .map(|o| oid_name(&oid(o.content)).to_owned());
    }
    for f in &fields[3.min(fields.len())..] {
        match f.tag {
            0xA0 => {
                for attr in children(f.content) {
                    let a = children(attr.content);
                    if let (Some(o), Some(set)) = (a.first(), a.get(1))
                        && oid(o.content) == "1.2.840.113549.1.9.5"
                    {
                        out.signing_time = children(set.content).first().and_then(time);
                    }
                }
            }
            0xA1 => {
                out.timestamp = children(f.content).iter().any(|attr| {
                    children(attr.content)
                        .first()
                        .is_some_and(|o| oid(o.content) == "1.2.840.113549.1.9.16.2.14")
                });
            }
            0x30 if out.signature_algorithm.is_none() && out.digest.is_some() => {
                out.signature_algorithm = children(f.content)
                    .first()
                    .filter(|o| o.tag == 0x06)
                    .map(|o| oid_name(&oid(o.content)).to_owned());
            }
            _ => {}
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_times_and_bounds() {
        // SEQUENCE { SET { SEQUENCE { OID 2.5.4.3, UTF8String "Ann" } } }
        let name_der = [
            0x31, 0x0A, 0x30, 0x08, 0x06, 0x03, 0x55, 0x04, 0x03, 0x0C, 0x01, b'A',
        ];
        assert_eq!(name(&name_der), "CN=A");
        let t = Tlv {
            tag: 0x17,
            content: b"261103101500Z",
            raw: &[],
        };
        assert_eq!(time(&t).unwrap(), "2026-11-03 10:15:00 UTC");
        assert_eq!(
            oid(&[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x02]),
            "1.2.840.113549.1.7.2"
        );
        assert!(read(&[0x30, 0x85, 1, 2, 3, 4, 5]).is_none());
        assert!(read(&[0x30, 0x05, 1]).is_none());
        assert!(cms(b"junk").is_none());
        assert!(certificate(&[0x30, 0x00]).is_none());
    }
}
