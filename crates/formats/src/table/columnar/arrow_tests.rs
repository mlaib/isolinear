use super::*;
use arrow_array::{Array, RecordBatch, StringArray};
use arrow_schema::Field;
use columnar::tests::*;

#[test]
fn files_streams_and_feather_have_schema_metadata_and_pages() {
    for stream in [false, true] {
        let t = Temp::new(if stream { "ipc" } else { "feather" });
        write_arrow(
            &t.path,
            &[simple(0, 4), simple(4, 4), simple(8, 4)],
            stream,
            None,
        );
        let before = std::fs::read(&t.path).unwrap();
        let mut s = Arrow::open(&t.path, Limits::default()).unwrap();
        assert_eq!(s.schema.rows, Some(12));
        assert_eq!(s.schema.columns[0].type_name, "int64");
        assert!(s.schema.details.contains(&("Batches".into(), "3".into())));
        assert!(s.schema.details.contains(&("origin".into(), "test".into())));
        assert_eq!(s.rows(0, 1).unwrap()[0], [Some("0".into()), None]);
        let p = s.rows(3, 6).unwrap();
        assert_eq!(p.len(), 6);
        assert_eq!(p[0][1].as_deref(), Some("café 3"));
        assert_eq!(p[5][0].as_deref(), Some("8"));
        assert_eq!(s.rows(10, 100).unwrap().len(), 2);
        assert!(s.rows(12, 1).unwrap().is_empty());
        assert!(s.rows(u64::MAX, 1).unwrap().is_empty());
        assert!(s.rows(0, 0).unwrap().is_empty());
        assert_eq!(std::fs::read(&t.path).unwrap(), before);
    }
}

#[test]
fn nested_and_compressed_buffers() {
    for stream in [false, true] {
        for codec in [
            None,
            Some(arrow_ipc::CompressionType::LZ4_FRAME),
            Some(arrow_ipc::CompressionType::ZSTD),
        ] {
            let t = Temp::new("arrow");
            write_arrow(&t.path, &[varied()], stream, codec);
            let mut s = Arrow::open(&t.path, Limits::default()).unwrap();
            let p = s.rows(0, 1).unwrap();
            assert_eq!(p[0][0].as_deref(), Some("[1, null, 3]"));
            assert_eq!(p[0][3].as_deref(), Some("123.45"));
            assert!(p[0][4].as_ref().unwrap().ends_with("80 bytes)"));
            assert!(p[0][5].as_ref().unwrap().contains("5000 characters"));
        }
    }
}

#[test]
fn dictionaries_are_shown_as_values_in_files_and_streams() {
    use arrow_array::{DictionaryArray, Int8Array, types::Int8Type};
    let d = DictionaryArray::<Int8Type>::try_new(
        Int8Array::from(vec![Some(1), None, Some(0), Some(2)]),
        Arc::new(StringArray::from(vec![Some("red"), Some("blue"), None])),
    )
    .unwrap();
    let schema = Arc::new(ArrowSchema::new(vec![Field::new(
        "color",
        d.data_type().clone(),
        true,
    )]));
    let batch = RecordBatch::try_new(schema, vec![Arc::new(d)]).unwrap();
    for stream in [false, true] {
        let t = Temp::new("arrow");
        write_arrow(
            &t.path,
            &[batch.clone(), batch.clone()],
            stream,
            Some(arrow_ipc::CompressionType::ZSTD),
        );
        let mut s = Arrow::open(&t.path, Limits::default()).unwrap();
        let p = s.rows(4, 4).unwrap();
        assert_eq!(
            p,
            vec![
                vec![Some("blue".into())],
                vec![None],
                vec![Some("red".into())],
                vec![None]
            ]
        );
    }
}

