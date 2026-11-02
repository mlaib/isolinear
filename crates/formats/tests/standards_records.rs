//! Clinical, software-supply-chain and data record standards (`inspect::standards`): FHIR,
//! CDA/C-CDA, SARIF, CycloneDX, SPDX, HAR, Jupyter and SDMX-JSON. Fixtures in
//! tests/fixtures/standards are written by hand from the published specifications.
use isolinear_formats::{
    detect,
    inspect::{Inspection, Stage, State, Table, Visual, inspect_file},
};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/standards")
        .join(name)
}

fn inspect(path: &Path) -> Option<Result<Inspection, String>> {
    let head = std::fs::read(path).unwrap();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let format = detect(&name, &head[..head.len().min(65536)]).format.id;
    inspect_file(path, format, &|| false)
}

fn open(name: &str) -> Inspection {
    inspect(&fixture(name))
        .unwrap_or_else(|| panic!("{name} not recognized"))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn fact<'a>(i: &'a Inspection, label: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.label == label)
        .map(|f| f.value.as_str())
        .unwrap_or_else(|| panic!("no fact {label:?} in {:#?}", i.facts))
}

fn fact_in<'a>(i: &'a Inspection, group: &str, label: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.group == group && f.label == label)
        .map(|f| f.value.as_str())
        .unwrap_or_else(|| panic!("no fact {group}/{label:?} in {:#?}", i.facts))
}

fn check(i: &Inspection, stage: Stage, contains: &str) -> (State, String) {
    let c = i
        .checks
        .iter()
        .find(|c| c.stage == stage && c.detail.contains(contains))
        .unwrap_or_else(|| panic!("no {stage:?} check {contains:?} in {:#?}", i.checks));
    (c.state, c.detail.clone())
}

fn table<'a>(i: &'a Inspection, name: &str) -> &'a Table {
    i.tables.iter().find(|t| t.name == name).unwrap_or_else(|| {
        panic!(
            "no table {name:?} in {:?}",
            i.tables.iter().map(|t| &t.name).collect::<Vec<_>>()
        )
    })
}

fn cell<'a>(t: &'a Table, row: usize, column: &str) -> &'a str {
    let c = t
        .columns
        .iter()
        .position(|c| c == column)
        .unwrap_or_else(|| panic!("no column {column:?} in {:?}", t.columns));
    &t.rows[row].cells[c]
}

fn outline_value<'a>(i: &'a Inspection, label: &str) -> &'a str {
    i.outline
        .iter()
        .find(|n| n.label == label)
        .map(|n| n.value.as_str())
        .unwrap_or_else(|| panic!("no outline entry {label:?}"))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("isolinear-records-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fhir_bundle_entries_resources_and_a_failing_reference_check() {
    let i = open("fhir-bundle.json");
    assert_eq!(i.family, "HL7 FHIR");
    assert!(i.kind.starts_with("Bundle"), "{}", i.kind);
    assert!(
        i.version.as_deref().unwrap().starts_with("R4"),
        "{:?}",
        i.version
    );
    assert_eq!(fact(&i, "Bundle type"), "collection");
    assert_eq!(fact(&i, "Entries"), "8");
    assert_eq!(fact_in(&i, "Resources in the Bundle", "Observation"), "2");

    let entries = table(&i, "Entries");
    assert_eq!(entries.rows.len(), 8);
    assert_eq!(cell(entries, 2, "Resource type"), "Observation");
    let summary = cell(entries, 2, "Summary");
    assert!(
        summary.contains("Heart rate (http://loinc.org|8867-4)"),
        "{summary}"
    );
    assert!(summary.contains("72 beats/minute"), "{summary}");
    // The Patient's summary holds no personal values.
    let patient_summary = cell(entries, 0, "Summary");
    assert!(!patient_summary.contains("Muller") && !patient_summary.contains("1984"));
    assert!(patient_summary.contains("female"));

    // Per-type tables; Patient columns are labelled so masking applies.
    let patients = table(&i, "Patients");
    assert_eq!(cell(patients, 0, "Patient name"), "Anna Maria Muller");
    assert_eq!(cell(patients, 0, "Patient birth date"), "1984-03-12");
    assert!(cell(patients, 0, "Patient identifiers").contains("MRN-004711"));
    for c in &patients.columns[1..] {
        assert!(c.contains("Patient") || c == "Managing organization", "{c}");
    }
    let obs = table(&i, "Observations");
    assert_eq!(cell(obs, 0, "Status"), "final");
    assert_eq!(cell(obs, 0, "Effective"), "2026-10-27T08:45:00+01:00");
    assert!(cell(obs, 0, "Interpretation").starts_with("Normal ("));
    assert!(cell(obs, 1, "Value").contains("Systolic blood pressure 128 mmHg"));
    let enc = table(&i, "Encounters");
    assert_eq!(
        cell(enc, 0, "Class"),
        "ambulatory (http://terminology.hl7.org/CodeSystem/v3-ActCode|AMB)"
    );
    assert_eq!(
        cell(enc, 0, "Period"),
        "2026-10-27T08:30:00+01:00 – 2026-10-27T09:10:00+01:00"
    );
    assert!(
        cell(table(&i, "Conditions"), 0, "Code").starts_with("Hypertension: Hypertensive disorder")
    );
    assert!(cell(table(&i, "Medication requests"), 0, "Medication").contains("lisinopril"));
    assert_eq!(cell(table(&i, "Diagnostic reports"), 0, "Results"), "3");
    assert_eq!(cell(table(&i, "Procedures"), 0, "Status"), "completed");

    // References: 12 in all; one points at a resource missing from the Bundle, one outside.
    let refs = table(&i, "References");
    assert_eq!(refs.rows.len(), 12);
    let (state, detail) = check(&i, Stage::Semantic, "References within the Bundle");
    assert_eq!(state, State::Failed, "{detail}");
    assert!(
        detail.contains("10 resolve, 1 do not (Observation/o-missing)"),
        "{detail}"
    );
    assert!(
        detail.contains("1 outside the file not checked"),
        "{detail}"
    );
    let missing = refs
        .rows
        .iter()
        .position(|r| r.cells.iter().any(|c| c == "Observation/o-missing"))
        .unwrap();
    assert_eq!(cell(refs, missing, "Resolves in this file"), "no");
    assert_eq!(cell(refs, missing, "From"), "DiagnosticReport/r1");
    // Every row points at its bytes.
    let text = std::fs::read_to_string(fixture("fhir-bundle.json")).unwrap();
    let src = refs.rows[missing].source.clone().unwrap();
    assert!(text[src.start as usize..src.end as usize].contains("o-missing"));
    assert_eq!(check(&i, Stage::Schema, "").0, State::NotDone);
}

