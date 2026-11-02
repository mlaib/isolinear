//! A small ASN.1 DER reader for certificates, CMS and PKCS#12: tag class, number and form,
//! definite lengths only (BER's indefinite lengths are refused), bounded nesting, and object
//! identifiers with names for the common ones. Every element keeps its byte range in the file.
//! Also the generic handler for `.der` files that no specific handler takes: an outline of the
//! structure.
use super::util::{clip, printable};
use crate::inspect::{Handler, Input, Inspection, OutlineNode, Probe, Stage, State};
use std::ops::Range;

/// Deepest nesting followed.
pub const MAX_DEPTH: usize = 40;
/// Children read from one constructed element.
pub const MAX_CHILDREN: usize = 200_000;
/// Outline entries produced for one file.
pub const MAX_OUTLINE: usize = 20_000;

pub const UNIVERSAL: u8 = 0;
pub const CONTEXT: u8 = 2;

pub const BOOLEAN: u32 = 1;
pub const INTEGER: u32 = 2;
pub const BIT_STRING: u32 = 3;
pub const OCTET_STRING: u32 = 4;
pub const NULL: u32 = 5;
pub const OID: u32 = 6;
pub const SEQUENCE: u32 = 16;
pub const SET: u32 = 17;

/// One element: identifier, length and content, as offsets into the whole buffer.
#[derive(Clone, Copy, Debug)]
pub struct Tlv<'a> {
    data: &'a [u8],
    pub class: u8,
    pub constructed: bool,
    pub tag: u32,
    /// Offset of the identifier byte in the buffer.
    pub start: usize,
    /// Identifier and length bytes.
    pub hlen: usize,
    /// Content bytes.
    pub len: usize,
}

/// Identifier and length at `at`, without requiring the content to be present.
/// Returns (class, constructed, tag, header length, content length).
pub fn header(data: &[u8], at: usize) -> Result<(u8, bool, u32, usize, usize), String> {
    let first = *data.get(at).ok_or("ends inside an element header")?;
    let class = first >> 6;
    let constructed = first & 0x20 != 0;
    let mut pos = at + 1;
    let mut tag = u32::from(first & 0x1f);
    if tag == 0x1f {
        tag = 0;
        for i in 0.. {
            let b = *data.get(pos).ok_or("ends inside a tag")?;
            pos += 1;
            if i >= 4 {
                return Err("tag number too large".into());
            }
            tag = (tag << 7) | u32::from(b & 0x7f);
            if b & 0x80 == 0 {
                break;
            }
        }
    }
    let l = *data.get(pos).ok_or("ends inside a length")?;
    pos += 1;
    let len = if l < 0x80 {
        usize::from(l)
    } else if l == 0x80 {
        return Err("indefinite length (BER); only DER's definite lengths are read".into());
    } else {
        let n = usize::from(l & 0x7f);
        if n > 4 {
            return Err("length field too large".into());
        }
        let mut v = 0usize;
        for _ in 0..n {
            let b = *data.get(pos).ok_or("ends inside a length")?;
            pos += 1;
            v = (v << 8) | usize::from(b);
        }
        v
    };
    Ok((class, constructed, tag, pos - at, len))
}

/// The element at `at`, which must end at or before `end`.
pub fn parse(data: &[u8], at: usize, end: usize) -> Result<Tlv<'_>, String> {
    let (class, constructed, tag, hlen, len) = header(data, at)?;
    let stop = at
        .checked_add(hlen)
        .and_then(|v| v.checked_add(len))
        .ok_or("length overflows")?;
    if stop > end.min(data.len()) {
        return Err(format!(
            "an element at byte {at} claims {len} bytes but its container ends first"
        ));
    }
    Ok(Tlv {
        data,
        class,
        constructed,
        tag,
        start: at,
        hlen,
        len,
    })
}

/// The element starting at the first byte.
pub fn root(data: &[u8]) -> Result<Tlv<'_>, String> {
    parse(data, 0, data.len())
}

