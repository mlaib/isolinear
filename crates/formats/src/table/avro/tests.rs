use super::*;
use std::{io::Write, path::PathBuf, time::Instant};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("isolinear-avro-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ----- a minimal Avro writer (zig-zag varints, length-prefixed bytes) -----

fn long(v: i64, out: &mut Vec<u8>) {
    let mut z = ((v << 1) ^ (v >> 63)) as u64;
    loop {
        let b = (z & 0x7f) as u8;
        z >>= 7;
        if z == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}
fn bytes(b: &[u8], out: &mut Vec<u8>) {
    long(b.len() as i64, out);
    out.extend(b);
}
const SYNC: [u8; 16] = *b"0123456789abcdef";

fn compress(codec: &str, data: &[u8]) -> Vec<u8> {
    match codec {
        "null" => data.to_vec(),
        "deflate" => {
            let mut e = flate2::write::DeflateEncoder::new(vec![], flate2::Compression::default());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        }
        "zstandard" => zstd::encode_all(data, 3).unwrap(),
        "snappy" => {
            let mut out = snap::raw::Encoder::new().compress_vec(data).unwrap();
            let mut crc = flate2::Crc::new();
            crc.update(data);
            out.extend(crc.sum().to_be_bytes());
            out
        }
        other => panic!("{other}"),
    }
}

/// An object container: header with `schema`, `codec` and one extra entry, then `blocks`.
fn container(schema: &str, codec: &str, blocks: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"Obj\x01".to_vec();
    long(3, &mut out);
    for (k, v) in [
        ("avro.schema", schema),
        ("avro.codec", codec),
        ("origin", "test"),
    ] {
        bytes(k.as_bytes(), &mut out);
        bytes(v.as_bytes(), &mut out);
    }
    long(0, &mut out);
    out.extend(SYNC);
    for (rows, data) in blocks {
        let packed = compress(codec, data);
        long(*rows as i64, &mut out);
        long(packed.len() as i64, &mut out);
        out.extend(packed);
        out.extend(SYNC);
    }
    out
}

const SCHEMA: &str = r#"{"type": "record", "name": "Order", "namespace": "shop", "fields": [
  {"name": "id", "type": "long"},
  {"name": "customer", "type": ["null", "string"]},
  {"name": "score", "type": ["null", "double"]},
  {"name": "paid", "type": "boolean"},
  {"name": "tags", "type": {"type": "array", "items": "string"}},
  {"name": "status", "type": {"type": "enum", "name": "Status", "symbols": ["NEW", "SHIPPED"]}},
  {"name": "at", "type": {"type": "long", "logicalType": "timestamp-millis"}},
  {"name": "day", "type": {"type": "int", "logicalType": "date"}},
  {"name": "price", "type": {"type": "bytes", "logicalType": "decimal", "precision": 9, "scale": 2}},
  {"name": "address", "type": {"type": "record", "name": "Address", "fields": [
    {"name": "city", "type": "string"}, {"name": "next", "type": ["null", "Address"]}]}},
  {"name": "attrs", "type": {"type": "map", "values": "int"}},
  {"name": "note", "type": {"type": "string"}, "doc": "café 😀"}
]}"#;

fn row(i: i64, out: &mut Vec<u8>) {
    long(i, out);
    if i % 5 == 0 {
        long(0, out);
    } else {
        long(1, out);
        bytes(format!("c{i}").as_bytes(), out);
    }
    long(1, out);
    out.extend((i as f64 * 1.5).to_le_bytes());
    out.push((i % 2) as u8);
    if i % 3 != 0 {
        long(2, out);
        bytes(b"a", out);
        bytes(b"b", out);
    }
    long(0, out);
    long(i % 2, out);
    long(1_700_000_000_000 + i * 1000, out);
    long(19_000 + i, out);
    let price = if i == 7 { -150i64 } else { i * 100 + 5 };
    let be = (price as i16).to_be_bytes();
    bytes(&be, out);
    bytes(b"Paris", out);
    long(1, out);
    bytes(b"Lyon", out);
    long(0, out);
    long(1, out);
    bytes(b"k", out);
    long(i, out);
    long(0, out);
    bytes("é \"q\"".as_bytes(), out);
}

/// Twelve rows in three blocks of four.
fn orders(codec: &str) -> Vec<u8> {
    let blocks: Vec<(u64, Vec<u8>)> = (0..3)
        .map(|b| {
            let mut data = vec![];
            for i in b * 4..b * 4 + 4 {
                row(i, &mut data);
            }
            (4, data)
        })
        .collect();
    container(SCHEMA, codec, &blocks)
}

