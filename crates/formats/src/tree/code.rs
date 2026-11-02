//! Bounded, lexical outlines of source code. These scanners recognize definitions, not
//! complete language grammars: nothing is expanded, type-checked, loaded or executed.
//! Strings and comments are opaque. Braces, indentation and explicit terminators provide
//! extents; malformed files keep the definitions whose headers could be recognized.
//!
//! Limits: 32 MiB of source, one million tokens and lines, 512 tokens per declaration
//! header, and the configured nesting/node/diagnostic limits. Scans poll cancellation
//! even inside long comments and strings. Unsupported languages have no outline.

use std::sync::atomic::{AtomicBool, Ordering};

use super::{Builder, Kind, Limits, Span, Stop, Tree, TreeFormat};

mod braces;
mod flat;
mod indent;
mod lexer;
#[cfg(test)]
mod tests;

const LANGUAGES: &[&str] = &[
    "c",
    "cpp",
    "csharp",
    "java",
    "javascript",
    "jsx",
    "typescript",
    "tsx",
    "rust",
    "go",
    "kotlin",
    "swift",
    "scala",
    "dart",
    "php",
    "groovy",
    "objective-c",
    "r",
    "shell",
    "powershell",
    "perl",
    "protobuf",
    "python",
    "ruby",
    "lua",
    "elixir",
    "julia",
    "sql",
    "makefile",
    "dockerfile",
    "cmake",
];
const PARSERS: [Code; LANGUAGES.len()] = {
    let mut out = [Code(""); LANGUAGES.len()];
    let mut i = 0;
    while i < LANGUAGES.len() {
        out[i] = Code(LANGUAGES[i]);
        i += 1;
    }
    out
};

/// Language names are the ones declared by `crate::language` (case-insensitive).
pub fn for_language(language: &str) -> Option<&'static dyn TreeFormat> {
    PARSERS
        .iter()
        .find(|p| p.0.eq_ignore_ascii_case(language))
        .map(|p| p as &dyn TreeFormat)
}

#[derive(Clone, Copy)]
struct Code(&'static str);

#[derive(Debug)]
struct Definition {
    start: u32,
    name: Span,
    word: Option<Span>,
    body: u32,
    end: u32,
}

impl TreeFormat for Code {
    fn parse(&self, source: &str, limits: &Limits, cancel: &AtomicBool) -> Tree {
        let mut b = Builder::new(source.len(), limits, cancel);
        let scan = lexer::scan(source, self.0, limits, cancel, &mut b);
        if scan.stopped == Some(Stop::Cancelled) {
            let mut tree = b.finish(scan.end);
            tree.stopped = scan.stopped;
            return tree;
        }
        let mut definitions = Vec::new();
        let flat = matches!(self.0, "sql" | "makefile" | "dockerfile" | "cmake");
        let stopped = if flat {
            flat::definitions(&scan, self.0, limits, cancel, &mut b, &mut definitions)
        } else if matches!(self.0, "python" | "ruby" | "lua" | "elixir" | "julia") {
            indent::definitions(&scan, self.0, limits, cancel, &mut b, &mut definitions)
        } else {
            braces::definitions(&scan, self.0, limits, cancel, &mut b, &mut definitions)
        };
        // Detection follows token order. A containing definition comes first even when
        // several definitions share a physical line (their spans start at the same byte).
        let mut open: Vec<(u32, u32)> = Vec::new();
        for d in definitions {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            while open
                .last()
                .is_some_and(|&(end, body)| flat || d.name.start >= end || d.name.start < body)
            {
                if let Some((end, _)) = open.pop() {
                    b.close(end);
                }
            }
            let end = open.last().map_or(d.end, |&(end, _)| d.end.min(end));
            let Ok(id) = b.open(Kind::Symbol, d.start, Some(d.name)) else {
                break;
            };
            if let Some(word) = d.word {
                b.set_value(id, word, None);
            }
            open.push((end, d.body));
        }
        while let Some((end, _)) = open.pop() {
            b.close(end);
        }
        let mut tree = b.finish(scan.end);
        if tree.stopped.is_none() {
            tree.stopped = if cancel.load(Ordering::Relaxed) {
                Some(Stop::Cancelled)
            } else {
                stopped.or(scan.stopped)
            };
        }
        tree
    }
}

/// Bound the intermediate definition list as well as the final builder.
fn add(out: &mut Vec<Definition>, d: Definition, limits: &Limits, b: &mut Builder<'_>) -> bool {
    if out.len().saturating_add(1) >= limits.max_nodes as usize {
        b.error(d.start..d.start, "more outline symbols than the node limit");
        return false;
    }
    out.push(d);
    true
}
