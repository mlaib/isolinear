//! Business, accounting and public-sector XML standards (UBL, Peppol, CII, XBRL, SAF-T, ACORD,
//! GS1, IATA Cargo-XML, Akoma Ntoso, SDMX, DDI, XML-DSig/XAdES). Fixtures in
//! tests/fixtures/standards are written by hand from the published specifications.
use isolinear_formats::{
    detect,
    inspect::{Inspection, Stage, State, Table, inspect_file},
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

/// The fact under `group` with `label`.
fn fact<'a>(i: &'a Inspection, group: &str, label: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.group == group && f.label == label)
        .map(|f| f.value.as_str())
        .unwrap_or_else(|| panic!("no fact {group}/{label} in {:#?}", i.facts))
}

fn check(i: &Inspection, stage: Stage, contains: &str) -> State {
    i.checks
        .iter()
        .find(|c| c.stage == stage && c.detail.contains(contains))
        .unwrap_or_else(|| panic!("no {stage:?} check {contains:?} in {:#?}", i.checks))
        .state
}

fn table<'a>(i: &'a Inspection, name: &str) -> &'a Table {
    i.tables
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no table {name:?} in {:?}", names(i)))
}

fn names(i: &Inspection) -> Vec<&str> {
    i.tables.iter().map(|t| t.name.as_str()).collect()
}

/// The cell of `row` under the column `column`.
fn cell<'a>(t: &'a Table, row: usize, column: &str) -> &'a str {
    let c = t
        .columns
        .iter()
        .position(|c| c == column)
        .unwrap_or_else(|| panic!("no column {column:?} in {:?}", t.columns));
    &t.rows[row].cells[c]
}

/// Write `text` to a temporary file and inspect it.
fn inspect_text(name: &str, text: &str) -> Option<Result<Inspection, String>> {
    let dir = std::env::temp_dir().join(format!("isolinear-standards-xml-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    let out = inspect(&path);
    let _ = std::fs::remove_file(&path);
    out
}

fn source_text(name: &str, range: &std::ops::Range<u64>) -> String {
    let text = std::fs::read_to_string(fixture(name)).unwrap();
    text[range.start as usize..range.end as usize].to_owned()
}

#[test]
fn ubl_invoice_parties_lines_tax_and_totals() {
    let i = open("ubl-invoice.xml");
    assert_eq!(i.family, "UBL");
    assert!(i.kind.starts_with("Invoice"), "{}", i.kind);
    assert!(i.version.as_deref().unwrap().contains("UBL 2.1"));
    assert_eq!(fact(&i, "Invoice", "Number"), "INV-2026-0042");
    assert_eq!(fact(&i, "Invoice", "Due date"), "2026-11-14");
    assert_eq!(fact(&i, "Invoice", "Currency"), "EUR");
    assert_eq!(fact(&i, "Supplier", "Name"), "Example Research S.A.");
    assert_eq!(fact(&i, "Supplier", "VAT number"), "LU12345613");
    assert_eq!(fact(&i, "Supplier", "City"), "Esch-sur-Alzette");
    assert_eq!(fact(&i, "Customer", "Name"), "Client & Partners GmbH");
    assert_eq!(fact(&i, "Customer", "Endpoint scheme"), "0088");
    assert_eq!(fact(&i, "Payment", "IBAN"), "LU280019400644750000");
    assert_eq!(fact(&i, "Payment", "Terms"), "30 days net");
    assert_eq!(fact(&i, "Totals", "Amount due"), "397.80");
    let lines = table(&i, "Invoice lines");
    assert_eq!(lines.rows.len(), 2);
    assert_eq!(cell(lines, 0, "Item"), "Data curation (hours)");
    assert_eq!(cell(lines, 0, "Unit"), "HUR");
    assert_eq!(cell(lines, 1, "Line amount"), "100.00");
    assert_eq!(cell(lines, 1, "Tax percent"), "17");
    let src = lines.rows[1].source.clone().unwrap();
    assert!(source_text("ubl-invoice.xml", &src).starts_with("<cac:InvoiceLine>"));
    let tax = table(&i, "Tax breakdown");
    assert_eq!(cell(tax, 0, "Taxable amount"), "340.00");
    assert_eq!(
        cell(table(&i, "Allowances and charges"), 0, "Amount"),
        "10.00"
    );
    for rule in [
        "Sum of line amounts",
        "Total without tax = lines",
        "Total with tax = total without tax + tax",
        "Tax total = sum of the tax breakdown",
        "Amount due",
    ] {
        assert_eq!(check(&i, Stage::Semantic, rule), State::Passed, "{rule}");
    }
    assert_eq!(check(&i, Stage::Schema, ""), State::NotDone);
    // The signature in the UBL extension is reported and not verified.
    assert_eq!(check(&i, Stage::Signature, "not verified"), State::NotDone);
}

#[test]
fn ubl_totals_that_do_not_add_up_fail() {
    let text = std::fs::read_to_string(fixture("ubl-invoice.xml")).unwrap();
    let wrong = text.replace(
        r#"<cbc:TaxInclusiveAmount currencyID="EUR">397.80"#,
        r#"<cbc:TaxInclusiveAmount currencyID="EUR">397.90"#,
    );
    let wrong = wrong.replace(
        r#"<cbc:LineExtensionAmount currencyID="EUR">100.00</cbc:LineExtensionAmount>"#,
        r#"<cbc:LineExtensionAmount currencyID="EUR">100.01</cbc:LineExtensionAmount>"#,
    );
    let i = inspect_text("wrong.xml", &wrong).unwrap().unwrap();
    assert_eq!(
        check(
            &i,
            Stage::Semantic,
            "Total with tax = total without tax + tax"
        ),
        State::Failed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Sum of line amounts"),
        State::Failed
    );
    // A missing total cannot be compared.
    let missing = text.replace(
        r#"<cbc:TaxExclusiveAmount currencyID="EUR">340.00</cbc:TaxExclusiveAmount>"#,
        "",
    );
    let i = inspect_text("missing.xml", &missing).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Semantic, "Total with tax"), State::NotDone);
}