/// Children of a constructed element, stopping at the first damaged one.
pub struct Children<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
    count: usize,
    pub error: Option<String>,
}
impl<'a> Iterator for Children<'a> {
    type Item = Tlv<'a>;
    fn next(&mut self) -> Option<Tlv<'a>> {
        if self.pos >= self.end || self.error.is_some() || self.count >= MAX_CHILDREN {
            return None;
        }
        match parse(self.data, self.pos, self.end) {
            Ok(t) => {
                self.pos = t.end();
                self.count += 1;
                Some(t)
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }
}

impl<'a> Tlv<'a> {
    pub fn content(&self) -> &'a [u8] {
        let s = self.start + self.hlen;
        self.data.get(s..s + self.len).unwrap_or(&[])
    }
    pub fn content_start(&self) -> usize {
        self.start + self.hlen
    }
    pub fn end(&self) -> usize {
        self.start + self.hlen + self.len
    }
    /// The whole element (identifier, length, content).
    pub fn bytes(&self) -> &'a [u8] {
        self.data.get(self.start..self.end()).unwrap_or(&[])
    }
    pub fn range(&self) -> Range<u64> {
        self.start as u64..self.end() as u64
    }
    pub fn is(&self, class: u8, tag: u32) -> bool {
        self.class == class && self.tag == tag
    }
    pub fn universal(&self, tag: u32) -> bool {
        self.is(UNIVERSAL, tag)
    }
    pub fn context(&self, n: u32) -> bool {
        self.is(CONTEXT, n)
    }
    pub fn children(&self) -> Children<'a> {
        Children {
            data: self.data,
            pos: if self.constructed {
                self.content_start()
            } else {
                self.end()
            },
            end: self.end(),
            count: 0,
            error: None,
        }
    }
    pub fn kids(&self) -> Vec<Tlv<'a>> {
        self.children().collect()
    }
    /// Children, or the first error met reading them.
    pub fn kids_strict(&self) -> Result<Vec<Tlv<'a>>, String> {
        let mut it = self.children();
        let v: Vec<_> = it.by_ref().collect();
        match it.error {
            Some(e) => Err(e),
            None => Ok(v),
        }
    }
    pub fn child(&self, i: usize) -> Option<Tlv<'a>> {
        self.children().nth(i)
    }
    /// The single element that fills this one's content exactly (EXPLICIT tags, and DER
    /// wrapped in OCTET STRING or BIT STRING).
    pub fn inner(&self) -> Option<Tlv<'a>> {
        let mut start = self.content_start();
        if self.universal(BIT_STRING) {
            if self.content().first() != Some(&0) {
                return None;
            }
            start += 1;
        }
        let t = parse(self.data, start, self.end()).ok()?;
        (t.end() == self.end()).then_some(t)
    }
    pub fn oid(&self) -> Option<String> {
        if !self.universal(OID) {
            return None;
        }
        oid_text(self.content())
    }
    /// The integer's content without the sign padding byte.
    pub fn uint_bytes(&self) -> Option<&'a [u8]> {
        if !self.universal(INTEGER) && self.class != CONTEXT {
            return None;
        }
        let c = self.content();
        Some(match c {
            [0, rest @ ..] if !rest.is_empty() => rest,
            _ => c,
        })
    }
    pub fn u64(&self) -> Option<u64> {
        let c = self.uint_bytes()?;
        if c.len() > 8
            || c.first()
                .is_some_and(|b| b & 0x80 != 0 && self.content()[0] != 0)
        {
            return None;
        }
        Some(c.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b)))
    }
    /// Small integers in decimal, large ones as colon hex (serial numbers).
    pub fn int_text(&self) -> Option<String> {
        if let Some(v) = self.u64() {
            return Some(v.to_string());
        }
        let c = self.uint_bytes()?;
        Some(super::hash::colon_hex(c).to_lowercase())
    }
    pub fn boolean(&self) -> Option<bool> {
        (self.universal(BOOLEAN) && self.len == 1).then(|| self.content()[0] != 0)
    }
    /// BIT STRING: (unused bits, bytes).
    pub fn bits(&self) -> Option<(u8, &'a [u8])> {
        if !self.universal(BIT_STRING) {
            return None;
        }
        let (&unused, rest) = self.content().split_first()?;
        Some((unused, rest))
    }
    /// Character strings of every ASN.1 string type, readable.
    pub fn string(&self) -> Option<String> {
        if self.class != UNIVERSAL {
            return None;
        }
        let c = self.content();
        let s = match self.tag {
            12 => String::from_utf8_lossy(c).into_owned(),
            18 | 19 | 22 | 26 | 25 | 27 => c.iter().map(|&b| b as char).collect(),
            // T61/Teletex: in practice Latin-1.
            20 | 21 => c.iter().map(|&b| b as char).collect(),
            30 => {
                let units: Vec<u16> = c
                    .chunks_exact(2)
                    .map(|p| u16::from_be_bytes([p[0], p[1]]))
                    .collect();
                String::from_utf16_lossy(&units)
            }
            28 => c
                .chunks_exact(4)
                .map(|p| {
                    char::from_u32(u32::from_be_bytes([p[0], p[1], p[2], p[3]]))
                        .unwrap_or('\u{fffd}')
                })
                .collect(),
            _ => return None,
        };
        Some(printable(&s))
    }
    /// UTCTime or GeneralizedTime: (readable, Unix seconds).
    pub fn time(&self) -> Option<(String, i64)> {
        if self.class != UNIVERSAL || !(self.tag == 23 || self.tag == 24) {
            return None;
        }
        let s = std::str::from_utf8(self.content()).ok()?;
        parse_time(s, self.tag == 23)
    }
    /// A short readable value for outlines.
    pub fn describe(&self) -> String {
        if self.class == UNIVERSAL {
            match self.tag {
                BOOLEAN => return self.boolean().map(|b| b.to_string()).unwrap_or_default(),
                INTEGER => {
                    if let Some(v) = self.u64() {
                        return v.to_string();
                    }
                    let c = self.content();
                    let shown = super::hash::hex(&c[..c.len().min(16)]);
                    return format!(
                        "0x{shown}{} ({} bytes)",
                        if c.len() > 16 { "…" } else { "" },
                        c.len()
                    );
                }
                BIT_STRING => {
                    return format!("{} bits", (self.len.saturating_sub(1)) * 8);
                }
                OCTET_STRING => return format!("{} bytes", self.len),
                OID => {
                    let Some(o) = self.oid() else {
                        return "invalid".into();
                    };
                    return match oid_name(&o) {
                        Some(n) => format!("{n} ({o})"),
                        None => o,
                    };
                }
                23 | 24 => return self.time().map(|t| t.0).unwrap_or_default(),
                _ => {}
            }
            if let Some(s) = self.string() {
                return clip(&s, 200);
            }
        }
        if self.constructed {
            String::new()
        } else {
            format!("{} bytes", self.len)
        }
    }
}

