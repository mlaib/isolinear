//! HDF5 files written by the reference implementation: h5py 3.15.1 (HDF5 1.14.6) with numpy
//! 2.3.4, run as `python -I make_fixtures.py tests/fixtures`. Expected values are what h5py reads
//! back. The script:
//!
//! ```python
//! """Write the HDF5 fixtures of crates/formats/tests/hdf5.rs (h5py 3.15.1, HDF5 1.14.6, numpy 2.3.4).
//! Usage: python -I make_fixtures.py <tests/fixtures directory>"""
//! import os
//! import sys
//! import zlib
//!
//! import h5py
//! import numpy as np
//!
//! out = sys.argv[1]
//!
//!
//! def low_level(group, name, data, chunk=None, compact=False, early=False):
//!     """A dataset with a layout h5py's high-level API does not offer (compact, implicit index)."""
//!     dcpl = h5py.h5p.create(h5py.h5p.DATASET_CREATE)
//!     if compact:
//!         dcpl.set_layout(h5py.h5d.COMPACT)
//!     if chunk:
//!         dcpl.set_chunk(chunk)
//!     if early:
//!         dcpl.set_alloc_time(h5py.h5d.ALLOC_TIME_EARLY)
//!     space = h5py.h5s.create_simple(data.shape)
//!     tid = h5py.h5t.py_create(data.dtype)
//!     dsid = h5py.h5d.create(group.id, name.encode(), tid, space, dcpl=dcpl)
//!     h5py.Dataset(dsid)[...] = data
//!
//!
//! def fill(f, latest):
//!     f.attrs["title"] = "Isolinear fixture"
//!     f.attrs["version"] = np.int32(3)
//!     f.attrs["scale"] = np.array([1.5, 2.5])
//!     m = f.create_group("measurements")
//!     m.attrs["site"] = "Belval"
//!     m.attrs["created"] = np.bytes_("2026-10-31")
//!     t = m.create_dataset("temperature", data=np.arange(300, dtype="<f8").reshape(100, 3) * 0.5)
//!     t.attrs["units"] = "K"
//!     m.create_dataset(
//!         "counts", data=np.arange(5000, dtype="<i4") * 7 - 1000, chunks=(512,),
//!         compression="gzip", shuffle=True,
//!     )
//!     grid = (np.arange(50)[:, None] * 10 + np.arange(7)[None, :]).astype("<f4") + 0.25
//!     m.create_dataset("grid", data=grid, chunks=(16, 4), compression="gzip")
//!     leaf = m.create_group("deep/er")
//!     leaf.create_dataset("leaf", data=np.arange(-5, 5, dtype="<i2"))
//!     leaf["up"] = m  # hard link back to an ancestor: a cycle
//!     low_level(f, "compact", np.array([7, 8, 9], dtype="<i4"), compact=True)
//!
//!     ty = f.create_group("types")
//!     ty["i8"] = np.array([-128, 0, 127], dtype="i1")
//!     ty["u8"] = np.array([0, 200, 255], dtype="u1")
//!     ty["i16"] = np.array([-32768, 1, 32767], dtype="<i2")
//!     ty["u16"] = np.array([0, 1, 65535], dtype="<u2")
//!     ty["i32"] = np.array([-2147483648, 2, 2147483647], dtype="<i4")
//!     ty["u32"] = np.array([0, 3, 4294967295], dtype="<u4")
//!     ty["i64"] = np.array([-(2**62), 4, 2**63 - 1], dtype="<i8")
//!     ty["u64"] = np.array([0, 5, 2**63 + 5], dtype="<u8")
//!     ty["f16"] = np.array([0.5, -2.0, 65504.0], dtype="<f2")
//!     ty["f32"] = np.array([0.25, -1.5, 3.0e38], dtype="<f4")
//!     ty["f64"] = np.array([0.1, -2.5, 1.0e300], dtype="<f8")
//!     ty["be_i32"] = np.array([1, -2, 300000], dtype=">i4")
//!     ty["be_f64"] = np.array([1.5, -2.25, 1.0e300], dtype=">f8")
//!     ty["bool"] = np.array([True, False, True])
//!     ty["fixed_ascii"] = np.array([b"alpha", b"beta", b"gamma"], dtype="S8")
//!     utf8 = ty.create_dataset("fixed_utf8", (2,), dtype=h5py.string_dtype("utf-8", 12))
//!     raw = np.array(["naïve".encode(), "Ωmega".encode()], dtype="S12")
//!     utf8.id.write(h5py.h5s.ALL, h5py.h5s.ALL, raw, mtype=utf8.id.get_type())
//!     ty.create_dataset(
//!         "vlen_utf8", data=["short", "a longer string with ünïcödé", ""], dtype=h5py.string_dtype()
//!     )
//!     point = np.dtype([("id", "<i4"), ("x", "<f8"), ("name", "S6"), ("pos", [("lat", "<f4"), ("lon", "<f4")])])
//!     rec = np.zeros(20, dtype=point)
//!     rec["id"] = np.arange(20) * 3
//!     rec["x"] = np.arange(20) / 4
//!     rec["name"] = [f"p{i}".encode() for i in range(20)]
//!     rec["pos"]["lat"] = 49.5
//!     rec["pos"]["lon"] = np.arange(20, dtype="f4")
//!     ty["compound"] = rec
//!     color = h5py.enum_dtype({"RED": 0, "GREEN": 1, "BLUE": 2}, basetype="u1")
//!     ty.create_dataset("enum", data=np.array([2, 0, 1, 2], dtype="u1"), dtype=color)
//!     arr = ty.create_dataset("array", (2,), dtype=np.dtype(("<i2", (3,))))
//!     arr[0] = [1, 2, 3]
//!     arr[1] = [4, 5, 6]
//!     ty["scalar"] = 42.5
//!     ty.create_dataset("answer", data="forty-two", dtype=h5py.string_dtype())
//!     ty["cube"] = np.arange(24, dtype="i1").reshape(4, 3, 2)
//!     ty["opaque"] = np.array([b"\x01\x02\x03\x04", b"\xff\x00\xff\x00"], dtype="V4")
//!     refs = ty.create_dataset("refs", (2,), dtype=h5py.ref_dtype)
//!     refs[0] = f["measurements"].ref
//!
//!     many = f.create_group("many")
//!     for i in range(12):
//!         many[f"item{i:02}"] = np.array([i, i * 10], dtype="<i8")
//!     for i in range(10):
//!         many.attrs[f"a{i}"] = np.int64(i * 100)
//!
//!     ix = f.create_group("index")
//!     ix.create_dataset("fixed", data=np.arange(1000) * 0.5, chunks=(100,), compression="gzip")
//!     if latest:
//!         # Enough chunks for a paged fixed array and for extensible-array super blocks; with
//!         # libver="earliest" these would be large version 1 B-trees.
//!         ix.create_dataset("fixed_paged", data=(np.arange(1030) % 251).astype("u1"), chunks=(1,))
//!         ix.create_dataset("extensible", data=np.arange(2000, dtype="<i8") * 3, chunks=(8,), maxshape=(None,))
//!     ix.create_dataset(
//!         "extensible_2d", data=np.arange(900, dtype="<i4").reshape(300, 3), chunks=(10, 3),
//!         maxshape=(None, 3), compression="gzip",
//!     )
//!     ix.create_dataset(
//!         "btree2", data=np.arange(800, dtype="<i2").reshape(400, 2), chunks=(2, 2),
//!         maxshape=(None, None), compression="gzip",
//!     )
//!     ix.create_dataset("single", data=np.arange(100, dtype="<u4"), chunks=(100,), compression="gzip")
//!     low_level(ix, "implicit", np.arange(95, dtype="<i4") - 50, chunk=(10,), early=True)
//!     sparse = ix.create_dataset("sparse", (100,), dtype="<f4", chunks=(10,), fillvalue=-1.0)
//!     sparse[20:30] = np.arange(10, dtype="f4")
//!
//!     f["soft"] = h5py.SoftLink("/measurements/temperature")
//!     f["external"] = h5py.ExternalLink("other.h5", "/data")
//!
//!
//! for libver in ("earliest", "latest"):
//!     with h5py.File(os.path.join(out, f"h5py-{libver}.h5"), "w", libver=libver, track_order=False) as f:
//!         fill(f, libver == "latest")
//!
//! # A 512-byte user block before the superblock.
//! fcpl = h5py.h5p.create(h5py.h5p.FILE_CREATE)
//! fcpl.set_userblock(512)
//! fid = h5py.h5f.create(os.path.join(out, "h5py-userblock.h5").encode(), h5py.h5f.ACC_TRUNC, fcpl=fcpl)
//! with h5py.File(fid) as f:
//!     f["numbers"] = np.arange(10, dtype="<i4")
//!     f.create_dataset("chunked", data=np.arange(200, dtype="<f8"), chunks=(50,), compression="gzip")
//! with open(os.path.join(out, "h5py-userblock.h5"), "r+b") as raw:
//!     raw.write(b"user block text")
//!
//! # Superblock version 2 (the 1.8 format), with a filter Isolinear cannot decode (lzf).
//! with h5py.File(os.path.join(out, "h5py-v108.h5"), "w", libver=("v108", "v108")) as f:
//!     f["plain"] = np.arange(6, dtype="<u2")
//!     f.create_dataset("lzf", data=np.arange(100, dtype="<i4"), chunks=(10,), compression="lzf")
//!     # A deflate bomb: the chunk of rows 10-19 (40 bytes) inflates to 10 MB.
//!     bomb = f.create_dataset("bomb", data=np.arange(100, dtype="<i4"), chunks=(10,), compression="gzip")
//!     bomb.id.write_direct_chunk((10,), zlib.compress(bytes(10_000_000), 9))
//! ```
use super::*;
use std::{path::PathBuf, time::Instant};
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/hdf5")
        .join(name)
}
fn inspect(path: &Path, table: Option<&str>) -> Result<Schema, String> {
    inspect_limits(path, table, &Limits::default())
}
fn inspect_limits(path: &Path, table: Option<&str>, limits: &Limits) -> Result<Schema, String> {
    Hdf5::with_limits(
        path,
        table,
        limits.clone(),
        Arc::new(AtomicBool::new(false)),
    )
    .map(|r| r.schema)
}
type Rows = Vec<Vec<Option<String>>>;
fn rows(
    path: &Path,
    table: &str,
    columns: &[usize],
    first: u64,
    count: usize,
) -> Result<Rows, String> {
    read(path, Some(table), columns, first, count, &Limits::default())
}
fn read(
    path: &Path,
    table: Option<&str>,
    columns: &[usize],
    first: u64,
    count: usize,
    limits: &Limits,
) -> Result<Rows, String> {
    Hdf5::with_limits(
        path,
        table,
        limits.clone(),
        Arc::new(AtomicBool::new(false)),
    )?
    .page(columns, first, count)
}
fn fields(s: &Schema) -> Vec<(String, String)> {
    s.columns
        .iter()
        .map(|c| (c.name.clone(), c.type_name.clone()))
        .collect()
}
fn chosen(s: &Schema) -> Option<&str> {
    s.tables.get(s.table).map(|t| t.name.as_str())
}
fn description<'a>(s: &'a Schema, name: &str) -> &'a str {
    kv(s, &format!("Dataset {name}")).unwrap()
}
/// One column of a page, as text.
fn column(path: &Path, table: &str, c: usize, first: u64, count: usize) -> Vec<String> {
    rows(path, table, &[c], first, count)
        .unwrap_or_else(|e| panic!("{table}: {e}"))
        .into_iter()
        .map(|r| r[0].clone().unwrap())
        .collect()
}
fn kv<'a>(s: &'a Schema, key: &str) -> Option<&'a str> {
    s.details
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("isolinear-hdf5-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn signature_is_recognised_at_the_start_or_after_a_user_block() {
    assert!(inspect(&fixture("h5py-earliest.h5"), None).is_ok());
    assert!(inspect(&fixture("h5py-userblock.h5"), None).is_ok());
    let not_hdf5 = scratch("not-hdf5");
    std::fs::write(&not_hdf5, b"SQLite format 3\0").unwrap();
    assert!(inspect(&not_hdf5, None).is_err());
}

