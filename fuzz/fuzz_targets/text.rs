//! The other text readers on arbitrary input: CSV, the formatter, Markdown, notebooks, BibTeX.
#![no_main]

use std::sync::atomic::AtomicBool;

use isolinear_formats::{bibtex, csv, markdown, notebook, pretty};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let d = csv::sniff(data, None);
    let t = csv::Table::index(data, d, &AtomicBool::new(false));
    for r in 0..t.rows().min(50) {
        for f in t.fields(data, Some(r)).unwrap_or_default() {
            let _ = csv::cell(data, &f, &d);
        }
    }
    for format in ["json", "jsonl", "xml"] {
        if let Some(out) = pretty::format(format, &text, &|| false) {
            let back = out.map.to_source(out.map.to_formatted(&out.text, text.len() / 2));
            assert!(back <= text.len());
        }
    }
    let _ = markdown::render(&text, &|_| None);
    let _ = notebook::parse(&text);
    let _ = bibtex::parse(&text);
});