/// Readable name of the element's tag.
pub fn tag_name(t: &Tlv) -> String {
    match t.class {
        UNIVERSAL => match t.tag {
            1 => "BOOLEAN",
            2 => "INTEGER",
            3 => "BIT STRING",
            4 => "OCTET STRING",
            5 => "NULL",
            6 => "OBJECT IDENTIFIER",
            10 => "ENUMERATED",
            12 => "UTF8String",
            16 => "SEQUENCE",
            17 => "SET",
            18 => "NumericString",
            19 => "PrintableString",
            20 => "T61String",
            22 => "IA5String",
            23 => "UTCTime",
            24 => "GeneralizedTime",
            26 => "VisibleString",
            28 => "UniversalString",
            30 => "BMPString",
            n => return format!("UNIVERSAL {n}"),
        }
        .into(),
        1 => format!("[APPLICATION {}]", t.tag),
        CONTEXT => format!("[{}]", t.tag),
        _ => format!("[PRIVATE {}]", t.tag),
    }
}

fn oid_text(c: &[u8]) -> Option<String> {
    if c.is_empty() || c.last()? & 0x80 != 0 {
        return None;
    }
    let mut arcs: Vec<u64> = vec![];
    let mut v = 0u64;
    for &b in c {
        v = v.checked_mul(128)? | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            arcs.push(v);
            v = 0;
        }
    }
    let first = arcs[0];
    let (a, b) = match first {
        0..=39 => (0, first),
        40..=79 => (1, first - 40),
        _ => (2, first - 80),
    };
    let mut s = format!("{a}.{b}");
    for arc in &arcs[1..] {
        s.push('.');
        s.push_str(&arc.to_string());
    }
    Some(s)
}

/// Days since 1970-01-01 for a civil date (H. Hinnant).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn parse_time(s: &str, utc_time: bool) -> Option<(String, i64)> {
    let digits = |r: Range<usize>| -> Option<i64> {
        let p = s.get(r)?;
        p.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| p.parse().ok())?
    };
    let (year, rest) = if utc_time {
        let yy = digits(0..2)?;
        (if yy < 50 { 2000 + yy } else { 1900 + yy }, 2)
    } else {
        (digits(0..4)?, 4)
    };
    let month = digits(rest..rest + 2)?;
    let day = digits(rest + 2..rest + 4)?;
    let hour = digits(rest + 4..rest + 6)?;
    let minute = digits(rest + 6..rest + 8)?;
    let second = digits(rest + 8..rest + 10).unwrap_or(0);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    let zone = if s.ends_with('Z') { " UTC" } else { "" };
    Some((
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}{zone}"),
        secs,
    ))
}

/// Name of a well-known object identifier.
pub fn oid_name(oid: &str) -> Option<&'static str> {
    OIDS.iter().find(|(o, _)| *o == oid).map(|(_, n)| *n)
}

/// The OID's name, or the dotted form when unknown.
pub fn oid_label(oid: &str) -> String {
    oid_name(oid)
        .map(str::to_owned)
        .unwrap_or_else(|| oid.to_owned())
}

