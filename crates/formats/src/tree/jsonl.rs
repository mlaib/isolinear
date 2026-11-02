//! JSON Lines: one JSON value per line, each a [`Kind::Record`]. A bad line is an error at its
//! position and the next line is still read; blank lines are skipped.

use std::sync::atomic::AtomicBool;

use super::json::{Halt, Parser};
use super::{Builder, Kind, Limits, Tree, TreeFormat};

pub const FORMAT: Option<&'static dyn TreeFormat> = Some(&JsonLines);

pub struct JsonLines;

impl TreeFormat for JsonLines {
    fn parse(&self, source: &str, limits: &Limits, cancel: &AtomicBool) -> Tree {
        let mut b = Builder::new(source.len(), limits, cancel);
        let mut start = 0usize;
        for line in source.split_inclusive('\n') {
            let line_start = start;
            start += line.len();
            let content = line.trim_end_matches(['\n', '\r']);
            if content.trim().is_empty() {
                continue;
            }
            let end = line_start + content.len();
            if b.open(Kind::Record, line_start as u32, None).is_err() {
                break;
            }
            let depth = b.depth();
            let mut p = Parser::new(source, line_start, end, &mut b);
            match p.document() {
                Ok(()) => {}
                Err(Halt::Syntax(span, message)) => b.error(span, message),
                Err(Halt::Stopped) => break,
            }
            while b.depth() >= depth {
                b.close(end as u32);
            }
        }
        b.finish(source.len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::super::dump;
    use super::*;

    fn parse(s: &str) -> Tree {
        JsonLines.parse(s, &Limits::default(), &AtomicBool::new(false))
    }

    #[test]
    fn one_record_per_line_and_bad_lines_do_not_stop_the_file() {
        let s = "{\"a\": 1}\n\n[1, 2\r\n\"ok\"\n";
        let t = parse(s);
        assert_eq!(
            dump(&t, s),
            "\
Document [3] @0..22
  Record [1] @0..8
    Object [1] @0..8
      Scalar(Number) \"a\" = 1 @1..7
  Record [1] @10..15
    Array [2] @10..15
      Scalar(Number) = 1 @11..12
      Scalar(Number) = 2 @14..15
  Record [1] @17..21
    Scalar(String) = \"ok\" @17..21
Error @15..15: expected ',' or ']', found the end
"
        );
        assert_eq!(t.stopped, None);
    }

    #[test]
    fn trailing_content_on_a_line_is_an_error() {
        let t = parse("1 2\n3\n");
        assert_eq!(t.errors(), 1);
        assert_eq!(t.node(super::super::ROOT).child_count(), 2);
    }
}