#[test]
fn datasets_are_listed_with_kinds_rows_and_openability() {
    for (name, extra) in [("h5py-earliest.h5", 0), ("h5py-latest.h5", 2)] {
        let path = fixture(name);
        let s = inspect(&path, None).unwrap();
        assert_eq!(chosen(&s), Some("/compact"), "{name}");
        assert_eq!(s.rows, Some(3));
        assert_eq!(fields(&s), [("value".to_owned(), "int32".to_owned())]);
        assert_eq!(s.tables.len(), 48 + extra, "{name}");
        assert!(
            s.tables
                .iter()
                .all(|t| inspect(&path, Some(&t.name)).is_ok()),
            "{name}"
        );
        let names: Vec<&str> = s.tables.iter().map(|t| t.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "name order");
        let find = |n: &str| s.tables.iter().find(|t| t.name == n).unwrap();
        assert_eq!(find("/measurements/counts").rows, Some(5000));
        assert_eq!(
            description(&s, "/measurements/counts"),
            "int32 (5000,), chunked (512,), shuffle, gzip level 4"
        );
        assert_eq!(find("/measurements/temperature").rows, Some(100));
        assert_eq!(
            description(&s, "/measurements/temperature"),
            "float64 (100, 3), contiguous"
        );
        assert_eq!(find("/types/scalar").rows, Some(1));
        assert_eq!(find("/types/cube").rows, Some(4));
        assert_eq!(find("/measurements/deep/er/leaf").rows, Some(10));
        assert_eq!(find("/many/item11").rows, Some(2));
        assert!(description(&s, "/compact").contains("compact"));
        // The hard link back to /measurements is listed once, not followed again.
        assert!(!names.iter().any(|n| n.contains("/up/")));
        assert!(
            s.text
                .as_deref()
                .unwrap_or("")
                .contains("up → /measurements (hard link"),
            "{}",
            s.text.as_deref().unwrap_or("")
        );
        assert!(
            s.text
                .as_deref()
                .unwrap_or("")
                .contains("soft → /measurements/temperature (soft link)")
        );
        assert!(
            s.text
                .as_deref()
                .unwrap_or("")
                .contains("external → /data in other.h5 (external link, not followed)")
        );
        assert!(
            s.text
                .as_deref()
                .unwrap_or("")
                .contains("@title = Isolinear fixture")
        );
        assert!(
            s.text
                .as_deref()
                .unwrap_or("")
                .contains("@scale = [1.5, 2.5]")
        );
        assert!(
            s.text.as_deref().unwrap_or("").contains("@a9 = 900"),
            "dense attributes in the latest format"
        );
        assert_eq!(
            kv(&s, "Objects").unwrap().split(' ').next(),
            Some(&*format!("{}", 48 + extra + 7))
        );
        assert!(kv(&s, "Warning").is_none(), "{:?}", s.details);
    }
    let s = inspect(&fixture("h5py-earliest.h5"), None).unwrap();
    assert_eq!(kv(&s, "Format"), Some("HDF5, superblock version 0"));
    assert_eq!(kv(&s, "Format"), Some("HDF5, superblock version 0"));
    let s = inspect(&fixture("h5py-latest.h5"), None).unwrap();
    assert_eq!(kv(&s, "Format"), Some("HDF5, superblock version 3"));
}