static OIDS: &[(&str, &str)] = &[
    // X.500 attribute types.
    ("2.5.4.3", "CN"),
    ("2.5.4.4", "SN"),
    ("2.5.4.5", "serialNumber"),
    ("2.5.4.6", "C"),
    ("2.5.4.7", "L"),
    ("2.5.4.8", "ST"),
    ("2.5.4.9", "street"),
    ("2.5.4.10", "O"),
    ("2.5.4.11", "OU"),
    ("2.5.4.12", "title"),
    ("2.5.4.13", "description"),
    ("2.5.4.15", "businessCategory"),
    ("2.5.4.17", "postalCode"),
    ("2.5.4.42", "GN"),
    ("2.5.4.43", "initials"),
    ("2.5.4.44", "generationQualifier"),
    ("2.5.4.46", "dnQualifier"),
    ("2.5.4.65", "pseudonym"),
    ("2.5.4.97", "organizationIdentifier"),
    ("0.9.2342.19200300.100.1.1", "UID"),
    ("0.9.2342.19200300.100.1.25", "DC"),
    ("1.2.840.113549.1.9.1", "emailAddress"),
    ("1.3.6.1.4.1.311.60.2.1.3", "jurisdictionC"),
    // Public key and signature algorithms.
    ("1.2.840.113549.1.1.1", "rsaEncryption"),
    ("1.2.840.113549.1.1.4", "md5WithRSAEncryption"),
    ("1.2.840.113549.1.1.5", "sha1WithRSAEncryption"),
    ("1.2.840.113549.1.1.7", "RSAES-OAEP"),
    ("1.2.840.113549.1.1.8", "MGF1"),
    ("1.2.840.113549.1.1.10", "RSASSA-PSS"),
    ("1.2.840.113549.1.1.11", "sha256WithRSAEncryption"),
    ("1.2.840.113549.1.1.12", "sha384WithRSAEncryption"),
    ("1.2.840.113549.1.1.13", "sha512WithRSAEncryption"),
    ("1.2.840.113549.1.1.14", "sha224WithRSAEncryption"),
    ("1.2.840.10045.2.1", "ecPublicKey"),
    ("1.2.840.10045.4.1", "ecdsa-with-SHA1"),
    ("1.2.840.10045.4.3.1", "ecdsa-with-SHA224"),
    ("1.2.840.10045.4.3.2", "ecdsa-with-SHA256"),
    ("1.2.840.10045.4.3.3", "ecdsa-with-SHA384"),
    ("1.2.840.10045.4.3.4", "ecdsa-with-SHA512"),
    ("1.2.840.10040.4.1", "DSA"),
    ("1.2.840.10040.4.3", "dsa-with-SHA1"),
    ("2.16.840.1.101.3.4.3.2", "dsa-with-SHA256"),
    ("1.2.840.10046.2.1", "dhpublicnumber"),
    ("1.2.840.113549.1.3.1", "dhKeyAgreement"),
    ("1.3.101.110", "X25519"),
    ("1.3.101.111", "X448"),
    ("1.3.101.112", "Ed25519"),
    ("1.3.101.113", "Ed448"),
    // Elliptic curves.
    ("1.2.840.10045.3.1.1", "prime192v1 (P-192)"),
    ("1.3.132.0.33", "secp224r1 (P-224)"),
    ("1.2.840.10045.3.1.7", "prime256v1 (P-256)"),
    ("1.3.132.0.34", "secp384r1 (P-384)"),
    ("1.3.132.0.35", "secp521r1 (P-521)"),
    ("1.3.132.0.10", "secp256k1"),
    ("1.3.36.3.3.2.8.1.1.7", "brainpoolP256r1"),
    ("1.3.36.3.3.2.8.1.1.11", "brainpoolP384r1"),
    ("1.3.36.3.3.2.8.1.1.13", "brainpoolP512r1"),
    // Digests, MACs and ciphers.
    ("1.2.840.113549.2.5", "md5"),
    ("1.3.14.3.2.26", "sha1"),
    ("2.16.840.1.101.3.4.2.1", "sha256"),
    ("2.16.840.1.101.3.4.2.2", "sha384"),
    ("2.16.840.1.101.3.4.2.3", "sha512"),
    ("2.16.840.1.101.3.4.2.4", "sha224"),
    ("2.16.840.1.101.3.4.2.6", "sha512-256"),
    ("2.16.840.1.101.3.4.2.8", "sha3-256"),
    ("2.16.840.1.101.3.4.2.9", "sha3-384"),
    ("2.16.840.1.101.3.4.2.10", "sha3-512"),
    ("1.2.840.113549.2.7", "hmacWithSHA1"),
    ("1.2.840.113549.2.9", "hmacWithSHA256"),
    ("1.2.840.113549.2.10", "hmacWithSHA384"),
    ("1.2.840.113549.2.11", "hmacWithSHA512"),
    ("2.16.840.1.101.3.4.1.2", "aes128-CBC"),
    ("2.16.840.1.101.3.4.1.22", "aes192-CBC"),
    ("2.16.840.1.101.3.4.1.42", "aes256-CBC"),
    ("2.16.840.1.101.3.4.1.6", "aes128-GCM"),
    ("2.16.840.1.101.3.4.1.46", "aes256-GCM"),
    ("1.2.840.113549.3.7", "des-ede3-cbc"),
    ("1.2.840.113549.1.5.12", "PBKDF2"),
    ("1.2.840.113549.1.5.13", "PBES2"),
    ("1.2.840.113549.1.12.1.1", "pbeWithSHAAnd128BitRC4"),
    ("1.2.840.113549.1.12.1.2", "pbeWithSHAAnd40BitRC4"),
    ("1.2.840.113549.1.12.1.3", "pbeWithSHAAnd3-KeyTripleDES-CBC"),
    ("1.2.840.113549.1.12.1.4", "pbeWithSHAAnd2-KeyTripleDES-CBC"),
    ("1.2.840.113549.1.12.1.5", "pbeWithSHAAnd128BitRC2-CBC"),
    ("1.2.840.113549.1.12.1.6", "pbeWithSHAAnd40BitRC2-CBC"),
    // Certificate extensions.
    ("2.5.29.9", "subjectDirectoryAttributes"),
    ("2.5.29.14", "subjectKeyIdentifier"),
    ("2.5.29.15", "keyUsage"),
    ("2.5.29.16", "privateKeyUsagePeriod"),
    ("2.5.29.17", "subjectAltName"),
    ("2.5.29.18", "issuerAltName"),
    ("2.5.29.19", "basicConstraints"),
    ("2.5.29.20", "cRLNumber"),
    ("2.5.29.21", "reasonCode"),
    ("2.5.29.30", "nameConstraints"),
    ("2.5.29.31", "cRLDistributionPoints"),
    ("2.5.29.32", "certificatePolicies"),
    ("2.5.29.32.0", "anyPolicy"),
    ("2.5.29.33", "policyMappings"),
    ("2.5.29.35", "authorityKeyIdentifier"),
    ("2.5.29.36", "policyConstraints"),
    ("2.5.29.37", "extKeyUsage"),
    ("2.5.29.37.0", "anyExtendedKeyUsage"),
    ("2.5.29.46", "freshestCRL"),
    ("2.5.29.54", "inhibitAnyPolicy"),
    ("1.3.6.1.5.5.7.1.1", "authorityInfoAccess"),
    ("1.3.6.1.5.5.7.1.3", "qcStatements"),
    ("1.3.6.1.5.5.7.1.11", "subjectInfoAccess"),
    ("1.3.6.1.5.5.7.48.1", "OCSP"),
    ("1.3.6.1.5.5.7.48.1.5", "ocspNoCheck"),
    ("1.3.6.1.5.5.7.48.2", "caIssuers"),
    ("1.3.6.1.5.5.7.48.3", "timeStamping"),
    ("1.3.6.1.4.1.11129.2.4.2", "signedCertificateTimestampList"),
    ("1.3.6.1.4.1.11129.2.4.3", "precertificatePoison"),
    ("2.16.840.1.113730.1.1", "netscapeCertType"),
    ("2.16.840.1.113730.1.13", "netscapeComment"),
    ("2.23.140.1.1", "extended-validation"),
    ("2.23.140.1.2.1", "domain-validated"),
    ("2.23.140.1.2.2", "organization-validated"),
    ("2.23.140.1.2.3", "individual-validated"),
    // Extended key usages.
    ("1.3.6.1.5.5.7.3.1", "serverAuth"),
    ("1.3.6.1.5.5.7.3.2", "clientAuth"),
    ("1.3.6.1.5.5.7.3.3", "codeSigning"),
    ("1.3.6.1.5.5.7.3.4", "emailProtection"),
    ("1.3.6.1.5.5.7.3.8", "timeStamping"),
    ("1.3.6.1.5.5.7.3.9", "OCSPSigning"),
    ("1.3.6.1.4.1.311.10.3.3", "Microsoft Server Gated Crypto"),
    ("1.3.6.1.4.1.311.10.3.4", "Microsoft Encrypted File System"),
    (
        "1.3.6.1.4.1.311.2.1.21",
        "Microsoft individual code signing",
    ),
    (
        "1.3.6.1.4.1.311.2.1.22",
        "Microsoft commercial code signing",
    ),
    // CMS content types and attributes.
    ("1.2.840.113549.1.7.1", "data"),
    ("1.2.840.113549.1.7.2", "signedData"),
    ("1.2.840.113549.1.7.3", "envelopedData"),
    ("1.2.840.113549.1.7.4", "signedAndEnvelopedData"),
    ("1.2.840.113549.1.7.5", "digestedData"),
    ("1.2.840.113549.1.7.6", "encryptedData"),
    ("1.2.840.113549.1.9.16.1.2", "authenticatedData"),
    ("1.2.840.113549.1.9.16.1.4", "TSTInfo (time-stamp token)"),
    ("1.2.840.113549.1.9.16.1.9", "compressedData"),
    ("1.2.840.113549.1.9.16.1.23", "authEnvelopedData"),
    ("1.3.6.1.4.1.311.2.1.4", "SPC indirect data (Authenticode)"),
    ("1.2.840.113549.1.9.3", "contentType"),
    ("1.2.840.113549.1.9.4", "messageDigest"),
    ("1.2.840.113549.1.9.5", "signingTime"),
    ("1.2.840.113549.1.9.6", "countersignature"),
    ("1.2.840.113549.1.9.7", "challengePassword"),
    ("1.2.840.113549.1.9.14", "extensionRequest"),
    ("1.2.840.113549.1.9.15", "smimeCapabilities"),
    ("1.2.840.113549.1.9.20", "friendlyName"),
    ("1.2.840.113549.1.9.21", "localKeyID"),
    ("1.2.840.113549.1.9.22.1", "x509Certificate"),
    ("1.2.840.113549.1.9.22.2", "sdsiCertificate"),
    ("1.2.840.113549.1.9.23.1", "x509CRL"),
    ("1.2.840.113549.1.9.52", "cmsAlgorithmProtection"),
    ("1.2.840.113549.1.9.16.2.11", "encrypKeyPref"),
    ("1.2.840.113549.1.9.16.2.12", "signingCertificate"),
    ("1.2.840.113549.1.9.16.2.14", "timeStampToken"),
    ("1.2.840.113549.1.9.16.2.15", "sigPolicyId"),
    ("1.2.840.113549.1.9.16.2.16", "commitmentType"),
    ("1.2.840.113549.1.9.16.2.47", "signingCertificateV2"),
    ("1.3.6.1.4.1.311.2.1.12", "SPC SP opus info"),
    ("1.3.6.1.4.1.311.3.3.1", "Microsoft RFC 3161 timestamp"),
    // PKCS#12 bag types.
    ("1.2.840.113549.1.12.10.1.1", "keyBag"),
    ("1.2.840.113549.1.12.10.1.2", "pkcs8ShroudedKeyBag"),
    ("1.2.840.113549.1.12.10.1.3", "certBag"),
    ("1.2.840.113549.1.12.10.1.4", "crlBag"),
    ("1.2.840.113549.1.12.10.1.5", "secretBag"),
    ("1.2.840.113549.1.12.10.1.6", "safeContentsBag"),
];

