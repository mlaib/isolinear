//! Associated Signature Containers, extended form (ASiC-E, ETSI EN 319 162; `.asice`, `.sce`):
//! a ZIP with a `mimetype` entry, the signed files, and `META-INF/signatures*.xml` holding
//! XAdES signatures. The entries, the manifest and what each signature claims (signing time,
//! signer certificate, referenced files and digest algorithms) are shown. Signatures are not
//! verified, and the container is never modified.
use super::doc_common::{Zip, base64, clip, human, push_row, table};
use super::doc_der;
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State, spec, xmldoc::Doc};

const MIME_E: &str = "application/vnd.etsi.asic-e+zip";
const MIME_S: &str = "application/vnd.etsi.asic-s+zip";
const MAX_SIGNATURE_FILES: usize = 100;

pub struct Asice;

fn short_algorithm(uri: &str) -> String {
    let tail = uri.rsplit(['#', '/']).next().unwrap_or(uri);
    match tail {
        "sha256" => "SHA-256".into(),
        "sha384" => "SHA-384".into(),
        "sha512" => "SHA-512".into(),
        "sha1" => "SHA-1".into(),
        "sha224" => "SHA-224".into(),
        "rsa-sha256" => "RSA with SHA-256".into(),
        "rsa-sha512" => "RSA with SHA-512".into(),
        "ecdsa-sha256" => "ECDSA with SHA-256".into(),
        "ecdsa-sha384" => "ECDSA with SHA-384".into(),
        _ => tail.to_owned(),
    }
}