#[test]
fn fhir_patient_in_xml_reads_like_json() {
    let i = open("fhir-patient.xml");
    assert!(i.kind.starts_with("Patient"), "{}", i.kind);
    assert!(i.version.as_deref().unwrap().starts_with("R4"));
    assert_eq!(fact(&i, "ID"), "pat-xml-1");
    assert_eq!(fact(&i, "Version ID"), "3");
    assert_eq!(fact(&i, "Narrative status"), "generated");
    assert_eq!(fact(&i, "Patient name"), "Jean Schmit");
    assert_eq!(fact(&i, "Patient gender"), "male");
    assert_eq!(fact(&i, "Patient birth date"), "1962-07-30");
    assert!(fact(&i, "Patient identifiers").contains("1962073012345 (urn:oid:1.2.442.0.1)"));
    assert_eq!(fact(&i, "Patient telecom"), "phone: +352 621 000 000");
    // Personal values are under labels the Overview masks.
    for f in i.facts.iter().filter(|f| f.group == "Patient") {
        assert!(
            f.label.contains("Patient") || f.label == "Managing organization",
            "{}",
            f.label
        );
    }
    // The contained organization resolves; the practitioner is outside the file.
    let (state, detail) = check(&i, Stage::Semantic, "References");
    assert_eq!(state, State::Passed, "{detail}");
    assert!(detail.contains("1 resolve, 0 do not"), "{detail}");
    assert!(detail.contains("1 outside the file"), "{detail}");
}

