use std::sync::atomic::{AtomicBool, Ordering};

use super::lexer::Scan;
use super::{Builder, Definition, Limits, Stop, add};

pub(super) fn definitions(
    s: &Scan<'_>,
    language: &str,
    limits: &Limits,
    cancel: &AtomicBool,
    b: &mut Builder<'_>,
    out: &mut Vec<Definition>,
) -> Option<Stop> {
    let mut stack: Vec<(usize, u32)> = Vec::new();
    let mut last_end = 0;
    let mut decorator: Option<(u32, u32)> = None;
    let mut header_through = 0;
    for (number, line) in s.lines.iter().enumerate() {
        if number.is_multiple_of(1024) && cancel.load(Ordering::Relaxed) {
            return Some(Stop::Cancelled);
        }
        if line.start >= s.end {
            break;
        }
        if line.first == line.past {
            continue;
        }
        let i = line.first;
        if s.start(i) < header_through {
            last_end = line.end.max(s.end(line.past.saturating_sub(1))).min(s.end);
            continue;
        }
        let terminator = language != "python" && s.is(i, "end");
        while stack
            .last()
            .is_some_and(|&(_, indent)| line.indent <= indent)
        {
            if let Some((index, indent)) = stack.pop() {
                if let Some(d) = out.get_mut(index) {
                    d.end = if terminator && line.indent == indent {
                        line.end.min(s.end)
                    } else {
                        last_end.max(d.name.end)
                    };
                }
                if language != "python" && !terminator {
                    b.warning(
                        line.start..line.start,
                        "expected 'end' before the next definition or dedented line",
                    );
                }
            }
        }
        if language == "python" && s.is(i, "@") {
            decorator.get_or_insert((line.start, line.indent));
            last_end = line.end.max(s.end(line.past.saturating_sub(1))).min(s.end);
            continue;
        }
        let intro = match (language, s.text(i)) {
            ("python", "async") if s.is(i + 1, "def") => Some(i + 1),
            ("python" | "ruby", "def" | "class") => Some(i),
            ("ruby", "module") => Some(i),
            ("lua", "local") if s.is(i + 1, "function") => Some(i + 1),
            ("lua", "function") => Some(i),
            ("elixir", "defmodule" | "def" | "defp" | "defmacro" | "defmacrop") => Some(i),
            ("julia", "function" | "struct" | "module" | "macro") => Some(i),
            ("julia", "mutable") if s.is(i + 1, "struct") => Some(i + 1),
            _ => None,
        };
        if let Some(intro) = intro.filter(|&i| s.word(i + 1)) {
            let (mut name, after) = s
                .qualified(intro + 1)
                .unwrap_or((s.start(intro + 1)..s.end(intro + 1), intro + 2));
            if language == "ruby" && matches!(s.text(after), "?" | "!") {
                name.end = s.end(after);
            }
            let start = decorator
                .take()
                .filter(|&(_, indent)| indent == line.indent)
                .map_or(line.start, |(start, _)| start);
            if stack.len() >= limits.max_depth as usize {
                b.error(
                    start..start,
                    "symbol nesting is deeper than the outline limit",
                );
                return Some(Stop::Depth);
            }
            if !add(
                out,
                Definition {
                    start,
                    name,
                    word: s.span(intro),
                    body: s.end(intro + 1),
                    end: s.end,
                },
                limits,
                b,
            ) {
                return Some(Stop::Nodes);
            }
            stack.push((out.len() - 1, line.indent));
            if s.is(after, "(") {
                header_through = s.mate(after).map_or(s.end, |j| s.end(j));
            }
        } else {
            decorator = None;
        }
        last_end = line.end.max(s.end(line.past.saturating_sub(1))).min(s.end);
    }
    for (index, _) in stack {
        if let Some(d) = out.get_mut(index) {
            d.end = last_end.max(d.name.end);
            if language != "python" {
                b.warning(d.name.clone(), "expected 'end' before the end of the file");
            } else if d.end <= d.body {
                b.warning(
                    d.name.clone(),
                    "expected an indented body after the definition",
                );
            }
        }
    }
    None
}