#[test]
fn ubl_credit_note_refers_to_its_invoice() {
    let i = open("ubl-creditnote.xml");
    assert!(i.kind.starts_with("Credit note"), "{}", i.kind);
    assert!(i.kind.contains("XRechnung 3.0"), "{}", i.kind);
    assert_eq!(fact(&i, "Credit note", "Number"), "CN-2026-0007");
    assert_eq!(fact(&i, "Original invoice", "Number"), "INV-2026-0042");
    assert_eq!(fact(&i, "Original invoice", "Issue date"), "2026-10-15");
    let lines = table(&i, "Credit note lines");
    assert_eq!(cell(lines, 0, "Quantity"), "2");
    assert_eq!(check(&i, Stage::Semantic, "Total with tax"), State::Passed);
    assert!(!i.checks.iter().any(|c| c.stage == Stage::Signature));
}

#[test]
fn peppol_invoice_from_its_customization() {
    let i = open("peppol-invoice.xml");
    assert_eq!(i.family, "Peppol BIS Billing 3.0");
    assert_eq!(i.version.as_deref(), Some("3.0"));
    assert!(i.kind.contains("Peppol"), "{}", i.kind);
    assert_eq!(fact(&i, "Peppol", "Profile"), "Billing (P01), version 1.0");
    assert_eq!(
        fact(&i, "Peppol", "Seller endpoint"),
        "0088:7612345000015 (GLN)"
    );
    assert_eq!(
        fact(&i, "Peppol", "Buyer endpoint"),
        "0208:0123456789 (BE enterprise number)"
    );
    assert_eq!(
        fact(&i, "Supplier", "Legal name"),
        "SupplierOfficialName Ltd"
    );
    assert_eq!(check(&i, Stage::Semantic, "Total with tax"), State::Passed);
}

#[test]
fn cii_factur_x_profile_lines_and_totals() {
    let i = open("cii-facturx.xml");
    assert_eq!(i.family, "UN/CEFACT CII");
    assert!(i.version.as_deref().unwrap().starts_with("EN 16931"));
    assert!(i.kind.contains("Factur-X"), "{}", i.kind);
    assert_eq!(fact(&i, "Document", "Number"), "FX-2026-118");
    assert_eq!(fact(&i, "Document", "Issue date"), "20261028");
    assert_eq!(fact(&i, "Seller", "Name"), "Papeterie Exemple SARL");
    assert_eq!(fact(&i, "Seller", "VAT number"), "FR32123456782");
    assert_eq!(fact(&i, "Buyer", "City"), "Lyon");
    assert_eq!(fact(&i, "Payment", "IBAN"), "FR7630006000011234567890189");
    assert_eq!(fact(&i, "Payment", "Due date"), "20261130");
    assert_eq!(fact(&i, "Totals", "Total with tax"), "180.00");
    let lines = table(&i, "Invoice lines");
    assert_eq!(cell(lines, 0, "Item"), "Cahier A4 ligné");
    assert_eq!(cell(lines, 0, "Quantity"), "5");
    assert_eq!(cell(lines, 1, "Line amount"), "50.00");
    assert_eq!(cell(table(&i, "Tax breakdown"), 0, "Tax amount"), "30.00");
    assert_eq!(check(&i, Stage::Semantic, "Sum of line"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "Total with tax"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "Tax total"), State::Passed);
    // A BASIC WL file names its profile; a wrong grand total fails.
    let text = std::fs::read_to_string(fixture("cii-facturx.xml")).unwrap();
    let basicwl = text
        .replace(
            "<ram:ID>urn:cen.eu:en16931:2017</ram:ID>",
            "<ram:ID>urn:factur-x.eu:1p0:basicwl</ram:ID>",
        )
        .replace(
            "<ram:GrandTotalAmount>180.00",
            "<ram:GrandTotalAmount>181.00",
        );
    let i = inspect_text("basicwl.xml", &basicwl).unwrap().unwrap();
    assert_eq!(i.version.as_deref(), Some("Factur-X / ZUGFeRD BASIC WL"));
    assert_eq!(check(&i, Stage::Semantic, "Total with tax"), State::Failed);
}