#[test]
fn values_match_what_h5py_reads() {
    for name in ["h5py-earliest.h5", "h5py-latest.h5"] {
        let path = fixture(name);
        let p = path.as_path();
        // Chunked, shuffled and deflated: rows across chunk boundaries and a deep page.
        assert_eq!(
            column(p, "/measurements/counts", 0, 510, 4),
            ["2570", "2577", "2584", "2591"]
        );
        let deep = column(p, "/measurements/counts", 0, 4990, 50);
        assert_eq!(deep.len(), 10);
        assert_eq!(deep[9], "33993");
        assert_eq!(column(p, "/measurements/counts", 0, 0, 1), ["-1000"]);
        // 2-D chunked with partial edge chunks, and column projection.
        let s = inspect(p, Some("/measurements/grid")).unwrap();
        assert_eq!(s.rows, Some(50));
        assert_eq!(fields(&s).len(), 7);
        assert_eq!(fields(&s)[0], ("0".to_owned(), "float32".to_owned()));
        let page = rows(p, "/measurements/grid", &[2, 5], 14, 4).unwrap();
        let expect: Rows = (14..18)
            .map(|r| {
                [2, 5]
                    .iter()
                    .map(|c| Some(format!("{}", f64::from(r * 10 + c) + 0.25)))
                    .collect()
            })
            .collect();
        assert_eq!(page, expect);
        assert_eq!(column(p, "/measurements/grid", 6, 49, 5), ["496.25"]);
        // Contiguous 2-D.
        assert_eq!(
            rows(p, "/measurements/temperature", &[0, 1, 2], 99, 1).unwrap(),
            [[
                Some("148.5".into()),
                Some("149.0".into()),
                Some("149.5".into())
            ]]
        );
        assert_eq!(
            column(p, "/measurements/deep/er/leaf", 0, 0, 3),
            ["-5", "-4", "-3"]
        );
        assert_eq!(column(p, "/compact", 0, 0, 10), ["7", "8", "9"]);
        // Every integer and float size, both byte orders.
        let t = |n: &str| column(p, &format!("/types/{n}"), 0, 0, 10);
        assert_eq!(t("i8"), ["-128", "0", "127"]);
        assert_eq!(t("u8"), ["0", "200", "255"]);
        assert_eq!(t("i16"), ["-32768", "1", "32767"]);
        assert_eq!(t("u16"), ["0", "1", "65535"]);
        assert_eq!(t("i32"), ["-2147483648", "2", "2147483647"]);
        assert_eq!(t("u32"), ["0", "3", "4294967295"]);
        assert_eq!(
            t("i64"),
            ["-4611686018427387904", "4", "9223372036854775807"]
        );
        assert_eq!(t("u64"), ["0", "5", "9223372036854775813"]);
        assert_eq!(t("f16"), ["0.5", "-2.0", "65504.0"]);
        assert_eq!(t("f32"), ["0.25", "-1.5", "3e38"]);
        assert_eq!(t("f64"), ["0.1", "-2.5", "1e300"]);
        assert_eq!(t("be_i32"), ["1", "-2", "300000"]);
        assert_eq!(t("be_f64"), ["1.5", "-2.25", "1e300"]);
        assert_eq!(t("bool"), ["TRUE", "FALSE", "TRUE"]);
        assert_eq!(t("enum"), ["BLUE", "RED", "GREEN", "BLUE"]);
        assert_eq!(t("fixed_ascii"), ["alpha", "beta", "gamma"]);
        assert_eq!(t("fixed_utf8"), ["naïve", "Ωmega"]);
        assert_eq!(
            t("vlen_utf8"),
            ["short", "a longer string with ünïcödé", ""]
        );
        assert_eq!(t("array"), ["[1, 2, 3]", "[4, 5, 6]"]);
        assert_eq!(t("scalar"), ["42.5"]);
        assert_eq!(t("answer"), ["forty-two"]);
        assert_eq!(t("opaque"), ["0x01020304", "0xff00ff00"]);
        assert_eq!(t("refs"), ["/measurements", "(null reference)"]);
        // Compound: one column per field, nested compound as text.
        let s = inspect(p, Some("/types/compound")).unwrap();
        let names: Vec<String> = fields(&s).into_iter().map(|f| f.0).collect();
        assert_eq!(names, ["id", "x", "name", "pos"]);
        assert_eq!(
            rows(p, "/types/compound", &[0, 1, 2, 3], 19, 5).unwrap(),
            [[
                Some("57".into()),
                Some("4.75".into()),
                Some("p19".into()),
                Some("{lat: 49.5, lon: 19}".into())
            ]]
        );
        // Three dimensions flatten into columns.
        let s = inspect(p, Some("/types/cube")).unwrap();
        let names: Vec<String> = fields(&s).into_iter().map(|f| f.0).collect();
        assert_eq!(names, ["0,0", "0,1", "1,0", "1,1", "2,0", "2,1"]);
        let row: Vec<String> = rows(p, "/types/cube", &[0, 1, 2, 3, 4, 5], 1, 1).unwrap()[0]
            .iter()
            .map(|v| v.clone().unwrap())
            .collect();
        assert_eq!(row, ["6", "7", "8", "9", "10", "11"]);
        assert_eq!(column(p, "/many/item07", 0, 0, 2), ["7", "70"]);
        // Chunk indexes of every kind (version 1 B-trees in the earliest format).
        assert_eq!(column(p, "/index/fixed", 0, 999, 1), ["499.5"]);
        assert_eq!(column(p, "/index/single", 0, 98, 5), ["98", "99"]);
        assert_eq!(column(p, "/index/implicit", 0, 0, 1), ["-50"]);
        assert_eq!(column(p, "/index/implicit", 0, 94, 1), ["44"]);
        assert_eq!(
            rows(p, "/index/btree2", &[0, 1], 201, 1).unwrap(),
            [[Some("402".into()), Some("403".into())]]
        );
        assert_eq!(column(p, "/index/btree2", 1, 399, 1), ["799"]);
        assert_eq!(column(p, "/index/extensible_2d", 2, 155, 1), ["467"]);
        assert_eq!(column(p, "/index/extensible_2d", 0, 299, 1), ["897"]);
        // Unwritten chunks hold the fill value.
        let sparse = column(p, "/index/sparse", 0, 18, 14);
        assert_eq!(
            sparse,
            [
                "-1.0", "-1.0", "0.0", "1.0", "2.0", "3.0", "4.0", "5.0", "6.0", "7.0", "8.0",
                "9.0", "-1.0", "-1.0"
            ]
        );
    }
    let path = fixture("h5py-latest.h5");
    let p = path.as_path();
    assert_eq!(column(p, "/index/fixed_paged", 0, 600, 1), ["98"]);
    assert_eq!(column(p, "/index/fixed_paged", 0, 1029, 5), ["25"]);
    assert_eq!(column(p, "/index/extensible", 0, 0, 1), ["0"]);
    assert_eq!(column(p, "/index/extensible", 0, 1000, 1), ["3000"]);
    assert_eq!(column(p, "/index/extensible", 0, 1999, 1), ["5997"]);
    let all = column(p, "/index/extensible", 0, 0, 3000);
    assert_eq!(all.len(), 2000);
    assert!(
        all.iter()
            .enumerate()
            .all(|(i, v)| *v == (i * 3).to_string())
    );
    let s = inspect(p, Some("/index/extensible")).unwrap();
    assert_eq!(kv(&s, "Chunk index"), Some("extensible array"));
    assert_eq!(kv(&s, "Maximum shape"), Some("(unlimited,)"));
    let s = inspect(p, Some("/index/fixed_paged")).unwrap();
    assert_eq!(kv(&s, "Chunk index"), Some("fixed array"));
    let s = inspect(p, Some("/index/btree2")).unwrap();
    assert_eq!(kv(&s, "Chunk index"), Some("version 2 B-tree"));
}

