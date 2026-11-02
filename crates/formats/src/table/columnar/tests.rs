use super::*;
use arrow_array::{
    Array, ArrayRef, BinaryArray, Decimal128Array, Int32Array, Int64Array, ListArray, RecordBatch,
    StringArray, StructArray, TimestampMicrosecondArray, types::Int32Type,
};
use arrow_schema::{DataType, Field, Schema};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
pub(crate) struct Temp {
    pub path: PathBuf,
}
impl Temp {
    pub(crate) fn new(ext: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "isolinear-columnar-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        Self {
            path: dir.join(format!("café data.{ext}")),
        }
    }
    pub(crate) fn write(&self, bytes: &[u8]) {
        std::fs::write(&self.path, bytes).unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

pub(crate) fn simple(start: i64, count: i64) -> RecordBatch {
    let schema = Arc::new(
        Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("value", DataType::Utf8, true),
        ])
        .with_metadata(std::collections::HashMap::from([(
            "origin".into(),
            "test".into(),
        )])),
    );
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(start..start + count)),
            Arc::new(StringArray::from_iter((start..start + count).map(
                |i| match i {
                    0 => None,
                    1 => Some("NULL".into()),
                    _ => Some(format!("café {i}")),
                },
            ))),
        ],
    )
    .unwrap()
}

pub(crate) fn varied() -> RecordBatch {
    let list =
        ListArray::from_iter_primitive::<Int32Type, _, _>([Some(vec![Some(1), None, Some(3)])]);
    let structure = StructArray::from(vec![(
        Arc::new(Field::new("x", DataType::Int32, true)),
        Arc::new(Int32Array::from(vec![Some(7)])) as ArrayRef,
    )]);
    let time = TimestampMicrosecondArray::from(vec![0]).with_timezone("UTC");
    let decimal = Decimal128Array::from(vec![12345])
        .with_precision_and_scale(10, 2)
        .unwrap();
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(list),
        Arc::new(structure),
        Arc::new(time),
        Arc::new(decimal),
        Arc::new(BinaryArray::from_iter_values([vec![0xab; 80]])),
        Arc::new(StringArray::from(vec!["é".repeat(5000)])),
    ];
    let fields = ["list", "record", "time", "decimal", "binary", "long"]
        .iter()
        .zip(&arrays)
        .map(|(n, a)| Field::new(*n, a.data_type().clone(), true))
        .collect::<Vec<_>>();
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).unwrap()
}

pub(crate) fn write_parquet(
    path: &Path,
    batch: &RecordBatch,
    group: usize,
    compression: parquet::basic::Compression,
) {
    let props = parquet::file::properties::WriterProperties::builder()
        .set_max_row_group_row_count(Some(group))
        .set_compression(compression)
        .set_key_value_metadata(Some(vec![parquet::file::metadata::KeyValue::new(
            "note".into(),
            "é".repeat(2500),
        )]))
        .build();
    let mut writer = parquet::arrow::ArrowWriter::try_new(
        File::create(path).unwrap(),
        batch.schema(),
        Some(props),
    )
    .unwrap();
    writer.write(batch).unwrap();
    writer.close().unwrap();
}

pub(crate) fn write_arrow(
    path: &Path,
    batches: &[RecordBatch],
    stream: bool,
    compression: Option<arrow_ipc::CompressionType>,
) {
    let schema = batches.first().unwrap().schema();
    let options = arrow_ipc::writer::IpcWriteOptions::default()
        .try_with_compression(compression)
        .unwrap();
    if stream {
        let mut w = arrow_ipc::writer::StreamWriter::try_new_with_options(
            File::create(path).unwrap(),
            &schema,
            options,
        )
        .unwrap();
        for b in batches {
            w.write(b).unwrap();
        }
        w.finish().unwrap();
    } else {
        let mut w = arrow_ipc::writer::FileWriter::try_new_with_options(
            File::create(path).unwrap(),
            &schema,
            options,
        )
        .unwrap();
        for b in batches {
            w.write(b).unwrap();
        }
        w.finish().unwrap();
    }
}