#[test]
fn xbrl_instance_contexts_units_and_facts() {
    let i = open("xbrl-instance.xbrl");
    assert_eq!(i.family, "XBRL");
    assert_eq!(i.kind, "XBRL instance");
    assert!(
        fact(&i, "Instance", "Taxonomy entry point")
            .ends_with("full_ifrs_entry_point_2024-03-27.xsd")
    );
    assert_eq!(fact(&i, "Instance", "Contexts"), "3");
    assert_eq!(fact(&i, "Instance", "Facts"), "8");
    assert_eq!(fact(&i, "Entity", "Identifier"), "5299000EXAMPLE00LU42");
    let contexts = table(&i, "Contexts");
    assert_eq!(cell(contexts, 0, "Period"), "2025-01-01 – 2025-12-31");
    assert_eq!(cell(contexts, 1, "Period"), "2025-12-31");
    assert_eq!(
        cell(contexts, 2, "Dimensions"),
        "ifrs-full:SegmentsAxis=ex:ServicesMember; ex:ProjectAxis=P-17"
    );
    let units = table(&i, "Units");
    assert_eq!(cell(units, 2, "Measure"), "iso4217:EUR / xbrli:shares");
    let facts = table(&i, "Facts");
    assert_eq!(cell(facts, 0, "Concept"), "ifrs-full:Revenue");
    assert_eq!(cell(facts, 0, "Value"), "12500000");
    assert_eq!(cell(facts, 0, "Unit"), "iso4217:EUR");
    assert_eq!(cell(facts, 0, "Decimals"), "-3");
    assert_eq!(cell(facts, 0, "Footnotes"), "1");
    assert!(cell(facts, 1, "Dimensions").contains("ServicesMember"));
    assert_eq!(cell(facts, 3, "Period"), "2025-12-31");
    assert_eq!(cell(facts, 7, "Value"), "(nil)");
    assert_eq!(check(&i, Stage::Semantic, "references"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "taxonomy"), State::NotDone);
    // A fact pointing at a context that does not exist is reported.
    let text = std::fs::read_to_string(fixture("xbrl-instance.xbrl")).unwrap();
    let broken = text.replace(
        r#"contextRef="I2025" unitRef="EUR""#,
        r#"contextRef="I2024" unitRef="USD""#,
    );
    let i = inspect_text("broken.xbrl", &broken).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Semantic, "references"), State::Failed);
}

#[test]
fn inline_xbrl_facts_in_xhtml() {
    let i = open("inline-xbrl.xhtml");
    assert_eq!(i.family, "XBRL");
    assert_eq!(i.kind, "Inline XBRL report");
    assert_eq!(
        fact(&i, "Document", "Title"),
        "Example Research S.A. — Annual financial report 2025"
    );
    assert_eq!(fact(&i, "Inline XBRL", "Numeric facts"), "3");
    assert_eq!(fact(&i, "Inline XBRL", "Text facts"), "1");
    let facts = table(&i, "Facts");
    assert_eq!(
        cell(facts, 0, "Concept"),
        "ifrs-full:NameOfReportingEntityOrOtherMeansOfIdentification"
    );
    assert_eq!(cell(facts, 1, "Displayed value"), "12,500");
    assert_eq!(cell(facts, 1, "Scale"), "3");
    assert_eq!(cell(facts, 2, "Sign"), "-");
    assert_eq!(cell(facts, 2, "Format"), "ixt:num-dot-decimal");
    assert_eq!(cell(facts, 3, "Period"), "2025-12-31");
    assert_eq!(cell(facts, 1, "Unit"), "iso4217:EUR");
    assert_eq!(table(&i, "Contexts").rows.len(), 2);
    // Plain XHTML without Inline XBRL is not claimed.
    let plain = r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>x</title></head><body/></html>"#;
    assert!(inspect_text("plain.xhtml", plain).is_none());
}

