//! BibTeX as a tree: one node per entry (named by its key), its fields as leaves. The database's
//! diagnostics (syntax, duplicate keys and fields, missing required fields) come with it.

use std::sync::atomic::AtomicBool;

use super::{Builder, Kind, Limits, Scalar, Tree, TreeFormat};
use crate::bibtex;

pub const FORMAT: Option<&'static dyn TreeFormat> = Some(&Bibtex);

pub struct Bibtex;

impl TreeFormat for Bibtex {
    fn parse(&self, source: &str, limits: &Limits, cancel: &AtomicBool) -> Tree {
        let bib = bibtex::parse(source);
        let mut b = Builder::new(source.len(), limits, cancel);
        'entries: for e in &bib.entries {
            let name = e.key.clone().unwrap_or(e.kind_span.clone());
            let name = name.start as u32..name.end as u32;
            if b.open(Kind::Object, e.span.start as u32, Some(name))
                .is_err()
            {
                break;
            }
            let id = b.current();
            b.set_value(id, e.kind_span.start as u32..e.kind_span.end as u32, None);
            for f in &e.fields {
                let span = f.name.start as u32..f.value.end as u32;
                let name = f.name.start as u32..f.name.end as u32;
                let value = f.value.start as u32..f.value.end as u32;
                let number = source[f.value.clone()].bytes().all(|c| c.is_ascii_digit());
                let scalar = if number {
                    Scalar::Number
                } else {
                    Scalar::String
                };
                if b.leaf(Kind::Scalar, Some(scalar), span, Some(name), Some(value))
                    .is_err()
                {
                    break 'entries;
                }
            }
            b.close(e.span.end as u32);
        }
        for d in bib.diagnostics {
            b.diagnostic(d.severity, d.span, d.message);
        }
        b.finish(source.len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::super::dump;
    use super::*;

    #[test]
    fn entries_and_fields() {
        let s = "@article{k, title={T}, year=2001}\n@misc{k}\n";
        let t = Bibtex.parse(s, &Limits::default(), &AtomicBool::new(false));
        assert_eq!(
            dump(&t, s),
            "\
Document [2] @0..43
  Object k = article [2] @0..33
    Scalar(String) title = {T} @12..21
    Scalar(Number) year = 2001 @23..32
  Object k = misc [0] @34..42
Warning @9..10: @article without author, journal
Warning @40..41: duplicate key k
"
        );
    }
}