#[test]
fn fhir_version_stated_inferred_or_unknown() {
    let dir = scratch("fhir");
    let p = dir.join("cap.json");
    std::fs::write(
        &p,
        r#"{"resourceType":"CapabilityStatement","status":"active","date":"2026-01-01","kind":"instance","fhirVersion":"4.0.1","format":["json","xml"],
            "software":{"name":"Example FHIR Server","version":"5.1"},
            "rest":[{"mode":"server","resource":[{"type":"Patient","interaction":[{"code":"read"},{"code":"search-type"}]}]}]}"#,
    )
    .unwrap();
    let i = inspect(&p).unwrap().unwrap();
    assert_eq!(i.version.as_deref(), Some("4.0.1 (R4)"));
    assert_eq!(fact(&i, "Software"), "Example FHIR Server");
    let t = table(&i, "Supported resources");
    assert_eq!(cell(t, 0, "Interactions"), "read, search-type");
    let p = dir.join("obs.json");
    std::fs::write(
        &p,
        r#"{"resourceType":"Observation","id":"x","status":"preliminary","code":{"text":"Glucose"},"valueQuantity":{"value":5.4,"unit":"mmol/L"}}"#,
    )
    .unwrap();
    let i = inspect(&p).unwrap().unwrap();
    assert_eq!(i.version.as_deref(), Some("unknown"));
    assert!(i.notes.iter().any(|n| n.contains("not stated")));
    assert_eq!(fact(&i, "Code"), "Glucose");
    assert_eq!(fact(&i, "Value"), "5.4 mmol/L");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ccda_document_patient_sections_and_entries() {
    let i = open("ccda-ccd.xml");
    assert_eq!(i.family, "HL7 CDA");
    assert_eq!(i.kind, "C-CDA Continuity of Care Document (CCD)");
    assert_eq!(i.version.as_deref(), Some("CDA R2, C-CDA R2.1"));
    assert_eq!(fact(&i, "Title"), "Continuity of Care Document");
    assert_eq!(fact(&i, "Type"), "Summarization of Episode Note");
    assert_eq!(fact(&i, "Type code"), "34133-9");
    assert_eq!(fact(&i, "Effective time"), "20261027101500+0100");
    assert_eq!(fact(&i, "Confidentiality"), "N");
    assert_eq!(fact(&i, "Language"), "en-US");
    assert_eq!(fact(&i, "Patient name"), "Eve Everywoman");
    assert_eq!(fact(&i, "Patient gender"), "Female");
    assert_eq!(fact(&i, "Patient birth time"), "19750501");
    assert!(fact(&i, "Patient IDs").contains("998991 (2.16.840.1.113883.19.5.99999.2)"));
    assert!(fact(&i, "Patient address").contains("1357 Amber Drive"));
    assert_eq!(fact(&i, "Author name"), "Dr. Henry Seven");
    assert_eq!(
        fact_in(&i, "Custodian", "Organization"),
        "Community Health and Hospitals"
    );
    assert_eq!(fact(&i, "Sections"), "4");
    assert_eq!(fact(&i, "Structured entries"), "4");
    for f in i.facts.iter().filter(|f| f.group == "Patient") {
        assert!(f.label.contains("Patient"), "{}", f.label);
    }
    // Outline: sections with their code, template and narrative.
    let allergies = outline_value(&i, "ALLERGIES AND ADVERSE REACTIONS");
    assert!(
        allergies.contains("template: Allergies and Intolerances"),
        "{allergies}"
    );
    assert!(allergies.contains("1 structured entries"), "{allergies}");
    let narrative = i.outline.iter().find(|n| n.label == "Narrative").unwrap();
    assert!(
        narrative
            .value
            .contains("Penicillin G benzathine Hives Active"),
        "{}",
        narrative.value
    );
    // Entries, with the statements nested in acts and organizers.
    let t = table(&i, "Entries");
    assert_eq!(t.rows.len(), 8);
    let problem = t
        .rows
        .iter()
        .position(|r| r.cells.iter().any(|c| c.contains("Essential hypertension")))
        .unwrap();
    assert_eq!(cell(t, problem, "Section"), "PROBLEMS");
    assert_eq!(cell(t, problem, "Entry type"), "observation");
    assert_eq!(cell(t, problem, "Within"), "act");
    assert_eq!(
        cell(t, problem, "Value"),
        "Essential hypertension (SNOMED CT|59621000)"
    );
    assert_eq!(cell(t, problem, "Time"), "20240502");
    let med = t
        .rows
        .iter()
        .position(|r| r.cells[1] == "substanceAdministration")
        .unwrap();
    assert!(cell(t, med, "Value").contains("lisinopril 10 MG Oral Tablet"));
    let hr = t
        .rows
        .iter()
        .position(|r| r.cells.iter().any(|c| c.starts_with("Heart rate")))
        .unwrap();
    assert_eq!(cell(t, hr, "Value"), "72 /min");
    assert_eq!(cell(t, hr, "Within"), "organizer");
}

#[test]
fn sarif_tools_results_rules_and_levels() {
    let i = open("results.sarif");
    assert_eq!(i.family, "SARIF");
    assert_eq!(i.version.as_deref(), Some("2.1.0"));
    assert_eq!(fact(&i, "Tool"), "ExampleLint");
    assert_eq!(fact(&i, "Tool version"), "3.2.1");
    assert_eq!(
        fact(&i, "Invocation"),
        "successful, exit code 0, 2026-10-30T14:02:11Z"
    );
    assert_eq!(fact(&i, "Invocations successful"), "1 of 1");
    assert_eq!(fact(&i, "Results"), "3");
    assert_eq!(fact(&i, "Files affected"), "2");
    assert_eq!(fact_in(&i, "Results by level", "error"), "1");
    assert_eq!(fact_in(&i, "Results by level", "warning"), "1");
    assert_eq!(fact_in(&i, "Results by level", "note"), "1");
    let r = table(&i, "Results");
    assert_eq!(r.rows.len(), 3);
    assert_eq!(cell(r, 1, "Rule ID"), "EL2002");
    assert_eq!(cell(r, 1, "Level"), "error (rule default)");
    assert_eq!(
        cell(r, 1, "Message"),
        "Input from request.args['q'] flows into a query."
    );
    assert_eq!(cell(r, 1, "File"), "%SRCROOT%/src/db.py");
    assert_eq!(cell(r, 1, "Start line"), "42");
    assert_eq!(cell(r, 1, "Start column"), "17");
    let rules = table(&i, "Rules");
    assert_eq!(rules.rows.len(), 3);
    assert_eq!(cell(rules, 1, "Name"), "SqlInjection");
    assert_eq!(cell(rules, 2, "Default level"), "warning (default)");
}