#[test]
fn stream_dictionary_replacements_before_deep_pages_are_applied() {
    use arrow_array::{DictionaryArray, Int8Array, types::Int8Type};
    let make = |value: &str| {
        let d = DictionaryArray::<Int8Type>::try_new(
            Int8Array::from(vec![0]),
            Arc::new(StringArray::from(vec![value])),
        )
        .unwrap();
        let schema = Arc::new(ArrowSchema::new(vec![Field::new(
            "color",
            d.data_type().clone(),
            true,
        )]));
        RecordBatch::try_new(schema, vec![Arc::new(d)]).unwrap()
    };
    let t = Temp::new("ipc");
    write_arrow(&t.path, &[make("red"), make("blue")], true, None);
    let mut s = Arrow::open(&t.path, Limits::default()).unwrap();
    assert_eq!(s.rows(1, 1).unwrap()[0][0].as_deref(), Some("blue"));
    assert_eq!(s.rows(0, 1).unwrap()[0][0].as_deref(), Some("red"));
}

#[test]
fn unrequested_batches_are_not_decoded() {
    for stream in [false, true] {
        let t = Temp::new("arrow");
        write_arrow(
            &t.path,
            &[simple(0, 4), simple(4, 4)],
            stream,
            Some(arrow_ipc::CompressionType::ZSTD),
        );
        let s = Arrow::open(&t.path, Limits::default()).unwrap();
        let entry = s.entries.iter().find(|e| e.rows.is_some()).unwrap();
        let body = entry.block.offset() as usize + entry.header.len();
        let len = entry.block.bodyLength() as usize;
        let mut bytes = std::fs::read(&t.path).unwrap();
        bytes[body..body + len].fill(0xff);
        drop(s);
        t.write(&bytes);
        let mut s = Arrow::open(&t.path, Limits::default()).unwrap();
        assert_eq!(s.rows(5, 1).unwrap()[0][0].as_deref(), Some("5"));
        assert!(s.rows(0, 1).is_err());
    }
}

#[test]
fn decompression_lengths_are_checked_before_allocation() {
    let t = Temp::new("arrow");
    write_arrow(
        &t.path,
        &[simple(0, 4)],
        false,
        Some(arrow_ipc::CompressionType::ZSTD),
    );
    let s = Arrow::open(&t.path, Limits::default()).unwrap();
    let entry = &s.entries[0];
    let message = arrow_ipc::root_as_message(&entry.header[entry.prefix..]).unwrap();
    let buffer = message
        .header_as_record_batch()
        .unwrap()
        .buffers()
        .unwrap()
        .iter()
        .find(|b| b.length() > 0)
        .unwrap();
    let at = entry.block.offset() as usize + entry.header.len() + buffer.offset() as usize;
    let mut bytes = std::fs::read(&t.path).unwrap();
    bytes[at..at + 8].copy_from_slice(&i64::MAX.to_le_bytes());
    drop(s);
    t.write(&bytes);
    let mut s = Arrow::open(&t.path, Limits::default()).unwrap();
    assert!(s.rows(0, 1).unwrap_err().contains("budget"));
}

