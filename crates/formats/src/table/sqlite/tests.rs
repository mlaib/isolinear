use super::*;
use std::io::Cursor;

const SHOP: &[u8] = include_bytes!("../../../../../tests/fixtures/sqlite-xlsx/shop.sqlite");
const UTF16: &[u8] = include_bytes!("../../../../../tests/fixtures/sqlite-xlsx/utf16.sqlite");
const WAL: &[u8] = include_bytes!("../../../../../tests/fixtures/sqlite-xlsx/wal.sqlite");
fn source(bytes: &[u8], name: Option<&str>) -> Result<Sqlite, String> {
    Sqlite::new(Db::new(Cursor::new(bytes.to_vec()))?, name)
}
fn some(v: &str) -> Option<String> {
    Some(v.into())
}

#[test]
fn lists_tables_views_types_and_header_facts() {
    let s = source(SHOP, None).unwrap();
    assert_eq!(s.schema.table, 0);
    assert_eq!(s.schema.rows, Some(600));
    assert!(s.runs.len() > 10);
    let tables: Vec<_> = s
        .schema
        .tables
        .iter()
        .map(|t| (t.name.as_str(), t.rows))
        .collect();
    assert_eq!(
        tables,
        [
            ("customers", Some(600)),
            ("order items", Some(450)),
            ("notes", Some(3)),
            ("copy", Some(3)),
            ("big_spenders", None),
            ("sqlite_schema", Some(6))
        ]
    );
    let cols: Vec<_> = s
        .schema
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.type_name.as_str()))
        .collect();
    assert_eq!(
        cols,
        [
            ("id", "INTEGER (rowid)"),
            ("name", "TEXT"),
            ("e-mail", "VARCHAR(80)"),
            ("score", "REAL"),
            ("photo", "BLOB")
        ]
    );
    assert!(
        s.schema
            .text
            .as_ref()
            .unwrap()
            .contains("CREATE TABLE customers")
    );
    assert!(s.schema.text.as_ref().unwrap().contains("CREATE VIEW"));
    for key in [
        "Page size",
        "Schema version",
        "User version",
        "Application id",
        "Text encoding",
    ] {
        assert!(s.schema.details.iter().any(|(k, _)| k == key));
    }
    assert!(
        s.schema
            .details
            .contains(&("Page size".into(), "512 bytes".into()))
    );
}

#[test]
fn rowid_real_affinity_paging_and_without_rowid() {
    let mut s = source(SHOP, None).unwrap();
    assert_eq!(
        s.rows(6, 1).unwrap()[0],
        [
            some("7"),
            some("Customer 7 – ünïcode"),
            None,
            some("0.875"),
            some("x'0700ff'")
        ]
    );
    assert_eq!(s.rows(7, 1).unwrap()[0][3], some("1.0"));
    let r = s.rows(499, 1).unwrap();
    assert_eq!(r[0][0], some("500"));
    assert_eq!(r[0][2], some("c500@example.org"));
    assert_eq!(r[0][3], some("62.5"));
    assert_eq!(s.rows(590, 50).unwrap().len(), 10);
    assert!(s.rows(u64::MAX, usize::MAX).unwrap().is_empty());
    assert!(s.rows(0, 0).unwrap().is_empty());
    let mut s = source(SHOP, Some("order items")).unwrap();
    let names: Vec<_> = s.schema.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["order_id", "line", "sku", "qty", "total"]);
    let rows = s.rows(0, 450).unwrap();
    assert_eq!(rows.len(), 450);
    assert_eq!(
        rows[1],
        [some("1"), some("2"), some("SKU-1-2"), some("2"), None]
    );
    assert_eq!(rows[449][2], some("SKU-150-3"));
}

#[test]
fn overflow_null_added_columns_and_create_as_select() {
    let mut s = source(SHOP, Some("notes")).unwrap();
    let rows = s.rows(0, 10).unwrap();
    assert!(rows[0][0].as_ref().unwrap().contains("3003 characters"));
    assert_eq!(rows[0][1], None);
    assert_eq!(rows[1], [None, None]);
    assert_eq!(rows[2], [some("short"), some("new column")]);
    let run = s.runs[0];
    let page = BTreePage::read(&mut s.db, run.page).unwrap();
    let (_, payload) = page.payload(&mut s.db, run.first).unwrap();
    let values = record(&s.db, &payload).unwrap();
    let Value::Text(text) = &values[0] else {
        panic!("expected text")
    };
    assert_eq!(text.len(), 3003);
    assert!(text.ends_with("END"));
    let mut s = source(SHOP, Some("copy")).unwrap();
    assert_eq!(s.schema.columns[0].type_name, "INT");
    assert_eq!(
        s.rows(2, 1).unwrap()[0],
        [some("3"), some("Customer 3 – ünïcode")]
    );
}