#[test]
fn cyclonedx_json_components_dependencies_vulnerabilities_services() {
    let i = open("bom.cdx.json");
    assert_eq!(i.family, "CycloneDX");
    assert_eq!(i.version.as_deref(), Some("CycloneDX 1.5"));
    assert_eq!(fact(&i, "Spec version"), "1.5");
    assert_eq!(
        fact(&i, "Serial number"),
        "urn:uuid:3e671687-395b-41f5-a30f-a58921a69b79"
    );
    assert_eq!(fact(&i, "Component"), "application acme-web 2.4.0");
    assert!(fact(&i, "Tools").contains("cdxgen 10.9.1"));
    assert_eq!(fact_in(&i, "Components", "Components"), "4");
    let c = table(&i, "Components");
    assert_eq!(c.rows.len(), 4);
    assert_eq!(cell(c, 0, "Name"), "express");
    assert_eq!(cell(c, 0, "Licenses"), "MIT");
    assert_eq!(cell(c, 0, "Hashes"), "2");
    assert_eq!(cell(c, 0, "Scope"), "required");
    assert_eq!(cell(c, 1, "Licenses"), "MIT");
    assert_eq!(cell(c, 2, "Group"), "org.apache.logging.log4j");
    assert_eq!(cell(c, 3, "Nested in"), "log4j-core");
    assert_eq!(
        outline_value(&i, "pkg:npm/acme-web@2.4.0"),
        "application acme-web 2.4.0 · depends on 2"
    );
    let v = table(&i, "Vulnerabilities");
    assert_eq!(cell(v, 0, "ID"), "CVE-2021-44228");
    assert_eq!(cell(v, 0, "Severity"), "critical");
    assert_eq!(cell(v, 0, "Score"), "10.0");
    assert!(cell(v, 0, "Affects").contains("log4j-core"));
    assert_eq!(fact(&i, "Severity critical"), "1");
    let s = table(&i, "Services");
    assert_eq!(cell(s, 0, "Name"), "payments-api");
    let (state, detail) = check(&i, Stage::Semantic, "bom-ref");
    assert_eq!(state, State::Passed, "{detail}");
}

#[test]
fn cyclonedx_xml_and_an_unknown_dependency() {
    let i = open("bom.cdx.xml");
    assert_eq!(i.version.as_deref(), Some("CycloneDX 1.4"));
    assert_eq!(fact(&i, "Spec version"), "1.4");
    assert_eq!(fact(&i, "BOM version"), "2");
    assert!(fact(&i, "Tools").contains("CycloneDX cyclonedx-maven-plugin 2.7.9"));
    assert_eq!(fact(&i, "Component"), "application lu.example/ledger 1.0.0");
    let c = table(&i, "Components");
    assert_eq!(c.rows.len(), 2);
    assert_eq!(cell(c, 0, "Licenses"), "Apache-2.0");
    assert_eq!(cell(c, 0, "Hashes"), "2");
    assert_eq!(cell(c, 1, "Licenses"), "MIT");
    assert_eq!(fact(&i, "Edges"), "3");
    let (state, detail) = check(&i, Stage::Semantic, "bom-ref");
    assert_eq!(state, State::Failed);
    assert!(detail.contains("jackson-core@2.15.2"), "{detail}");
}

#[test]
fn spdx_json_packages_files_relationships() {
    let i = open("sbom.spdx.json");
    assert_eq!(i.family, "SPDX");
    assert_eq!(i.version.as_deref(), Some("SPDX-2.3"));
    assert_eq!(fact(&i, "Document name"), "acme-cli-1.3.0");
    assert_eq!(
        fact(&i, "Namespace"),
        "https://spdx.example.org/acme-cli-1.3.0-7f0c1d2e"
    );
    assert_eq!(
        fact(&i, "Creators"),
        "Tool: syft-1.14.0, Organization: Acme Software"
    );
    let p = table(&i, "Packages");
    assert_eq!(p.rows.len(), 2);
    assert_eq!(
        cell(p, 0, "Checksums"),
        "SHA256: d2a84f4b8b650937ec8f73cd8be2c74add5a911ba64df27458ed8229da804a26"
    );
    assert_eq!(
        cell(p, 1, "Package URL"),
        "pkg:golang/github.com/spf13/cobra@v1.8.0"
    );
    assert!(cell(p, 1, "Other external refs").starts_with("cpe23Type: cpe:2.3:a:spf13"));
    let f = table(&i, "Files");
    assert_eq!(cell(f, 0, "File name"), "./cmd/acme/main.go");
    assert_eq!(cell(f, 0, "Licence info in file"), "Apache-2.0");
    let r = table(&i, "Relationships");
    assert_eq!(r.rows.len(), 3);
    assert_eq!(cell(r, 1, "Relationship"), "DEPENDS_ON");
    assert_eq!(cell(r, 1, "Related name"), "github.com/spf13/cobra");
    assert_eq!(check(&i, Stage::Semantic, "Relationship").0, State::Passed);
}

