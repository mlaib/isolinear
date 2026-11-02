//! W3C XML Signatures and their ETSI XAdES qualifying properties: algorithms, references with
//! their transforms and digests, the key information given, and the signed properties (signing
//! time, certificate digest, policy, role). Nothing is verified: no digest is recomputed and no
//! signature value is checked.
use super::support::{find_all, value};
use crate::inspect::{
    Inspection, Stage, State,
    spec::{F, T, XmlSpec, select},
    xmldoc::Doc,
};
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};

pub const DSIG_NS: &str = "http://www.w3.org/2000/09/xmldsig#";
const XADES_PREFIX: &str = "http://uri.etsi.org/01903/";

/// Whether a `ds:Signature` element starts within the first levels of the document (the root's
/// children or grandchildren), read from the start of the file only.
pub fn signature_near_root(head: &str) -> bool {
    if !head.contains(DSIG_NS) {
        return false;
    }
    let mut reader = NsReader::from_str(head);
    let mut depth = 0usize;
    for _ in 0..200_000 {
        match reader.read_resolved_event() {
            Ok((resolved, Event::Start(tag))) => {
                depth += 1;
                if depth <= 3
                    && tag.local_name().as_ref() == b"Signature"
                    && matches!(resolved, ResolveResult::Bound(ns) if ns.as_ref() == DSIG_NS.as_bytes())
                {
                    return true;
                }
            }
            Ok((resolved, Event::Empty(tag))) => {
                if depth < 3
                    && tag.local_name().as_ref() == b"Signature"
                    && matches!(resolved, ResolveResult::Bound(ns) if ns.as_ref() == DSIG_NS.as_bytes())
                {
                    return true;
                }
            }
            Ok((_, Event::End(_))) => depth = depth.saturating_sub(1),
            Ok((_, Event::Eof)) | Err(_) => return false,
            _ => {}
        }
    }
    false
}

/// Short names of the common algorithm URIs.
fn algorithm(uri: &str) -> Option<&'static str> {
    if uri.contains("xml-exc-c14n") {
        return Some(if uri.ends_with("WithComments") {
            "Exclusive canonical XML with comments"
        } else {
            "Exclusive canonical XML"
        });
    }
    let trimmed = uri.trim_end_matches('#');
    let frag = trimmed.rsplit(['#', '/']).next().unwrap_or(trimmed);
    Some(match frag {
        "rsa-sha1" => "RSA with SHA-1",
        "rsa-sha256" => "RSA with SHA-256",
        "rsa-sha384" => "RSA with SHA-384",
        "rsa-sha512" => "RSA with SHA-512",
        "sha256-rsa-MGF1" => "RSASSA-PSS with SHA-256",
        "ecdsa-sha256" => "ECDSA with SHA-256",
        "ecdsa-sha384" => "ECDSA with SHA-384",
        "ecdsa-sha512" => "ECDSA with SHA-512",
        "dsa-sha1" => "DSA with SHA-1",
        "hmac-sha1" => "HMAC-SHA1",
        "hmac-sha256" => "HMAC-SHA256",
        "sha1" => "SHA-1",
        "sha256" => "SHA-256",
        "sha384" => "SHA-384",
        "sha512" => "SHA-512",
        "REC-xml-c14n-20010315" => "Canonical XML 1.0",
        "WithComments" => "Canonical XML 1.0 with comments",
        "xml-c14n11" => "Canonical XML 1.1",
        "xml-exc-c14n" => "Exclusive canonical XML",
        "enveloped-signature" => "Enveloped signature",
        "base64" => "Base64",
        _ => return None,
    })
}

fn named(out: &mut Inspection, group: &str, label: &str, uri: Option<String>) {
    if let Some(name) = uri.as_deref().and_then(algorithm) {
        out.fact(group, label, name, None);
    }
}

fn custom(doc: &Doc, out: &mut Inspection) {
    let signatures = find_all(doc, "Signature", DSIG_NS);
    let xades = doc.elements.iter().any(|e| e.ns.starts_with(XADES_PREFIX));
    out.fact(
        "Signature",
        "Signatures",
        signatures.len().to_string(),
        None,
    );
    named(
        out,
        "Signature",
        "Signature algorithm",
        value(doc, 0, "**/SignedInfo/SignatureMethod/@Algorithm"),
    );
    named(
        out,
        "Signature",
        "Canonicalization",
        value(doc, 0, "**/SignedInfo/CanonicalizationMethod/@Algorithm"),
    );
    let certs = select(doc, 0, "**/KeyInfo/X509Data/X509Certificate").len();
    out.fact(
        "Key",
        "Certificate included",
        if certs > 0 {
            format!("yes ({certs})")
        } else {
            "no".into()
        },
        None,
    );
    for (path, kind) in [
        ("**/KeyInfo/KeyValue/RSAKeyValue", "RSA public key"),
        ("**/KeyInfo/KeyValue/ECKeyValue", "EC public key"),
        ("**/KeyInfo/KeyValue/DSAKeyValue", "DSA public key"),
    ] {
        if !select(doc, 0, path).is_empty() {
            out.fact("Key", "Key value", kind, None);
            break;
        }
    }
    let root = doc.root();
    if root.name != "Signature" || root.ns != DSIG_NS {
        out.kind = format!("XML document <{}> with an XML signature", root.name);
        out.fact(
            "Document",
            "Root element",
            if root.ns.is_empty() {
                root.name.clone()
            } else {
                format!("{} ({})", root.name, root.ns)
            },
            None,
        );
    }
    if xades {
        out.family = "XAdES".into();
        let has = |name: &str| {
            doc.elements
                .iter()
                .any(|e| e.name == name && e.ns.starts_with(XADES_PREFIX))
        };
        let level = if has("ArchiveTimeStamp") {
            "LTA"
        } else if has("CertificateValues") && has("RevocationValues") {
            "LT"
        } else if has("SignatureTimeStamp") {
            "T"
        } else {
            "B"
        };
        let policy = if has("SignaturePolicyId") {
            ", with an explicit policy (EPES)"
        } else {
            ""
        };
        out.fact(
            "XAdES",
            "Level (from the properties present)",
            format!("{level}{policy}"),
            None,
        );
        if !out.kind.contains("document <") {
            out.kind = "XAdES signature".into();
        }
        out.version = doc
            .elements
            .iter()
            .find(|e| e.ns.starts_with(XADES_PREFIX))
            .map(|e| {
                format!(
                    "XAdES {}",
                    e.ns.trim_start_matches(XADES_PREFIX).trim_end_matches('#')
                )
            });
    }
    out.check(
        Stage::Signature,
        State::NotDone,
        "Signatures were not verified",
    );
    out.notes.push(
        "Digests are shown as written and were not recomputed; certificates are not decoded or checked against a trust list"
            .into(),
    );
}