#[test]
fn attributes_and_properties_of_the_chosen_dataset() {
    for name in ["h5py-earliest.h5", "h5py-latest.h5"] {
        let path = fixture(name);
        let s = inspect(&path, Some("/measurements/temperature")).unwrap();
        assert_eq!(chosen(&s), Some("/measurements/temperature"));
        assert_eq!(kv(&s, "@units"), Some("K"));
        assert_eq!(kv(&s, "Datatype"), Some("float64"));
        assert_eq!(kv(&s, "Shape"), Some("(100, 3)"));
        assert_eq!(kv(&s, "Layout"), Some("contiguous"));
        let s = inspect(&path, Some("measurements/counts")).unwrap();
        assert_eq!(kv(&s, "Filters"), Some("shuffle, gzip level 4"));
        assert_eq!(kv(&s, "Layout"), Some("chunked (512,)"));
        let s = inspect(&path, Some("/index/sparse")).unwrap();
        assert_eq!(kv(&s, "Fill value"), Some("-1"));
        assert!(s.text.as_deref().unwrap_or("").contains("@site = Belval"));
        assert!(
            s.text
                .as_deref()
                .unwrap_or("")
                .contains("@created = 2026-10-31")
        );
        assert!(inspect(&path, Some("/nothing")).is_err());
        assert!(read(&path, Some("/compact"), &[1], 0, 1, &Limits::default()).is_err());
    }
}