#[test]
fn utf16_and_large_blob_display() {
    assert_eq!(
        source(UTF16, None).unwrap().rows(0, 10).unwrap(),
        [
            [some("héllo"), some("1")],
            [some("日本語"), some("2")],
            [some("😀"), some("-9000000000")]
        ]
    );
    let shown = show(&Value::Blob(vec![0xab; 1024])).unwrap();
    assert!(shown.starts_with("x'abab"));
    assert!(shown.ends_with("(1024 bytes)"));
    assert!(shown.len() < 300);
}

#[test]
fn never_executes_views_or_reads_sidecars() {
    assert!(
        source(SHOP, Some("big_spenders"))
            .err()
            .unwrap()
            .contains("never runs SQL")
    );
    assert!(source(SHOP, Some("missing")).is_err());
    let mut s = source(WAL, None).unwrap();
    assert_eq!(s.schema.rows, Some(3));
    assert_eq!(s.rows(0, 100).unwrap().len(), 3);
    assert!(
        s.schema
            .details
            .iter()
            .any(|(k, v)| k == "Warning" && v.contains("write-ahead log"))
    );
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sqlite-xlsx/shop.sqlite");
    let before = std::fs::read(&path).unwrap();
    let mut reader = open(&path, None).unwrap();
    assert_eq!(reader.rows(0, 1).unwrap().len(), 1);
    assert_eq!(before, std::fs::read(&path).unwrap());
    for suffix in ["-shm", "-wal", "-journal"] {
        assert!(!Path::new(&format!("{}{suffix}", path.display())).exists());
    }
}

#[test]
fn corruption_in_btrees_records_and_overflow_is_reported() {
    for length in [0, 1, 99, 100, 511, 1024, 2048] {
        assert!(source(&SHOP[..length], None).is_err());
    }
    let mut bytes = SHOP.to_vec();
    bytes[512 + 8..512 + 12].copy_from_slice(&2u32.to_be_bytes());
    assert!(source(&bytes, None).err().unwrap().contains("loops"));
    bytes = SHOP.to_vec();
    bytes[16..18].copy_from_slice(&1000u16.to_be_bytes());
    assert!(source(&bytes, None).is_err());
    let db = Db::new(Cursor::new(SHOP.to_vec())).unwrap();
    for bad in [&[0u8][..], &[2, 255, 255][..], &[2, 10][..], &[255; 20][..]] {
        assert!(record(&db, bad).is_err());
    }
    // Locate an actual overflow pointer, then make its chain loop on the first page.
    let mut s = source(SHOP, Some("notes")).unwrap();
    let run = s.runs[0];
    let page = BTreePage::read(&mut s.db, run.page).unwrap();
    let mut at = page.cell_offset(run.first).unwrap();
    let size = varint(&page.data, &mut at).unwrap();
    varint(&page.data, &mut at).unwrap();
    let u = s.db.usable;
    let m = (u - 12) * 32 / 255 - 23;
    let k = m + (size - m) % (u - 4);
    let local = if k <= u - 35 { k } else { m } as usize;
    let overflow = u32::from_be_bytes(page.data[at + local..at + local + 4].try_into().unwrap());
    let mut bytes = SHOP.to_vec();
    let offset = (overflow as usize - 1) * 512;
    bytes[offset..offset + 4].copy_from_slice(&overflow.to_be_bytes());
    let mut s = source(&bytes, Some("notes")).unwrap();
    assert!(s.rows(0, 1).err().unwrap().contains("overflow chain loops"));
}

#[test]
fn schema_parser_preserves_declared_ordering_and_computed_columns() {
    let (c, without) =
        columns("CREATE TABLE x(v TEXT, a INTEGER, b INTEGER, PRIMARY KEY(a,b)) WITHOUT ROWID");
    assert!(without);
    let names: Vec<_> = c.unwrap().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["a", "b", "v"]);
    let (c, _) = columns(
        "CREATE TABLE x(a INTEGER PRIMARY KEY DESC, b REAL, c AS (b*2), d AS (b*3) STORED)",
    );
    let c = c.unwrap();
    assert!(!c[0].rowid);
    assert!(!c[2].stored);
    assert!(c[3].stored);
    assert!(columns("CREATE TABLE t AS SELECT 1").0.is_none());
}

#[test]
fn twenty_thousand_mutated_databases_never_panic() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..20_000 {
        // Mostly small databases, plus deep b-trees and overflow records.
        let original = if round % 20 == 0 { SHOP } else { UTF16 };
        let mut bytes = original.to_vec();
        for _ in 0..next() % 8 + 1 {
            let i = next() as usize % bytes.len();
            bytes[i] = next() as u8;
        }
        if next() % 5 == 0 {
            bytes.truncate(next() as usize % bytes.len());
        }
        if let Ok(mut s) = source(&bytes, None) {
            let _ = s.rows(0, 10);
            let _ = s.rows(s.schema.rows.unwrap_or(1).saturating_sub(1), 1);
        }
    }
}