#[test]
fn saft_portugal_ledger_master_files_and_invoices() {
    let i = open("saft-pt.xml");
    assert_eq!(i.family, "SAF-T");
    assert!(i.kind.contains("PT"), "{}", i.kind);
    assert_eq!(i.version.as_deref(), Some("1.04_01"));
    assert_eq!(fact(&i, "Company", "Name"), "Exemplo Investigação, Lda.");
    assert_eq!(fact(&i, "Company", "Tax registration"), "500000000");
    assert_eq!(fact(&i, "Period", "Fiscal year"), "2025");
    assert_eq!(fact(&i, "Software", "Certificate number"), "9999");
    assert_eq!(table(&i, "General ledger accounts").rows.len(), 2);
    assert_eq!(cell(table(&i, "Customers"), 0, "Tax ID"), "501234567");
    assert_eq!(cell(table(&i, "Suppliers"), 0, "City"), "Porto");
    assert_eq!(cell(table(&i, "Tax table"), 1, "Percent"), "6");
    let tx = table(&i, "Ledger transactions");
    assert_eq!(cell(tx, 0, "Journal"), "VND");
    assert_eq!(cell(tx, 0, "Debit"), "1170.00");
    assert_eq!(cell(tx, 0, "Credit"), "1170.00");
    assert_eq!(cell(tx, 0, "Lines"), "3");
    let lines = table(&i, "Ledger lines");
    assert_eq!(lines.rows.len(), 5);
    assert_eq!(cell(lines, 1, "Credit"), "1000.00");
    assert_eq!(cell(lines, 1, "Transaction"), "2025-03-14 VND 1");
    let inv = table(&i, "Sales invoices");
    assert_eq!(cell(inv, 1, "Status"), "A");
    assert_eq!(cell(table(&i, "Payments"), 0, "Amount"), "500.00");
    assert_eq!(
        check(&i, Stage::Semantic, "Debits equal credits"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Total debits = total credits"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Declared total debit"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Declared number of entries"),
        State::Passed
    );
    // The cancelled invoice is left out of the declared credit total.
    assert_eq!(
        check(&i, Stage::Semantic, "Sales invoices: declared total credit"),
        State::Passed
    );
    assert_eq!(check(&i, Stage::Signature, "hashes"), State::NotDone);
}

#[test]
fn saft_norway_reports_an_unbalanced_transaction() {
    let i = open("saft-no.xml");
    assert_eq!(i.kind, "SAF-T Financial (Norway)");
    assert_eq!(i.version.as_deref(), Some("1.30"));
    assert_eq!(fact(&i, "Company", "Name"), "Eksempel Forskning AS");
    assert_eq!(fact(&i, "Period", "Start year"), "2025");
    assert_eq!(cell(table(&i, "Customers"), 0, "Name"), "Kunde AS");
    assert_eq!(cell(table(&i, "Tax table"), 0, "Type"), "MVA");
    let lines = table(&i, "Ledger lines");
    assert_eq!(cell(lines, 0, "Debit"), "12500.00");
    assert_eq!(cell(lines, 0, "Customer"), "K10");
    assert_eq!(
        cell(table(&i, "Ledger transactions"), 1, "Credit"),
        "450.00"
    );
    let unbalanced = i
        .checks
        .iter()
        .find(|c| c.detail.starts_with("Debits differ"))
        .unwrap();
    assert_eq!(unbalanced.state, State::Failed);
    assert!(unbalanced.detail.ends_with("102"), "{}", unbalanced.detail);
    assert_eq!(
        check(&i, Stage::Semantic, "Total debits = total credits"),
        State::Failed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Declared total debit"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Declared total credit"),
        State::Failed
    );
}

#[test]
fn polish_jpk_counts_its_records() {
    let i = open("jpk-vat.xml");
    assert_eq!(i.family, "SAF-T");
    assert_eq!(i.version.as_deref(), Some("JPK_VAT (variant 2)"));
    assert_eq!(fact(&i, "Subject", "Tax ID (NIP)"), "5250000001");
    assert_eq!(cell(table(&i, "Sales records"), 0, "VAT (K_20)"), "230.00");
    assert_eq!(check(&i, Stage::Semantic, "sales records"), State::Failed);
}