#[test]
fn user_blocks_and_older_superblocks() {
    let path = fixture("h5py-userblock.h5");
    let s = inspect(&path, None).unwrap();
    assert_eq!(chosen(&s), Some("/chunked"));
    assert_eq!(kv(&s, "User block"), Some("512 bytes"));
    assert_eq!(
        column(&path, "/chunked", 0, 148, 3),
        ["148.0", "149.0", "150.0"]
    );
    assert_eq!(column(&path, "/numbers", 0, 9, 1), ["9"]);

    // Version 1 of the superblock adds four bytes (the chunk B-tree K); make one from the
    // version 0 file by inserting them and moving every address with the base address.
    let mut v1 = std::fs::read(fixture("h5py-earliest.h5")).unwrap();
    v1[8] = 1;
    v1.splice(24..24, [32, 0, 0, 0]);
    v1[28..36].copy_from_slice(&4u64.to_le_bytes());
    let p1 = scratch("v1.h5");
    std::fs::write(&p1, &v1).unwrap();
    let s = inspect(&p1, None).unwrap();
    assert_eq!(kv(&s, "Format"), Some("HDF5, superblock version 1"));
    assert_eq!(s.tables.len(), 48);
    assert_eq!(column(&p1, "/measurements/counts", 0, 4999, 1), ["33993"]);

    let path = fixture("h5py-v108.h5");
    let s = inspect(&path, None).unwrap();
    assert_eq!(kv(&s, "Format"), Some("HDF5, superblock version 2"));
    assert_eq!(chosen(&s), Some("/bomb"));
    let lzf = s.tables.iter().find(|t| t.name == "/lzf").unwrap();
    assert!(inspect(&path, Some(&lzf.name)).is_err());
    assert!(
        description(&s, &lzf.name).contains("lzf (32000) is not supported"),
        "{}",
        description(&s, &lzf.name)
    );
    let e = inspect(&path, Some("/lzf")).err().unwrap();
    assert!(e.contains("lzf"), "{e}");
    assert_eq!(
        column(&path, "/plain", 0, 0, 6),
        ["0", "1", "2", "3", "4", "5"]
    );
}