#[test]
fn limits_and_damaged_offsets_fail_cleanly() {
    let t = Temp::new("arrow");
    write_arrow(&t.path, &[simple(0, 4), simple(4, 4)], false, None);
    let good = std::fs::read(&t.path).unwrap();
    for limits in [
        Limits {
            metadata: 4,
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
        assert!(Arrow::open(&t.path, limits).is_err());
    }
    let mut s = Arrow::open(
        &t.path,
        Limits {
            decoded: 8,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(s.rows(0, 1).is_err());
    assert!(s.rows(0, usize::MAX).is_err());
    for len in [0, 4, 8, good.len() - 1, good.len() - 20] {
        t.write(&good[..len]);
        assert!(open(&t.path, None).is_err());
    }
    let mut huge = good.clone();
    let at = huge.len() - 10;
    huge[at..at + 4].copy_from_slice(&i32::MAX.to_le_bytes());
    t.write(&huge);
    assert!(open(&t.path, None).is_err());
    let footer_len =
        i32::from_le_bytes(good[good.len() - 10..good.len() - 6].try_into().unwrap()) as usize;
    let at = good.len() - 10 - footer_len;
    let footer = arrow_ipc::root_as_footer(&good[at..good.len() - 10]).unwrap();
    let block = footer.recordBatches().unwrap().get(0);
    let block_offset = (block as *const Block as usize) - good.as_ptr() as usize;
    let mut bad = good.clone();
    bad[block_offset..block_offset + 8].copy_from_slice(&i64::MAX.to_le_bytes());
    t.write(&bad);
    assert!(open(&t.path, None).is_err());
}

#[test]
fn empty_batches_and_empty_files() {
    for stream in [false, true] {
        let t = Temp::new("arrow");
        write_arrow(
            &t.path,
            &[simple(0, 0), simple(0, 2), simple(2, 0)],
            stream,
            None,
        );
        let mut s = Arrow::open(&t.path, Limits::default()).unwrap();
        assert_eq!(s.rows(0, 10).unwrap().len(), 2);
        write_arrow(&t.path, &[simple(0, 0)], stream, None);
        let mut s = Arrow::open(&t.path, Limits::default()).unwrap();
        assert_eq!(s.schema.rows, Some(0));
        assert!(s.rows(0, 1).unwrap().is_empty());
    }
}

#[test]
fn total_decoded_budget_covers_all_selected_batches() {
    let t = Temp::new("arrow");
    write_arrow(&t.path, &[simple(0, 4), simple(4, 4)], false, None);
    let mut s = Arrow::open(&t.path, Limits::default()).unwrap();
    let body = s.entries[0].block.bodyLength() as usize;
    s.limits.decoded = body + body / 2;
    assert!(s.rows(0, 8).is_err());
}

#[test]
fn twenty_thousand_mutated_files_and_streams_never_panic() {
    let t = Temp::new("arrow");
    let mut seeds = vec![];
    for stream in [false, true] {
        for compression in [None, Some(arrow_ipc::CompressionType::ZSTD)] {
            write_arrow(&t.path, &[simple(0, 4), simple(4, 4)], stream, compression);
            seeds.push(std::fs::read(&t.path).unwrap());
        }
    }
    let mut rng = Rng(0xd0eabc1234567890);
    let limits = Limits {
        metadata: 64 << 10,
        columns: 64,
        batches: 64,
        decoded: 1 << 20,
        ..Limits::default()
    };
    for round in 0..20_000 {
        t.write(&rng.mutate(&seeds, round));
        if let Ok(mut s) = guard(|| Arrow::open(&t.path, limits)) {
            let _ = s.rows(0, 8);
        }
    }
}

#[test]
fn stream_dictionary_deltas_are_applied() {
    use arrow_array::{DictionaryArray, Int8Array, types::Int8Type};
    use arrow_ipc::writer::{DictionaryHandling, IpcWriteOptions, StreamWriter};
    let make = |values: Vec<&str>, key| {
        let d = DictionaryArray::<Int8Type>::try_new(
            Int8Array::from(vec![key]),
            Arc::new(StringArray::from(values)),
        )
        .unwrap();
        let schema = Arc::new(ArrowSchema::new(vec![Field::new(
            "color",
            d.data_type().clone(),
            true,
        )]));
        RecordBatch::try_new(schema, vec![Arc::new(d)]).unwrap()
    };
    let first = make(vec!["red"], 0);
    let second = make(vec!["red", "blue"], 1);
    let t = Temp::new("ipc");
    let options = IpcWriteOptions::default().with_dictionary_handling(DictionaryHandling::Delta);
    let mut writer = StreamWriter::try_new_with_options(
        File::create(&t.path).unwrap(),
        &first.schema(),
        options,
    )
    .unwrap();
    writer.write(&first).unwrap();
    writer.write(&second).unwrap();
    writer.finish().unwrap();
    let mut s = Arrow::open(&t.path, Limits::default()).unwrap();
    assert_eq!(s.rows(1, 1).unwrap()[0][0].as_deref(), Some("blue"));
    assert_eq!(s.rows(0, 2).unwrap()[0][0].as_deref(), Some("red"));
}
