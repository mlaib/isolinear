use super::*;
use columnar::tests::*;
use parquet::basic::Compression;

#[test]
fn summary_and_pages_preserve_source() {
    let temp = Temp::new("parquet");
    write_parquet(&temp.path, &simple(0, 1000), 100, Compression::UNCOMPRESSED);
    let before = std::fs::read(&temp.path).unwrap();
    let mut s = Parquet::open(&temp.path, Limits::default()).unwrap();
    assert_eq!(s.schema.rows, Some(1000));
    assert_eq!(s.schema.columns[1].type_name, "utf8");
    assert!(
        s.schema
            .details
            .contains(&("Row groups".into(), "10".into()))
    );
    assert!(
        s.schema
            .details
            .iter()
            .any(|(k, v)| k == "note" && v.contains("2500 characters"))
    );
    assert!(s.schema.text.as_ref().unwrap().contains("message"));
    assert_eq!(
        s.rows(0, 2).unwrap(),
        vec![
            vec![Some("0".into()), None],
            vec![Some("1".into()), Some("NULL".into())]
        ]
    );
    let page = s.rows(195, 10).unwrap();
    assert_eq!(page.len(), 10);
    assert_eq!(page[0], [Some("195".into()), Some("café 195".into())]);
    assert_eq!(page[9][0].as_deref(), Some("204"));
    assert_eq!(s.rows(995, 100).unwrap().len(), 5);
    assert!(s.rows(1000, 1).unwrap().is_empty());
    assert!(s.rows(u64::MAX, 1).unwrap().is_empty());
    assert!(s.rows(0, 0).unwrap().is_empty());
    assert_eq!(std::fs::read(&temp.path).unwrap(), before);
}

#[test]
fn nested_values_and_all_enabled_codecs() {
    for codec in [
        Compression::UNCOMPRESSED,
        Compression::SNAPPY,
        Compression::GZIP(Default::default()),
        Compression::LZ4_RAW,
        Compression::ZSTD(Default::default()),
    ] {
        let t = Temp::new("parquet");
        write_parquet(&t.path, &varied(), 100, codec);
        let mut s = Parquet::open(&t.path, Limits::default()).unwrap();
        let page = s.rows(0, 1).unwrap();
        assert_eq!(page[0][0].as_deref(), Some("[1, null, 3]"));
        assert_eq!(page[0][3].as_deref(), Some("123.45"));
        assert!(page[0][4].as_ref().unwrap().ends_with("(80 bytes)"));
        assert!(page[0][5].as_ref().unwrap().contains("5000 characters"));
        assert!(
            s.schema
                .details
                .iter()
                .any(|(k, v)| k.starts_with("Compression:") && v.contains(&format!("{codec:?}")))
        );
    }
}

#[test]
fn limits_damage_and_encryption_are_refused() {
    let t = Temp::new("parquet");
    write_parquet(&t.path, &simple(0, 10), 5, Compression::UNCOMPRESSED);
    let good = std::fs::read(&t.path).unwrap();
    for limits in [
        Limits {
            metadata: 50,
            ..Limits::default()
        },
        Limits {
            columns: 1,
            ..Limits::default()
        },
        Limits {
            batches: 1,
            ..Limits::default()
        },
    ] {
        assert!(Parquet::open(&t.path, limits).is_err());
    }
    let mut s = Parquet::open(
        &t.path,
        Limits {
            decoded: 8,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(s.rows(0, 1).is_err());
    assert!(s.rows(0, usize::MAX).is_err());
    for bytes in [
        b"PAR1bad!PAR1".as_slice(),
        b"PAR1\0\0\0\0\x04\0\0\0PARE",
        &good[..good.len() - 10],
        b"not parquet",
    ] {
        t.write(bytes);
        assert!(open(&t.path, None).is_err());
    }
    let mut changed = good.clone();
    let at = changed.len() - 8;
    changed[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    t.write(&changed);
    assert!(open(&t.path, None).is_err());
}

#[test]
fn pages_do_not_decode_unrequested_row_groups() {
    let t = Temp::new("parquet");
    write_parquet(&t.path, &simple(0, 20), 10, Compression::UNCOMPRESSED);
    let s = Parquet::open(&t.path, Limits::default()).unwrap();
    let (offset, len) = s.metadata.metadata().row_groups()[0].columns()[0].byte_range();
    drop(s);
    let mut bytes = std::fs::read(&t.path).unwrap();
    bytes[offset as usize..(offset + len) as usize].fill(0xff);
    t.write(&bytes);
    let mut s = Parquet::open(&t.path, Limits::default()).unwrap();
    assert_eq!(s.rows(15, 1).unwrap()[0][0].as_deref(), Some("15"));
    assert!(s.rows(0, 1).is_err());
}

#[test]
fn changed_file_requires_reopening() {
    let t = Temp::new("parquet");
    write_parquet(&t.path, &simple(0, 2), 10, Compression::UNCOMPRESSED);
    let mut s = Parquet::open(&t.path, Limits::default()).unwrap();
    t.write(b"changed");
    assert!(s.rows(0, 1).unwrap_err().contains("changed"));
}

#[test]
fn compact_page_lengths_are_bounded_before_decode() {
    // PageHeader: type=DATA_PAGE, uncompressed=2^30, compressed=0, stop.
    let bytes = [0x15, 0, 0x15, 0x80, 0x80, 0x80, 0x80, 8, 0x15, 0, 0];
    assert!(
        columnar::compact::page(bytes.as_slice(), bytes.len(), 256)
            .unwrap_err()
            .contains("budget")
    );
    assert!(columnar::compact::page([0x15, 0, 0x15, 2, 0x15, 100, 0].as_slice(), 7, 256).is_err());
}

#[test]
fn twenty_thousand_mutations_never_panic() {
    let t = Temp::new("parquet");
    write_parquet(&t.path, &simple(0, 12), 4, Compression::SNAPPY);
    let mut seeds = vec![std::fs::read(&t.path).unwrap()];
    write_parquet(&t.path, &varied(), 4, Compression::ZSTD(Default::default()));
    seeds.push(std::fs::read(&t.path).unwrap());
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let limits = Limits {
        metadata: 64 << 10,
        columns: 64,
        batches: 64,
        decoded: 1 << 20,
        ..Limits::default()
    };
    for round in 0..20_000 {
        t.write(&rng.mutate(&seeds, round));
        if let Ok(mut s) = guard(|| Parquet::open(&t.path, limits)) {
            let _ = s.rows(0, 12);
        }
    }
}

#[test]
fn malicious_schema_depth_and_collection_lengths_are_rejected() {
    let mut footer = vec![0x29, 0xfc, 66]; // field 2: list of 66 schema structs
    for _ in 0..65 {
        footer.extend([0x48, 1, b'r', 0x15, 2, 0]);
    }
    footer.extend([0x15, 2, 0x38, 1, b'x', 0, 0]);
    assert!(
        columnar::compact::footer(&footer, Limits::default())
            .unwrap_err()
            .contains("64 levels")
    );
    assert!(
        columnar::compact::footer(&[0x29, 0xfc, 0xff, 0xff, 0xff, 0xff, 7], Limits::default())
            .is_err()
    );
}