pub(crate) struct Rng(pub u64);
impl Rng {
    pub(crate) fn n(&mut self, max: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % max as u64) as usize
    }
    pub(crate) fn mutate(&mut self, seeds: &[Vec<u8>], round: usize) -> Vec<u8> {
        let mut bytes = if round.is_multiple_of(4) {
            (0..self.n(128)).map(|_| self.n(256) as u8).collect()
        } else {
            seeds[self.n(seeds.len())].clone()
        };
        for _ in 0..1 + self.n(6) {
            if bytes.is_empty() {
                break;
            }
            let at = self.n(bytes.len());
            if self.n(8) == 0 {
                bytes.truncate(at);
            } else {
                bytes[at] = self.n(256) as u8;
            }
        }
        bytes
    }
}

#[test]
fn formatted_values_types_nulls_and_limits() {
    let b = varied();
    let limits = Limits::default();
    let s = schema(Path::new("data"), &b.schema(), 1, limits).unwrap();
    assert_eq!(s.columns[0].type_name, "list<int32>");
    assert_eq!(s.columns[1].type_name, "struct<x: int32>");
    assert_eq!(s.columns[2].type_name, "timestamp[us, UTC]");
    let mut out = vec![];
    append(&b, 0, 1, &mut out, &mut 0, limits).unwrap();
    assert_eq!(out[0][0].as_deref(), Some("[1, null, 3]"));
    assert!(out[0][1].as_ref().unwrap().contains("x: 7"));
    assert!(out[0][2].as_ref().unwrap().starts_with("1970-01-01"));
    assert_eq!(out[0][3].as_deref(), Some("123.45"));
    assert_eq!(out[0][4], Some(format!("{}… (80 bytes)", "ab".repeat(64))));
    assert_eq!(
        out[0][5],
        Some(format!("{}… (5000 characters)", "é".repeat(1000)))
    );
    assert!(
        append(
            &b,
            0,
            1,
            &mut vec![],
            &mut 0,
            Limits {
                decoded: 16,
                ..limits
            }
        )
        .is_err()
    );
}

#[test]
fn dictionary_nulls_and_binary_use_values() {
    use arrow_array::{DictionaryArray, Int8Array, types::Int8Type};
    let values = Arc::new(BinaryArray::from(vec![
        Some(&b"hi"[..]),
        None,
        Some(&[0xab; 80][..]),
    ]));
    let d = DictionaryArray::<Int8Type>::try_new(
        Int8Array::from(vec![Some(0), Some(1), Some(2), None]),
        values,
    )
    .unwrap();
    let schema = Arc::new(Schema::new(vec![Field::new(
        "d",
        d.data_type().clone(),
        true,
    )]));
    let b = RecordBatch::try_new(schema, vec![Arc::new(d)]).unwrap();
    let mut out = vec![];
    append(&b, 0, 4, &mut out, &mut 0, Limits::default()).unwrap();
    assert_eq!(out[0][0].as_deref(), Some("6869"));
    assert_eq!(out[1][0], None);
    assert_eq!(out[3][0], None);
    assert_eq!(out[2][0], Some(format!("{}… (80 bytes)", "ab".repeat(64))));
}

#[test]
fn null_cells_still_consume_the_page_memory_budget() {
    let b = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new("null", DataType::Null, true)])),
        vec![Arc::new(arrow_array::NullArray::new(100))],
    )
    .unwrap();
    assert!(
        append(
            &b,
            0,
            100,
            &mut vec![],
            &mut 0,
            Limits {
                decoded: 100,
                ..Limits::default()
            }
        )
        .is_err()
    );
}

#[test]
fn nested_timestamps_and_binary_use_bounded_display() {
    let timestamp =
        Arc::new(TimestampMicrosecondArray::from(vec![0]).with_timezone("UTC")) as ArrayRef;
    let binary = Arc::new(BinaryArray::from_iter_values([vec![0xcd; 80]])) as ArrayRef;
    let nested = StructArray::from(vec![
        (
            Arc::new(Field::new("time", timestamp.data_type().clone(), true)),
            timestamp,
        ),
        (Arc::new(Field::new("blob", DataType::Binary, true)), binary),
    ]);
    let s = Arc::new(Schema::new(vec![Field::new(
        "nested",
        nested.data_type().clone(),
        true,
    )]));
    let b = RecordBatch::try_new(s, vec![Arc::new(nested)]).unwrap();
    let mut out = vec![];
    append(&b, 0, 1, &mut out, &mut 0, Limits::default()).unwrap();
    let text = out[0][0].as_ref().unwrap();
    assert!(text.contains("1970-01-01"));
    assert!(text.contains("80 bytes"));
    assert!(!text.contains(&"cd".repeat(65)));
}
