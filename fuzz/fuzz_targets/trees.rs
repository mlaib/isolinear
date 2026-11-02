//! Every structure-tree parser on arbitrary text: no panic, spans inside the text, paths found.
#![no_main]

use std::sync::atomic::AtomicBool;

use isolinear_formats::tree::{self, Limits};
use libfuzzer_sys::fuzz_target;

const FORMATS: &[(&str, Option<&str>)] = &[
    ("json", None),
    ("jsonl", None),
    ("xml", None),
    ("yaml", None),
    ("toml", None),
    ("ini", None),
    ("markdown", None),
    ("tex", None),
    ("bibtex", None),
    ("code", Some("python")),
    ("code", Some("rust")),
    ("code", Some("c")),
];

fuzz_target!(|data: &[u8]| {
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    let (format, language) = FORMATS[pick as usize % FORMATS.len()];
    let Some(parser) = tree::for_format(format, language) else {
        return;
    };
    let text = String::from_utf8_lossy(rest);
    let limits = Limits {
        max_depth: 64,
        max_nodes: 10_000,
        max_diagnostics: 100,
    };
    let t = parser.parse(&text, &limits, &AtomicBool::new(false));
    for id in 0..t.len() as u32 {
        let span = &t.node(id).span;
        assert!(span.start <= span.end && span.end as usize <= text.len());
    }
    let _ = t.path_at((rest.len() / 2) as u32);
    let _ = tree::dump(&t, &text);
});