#[test]
fn damaged_files_fail_cleanly() {
    let limits = Limits::default();
    // A chunk whose deflate stream expands far past its size.
    let bomb = fixture("h5py-v108.h5");
    assert_eq!(column(&bomb, "/bomb", 0, 0, 10)[9], "9");
    let e = read(&bomb, Some("/bomb"), &[0], 5, 10, &limits)
        .err()
        .unwrap();
    assert!(e.contains("expands"), "{e}");
    assert_eq!(column(&bomb, "/bomb", 0, 20, 3), ["20", "21", "22"]);

    let earliest = std::fs::read(fixture("h5py-earliest.h5")).unwrap();
    let latest = std::fs::read(fixture("h5py-latest.h5")).unwrap();
    // Root object header outside the file (superblock versions 0 and 3).
    let path = scratch("outside.h5");
    let mut b = earliest.clone();
    b[64..72].copy_from_slice(&(1u64 << 40).to_le_bytes());
    std::fs::write(&path, &b).unwrap();
    let e = inspect(&path, None).err().unwrap();
    assert!(e.contains("outside the file"), "{e}");
    let mut b = latest.clone();
    b[36..44].copy_from_slice(&(latest.len() as u64 + 8).to_le_bytes());
    let path = scratch("outside3.h5");
    std::fs::write(&path, &b).unwrap();
    assert!(inspect(&path, None).is_err());

    // Truncated: what lies inside still lists, the rest becomes warnings or errors.
    for (i, cut) in [10usize, 100, 2000, 20_000, earliest.len() / 2]
        .into_iter()
        .enumerate()
    {
        let path = scratch(&format!("cut{i}.h5"));
        std::fs::write(&path, &earliest[..cut]).unwrap();
        if let Ok(s) = inspect(&path, None) {
            for t in &s.tables {
                let _ = read(&path, Some(&t.name), &[0], 0, 100, &limits);
            }
        }
    }
    let path = scratch("half.h5");
    std::fs::write(&path, &latest[..latest.len() / 2]).unwrap();
    let s = inspect(&path, None).unwrap();
    assert!(
        s.details
            .iter()
            .any(|(k, v)| k == "Warning" && v.contains("truncated")),
        "{:?}",
        s.details
    );

    // Every group B-tree node (leaves, in this file) made to point at itself.
    let mut b = earliest.clone();
    let nodes: Vec<usize> = b
        .windows(6)
        .enumerate()
        .filter(|(_, w)| *w == b"TREE\x00\x00")
        .map(|(i, _)| i)
        .collect();
    assert!(!nodes.is_empty());
    for node in nodes {
        b[node + 5] = 1;
        b[node + 32..node + 40].copy_from_slice(&(node as u64).to_le_bytes());
    }
    let path = scratch("loop.h5");
    std::fs::write(&path, &b).unwrap();
    let s = inspect(&path, None).unwrap();
    // The damaged groups become warnings; what lies outside them still lists.
    assert_eq!(chosen(&s), Some("/compact"));
    assert!(!s.tables.iter().any(|t| t.name.starts_with("/types/")));
    assert!(
        s.details
            .iter()
            .any(|(k, v)| k == "Warning" && v.contains("loops")),
        "{:?}",
        s.details
    );
    // A chunk B-tree node (rank 2: 32-byte keys) pointing at itself.
    let mut b = earliest.clone();
    let node = b
        .windows(6)
        .position(|w| w == b"TREE\x01\x01")
        .expect("an interior chunk B-tree node");
    b[node + 56..node + 64].copy_from_slice(&(node as u64).to_le_bytes());
    let path = scratch("chunkloop.h5");
    std::fs::write(&path, &b).unwrap();
    let s = inspect(&path, None).unwrap();
    let looped: Vec<String> = s
        .tables
        .iter()
        .filter_map(|t| read(&path, Some(&t.name), &[0], 0, 1000, &limits).err())
        .collect();
    assert!(looped.iter().any(|e| e.contains("loops")), "{looped:?}");

    // Absurd dimensions: /measurements/temperature (100, 3) declared as 2^60 rows, then columns.
    let dims: Vec<u8> = 100u64
        .to_le_bytes()
        .into_iter()
        .chain(3u64.to_le_bytes())
        .collect();
    let at = earliest
        .windows(24)
        .position(|w| w[..2] == [1, 2] && w[3..8] == [0; 5] && w[8..] == dims[..])
        .expect("temperature's dataspace");
    let mut b = earliest.clone();
    b[at + 8..at + 16].copy_from_slice(&(1u64 << 60).to_le_bytes());
    let path = scratch("rows.h5");
    std::fs::write(&path, &b).unwrap();
    let s = inspect(&path, Some("/measurements/temperature")).unwrap();
    assert_eq!(s.rows, Some(1 << 60));
    assert!(
        read(
            &path,
            Some("/measurements/temperature"),
            &[0],
            1 << 59,
            100,
            &limits
        )
        .is_err()
    );
    let mut b = earliest.clone();
    b[at + 16..at + 24].copy_from_slice(&(1u64 << 60).to_le_bytes());
    let path = scratch("columns.h5");
    std::fs::write(&path, &b).unwrap();
    let s = inspect(&path, Some("/measurements/temperature")).unwrap();
    assert_eq!(fields(&s).len(), limits.columns);
    assert!(kv(&s, "Warning").unwrap().contains("columns are shown"));
    assert!(
        read(
            &path,
            Some("/measurements/temperature"),
            &[0, 5],
            0,
            10,
            &limits
        )
        .is_err()
    );
}