#[test]
fn spdx_tag_value_with_text_blocks_and_an_undefined_element() {
    let path = fixture("sbom.spdx");
    let i = open("sbom.spdx");
    assert_eq!(i.family, "SPDX");
    assert_eq!(i.version.as_deref(), Some("SPDX-2.3"));
    assert_eq!(fact(&i, "Document name"), "acme-cli-1.3.0");
    assert!(fact(&i, "Comment").contains("Covers the command-line tool"));
    assert_eq!(
        fact(&i, "Creators"),
        "Tool: syft-1.14.0, Organization: Acme Software"
    );
    let p = table(&i, "Packages");
    assert_eq!(p.rows.len(), 2);
    assert_eq!(cell(p, 0, "SPDXID"), "SPDXRef-Package-acme-cli");
    assert_eq!(
        cell(p, 0, "Package URL"),
        "pkg:golang/git.example.org/acme/cli@v1.3.0"
    );
    assert_eq!(cell(p, 1, "Version"), "v1.8.0");
    let text = std::fs::read_to_string(&path).unwrap();
    let src = p.rows[0].source.clone().unwrap();
    let block = &text[src.start as usize..src.end as usize];
    assert!(block.starts_with("PackageName: acme-cli"), "{block}");
    assert!(block.contains("ExternalRef:"), "{block}");
    let f = table(&i, "Files");
    assert_eq!(cell(f, 0, "SPDXID"), "SPDXRef-File-main");
    assert_eq!(
        cell(f, 0, "Checksums"),
        "SHA1: 85ed0817af83a24ad8da68c2b5094de69833983c"
    );
    assert_eq!(table(&i, "Relationships").rows.len(), 4);
    let (state, detail) = check(&i, Stage::Semantic, "Relationship");
    assert_eq!(state, State::Failed);
    assert!(detail.contains("SPDXRef-Package-viper"), "{detail}");
}

#[test]
fn spdx3_json_ld_is_read_in_part() {
    let dir = scratch("spdx3");
    let p = dir.join("sbom.spdx3.json");
    std::fs::write(
        &p,
        r#"{"@context":"https://spdx.org/rdf/3.0.1/spdx-context.jsonld","@graph":[
          {"type":"CreationInfo","@id":"_:c","specVersion":"3.0.1","created":"2026-10-01T00:00:00Z","createdBy":["urn:x:tool"]},
          {"type":"Tool","spdxId":"urn:x:tool","name":"example-sbom-tool"},
          {"type":"SpdxDocument","spdxId":"urn:x:doc","name":"demo-sbom","creationInfo":"_:c","rootElement":["urn:x:pkg"]},
          {"type":"Relationship","spdxId":"urn:x:r1","from":"urn:x:pkg","relationshipType":"hasConcludedLicense","to":["urn:x:lic"]},
          {"type":"software_Package","spdxId":"urn:x:pkg","name":"demo","software_packageVersion":"0.9","software_packageUrl":"pkg:pypi/demo@0.9"},
          {"type":"simplelicensing_LicenseExpression","spdxId":"urn:x:lic","simplelicensing_licenseExpression":"MIT OR Apache-2.0"},
          {"type":"software_File","spdxId":"urn:x:f1","name":"demo/__init__.py"},
          {"type":"Relationship","spdxId":"urn:x:r2","from":"urn:x:pkg","relationshipType":"contains","to":["urn:x:f1"]}
        ]}"#,
    )
    .unwrap();
    let i = inspect(&p).unwrap().unwrap();
    assert_eq!(i.version.as_deref(), Some("SPDX 3.0.1 (JSON-LD)"));
    assert_eq!(fact(&i, "Document name"), "demo-sbom");
    assert_eq!(fact(&i, "Creators"), "example-sbom-tool");
    let pk = table(&i, "Packages");
    assert_eq!(cell(pk, 0, "Version"), "0.9");
    assert_eq!(cell(pk, 0, "Package URL"), "pkg:pypi/demo@0.9");
    assert_eq!(cell(pk, 0, "Licence concluded"), "MIT OR Apache-2.0");
    assert_eq!(table(&i, "Relationships").rows.len(), 2);
    assert_eq!(check(&i, Stage::Parsed, "SPDX 3").0, State::Partial);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn har_entries_timings_and_private_headers() {
    let i = open("session.har");
    assert_eq!(i.family, "HAR");
    assert_eq!(i.version.as_deref(), Some("HAR 1.2"));
    assert_eq!(fact(&i, "Creator"), "Firefox 131.0");
    assert_eq!(fact(&i, "Browser"), "Firefox 131.0");
    assert_eq!(fact(&i, "Entries"), "3");
    assert_eq!(fact(&i, "Total response bytes (content)"), "17426");
    assert_eq!(
        fact(&i, "Slowest request"),
        "POST https://api.shop.example.com/v1/login — 640 ms"
    );
    assert_eq!(fact_in(&i, "Status codes", "200"), "2");
    assert_eq!(fact_in(&i, "Status codes", "401"), "1");
    let e = table(&i, "Entries");
    assert_eq!(e.rows.len(), 3);
    assert_eq!(cell(e, 1, "Method"), "POST");
    assert_eq!(cell(e, 1, "Status text"), "Unauthorized");
    assert_eq!(cell(e, 0, "Response size (bytes)"), "15320");
    assert_eq!(cell(e, 0, "Server IP"), "203.0.113.10");
    let t = table(&i, "Timings");
    assert_eq!(cell(t, 0, "DNS"), "12");
    assert_eq!(cell(t, 1, "DNS"), "", "-1 means not applicable");
    assert_eq!(cell(t, 1, "Wait"), "610");
    assert_eq!(table(&i, "Pages").rows.len(), 1);
    // Headers and cookies sit under labels the Overview masks; credentials are never shown.
    assert_eq!(outline_value(&i, "Cookie session"), "9f8e7d6c5b4a");
    assert!(outline_value(&i, "Cookie").contains("session="));
    assert_eq!(outline_value(&i, "Set-Cookie csrf"), "abc123");
    let auth = outline_value(&i, "Authorization");
    assert!(auth.starts_with("Bearer (credentials not shown"), "{auth}");
    assert!(i.outline.iter().all(|n| !n.value.contains("eyJhbGci")));
    assert!(i.outline.iter().all(|n| !n.value.contains("hunter2")));
    assert!(i.notes.iter().any(|n| n.contains("Nothing is fetched")));
    // The waterfall: starts from the first request, TLS drawn apart from connecting.
    let Some(Visual::Timeline { rows, phases, unit }) = i.visuals.first() else {
        panic!("no waterfall: {:?}", i.visuals);
    };
    assert_eq!(unit, "ms");
    assert_eq!(rows.len(), 3);
    let starts: Vec<f64> = rows
        .iter()
        .map(|r| (r.start * 10.0).round() / 10.0)
        .collect();
    assert_eq!(starts, [0.0, 240.0, 290.0]);
    let at = |name: &str| phases.iter().position(|p| p == name).unwrap();
    assert_eq!(rows[0].parts[at("Connect")], 8.0);
    assert_eq!(rows[0].parts[at("TLS")], 22.0);
    assert_eq!(rows[1].parts[at("DNS")], 0.0, "-1 is not drawn");
    assert_eq!(rows[1].parts[at("Wait")], 610.0);
    assert!(
        rows[1]
            .label
            .starts_with("#2 POST https://api.shop.example.com")
    );
    assert!(rows.iter().all(|r| r.source.is_some()));
}