/// Shape of a DER file, judged from its first elements (cheap: headers only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Certificate,
    CertificateRequest,
    Crl,
    Cms,
    Pkcs12,
    PrivateKey,
    PublicKey,
    Other,
}

/// What a DER buffer (possibly only its head) looks like.
pub fn shape(head: &[u8]) -> Option<Shape> {
    let (class, cons, tag, h0, _) = header(head, 0).ok()?;
    if class != UNIVERSAL || !cons || tag != SEQUENCE {
        return None;
    }
    let kid = |at: usize| header(head, at).ok();
    let (c1, k1, t1, h1, l1) = kid(h0)?;
    let second = h0 + h1 + l1;
    let first_int = |at: usize| -> Option<u64> {
        let (c, _, t, h, l) = header(head, at).ok()?;
        (c == UNIVERSAL && t == INTEGER && l <= 8).then(|| {
            head.get(at + h..at + h + l)
                .unwrap_or(&[])
                .iter()
                .fold(0u64, |a, &b| (a << 8) | u64::from(b))
        })
    };
    let oid_at = |at: usize| -> Option<String> {
        let (c, _, t, h, l) = header(head, at).ok()?;
        (c == UNIVERSAL && t == OID)
            .then(|| oid_text(head.get(at + h..at + h + l)?))
            .flatten()
    };
    // ContentInfo: SEQUENCE { contentType OID, [0] content }.
    if let Some(o) = oid_at(h0) {
        if o.starts_with("1.2.840.113549.1.7.") || o.starts_with("1.2.840.113549.1.9.16.1.") {
            return Some(Shape::Cms);
        }
        return Some(Shape::Other);
    }
    // PFX: SEQUENCE { version 3, authSafe ContentInfo, ... }.
    if first_int(h0) == Some(3)
        && let Some((UNIVERSAL, true, SEQUENCE, hh, _)) = kid(second)
        && oid_at(second + hh).is_some_and(|o| o.starts_with("1.2.840.113549.1.7."))
    {
        return Some(Shape::Pkcs12);
    }
    if c1 == UNIVERSAL && k1 && t1 == SEQUENCE {
        // SubjectPublicKeyInfo: SEQUENCE { AlgorithmIdentifier, BIT STRING }.
        if oid_at(h0 + h1).is_some() {
            if let Some((UNIVERSAL, false, BIT_STRING, ..)) = kid(second) {
                return Some(Shape::PublicKey);
            }
            // EncryptedPrivateKeyInfo: SEQUENCE { AlgorithmIdentifier, OCTET STRING }.
            if let Some((UNIVERSAL, false, OCTET_STRING, ..)) = kid(second) {
                return Some(Shape::PrivateKey);
            }
        }
        // Signed structures: SEQUENCE { tbs SEQUENCE, AlgorithmIdentifier, BIT STRING }.
        let tbs = h0 + h1;
        let mut at = tbs;
        let (cv, kv, tv, hv, lv) = kid(at)?;
        if cv == CONTEXT && kv && tv == 0 {
            // [0] EXPLICIT version: a certificate (v2/v3).
            at += hv + lv;
            let (ci, _, ti, ..) = kid(at)?;
            return Some(if ci == UNIVERSAL && ti == INTEGER {
                Shape::Certificate
            } else {
                Shape::Other
            });
        }
        if cv == UNIVERSAL && tv == INTEGER {
            at += hv + lv;
            let (cn, kn, tn, hn, _) = kid(at)?;
            if cn == UNIVERSAL && kn && tn == SEQUENCE {
                // v1 certificate: serial, then AlgorithmIdentifier { OID }.
                // CSR: version, then Name { SET ... }. CRL v2: version, AlgorithmIdentifier, Name.
                if oid_at(at + hn).is_some() {
                    // After the AlgorithmIdentifier and the issuer Name: validity (a SEQUENCE)
                    // for a certificate, thisUpdate (a time) for a CRL v2.
                    let after = || -> Option<u32> {
                        let (_, _, _, h2, l2) = kid(at)?;
                        let name = at + h2 + l2;
                        let (_, _, _, h3, l3) = kid(name)?;
                        kid(name + h3 + l3).map(|k| k.2)
                    };
                    return Some(match after() {
                        Some(23) | Some(24) => Shape::Crl,
                        _ => Shape::Certificate,
                    });
                }
                if let Some((UNIVERSAL, true, SET, ..)) = kid(at + hn) {
                    return Some(Shape::CertificateRequest);
                }
            }
            return Some(Shape::Other);
        }
        if cv == UNIVERSAL && kv && tv == SEQUENCE && oid_at(at + hv).is_some() {
            // CRL v1: AlgorithmIdentifier first, then the issuer Name.
            return Some(Shape::Crl);
        }
        return Some(Shape::Other);
    }
    // Private keys: PKCS#8 { 0|1, AlgorithmIdentifier, OCTET STRING }, PKCS#1 RSA
    // { 0, n, e, d, ... }, SEC1 EC { 1, OCTET STRING, ... }.
    if let Some(v) = first_int(h0)
        && v <= 1
    {
        match kid(second) {
            Some((UNIVERSAL, true, SEQUENCE, ..)) => return Some(Shape::PrivateKey),
            Some((UNIVERSAL, false, INTEGER, ..)) | Some((UNIVERSAL, false, OCTET_STRING, ..)) => {
                return Some(Shape::PrivateKey);
            }
            _ => {}
        }
    }
    Some(Shape::Other)
}

