use super::*;
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

type Rows = Vec<Vec<Option<String>>>;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/avro-orc")
        .join(name)
}
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("isolinear-orc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Rows of `columns` as display text (as the table shows them), and rows per batch.
fn read(
    path: &Path,
    columns: &[usize],
    first: u64,
    count: usize,
) -> Result<(Rows, Vec<usize>), String> {
    let mut source = Orc::open(path)?;
    if columns.iter().any(|&c| c >= source.schema.columns.len()) {
        return Err("Column does not exist".into());
    }
    let rows = source
        .rows(first, count)?
        .into_iter()
        .map(|r| columns.iter().map(|&c| r[c].clone()).collect())
        .collect();
    let mut at = 0u64;
    let mut sizes = vec![];
    let end = first.saturating_add(count as u64);
    for stripe in &source.tail.footer.stripes {
        let next = at + stripe.rows;
        if at < end && next > first {
            sizes.push((end.min(next) - first.max(at)) as usize);
        }
        at = next;
    }
    Ok((rows, sizes))
}

/// Column names and rows of `orc-expected.tsv` (`\N` is null; `\t`, `\n`, `\\` escaped).
fn expected() -> (Vec<String>, Rows) {
    let text = std::fs::read_to_string(fixture("orc-expected.tsv")).unwrap();
    let mut lines = text.lines();
    let names = lines
        .next()
        .unwrap()
        .split('\t')
        .map(str::to_owned)
        .collect();
    let unescape = |cell: &str| {
        let mut out = String::new();
        let mut chars = cell.chars();
        while let Some(c) = chars.next() {
            out.push(match (c, c == '\\') {
                (_, true) => match chars.next() {
                    Some('t') => '\t',
                    Some('n') => '\n',
                    Some('\\') => '\\',
                    other => panic!("bad escape {other:?}"),
                },
                (c, false) => c,
            });
        }
        out
    };
    let rows = lines
        .map(|l| {
            l.split('\t')
                .map(|c| (c != "\\N").then(|| unescape(c)))
                .collect()
        })
        .collect();
    (names, rows)
}

const FILES: [&str; 6] = [
    "orc-none.orc",
    "orc-zlib.orc",
    "orc-snappy.orc",
    "orc-zstd.orc",
    "orc-lz4.orc",
    "orc-v011-zlib.orc",
];

#[test]
fn orc_files_are_detected_and_inspected_from_the_tail() {
    let d = dir("inspect");
    let fake = d.join("fake.orc");
    std::fs::write(&fake, "not orc").unwrap();
    assert!(!std::fs::read(&fake).unwrap().starts_with(b"ORC"));
    assert!(inspect(&fake, &Limits::default()).is_err());
    for (name, codec) in FILES.iter().zip([
        "none",
        "ZLIB, 64 KiB blocks",
        "SNAPPY, 64 KiB blocks",
        "ZSTD, 64 KiB blocks",
        "LZ4, 64 KiB blocks",
        "ZLIB, 64 KiB blocks",
    ]) {
        let path = fixture(name);
        assert!(std::fs::read(&path).unwrap().starts_with(b"ORC"), "{name}");
        let s = inspect(&path, &Limits::default()).unwrap();
        assert_eq!(s.rows, 300, "{name}");
        assert_eq!(
            s.row_groups.iter().map(|g| g.rows).collect::<Vec<_>>(),
            vec![50; 6],
            "{name}"
        );
        assert!(s.row_groups.iter().all(|g| g.compressed > 0));
        let kv = |k: &str| {
            s.key_value
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(kv("Compression"), Some(codec), "{name}");
        assert_eq!(kv("Stripes"), Some("6"));
        assert_eq!(kv("Row index stride"), Some("10000"));
        assert_eq!(
            kv("ORC version"),
            Some(if name.contains("v011") {
                "0.11"
            } else {
                "0.12"
            })
        );
        assert!(kv("Written by").unwrap().starts_with("ORC C++"), "{name}");
        assert_eq!(kv("Writer time zone"), Some("GMT"));
        assert_eq!(kv("Warning"), None, "{:?}", s.key_value);
        assert_eq!(
            kv("Statistics of id"),
            Some("300 values, no nulls, min 0, max 299")
        );
        assert!(
            kv("Statistics of small")
                .unwrap()
                .starts_with("276 values, has nulls"),
            "{:?}",
            kv("Statistics of small")
        );
        let fields: Vec<(&str, &str)> = s
            .fields
            .iter()
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect();
        assert_eq!(
            fields[..19],
            [
                ("id", "bigint"),
                ("flag", "boolean"),
                ("tiny", "tinyint"),
                ("small", "smallint"),
                ("mid", "int"),
                ("big", "bigint"),
                ("f32", "float"),
                ("f64", "double"),
                ("name", "string"),
                ("color", "string"),
                ("raw", "binary"),
                ("price", "decimal(10,2)"),
                ("day", "date"),
                ("ts", "timestamp"),
                ("tsz", "timestamp with local time zone"),
                ("tags", "array<int>"),
                ("attrs", "map<string,bigint>"),
                ("point", "struct<x:int,label:string>"),
                ("choice", "uniontype<int,string>"),
            ],
            "{name}"
        );
        assert_eq!(fields.len(), if name == &"orc-none.orc" { 19 } else { 20 });
        assert!(
            s.schema
                .contains("point: struct  (column 21)\n  x: int  (column 22)\n")
        );
        assert!(s.schema.contains("  element: int  (column 17)"));
    }
}

#[test]
fn orc_pages_match_pyarrow_for_every_codec_and_rle_version() {
    let (names, want) = expected();
    assert_eq!(names.len(), 19);
    assert_eq!(want.len(), 300);
    let all: Vec<usize> = (0..19).collect();
    for name in FILES {
        let path = fixture(name);
        let (rows, sizes) = read(&path, &all, 0, 1000).unwrap();
        assert_eq!(sizes, vec![50; 6], "{name}: one batch per stripe");
        for (i, (got, want)) in rows.iter().zip(&want).enumerate() {
            for (c, (g, w)) in got.iter().zip(want).enumerate() {
                assert_eq!(g, w, "{name}: row {i}, column {}", names[c]);
            }
        }
    }
    // Spot checks of what the expected file says, so a wrong fixture cannot hide a bug.
    let s = |v: &str| Some(v.to_owned());
    assert_eq!(want[5][8], s(""), "an empty string");
    assert_eq!(want[9][8], None, "a null string");
    assert_eq!(want[42][8], s("café ✓"));
    assert_eq!(want[3][9], s(""), "an empty dictionary entry");
    assert_eq!(want[0][9], None);
    assert_eq!(want[7][5], s("1152921504606846983"), "a patched outlier");
    assert_eq!(want[1][11], s("-9876.55"));
    assert_eq!(want[0][12], s("1942-08-16"));
    assert_eq!(want[1][13], s("1920-05-14 16:00:00.001234567"));
    assert_eq!(want[1][14], s("2023-11-14 22:13:21.000000123 UTC"));
    assert_eq!(want[3][15], None);
    assert_eq!(want[2][16], s(r#"{"k0": 0, "k1": 2}"#));
    assert_eq!(want[0][17], s(r#"{"x": 0, "label": null}"#));
    assert_eq!(want[5][17], None, "a null struct");
    assert_eq!(want[1][18], s("u1"));
}

#[test]
fn deep_pages_cross_stripes_and_stop_at_the_end() {
    let (_, want) = expected();
    for name in ["orc-zlib.orc", "orc-v011-zlib.orc", "orc-none.orc"] {
        let path = fixture(name);
        // Rows 123-182 cross from the third stripe into the fourth.
        let (rows, sizes) = read(&path, &[3, 9, 17], 123, 60).unwrap();
        assert_eq!(sizes, vec![27, 33], "{name}");
        for (k, row) in rows.iter().enumerate() {
            let w = &want[123 + k];
            assert_eq!(
                row,
                &vec![w[3].clone(), w[9].clone(), w[17].clone()],
                "{name}"
            );
        }
        // The last rows, a page past the end, and a column that does not exist.
        let (rows, _) = read(&path, &[0, 18], 295, 50).unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[4], vec![want[299][0].clone(), want[299][18].clone()]);
        assert!(read(&path, &[0], 300, 10).unwrap().0.is_empty());
        assert!(read(&path, &[0], 10, 0).unwrap().0.is_empty());
        assert!(
            read(&path, &[40], 0, 1)
                .unwrap_err()
                .contains("does not exist")
        );
    }
    let source = Orc::open(&fixture("orc-zlib.orc")).unwrap();
    assert_eq!(source.schema.columns[0].type_name, "bigint");
}

// ----- a minimal ORC writer (uncompressed, one stripe) for hostile files -----

fn varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}
fn int_field(field: u64, v: u64, out: &mut Vec<u8>) {
    varint(field << 3, out);
    varint(v, out);
}
fn bytes_field(field: u64, b: &[u8], out: &mut Vec<u8>) {
    varint(field << 3 | 2, out);
    varint(b.len() as u64, out);
    out.extend(b);
}
/// A `Type` message: kind, subtypes and field names.
fn ty(kind: u64, subtypes: &[u64], names: &[&str]) -> Vec<u8> {
    let mut t = vec![];
    int_field(1, kind, &mut t);
    for &s in subtypes {
        int_field(2, s, &mut t);
    }
    for n in names {
        bytes_field(3, n.as_bytes(), &mut t);
    }
    t
}
const STRUCT: u64 = 12;
const INT: u64 = 3;
const LIST: u64 = 10;
const STRING: u64 = 7;

/// An uncompressed ORC file with one stripe of `rows` rows holding `streams` (kind, column,
/// bytes); every column is DIRECT_V2 (DICTIONARY_V2 where `dict` gives a dictionary size).
fn orc_file(
    types: &[Vec<u8>],
    streams: &[(u64, u64, Vec<u8>)],
    rows: u64,
    dict: Option<(u64, u64)>,
    compression: u64,
) -> Vec<u8> {
    let mut out = b"ORC".to_vec();
    let mut sf = vec![];
    let mut data_len = 0;
    for (kind, column, bytes) in streams {
        out.extend(bytes);
        data_len += bytes.len() as u64;
        let mut s = vec![];
        int_field(1, *kind, &mut s);
        int_field(2, *column, &mut s);
        int_field(3, bytes.len() as u64, &mut s);
        bytes_field(1, &s, &mut sf);
    }
    for c in 0..types.len() as u64 {
        let mut e = vec![];
        match dict {
            Some((col, size)) if col == c => {
                int_field(1, 3, &mut e);
                int_field(2, size, &mut e);
            }
            _ => int_field(1, 2, &mut e),
        }
        bytes_field(2, &e, &mut sf);
    }
    bytes_field(3, b"UTC", &mut sf);
    out.extend(&sf);
    let mut stripe = vec![];
    int_field(1, 3, &mut stripe);
    int_field(2, 0, &mut stripe);
    int_field(3, data_len, &mut stripe);
    int_field(4, sf.len() as u64, &mut stripe);
    int_field(5, rows, &mut stripe);
    let mut footer = vec![];
    int_field(1, 3, &mut footer);
    int_field(2, out.len() as u64 - 3, &mut footer);
    bytes_field(3, &stripe, &mut footer);
    for t in types {
        bytes_field(4, t, &mut footer);
    }
    int_field(6, rows, &mut footer);
    out.extend(&footer);
    let mut ps = vec![];
    int_field(1, footer.len() as u64, &mut ps);
    int_field(2, compression, &mut ps);
    int_field(3, 65536, &mut ps);
    bytes_field(4, &[0, 12], &mut ps);
    int_field(5, 0, &mut ps);
    bytes_field(8000, b"ORC", &mut ps);
    out.extend(&ps);
    out.push(ps.len() as u8);
    out
}

/// Footer and metadata lengths and the postscript length of a file.
fn tail_lengths(file: &[u8]) -> (usize, usize, usize) {
    let ps_len = *file.last().unwrap() as usize;
    let ps = &file[file.len() - 1 - ps_len..file.len() - 1];
    let (mut footer, mut metadata, mut i) = (0, 0, 0);
    let read = |i: &mut usize| {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = ps[*i];
            *i += 1;
            v |= u64::from(b & 0x7f) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                return v;
            }
        }
    };
    while i < ps.len() {
        let key = read(&mut i);
        let value = match key & 7 {
            0 => read(&mut i),
            2 => {
                let n = read(&mut i) as usize;
                i += n;
                0
            }
            _ => unreachable!(),
        };
        match key >> 3 {
            1 => footer = value as usize,
            5 => metadata = value as usize,
            _ => {}
        }
    }
    (footer, metadata, ps_len)
}

fn inspect_bytes(path: &Path, bytes: &[u8]) -> Result<Summary, String> {
    std::fs::write(path, bytes).unwrap();
    inspect(path, &Limits::default())
}
fn fails(path: &Path, bytes: &[u8]) -> String {
    match inspect_bytes(path, bytes) {
        Ok(_) => panic!("a damaged file opened"),
        Err(e) => e,
    }
}
fn page_bytes(path: &Path, bytes: &[u8], columns: &[usize]) -> Result<Rows, String> {
    std::fs::write(path, bytes).unwrap();
    read(path, columns, 0, 1000).map(|(rows, _)| rows)
}

#[test]
fn a_hand_built_file_reads_back() {
    let d = dir("built");
    let path = d.join("x.orc");
    // int column: DELTA run 5, 7, 9; string column (DIRECT_V2): lengths SHORT_REPEAT 2 x3.
    let types = [
        ty(STRUCT, &[1, 2], &["n", "s"]),
        ty(INT, &[], &[]),
        ty(STRING, &[], &[]),
    ];
    let streams = [
        (1, 1, vec![0xc0, 0x02, 0x0a, 0x04]),
        (1, 2, b"abcdef".to_vec()),
        (2, 2, vec![0x00, 0x02]),
    ];
    let rows = page_bytes(&path, &orc_file(&types, &streams, 3, None, 0), &[0, 1]).unwrap();
    let s = |v: &str| Some(v.to_owned());
    assert_eq!(
        rows,
        vec![
            vec![s("5"), s("ab")],
            vec![s("7"), s("cd")],
            vec![s("9"), s("ef")]
        ]
    );
    // The same strings through a dictionary: entries "ab", "cdef", indices 1, 0, 1.
    let streams = [
        (1, 1, vec![0xc0, 0x02, 0x0a, 0x04]),
        (3, 2, b"abcdef".to_vec()),
        (2, 2, vec![0x44, 0x01, 0x50]),
        (1, 2, vec![0x40, 0x02, 0xa0]),
    ];
    let rows = page_bytes(&path, &orc_file(&types, &streams, 3, Some((2, 2)), 0), &[1]).unwrap();
    assert_eq!(rows, vec![vec![s("cdef")], vec![s("ab")], vec![s("cdef")]]);
}

#[test]
fn damaged_and_hostile_orc_files_fail_cleanly() {
    let d = dir("damaged");
    let path = d.join("x.orc");
    let good = std::fs::read(fixture("orc-zlib.orc")).unwrap();
    let all: Vec<usize> = (0..20).collect();
    // Truncated anywhere: the tail is gone.
    for cut in [1, 10, 100, good.len() / 2, good.len() - 3] {
        let e = fails(&path, &good[..good.len() - cut]);
        assert!(!e.is_empty());
    }
    assert!(inspect_bytes(&path, b"ORC").is_err());
    // A postscript length of zero or past the start of the file.
    for len in [0u8, 255] {
        let mut bad = good.clone();
        *bad.last_mut().unwrap() = len;
        let e = fails(&path, &bad);
        assert!(e.contains("postscript"), "{e}");
    }
    // A footer chunk header claiming more bytes than there are.
    let (footer, metadata, ps_len) = tail_lengths(&good);
    let at = good.len() - 1 - ps_len - footer;
    let mut bad = good.clone();
    bad[at..at + 3].copy_from_slice(&[0xfe, 0xff, 0xff]);
    let e = fails(&path, &bad);
    assert!(e.contains("runs past its stream"), "{e}");
    // A damaged metadata section only loses the stripe statistics.
    let mut bad = good.clone();
    bad[at - metadata..at - metadata + 3].copy_from_slice(&[0xfe, 0xff, 0xff]);
    let s = inspect_bytes(&path, &bad).unwrap();
    assert!(
        s.key_value[0].1.contains("stripe statistics"),
        "{:?}",
        s.key_value
    );
    // The middle of the first stripe (its data streams) overwritten: that stripe fails, the others
    // still read.
    let size = inspect(&fixture("orc-zlib.orc"), &Limits::default())
        .unwrap()
        .row_groups[0]
        .compressed as usize;
    let mut bad = good.clone();
    bad[3 + size / 4..3 + size * 3 / 4].fill(0xff);
    std::fs::write(&path, &bad).unwrap();
    assert!(inspect(&path, &Limits::default()).is_ok());
    let e = read(&path, &all, 0, 10).unwrap_err();
    assert!(e.contains("damaged") && e.contains("stripe 1"), "{e}");
    assert_eq!(read(&path, &all, 100, 10).unwrap().0.len(), 10);
    // Unsupported LZO and a block size no writer uses.
    let built = |compression| {
        orc_file(
            &[ty(STRUCT, &[1], &["n"]), ty(INT, &[], &[])],
            &[(1, 1, vec![0x00, 0x02])],
            3,
            None,
            compression,
        )
    };
    assert!(fails(&path, &built(3)).contains("LZO"));
    assert!(fails(&path, &built(9)).contains("compression"));
    assert!(inspect_bytes(&path, &built(0)).is_ok());

    let started = Instant::now();
    // A footer claiming 200,000 columns, and a struct claiming 2 million fields.
    let mut types = vec![ty(STRUCT, &[1], &["n"])];
    types.extend((0..200_000).map(|_| ty(INT, &[], &[])));
    let e = fails(&path, &orc_file(&types, &[], 1, None, 0));
    assert!(e.contains("More than 100000 columns"), "{e}");
    let mut root = vec![];
    int_field(1, STRUCT, &mut root);
    let mut packed = vec![];
    for i in 1..2_000_000u64 {
        varint(i, &mut packed);
    }
    bytes_field(2, &packed, &mut root);
    let e = fails(&path, &orc_file(&[root], &[], 1, None, 0));
    assert!(e.contains("too long"), "{e}");
    // Types nested 100 deep, a type that is its own child, a list without an element type.
    let deep: Vec<Vec<u8>> = (0..100)
        .map(|i| ty(LIST, &[i + 1], &[]))
        .chain([ty(INT, &[], &[])])
        .collect();
    let e = fails(&path, &orc_file(&deep, &[], 1, None, 0));
    assert!(e.contains("nested too deeply"), "{e}");
    let e = fails(
        &path,
        &orc_file(&[ty(STRUCT, &[0], &["me"])], &[], 1, None, 0),
    );
    assert!(e.contains("tree"), "{e}");
    let e = fails(
        &path,
        &orc_file(
            &[ty(STRUCT, &[1], &["l"]), ty(LIST, &[], &[])],
            &[],
            1,
            None,
            0,
        ),
    );
    assert!(e.contains("children"), "{e}");

    // A list whose LENGTH run claims 2^60 elements of an empty struct (which take no input): the
    // value budget stops it.
    let types = [
        ty(STRUCT, &[1], &["l"]),
        ty(LIST, &[2], &[]),
        ty(STRUCT, &[], &[]),
    ];
    let huge = vec![0x38, 0x10, 0, 0, 0, 0, 0, 0, 0];
    let file = orc_file(&types, &[(2, 1, huge.clone())], 1, None, 0);
    assert!(inspect_bytes(&path, &file).is_ok());
    let e = page_bytes(&path, &file, &[0]).unwrap_err();
    assert!(e.contains("too many values"), "{e}");
    // The same run as string lengths runs out of string bytes; as a dictionary size, it is
    // refused before anything is allocated.
    let types = [ty(STRUCT, &[1], &["s"]), ty(STRING, &[], &[])];
    let file = orc_file(&types, &[(2, 1, huge), (1, 1, b"abc".to_vec())], 1, None, 0);
    let e = page_bytes(&path, &file, &[0]).unwrap_err();
    assert!(
        e.contains("ends before its values") || e.contains("too large"),
        "{e}"
    );
    let file = orc_file(
        &types,
        &[(3, 1, b"abc".to_vec()), (2, 1, vec![0x00, 0x01])],
        1,
        Some((1, 1 << 40)),
        0,
    );
    let e = page_bytes(&path, &file, &[0]).unwrap_err();
    assert!(e.contains("dictionary"), "{e}");
    // A stripe claiming 2^40 rows of an int column whose data runs out.
    let file = orc_file(
        &[ty(STRUCT, &[1], &["n"]), ty(INT, &[], &[])],
        &[(1, 1, vec![0x00, 0x02])],
        1 << 40,
        None,
        0,
    );
    std::fs::write(&path, &file).unwrap();
    let e = read(&path, &[0], (1 << 40) - 5, 5).unwrap_err();
    assert!(e.contains("ends before its values"), "{e}");
    assert!(started.elapsed().as_secs() < 10);
}

#[test]
fn random_orc_files_never_panic() {
    let d = dir("random");
    let path = d.join("r.orc");
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let bases: Vec<Vec<u8>> = [
        "orc-none.orc",
        "orc-zlib.orc",
        "orc-v011-zlib.orc",
        "orc-snappy.orc",
        "orc-lz4.orc",
        "orc-zstd.orc",
    ]
    .iter()
    .map(|n| std::fs::read(fixture(n)).unwrap())
    .collect();
    let all: Vec<usize> = (0..20).collect();
    for i in 0..20_000 {
        let mut b = bases[i % bases.len()].clone();
        // Half the cases damage the tail, where the lengths and counts live.
        let tail_only = next() % 2 == 0;
        for _ in 0..(next() % 6 + 1) {
            let span = if tail_only { 600.min(b.len()) } else { b.len() };
            let at = b.len() - 1 - (next() % span as u64) as usize;
            b[at] = next() as u8;
        }
        if next() % 5 == 0 {
            b.truncate((next() % b.len() as u64) as usize);
        }
        std::fs::write(&path, &b).unwrap();
        let started = Instant::now();
        if let Ok(s) = inspect(&path, &Limits::default()) {
            let columns: Vec<usize> = all
                .iter()
                .copied()
                .filter(|&c| c < s.fields.len())
                .collect();
            let _ = read(&path, &columns, 0, 400);
            let _ = read(&path, &columns, next() % 300, 30);
        }
        assert!(started.elapsed().as_secs() < 5, "case {i} took too long");
    }
}

struct Summary {
    rows: u64,
    fields: Vec<(String, String)>,
    key_value: Vec<(String, String)>,
    schema: String,
    row_groups: Vec<Group>,
}
struct Group {
    rows: u64,
    compressed: u64,
}
fn inspect(path: &Path, _limits: &Limits) -> Result<Summary, String> {
    let source = Orc::open(path)?;
    let schema = source.schema;
    Ok(Summary {
        rows: schema.rows.unwrap(),
        fields: schema
            .columns
            .into_iter()
            .map(|c| (c.name, c.type_name))
            .collect(),
        key_value: schema.details,
        schema: schema.text.unwrap(),
        row_groups: source
            .tail
            .footer
            .stripes
            .iter()
            .map(|s| Group {
                rows: s.rows,
                compressed: s.size(),
            })
            .collect(),
    })
}

#[test]
fn long_cells_null_root_rows_and_empty_requests() {
    let d = dir("limits");
    let path = d.join("x.orc");
    let text = "é🙂".repeat(1000);
    let n = text.len() as u16;
    let lengths = vec![0x08, (n >> 8) as u8, n as u8]; // SHORT_REPEAT, two-byte length, three rows.
    let types = [ty(STRUCT, &[1], &["text"]), ty(STRING, &[], &[])];
    let data = orc_file(
        &types,
        &[(DATA, 1, text.repeat(3).into_bytes()), (LENGTH, 1, lengths)],
        3,
        None,
        0,
    );
    std::fs::write(&path, &data).unwrap();
    let mut source = open(&path, None).unwrap();
    assert_eq!(
        source.rows(0, 3).unwrap(),
        vec![vec![Some(super::super::cut(text))]; 3]
    );
    assert!(source.rows(1, 0).unwrap().is_empty());
    assert!(source.rows(u64::MAX, 10).unwrap().is_empty());
    assert!(open(&path, Some("missing")).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), data);
    // Null root structs do not consume their children's values.
    let types = [ty(STRUCT, &[1], &["n"]), ty(INT, &[], &[])];
    let data = orc_file(
        &types,
        &[
            (PRESENT, 0, vec![0xff, 0b1010_0000]),
            (DATA, 1, vec![0x00, 0x0a]),
        ],
        3,
        None,
        0,
    );
    let rows = page_bytes(&path, &data, &[0]).unwrap();
    assert_eq!(
        rows,
        vec![vec![Some("5".into())], vec![None], vec![Some("5".into())]]
    );
}

#[test]
fn malformed_varints_and_stream_layouts_are_rejected() {
    let mut overflow = vec![0xff; 9];
    overflow.push(0x7f);
    assert!(Proto::new(&overflow).varint().is_err());
    assert!(Input::new(overflow).varint().is_err());
    let mut decimal_overflow = vec![0xff; 18];
    decimal_overflow.push(0x7f);
    assert!(decimal_varint(&mut Input::new(decimal_overflow)).is_err());
    assert!(Proto::new(&[0, 0]).field().is_err());
    let d = dir("streams");
    let path = d.join("x.orc");
    let types = [ty(STRUCT, &[1], &["n"]), ty(INT, &[], &[])];
    let duplicate = orc_file(
        &types,
        &[(DATA, 1, vec![0x00, 0x02]), (DATA, 1, vec![0x00, 0x04])],
        3,
        None,
        0,
    );
    assert!(
        page_bytes(&path, &duplicate, &[0])
            .unwrap_err()
            .contains("duplicate")
    );
    let unknown = orc_file(&types, &[(DATA, 99, vec![0x00, 0x02])], 3, None, 0);
    assert!(
        page_bytes(&path, &unknown, &[0])
            .unwrap_err()
            .contains("does not exist")
    );
}

#[test]
fn orc_signature_needs_its_extension_and_metadata_is_lazy() {
    let path = fixture("orc-zlib.orc");
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(crate::detect::detect("data.orc", &bytes).format.id, "orc");
    assert_ne!(crate::detect::detect("data.bin", &bytes).format.id, "orc");
    assert_ne!(
        crate::detect::detect("notes.txt", b"ORChard notes")
            .format
            .id,
        "orc"
    );
    let d = dir("lazy");
    let path = d.join("x.orc");
    let data = orc_file(
        &[ty(STRUCT, &[1], &["n"]), ty(INT, &[], &[])],
        &[],
        2_000_000,
        None,
        0,
    );
    std::fs::write(&path, data).unwrap();
    let mut source = open(&path, None).unwrap();
    assert_eq!(source.schema().rows, Some(2_000_000));
    assert!(source.rows(100, 0).unwrap().is_empty());
    assert!(
        source
            .rows(0, 2_000_000)
            .unwrap_err()
            .contains("1,000,000 cells")
    );
    assert!(
        source
            .rows(0, 1)
            .unwrap_err()
            .contains("ends before its values")
    );
}
