//! Electronic data interchange standards (`inspect::standards`): UN/EDIFACT, ANSI X12, HL7 v2
//! and IATA Cargo-IMP. Fixtures in tests/fixtures/standards are written by hand from the
//! published message layouts.
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
        .unwrap_or_else(|| panic!("no fact {group:?}/{label:?} in {:#?}", i.facts))
}

fn check(i: &Inspection, stage: Stage, contains: &str) -> State {
    i.checks
        .iter()
        .find(|c| c.stage == stage && c.detail.contains(contains))
        .unwrap_or_else(|| panic!("no {stage:?} check {contains:?} in {:#?}", i.checks))
        .state
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

/// The bytes of the file a source range points at.
fn source_text(name: &str, r: &std::ops::Range<u64>) -> String {
    let bytes = std::fs::read(fixture(name)).unwrap();
    String::from_utf8_lossy(&bytes[r.start as usize..r.end as usize]).into_owned()
}

fn no_failures(i: &Inspection) {
    let failed: Vec<_> = i
        .checks
        .iter()
        .filter(|c| c.state == State::Failed)
        .collect();
    assert!(failed.is_empty(), "{failed:#?}");
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("isolinear-edi-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------------------------
// UN/EDIFACT

#[test]
fn edifact_eancom_invoice_with_parties_lines_totals_and_tax() {
    let name = "edifact-invoic.edi";
    let i = open(name);
    assert_eq!(i.family, "UN/EDIFACT");
    assert_eq!(i.kind, "INVOIC — invoice (GS1 EANCOM)");
    assert_eq!(
        i.version.as_deref(),
        Some("D.96A (UN), EANCOM subset EAN008")
    );
    assert_eq!(fact(&i, "Syntax"), "UNOC version 3");
    assert_eq!(
        fact_in(&i, "Interchange", "Sender"),
        "5401234000012 (qualifier 14 (GS1 GLN))"
    );
    assert_eq!(fact(&i, "Control reference"), "INV00042");
    assert_eq!(fact(&i, "Test indicator"), "1 (test)");
    assert_eq!(
        fact(&i, "EANCOM subset"),
        "EAN008 on directory D.96A (EANCOM 1997)"
    );
    assert_eq!(fact(&i, "Invoice number"), "INV-2026-0042");
    assert_eq!(fact(&i, "Document type"), "380 (Commercial invoice)");
    assert_eq!(fact(&i, "Document date"), "20261101 (2026-11-01)");
    assert_eq!(fact(&i, "Delivery date actual"), "20261028 (2026-10-28)");
    assert_eq!(fact(&i, "Order number (buyer)"), "PO-77812");
    assert_eq!(
        fact(&i, "Order number (buyer) date"),
        "20261015 (2026-10-15)"
    );
    assert_eq!(fact(&i, "Currency"), "EUR");
    // ISO 8859-1 text (syntax UNOC) is decoded.
    assert_eq!(fact_in(&i, "Buyer (BY)", "Name"), "Hôpital Central SA");
    assert_eq!(fact_in(&i, "Buyer (BY)", "GLN"), "5401234000005");
    assert_eq!(
        fact_in(&i, "Buyer (BY)", "VAT registration number"),
        "LU12345678"
    );
    assert_eq!(fact_in(&i, "Supplier (SU)", "City"), "54290 Trier");
    assert_eq!(fact_in(&i, "Delivery party (DP)", "GLN"), "5401234000036");
    assert_eq!(fact(&i, "Invoice amount (77)"), "228.74");
    assert_eq!(fact(&i, "Total line items amount (79)"), "195.50");

    let lines = table(&i, "Line items");
    assert_eq!(lines.rows.len(), 2);
    assert_eq!(cell(lines, 0, "Product ID"), "4001234123455");
    assert_eq!(cell(lines, 0, "ID type"), "SRV");
    assert_eq!(cell(lines, 0, "Other IDs"), "SA ART-100");
    // The released colon ("?:") is part of the description, not a separator.
    assert_eq!(
        cell(lines, 0, "Description"),
        "Nitrile gloves, size M: box of 100"
    );
    assert_eq!(cell(lines, 0, "Quantity"), "20");
    assert_eq!(cell(lines, 0, "Unit"), "PCE");
    assert_eq!(cell(lines, 0, "Price"), "7.50 (AAA)");
    assert_eq!(cell(lines, 1, "Line amount"), "45.50");
    assert_eq!(cell(lines, 1, "Tax %"), "17");
    let src = lines.rows[1].source.clone().unwrap();
    let text = source_text(name, &src);
    assert!(text.starts_with("LIN+2++4001234123462:SRV'"), "{text}");
    assert!(text.trim_end().ends_with("TAX+7+VAT+++:::17+S'"), "{text}");

    let tax = table(&i, "Tax summary");
    assert_eq!(tax.rows.len(), 1);
    assert_eq!(
        tax.rows[0].cells[..5],
        ["VAT", "S", "17", "195.50", "33.24"]
    );

    assert_eq!(check(&i, Stage::Parsed, "segments read"), State::Passed);
    assert_eq!(check(&i, Stage::Schema, ""), State::NotDone);
    assert_eq!(
        check(&i, Stage::Semantic, "UNT segment count"),
        State::Passed
    );
    assert_eq!(check(&i, Stage::Semantic, "UNZ count"), State::Passed);
    assert_eq!(
        check(&i, Stage::Semantic, "UNZ control reference"),
        State::Passed
    );
    assert_eq!(check(&i, Stage::Semantic, "Line item count"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "MOA 79"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "MOA 176"), State::Passed);
    // Four GLNs and two GTINs, all with valid check digits.
    assert_eq!(
        check(&i, Stage::Semantic, "GS1 check digits"),
        State::Passed
    );
    assert_eq!(table(&i, "GS1 identifiers").rows.len(), 6);
    no_failures(&i);

    // Outline: the interchange encloses the message, which encloses its segments.
    let unh = i
        .outline
        .iter()
        .find(|n| n.label.starts_with("UNH"))
        .unwrap();
    assert_eq!(unh.label, "UNH — Message header");
    assert_eq!(unh.depth, 1);
    let bgm = i
        .outline
        .iter()
        .find(|n| n.label.starts_with("BGM"))
        .unwrap();
    assert_eq!(bgm.depth, 2);
    let bgm02 = i.outline.iter().find(|n| n.label == "BGM02").unwrap();
    assert_eq!(bgm02.value, "INV-2026-0042");
    assert_eq!(
        source_text(name, bgm02.source.as_ref().unwrap()),
        "INV-2026-0042"
    );
    let segments = table(&i, "Segments");
    assert_eq!(segments.rows.len(), 40);
}

#[test]
fn edifact_orders_with_default_separators_and_released_characters() {
    let i = open("edifact-orders.edi");
    assert_eq!(i.kind, "ORDERS — purchase order");
    assert_eq!(
        fact(&i, "Separators"),
        "component :  element +  decimal .  release ?  terminator '"
    );
    assert_eq!(fact(&i, "Order number"), "PO-77812");
    assert_eq!(fact(&i, "Delivery date requested"), "20261105 (2026-11-05)");
    assert_eq!(fact_in(&i, "Buyer (BY)", "Name"), "Smith + Sons Ltd");
    assert_eq!(fact_in(&i, "Buyer (BY)", "Party ID"), "BUYER01 (agency 92)");
    // A released terminator does not end the segment.
    assert_eq!(
        fact_in(&i, "Delivery party (DP)", "Name"),
        "Smith + Sons Warehouse 'B'"
    );
    let lines = table(&i, "Line items");
    assert_eq!(lines.rows.len(), 2);
    assert_eq!(cell(lines, 1, "Product ID"), "ART-200");
    assert_eq!(cell(lines, 1, "Quantity"), "10");
    assert_eq!(cell(lines, 1, "Price"), "4.55 (AAA)");
    assert_eq!(cell(lines, 1, "Description"), "Hand sanitizer");
    assert!(!i.kind.contains("EANCOM"));
    assert!(i.tables.iter().all(|t| t.name != "GS1 identifiers"));
    no_failures(&i);
}

#[test]
fn edifact_desadv_packaging_hierarchy_and_a_bad_sscc() {
    let i = open("edifact-desadv.edi");
    assert_eq!(i.kind, "DESADV — despatch advice (GS1 EANCOM)");
    assert_eq!(fact(&i, "Despatch advice number"), "DES-2026-0007");
    assert_eq!(fact(&i, "Despatch date"), "20261031 (2026-10-31)");
    assert_eq!(fact(&i, "Shipment reference number"), "SHIP-5512");
    assert_eq!(
        fact(&i, "Transport"),
        "conveyance TRIP-88, mode 30, carrier CARRIERX"
    );
    let p = table(&i, "Packages");
    assert_eq!(p.rows.len(), 3);
    assert_eq!(cell(p, 1, "Parent"), "1");
    assert_eq!(cell(p, 1, "Package type"), "201 (pallet ISO 1)");
    assert_eq!(cell(p, 1, "SSCC"), "000123456000000005");
    assert_eq!(cell(p, 1, "Lines"), "1");
    let lines = table(&i, "Line items");
    assert_eq!(cell(lines, 1, "Package (CPS)"), "3");
    assert_eq!(cell(lines, 1, "Quantity"), "10");
    // The hierarchy: CPS 1 with CPS 2 and 3 under it.
    let h: Vec<_> = i
        .outline
        .iter()
        .skip_while(|n| n.label != "Packaging hierarchy (CPS)")
        .take(4)
        .map(|n| (n.depth, n.label.as_str()))
        .collect();
    assert_eq!(
        h,
        [
            (0, "Packaging hierarchy (CPS)"),
            (1, "CPS 1"),
            (2, "CPS 2"),
            (2, "CPS 3")
        ]
    );
    // The second SSCC was written with a wrong check digit (…013 instead of …012).
    assert_eq!(
        check(&i, Stage::Semantic, "GS1 check digits"),
        State::Failed
    );
    assert!(i.checks.iter().any(|c| {
        c.detail
            .contains("SSCC 000123456000000013 (GIN+BJ) has a wrong check digit")
    }));
    assert_eq!(
        check(&i, Stage::Semantic, "UNT segment count"),
        State::Passed
    );
}

#[test]
fn edifact_custom_una_with_decimal_comma() {
    let i = open("edifact-custom-una.edi");
    assert_eq!(i.kind, "INVOIC — invoice");
    assert_eq!(fact(&i, "Service string advice"), "UNA*|,! ~");
    assert_eq!(
        fact(&i, "Separators"),
        "component *  element |  decimal ,  release !  terminator ~"
    );
    assert_eq!(
        fact(&i, "Sender"),
        "SENDER1 (qualifier ZZZ (mutually defined))"
    );
    assert_eq!(fact(&i, "Invoice number"), "INV|77");
    assert_eq!(fact_in(&i, "Supplier (SU)", "Name"), "Pipe | Co");
    let lines = table(&i, "Line items");
    // Values as written, decimal comma kept.
    assert_eq!(cell(lines, 0, "Line amount"), "100,50");
    // 100,50 + 20,25 = 120,75 with the decimal comma understood.
    assert_eq!(check(&i, Stage::Semantic, "MOA 79"), State::Passed);
    assert_eq!(
        check(&i, Stage::Semantic, "UNT segment count"),
        State::Passed
    );
    no_failures(&i);
}

#[test]
fn edifact_other_message_in_a_group() {
    let i = open("edifact-iftsta.edi");
    assert_eq!(i.kind, "IFTSTA — international multimodal status report");
    assert_eq!(
        fact(&i, "Message type"),
        "IFTSTA (international multimodal status report)"
    );
    assert_eq!(fact(&i, "Group reference"), "GRP1");
    assert_eq!(
        check(&i, Stage::Parsed, "No business view for IFTSTA"),
        State::Partial
    );
    assert_eq!(
        check(&i, Stage::Semantic, "UNE message count"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "UNE group reference"),
        State::Passed
    );
    assert_eq!(check(&i, Stage::Semantic, "UNZ count"), State::Passed);
    assert!(i.outline.iter().any(|n| n.label == "STS — Status"));
}

#[test]
fn edifact_wrong_counts_and_references_fail() {
    let dir = scratch("edifact");
    let text = std::fs::read(fixture("edifact-iftsta.edi")).unwrap();
    let text = String::from_utf8(text).unwrap();
    let bad = text
        .replace("UNT+6+S1'", "UNT+7+S2'")
        .replace("UNZ+1+IFT1'", "UNZ+2+IFT9'");
    let p = dir.join("bad.edi");
    std::fs::write(&p, bad).unwrap();
    let i = inspect(&p).unwrap().unwrap();
    assert_eq!(
        check(&i, Stage::Semantic, "UNT segment count"),
        State::Failed
    );
    assert!(
        i.checks
            .iter()
            .any(|c| c.detail.contains("message S1: declared 7, found 6"))
    );
    assert_eq!(
        check(&i, Stage::Semantic, "UNT message reference"),
        State::Failed
    );
    assert_eq!(check(&i, Stage::Semantic, "UNZ count"), State::Failed);
    assert_eq!(
        check(&i, Stage::Semantic, "UNZ control reference"),
        State::Failed
    );
    // Truncated: what was read is shown, marked partial.
    let cut = dir.join("cut.edi");
    std::fs::write(&cut, &text[..text.find("RFF").unwrap()]).unwrap();
    let i = inspect(&cut).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Parsed, "ends without UNT"), State::Partial);
    assert_eq!(
        check(&i, Stage::Semantic, "UNT segment count"),
        State::Failed
    );
    assert_eq!(fact(&i, "Message reference"), "S1");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------------------------
// ANSI X12

#[test]
fn x12_810_invoice() {
    let name = "x12-810.x12";
    let i = open(name);
    assert_eq!(i.family, "ANSI X12");
    assert_eq!(i.kind, "810 — invoice");
    assert_eq!(i.version.as_deref(), Some("004010"));
    assert_eq!(
        fact(&i, "Separators"),
        "element *  component >  repetition —  terminator ~"
    );
    assert_eq!(
        fact_in(&i, "Interchange", "Sender"),
        "SENDER (qualifier ZZ)"
    );
    assert_eq!(fact_in(&i, "Interchange", "Control number"), "000000905");
    assert_eq!(fact(&i, "Usage"), "P (production)");
    assert_eq!(fact(&i, "Functional identifier"), "IN (invoice)");
    assert_eq!(fact(&i, "Invoice number"), "INV-3001");
    assert_eq!(fact(&i, "Invoice date"), "20261101 (2026-11-01)");
    assert_eq!(fact(&i, "Purchase order number"), "PO-4501");
    assert_eq!(fact(&i, "Currency"), "USD");
    assert_eq!(fact(&i, "Terms net days"), "30");
    assert_eq!(fact_in(&i, "Bill-to party (BT)", "Name"), "Acme Hospital");
    assert_eq!(
        fact_in(&i, "Bill-to party (BT)", "City/state/postal code"),
        "Springfield IL 62701 US"
    );
    assert_eq!(
        fact(&i, "Total invoice amount"),
        "19550 (195.50, two implied decimals)"
    );
    let lines = table(&i, "Line items");
    assert_eq!(lines.rows.len(), 2);
    assert_eq!(cell(lines, 0, "Product IDs"), "UP 012345678905, VN ART-100");
    assert_eq!(cell(lines, 0, "Description"), "Nitrile gloves, box of 100");
    assert_eq!(cell(lines, 1, "Unit price"), "4.55");
    let src = lines.rows[0].source.clone().unwrap();
    assert!(source_text(name, &src).starts_with("IT1*1*20*BX"));
    assert_eq!(
        check(&i, Stage::Semantic, "SE segment count"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "SE control number"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "GE transaction set count"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "IEA control number"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Line item count (CTT01"),
        State::Passed
    );
    // The remit-to GLN and the UPCs carry valid check digits.
    assert_eq!(
        check(&i, Stage::Semantic, "GS1 check digits"),
        State::Passed
    );
    assert_eq!(check(&i, Stage::Schema, ""), State::NotDone);
    no_failures(&i);
}

#[test]
fn x12_850_purchase_order_with_a_wrong_group_control_number() {
    let i = open("x12-850.x12");
    assert_eq!(i.kind, "850 — purchase order");
    assert_eq!(fact(&i, "Purchase order number"), "PO-4501");
    assert_eq!(fact(&i, "Order type"), "SA (stand-alone order)");
    assert_eq!(fact(&i, "Delivery requested"), "20261105 (2026-11-05)");
    assert_eq!(fact_in(&i, "Ship to (ST)", "Name"), "Acme Hospital Dock 3");
    let lines = table(&i, "Line items");
    assert_eq!(lines.rows.len(), 3);
    assert_eq!(cell(lines, 2, "Quantity"), "5");
    assert_eq!(cell(lines, 2, "Unit"), "CS");
    assert_eq!(
        check(&i, Stage::Semantic, "Line item count (CTT01"),
        State::Passed
    );
    // GE02 says 77, GS06 says 76.
    assert_eq!(
        check(&i, Stage::Semantic, "GE control number"),
        State::Failed
    );
    assert!(
        i.checks
            .iter()
            .any(|c| c.detail.contains("group 76: declared 77, found 76"))
    );
    assert_eq!(
        check(&i, Stage::Semantic, "SE segment count"),
        State::Passed
    );
}

#[test]
fn x12_856_ship_notice_hierarchy() {
    let i = open("x12-856.x12");
    assert_eq!(i.kind, "856 — ship notice/manifest");
    assert_eq!(fact(&i, "Shipment ID"), "SHIP-5512");
    assert_eq!(
        fact(&i, "Hierarchical structure"),
        "0001 (shipment, order, packaging, item)"
    );
    assert_eq!(fact(&i, "Bill of lading"), "BOL-889");
    assert_eq!(fact(&i, "Weight"), "350 LB");
    assert_eq!(fact(&i, "Carrier"), "CARX");
    assert_eq!(fact(&i, "Items"), "2");
    let h = table(&i, "Hierarchy");
    assert_eq!(h.rows.len(), 6);
    assert_eq!(cell(h, 3, "Level"), "I (Item)");
    assert_eq!(cell(h, 3, "Parent"), "3");
    // Items inherit their order and package from the levels above.
    assert_eq!(cell(h, 3, "Purchase order"), "PO-4501");
    assert_eq!(cell(h, 3, "Package marks"), "GM 000123456000000005");
    assert_eq!(cell(h, 5, "Package marks"), "GM 00000123456000000012");
    assert_eq!(cell(h, 5, "Quantity shipped"), "10");
    let tree: Vec<_> = i
        .outline
        .iter()
        .skip_while(|n| n.label != "HL hierarchy")
        .take(7)
        .map(|n| (n.depth, n.label.as_str()))
        .collect();
    assert_eq!(
        tree,
        [
            (0, "HL hierarchy"),
            (1, "HL 1 — Shipment"),
            (2, "HL 2 — Order"),
            (3, "HL 3 — Pack"),
            (4, "HL 4 — Item"),
            (3, "HL 5 — Pack"),
            (4, "HL 6 — Item"),
        ]
    );
    assert_eq!(
        check(&i, Stage::Semantic, "HL parent references"),
        State::Passed
    );
    // SSCCs (one with its "00" application identifier) and UPCs.
    assert_eq!(
        check(&i, Stage::Semantic, "GS1 check digits"),
        State::Passed
    );
    no_failures(&i);
}

#[test]
fn x12_837_professional_claim() {
    let i = open("x12-837p.x12");
    assert_eq!(i.kind, "837 — health care claim (Professional)");
    assert_eq!(
        i.version.as_deref(),
        Some("005010X222A1 (837 Professional)")
    );
    assert_eq!(
        fact(&i, "Separators"),
        "element *  component :  repetition ^  terminator ~"
    );
    assert_eq!(fact(&i, "Usage"), "T (test)");
    assert_eq!(
        fact_in(&i, "Billing provider (85)", "Name"),
        "SPRINGFIELD CLINIC"
    );
    assert_eq!(
        fact_in(&i, "Billing provider (85)", "ID"),
        "1234567893 (XX (NPI))"
    );
    assert_eq!(fact_in(&i, "Subscriber (IL)", "Name"), "JOHN A DOE");
    assert_eq!(
        fact_in(&i, "Subscriber (IL)", "Birth date"),
        "19800115 (1980-01-15)"
    );
    assert_eq!(fact(&i, "Claim filing indicator"), "CI");
    assert_eq!(fact(&i, "Prior authorization"), "AUTH-9");
    let c = table(&i, "Claims");
    assert_eq!(c.rows.len(), 1);
    assert_eq!(cell(c, 0, "Claim ID"), "PCN-0001");
    assert_eq!(cell(c, 0, "Patient"), "JOHN A DOE");
    assert_eq!(cell(c, 0, "Member ID"), "MEMBER001");
    assert_eq!(cell(c, 0, "Payer"), "EXAMPLE PAYER");
    assert_eq!(cell(c, 0, "Total charge"), "175.00");
    assert_eq!(
        cell(c, 0, "Facility/place of service"),
        "11 (place of service)"
    );
    assert_eq!(cell(c, 0, "Frequency"), "1 (original)");
    assert_eq!(cell(c, 0, "Diagnoses"), "J209, R05");
    let s = table(&i, "Service lines");
    assert_eq!(s.rows.len(), 2);
    assert_eq!(cell(s, 0, "Procedure"), "HC:99213");
    assert_eq!(cell(s, 0, "Modifiers"), "25");
    assert_eq!(cell(s, 0, "Diagnosis pointers"), "1,2");
    assert_eq!(cell(s, 1, "Service date"), "20261028 (2026-10-28)");
    let d = table(&i, "Diagnosis codes");
    assert_eq!(d.rows[0].cells[2], "ABK (principal diagnosis (ICD-10-CM))");
    assert_eq!(
        check(&i, Stage::Semantic, "Claim total charge"),
        State::Passed
    );
    no_failures(&i);
}

#[test]
fn x12_834_enrollment() {
    let i = open("x12-834.x12");
    assert_eq!(i.kind, "834 — benefit enrollment and maintenance");
    assert_eq!(fact(&i, "Master policy number"), "POLICY-123");
    assert_eq!(
        fact_in(&i, "Plan sponsor (P5)", "Name"),
        "EXAMPLE EMPLOYER INC"
    );
    assert_eq!(
        fact_in(&i, "Insurer (IN)", "ID"),
        "987654321 (FI (federal tax ID))"
    );
    assert_eq!(fact(&i, "Members"), "2");
    let m = table(&i, "Members");
    assert_eq!(m.rows.len(), 2);
    assert_eq!(cell(m, 0, "Relationship"), "18 (self)");
    assert_eq!(cell(m, 0, "Maintenance"), "021 (addition)");
    assert_eq!(cell(m, 0, "Member ID"), "SUB-001");
    assert_eq!(cell(m, 1, "Name"), "JANE DOE");
    assert_eq!(cell(m, 1, "Relationship"), "01 (spouse)");
    assert_eq!(cell(m, 1, "Gender"), "F");
    assert_eq!(
        cell(m, 0, "Coverages"),
        "HLT (health) PPO GOLD (FAM (family)) from 20261101 (2026-11-01)"
    );
    let c = table(&i, "Coverages");
    assert_eq!(cell(c, 1, "Member"), "JANE DOE");
    assert_eq!(cell(c, 1, "Begin"), "20261101 (2026-11-01)");
    no_failures(&i);
}

#[test]
fn x12_835_remittance_balances() {
    let i = open("x12-835.x12");
    assert_eq!(i.kind, "835 — health care claim payment/advice");
    assert_eq!(fact(&i, "Total payment amount"), "155.00");
    assert_eq!(fact(&i, "Payment method"), "ACH (automated clearing house)");
    assert_eq!(fact(&i, "Payment date"), "20261105 (2026-11-05)");
    assert_eq!(fact(&i, "Check/EFT trace number"), "EFT-778899");
    assert_eq!(fact_in(&i, "Payee (PE)", "Name"), "SPRINGFIELD CLINIC");
    let c = table(&i, "Claim payments");
    assert_eq!(c.rows.len(), 2);
    assert_eq!(cell(c, 0, "Status"), "1 (processed as primary)");
    assert_eq!(cell(c, 0, "Paid"), "130.00");
    assert_eq!(cell(c, 0, "Patient"), "JOHN DOE");
    assert_eq!(cell(c, 0, "Service lines"), "2");
    let a = table(&i, "Adjustments");
    assert_eq!(a.rows.len(), 4);
    assert_eq!(cell(a, 1, "Group"), "PR (patient responsibility)");
    assert_eq!(cell(a, 1, "Level"), "service HC:99213");
    let p = table(&i, "Provider adjustments");
    assert_eq!(cell(p, 0, "Reason"), "WO");
    assert_eq!(cell(p, 0, "Amount"), "5.00");
    // 130.00 + 30.00 − 5.00 = 155.00, and each claim and line balances.
    assert_eq!(
        check(&i, Stage::Semantic, "Total payment (BPR02)"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Claim payment (CLP04)"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Service line balance"),
        State::Passed
    );
    no_failures(&i);
    // An unbalanced line is reported.
    let dir = scratch("x12");
    let text = std::fs::read_to_string(fixture("x12-835.x12")).unwrap();
    let p = dir.join("bad.x12");
    std::fs::write(&p, text.replace("CAS*CO*45*15.00~", "CAS*CO*45*16.00~")).unwrap();
    let i = inspect(&p).unwrap().unwrap();
    assert_eq!(
        check(&i, Stage::Semantic, "Service line balance"),
        State::Failed
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn x12_270_271_eligibility_in_two_groups() {
    let i = open("x12-270-271.x12");
    assert_eq!(
        i.kind,
        "ANSI X12 interchange — 2 transaction sets (270, 271)"
    );
    assert_eq!(
        i.version.as_deref(),
        Some("005010X279A1 (270/271 Eligibility benefit inquiry and response)")
    );
    let q = table(&i, "Eligibility inquiries");
    assert_eq!(q.rows.len(), 1);
    assert_eq!(cell(q, 0, "Person"), "JOHN DOE");
    // The repetition separator (ISA11 "^") splits the service types.
    assert_eq!(cell(q, 0, "Service types"), "30, 98");
    assert_eq!(cell(q, 0, "Trace"), "TRACE-001");
    let b = table(&i, "Benefits");
    assert_eq!(b.rows.len(), 4);
    assert_eq!(cell(b, 0, "Information"), "1 (active coverage)");
    assert_eq!(cell(b, 1, "Amount"), "25.00");
    assert_eq!(cell(b, 3, "Percent"), "0.2");
    assert_eq!(
        check(&i, Stage::Semantic, "IEA functional group count"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "GE transaction set count"),
        State::Passed
    );
    assert_eq!(table(&i, "Transaction sets").rows.len(), 2);
    no_failures(&i);
}

#[test]
fn x12_other_transaction_set_is_generic() {
    let i = open("x12-997.x12");
    assert_eq!(i.kind, "997 — functional acknowledgment");
    assert_eq!(
        check(
            &i,
            Stage::Parsed,
            "No business view for transaction set 997"
        ),
        State::Partial
    );
    assert!(
        i.outline
            .iter()
            .any(|n| n.label == "AK9 — Functional group response trailer")
    );
    no_failures(&i);
}

// ---------------------------------------------------------------------------------------------
// HL7 v2

#[test]
fn hl7_adt_a01_with_cr_segments() {
    let name = "hl7-adt-a01.hl7";
    let i = open(name);
    assert_eq!(i.family, "HL7 v2");
    assert_eq!(i.kind, "ADT^A01 — admit/visit notification");
    assert_eq!(i.version.as_deref(), Some("2.5.1"));
    assert_eq!(fact(&i, "Sending application"), "ADT1");
    assert_eq!(fact(&i, "Sending facility"), "GOOD HEALTH HOSPITAL");
    assert_eq!(
        fact(&i, "Message type"),
        "ADT^A01^ADT_A01 (admit/visit notification)"
    );
    assert_eq!(fact(&i, "Message control ID"), "MSG00001");
    assert_eq!(fact(&i, "Processing ID"), "P (production)");
    assert_eq!(
        fact(&i, "Date/time of message"),
        "202611011030 (2026-11-01 10:30)"
    );
    assert_eq!(fact(&i, "Event type"), "A01");
    assert_eq!(fact_in(&i, "Patient", "Name"), "EVE E EVERYWOMAN");
    assert_eq!(
        fact_in(&i, "Patient", "Identifiers"),
        "PATID1234 (MR, GHH); 987654321 (SS, USSSA)"
    );
    assert_eq!(
        fact_in(&i, "Patient", "Birth date"),
        "19620320 (1962-03-20)"
    );
    assert_eq!(fact_in(&i, "Patient", "Administrative sex"), "F (female)");
    assert_eq!(
        fact_in(&i, "Patient", "Address"),
        "153 FERNWOOD DR., 35292 STATESVILLE, OH, USA"
    );
    assert_eq!(fact_in(&i, "Visit", "Patient class"), "I (inpatient)");
    assert_eq!(
        fact_in(&i, "Visit", "Location"),
        "2000, room 2012, bed 01, GHH"
    );
    assert_eq!(
        fact_in(&i, "Visit", "Attending doctor"),
        "AARON A ATTEND (004777)"
    );
    let msh7 = i
        .facts
        .iter()
        .find(|f| f.label == "Message control ID")
        .unwrap();
    assert_eq!(source_text(name, msh7.source.as_ref().unwrap()), "MSG00001");
    // Fields numbered and named, repetitions and components shown.
    let pid5 = i
        .outline
        .iter()
        .find(|n| n.label == "PID-5 Patient name")
        .unwrap();
    assert_eq!(pid5.value, "EVERYWOMAN^EVE^E^^^^L");
    assert!(
        i.outline
            .iter()
            .any(|n| n.label == "PID-3[2]" && n.value == "987654321^^^USSSA^SS")
    );
    assert!(
        i.outline
            .iter()
            .any(|n| n.label == "PID-5.2" && n.value == "EVE")
    );
    assert!(i.outline.iter().any(|n| n.label == "MSH-9 Message type"));
    assert!(
        i.outline
            .iter()
            .any(|n| n.label == "AL1-3 Allergen code/mnemonic/description")
    );
    assert!(
        i.outline
            .iter()
            .any(|n| n.label == "IN1-4 Insurance company name")
    );
    assert!(i.outline.iter().any(|n| n.label == "DG1-3 Diagnosis code"));
    assert!(i.outline.iter().any(|n| n.label == "NK1-3 Relationship"));
    assert_eq!(table(&i, "Segments").rows.len(), 8);
    assert_eq!(check(&i, Stage::Parsed, "segments read"), State::Passed);
    assert_eq!(check(&i, Stage::Schema, ""), State::NotDone);
}

#[test]
fn hl7_oru_results_with_escapes() {
    let i = open("hl7-oru-r01.hl7");
    assert_eq!(i.kind, "ORU^R01 — unsolicited observation result");
    let r = table(&i, "Results (OBX)");
    assert_eq!(r.rows.len(), 4);
    assert_eq!(cell(r, 0, "Observation ID"), "2093-3");
    assert_eq!(cell(r, 0, "Observation"), "Cholesterol, total");
    assert_eq!(cell(r, 0, "Value"), "196");
    assert_eq!(cell(r, 0, "Units"), "mg/dL");
    assert_eq!(cell(r, 0, "Reference range"), "<200");
    assert_eq!(cell(r, 0, "Observed"), "202611011100 (2026-11-01 11:00)");
    assert_eq!(cell(r, 1, "Abnormal flags"), "H");
    assert_eq!(cell(r, 2, "Abnormal flags"), "L");
    assert_eq!(cell(r, 2, "Status"), "F");
    // \T\ \X0A\ \F\ \S\ resolved.
    assert_eq!(cell(r, 3, "Value"), "Fasting & hydrated\nSample | ok^done");
    assert_eq!(cell(r, 3, "Value type"), "ST");
    assert_eq!(fact_in(&i, "Patient", "Name"), "JANE Q DOE");
    assert!(
        i.outline
            .iter()
            .any(|n| n.label == "OBR-4 Universal service identifier")
    );
    assert!(
        i.outline
            .iter()
            .any(|n| n.label == "ORC-1 Order control" && n.value == "RE")
    );
}

#[test]
fn hl7_batch_with_counts() {
    let i = open("hl7-batch.hl7");
    assert_eq!(
        i.kind,
        "ORU^R01 — 2 messages (unsolicited observation result)"
    );
    assert_eq!(fact(&i, "Messages"), "2");
    assert_eq!(fact_in(&i, "Message 2", "Message control ID"), "B2");
    assert_eq!(fact_in(&i, "Patient 2", "Name"), "BOB BETA");
    let m = table(&i, "Messages");
    assert_eq!(m.rows.len(), 2);
    assert_eq!(cell(m, 0, "Patient"), "ANN ALPHA");
    assert_eq!(cell(m, 1, "Segments"), "3");
    let r = table(&i, "Results (OBX)");
    assert_eq!(cell(r, 1, "Message"), "2");
    assert_eq!(cell(r, 1, "Value"), "11.1");
    assert_eq!(
        check(&i, Stage::Semantic, "BTS batch message count"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "FTS file batch count"),
        State::Passed
    );
    let first = i.outline.iter().find(|n| n.label == "Message 1").unwrap();
    assert_eq!(first.depth, 0);
}

// ---------------------------------------------------------------------------------------------
// IATA Cargo-IMP

#[test]
fn cargo_fwb_air_waybill() {
    let name = "cargo-fwb.txt";
    let i = open(name);
    assert_eq!(i.family, "IATA Cargo-IMP");
    assert_eq!(i.kind, "FWB — air waybill data");
    assert_eq!(i.version.as_deref(), Some("FWB/16"));
    assert_eq!(fact(&i, "Air waybill"), "172-12345675");
    assert_eq!(fact(&i, "Origin"), "LHR");
    assert_eq!(fact(&i, "Destination"), "JFK");
    assert_eq!(fact(&i, "Pieces"), "5");
    assert_eq!(fact(&i, "Weight"), "120.5 kg");
    assert_eq!(fact(&i, "Volume"), "0.75 m³");
    assert_eq!(fact(&i, "Flights"), "BA0175 on day 12");
    assert_eq!(fact(&i, "Routing"), "to JFK by BA");
    assert_eq!(fact_in(&i, "Shipper", "Name"), "EXAMPLE EXPORTS LTD");
    assert_eq!(fact_in(&i, "Consignee", "Place"), "NEW YORK, NY");
    assert_eq!(fact_in(&i, "Agent", "Account"), "9112345");
    assert_eq!(fact_in(&i, "Charges", "Currency"), "GBP");
    assert_eq!(fact_in(&i, "Charges", "Weight/valuation"), "prepaid");
    assert_eq!(fact_in(&i, "Issue", "Place"), "LONDON");
    let r = table(&i, "Rate description");
    assert_eq!(cell(r, 0, "Pieces"), "5");
    assert_eq!(cell(r, 0, "Rate class"), "Q");
    assert_eq!(cell(r, 0, "Chargeable weight"), "121.0");
    assert_eq!(cell(r, 0, "Total"), "544.50");
    assert_eq!(cell(r, 0, "Goods"), "MACHINE PARTS");
    let lines = table(&i, "Lines");
    assert_eq!(cell(lines, 1, "Identifier"), "AWB");
    let src = lines.rows[1].source.clone().unwrap();
    assert_eq!(source_text(name, &src), "172-12345675LHRJFK/T5K120.5MC0.75");
    assert_eq!(
        check(&i, Stage::Semantic, "Air waybill check digit"),
        State::Passed
    );
    no_failures(&i);
}

#[test]
fn cargo_ffm_manifest_with_a_bad_check_digit() {
    let i = open("cargo-ffm.txt");
    assert_eq!(i.kind, "FFM — airline flight manifest");
    assert_eq!(fact(&i, "Flight"), "BA0175");
    assert_eq!(fact(&i, "Departure airport"), "LHR");
    assert_eq!(fact(&i, "Aircraft registration"), "GBNLY");
    assert_eq!(fact(&i, "Points of unloading"), "JFK, ORD/13NOV");
    assert_eq!(fact(&i, "Manifest part"), "last part");
    assert_eq!(fact(&i, "Consignments"), "4");
    assert_eq!(
        fact(&i, "Weight of the listed consignments (computed)"),
        "246.0 kg"
    );
    let c = table(&i, "Consignments");
    assert_eq!(cell(c, 0, "ULD"), "PMC12345BA");
    assert_eq!(cell(c, 1, "Shipment"), "P (part consignment)");
    assert_eq!(cell(c, 1, "Total pieces"), "4");
    assert_eq!(cell(c, 1, "Description"), "BOOKS");
    assert_eq!(cell(c, 3, "Unloading point"), "ORD");
    assert_eq!(cell(c, 3, "ULD"), "");
    // 172-76543210: 7654321 mod 7 = 3, not 0.
    assert_eq!(
        check(&i, Stage::Semantic, "Air waybill check digit"),
        State::Failed
    );
    assert!(
        i.checks
            .iter()
            .any(|c| c.detail.contains("172-76543210: check digit 0, expected 3"))
    );
}

#[test]
fn cargo_fsu_status_after_a_type_b_envelope() {
    let i = open("cargo-fsu.txt");
    assert_eq!(i.kind, "FSU — status update");
    assert_eq!(fact(&i, "Air waybill"), "172-12345675");
    assert_eq!(fact(&i, "Latest status"), "DLV (delivered)");
    let s = table(&i, "Status events");
    assert_eq!(s.rows.len(), 5);
    assert_eq!(cell(s, 1, "Status"), "DEP");
    assert_eq!(cell(s, 1, "Flight"), "BA0175");
    assert_eq!(cell(s, 1, "Date/time"), "13NOV0900");
    assert_eq!(cell(s, 1, "Airports"), "LHR → JFK");
    assert_eq!(cell(s, 1, "Pieces"), "5");
    assert_eq!(cell(s, 4, "Details"), "J DOE");
    let lines = table(&i, "Lines");
    assert_eq!(cell(lines, 0, "Meaning"), "Type B envelope");
    assert_eq!(
        check(&i, Stage::Semantic, "Air waybill check digit"),
        State::Passed
    );
}

#[test]
fn cargo_fhl_house_manifest() {
    let i = open("cargo-fhl.txt");
    assert_eq!(i.kind, "FHL — consolidation list (house manifest)");
    assert_eq!(fact_in(&i, "Master waybill", "Air waybill"), "172-12345675");
    assert_eq!(fact(&i, "House waybills"), "2");
    let h = table(&i, "House waybills");
    assert_eq!(cell(h, 1, "House waybill"), "HAWB002");
    assert_eq!(cell(h, 1, "Weight"), "80.5 kg");
    assert_eq!(cell(h, 1, "Description"), "MACHINE PARTS");
    assert_eq!(
        check(&i, Stage::Semantic, "House waybill pieces"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Air waybill check digit"),
        State::Passed
    );
}

// ---------------------------------------------------------------------------------------------
// Recognition and damage

#[test]
fn ordinary_text_is_not_claimed() {
    let dir = scratch("plain");
    for (name, text) in [
        ("notes.txt", "UNA is a word.\nMSH is not a header here.\n"),
        ("isa.txt", "ISA rules apply to the account.\n"),
        ("fwb.txt", "FWB/16 is the message we will use\nnext week\n"),
        ("fwb2.txt", "FWB/16\nhello world\n"),
        ("msh.txt", "MSH|hello world\n"),
        ("unb.edi", "UNB+hello'\n"),
        ("dear.txt", "Dear team,\nplease find attached.\n"),
    ] {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        assert!(inspect(&p).is_none(), "{name} was claimed");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn truncated_and_damaged_files_do_not_panic() {
    let dir = scratch("damage");
    let names = [
        "edifact-invoic.edi",
        "edifact-orders.edi",
        "edifact-desadv.edi",
        "edifact-custom-una.edi",
        "edifact-iftsta.edi",
        "x12-810.x12",
        "x12-850.x12",
        "x12-856.x12",
        "x12-837p.x12",
        "x12-834.x12",
        "x12-835.x12",
        "x12-270-271.x12",
        "hl7-adt-a01.hl7",
        "hl7-oru-r01.hl7",
        "hl7-batch.hl7",
        "cargo-fwb.txt",
        "cargo-ffm.txt",
        "cargo-fsu.txt",
        "cargo-fhl.txt",
    ];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for name in names {
        let bytes = std::fs::read(fixture(name)).unwrap();
        // Every truncation point in the first part, and some later ones.
        for cut in (0..bytes.len()).step_by(7) {
            let p = dir.join(format!("cut-{cut}-{name}"));
            std::fs::write(&p, &bytes[..cut]).unwrap();
            if let Some(r) = inspect(&p) {
                let _ = r;
            }
            let _ = std::fs::remove_file(&p);
        }
        for round in 0..60 {
            let mut b = bytes.clone();
            let edits = 1 + next() % 8;
            for _ in 0..edits {
                let r = next();
                let at = (r % b.len() as u64) as usize;
                match r % 4 {
                    0 => b[at] = (r >> 24) as u8,
                    1 => {
                        b.remove(at);
                    }
                    2 => {
                        let set = b"+:'?*~|^\\&\r\n/";
                        b.insert(at, set[(r >> 40) as usize % set.len()]);
                    }
                    _ => {
                        let end = (at + (r >> 20) as usize % 40).min(b.len());
                        let piece = b[at..end].to_vec();
                        b.splice(at..at, piece);
                    }
                }
                if b.is_empty() {
                    b.push(b'U');
                }
            }
            let p = dir.join(format!("{round}-{name}"));
            std::fs::write(&p, &b).unwrap();
            let started = std::time::Instant::now();
            let _ = inspect(&p);
            assert!(started.elapsed().as_secs() < 2, "{name} round {round}");
            let _ = std::fs::remove_file(&p);
        }
    }
    // Hostile shapes: no terminators, huge element counts, deep HL chains, cycles.
    let mut hostile: Vec<(&str, Vec<u8>)> = vec![
        ("noterm.edi", b"UNB+UNOC:3+A+B+1+2".to_vec()),
        (
            "plus.edi",
            [b"UNB+UNOC:3".as_slice(), &[b'+'; 200_000]].concat(),
        ),
        (
            "colon.edi",
            [b"UNA:+.? 'UNB+UNOC:3+".as_slice(), &[b':'; 200_000], b"'"].concat(),
        ),
        (
            "release.edi",
            [
                b"UNB+UNOC:3+A'UNH+1+INVOIC:D:96A:UN'IMD+F++".as_slice(),
                &[b'?'; 100_001],
            ]
            .concat(),
        ),
        (
            "hl7.hl7",
            [
                b"MSH|^~\\&|".as_slice(),
                &[b'|'; 100_000],
                b"\rOBX",
                &[b'^'; 100_000],
            ]
            .concat(),
        ),
        (
            "hl7esc.hl7",
            [
                b"MSH|^~\\&|A\rOBX|1|ST|X||".as_slice(),
                &b"\\X".repeat(50_000),
            ]
            .concat(),
        ),
        (
            "fwb.txt",
            [
                b"FWB/16\n172-12345675LHRJFK/T5K1\n".as_slice(),
                &b"/\n".repeat(50_000),
            ]
            .concat(),
        ),
    ];
    let mut hl = b"ISA*00*          *00*          *ZZ*A              *ZZ*B              *261101*1030*U*00401*000000001*0*P*>~GS*SH*A*B*20261101*1030*1*X*004010~ST*856*0001~BSN*00*S*20261101*1200*0001~HL*A*B*S~HL*B*A*O~".to_vec();
    for k in 1..20_000 {
        hl.extend_from_slice(format!("HL*{k}*{}*I~", k + 1).as_bytes());
    }
    hostile.push(("cycle.x12", hl));
    let mut deep = b"ISA*00*          *00*          *ZZ*A              *ZZ*B              *261101*1030*U*00401*000000001*0*P*>~GS*SH*A*B*20261101*1030*1*X*004010~ST*856*0001~".to_vec();
    for k in 1..20_000 {
        deep.extend_from_slice(format!("HL*{k}*{}*I~", k - 1).as_bytes());
    }
    hostile.push(("deep.x12", deep));
    let mut cps = b"UNA:+.? 'UNB+UNOC:3+A+B+1+2'UNH+1+DESADV:D:96A:UN'".to_vec();
    for k in 1..20_000 {
        cps.extend_from_slice(format!("CPS+{k}+{}'", k + 1).as_bytes());
    }
    hostile.push(("cps.edi", cps));
    for (name, bytes) in hostile {
        let p = dir.join(name);
        std::fs::write(&p, &bytes).unwrap();
        let started = std::time::Instant::now();
        let r = inspect(&p);
        assert!(r.is_some(), "{name} not recognized");
        assert!(started.elapsed().as_secs() < 5, "{name} slow");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
