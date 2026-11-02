//! SPDX software bills of materials: SPDX 2.x JSON (`spdxVersion`), SPDX 2.x tag-value text
//! (`SPDXVersion: SPDX-2.3`), and SPDX 3 JSON-LD (read in part). All three are turned into one
//! small model, shown the same way.
use super::records::{Rows, join, nodes, s, span, str_after};
use crate::inspect::{
    Handler, Input, Inspection, Probe, Stage, State, jsondoc::Node, spec::JsonSpec,
};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

#[derive(Default)]
struct Item {
    id: String,
    name: String,
    version: String,
    supplier: String,
    download: String,
    concluded: String,
    declared: String,
    checksums: Vec<String>,
    purls: Vec<String>,
    other_refs: Vec<String>,
    span: Option<Range<u64>>,
}

#[derive(Default)]
struct Model {
    /// (label, value, source) for the document facts.
    header: Vec<(&'static str, String, Option<Range<u64>>)>,
    packages: Vec<Item>,
    files: Vec<Item>,
    snippets: Vec<String>,
    licenses: Vec<String>,
    relationships: Vec<(String, String, String, Option<Range<u64>>)>,
    document_id: String,
    /// Lines or records that could not be read.
    skipped: usize,
}

pub static SPDX_JSON: JsonSpec = JsonSpec {
    family: "SPDX",
    kind: "Software bill of materials",
    matches: |keys, head| {
        (keys.iter().any(|k| k == "spdxVersion")
            && str_after(head, "spdxVersion").is_some_and(|v| v.starts_with("SPDX-")))
            || (keys.iter().any(|k| k == "@context") && head.contains("spdx.org/rdf/3"))
    },
    version: |root| {
        let v = s(root, "spdxVersion");
        if !v.is_empty() {
            return Some(v);
        }
        let ctx = s(root, "@context");
        ctx.split('/')
            .find(|p| p.starts_with("3."))
            .map(|v| format!("SPDX {v} (JSON-LD)"))
            .or(Some("SPDX 3 (JSON-LD)".into()))
    },
    facts: &[],
    tables: &[],
    custom: Some(|root, out| {
        if root.get("spdxVersion").is_some() {
            render(&from_json2(root), out, true);
        } else {
            render(&from_json3(root), out, false);
            out.check(
                Stage::Parsed,
                State::Partial,
                "SPDX 3 is read in part: the document, packages, files, relationships and licence relationships; profiles such as security, build, AI and dataset are not interpreted",
            );
            out.notes
                .push("SPDX 3 JSON-LD support is partial (see the Parsed check)".into());
        }
    }),
};

fn checksums(n: &Node) -> Vec<String> {
    nodes(n, "checksums")
        .into_iter()
        .map(|c| join(&[s(c, "algorithm"), s(c, "checksumValue")], ": "))
        .collect()
}

fn from_json2(root: &Node) -> Model {
    let mut m = Model::default();
    for (label, path) in [
        ("SPDX version", "spdxVersion"),
        ("Document name", "name"),
        ("Namespace", "documentNamespace"),
        ("Data licence", "dataLicense"),
        ("SPDX ID", "SPDXID"),
        ("Created", "creationInfo/created"),
        ("Creators", "creationInfo/creators"),
        ("Licence list version", "creationInfo/licenseListVersion"),
        ("Describes", "documentDescribes"),
        ("Comment", "comment"),
    ] {
        if let Some(n) = nodes(root, path).first() {
            m.header.push((label, s(root, path), span(n)));
        }
    }
    m.document_id = s(root, "SPDXID");
    for p in nodes(root, "packages") {
        let mut item = Item {
            id: s(p, "SPDXID"),
            name: s(p, "name"),
            version: s(p, "versionInfo"),
            supplier: s(p, "supplier"),
            download: s(p, "downloadLocation"),
            concluded: s(p, "licenseConcluded"),
            declared: s(p, "licenseDeclared"),
            checksums: checksums(p),
            span: span(p),
            ..Item::default()
        };
        for r in nodes(p, "externalRefs") {
            let (ty, loc) = (s(r, "referenceType"), s(r, "referenceLocator"));
            if ty == "purl" {
                item.purls.push(loc);
            } else {
                item.other_refs.push(format!("{ty}: {loc}"));
            }
        }
        m.packages.push(item);
    }
    for f in nodes(root, "files") {
        m.files.push(Item {
            id: s(f, "SPDXID"),
            name: s(f, "fileName"),
            concluded: s(f, "licenseConcluded"),
            declared: s(f, "licenseInfoInFiles"),
            checksums: checksums(f),
            span: span(f),
            ..Item::default()
        });
    }
    m.snippets = nodes(root, "snippets/SPDXID")
        .into_iter()
        .map(|n| n.text())
        .collect();
    m.licenses = nodes(root, "hasExtractedLicensingInfos/licenseId")
        .into_iter()
        .map(|n| n.text())
        .collect();
    for r in nodes(root, "relationships") {
        m.relationships.push((
            s(r, "spdxElementId"),
            s(r, "relationshipType"),
            s(r, "relatedSpdxElement"),
            span(r),
        ));
    }
    // documentDescribes is a DESCRIBES relationship of the document.
    for d in nodes(root, "documentDescribes") {
        let target = d.text();
        if !m
            .relationships
            .iter()
            .any(|(a, t, b, _)| *a == m.document_id && t == "DESCRIBES" && *b == target)
        {
            m.relationships
                .push((m.document_id.clone(), "DESCRIBES".into(), target, span(d)));
        }
    }
    m
}

/// The type of an SPDX 3 element (`type` or `@type`), without a namespace prefix.
fn type3(n: &Node) -> String {
    let t = s(n, "type | @type");
    t.rsplit(['/', '#']).next().unwrap_or(&t).to_owned()
}

fn from_json3(root: &Node) -> Model {
    let mut m = Model::default();
    let graph = nodes(root, "@graph");
    let id_of = |n: &Node| s(n, "spdxId | @id");
    let by_id: HashMap<String, &Node> = graph.iter().map(|n| (id_of(n), *n)).collect();
    let name_of = |id: &str| {
        by_id
            .get(id)
            .map(|n| {
                let v = s(n, "simplelicensing_licenseExpression | name");
                if v.is_empty() { id.to_owned() } else { v }
            })
            .unwrap_or_else(|| id.rsplit('/').next().unwrap_or(id).to_owned())
    };
    let hashes = |n: &Node| -> Vec<String> {
        nodes(n, "verifiedUsing")
            .into_iter()
            .map(|h| join(&[s(h, "algorithm"), s(h, "hashValue")], ": "))
            .collect()
    };
    for n in &graph {
        match type3(n).as_str() {
            "SpdxDocument" => {
                m.document_id = id_of(n);
                m.header.push(("Document name", s(n, "name"), span(n)));
                m.header.push(("SPDX ID", id_of(n), None));
                m.header.push(("Data licence", s(n, "dataLicense"), None));
                m.header
                    .push(("Profiles", s(n, "profileConformance"), None));
                m.header.push(("Root elements", s(n, "rootElement"), None));
            }
            "CreationInfo" => {
                m.header.push(("Created", s(n, "created"), span(n)));
                let by: Vec<String> = nodes(n, "createdBy")
                    .into_iter()
                    .map(|b| name_of(&b.text()))
                    .collect();
                m.header.push(("Creators", by.join(", "), None));
                m.header.push(("Spec version", s(n, "specVersion"), None));
            }
            "software_Package" | "Package" => m.packages.push(Item {
                id: id_of(n),
                name: s(n, "name"),
                version: s(n, "software_packageVersion | packageVersion"),
                supplier: {
                    let sup = s(n, "suppliedBy");
                    if sup.is_empty() { sup } else { name_of(&sup) }
                },
                download: s(n, "software_downloadLocation | downloadLocation"),
                purls: {
                    let u = s(n, "software_packageUrl | packageUrl");
                    if u.is_empty() { vec![] } else { vec![u] }
                },
                checksums: hashes(n),
                span: span(n),
                ..Item::default()
            }),
            "software_File" | "File" => m.files.push(Item {
                id: id_of(n),
                name: s(n, "name"),
                checksums: hashes(n),
                span: span(n),
                ..Item::default()
            }),
            "software_Snippet" | "Snippet" => m.snippets.push(id_of(n)),
            "Relationship" | "LifecycleScopedRelationship" => {
                let (from, ty) = (s(n, "from"), s(n, "relationshipType"));
                let tos: Vec<String> = nodes(n, "to").into_iter().map(|t| t.text()).collect();
                for to in &tos {
                    m.relationships
                        .push((from.clone(), ty.clone(), to.clone(), span(n)));
                }
            }
            _ => {}
        }
    }
    // Licence relationships may come before the packages they name.
    for (from, ty, to, _) in &m.relationships {
        if ty != "hasConcludedLicense" && ty != "hasDeclaredLicense" {
            continue;
        }
        let lic = name_of(to);
        for item in m.packages.iter_mut().chain(m.files.iter_mut()) {
            let slot = if ty == "hasConcludedLicense" {
                &mut item.concluded
            } else {
                &mut item.declared
            };
            if item.id == *from && !slot.contains(&lic) {
                *slot = join(&[slot.clone(), lic.clone()], ", ");
            }
        }
    }
    m
}

/// Parse the tag-value form (`Tag: value`, multi-line values in `<text>…</text>`).
fn from_tag_value(text: &str, cancelled: &dyn Fn() -> bool) -> Result<Model, String> {
    #[derive(Clone, Copy)]
    enum Current {
        Document,
        Package,
        File,
        Other,
    }
    let mut m = Model::default();
    let mut current = Current::Document;
    let mut offset = 0usize;
    let mut lines = text.split_inclusive('\n');
    let mut count = 0usize;
    let mut creators: Vec<String> = vec![];
    let mut describes: Vec<String> = vec![];
    let push_header = |m: &mut Model, label: &'static str, v: &str, sp: Range<u64>| {
        m.header.push((label, v.to_owned(), Some(sp)));
    };
    while let Some(line) = lines.next() {
        count += 1;
        if count.is_multiple_of(65536) && cancelled() {
            return Err("Cancelled".into());
        }
        if count > 4_000_000 {
            m.skipped += 1;
            break;
        }
        let start = offset;
        offset += line.len();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((tag, value)) = trimmed.split_once(':') else {
            m.skipped += 1;
            continue;
        };
        let tag = tag.trim();
        let mut value = value.trim().to_owned();
        if let Some(rest) = value.strip_prefix("<text>") {
            let mut body = rest.to_owned();
            let mut guard = 0;
            while !body.contains("</text>") {
                guard += 1;
                let Some(next) = lines.next().filter(|_| guard < 100_000) else {
                    break;
                };
                offset += next.len();
                body.push('\n');
                body.push_str(next.trim_end_matches(['\r', '\n']));
            }
            value = body.split("</text>").next().unwrap_or("").trim().to_owned();
        }
        let sp = start as u64..offset as u64;
        let extend = |item: Option<&mut Item>| {
            if let Some(i) = item {
                let s = i.span.get_or_insert(sp.clone());
                s.end = sp.end;
            }
        };
        match tag {
            "PackageName" => {
                current = Current::Package;
                m.packages.push(Item {
                    name: value.clone(),
                    span: Some(sp.clone()),
                    ..Item::default()
                });
            }
            "FileName" => {
                current = Current::File;
                m.files.push(Item {
                    name: value.clone(),
                    span: Some(sp.clone()),
                    ..Item::default()
                });
            }
            "SnippetSPDXID" => {
                current = Current::Other;
                m.snippets.push(value.clone());
            }
            "LicenseID" => {
                current = Current::Other;
                m.licenses.push(value.clone());
            }
            "Relationship" => {
                let parts: Vec<&str> = value.split_whitespace().collect();
                if parts.len() >= 3 {
                    m.relationships.push((
                        parts[0].into(),
                        parts[1].into(),
                        parts[2].into(),
                        Some(sp.clone()),
                    ));
                } else {
                    m.skipped += 1;
                }
            }
            "SPDXVersion" => push_header(&mut m, "SPDX version", &value, sp.clone()),
            "DataLicense" => push_header(&mut m, "Data licence", &value, sp.clone()),
            "DocumentName" => push_header(&mut m, "Document name", &value, sp.clone()),
            "DocumentNamespace" => push_header(&mut m, "Namespace", &value, sp.clone()),
            "Created" => push_header(&mut m, "Created", &value, sp.clone()),
            "LicenseListVersion" => push_header(&mut m, "Licence list version", &value, sp.clone()),
            "DocumentComment" => push_header(&mut m, "Comment", &value, sp.clone()),
            "Creator" => creators.push(value.clone()),
            "ExternalDocumentRef" => push_header(&mut m, "External document", &value, sp.clone()),
            _ => {}
        }
        if tag == "Relationship" {
            continue;
        }
        match current {
            Current::Document => {
                if tag == "SPDXID" {
                    m.document_id = value.clone();
                    push_header(&mut m, "SPDX ID", &value, sp.clone());
                }
            }
            Current::Package => {
                let item = m.packages.last_mut();
                if let Some(p) = item {
                    match tag {
                        "SPDXID" => p.id = value.clone(),
                        "PackageVersion" => p.version = value.clone(),
                        "PackageSupplier" => p.supplier = value.clone(),
                        "PackageDownloadLocation" => p.download = value.clone(),
                        "PackageLicenseConcluded" => p.concluded = value.clone(),
                        "PackageLicenseDeclared" => p.declared = value.clone(),
                        "PackageChecksum" => p.checksums.push(value.clone()),
                        "ExternalRef" => {
                            let parts: Vec<&str> = value.split_whitespace().collect();
                            if parts.len() >= 3 && parts[1] == "purl" {
                                p.purls.push(parts[2].into());
                            } else if parts.len() >= 3 {
                                p.other_refs.push(format!("{}: {}", parts[1], parts[2]));
                            }
                        }
                        _ => {}
                    }
                    extend(Some(p));
                }
            }
            Current::File => {
                if let Some(f) = m.files.last_mut() {
                    match tag {
                        "SPDXID" => f.id = value.clone(),
                        "FileChecksum" => f.checksums.push(value.clone()),
                        "LicenseConcluded" => f.concluded = value.clone(),
                        "LicenseInfoInFile" => {
                            f.declared = join(&[f.declared.clone(), value.clone()], ", ")
                        }
                        _ => {}
                    }
                    extend(Some(f));
                }
            }
            Current::Other => {}
        }
        if tag == "DocumentDescribes" {
            describes.push(value.clone());
        }
    }
    if !creators.is_empty() {
        m.header.push(("Creators", creators.join(", "), None));
    }
    for d in describes.iter().flat_map(|d| d.split(',')) {
        m.relationships.push((
            m.document_id.clone(),
            "DESCRIBES".into(),
            d.trim().into(),
            None,
        ));
    }
    if m.header.is_empty() && m.packages.is_empty() && m.files.is_empty() {
        return Err("No SPDX tags found".into());
    }
    Ok(m)
}