#[test]
fn mutated_files_never_panic_or_hang() {
    const CASES: usize = 20_000;
    let limits = Limits {
        columns: 64,
        ..Limits::default()
    };
    let files = [
        std::fs::read(fixture("h5py-earliest.h5")).unwrap(),
        std::fs::read(fixture("h5py-latest.h5")).unwrap(),
        std::fs::read(fixture("h5py-userblock.h5")).unwrap(),
        std::fs::read(fixture("h5py-v108.h5")).unwrap(),
    ];
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in 0..CASES {
        let mut b = files[i % files.len()].clone();
        for _ in 0..(next() % 16 + 1) {
            let at = (next() % b.len() as u64) as usize;
            b[at] = next() as u8;
        }
        if next() % 5 == 0 {
            b.truncate((next() % b.len() as u64) as usize);
        }
        // Overwrite a single scratch file; each open owns a fresh structure.
        let path = scratch("mutated.h5");
        std::fs::write(&path, &b).unwrap();
        let started = Instant::now();
        if let Ok(mut table) = Hdf5::with_limits(
            &path,
            None,
            limits.clone(),
            Arc::new(AtomicBool::new(false)),
        ) {
            let _ = table.rows(0, 4);
            if let Some(t) = table
                .schema
                .tables
                .get(i % table.schema.tables.len().max(1))
            {
                let _ = read(&path, Some(&t.name), &[0], 300, 4, &limits);
            }
        }
        let _ = std::fs::remove_file(&path);
        assert!(started.elapsed().as_secs() < 30, "case {i} took too long");
    }
}

