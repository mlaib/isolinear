//! Markdown outline: headings as nested sections. A section spans from its heading to the next
//! heading of the same or a higher level; its name is the heading text.

use std::sync::atomic::AtomicBool;

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use super::{Builder, Kind, Limits, Tree, TreeFormat};

pub const FORMAT: Option<&'static dyn TreeFormat> = Some(&Markdown);

pub struct Markdown;

/// A heading being read: its level, where it starts, and the extent of its text so far.
struct Heading {
    level: HeadingLevel,
    start: usize,
    text: Option<(usize, usize)>,
}

impl TreeFormat for Markdown {
    fn parse(&self, source: &str, limits: &Limits, cancel: &AtomicBool) -> Tree {
        let mut b = Builder::new(source.len(), limits, cancel);
        // Levels of the open sections, innermost last.
        let mut open: Vec<HeadingLevel> = Vec::new();
        let mut heading: Option<Heading> = None;
        for (event, range) in Parser::new_ext(source, crate::markdown::options()).into_offset_iter()
        {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    while open.last().is_some_and(|l| *l >= level) {
                        open.pop();
                        b.close(range.start as u32);
                    }
                    heading = Some(Heading {
                        level,
                        start: range.start,
                        text: None,
                    });
                }
                Event::Text(_) | Event::Code(_) => {
                    if let Some(h) = &mut heading {
                        let t = h.text.get_or_insert((range.start, range.end));
                        t.1 = range.end;
                    }
                }
                Event::End(TagEnd::Heading(_)) => {
                    let Some(h) = heading.take() else {
                        continue;
                    };
                    let name = h.text.map(|(s, e)| s as u32..e as u32);
                    if b.open(Kind::Section, h.start as u32, name).is_err() {
                        break;
                    }
                    open.push(h.level);
                }
                _ => {}
            }
        }
        b.finish(source.trim_end().len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::super::dump;
    use super::*;

    #[test]
    fn headings_nest_by_level() {
        let s = "# A\ntext\n## B\n### C\n## D\n# E\n";
        let t = Markdown.parse(s, &Limits::default(), &AtomicBool::new(false));
        assert_eq!(
            dump(&t, s),
            "\
Document [2] @0..29
  Section A [2] @0..25
    Section B [1] @9..20
      Section C [0] @14..20
    Section D [0] @20..25
  Section E [0] @25..28
"
        );
    }

    #[test]
    fn setext_headings_and_code_spans() {
        let s = "Title `x`\n=====\n\n```\n# not a heading\n```\n";
        let t = Markdown.parse(s, &Limits::default(), &AtomicBool::new(false));
        assert_eq!(t.node(super::super::ROOT).child_count(), 1);
        let d = dump(&t, s);
        assert!(d.contains("Section Title `x`"), "{d}");
    }
}