/// Adds the outline of `t` and its descendants, within the depth and size limits.
pub fn outline(t: &Tlv, depth: usize, out: &mut Vec<OutlineNode>) {
    if out.len() >= MAX_OUTLINE {
        return;
    }
    out.push(OutlineNode {
        depth: depth as u16,
        label: tag_name(t),
        value: t.describe(),
        source: Some(t.range()),
    });
    if depth >= MAX_DEPTH {
        return;
    }
    if t.constructed {
        for c in t.children() {
            outline(&c, depth + 1, out);
        }
    } else if (t.universal(OCTET_STRING) || t.universal(BIT_STRING))
        && let Some(inner) = t.inner()
        && inner.constructed
        && inner.class == UNIVERSAL
    {
        // DER wrapped in a string (extension values, encapsulated content).
        outline(&inner, depth + 1, out);
    }
}

/// `.der` and `.cer` files whose structure no specific handler claims.
pub struct GenericDer;
impl Handler for GenericDer {
    fn name(&self) -> &'static str {
        "ASN.1 DER"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if !probe.ext_is(&[
            "der", "cer", "crt", "crl", "csr", "p10", "pkcs8", "key", "asn1",
        ]) {
            return false;
        }
        // The first element must span the whole file (a trailing newline is tolerated).
        let Ok((UNIVERSAL, true, _, h, l)) = header(probe.head, 0) else {
            return false;
        };
        let total = (h as u64).saturating_add(l as u64);
        total <= probe.size && probe.size - total <= 2
    }
    fn inspect(&self, input: &Input, _cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        let top = root(b).map_err(|e| format!("Not readable as DER: {e}"))?;
        let shape = shape(b).unwrap_or(Shape::Other);
        if shape == Shape::PrivateKey {
            return Ok(private_key_only(&top));
        }
        let mut out = Inspection::new("ASN.1", "DER-encoded structure");
        if shape == Shape::CertificateRequest
            && let Ok(()) = super::x509::csr_facts(&top, &mut out, "Request")
        {
            out.kind = "PKCS#10 certificate signing request (DER)".into();
            out.check(
                Stage::Signature,
                State::NotDone,
                "The request's signature was not verified",
            );
        }
        if shape == Shape::PublicKey {
            out.kind = "Public key (SubjectPublicKeyInfo, DER)".into();
            let (alg, size) = super::x509::public_key(&top);
            out.fact("Public key", "Algorithm", alg, Some(top.range()));
            out.fact("Public key", "Size", size, None);
        }
        if shape == Shape::Crl {
            out.kind = "Certificate revocation list (DER)".into();
            super::x509::crl_facts(&top, &mut out, "CRL");
            out.check(
                Stage::Signature,
                State::NotDone,
                "The list's signature was not verified",
            );
        }
        outline(&top, 0, &mut out.outline);
        out.fact(
            "Structure",
            "Top element",
            tag_name(&top),
            Some(top.range()),
        );
        out.fact(
            "Structure",
            "Elements shown",
            out.outline.len().to_string(),
            None,
        );
        let damaged = scan_errors(&top, 0);
        match damaged {
            None => out.check(
                Stage::Parsed,
                State::Passed,
                "Read as DER; the structure is shown without interpreting it against a schema",
            ),
            Some(e) => out.check(Stage::Parsed, State::Partial, format!("Damaged: {e}")),
        }
        if out.outline.len() >= MAX_OUTLINE {
            out.notes
                .push(format!("Only the first {MAX_OUTLINE} elements are shown"));
        }
        out.not_validated();
        Ok(out)
    }
}