fn render(m: &Model, out: &mut Inspection, check_refs: bool) {
    for (label, value, src) in &m.header {
        out.fact("Document", label, value.clone(), src.clone());
    }
    out.fact("Contents", "Packages", m.packages.len().to_string(), None);
    out.fact("Contents", "Files", m.files.len().to_string(), None);
    if !m.snippets.is_empty() {
        out.fact("Contents", "Snippets", m.snippets.len().to_string(), None);
    }
    if !m.licenses.is_empty() {
        out.fact(
            "Contents",
            "Extracted licences",
            m.licenses.len().to_string(),
            None,
        );
    }
    out.fact(
        "Contents",
        "Relationships",
        m.relationships.len().to_string(),
        None,
    );
    let mut pt = Rows::new(
        "Packages",
        &[
            "SPDXID",
            "Name",
            "Version",
            "Supplier",
            "Download location (not opened)",
            "Licence concluded",
            "Licence declared",
            "Checksums",
            "Package URL",
            "Other external refs",
        ],
    );
    for p in &m.packages {
        pt.push(
            vec![
                p.id.clone(),
                p.name.clone(),
                p.version.clone(),
                p.supplier.clone(),
                p.download.clone(),
                p.concluded.clone(),
                p.declared.clone(),
                p.checksums.join("; "),
                p.purls.join("; "),
                p.other_refs.join("; "),
            ],
            p.span.clone(),
        );
    }
    pt.finish(out);
    let mut ft = Rows::new(
        "Files",
        &[
            "File name",
            "SPDXID",
            "Checksums",
            "Licence concluded",
            "Licence info in file",
        ],
    );
    for f in &m.files {
        ft.push(
            vec![
                f.name.clone(),
                f.id.clone(),
                f.checksums.join("; "),
                f.concluded.clone(),
                f.declared.clone(),
            ],
            f.span.clone(),
        );
    }
    ft.finish(out);
    let names: HashMap<&str, &str> = m
        .packages
        .iter()
        .chain(m.files.iter())
        .map(|i| (i.id.as_str(), i.name.as_str()))
        .collect();
    let mut rt = Rows::new(
        "Relationships",
        &[
            "Element",
            "Relationship",
            "Related element",
            "Element name",
            "Related name",
        ],
    );
    for (a, t, b, src) in &m.relationships {
        rt.push(
            vec![
                a.clone(),
                t.clone(),
                b.clone(),
                names
                    .get(a.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                names
                    .get(b.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
            ],
            src.clone(),
        );
    }
    rt.finish(out);
    if m.skipped > 0 {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("{} lines or records could not be read", m.skipped),
        );
    }
    if !check_refs || m.relationships.is_empty() {
        return;
    }
    // Every element named in a relationship is defined in this document (external document
    // references and NONE/NOASSERTION are not checked).
    let mut known: HashSet<&str> = names.keys().copied().collect();
    known.insert(m.document_id.as_str());
    known.extend(m.snippets.iter().map(String::as_str));
    let (mut ok, mut bad, mut external) = (0, 0, 0);
    let mut examples = vec![];
    for (a, _, b, _) in &m.relationships {
        for id in [a, b] {
            if id.starts_with("DocumentRef-") || id == "NONE" || id == "NOASSERTION" {
                external += 1;
            } else if known.contains(id.as_str()) {
                ok += 1;
            } else {
                bad += 1;
                if examples.len() < 5 {
                    examples.push(id.clone());
                }
            }
        }
    }
    out.check(
        Stage::Semantic,
        if bad > 0 { State::Failed } else { State::Passed },
        format!(
            "Relationship elements: {ok} defined in this document, {bad} not{}; {external} external or NONE/NOASSERTION not checked",
            if examples.is_empty() {
                String::new()
            } else {
                format!(" ({})", examples.join(", "))
            }
        ),
    );
}

/// SPDX tag-value text.
pub struct SpdxTagValue;

fn first_tag(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
}

impl Handler for SpdxTagValue {
    fn name(&self) -> &'static str {
        "SPDX tag-value"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        probe
            .text()
            .and_then(first_tag)
            .is_some_and(|l| l.starts_with("SPDXVersion:"))
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let text = input.text()?;
        let m = from_tag_value(&text, cancelled)?;
        let mut out = Inspection::new("SPDX", "Software bill of materials (tag-value)");
        out.version = m
            .header
            .iter()
            .find(|(l, _, _)| *l == "SPDX version")
            .map(|(_, v, _)| v.clone());
        render(&m, &mut out, true);
        if !out.checks.iter().any(|c| c.stage == Stage::Parsed) {
            out.check(
                Stage::Parsed,
                State::Passed,
                "Read as SPDX tag-value; the tags shown were found by name",
            );
        }
        out.not_validated();
        Ok(out)
    }
}