#[test]
fn acord_policy_insured_coverages_and_losses() {
    let i = open("acord.xml");
    assert_eq!(i.family, "ACORD");
    assert!(i.kind.contains("Property & Casualty"));
    assert_eq!(fact(&i, "Message", "Message"), "PersAutoPolicyQuoteInqRq");
    assert_eq!(fact(&i, "Policy", "Policy number"), "PA-2026-001234");
    assert_eq!(fact(&i, "Policy", "Premium"), "642.00");
    assert_eq!(fact(&i, "Counts", "Vehicles"), "1");
    let policies = table(&i, "Policies");
    assert_eq!(cell(policies, 0, "Expiration"), "2027-05-15");
    assert_eq!(cell(policies, 0, "Line of business"), "AUTOP");
    let insured = table(&i, "Insured parties");
    assert_eq!(cell(insured, 0, "Name"), "Doe");
    assert_eq!(cell(insured, 0, "City"), "Springfield");
    let cov = table(&i, "Coverages");
    assert_eq!(cov.rows.len(), 3);
    assert_eq!(cell(cov, 0, "Limit"), "100000");
    assert_eq!(cell(cov, 1, "Deductible"), "500");
    assert_eq!(cell(table(&i, "Prior losses"), 0, "Paid"), "3150.00");
    assert_eq!(check(&i, Stage::Parsed, "only policies"), State::Partial);
}

#[test]
fn gs1_despatch_advice_and_check_digits() {
    let i = open("gs1-despatch.xml");
    assert_eq!(i.family, "GS1 XML");
    assert_eq!(i.kind, "GS1 XML — Despatch advice");
    assert_eq!(i.version.as_deref(), Some("3.4"));
    assert_eq!(fact(&i, "Header", "Sender"), "5412345000013");
    assert_eq!(fact(&i, "Header", "Type"), "Despatch Advice");
    assert_eq!(fact(&i, "Document", "Number"), "DA-100045");
    assert_eq!(fact(&i, "Parties", "Ship to GLN"), "7612345000022");
    let lines = table(&i, "Despatch lines");
    assert_eq!(cell(lines, 0, "GTIN"), "04006381333948");
    assert_eq!(cell(lines, 0, "SSCC"), "376123450000000016");
    assert_eq!(cell(lines, 1, "Quantity"), "12");
    assert_eq!(cell(table(&i, "Logistic units"), 0, "Lines"), "2");
    let ids = table(&i, "GS1 identifiers");
    assert!(ids.rows.iter().all(|r| r.cells[3] == "valid"));
    assert_eq!(
        check(&i, Stage::Semantic, "GS1 check digits"),
        State::Passed
    );
    // One wrong digit in a GTIN is caught.
    let text = std::fs::read_to_string(fixture("gs1-despatch.xml")).unwrap();
    let wrong = text.replace("<gtin>4006381333931</gtin>", "<gtin>4006381333932</gtin>");
    let i = inspect_text("gs1-wrong.xml", &wrong).unwrap().unwrap();
    let c = i
        .checks
        .iter()
        .find(|c| c.detail.starts_with("GS1 check digits"))
        .unwrap();
    assert_eq!(c.state, State::Failed);
    assert!(c.detail.contains("4006381333932"));
}

#[test]
fn iata_air_waybill_and_flight_manifest() {
    let i = open("iata-xfwb.xml");
    assert_eq!(i.family, "IATA Cargo-XML");
    assert_eq!(i.kind, "Air waybill (XFWB)");
    assert_eq!(fact(&i, "Waybill", "Number"), "176-12345675");
    assert_eq!(fact(&i, "Route", "Origin"), "LUX");
    assert_eq!(fact(&i, "Route", "Destination"), "DXB");
    assert_eq!(fact(&i, "Shipper", "Name"), "Example Research S.A.");
    assert_eq!(fact(&i, "Consignee", "Country"), "AE");
    assert_eq!(fact(&i, "Cargo", "Pieces"), "5");
    assert_eq!(fact(&i, "Cargo", "Gross weight"), "412.5");
    assert_eq!(fact(&i, "Cargo", "Weight unit"), "KGM");
    let routing = table(&i, "Routing");
    assert_eq!(cell(routing, 0, "Flight"), "EK9937");
    assert_eq!(cell(routing, 0, "From"), "LUX");
    assert_eq!(cell(table(&i, "Items"), 1, "Goods"), "CONSUMABLES");
    assert_eq!(check(&i, Stage::Semantic, "check digits"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "Total pieces"), State::Passed);

    let i = open("iata-xffm.xml");
    assert_eq!(i.kind, "Flight manifest (XFFM)");
    assert_eq!(fact(&i, "Flight", "Flight"), "EK9937");
    assert_eq!(fact(&i, "Flight", "Consignments"), "2");
    let legs = table(&i, "Legs");
    assert_eq!(cell(legs, 0, "Arrival airport"), "DXB");
    assert_eq!(cell(legs, 0, "Consignments"), "2");
    let c = table(&i, "Consignments");
    assert_eq!(cell(c, 1, "Air waybill"), "176-76543213");
    assert_eq!(cell(c, 1, "ULD"), "PMC12345EK");
    assert_eq!(cell(c, 1, "Unloaded at"), "DXB");
    assert_eq!(check(&i, Stage::Semantic, "Total pieces"), State::Passed);
    // A mistyped air waybill number fails its check digit.
    let text = std::fs::read_to_string(fixture("iata-xffm.xml")).unwrap();
    let wrong = text.replace("176-76543213", "176-76543214");
    let i = inspect_text("xffm-wrong.xml", &wrong).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Semantic, "check digit"), State::Failed);
}