#[test]
fn cancellation_and_changed_files_are_reported() {
    let token = Arc::new(AtomicBool::new(true));
    let path = fixture("h5py-latest.h5");
    assert!(
        Hdf5::open(&path, None, token.clone())
            .err()
            .unwrap()
            .contains("cancelled")
    );
    token.store(false, Ordering::Relaxed);
    let mut source = Hdf5::open(&path, Some("/measurements/counts"), token.clone()).unwrap();
    token.store(true, Ordering::Relaxed);
    assert!(source.rows(0, 3).unwrap_err().contains("cancelled"));
    let copy = scratch("changed.h5");
    std::fs::write(&copy, std::fs::read(path).unwrap()).unwrap();
    let mut source = Hdf5::open(&copy, None, Arc::new(AtomicBool::new(false))).unwrap();
    std::fs::write(&copy, b"changed").unwrap();
    assert!(source.rows(0, 1).unwrap_err().contains("changed"));
    std::fs::remove_file(copy).unwrap();
}

#[test]
fn nested_datatypes_and_invalid_copy_ranges_are_bounded() {
    let mut bytes = Vec::new();
    for _ in 0..1000 {
        bytes.extend([0x39, 0, 0, 0, 16, 0, 0, 0]);
    }
    assert!(
        parse_type(&mut Cur::new(&bytes), 0)
            .err()
            .unwrap()
            .contains("deeply")
    );
    let sel = Selection {
        start: vec![u64::MAX],
        count: vec![2],
        element: 8,
    };
    assert!(copy_block(&sel, &[0], &[u64::MAX], &mut [0; 16], &mut |_, _| Ok(())).is_err());
    let mut source = Hdf5::open(
        &fixture("h5py-latest.h5"),
        Some("/measurements/counts"),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert!(source.rows(u64::MAX, usize::MAX).unwrap().is_empty());
    assert!(source.rows(0, 0).unwrap().is_empty());
}
