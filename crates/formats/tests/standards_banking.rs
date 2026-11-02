//! ISO 20022 messages recognized on top of XML (`inspect`): the reference standard of the
//! banking family. Fixtures in tests/fixtures/standards are written by hand from the published
//! message definitions.
use isolinear_formats::{
    detect,
    inspect::{Inspection, Stage, State, inspect_file},
};
use std::path::{Path, PathBuf};

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/standards")
        .join(name)
}

pub fn inspect(path: &Path) -> Option<Result<Inspection, String>> {
    let head = std::fs::read(path).unwrap();
    let name = path.file_name().unwrap().to_string_lossy();
    let d = detect(&name, &head[..head.len().min(65536)]);
    inspect_file(path, d.format.id, &|| false)
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

#[test]
fn camt053_statement_with_balances_entries_and_a_balance_check() {
    let path = fixture("camt053.xml");
    let i = inspect(&path).unwrap().unwrap();
    assert_eq!(i.family, "ISO 20022");
    assert!(i.kind.starts_with("camt.053"), "{}", i.kind);
    assert!(i.version.as_deref().unwrap().contains("version 08"));
    assert_eq!(fact(&i, "IBAN"), "LU280019400644750000");
    assert_eq!(fact(&i, "Owner"), "Example Research S.A.");
    assert_eq!(fact(&i, "Entries"), "2");
    let entries = i.tables.iter().find(|t| t.name == "Entries").unwrap();
    assert_eq!(entries.rows.len(), 2);
    let col = |name: &str| entries.columns.iter().position(|c| c == name).unwrap();
    assert_eq!(
        entries.rows[0].cells[col("Counterparty")],
        "Client & Partners GmbH"
    );
    assert_eq!(entries.rows[1].cells[col("Credit/debit")], "DBIT");
    assert_eq!(
        entries.rows[1].cells[col("Remittance")],
        "Order 7781 — café et papier"
    );
    // Every row points at its element in the file.
    let text = std::fs::read_to_string(&path).unwrap();
    let src = entries.rows[1].source.clone().unwrap();
    assert!(text[src.start as usize..src.end as usize].starts_with("<Ntry>"));
    // 10000.00 + 1500.00 - 265.44 = 11234.56
    assert_eq!(check(&i, Stage::Semantic, "Closing balance"), State::Passed);
    assert_eq!(check(&i, Stage::Schema, ""), State::NotDone);
}

#[test]
fn pain001_compares_declared_counts_and_control_sum() {
    let i = inspect(&fixture("pain001.xml")).unwrap().unwrap();
    assert!(i.kind.starts_with("pain.001"));
    assert_eq!(fact(&i, "Requested execution date"), "2026-11-03");
    let t = &i.tables[0];
    assert_eq!(t.rows.len(), 2);
    assert_eq!(t.rows[1].cells[2], "250.00");
    // The second row has no agent of its own; the debtor comes from its payment information.
    assert_eq!(t.rows[1].cells[8], "Example Research S.A.");
    assert_eq!(
        check(&i, Stage::Semantic, "Number of transactions"),
        State::Passed
    );
    // Declared 1300.00, the transfers add up to 1250.00.
    assert_eq!(check(&i, Stage::Semantic, "Control sum"), State::Failed);
}

#[test]
fn pain002_pacs008_and_other_messages() {
    let i = inspect(&fixture("pain002.xml")).unwrap().unwrap();
    assert_eq!(fact(&i, "Group status"), "PART");
    let t = i
        .tables
        .iter()
        .find(|t| t.name == "Transaction statuses")
        .unwrap();
    assert_eq!(t.rows[1].cells[..4], ["E2E-2", "", "RJCT", "AC04"]);
    let i = inspect(&fixture("pacs008.xml")).unwrap().unwrap();
    assert!(i.kind.starts_with("pacs.008"));
    assert_eq!(fact(&i, "Method"), "CLRG");
    assert_eq!(
        check(&i, Stage::Semantic, "Total settlement amount"),
        State::Passed
    );
    let i = inspect(&fixture("camt056.xml")).unwrap().unwrap();
    assert!(i.kind.starts_with("camt.056"), "{}", i.kind);
    assert_eq!(fact(&i, "Message ID"), "CXL-1");
    assert_eq!(check(&i, Stage::Parsed, "Only the header"), State::Partial);
}

#[test]
fn damaged_and_unrelated_files() {
    let dir = std::env::temp_dir().join(format!("isolinear-standards-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Truncated ISO 20022: what was read is shown, marked as partial.
    let text = std::fs::read_to_string(fixture("camt053.xml")).unwrap();
    let cut = dir.join("cut.xml");
    std::fs::write(&cut, &text[..text.len() / 2]).unwrap();
    let i = inspect(&cut).unwrap().unwrap();
    assert_eq!(fact(&i, "IBAN"), "LU280019400644750000");
    assert_eq!(check(&i, Stage::Parsed, "ends before"), State::Partial);
    // An XML file of no known standard is not claimed.
    let plain = dir.join("plain.xml");
    std::fs::write(&plain, "<note><to>Ann</to></note>").unwrap();
    assert!(inspect(&plain).is_none());
    // Random damage to every fixture: recognized or not, no panic, quick.
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for name in ["camt053.xml", "pain001.xml", "pain002.xml", "pacs008.xml"] {
        let bytes = std::fs::read(fixture(name)).unwrap();
        for round in 0..50 {
            let mut b = bytes.clone();
            for _ in 0..5 {
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
            assert!(started.elapsed().as_secs() < 2);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