#[test]
fn akoma_ntoso_identification_and_outline() {
    let i = open("akn-act.xml");
    assert_eq!(i.family, "Akoma Ntoso");
    assert_eq!(i.kind, "Akoma Ntoso act");
    assert_eq!(fact(&i, "Document", "Title"), "Open Research Data Act");
    assert_eq!(fact(&i, "Work", "URI"), "/akn/lu/act/2026-03-12/17/!main");
    assert_eq!(fact(&i, "Work", "Date"), "2026-03-12");
    assert_eq!(fact(&i, "Work", "Country"), "lu");
    assert_eq!(fact(&i, "Expression", "Language"), "eng");
    assert_eq!(fact(&i, "Manifestation", "Date"), "2026-04-02");
    assert_eq!(fact(&i, "Counts", "Passive modifications"), "1");
    assert_eq!(fact(&i, "Counts", "Inline references"), "1");
    // chapter, 2 articles (1 + 2 paragraphs), chapter, article (1 paragraph)
    let labels: Vec<(u16, &str)> = i
        .outline
        .iter()
        .map(|n| (n.depth, n.label.as_str()))
        .collect();
    assert_eq!(
        labels,
        [
            (0, "Chapter I"),
            (1, "Article 1"),
            (2, "Paragraph 1."),
            (1, "Article 2"),
            (2, "Paragraph 1."),
            (2, "Paragraph 2."),
            (0, "Chapter II"),
            (1, "Article 3"),
            (2, "Paragraph"),
        ]
    );
    assert_eq!(i.outline[1].value, "Purpose");
    let src = i.outline[3].source.clone().unwrap();
    assert!(source_text("akn-act.xml", &src).starts_with(r#"<article eId="art_2">"#));
    assert_eq!(cell(table(&i, "Lifecycle"), 1, "Type"), "amendment");
    assert_eq!(
        cell(table(&i, "References"), 1, "Shown as"),
        "Amending act 21/2026"
    );
    let m = table(&i, "Modifications");
    assert_eq!(cell(m, 0, "Type"), "substitution");
    assert_eq!(cell(m, 0, "Destination"), "#art_2__para_1");
}

#[test]
fn sdmx_data_and_structure_messages() {
    let i = open("sdmx-data.xml");
    assert_eq!(i.family, "SDMX");
    assert_eq!(i.kind, "SDMX-ML structure-specific data message");
    assert_eq!(i.version.as_deref(), Some("SDMX-ML 2.1"));
    assert_eq!(fact(&i, "Header", "Sender"), "ESTAT");
    assert_eq!(fact(&i, "Structure", "Dataflow"), "UNE_RT_A");
    assert_eq!(fact(&i, "Data", "Observations"), "4");
    assert_eq!(fact(&i, "Data", "Series"), "2");
    let obs = table(&i, "Observations");
    assert_eq!(
        obs.columns,
        [
            "freq",
            "age",
            "unit",
            "sex",
            "geo",
            "TIME_PERIOD",
            "OBS_VALUE",
            "OBS_FLAG"
        ]
    );
    assert_eq!(cell(obs, 1, "OBS_VALUE"), "6.4");
    assert_eq!(cell(obs, 1, "OBS_FLAG"), "p");
    assert_eq!(cell(obs, 2, "geo"), "BE");
    assert_eq!(cell(table(&i, "Series"), 0, "Observations"), "2");

    // Generic data (2.1): keys and values in child elements.
    let i = open("sdmx-generic.xml");
    assert_eq!(i.kind, "SDMX-ML generic data message");
    let obs = table(&i, "Observations");
    assert_eq!(
        obs.columns,
        ["FREQ", "CURRENCY", "TIME_PERIOD", "OBS_VALUE", "OBS_STATUS"]
    );
    assert_eq!(obs.rows[0].cells, ["M", "USD", "2026-09", "1.1105", "A"]);

    let i = open("sdmx-structure.xml");
    assert_eq!(i.kind, "SDMX-ML structure message");
    assert_eq!(fact(&i, "Content", "Codelists"), "2");
    let codelists = table(&i, "Codelists");
    assert_eq!(cell(codelists, 1, "Codes"), "3");
    let codes = table(&i, "Codes");
    assert_eq!(codes.rows.len(), 5);
    assert_eq!(cell(codes, 4, "Name"), "Japanese yen");
    assert_eq!(cell(codes, 4, "Codelist"), "CL_CURRENCY");
    let dims = table(&i, "Dimensions");
    assert_eq!(dims.rows.len(), 3);
    assert_eq!(cell(dims, 1, "Codelist"), "CL_CURRENCY");
    assert_eq!(cell(dims, 2, "Text type"), "ObservationalTimePeriod");
    assert_eq!(cell(dims, 0, "Data structure"), "ECB_EXR1");
    assert_eq!(table(&i, "Concepts").rows.len(), 4);
}

#[test]
fn ddi_codebook_study_and_variables() {
    let i = open("ddi-codebook.xml");
    assert_eq!(i.family, "DDI");
    assert_eq!(i.version.as_deref(), Some("Codebook 2.5"));
    assert_eq!(fact(&i, "Study", "Title"), "Luxembourg Health Survey 2025");
    assert_eq!(fact(&i, "Study", "First author"), "Müller, Anna");
    assert_eq!(fact(&i, "Study", "Authors"), "2");
    assert_eq!(fact(&i, "Study", "Producer"), "Example Research Institute");
    assert_eq!(fact(&i, "Study", "Production date"), "2026-02-01");
    assert_eq!(fact(&i, "Coverage", "Collection start"), "2025-03-01");
    assert_eq!(fact(&i, "Coverage", "Collection end"), "2025-06-30");
    assert_eq!(fact(&i, "Files", "Cases"), "4212");
    let vars = table(&i, "Variables");
    assert_eq!(vars.rows.len(), 3);
    assert_eq!(cell(vars, 0, "Name"), "sex");
    assert_eq!(cell(vars, 0, "Question"), "What is your sex?");
    assert_eq!(cell(vars, 0, "Categories"), "3");
    assert_eq!(cell(vars, 1, "Type"), "numeric");
    assert_eq!(cell(vars, 1, "Interval"), "contin");
    let cats = table(&i, "Categories");
    assert_eq!(cats.rows.len(), 7);
    assert_eq!(cell(cats, 2, "Missing"), "Y");
    assert_eq!(cell(cats, 0, "Frequency"), "2050");
    assert_eq!(cell(cats, 6, "Variable"), "health");
    assert_eq!(
        cell(table(&i, "Authors"), 1, "Affiliation"),
        "University of Example"
    );

    let i = open("ddi-lifecycle.xml");
    assert_eq!(i.version.as_deref(), Some("Lifecycle 3.3"));
    assert_eq!(fact(&i, "Study", "Title"), "European Values Pilot");
    let vars = table(&i, "Variables");
    assert_eq!(cell(vars, 0, "Name"), "trust");
    assert_eq!(cell(vars, 0, "Question"), "Q1");
    assert_eq!(
        cell(table(&i, "Questions"), 0, "Text"),
        "Do you trust people?"
    );
}

#[test]
fn xades_signature_in_a_document_of_no_other_standard() {
    let i = open("xades-signed.xml");
    assert_eq!(i.family, "XAdES");
    assert_eq!(i.kind, "XML document <Contract> with an XML signature");
    assert_eq!(i.version.as_deref(), Some("XAdES v1.3.2"));
    assert_eq!(
        fact(&i, "Signature", "Signature algorithm"),
        "RSA with SHA-256"
    );
    assert_eq!(
        fact(&i, "Signature", "Canonicalization"),
        "Exclusive canonical XML"
    );
    assert_eq!(fact(&i, "Signature", "ID"), "sig-1");
    assert_eq!(
        fact(&i, "Key", "Subject"),
        "CN=Jane Doe,O=Example Research S.A.,C=LU"
    );
    assert_eq!(fact(&i, "Key", "Certificate included"), "yes (1)");
    assert_eq!(fact(&i, "XAdES", "Signing time"), "2026-10-30T15:42:10Z");
    assert_eq!(
        fact(&i, "XAdES", "Signing certificate digest"),
        "Y2VydGlmaWNhdGUgZGlnZXN0IGV4YW1wbGU="
    );
    assert_eq!(fact(&i, "XAdES", "Policy"), "urn:oid:1.3.6.1.4.1.99999.1.1");
    assert_eq!(fact(&i, "XAdES", "Signer role"), "Managing director");
    assert_eq!(
        fact(&i, "XAdES", "Level (from the properties present)"),
        "T, with an explicit policy (EPES)"
    );
    let refs = table(&i, "References");
    assert_eq!(refs.rows.len(), 2);
    assert_eq!(cell(refs, 0, "Transforms"), "2");
    assert_eq!(cell(refs, 1, "URI"), "#sig-1-signedprops");
    assert_eq!(
        cell(refs, 0, "Digest value"),
        "3dJ3xG8e5w1V0m3bQ2vN6Z4tY9p8r7s6q5o4n3m2l1k="
    );
    assert_eq!(
        check(&i, Stage::Signature, "Signatures were not verified"),
        State::NotDone
    );
    // A detached signature as the root element.
    let text = std::fs::read_to_string(fixture("xades-signed.xml")).unwrap();
    let start = text.find("<ds:Signature").unwrap();
    let end = text.find("</ds:Signature>").unwrap() + "</ds:Signature>".len();
    let detached = format!("<?xml version=\"1.0\"?>\n{}", &text[start..end]);
    let i = inspect_text("detached.xml", &detached).unwrap().unwrap();
    assert_eq!(i.kind, "XAdES signature");
    assert_eq!(cell(table(&i, "References"), 0, "URI"), "");
    assert_eq!(check(&i, Stage::Signature, "not verified"), State::NotDone);
    // A plain XML signature without XAdES properties.
    let i = open("xmldsig-plain.xml");
    assert_eq!(i.family, "XML-DSig");
    assert_eq!(
        fact(&i, "Signature", "Signature algorithm"),
        "RSA with SHA-1"
    );
    assert_eq!(fact(&i, "Key", "Key value"), "RSA public key");
    assert_eq!(fact(&i, "Key", "Certificate included"), "no");
    // A signature deep inside a document is not enough to claim it.
    let deep =
        r#"<a><b><c><d><Signature xmlns="http://www.w3.org/2000/09/xmldsig#"/></d></c></b></a>"#;
    assert!(inspect_text("deep.xml", deep).is_none());
}

#[test]
fn damaged_and_truncated_business_documents() {
    let dir = std::env::temp_dir().join(format!(
        "isolinear-standards-xml-damage-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let all = [
        "ubl-invoice.xml",
        "ubl-creditnote.xml",
        "peppol-invoice.xml",
        "cii-facturx.xml",
        "xbrl-instance.xbrl",
        "inline-xbrl.xhtml",
        "saft-pt.xml",
        "saft-no.xml",
        "acord.xml",
        "gs1-despatch.xml",
        "iata-xfwb.xml",
        "iata-xffm.xml",
        "akn-act.xml",
        "sdmx-data.xml",
        "sdmx-structure.xml",
        "ddi-codebook.xml",
        "xades-signed.xml",
    ];
    // Truncated: what was read is shown, marked as partial.
    for name in all {
        let text = std::fs::read_to_string(fixture(name)).unwrap();
        // Cut after a tag, two thirds in (a cut inside a tag is an XML error, reported).
        let cut = text[..text.len() * 2 / 3].rfind('>').unwrap() + 1;
        let p = dir.join(format!("cut-{name}"));
        std::fs::write(&p, &text[..cut]).unwrap();
        let i = inspect(&p)
            .unwrap_or_else(|| panic!("cut {name} not recognized"))
            .unwrap_or_else(|e| panic!("cut {name}: {e}"));
        assert_eq!(
            check(&i, Stage::Parsed, "ends before"),
            State::Partial,
            "{name}"
        );
    }
    let i = inspect(&dir.join("cut-saft-pt.xml")).unwrap().unwrap();
    assert_eq!(fact(&i, "Company", "Name"), "Exemplo Investigação, Lda.");
    // Random damage to every fixture: recognized or not, no panic, quick.
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    for name in all {
        let bytes = std::fs::read(fixture(name)).unwrap();
        for round in 0..40 {
            let mut b = bytes.clone();
            for _ in 0..6 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let at = (seed % b.len() as u64) as usize;
                b[at] = (seed >> 32) as u8;
            }
            let p = dir.join(format!("{round}-{name}"));
            std::fs::write(&p, &b).unwrap();
            let started = std::time::Instant::now();
            let _ = inspect(&p);
            assert!(started.elapsed().as_secs() < 2, "{name}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