/// The first structural error under `t`, if any.
pub fn scan_errors(t: &Tlv, depth: usize) -> Option<String> {
    if !t.constructed || depth >= MAX_DEPTH {
        return None;
    }
    let mut it = t.children();
    for c in it.by_ref() {
        if let Some(e) = scan_errors(&c, depth + 1) {
            return Some(e);
        }
    }
    it.error
}

/// A private key in DER: its kind and nothing of its content.
pub fn private_key_only(top: &Tlv) -> Inspection {
    let mut out = Inspection::new("ASN.1", "Private key (DER)");
    let kind = private_key_kind(top);
    out.fact("Private key", "Kind", kind, Some(top.range()));
    out.fact(
        "Private key",
        "Size of the structure",
        super::util::bytes_text(top.len as u64),
        None,
    );
    out.notes.push(PRIVATE_NOTE.into());
    out.check(
        Stage::Parsed,
        State::Partial,
        "Recognized as a private key; its content is deliberately not shown",
    );
    out.not_validated();
    out
}

pub const PRIVATE_NOTE: &str = "This is a private key. Isolinear shows only its type and never \
displays, copies or decodes the key material.";

/// "PKCS#8 private key (rsaEncryption)", "Encrypted PKCS#8 private key (PBES2)", …
pub fn private_key_kind(top: &Tlv) -> String {
    let kids = top.kids();
    match kids.as_slice() {
        [alg, data] if alg.universal(SEQUENCE) && data.universal(OCTET_STRING) => {
            let a = alg.child(0).and_then(|o| o.oid()).unwrap_or_default();
            format!("Encrypted PKCS#8 private key ({})", oid_label(&a))
        }
        [v, alg, key, ..]
            if v.universal(INTEGER) && alg.universal(SEQUENCE) && key.universal(OCTET_STRING) =>
        {
            let a = alg.child(0).and_then(|o| o.oid()).unwrap_or_default();
            format!("PKCS#8 private key ({})", oid_label(&a))
        }
        [v, key, ..] if v.universal(INTEGER) && key.universal(OCTET_STRING) => {
            "EC private key (SEC 1)".into()
        }
        [v, n, ..] if v.universal(INTEGER) && n.universal(INTEGER) && kids.len() >= 9 => {
            "RSA private key (PKCS#1)".into()
        }
        [v, ..] if v.universal(INTEGER) && kids.len() == 6 => "DSA private key".into(),
        _ => "Private key".into(),
    }
}
