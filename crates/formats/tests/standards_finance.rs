//! Financial text formats on top of plain text (`inspect::standards`): SWIFT MT, MT940/MT942,
//! BAI2, FIX, OFX, QIF and the French FEC. Fixtures in tests/fixtures/standards are written by
//! hand from the published layouts (tools: none; the FIX checksums were computed when writing).
use isolinear_formats::{
    detect,
    inspect::{Inspection, Stage, State, Table, inspect_file},
};
use std::path::{Path, PathBuf};

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/standards")
        .join(name)
}

pub fn inspect(path: &Path) -> Option<Result<Inspection, String>> {
    let head = std::fs::read(path).unwrap();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let format = detect(&name, &head[..head.len().min(65536)]).format.id;
    inspect_file(path, format, &|| false)
}

pub fn fact<'a>(i: &'a Inspection, label: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.label == label)
        .map(|f| f.value.as_str())
        .unwrap_or_else(|| panic!("no fact {label:?} in {:#?}", i.facts))
}

pub fn check(i: &Inspection, stage: Stage, contains: &str) -> State {
    i.checks
        .iter()
        .find(|c| c.stage == stage && c.detail.contains(contains))
        .unwrap_or_else(|| panic!("no {stage:?} check {contains:?} in {:#?}", i.checks))
        .state
}

fn detail<'a>(i: &'a Inspection, stage: Stage, contains: &str) -> &'a str {
    &i.checks
        .iter()
        .find(|c| c.stage == stage && c.detail.contains(contains))
        .unwrap_or_else(|| panic!("no {stage:?} check {contains:?} in {:#?}", i.checks))
        .detail
}

fn table<'a>(i: &'a Inspection, name: &str) -> &'a Table {
    i.tables.iter().find(|t| t.name == name).unwrap_or_else(|| {
        panic!(
            "no table {name:?} in {:?}",
            i.tables.iter().map(|t| &t.name).collect::<Vec<_>>()
        )
    })
}

/// Cell of `row` under the column `name`.
fn cell<'a>(t: &'a Table, row: usize, name: &str) -> &'a str {
    let c = t
        .columns
        .iter()
        .position(|c| c == name)
        .unwrap_or_else(|| panic!("no column {name:?} in {:?}", t.columns));
    &t.rows[row].cells[c]
}