fn page(
    path: &std::path::Path,
    columns: &[usize],
    first: u64,
    count: usize,
) -> Vec<Vec<Option<String>>> {
    let rows = Avro::open(path).unwrap().rows(first, count).unwrap();
    rows.into_iter()
        .map(|r| columns.iter().map(|&c| r[c].clone()).collect())
        .collect()
}

#[test]
fn avro_files_are_detected_and_inspected_from_block_headers() {
    let d = dir("inspect");
    let path = d.join("orders.avro");
    std::fs::write(&path, orders("deflate")).unwrap();
    let head = std::fs::read(&path).unwrap();
    let detection = crate::detect::detect("orders.avro", &head);
    assert_eq!(detection.format.id, "avro");
    assert!(detection.views.contains(&crate::registry::View::Table));
    let fake = d.join("fake.avro");
    std::fs::write(&fake, "not avro").unwrap();
    assert!(open(&fake, None).is_err());

    let s = Avro::open(&path).map(|s| s.schema).unwrap();
    assert_eq!(s.rows, Some(12));
    assert!(s.details.contains(&("Blocks".into(), "3".into())));
    let types: Vec<(&str, &str)> = s
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.type_name.as_str()))
        .collect();
    assert_eq!(types[0], ("id", "long"));
    assert_eq!(types[1], ("customer", "union<null, string>"));
    assert_eq!(types[4], ("tags", "array<string>"));
    assert_eq!(types[5], ("status", "enum Status"));
    assert_eq!(types[6], ("at", "timestamp-millis"));
    assert_eq!(types[8], ("price", "decimal(scale 2)"));
    assert_eq!(types[9], ("address", "record shop.Address"));
    assert!(s.details.contains(&("origin".into(), "test".into())));
    assert!(s.details.contains(&("avro.codec".into(), "deflate".into())));
    assert!(
        s.text
            .as_ref()
            .unwrap()
            .contains("\"logicalType\": \"timestamp-millis\"")
    );
    assert!(
        s.text.as_ref().unwrap().contains("café 😀"),
        "JSON escapes and surrogate pairs"
    );
    assert!(s.details.contains(&("Codec".into(), "deflate".into())));
}

