use super::*;
use std::io::Cursor;

const OPENPYXL: &[u8] = include_bytes!("../../../../../tests/fixtures/sqlite-xlsx/openpyxl.xlsx");
const LIBREOFFICE: &[u8] =
    include_bytes!("../../../../../tests/fixtures/sqlite-xlsx/libreoffice.xlsx");
const MAC: &[u8] = include_bytes!("../../../../../tests/fixtures/sqlite-xlsx/mac1904.xlsx");
fn source(bytes: &[u8], name: Option<&str>) -> Result<Xlsx, String> {
    Xlsx::new(Cursor::new(bytes), name)
}
fn s(v: &str) -> Option<String> {
    Some(v.into())
}
fn format_number(value: &str, code: &str, date1904: bool) -> String {
    let book = Book {
        sheets: vec![],
        strings: vec![],
        styles: vec![classify(code)],
        date1904,
        active: 0,
        facts: vec![],
        warnings: vec![],
    };
    display(&book, "n", 0, value, "").0
}

/// A minimal stored ZIP with real CRCs.
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut out, mut central) = (vec![], vec![]);
    for (name, data) in entries {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let header = |sig: &[u8], central: bool, offset: u32| {
            let mut h = sig.to_vec();
            if central {
                h.extend(20u16.to_le_bytes());
            }
            h.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            h.extend(crc.sum().to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes());
            if central {
                h.extend([0u8; 10]);
                h.extend(offset.to_le_bytes());
            }
            h.extend(name.as_bytes());
            h
        };
        central.extend(header(b"PK\x01\x02", true, out.len() as u32));
        out.extend(header(b"PK\x03\x04", false, 0));
        out.extend(*data);
    }
    let at = out.len() as u32;
    out.extend(&central);
    out.extend(b"PK\x05\x06\0\0\0\0");
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(at.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

const NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

fn workbook(sheet_xml: &str, strings: Option<&str>) -> Vec<u8> {
    let book = format!(
        r#"<workbook {NS}><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#;
    let sheet = format!(r#"<worksheet {NS}><sheetData>{sheet_xml}</sheetData></worksheet>"#);
    let sst = strings.map(|s| format!(r#"<sst {NS}>{s}</sst>"#));
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("xl/workbook.xml", book.as_bytes()),
        ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ];
    if let Some(sst) = &sst {
        entries.push(("xl/sharedStrings.xml", sst.as_bytes()));
    }
    zip(&entries)
}

#[test]
fn numbers_show_as_excel_formats_them() {
    assert_eq!(general(0.1 + 0.2), "0.3");
    assert_eq!(general(1.0 / 3.0), "0.333333333333333");
    assert_eq!(general(-42.0), "-42");
    assert_eq!(general(1e20), "1E+20");
    assert_eq!(general(1.5e-12), "1.5E-12");
    assert_eq!(general(123456.789), "123456.789");
    let f = |v: &str, code: &str| format_number(v, code, false);
    assert_eq!(
        f("60", "yyyy-mm-dd"),
        "1900-02-29",
        "Lotus's leap day is kept"
    );
    assert_eq!(f("59", "yyyy-mm-dd"), "1900-02-28");
    assert_eq!(f("45960", "dd/mm/yyyy"), "2025-10-30");
    assert_eq!(f("45960.75", "yyyy-mm-dd"), "2025-10-30 18:00:00");
    assert_eq!(f("0.5", "h:mm:ss AM/PM"), "12:00:00");
    assert_eq!(f("1.5", "[h]:mm"), "36:00:00", "elapsed time");
    assert_eq!(f("0.9999999999", "hh:mm:ss"), "24:00:00");
    assert_eq!(f("0.123", "0.0%"), "12.3%");
    assert_eq!(f("12", "\"Day \"0"), "12", "quoted text is not a date code");
    assert_eq!(f("12", "[Red]0.00;[Blue]-0.00"), "12");
    assert_eq!(f("12", "General"), "12");
    assert_eq!(
        f("-5", "yyyy-mm-dd"),
        "-5",
        "negative serials are not dates"
    );
    assert_eq!(f("not a number", "0"), "not a number");
    assert_eq!(format_number("0", "yyyy-mm-dd", true), "1904-01-01");
    for (i, name) in [
        (0, "A"),
        (25, "Z"),
        (26, "AA"),
        (701, "ZZ"),
        (702, "AAA"),
        (16383, "XFD"),
    ] {
        assert_eq!(column_name(i), name);
    }
}

#[test]
fn workbooks_from_two_producers_read_their_cells() {
    for (bytes, doubled) in [
        (OPENPYXL, [s(""), s(""), s("")]),
        (LIBREOFFICE, [s("2"), s("0.6"), s("246")]),
    ] {
        let mut book = source(bytes, None).unwrap();
        assert_eq!(book.schema.tables[book.schema.table].name, "Data");
        assert_eq!(book.schema.rows, Some(20));
        let fields: Vec<_> = book
            .schema
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.type_name.as_str()))
            .collect();
        assert_eq!(
            fields[..5],
            [
                ("A", "text"),
                ("B", "mixed"),
                ("C", "date"),
                ("D", "mixed"),
                ("E", "boolean")
            ]
        );
        assert!(
            book.schema
                .details
                .contains(&("Title".into(), "Workbook sample".into()))
        );
        assert!(
            book.schema
                .details
                .contains(&("Date system".into(), "1900".into()))
        );
        assert!(
            book.schema
                .details
                .iter()
                .any(|(k, v)| k == "Formulas" && v.contains("nothing is recalculated"))
        );
        let all = book.rows(0, 30).unwrap();
        assert_eq!(all.len(), 20);
        assert_eq!(
            all[0],
            [
                s("Name"),
                s("Amount"),
                s("When"),
                s("Ratio"),
                s("Flag"),
                s("Double")
            ]
        );
        assert_eq!(
            all[1],
            [
                s("Ünïcode – 日本語"),
                s("1"),
                s("2026-10-30 14:30:00"),
                s("25%"),
                s("TRUE"),
                doubled[0].clone()
            ]
        );
        assert_eq!(
            all[2][..5],
            [s("plain"), s("0.3"), s("1900-01-01"), s("150%"), s("FALSE")]
        );
        assert_eq!(all[2][5], doubled[1]);
        assert_eq!(
            all[3][..4],
            [s("leading zeros"), s("00123"), s("1900-03-01"), s("0.05%")]
        );
        assert_eq!(all[3][5], doubled[2]);
        assert_eq!(all[4], vec![s(""); 6]);
        assert_eq!(all[5][2], s("09:05:07"));
        assert_eq!(all[6][1], s("1.23456789012346E+19"));
        assert_eq!(all[7][1], s("#N/A"));
        assert_eq!(all[9][..2], [s("merged"), s("")]);
        assert_eq!(all[19][3], s("far away"));
        assert_eq!(book.rows(19, 5).unwrap(), [all[19].clone()]);
        assert!(book.rows(u64::MAX, usize::MAX).unwrap().is_empty());
        assert!(book.rows(0, 0).unwrap().is_empty());
        assert_eq!(
            source(bytes, Some("Hidden")).unwrap().rows(0, 10).unwrap(),
            [[s("secret")]]
        );
        let empty = source(bytes, Some("Empty")).unwrap();
        assert_eq!(
            (empty.schema.rows, empty.schema.columns.len()),
            (Some(0), 0)
        );
        assert!(source(bytes, Some("Missing")).is_err());
    }
    let e = source(OPENPYXL, Some("Chart")).err().unwrap();
    assert!(e.contains("chart sheet"), "{e}");
    assert_eq!(
        source(MAC, None).unwrap().rows(0, 5).unwrap(),
        [[s("2026-10-30")]]
    );
}

#[test]
fn odd_references_rich_strings_and_missing_indices() {
    let bytes = workbook(
        concat!(
            r#"<row r="1"><c r="XFE1"><v>1</v></c><c t="inlineStr"><is><t>inline</t><rPh><t>ふりがな</t></rPh></is></c><c><v>2</v></c></row>"#,
            r#"<row><c t="s"><v>0</v></c><c t="s"><v>99</v></c><c t="e"><v>#DIV/0!</v></c></row>"#,
            r#"<row r="1048576"><c r="C1048576"><v>3</v></c></row>"#,
        ),
        Some(r#"<si><r><t>rich </t></r><r><t>text</t></r><rPh><t>x</t></rPh></si>"#),
    );
    let mut b = source(&bytes, None).unwrap();
    assert_eq!(b.schema.rows, Some(1_048_576));
    assert_eq!(
        b.rows(0, 2).unwrap(),
        [
            [s("inline"), s("2"), s("")],
            [
                s("rich text"),
                s("[missing shared string 99]"),
                s("#DIV/0!")
            ]
        ]
    );
    assert_eq!(b.rows(1_048_575, 10).unwrap(), [[s(""), s(""), s("3")]]);
    assert!(b.rows(0, usize::MAX).err().unwrap().contains("1,000,000"));
}

#[test]
fn malformed_xml_keeps_completed_rows_with_a_warning() {
    let rows: String = (1..=200)
        .map(|r| format!(r#"<row r="{r}"><c r="A{r}"><v>{r}</v></c></row>"#))
        .collect();
    let full = workbook(&rows, None);
    assert_eq!(source(&full, None).unwrap().schema.rows, Some(200));
    let mut file = Cursor::new(full);
    let listing = super::zip::list_zip(&mut file, &|| false).unwrap();
    let mut parts = Vec::new();
    for entry in &listing.entries {
        let mut data = Vec::new();
        super::zip::extract_zip(&mut file, entry, &mut data).unwrap();
        if entry.name.contains("sheet1.xml") {
            data.truncate(data.len() / 2);
        }
        parts.push((entry.name.clone(), data));
    }
    let entries: Vec<_> = parts
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    let bytes = zip(&entries);
    let mut b = source(&bytes, None).unwrap();
    let n = b.schema.rows.unwrap();
    assert!(n > 50 && n < 200);
    assert!(
        b.schema
            .details
            .iter()
            .any(|(k, v)| k == "Warning" && v.contains("Only the first"))
    );
    assert_eq!(b.rows(0, 10).unwrap().len(), 10);
}

#[test]
fn macros_links_and_formulas_are_inert_and_merged_cells_stay_sparse() {
    let book = format!(r#"<workbook {NS}><sheets><sheet name="S" r:id="s"/></sheets></workbook>"#);
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="s" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/s.xml"/><Relationship Id="evil" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" TargetMode="External" Target="file:///definitely-not-a-workbook"/></Relationships>"#;
    let sheet = format!(
        r#"<worksheet {NS}><sheetData><row><c><f>1/0</f><v>42</v></c><c><f>UNSUPPORTED()</f></c><c t="e"><v>#DIV/0!</v></c></row></sheetData><mergeCells><mergeCell ref="A1:B1"/></mergeCells></worksheet>"#
    );
    let bytes = zip(&[
        ("xl/workbook.xml", book.as_bytes()),
        ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
        ("xl/worksheets/s.xml", sheet.as_bytes()),
        ("xl/vbaProject.bin", b"inert"),
        ("xl/externalLinks/externalLink1.xml", b"inert"),
    ]);
    let mut b = source(&bytes, None).unwrap();
    assert_eq!(b.rows(0, 1).unwrap(), [[s("42"), s(""), s("#DIV/0!")]]);
    assert!(
        b.schema
            .details
            .iter()
            .any(|(_, v)| v.contains("Contains macros"))
    );
    assert!(
        b.schema
            .details
            .iter()
            .any(|(_, v)| v.contains("never opened"))
    );
    assert!(
        b.schema
            .details
            .iter()
            .any(|(k, _)| k == "Merged cell ranges")
    );
    assert!(!b.schema.text.as_ref().unwrap().contains("UNSUPPORTED"));
}

#[test]
fn opening_indexes_metadata_without_retaining_display_rows() {
    let bytes = workbook(
        r#"<row><c><v>1</v></c></row><row r="999"><c><v>2</v></c></row>"#,
        None,
    );
    let b = source(&bytes, None).unwrap();
    let meta = read_sheet(&b.xml, &b.book, None, &|| false).unwrap();
    assert!(meta.rows.is_empty());
    assert_eq!(meta.height, 999);
    let page = read_sheet(&b.xml, &b.book, Some(998..999), &|| false).unwrap();
    assert_eq!(page.rows, [(998, vec![(0, "2".into())])]);
}

#[test]
fn xml_depth_entities_and_cancellation_are_bounded() {
    let deep = format!("{}{}", "<a>".repeat(257), "</a>".repeat(257));
    assert!(
        office::walk(deep.as_bytes(), &|| false, |_| Ok(()))
            .err()
            .unwrap()
            .contains("256")
    );
    assert!(
        office::walk(b"<a/>", &|| true, |_| Ok(()))
            .err()
            .unwrap()
            .contains("Cancelled")
    );
    assert!(
        office::walk(
            b"<!DOCTYPE a [<!ENTITY x 'abc'>]><a>&x;</a>",
            &|| false,
            |_| Ok(())
        )
        .is_err()
    );
    assert!(office::walk(b"<a x='1' x='2'/>", &|| false, |_| Ok(())).is_err());
    assert!(office::walk(b"<a>", &|| false, |_| Ok(())).is_err());
    let strict = workbook(
        r#"<row><c t="inlineStr"><is><t>&amp;&lt;&#233;</t></is></c></row>"#,
        None,
    );
    assert_eq!(
        source(&strict, None).unwrap().rows(0, 1).unwrap(),
        [[s("&<é")]]
    );
}

#[test]
fn zip_truncation_offsets_crc_lengths_and_encryption_are_checked() {
    let good = workbook(r#"<row><c><v>1</v></c></row>"#, None);
    for length in [0, 4, 30, good.len() - 1] {
        assert!(source(&good[..length], None).is_err());
    }
    let central = good.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    for (offset, bytes) in [
        (central + 42, vec![255; 4]),
        (central + 24, vec![255; 4]),
        (central + 8, vec![1, 0]),
        (central + 16, vec![0; 4]),
        (central + 28, vec![255, 255]),
    ] {
        let mut bad = good.clone();
        bad[offset..offset + bytes.len()].copy_from_slice(&bytes);
        assert!(source(&bad, None).is_err(), "offset {offset}");
    }
}

#[test]
fn twenty_thousand_mutated_workbook_parts_never_panic() {
    let mut seed = 0x51_7cc1_b727_220au64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let original = workbook(
        r#"<row><c t="s"><v>0</v></c><c><f>1+1</f><v>2</v></c></row><row r="20"><c t="inlineStr"><is><t>é日😀</t></is></c></row>"#,
        Some("<si><t>text</t></si>"),
    );
    let mut file = Cursor::new(&original);
    let listing = super::zip::list_zip(&mut file, &|| false).unwrap();
    let parts: Vec<_> = listing
        .entries
        .iter()
        .map(|e| {
            let mut data = Vec::new();
            super::zip::extract_zip(&mut file, e, &mut data).unwrap();
            (e.name.clone(), data)
        })
        .collect();
    for round in 0..20_000 {
        let mut parts = parts.clone();
        let i = next() as usize % parts.len();
        let data = &mut parts[i].1;
        for _ in 0..next() % 5 + 1 {
            let at = next() as usize % data.len();
            data[at] = next() as u8;
        }
        if next() % 5 == 0 {
            data.truncate(next() as usize % data.len());
        }
        let entries: Vec<_> = parts
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect();
        let mut bytes = zip(&entries);
        if round % 2 == 0 {
            let at = next() as usize % bytes.len();
            bytes[at] = next() as u8;
        }
        if let Ok(mut b) = source(&bytes, None) {
            let _ = b.rows(0, 2);
            let _ = b.rows(b.schema.rows.unwrap_or(1).saturating_sub(1), 1);
        }
    }
}

#[test]
fn shared_strings_and_sheet_cell_budgets_report_truncation() {
    let xml = format!(r#"<sst {NS}><si/><si/><si/></sst>"#);
    assert!(
        shared_strings_with_limit(xml.as_bytes(), &|| false, 2)
            .err()
            .unwrap()
            .contains("shared strings")
    );
    let bytes = workbook(
        r#"<row><c><v>1</v></c></row><row><c><v>22</v></c></row><row><c><v>333</v></c></row>"#,
        None,
    );
    let b = source(&bytes, None).unwrap();
    for (cells, bytes) in [(2, 100), (100, 3)] {
        let sheet =
            read_sheet_with_budget(&b.xml, &b.book, Some(0..3), &|| false, cells, bytes).unwrap();
        assert!(sheet.truncated);
        assert_eq!(sheet.height, 2);
        assert_eq!(sheet.rows.len(), 2);
    }
}