/// The bytes a row points at.
fn source_text(path: &Path, t: &Table, row: usize) -> String {
    let bytes = std::fs::read(path).unwrap();
    let r = t.rows[row].source.clone().unwrap();
    String::from_utf8_lossy(&bytes[r.start as usize..r.end as usize]).into_owned()
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("isolinear-finance-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fixture with `from` replaced by `to`, written under `name` in `dir`.
fn variant(dir: &Path, fixture_name: &str, name: &str, from: &str, to: &str) -> PathBuf {
    let text = std::fs::read_to_string(fixture(fixture_name)).unwrap();
    assert!(text.contains(from), "{from:?} not in {fixture_name}");
    let p = dir.join(name);
    std::fs::write(&p, text.replacen(from, to, 1)).unwrap();
    p
}

#[test]
fn swift_mt103_headers_fields_and_amounts() {
    let path = fixture("mt103.fin");
    let i = inspect(&path).unwrap().unwrap();
    assert_eq!(i.family, "SWIFT MT");
    assert_eq!(i.kind, "MT103 — Single customer credit transfer");
    assert_eq!(fact(&i, "Sender (BIC)"), "BANKBEBBXXX");
    assert_eq!(fact(&i, "Receiver (BIC)"), "BANKDEFFXXX");
    assert_eq!(fact(&i, "Priority"), "N (normal)");
    assert_eq!(fact(&i, "Direction"), "Input (sent to SWIFT)");
    assert_eq!(fact(&i, "Message user reference"), "MUR-2026-001");
    assert_eq!(fact(&i, "UETR"), "e3b0c442-98fc-4c14-9afb-f4c8996fb924");
    assert_eq!(fact(&i, "Checksum"), "A1B2C3D4E5F6");
    assert_eq!(
        fact(&i, "Value date/currency/interbank settled amount"),
        "2026-11-03, EUR 12500.50 (as written: 261103EUR12500,50)"
    );
    let t = table(&i, "Fields");
    let row = t.rows.iter().position(|r| r.cells[0] == "32A").unwrap();
    assert_eq!(cell(t, row, "Value"), "261103EUR12500,50");
    assert_eq!(cell(t, row, "Interpreted"), "2026-11-03, EUR 12500.50");
    assert!(source_text(&path, t, row).starts_with(":32A:261103EUR12500,50"));
    let row = t.rows.iter().position(|r| r.cells[0] == "50K").unwrap();
    assert_eq!(cell(t, row, "Field"), "Ordering customer");
    assert_eq!(
        cell(t, row, "Value"),
        "/LU280019400644750000\nEXAMPLE RESEARCH S.A.\n1 RUE DE LA GARE\nL-1611 LUXEMBOURG"
    );
    let row = t.rows.iter().position(|r| r.cells[0] == "71A").unwrap();
    assert_eq!(cell(t, row, "Interpreted"), "Charges shared");
    assert_eq!(t.rows.len(), 10);
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    assert_eq!(check(&i, Stage::Schema, ""), State::NotDone);
}

#[test]
fn swift_mt_file_with_two_messages() {
    let i = inspect(&fixture("mt202-mt103-two-messages.fin"))
        .unwrap()
        .unwrap();
    assert_eq!(i.kind, "SWIFT MT messages (2)");
    let m = table(&i, "Messages");
    assert_eq!(m.rows.len(), 2);
    assert_eq!(cell(m, 0, "Type"), "MT202");
    // An output message: the sender is in the input reference, the receiver in block 1.
    assert_eq!(cell(m, 0, "Sender"), "BANKBEBBXXX");
    assert_eq!(cell(m, 0, "Receiver"), "BANKDEFFXXX");
    assert_eq!(cell(m, 1, "Amount (32A/32B)"), "2026-11-04, USD 980.00");
    let f = table(&i, "Fields");
    let row = f.rows.iter().position(|r| r.cells[1] == "58A").unwrap();
    assert_eq!(cell(f, row, "Field"), "Beneficiary institution");
    assert_eq!(cell(f, row, "Message"), "1");
    let groups: Vec<(&str, &str)> = i
        .facts
        .iter()
        .map(|f| (f.group.as_str(), f.label.as_str()))
        .collect();
    assert!(groups.contains(&("Message 1", "Possible duplicate emission")));
    assert!(
        i.facts
            .iter()
            .any(|f| f.group == "Message 2" && f.label == "Priority" && f.value == "U (urgent)")
    );
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
}

#[test]
fn mt940_two_statements_with_lines_information_and_balances() {
    let path = fixture("mt940-two-statements.sta");
    let i = inspect(&path).unwrap().unwrap();
    assert_eq!(i.family, "SWIFT MT");
    assert!(i.kind.starts_with("MT940"), "{}", i.kind);
    assert_eq!(fact(&i, "Statements"), "2");
    let s1: Vec<_> = i
        .facts
        .iter()
        .filter(|f| f.group == "Statement 1")
        .collect();
    assert!(
        s1.iter()
            .any(|f| f.label == "Account (25)" && f.value == "LU280019400644750000")
    );
    assert!(s1.iter().any(
        |f| f.label == "Opening balance (60F)" && f.value == "2026-10-29 EUR 10000.00 (credit)"
    ));
    assert!(
        s1.iter()
            .any(|f| f.label == "Statement number (28C)" && f.value == "statement 210, sequence 1")
    );
    assert!(
        i.facts
            .iter()
            .any(|f| f.group == "Statement 2" && f.value == "End of statement")
    );
    let t = table(&i, "Transactions");
    assert_eq!(t.rows.len(), 5);
    assert_eq!(cell(t, 0, "Value date"), "2026-10-30");
    assert_eq!(cell(t, 0, "Entry date"), "2026-10-30");
    assert_eq!(cell(t, 0, "Supplementary details"), "INVOICE PAYMENT");
    assert_eq!(
        cell(t, 0, "Information (86)"),
        "Client & Partners GmbH, invoice 2026-117"
    );
    assert_eq!(cell(t, 1, "D/C"), "D");
    assert_eq!(cell(t, 1, "Funds code"), "R");
    assert_eq!(cell(t, 1, "Amount"), "265.44");
    assert_eq!(cell(t, 1, "Currency"), "EUR");
    assert_eq!(cell(t, 1, "Type"), "NCHG (charges)");
    assert_eq!(cell(t, 1, "Customer reference"), "REF-OUT-1");
    assert_eq!(cell(t, 1, "Bank reference"), "B6103000124");
    assert_eq!(cell(t, 2, "Statement"), "2");
    assert_eq!(
        cell(t, 2, "Information (86)"),
        "Order 7781 — café et papier"
    );
    assert_eq!(cell(t, 3, "Information (86)"), "");
    assert_eq!(cell(t, 4, "D/C"), "RD");
    assert_eq!(cell(t, 4, "As written (61)"), "261031RD20,00NMSCNONREF");
    // A row covers its :61: and the :86: that follows it.
    let src = source_text(&path, t, 0);
    assert!(src.starts_with(":61:2610301030C1500,00"), "{src}");
    assert!(src.ends_with("invoice 2026-117"), "{src}");
    let st = table(&i, "Statements");
    assert_eq!(cell(st, 1, "Closing (computed)"), "10020.50");
    // 10000.00 + 1500.00 − 265.44 = 11234.56; 11234.56 − 1234.56 + 0.50 + 20.00 = 10020.50
    assert_eq!(check(&i, Stage::Semantic, "Closing balance"), State::Passed);
    assert!(detail(&i, Stage::Semantic, "Closing balance").contains("2 of 2"));
    assert_eq!(check(&i, Stage::Semantic, "Continuity"), State::Passed);
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    assert_eq!(check(&i, Stage::Schema, ""), State::NotDone);
}

#[test]
fn mt940_unbalanced_statement_fails_the_balance_rule() {
    let i = inspect(&fixture("mt940-unbalanced.sta")).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Semantic, "Closing balance"), State::Failed);
    let d = detail(&i, Stage::Semantic, "Closing balance");
    assert!(
        d.contains("= 11234.56") && d.contains("declared closing 11234.65"),
        "{d}"
    );
}