#[test]
fn har_waterfall_needs_readable_starts() {
    // A HAR whose dates are not ISO 8601 still opens, without a waterfall; one with a date
    // offset is put on the same clock as the others.
    let dir = std::env::temp_dir().join(format!("isolinear-har-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("odd.har");
    let entry = |start: &str, time: u32| {
        format!(
            r#"{{"startedDateTime":"{start}","time":{time},"request":{{"method":"GET","url":"https://e.example/"}},"response":{{"status":200}},"timings":{{"send":-1,"wait":-1,"receive":-1}}}}"#
        )
    };
    let har = |entries: &[String]| {
        format!(
            r#"{{"log":{{"version":"1.2","creator":{{"name":"t"}},"entries":[{}]}}}}"#,
            entries.join(",")
        )
    };
    std::fs::write(&path, har(&[entry("yesterday", 5)])).unwrap();
    let i = inspect_file(&path, "json", &|| false).unwrap().unwrap();
    assert_eq!(fact(&i, "Entries"), "1");
    assert!(i.visuals.is_empty());
    std::fs::write(
        &path,
        har(&[
            entry("2026-10-30T10:00:00+01:00", 5),
            entry("2026-10-30T09:00:00.5Z", 7),
        ]),
    )
    .unwrap();
    let i = inspect_file(&path, "json", &|| false).unwrap().unwrap();
    let Some(Visual::Timeline { rows, phases, .. }) = i.visuals.first() else {
        panic!("no waterfall");
    };
    assert_eq!(rows[0].start, 0.0);
    assert_eq!(rows[1].start, 500.0);
    // Without timings the whole time is one unsplit bar.
    assert_eq!(
        rows[1].parts[phases.iter().position(|p| p == "Not split").unwrap()],
        7.0
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn jupyter_cells_outputs_and_pictures_without_execution() {
    let i = open("notebook.ipynb");
    assert_eq!(i.family, "Jupyter");
    assert_eq!(i.version.as_deref(), Some("nbformat 4.5"));
    assert_eq!(fact(&i, "Kernel"), "Python 3 (ipykernel) (python3)");
    assert_eq!(fact(&i, "Language"), "python 3.12.4");
    assert_eq!(fact(&i, "Cells"), "6");
    assert_eq!(fact(&i, "Code cells"), "5");
    assert_eq!(fact(&i, "Markdown cells"), "1");
    assert_eq!(fact(&i, "Code cells with an execution count"), "4");
    assert_eq!(fact(&i, "Pictures shown"), "1");
    let t = table(&i, "Cells");
    assert_eq!(t.rows.len(), 6);
    assert_eq!(cell(t, 0, "First line"), "# Sensor check");
    assert_eq!(cell(t, 1, "First line"), "import statistics");
    assert_eq!(cell(t, 1, "Lines"), "4");
    assert_eq!(cell(t, 1, "Execution count"), "1");
    assert_eq!(
        cell(t, 2, "Output types"),
        "display_data (image/png, text/plain)"
    );
    let pictures: Vec<_> = i
        .visuals
        .iter()
        .filter_map(|v| match v {
            Visual::Picture { bytes, caption } => Some((bytes, caption)),
            _ => None,
        })
        .collect();
    assert_eq!(pictures.len(), 1);
    assert_eq!(pictures[0].1, "Cell 3 output");
    assert!(pictures[0].0.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(
        outline_value(&i, "Output 1 (stdout)"),
        "rows: 128 ⏎ mean: 21.4 ⏎ "
    );
    let html = i
        .outline
        .iter()
        .find(|n| n.label == "Output 1 (execute_result)")
        .unwrap();
    assert!(
        html.value.contains("HTML output not rendered"),
        "{}",
        html.value
    );
    assert!(!html.value.contains("alert"));
    assert_eq!(
        outline_value(&i, "Output 1 (error)"),
        "ZeroDivisionError: division by zero"
    );
    assert!(
        i.notes
            .iter()
            .any(|n| n.contains("Nothing in the notebook is executed"))
    );
}

#[test]
fn sdmx_json_observations_with_decoded_keys() {
    let i = open("sdmx-data.json");
    assert_eq!(i.family, "SDMX");
    assert_eq!(i.version.as_deref(), Some("SDMX-JSON 1.0"));
    assert_eq!(fact(&i, "Message ID"), "IREF-EXR-2026-10");
    assert_eq!(fact(&i, "Sender"), "ECB");
    assert_eq!(fact(&i, "Sender name"), "European Central Bank");
    assert_eq!(fact(&i, "Name"), "Exchange rates");
    assert_eq!(fact_in(&i, "Fixed dimensions", "FREQ"), "M (Monthly)");
    assert_eq!(fact(&i, "Observations"), "4");
    let d = table(&i, "Dimensions");
    assert_eq!(d.rows.len(), 4);
    assert_eq!(cell(d, 1, "ID"), "CURRENCY");
    assert_eq!(
        cell(d, 1, "First values"),
        "USD (US dollar), CHF (Swiss franc)"
    );
    assert_eq!(table(&i, "Attributes").rows.len(), 2);
    let o = table(&i, "Observations");
    assert_eq!(
        o.columns,
        [
            "CURRENCY",
            "EXR_TYPE",
            "TIME_PERIOD",
            "Value",
            "UNIT",
            "OBS_STATUS"
        ]
    );
    assert_eq!(
        o.rows[0].cells,
        [
            "USD (US dollar)",
            "SP00 (Spot)",
            "2026-08",
            "1.0921",
            "USD (US dollar)",
            "A (Normal value)"
        ]
    );
    assert_eq!(cell(o, 1, "OBS_STATUS"), "P (Provisional value)");
    assert_eq!(cell(o, 3, "CURRENCY"), "CHF (Swiss franc)");
    assert_eq!(cell(o, 3, "OBS_STATUS"), "");
    assert_eq!(check(&i, Stage::Semantic, "keys").0, State::Passed);
}

#[test]
fn sdmx_csv_1_observations_dimensions_and_labels() {
    let i = open("sdmx-data.csv");
    assert_eq!(i.family, "SDMX");
    assert_eq!(i.version.as_deref(), Some("SDMX-CSV 1.0"));
    assert_eq!(fact(&i, "Dataflow"), "ECB:EXR(1.0)");
    assert_eq!(fact(&i, "Observations"), "4");
    assert_eq!(fact(&i, "Time periods"), "2026-07 to 2026-08");
    assert_eq!(fact(&i, "Missing values"), "1");
    assert_eq!(
        fact(&i, "Dimensions"),
        "FREQ, CURRENCY, EXR_TYPE, TIME_PERIOD"
    );
    assert_eq!(fact(&i, "Measure"), "OBS_VALUE");
    assert_eq!(fact(&i, "Attributes"), "OBS_STATUS, UNIT_MULT");
    assert_eq!(
        fact_in(&i, "Dimensions", "CURRENCY (Currency)"),
        "2 distinct: CHF, USD"
    );
    let o = table(&i, "Observations");
    assert_eq!(o.columns[1], "FREQ: Frequency", "headers keep their labels");
    assert_eq!(o.rows.len(), 4);
    assert_eq!(cell(o, 1, "OBS_VALUE"), "1.0921");
    // Each row points at its line.
    let text = std::fs::read_to_string(fixture("sdmx-data.csv")).unwrap();
    let src = o.rows[0].source.clone().unwrap();
    assert!(
        text[src.start as usize..src.end as usize].starts_with("ECB:EXR(1.0),M,USD,SP00,2026-07")
    );
    assert_eq!(check(&i, Stage::Parsed, "4 observations").0, State::Passed);
    assert_eq!(check(&i, Stage::Schema, "DSD").0, State::NotDone);
    assert_eq!(check(&i, Stage::Semantic, "numbers").0, State::Passed);
}

#[test]
fn sdmx_csv_2_structures_and_actions() {
    let i = open("sdmx-data-v2.csv");
    assert_eq!(i.version.as_deref(), Some("SDMX-CSV 2.0"));
    assert_eq!(fact(&i, "Structure type"), "dataflow, datastructure");
    assert_eq!(
        fact(&i, "Structure"),
        "ESTAT:DSD_NAMA(2.1), ESTAT:NAMA_10_GDP(1.0)"
    );
    assert_eq!(fact(&i, "Dimensions"), "REF_AREA, INDICATOR, TIME_PERIOD");
    assert_eq!(fact(&i, "Attributes"), "COMMENT");
    assert_eq!(fact_in(&i, "Actions", "I (information)"), "2");
    assert_eq!(fact_in(&i, "Actions", "D (delete)"), "1");
    assert_eq!(
        cell(table(&i, "Observations"), 0, "COMMENT"),
        "Provisional, revised"
    );
    // Deleting an observation sent earlier is not a duplicate.
    assert_eq!(check(&i, Stage::Semantic, "twice").0, State::Passed);
}

#[test]
fn sdmx_csv_problems_are_reported() {
    let i = open("sdmx-bad.csv");
    let (state, detail) = check(&i, Stage::Parsed, "field");
    assert_eq!(state, State::Partial, "{detail}");
    let (state, detail) = check(&i, Stage::Semantic, "twice");
    assert_eq!(state, State::Failed);
    assert_eq!(
        detail,
        "1 values are not numbers; 1 observations appear twice"
    );
    let notes = i.notes.join(" ");
    assert!(notes.contains("line 3: the same observation"), "{notes}");
    assert!(notes.contains("line 4: OBS_VALUE \"n/a\""), "{notes}");
    assert!(notes.contains("line 5: 2 fields for 4 columns"), "{notes}");
    // Ordinary CSV files, and a DATAFLOW column without observations, are not claimed.
    let dir = scratch("sdmx-csv");
    for (name, text) in [
        ("plain.csv", "name,amount\nAnn,1\n"),
        ("flow.csv", "DATAFLOW,OWNER\nx,y\n"),
        ("struct.csv", "STRUCTURE;ID\na;b\n"),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        assert!(
            inspect(&path).is_none_or(|r| r.map_or(true, |i| i.family != "SDMX")),
            "{name} claimed"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ordinary_json_and_xml_are_not_claimed() {
    let dir = scratch("plain");
    for (name, text) in [
        ("a.json", r#"{"name": "x", "version": "2.1.0"}"#),
        ("b.json", r#"{"log": {"level": "info"}}"#),
        ("c.json", r#"{"resourceType": 5}"#),
        ("d.json", r#"{"runs": [1, 2], "version": "1.0"}"#),
        ("e.json", r#"{"meta": {}, "data": [1, 2]}"#),
        ("f.xml", "<bom><component/></bom>"),
        (
            "g.xml",
            "<ClinicalDocument><title>x</title></ClinicalDocument>",
        ),
        (
            "h.txt",
            "Just a note that mentions SPDXVersion: somewhere\n",
        ),
    ] {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        assert!(inspect(&p).is_none(), "{name} was claimed");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn truncated_and_mutated_inputs_do_not_panic_and_finish_quickly() {
    let dir = scratch("damage");
    let names = [
        "fhir-bundle.json",
        "fhir-patient.xml",
        "ccda-ccd.xml",
        "results.sarif",
        "bom.cdx.json",
        "bom.cdx.xml",
        "sbom.spdx.json",
        "sbom.spdx",
        "session.har",
        "sdmx-data.csv",
        "sdmx-data-v2.csv",
        "sdmx-bad.csv",
        "notebook.ipynb",
        "sdmx-data.json",
    ];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    for name in names {
        let bytes = std::fs::read(fixture(name)).unwrap();
        let mut variants: Vec<Vec<u8>> = [1, 3, 2, 4]
            .iter()
            .map(|d| bytes[..bytes.len() * (d - 1).max(1) / d].to_vec())
            .collect();
        variants.push(bytes[..bytes.len() - 1].to_vec());
        for _ in 0..60 {
            let mut b = bytes.clone();
            for _ in 0..6 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let at = (seed % b.len() as u64) as usize;
                // Structural characters make the most interesting damage.
                b[at] = b"{}[]<>\":,/0 \n"[(seed >> 40) as usize % 13];
            }
            variants.push(b);
        }
        for (round, b) in variants.iter().enumerate() {
            let p = dir.join(format!("{round}-{name}"));
            std::fs::write(&p, b).unwrap();
            let started = std::time::Instant::now();
            let _ = inspect(&p);
            assert!(
                started.elapsed().as_secs() < 2,
                "{name} variant {round} took too long"
            );
        }
    }
    // A cut XML document still shows what was read.
    let text = std::fs::read_to_string(fixture("ccda-ccd.xml")).unwrap();
    let cut = dir.join("cut-ccda.xml");
    // Cut between two tags (a half-written tag is an XML error, reported as such).
    let at = text[text.len() / 2..].find('>').unwrap() + text.len() / 2 + 1;
    std::fs::write(&cut, &text[..at]).unwrap();
    let i = inspect(&cut).unwrap().unwrap();
    assert_eq!(fact(&i, "Patient name"), "Eve Everywoman");
    assert_eq!(check(&i, Stage::Parsed, "ends before").0, State::Partial);
    let _ = std::fs::remove_dir_all(&dir);
}