const FACTS: &[F] = &[
    F("Signature", "ID", "**/SignedInfo/../@Id"),
    F(
        "Signature",
        "Signature method",
        "**/SignedInfo/SignatureMethod/@Algorithm",
    ),
    F(
        "Signature",
        "Canonicalization method",
        "**/SignedInfo/CanonicalizationMethod/@Algorithm",
    ),
    F("Signature", "References", "count:**/SignedInfo/Reference"),
    F("Key", "Key name", "**/KeyInfo/KeyName"),
    F("Key", "Subject", "**/KeyInfo/X509Data/X509SubjectName"),
    F(
        "Key",
        "Issuer",
        "**/KeyInfo/X509Data/X509IssuerSerial/X509IssuerName",
    ),
    F(
        "Key",
        "Serial number",
        "**/KeyInfo/X509Data/X509IssuerSerial/X509SerialNumber",
    ),
    F(
        "XAdES",
        "Signing time",
        "**/SignedSignatureProperties/SigningTime",
    ),
    F(
        "XAdES",
        "Signing certificate digest method",
        "**/SignedSignatureProperties/SigningCertificate/Cert/CertDigest/DigestMethod/@Algorithm | **/SignedSignatureProperties/SigningCertificateV2/Cert/CertDigest/DigestMethod/@Algorithm",
    ),
    F(
        "XAdES",
        "Signing certificate digest",
        "**/SignedSignatureProperties/SigningCertificate/Cert/CertDigest/DigestValue | **/SignedSignatureProperties/SigningCertificateV2/Cert/CertDigest/DigestValue",
    ),
    F(
        "XAdES",
        "Signing certificate issuer",
        "**/SignedSignatureProperties/SigningCertificate/Cert/IssuerSerial/X509IssuerName",
    ),
    F(
        "XAdES",
        "Policy",
        "**/SignaturePolicyIdentifier/SignaturePolicyId/SigPolicyId/Identifier",
    ),
    F(
        "XAdES",
        "Policy description",
        "**/SignaturePolicyIdentifier/SignaturePolicyId/SigPolicyId/Description",
    ),
    F(
        "XAdES",
        "Signer role",
        "**/SignerRole/ClaimedRoles/ClaimedRole | **/SignerRoleV2/ClaimedRoles/ClaimedRole",
    ),
    F(
        "XAdES",
        "Production place",
        "**/SignatureProductionPlace/City | **/SignatureProductionPlaceV2/City",
    ),
    F(
        "XAdES",
        "Production country",
        "**/SignatureProductionPlace/CountryName | **/SignatureProductionPlaceV2/CountryName",
    ),
    F(
        "XAdES",
        "Signed data format",
        "**/SignedDataObjectProperties/DataObjectFormat/MimeType",
    ),
    F(
        "XAdES",
        "Commitment",
        "**/CommitmentTypeIndication/CommitmentTypeId/Identifier",
    ),
    F(
        "XAdES",
        "Signature time-stamps",
        "count:**/UnsignedSignatureProperties/SignatureTimeStamp",
    ),
];

const TABLES: &[T] = &[
    T {
        name: "References",
        rows: "**/SignedInfo/Reference",
        columns: &[
            ("URI", "@URI"),
            ("Type", "@Type"),
            ("ID", "@Id"),
            ("First transform", "Transforms/Transform/@Algorithm"),
            ("Transforms", "count:Transforms/Transform"),
            ("Digest method", "DigestMethod/@Algorithm"),
            ("Digest value", "DigestValue"),
        ],
    },
    T {
        name: "Certificates",
        rows: "**/KeyInfo/X509Data",
        columns: &[
            ("Subject", "X509SubjectName"),
            ("Issuer", "X509IssuerSerial/X509IssuerName"),
            ("Serial", "X509IssuerSerial/X509SerialNumber"),
            ("Certificates", "count:X509Certificate"),
        ],
    },
];

pub static SIGNATURE: XmlSpec = XmlSpec {
    family: "XML-DSig",
    kind: "XML signature (detached or enveloping)",
    matches: |root, ns| root == "Signature" && ns == DSIG_NS,
    version: |_| Some("XML Signature 1.x".into()),
    facts: FACTS,
    tables: TABLES,
    custom: Some(custom),
};

/// A document of no known standard that carries a signature near its root.
pub static EMBEDDED: XmlSpec = XmlSpec {
    family: "XML-DSig",
    kind: "XML document with an XML signature",
    matches: |_, _| true,
    version: |_| Some("XML Signature 1.x".into()),
    facts: FACTS,
    tables: TABLES,
    custom: Some(custom),
};