#[test]
fn avro_pages_decode_every_type_across_blocks_and_codecs() {
    let d = dir("pages");
    let mut first = None;
    for codec in ["null", "deflate", "zstandard", "snappy"] {
        let path = d.join(format!("{codec}.avro"));
        std::fs::write(&path, orders(codec)).unwrap();
        let rows = page(&path, &(0..12).collect::<Vec<_>>(), 3, 5);
        assert_eq!(rows.len(), 5, "{codec}");
        match &first {
            None => first = Some(rows),
            Some(f) => assert_eq!(&rows, f, "{codec} decodes like the others"),
        }
    }
    let rows = first.unwrap();
    let s = |v: &str| Some(v.to_owned());
    // Row 3 (block 0), then rows 4..7 (block 1).
    assert_eq!(rows[0][0], s("3"));
    assert_eq!(rows[0][1], s("c3"));
    assert_eq!(rows[2][1], None, "null branch of a nullable union");
    assert_eq!(rows[0][2], s("4.5"));
    assert_eq!(rows[0][3], s("true"));
    assert_eq!(rows[0][4], s("[]"));
    assert_eq!(rows[1][4], s(r#"["a", "b"]"#));
    assert_eq!(rows[0][5], s("SHIPPED"));
    assert_eq!(rows[1][5], s("NEW"));
    assert_eq!(rows[0][6], s("2023-11-14 22:13:23.000"));
    assert_eq!(rows[0][7], s("2022-01-11"));
    assert_eq!(rows[0][8], s("3.05"));
    assert_eq!(rows[4][8], s("-1.50"), "row 7: negative decimal");
    assert_eq!(
        rows[0][9],
        s(r#"{"city": "Paris", "next": {"city": "Lyon", "next": null}}"#),
        "recursive named record"
    );
    assert_eq!(rows[0][10], s(r#"{"k": 3}"#));
    assert_eq!(rows[0][11], s("é \"q\""));

    // A reordered projection, a deep start, and a single cell.
    let path = d.join("deflate.avro");
    let rows = page(&path, &[6, 0], 10, 50);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1], vec![s("2023-11-14 22:13:31.000"), s("11")]);
    assert_eq!(page(&path, &[1], 6, 1)[0][0], Some("c6".to_owned()));
}

#[test]
fn damaged_and_hostile_avro_files_fail_cleanly() {
    let d = dir("damaged");
    let good = orders("deflate");
    let path = d.join("x.avro");
    let open = |bytes: &[u8]| {
        std::fs::write(&path, bytes).unwrap();
        Avro::open(&path).map(|s| s.schema)
    };
    // Cut inside the last block: the complete blocks stay readable, with a warning.
    let s = open(&good[..good.len() - 20]).unwrap();
    assert_eq!(s.rows, Some(8));
    assert!(s.details[0].1.contains("after 2 blocks"), "{:?}", s.details);
    // A wrong sync marker after the first block.
    let mut bad = good.clone();
    let at = bad
        .windows(16)
        .enumerate()
        .filter(|(_, w)| *w == SYNC)
        .nth(2)
        .unwrap()
        .0;
    bad[at] ^= 1;
    assert_eq!(open(&bad).unwrap().rows, Some(4));
    // A block shorter than its rows, an unsupported codec, invalid and unknown schemas.
    std::fs::write(&path, container(SCHEMA, "null", &[(1, vec![])])).unwrap();
    let e = Avro::open(&path)
        .and_then(|mut s| s.rows(0, 1))
        .err()
        .unwrap();
    assert!(e.contains("damaged"), "{e}");
    let mut bz = b"Obj\x01".to_vec();
    long(2, &mut bz);
    for (k, v) in [("avro.schema", "\"long\""), ("avro.codec", "bzip2")] {
        bytes(k.as_bytes(), &mut bz);
        bytes(v.as_bytes(), &mut bz);
    }
    long(0, &mut bz);
    bz.extend(SYNC);
    long(1, &mut bz);
    long(2, &mut bz);
    bz.extend([0, 0]);
    bz.extend(SYNC);
    std::fs::write(&path, &bz).unwrap();
    let e = Avro::open(&path)
        .and_then(|mut s| s.rows(0, 1))
        .err()
        .unwrap();
    assert!(e.contains("bzip2"), "{e}");
    assert!(open(&container("{\"type\": ", "null", &[])).is_err());
    assert!(open(&container("\"Nowhere\"", "null", &[])).is_err());
    // An array of nulls declaring 2^40 items stops at the item budget instead of looping.
    let mut data = vec![];
    long(1 << 40, &mut data);
    std::fs::write(
        &path,
        container(
            r#"{"type": "array", "items": "null"}"#,
            "null",
            &[(1, data)],
        ),
    )
    .unwrap();
    let started = Instant::now();
    let e = Avro::open(&path)
        .and_then(|mut s| s.rows(0, 1))
        .err()
        .unwrap();
    assert!(e.contains("too many items"), "{e}");
    assert!(started.elapsed().as_secs() < 5);
    // Deep nesting through a recursive type is bounded.
    let mut deep = vec![];
    for _ in 0..10_000 {
        long(1, &mut deep);
    }
    long(0, &mut deep);
    std::fs::write(
        &path,
        container(
            r#"{"type": "record", "name": "L", "fields": [{"name": "next", "type": ["null", "L"]}]}"#,
            "null",
            &[(1, deep)],
        ),
    )
    .unwrap();
    let e = Avro::open(&path)
        .and_then(|mut s| s.rows(0, 1))
        .err()
        .unwrap();
    assert!(e.contains("nested too deeply"), "{e}");
    // A snappy block whose checksum does not match.
    let mut snappy = orders("snappy");
    let at = snappy
        .windows(16)
        .enumerate()
        .filter(|(_, w)| *w == SYNC)
        .nth(1)
        .unwrap()
        .0;
    snappy[at - 1] ^= 0xff;
    std::fs::write(&path, &snappy).unwrap();
    let e = Avro::open(&path)
        .and_then(|mut s| s.rows(0, 1))
        .err()
        .unwrap();
    assert!(e.contains("checksum"), "{e}");
}

#[test]
fn random_avro_files_never_panic() {
    let d = dir("random");
    let path = d.join("r.avro");
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let bases = [orders("null"), orders("deflate"), orders("snappy")];
    for i in 0..20_000 {
        let mut b = bases[i % 3].clone();
        for _ in 0..(next() % 6 + 1) {
            let at = (next() % b.len() as u64) as usize;
            b[at] = next() as u8;
        }
        if next() % 4 == 0 {
            b.truncate((next() % b.len() as u64) as usize);
        }
        std::fs::write(&path, &b).unwrap();
        let started = Instant::now();
        if let Ok(mut source) = Avro::open(&path) {
            let _ = source.rows(0, 12);
        }
        assert!(started.elapsed().as_secs() < 2, "case {i} took too long");
    }
}

/// Files written by fastavro 1.x (an independent implementation): tests/fixtures/fastavro-*.avro,
/// 40 records in blocks of 2-3, with logical types, a fixed decimal and a three-way union.
#[test]
fn files_from_another_writer_read_the_same() {
    let fixtures =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/avro-orc");
    for codec in ["null", "deflate"] {
        let path = fixtures.join(format!("fastavro-{codec}.avro"));
        let s = Avro::open(&path).map(|s| s.schema).unwrap();
        assert_eq!(s.rows, Some(40), "{codec}");
        assert!(s.details.contains(&("Blocks".into(), "14".into())));
        assert!(s.details.contains(&("made.by".into(), "fastavro".into())));
        assert_eq!(
            (s.columns[7].name.as_str(), s.columns[7].type_name.as_str()),
            ("amount", "decimal(scale 3)")
        );
        let rows = page(&path, &(0..13).collect::<Vec<_>>(), 3, 3);
        let s = |v: &str| Some(v.to_owned());
        assert_eq!(
            rows[0],
            vec![
                s("3"),
                s("s3"),
                s("0.75"),
                s("false"),
                s("03ff"),
                s("2024-02-29 12:00:03.123456"),
                s("2024-01-04"),
                s("-3.007"),
                s("00000000-0000-0000-0000-000000000003"),
                s("HIGH"),
                s("[3, 3.5]"),
                s(r#"{"k": "v3"}"#),
                s("3"),
            ],
            "{codec}"
        );
        assert_eq!(rows[1][1], None);
        assert_eq!(rows[1][12], s("t4"));
        assert_eq!(rows[2][12], None, "null branch of a three-way union");
    }
}

#[test]
fn scalar_schemas_empty_records_and_bounded_unicode_cells() {
    let d = dir("scalar");
    let path = d.join("scalar.avro");
    std::fs::write(&path, container("\"null\"", "null", &[(3, vec![])])).unwrap();
    let mut source = open(&path, Some("rows")).unwrap();
    assert_eq!(source.schema().columns[0].name, "value");
    assert_eq!(source.rows(0, 10).unwrap(), vec![vec![None]; 3]);
    assert!(source.rows(u64::MAX, usize::MAX).unwrap().is_empty());
    assert!(open(&path, Some("missing")).is_err());
    std::fs::write(
        &path,
        container(
            r#"{"type":"record","name":"Empty","fields":[]}"#,
            "null",
            &[(3, vec![])],
        ),
    )
    .unwrap();
    assert_eq!(
        open(&path, None).unwrap().rows(0, 3).unwrap(),
        vec![vec![]; 3]
    );
    let text = "é🙂".repeat(2000);
    let mut data = vec![];
    bytes(text.as_bytes(), &mut data);
    std::fs::write(&path, container("\"string\"", "null", &[(1, data)])).unwrap();
    assert_eq!(
        open(&path, None).unwrap().rows(0, 1).unwrap()[0][0],
        Some(super::super::cut(text))
    );
    let mut data = vec![];
    bytes(&vec![0xff; 4000], &mut data);
    std::fs::write(&path, container("\"string\"", "null", &[(1, data)])).unwrap();
    assert_eq!(
        open(&path, None).unwrap().rows(0, 1).unwrap()[0][0],
        Some(super::super::cut("�".repeat(4000)))
    );
}

#[test]
fn sized_array_blocks_and_numeric_overflows_are_checked() {
    let d = dir("sized");
    let path = d.join("x.avro");
    let schema = r#"{"type":"array","items":"long"}"#;
    let mut data = vec![];
    long(-2, &mut data);
    long(2, &mut data);
    long(1, &mut data);
    long(2, &mut data);
    long(0, &mut data);
    std::fs::write(&path, container(schema, "null", &[(1, data.clone())])).unwrap();
    assert_eq!(
        open(&path, None).unwrap().rows(0, 1).unwrap()[0][0].as_deref(),
        Some("[1, 2]")
    );
    data[1] = 6; // Three bytes declared, but the two values occupy two.
    std::fs::write(&path, container(schema, "null", &[(1, data)])).unwrap();
    assert!(
        open(&path, None)
            .unwrap()
            .rows(0, 1)
            .unwrap_err()
            .contains("declared size")
    );
    let mut bad = vec![0xff; 9];
    bad.push(0x7f);
    std::fs::write(&path, container("\"long\"", "null", &[(1, bad)])).unwrap();
    assert!(
        open(&path, None)
            .unwrap()
            .rows(0, 1)
            .unwrap_err()
            .contains("too large")
    );
    let mut bad = vec![];
    long(i64::MAX, &mut bad);
    std::fs::write(&path, container("\"int\"", "null", &[(1, bad)])).unwrap();
    assert!(
        open(&path, None)
            .unwrap()
            .rows(0, 1)
            .unwrap_err()
            .contains("out of range")
    );
}

#[test]
fn blocks_are_lazy_requests_are_bounded_and_source_bytes_stay_unchanged() {
    let d = dir("lazy");
    let path = d.join("x.avro");
    // A valid container with one row but no value bytes: metadata must still open.
    let data = container("\"long\"", "null", &[(1, vec![])]);
    std::fs::write(&path, &data).unwrap();
    let mut source = open(&path, None).unwrap();
    assert_eq!(source.schema().rows, Some(1));
    assert!(source.rows(0, 0).unwrap().is_empty());
    assert!(source.rows(0, 1).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), data);
    std::fs::write(&path, container("\"null\"", "null", &[(2_000_000, vec![])])).unwrap();
    assert!(
        open(&path, None)
            .unwrap()
            .rows(0, 2_000_000)
            .unwrap_err()
            .contains("1,000,000 cells")
    );
    let mut short = b"Obj\x01".to_vec();
    long(1, &mut short);
    long(60_000_000, &mut short);
    std::fs::write(&path, short).unwrap();
    assert!(open(&path, None).is_err());
}

#[test]
fn qualified_names_are_resolved_in_the_current_namespace() {
    let schema = r#"{"type":"record","name":"Root","fields":[
        {"name":"a","type":{"type":"record","name":"a.Item","fields":[{"name":"x","type":"long"}]}},
        {"name":"b","type":{"type":"record","name":"b.Item","fields":[{"name":"text","type":"string"},{"name":"next","type":["null","Item"]}]}}
    ]}"#;
    let mut data = vec![];
    long(42, &mut data);
    bytes(b"one", &mut data);
    long(1, &mut data);
    bytes(b"two", &mut data);
    long(0, &mut data);
    let d = dir("names");
    let path = d.join("x.avro");
    std::fs::write(&path, container(schema, "null", &[(1, data)])).unwrap();
    let rows = open(&path, None).unwrap().rows(0, 1).unwrap();
    assert_eq!(rows[0][0].as_deref(), Some(r#"{"x": 42}"#));
    assert_eq!(
        rows[0][1].as_deref(),
        Some(r#"{"text": "one", "next": {"text": "two", "next": null}}"#)
    );
}

#[test]
fn metadata_block_sizes_are_checked() {
    let path = dir("metadata-size").join("x.avro");
    let mut entry = vec![];
    bytes(b"avro.schema", &mut entry);
    bytes(b"\"long\"", &mut entry);
    for size in [entry.len() as i64, entry.len() as i64 - 1, -1, i64::MAX] {
        let mut raw = b"Obj\x01".to_vec();
        long(-1, &mut raw);
        long(size, &mut raw);
        raw.extend(&entry);
        long(0, &mut raw);
        raw.extend(SYNC);
        std::fs::write(&path, raw).unwrap();
        assert_eq!(open(&path, None).is_ok(), size == entry.len() as i64);
    }
}

#[test]
fn schema_expansion_is_bounded() {
    let path = dir("schema-limits").join("x.avro");
    let schema = serde_json::json!({"type":"record", "name":"Row", "namespace":"x".repeat(1025), "fields":[]}).to_string();
    std::fs::write(&path, container(&schema, "null", &[])).unwrap();
    assert!(open(&path, None).err().unwrap().contains("1,024 bytes"));
    let schema = serde_json::to_string(&vec!["null"; 10_001]).unwrap();
    std::fs::write(&path, container(&schema, "null", &[])).unwrap();
    assert!(open(&path, None).err().unwrap().contains("10,000 branches"));
}
