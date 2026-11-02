//! Binary formats shown as JSON: MessagePack (fixtures by msgpack 1.1.2) and Protobuf without a
//! schema (descriptor, Struct and Timestamp messages serialized by protobuf 6.33).
use isolinear_formats::{
    decoded::{self, msgpack, protobuf},
    detect,
    tree::{self, Limits, Severity},
};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name),
    )
    .unwrap()
}

/// The decoded text is valid JSON for Isolinear's own strict JSON tree (warnings, such as
/// integers beyond 2^53, are fine).
fn valid_json(text: &str) {
    let tree = tree::for_format("json", None).unwrap().parse(
        text,
        &Limits::default(),
        &AtomicBool::new(false),
    );
    assert!(
        tree.stopped.is_none()
            && !tree
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error),
        "{:?}\n{text}",
        tree.diagnostics
    );
}

#[test]
fn messagepack_values_become_json() {
    let bytes = fixture("doc.msgpack");
    let d = detect("doc.msgpack", &bytes);
    assert_eq!(d.format.id, "msgpack");
    assert!(!d.editable);
    let (text, note) = msgpack::to_json(&bytes, &|| false).unwrap();
    valid_json(&text);
    assert!(note.contains("read-only"), "{note}");
    for line in [
        r#""name": "Ünïcode – 日本語","#,
        "    18446744073709551615,",
        "    -9223372036854775808",
        "    -0.0,",
        r#"    "inf""#,
        r#""bytes": "<6 bytes: 0001ff616263>","#,
        r#""when": "2026-10-30T12:34:56.789000000Z","#,
        r#""custom": "<extension 5, 3 bytes: 616263>","#,
        r#""k19": 19"#,
        r#""quote": "a \"quoted\"\nline""#,
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    // Several values, non-string keys and a 32-bit float.
    let (text, note) = msgpack::to_json(&fixture("stream.msgpack"), &|| false).unwrap();
    valid_json(&text);
    assert!(note.contains("3 values"), "{note}");
    assert!(note.contains("not strings"), "{note}");
    assert!(text.contains(r#""[ 1, 2 ]": "pair""#), "{text}");
    assert!(text.contains("\n  0.25,"), "{text}");
}

#[test]
fn protobuf_fields_are_shown_by_number() {
    let bytes = fixture("descriptor.pb");
    assert_eq!(detect("descriptor.pb", &bytes).format.id, "protobuf");
    let (text, note) = protobuf::to_json(&bytes, &|| false).unwrap();
    valid_json(&text);
    assert!(note.contains("without its schema"), "{note}");
    for line in [
        r#""1": "isolinear.proto","#,
        r#""1": "Person","#,
        r#""1": "emails","#,
        r#""5": -5,"#,
        r#""6": "0x4004000000000000 (2.5 as double)","#,
        r#""7": "<8 bytes: 000162696e617279>""#,
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    // Repeated nested messages become a list; text inside a nested message stays text.
    let (text, _) = protobuf::to_json(&fixture("struct.pb"), &|| false).unwrap();
    valid_json(&text);
    assert!(text.starts_with("{\n  \"1\": [\n"), "{text}");
    assert!(text.contains(r#""3": "Ann""#), "{text}");
    assert!(text.contains("(1.5 as double)"), "{text}");
    let (text, _) = protobuf::to_json(&fixture("timestamp.binpb"), &|| false).unwrap();
    assert_eq!(text, "{\n  \"1\": 1798547696,\n  \"2\": 5\n}\n");
    // Decoded formats are shown through the JSON tree, read-only.
    assert!(decoded::supports("protobuf") && decoded::supports("msgpack"));
    assert!(decoded::to_json("json", b"{}", &|| false).is_none());
}

#[test]
fn damage_and_limits() {
    // MessagePack: 0xc1 is never used; a damaged value after good ones keeps the good ones.
    assert!(msgpack::to_json(&[0xc1], &|| false).is_err());
    let (text, note) = msgpack::to_json(&[0x01, 0x02, 0xc1], &|| false).unwrap();
    assert_eq!(text, "[\n  1,\n  2\n]\n");
    assert!(note.contains("decoding stopped"), "{note}");
    // An array claiming four billion items with nothing after it is refused before allocating.
    let e = msgpack::to_json(&[0xdd, 0xff, 0xff, 0xff, 0xff], &|| false).unwrap_err();
    assert!(e.contains("more than the bytes left"), "{e}");
    // A string longer than the file.
    assert!(msgpack::to_json(&[0xdb, 0x7f, 0xff, 0xff, 0xff, b'a'], &|| false).is_err());
    // Nesting deeper than the limit.
    let deep = vec![0x91u8; 10_000];
    let e = msgpack::to_json(&deep, &|| false).unwrap_err();
    assert!(e.contains("nest deeper"), "{e}");
    assert!(msgpack::to_json(&[], &|| false).is_err());

    // Protobuf: text is not a message; a length past the end; deeply nested messages are
    // decoded down to the guessing limit and shown as bytes below it, without overflowing.
    assert!(protobuf::to_json(b"hello, world", &|| false).is_err());
    assert!(protobuf::to_json(&[0x0a, 0x7f, 0x01], &|| false).is_err());
    let mut nested = vec![0x08, 0x01];
    for _ in 0..500 {
        let mut outer = vec![0x0a];
        let mut len = nested.len();
        while len >= 0x80 {
            outer.push((len as u8 & 0x7f) | 0x80);
            len >>= 7;
        }
        outer.push(len as u8);
        outer.extend(&nested);
        nested = outer;
    }
    let (text, _) = protobuf::to_json(&nested, &|| false).unwrap();
    valid_json(&text);
    assert!(text.contains("bytes: "), "the deepest levels stay bytes");

    // Random bytes and mutations (20,000 inputs) never panic and finish quickly.
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let bases = [
        fixture("doc.msgpack"),
        fixture("stream.msgpack"),
        fixture("descriptor.pb"),
        fixture("struct.pb"),
    ];
    for i in 0..20_000 {
        let mut b = bases[i % bases.len()].clone();
        for _ in 0..(next() % 6 + 1) {
            let at = (next() % b.len() as u64) as usize;
            b[at] = next() as u8;
        }
        if next() % 4 == 0 {
            b.truncate((next() % b.len() as u64) as usize);
        }
        let started = Instant::now();
        if let Ok((text, _)) = msgpack::to_json(&b, &|| false) {
            valid_json(&text);
        }
        if let Ok((text, _)) = protobuf::to_json(&b, &|| false) {
            valid_json(&text);
        }
        assert!(started.elapsed().as_secs() < 2, "case {i}");
    }
}