#[test]
fn mt942_interim_report_with_floor_limits_and_totals() {
    let i = inspect(&fixture("mt942-interim.fin")).unwrap().unwrap();
    assert_eq!(i.kind, "MT942 — Interim transaction report");
    assert_eq!(fact(&i, "Sender (BIC)"), "BANKLULLXXX");
    assert_eq!(fact(&i, "Receiver (BIC)"), "BANKBEBBXXX");
    assert_eq!(fact(&i, "Floor limit (34F)"), "EUR 0.00 (debit)");
    assert!(
        i.facts
            .iter()
            .any(|f| f.label == "Floor limit (34F)" && f.value == "EUR 100.00 (credit)")
    );
    assert_eq!(fact(&i, "Date/time (13D)"), "2026-10-31 15:30 UTC+01:00");
    let t = table(&i, "Transactions");
    assert_eq!(t.rows.len(), 3);
    assert_eq!(cell(t, 1, "Type"), "NDDT (direct debit)");
    assert_eq!(cell(t, 0, "Currency"), "EUR");
    assert_eq!(
        check(&i, Stage::Semantic, "Debit entries (90D)"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Credit entries (90C)"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "no statement has both"),
        State::NotDone
    );
}

#[test]
fn bai2_records_continuations_and_control_totals() {
    let path = fixture("bai2-two-accounts.bai");
    let i = inspect(&path).unwrap().unwrap();
    assert_eq!(i.family, "BAI2");
    assert_eq!(i.version.as_deref(), Some("BAI version 2"));
    assert_eq!(fact(&i, "Sender"), "122099999");
    assert_eq!(fact(&i, "Created"), "2026-10-31 22:00");
    assert_eq!(fact(&i, "As of"), "2026-10-31 end of day (2400)");
    assert_eq!(fact(&i, "Status"), "1 (update)");
    assert_eq!(fact(&i, "Accounts"), "2");
    assert_eq!(fact(&i, "Transactions"), "3");
    let b = table(&i, "Balances and summaries");
    assert_eq!(b.rows.len(), 6);
    assert_eq!(cell(b, 0, "Description"), "Opening ledger");
    assert_eq!(cell(b, 0, "Amount"), "10000.00");
    assert_eq!(cell(b, 2, "Description"), "Total credits");
    assert_eq!(cell(b, 2, "Item count"), "2");
    assert_eq!(cell(b, 4, "Account"), "0007654321");
    let t = table(&i, "Transactions");
    assert_eq!(
        cell(t, 0, "Text"),
        "INCOMING WIRE FROM CLIENT AND PARTNERS GMBH"
    );
    assert_eq!(cell(t, 0, "Description"), "Incoming money transfer");
    assert_eq!(cell(t, 0, "Credit/debit"), "Credit");
    assert_eq!(cell(t, 0, "Amount"), "1500.00");
    assert_eq!(cell(t, 0, "Funds type"), "0 (immediate availability)");
    assert_eq!(cell(t, 1, "Description"), "Preauthorized ACH credit");
    assert_eq!(cell(t, 2, "Description"), "Check paid");
    assert_eq!(cell(t, 2, "Credit/debit"), "Debit");
    assert_eq!(cell(t, 2, "Customer reference"), "1001");
    // The continued record covers both physical lines.
    let src = source_text(&path, t, 0);
    assert!(
        src.starts_with("16,195") && src.ends_with("88,PARTNERS GMBH"),
        "{src}"
    );
    for what in [
        "Account control totals",
        "Account record counts",
        "Group control totals",
        "Group account and record counts",
        "File control total",
        "File group and record counts",
    ] {
        assert_eq!(check(&i, Stage::Semantic, what), State::Passed, "{what}");
    }
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
}

#[test]
fn bai2_wrong_totals_and_missing_trailer() {
    let dir = temp_dir("bai2");
    let p = variant(
        &dir,
        "bai2-two-accounts.bai",
        "bad.bai",
        "49,2775000,6/",
        "49,2775100,7/",
    );
    let i = inspect(&p).unwrap().unwrap();
    assert_eq!(
        check(&i, Stage::Semantic, "Account control totals"),
        State::Failed
    );
    assert!(
        detail(&i, Stage::Semantic, "Account control totals")
            .contains("declared 27751.00, computed 27750.00")
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Account record counts"),
        State::Failed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Group control totals"),
        State::Failed
    );
    let p = variant(
        &dir,
        "bai2-two-accounts.bai",
        "cut.bai",
        "99,3775000,1,12/",
        "",
    );
    let i = inspect(&p).unwrap().unwrap();
    assert_eq!(
        check(&i, Stage::Parsed, "file trailer (99) is missing"),
        State::Partial
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fix_log_with_prefixes_and_pipes() {
    let path = fixture("fix44-session.log");
    let i = inspect(&path).unwrap().unwrap();
    assert_eq!(i.family, "FIX");
    assert_eq!(i.version.as_deref(), Some("FIX.4.4"));
    assert_eq!(i.kind, "FIX messages (4)");
    assert_eq!(fact(&i, "Messages"), "4");
    assert_eq!(fact(&i, "Other lines"), "2");
    assert!(fact(&i, "Delimiter").starts_with('|'));
    assert!(fact(&i, "Message types").contains("8 — ExecutionReport: 2"));
    assert!(fact(&i, "Sessions (49 → 56)").contains("CLIENT1 → BROKER1: 2"));
    let t = table(&i, "Messages");
    assert_eq!(t.rows.len(), 4);
    assert_eq!(cell(t, 0, "MsgType (35)"), "A — Logon");
    assert_eq!(cell(t, 1, "MsgType (35)"), "D — NewOrderSingle");
    assert_eq!(cell(t, 1, "Side (54)"), "1 — Buy");
    assert_eq!(cell(t, 1, "OrdType (40)"), "2 — Limit");
    assert_eq!(cell(t, 1, "Symbol (55)"), "ACME");
    assert_eq!(cell(t, 1, "Price (44)"), "12.34");
    assert_eq!(cell(t, 1, "Seq (34)"), "2");
    assert_eq!(
        cell(t, 1, "Log prefix"),
        "2026-11-01 09:00:01.251 INFO  [FIX/CLIENT1] OUT:"
    );
    assert_eq!(cell(t, 3, "ExecType (150)"), "F — Trade");
    assert_eq!(cell(t, 3, "OrdStatus (39)"), "2 — Filled");
    // "58=FIX…" inside a message does not start another one.
    assert_eq!(cell(t, 3, "Text (58)"), "FIX order done");
    assert!(source_text(&path, t, 1).starts_with("8=FIX.4.4|9="));
    assert!(source_text(&path, t, 1).ends_with('|') || source_text(&path, t, 1).contains("|10="));
    let f = table(&i, "Fields");
    let hb = f.rows.iter().position(|r| r.cells[1] == "108").unwrap();
    assert_eq!(cell(f, hb, "Name"), "HeartBtInt");
    let tif = f.rows.iter().position(|r| r.cells[1] == "59").unwrap();
    assert_eq!(cell(f, tif, "Meaning"), "Day");
    assert_eq!(check(&i, Stage::Semantic, "BodyLength"), State::Passed);
    assert!(detail(&i, Stage::Semantic, "BodyLength").contains("4 of 4"));
    assert_eq!(check(&i, Stage::Semantic, "CheckSum"), State::Passed);
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    assert_eq!(check(&i, Stage::Schema, ""), State::NotDone);
}

#[test]
fn fix_soh_capture_with_a_wrong_checksum() {
    let i = inspect(&fixture("fixt11-bad-checksum.fix"))
        .unwrap()
        .unwrap();
    assert_eq!(i.version.as_deref(), Some("FIXT.1.1"));
    assert_eq!(fact(&i, "Delimiter"), "SOH (0x01)");
    let t = table(&i, "Messages");
    assert_eq!(cell(t, 1, "Side (54)"), "2 — Sell");
    assert_eq!(check(&i, Stage::Semantic, "BodyLength"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "CheckSum"), State::Failed);
    assert!(detail(&i, Stage::Semantic, "CheckSum").contains("1 of 2 agree — message 2"));
}

#[test]
fn ofx1_sgml_bank_statement() {
    let path = fixture("ofx1-bank-statement.ofx");
    let i = inspect(&path).unwrap().unwrap();
    assert_eq!(i.family, "OFX");
    assert_eq!(i.version.as_deref(), Some("1.0.2 (SGML)"));
    assert_eq!(i.kind, "OFX — bank statement");
    assert_eq!(fact(&i, "DATA"), "OFXSGML");
    assert_eq!(fact(&i, "Status"), "0 (success)");
    assert_eq!(fact(&i, "Institution"), "Example Bank");
    assert_eq!(fact(&i, "Account"), "000123456789");
    assert_eq!(fact(&i, "Account type"), "CHECKING");
    assert_eq!(fact(&i, "Currency"), "USD");
    assert_eq!(fact(&i, "From"), "2026-10-01");
    assert_eq!(fact(&i, "Ledger balance"), "3207.83");
    assert_eq!(fact(&i, "Available balance"), "3107.83");
    assert_eq!(fact(&i, "Sum of transactions"), "1207.83");
    let t = table(&i, "Transactions");
    assert_eq!(t.rows.len(), 3);
    assert_eq!(cell(t, 0, "Type"), "CREDIT (generic credit)");
    assert_eq!(cell(t, 0, "Memo"), "Salary October");
    assert_eq!(cell(t, 1, "Date posted"), "2026-10-10 12:00:00");
    assert_eq!(cell(t, 1, "Amount"), "-250.00");
    assert_eq!(cell(t, 1, "Check number"), "1001");
    assert_eq!(cell(t, 1, "Name / payee"), "Landlord & Co");
    assert_eq!(cell(t, 1, "Currency"), "USD");
    let src = source_text(&path, t, 2);
    assert!(
        src.starts_with("<STMTTRN>") && src.ends_with("</STMTTRN>"),
        "{src}"
    );
    assert_eq!(check(&i, Stage::Semantic, "FITID unique"), State::Passed);
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    assert_eq!(check(&i, Stage::Schema, ""), State::NotDone);
}

#[test]
fn ofx2_xml_credit_card_statement_duplicates_and_truncation() {
    let path = fixture("ofx2-credit-card.qfx");
    let i = inspect(&path).unwrap().unwrap();
    assert_eq!(i.version.as_deref(), Some("2.2.0 (XML)"));
    assert_eq!(i.kind, "OFX — credit card statement");
    assert_eq!(fact(&i, "Account"), "4970XXXXXXXX1234");
    assert_eq!(fact(&i, "Intuit bank ID"), "30004");
    assert_eq!(fact(&i, "Ledger balance"), "-412.30");
    let t = table(&i, "Transactions");
    assert_eq!(cell(t, 0, "Memo"), "Livres & cartes");
    assert_eq!(cell(t, 1, "Name / payee"), "Hôtel de la Gare");
    assert_eq!(cell(t, 1, "Currency"), "EUR");
    assert_eq!(check(&i, Stage::Semantic, "FITID unique"), State::Passed);
    let dir = temp_dir("ofx");
    let p = variant(
        &dir,
        "ofx2-credit-card.qfx",
        "dup.qfx",
        "<FITID>CC-2<",
        "<FITID>CC-1<",
    );
    let i = inspect(&p).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Semantic, "FITID unique"), State::Failed);
    assert!(detail(&i, Stage::Semantic, "FITID unique").contains("repeated CC-1"));
    let text = std::fs::read_to_string(&path).unwrap();
    let cut = dir.join("cut.qfx");
    std::fs::write(&cut, &text[..text.find("<PAYEE>").unwrap()]).unwrap();
    let i = inspect(&cut).unwrap().unwrap();
    assert_eq!(fact(&i, "Account"), "4970XXXXXXXX1234");
    assert_eq!(check(&i, Stage::Parsed, "ends before"), State::Partial);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn qif_bank_transactions_splits_and_lists() {
    let path = fixture("qif-bank-with-splits.qif");
    let i = inspect(&path).unwrap().unwrap();
    assert_eq!(i.family, "QIF");
    assert_eq!(i.kind, "QIF — bank account (Bank)");
    assert_eq!(fact(&i, "Transactions"), "4");
    assert_eq!(fact(&i, "Accounts"), "1");
    assert_eq!(fact(&i, "First date (as written)"), "10/03/2026");
    assert_eq!(fact(&i, "Bank"), "1070.00");
    let t = table(&i, "Transactions");
    assert_eq!(t.rows.len(), 4);
    assert_eq!(cell(t, 0, "Account"), "Checking");
    assert_eq!(cell(t, 0, "Amount"), "1,500.00");
    assert_eq!(cell(t, 0, "Cleared"), "* (cleared)");
    assert_eq!(cell(t, 1, "Number"), "1001");
    assert_eq!(cell(t, 1, "Cleared"), "X (reconciled)");
    assert_eq!(cell(t, 3, "Address"), "12 Main Street, Springfield");
    assert_eq!(cell(t, 3, "Splits"), "2");
    let src = source_text(&path, t, 2);
    assert!(
        src.starts_with("D10/15/2026") && src.ends_with('^'),
        "{src}"
    );
    let s = table(&i, "Splits");
    assert_eq!(s.rows.len(), 4);
    assert_eq!(
        s.rows[1].cells,
        ["3", "Household", "Cleaning", "-40.00", ""]
    );
    let c = table(&i, "Categories");
    assert_eq!(c.rows[0].cells[2], "Expense");
    assert_eq!(c.rows[1].cells[2], "Income");
    assert_eq!(c.rows[1].cells[3], "yes");
    assert_eq!(
        table(&i, "Accounts").rows[0].cells[..2],
        ["Checking", "Bank"]
    );
    assert_eq!(check(&i, Stage::Semantic, "Split amounts"), State::Passed);
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    let dir = temp_dir("qif");
    let p = variant(
        &dir,
        "qif-bank-with-splits.qif",
        "bad.qif",
        "$-25.00",
        "$-20.00",
    );
    let i = inspect(&p).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Semantic, "Split amounts"), State::Failed);
    assert!(detail(&i, Stage::Semantic, "Split amounts").contains("amount -60.00, splits -55.00"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fec_entries_balances_and_identifiers_as_written() {
    let path = fixture("123456789FEC20251231.txt");
    let i = inspect(&path).unwrap().unwrap();
    assert_eq!(i.family, "FEC");
    assert_eq!(fact(&i, "SIREN (from the file name)"), "123456789");
    assert_eq!(fact(&i, "Closing date (from the file name)"), "2025-12-31");
    assert_eq!(fact(&i, "Separator"), "| (pipe)");
    assert_eq!(fact(&i, "Entries"), "4");
    assert_eq!(fact(&i, "Lines"), "10");
    assert_eq!(fact(&i, "Period (entry dates)"), "2025-01-01 to 2025-12-31");
    assert_eq!(fact(&i, "Total debit"), "11440.00");
    assert_eq!(fact(&i, "Total credit"), "11440.00");
    assert_eq!(fact(&i, "Currencies (Idevise)"), "USD");
    assert!(fact(&i, "Journals").contains("AC Achats (1)"));
    let t = table(&i, "Entries");
    assert_eq!(t.columns.len(), 18);
    assert_eq!(t.rows.len(), 10);
    assert_eq!(cell(t, 5, "EcritureNum"), "000123");
    assert_eq!(cell(t, 5, "CompAuxNum"), "0042DUP");
    assert_eq!(cell(t, 5, "EcritureLib"), "Règlement Dupont");
    assert_eq!(cell(t, 7, "Montantdevise"), "1000,00");
    assert_eq!(cell(t, 7, "Debit"), "1200,00");
    assert!(source_text(&path, t, 0).starts_with("AN|A nouveaux|AN000001|"));
    let j = table(&i, "Journals");
    assert_eq!(
        j.rows[0].cells,
        ["AC", "Achats", "1", "3", "120.00", "120.00"]
    );
    assert_eq!(
        check(&i, Stage::Semantic, "Entries balanced"),
        State::Passed
    );
    assert!(detail(&i, Stage::Semantic, "Entries balanced").contains("4 of 4"));
    assert_eq!(check(&i, Stage::Semantic, "File balanced"), State::Passed);
    assert_eq!(check(&i, Stage::Schema, ""), State::Partial);
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
}

#[test]
fn fec_unbalanced_tab_separated_latin9() {
    let i = inspect(&fixture("fec-unbalanced-latin9.txt"))
        .unwrap()
        .unwrap();
    assert_eq!(fact(&i, "Separator"), "tab");
    let t = table(&i, "Entries");
    assert_eq!(t.rows.len(), 4);
    assert_eq!(cell(t, 0, "JournalLib"), "Opérations diverses");
    assert_eq!(cell(t, 0, "EcritureLib"), "Note de frais 50 €");
    assert_eq!(
        check(&i, Stage::Semantic, "Entries balanced"),
        State::Failed
    );
    let d = detail(&i, Stage::Semantic, "Entries balanced");
    assert!(
        d.contains("1 of 2 agree — OD 1 (line 2): debit 50.00, credit 45.00"),
        "{d}"
    );
    assert_eq!(check(&i, Stage::Semantic, "File balanced"), State::Failed);
    // The cut line has 12 fields instead of 18: skipped and reported.
    assert_eq!(check(&i, Stage::Parsed, "line 6"), State::Partial);
    assert_eq!(fact(&i, "Lines"), "4");
}

#[test]
fn similar_looking_text_is_not_claimed() {
    let dir = temp_dir("plain");
    let cases: &[(&str, &[u8])] = &[
        (
            "notes.txt",
            b"Meeting at 10:20:30\n:20: is not a field here\n",
        ),
        ("numbers.csv", b"01,2,3\n02,5,6\n"),
        ("short.txt", b"01,a,b/\n02,c/\n"),
        (
            "journal.csv",
            b"JournalCode,JournalLib,EcritureNum\nAC,Achats,1\n",
        ),
        (
            "chat.log",
            b"user: we use 8=FIX in our tests\nanother line\n",
        ),
        ("readme.txt", b"This mentions OFX and <OFX> tags.\n"),
        ("tags.txt", b":20:REF\nthen prose\n"),
        ("braces.txt", b"{1:hello}{2:world}\n"),
    ];
    for (name, bytes) in cases {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        assert!(inspect(&p).is_none(), "{name} was claimed");
    }
    // The ISO 20022 statement is still taken by the XML standards.
    let i = inspect(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/standards/camt053.xml"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(i.family, "ISO 20022");
    let _ = std::fs::remove_dir_all(&dir);
}

const FIXTURES: &[&str] = &[
    "mt103.fin",
    "mt202-mt103-two-messages.fin",
    "mt940-two-statements.sta",
    "mt940-unbalanced.sta",
    "mt942-interim.fin",
    "bai2-two-accounts.bai",
    "fix44-session.log",
    "fixt11-bad-checksum.fix",
    "ofx1-bank-statement.ofx",
    "ofx2-credit-card.qfx",
    "qif-bank-with-splits.qif",
    "123456789FEC20251231.txt",
    "fec-unbalanced-latin9.txt",
];

#[test]
fn every_fixture_is_recognized_by_its_own_handler() {
    let expected = [
        "SWIFT MT", "SWIFT MT", "SWIFT MT", "SWIFT MT", "SWIFT MT", "BAI2", "FIX", "FIX", "OFX",
        "OFX", "QIF", "FEC", "FEC",
    ];
    for (name, family) in FIXTURES.iter().zip(expected) {
        let i = inspect(&fixture(name)).unwrap().unwrap();
        assert_eq!(i.family, family, "{name}");
        // Every row of every table points into the file.
        let size = std::fs::metadata(fixture(name)).unwrap().len();
        for t in &i.tables {
            for r in &t.rows {
                if let Some(s) = &r.source {
                    assert!(s.start <= s.end && s.end <= size, "{name} {}", t.name);
                }
            }
        }
    }
}

#[test]
fn truncated_fixtures_are_shown_in_part() {
    let dir = temp_dir("cut");
    let text = std::fs::read(fixture("mt103.fin")).unwrap();
    let p = dir.join("cut.fin");
    std::fs::write(&p, &text[..text.len() / 2]).unwrap();
    let i = inspect(&p).unwrap().unwrap();
    assert_eq!(fact(&i, "Sender (BIC)"), "BANKBEBBXXX");
    assert_eq!(check(&i, Stage::Parsed, "not closed"), State::Partial);
    for name in FIXTURES {
        let bytes = std::fs::read(fixture(name)).unwrap();
        for cut in [1, 7, bytes.len() / 3, bytes.len() / 2, bytes.len() - 1] {
            let p = dir.join(format!("{cut}-{name}"));
            std::fs::write(&p, &bytes[..cut.min(bytes.len())]).unwrap();
            let _ = inspect(&p);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn random_damage_and_hostile_inputs_neither_panic_nor_hang() {
    let dir = temp_dir("fuzz");
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for name in FIXTURES {
        let bytes = std::fs::read(fixture(name)).unwrap();
        for round in 0..60 {
            let mut b = bytes.clone();
            for _ in 0..1 + round % 8 {
                let r = next();
                let at = (r % b.len() as u64) as usize;
                match r >> 60 {
                    0 => {
                        b.remove(at);
                    }
                    1 => b.insert(at, b"{}:|^\n,/<>\x01"[(r >> 8) as usize % 11]),
                    _ => b[at] = (r >> 32) as u8,
                }
            }
            let p = dir.join(format!("{round}-{name}"));
            std::fs::write(&p, &b).unwrap();
            let started = std::time::Instant::now();
            let _ = inspect(&p);
            assert!(started.elapsed().as_secs() < 2, "{name} round {round}");
        }
    }
    let big = |unit: &str, n: usize| unit.repeat(n).into_bytes();
    let hostile: Vec<(&str, Vec<u8>)> = vec![
        (
            "braces.fin",
            [b"{1:F01".as_slice(), &big("{", 200_000)].concat(),
        ),
        (
            "headers.fin",
            big("{1:F01BANKBEBBAXXX0000000000}{2:I103", 20_000),
        ),
        (
            "block4.fin",
            [b"{1:F01X}{4:\n".as_slice(), &big(":20:A\n-x}\n", 50_000)].concat(),
        ),
        (
            "bare.sta",
            [
                b":20:A\n:25:B\n:60F:C261029EUR1,\n".as_slice(),
                &big(":61:261030C1,NTRF\n:86:x\n", 50_000),
            ]
            .concat(),
        ),
        ("fix.log", big("8=FIX.4.4|9=5|35=0|10=000|", 20_000)),
        ("fixbad.log", big("8=FIX.4.4|9=|35=|", 30_000)),
        ("fixnl.fix", big("8=FIX.4.2\x019=1\x0135=D\x01\n", 20_000)),
        (
            "deep.ofx",
            [b"OFXHEADER:100\n\n<OFX>".as_slice(), &big("<A>", 200_000)].concat(),
        ),
        (
            "ents.ofx",
            [
                b"OFXHEADER:100\n\n<OFX><X>".as_slice(),
                &big("&#99999999;&", 100_000),
            ]
            .concat(),
        ),
        (
            "bai.bai",
            [
                b"01,a,b,261031,2200,1,80,10,2/\n02,x/\n".as_slice(),
                &big("16,195,1,D,99999999999,1,2,3/\n88,x\n", 30_000),
            ]
            .concat(),
        ),
        (
            "cont.bai",
            [
                b"01,a,b,c,d,e,f,g,2/\n02,x/\n03,1,USD/\n".as_slice(),
                &big("88,1,2,3,4\n", 50_000),
            ]
            .concat(),
        ),
        (
            "qif.qif",
            [b"!Type:Bank\n".as_slice(), &big("S\nE\n$\n%\n", 50_000)].concat(),
        ),
        (
            "fec.txt",
            [
                b"JournalCode|JournalLib|EcritureNum\n".as_slice(),
                &big("a|b|c\n", 100_000),
                &big("|", 100_000),
            ]
            .concat(),
        ),
    ];
    for (name, bytes) in hostile {
        let p = dir.join(name);
        std::fs::write(&p, &bytes).unwrap();
        let started = std::time::Instant::now();
        let r = inspect(&p);
        assert!(r.is_some(), "{name} not recognized");
        assert!(
            started.elapsed().as_secs() < 5,
            "{name} took {:?}",
            started.elapsed()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