/// A reference URI as an entry name: `./dir/a%20b.txt` → `dir/a b.txt`.
fn entry_name(uri: &str) -> String {
    let uri = uri.trim_start_matches("./");
    let mut out = Vec::with_capacity(uri.len());
    let b = uri.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = uri
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Handler for Asice {
    fn name(&self) -> &'static str {
        "ASiC-E signature container"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let head = &probe.head[..probe.head.len().min(256)];
        probe.head.starts_with(b"PK\x03\x04")
            && (probe.head.get(30..38) == Some(b"mimetype")
                && (memchr::memmem::find(head, MIME_E.as_bytes()).is_some()
                    || memchr::memmem::find(head, MIME_S.as_bytes()).is_some())
                || probe.ext_is(&["asice", "sce"]))
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let mut zip = Zip::open(input.bytes, cancelled)?;
        let mimetype = zip
            .part("mimetype")?
            .map(|m| String::from_utf8_lossy(&m).trim().to_owned())
            .unwrap_or_default();
        let kind = match mimetype.as_str() {
            MIME_E => "ASiC-E — extended associated signature container",
            MIME_S => "ASiC-S — simple associated signature container",
            _ => "Associated signature container (no ASiC mimetype)",
        };
        let mut out = Inspection::new("ETSI ASiC", kind);
        out.version = Some("ETSI EN 319 162".into());
        out.fact(
            "Container",
            "Media type",
            mimetype.clone(),
            zip.span("mimetype"),
        );
        out.fact(
            "Container",
            "Entries",
            zip.listing.entries.len().to_string(),
            None,
        );
        let first = zip.listing.entries.first().cloned();
        let first_ok = first
            .as_ref()
            .is_some_and(|e| e.name == "mimetype" && e.method_id == 0);
        out.check(
            Stage::Semantic,
            if first_ok && !mimetype.is_empty() {
                State::Passed
            } else {
                State::Failed
            },
            if first_ok {
                "The mimetype entry comes first and is stored uncompressed".to_owned()
            } else {
                "The mimetype entry must be the first entry, stored uncompressed".to_owned()
            },
        );

        // Manifest.
        if let Some(m) = zip.part("META-INF/manifest.xml")? {
            let text = String::from_utf8_lossy(&m).into_owned();
            if let Ok(doc) = Doc::parse(&text, cancelled) {
                let mut t = table("Manifest", &["Path", "Media type"]);
                for e in spec::select(&doc, 0, "**/file-entry") {
                    push_row(
                        &mut t,
                        vec![
                            doc.attr(e, "full-path").unwrap_or("").to_owned(),
                            doc.attr(e, "media-type").unwrap_or("").to_owned(),
                        ],
                        zip.span("META-INF/manifest.xml"),
                    );
                }
                out.tables.push(t);
            }
        }

        // Signatures.
        let signature_files: Vec<String> = zip
            .listing
            .entries
            .iter()
            .filter(|e| {
                let n = e.name.to_ascii_lowercase();
                n.starts_with("meta-inf/") && n.contains("signature") && n.ends_with(".xml")
            })
            .map(|e| e.name.clone())
            .take(MAX_SIGNATURE_FILES)
            .collect();
        let mut signatures = table(
            "Signatures",
            &[
                "File",
                "Signature ID",
                "Signing time",
                "Signer certificate subject",
                "Certificate issuer",
                "Certificate valid",
                "Method",
                "References",
                "Timestamp",
            ],
        );
        let mut references = table(
            "Signed references",
            &["Signature file", "URI", "Digest algorithm", "Digest value"],
        );
        let mut signed: Vec<String> = vec![];
        let mut unreadable = vec![];
        for file in &signature_files {
            if cancelled() {
                return Err("Cancelled".into());
            }
            let Some(bytes) = zip.part(file)? else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let doc = match Doc::parse(&text, cancelled) {
                Ok(d) => d,
                Err(e) => {
                    unreadable.push(format!("{file}: {e}"));
                    continue;
                }
            };
            let span = zip.span(file);
            for sig in spec::select(&doc, 0, "**/Signature") {
                let val = |p: &str| {
                    spec::value_at(&doc, sig, p)
                        .map(|(v, _)| v)
                        .unwrap_or_default()
                };
                let refs = spec::select(&doc, sig, "SignedInfo/Reference");
                let mut count = 0;
                for r in &refs {
                    let uri = doc.attr(*r, "URI").unwrap_or("").to_owned();
                    let typ = doc.attr(*r, "Type").unwrap_or("");
                    if typ.ends_with("#SignedProperties") || uri.starts_with('#') {
                        continue;
                    }
                    count += 1;
                    let name = entry_name(&uri);
                    if !signed.contains(&name) {
                        signed.push(name);
                    }
                    push_row(
                        &mut references,
                        vec![
                            file.clone(),
                            uri,
                            spec::value_at(&doc, *r, "DigestMethod/@Algorithm")
                                .map(|(v, _)| short_algorithm(&v))
                                .unwrap_or_default(),
                            clip(
                                &spec::value_at(&doc, *r, "DigestValue")
                                    .map(|(v, _)| v)
                                    .unwrap_or_default(),
                                60,
                            ),
                        ],
                        span.clone(),
                    );
                }
                let cert = spec::value_at(&doc, sig, "KeyInfo/X509Data/X509Certificate")
                    .and_then(|(b64, _)| doc_der::certificate(&base64(b64.as_bytes(), 1 << 20).0));
                let (subject, issuer, valid) = match &cert {
                    Some(c) => (
                        c.subject.clone(),
                        c.issuer.clone(),
                        format!("{} to {}", c.not_before, c.not_after),
                    ),
                    None => (
                        val("**/X509SubjectName"),
                        val("**/X509IssuerName"),
                        String::new(),
                    ),
                };
                let timestamp = !spec::select(&doc, sig, "**/SignatureTimeStamp").is_empty();
                push_row(
                    &mut signatures,
                    vec![
                        file.clone(),
                        doc.attr(sig, "Id").unwrap_or("").to_owned(),
                        val("**/SigningTime"),
                        subject,
                        issuer,
                        valid,
                        spec::value_at(&doc, sig, "SignedInfo/SignatureMethod/@Algorithm")
                            .map(|(v, _)| short_algorithm(&v))
                            .unwrap_or_default(),
                        count.to_string(),
                        if timestamp {
                            "present (not verified)"
                        } else {
                            ""
                        }
                        .into(),
                    ],
                    span.clone(),
                );
            }
        }
        out.fact(
            "Signatures",
            "Signature files",
            signature_files.len().to_string(),
            None,
        );
        out.fact(
            "Signatures",
            "Signatures",
            signatures.rows.len().to_string(),
            None,
        );
        if let Some(first) = signatures.rows.first() {
            out.fact(
                "Signatures",
                "First signing time",
                first.cells[2].clone(),
                first.source.clone(),
            );
            out.fact(
                "Signatures",
                "First signer name",
                first.cells[3].clone(),
                first.source.clone(),
            );
        }

        let mut entries = table(
            "Entries",
            &[
                "Name",
                "Size",
                "Packed",
                "Compression",
                "Modified",
                "Signed",
            ],
        );
        let mut unsigned = vec![];
        for e in &zip.listing.entries {
            let meta = e.name.starts_with("META-INF/") || e.name == "mimetype" || e.dir;
            let is_signed = signed.iter().any(|s| s == &e.name);
            if !meta && !is_signed {
                unsigned.push(e.name.clone());
            }
            push_row(
                &mut entries,
                vec![
                    e.name.clone(),
                    e.size.to_string(),
                    e.packed.map(|p| p.to_string()).unwrap_or_default(),
                    e.method.clone(),
                    e.modified.clone(),
                    if meta {
                        "—".into()
                    } else if is_signed {
                        "yes".into()
                    } else {
                        "no".into()
                    },
                ],
                zip.span(&e.name),
            );
        }
        out.fact(
            "Container",
            "Size of entries",
            human(zip.listing.total_size()),
            None,
        );
        out.tables.insert(0, entries);
        if !signatures.rows.is_empty() {
            out.tables.push(signatures);
        }
        if !references.rows.is_empty() {
            out.tables.push(references);
        }
        let missing: Vec<&String> = signed
            .iter()
            .filter(|s| !zip.listing.entries.iter().any(|e| &e.name == *s))
            .collect();
        if signature_files.is_empty() {
            out.check(
                Stage::Semantic,
                State::Failed,
                "The container holds no signature file",
            );
        } else if unsigned.is_empty() && missing.is_empty() {
            out.check(
                Stage::Semantic,
                State::Passed,
                "Every data file is referenced by a signature",
            );
        } else {
            let mut problems = vec![];
            if !unsigned.is_empty() {
                problems.push(format!(
                    "{} files are not covered by any signature: {}",
                    unsigned.len(),
                    clip(&unsigned.join(", "), 200)
                ));
            }
            if !missing.is_empty() {
                problems.push(format!(
                    "signatures reference {} files missing from the container",
                    missing.len()
                ));
            }
            out.check(Stage::Semantic, State::Failed, problems.join("; "));
        }
        if unreadable.is_empty() {
            out.check(
                Stage::Parsed,
                State::Passed,
                "Container, manifest and signature files read",
            );
        } else {
            out.check(
                Stage::Parsed,
                State::Partial,
                format!(
                    "Some signature files could not be read: {}",
                    unreadable.join("; ")
                ),
            );
        }
        out.not_validated();
        out.check(
            Stage::Signature,
            State::NotDone,
            "Signatures were not verified",
        );
        out.notes.push("Digest values and signatures are shown as written; Isolinear did not recompute digests, verify signatures or check certificates and revocation".into());
        Ok(out)
    }
}
