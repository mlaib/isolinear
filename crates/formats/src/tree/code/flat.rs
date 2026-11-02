use std::sync::atomic::{AtomicBool, Ordering};

use super::lexer::{HEADER, Scan};
use super::{Builder, Definition, Limits, Stop, add};

pub(super) fn definitions(
    s: &Scan<'_>,
    language: &str,
    limits: &Limits,
    cancel: &AtomicBool,
    b: &mut Builder<'_>,
    out: &mut Vec<Definition>,
) -> Option<Stop> {
    match language {
        "sql" => sql(s, limits, cancel, b, out),
        "cmake" => cmake(s, limits, cancel, b, out),
        _ => lines(s, language, limits, cancel, b, out),
    }
}

fn sql(
    s: &Scan<'_>,
    limits: &Limits,
    cancel: &AtomicBool,
    b: &mut Builder<'_>,
    out: &mut Vec<Definition>,
) -> Option<Stop> {
    let mut i = 0;
    while i < s.tokens.len() {
        if cancel.load(Ordering::Relaxed) {
            return Some(Stop::Cancelled);
        }
        if !s.eq(i, "create") {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        if s.eq(j, "or") && s.eq(j + 1, "replace") {
            j += 2;
        }
        if s.eq(j, "temporary") || s.eq(j, "temp") || s.eq(j, "unique") {
            j += 1;
        }
        if ![
            "table",
            "view",
            "function",
            "procedure",
            "index",
            "trigger",
            "type",
            "schema",
        ]
        .iter()
        .any(|w| s.eq(j, w))
        {
            i += 1;
            continue;
        }
        let intro = j;
        j += 1;
        if s.eq(j, "if") && s.eq(j + 1, "not") && s.eq(j + 2, "exists") {
            j += 3;
        }
        let Some(mut name) = s.span(j) else {
            break;
        };
        if s.is(j, "[") {
            name.end = s.mate(j).map_or(s.end, |n| s.end(n));
            j = s.after_group(j);
        } else {
            j += 1;
        }
        while s.is(j, ".") && s.span(j + 1).is_some() {
            name.end = s.end(j + 1);
            j += 2;
        }
        let body = name.end;
        while j < s.tokens.len() && !s.is(j, ";") {
            if j.is_multiple_of(1024) && cancel.load(Ordering::Relaxed) {
                return Some(Stop::Cancelled);
            }
            if matches!(s.text(j), "(" | "[" | "{") {
                j = s.after_group(j);
            } else {
                j += 1;
            }
        }
        if !add(
            out,
            Definition {
                start: s.line_start(i),
                name,
                word: s.span(intro),
                body,
                end: s.end(j),
            },
            limits,
            b,
        ) {
            return Some(Stop::Nodes);
        }
        i = j + 1;
    }
    None
}

fn lines(
    s: &Scan<'_>,
    language: &str,
    limits: &Limits,
    cancel: &AtomicBool,
    b: &mut Builder<'_>,
    out: &mut Vec<Definition>,
) -> Option<Stop> {
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
        let first = line.first;
        let definition = if language == "dockerfile" {
            if !s.eq(first, "from") {
                continue;
            }
            if let Some(previous) = out.last_mut() {
                previous.end = line.start;
            }
            (first + 1..line.past.min(first + HEADER))
                .find(|&i| s.eq(i, "as"))
                .and_then(|i| {
                    s.span(i + 1)
                        .map(|name| name.start..s.end(line.past.saturating_sub(1)))
                })
                .map(|name| Definition {
                    start: line.start,
                    body: name.end,
                    name,
                    word: s.span(first),
                    end: s.end,
                })
        } else {
            if s.source.as_bytes().get(line.start as usize) == Some(&b'\t') {
                continue;
            }
            // A recipe ends at the next non-recipe statement, including assignments.
            if let Some(previous) = out.last_mut() {
                previous.end = line.start;
            }
            (first..line.past.min(first + HEADER))
                .find(|&i| s.is(i, ":") || s.is(i, "="))
                .filter(|&i| s.is(i, ":") && !matches!(s.text(i + 1), "=" | ":") && i > first)
                .map(|i| Definition {
                    start: line.start,
                    name: s.start(first)..s.end(i - 1),
                    word: None,
                    body: s.end(i),
                    end: s.end,
                })
        };
        if let Some(d) = definition
            && !add(out, d, limits, b)
        {
            return Some(Stop::Nodes);
        }
    }
    None
}

fn cmake(
    s: &Scan<'_>,
    limits: &Limits,
    cancel: &AtomicBool,
    b: &mut Builder<'_>,
    out: &mut Vec<Definition>,
) -> Option<Stop> {
    let mut stack: Vec<(usize, bool)> = Vec::new();
    let mut i = 0;
    while i < s.tokens.len() {
        if cancel.load(Ordering::Relaxed) {
            return Some(Stop::Cancelled);
        }
        if (s.eq(i, "function") || s.eq(i, "macro")) && s.is(i + 1, "(") {
            let Some(name) = s.span(i + 2) else {
                break;
            };
            if stack.len() >= limits.max_depth as usize {
                b.error(name, "symbol nesting is deeper than the outline limit");
                return Some(Stop::Depth);
            }
            if !add(
                out,
                Definition {
                    start: s.line_start(i),
                    body: name.end,
                    name,
                    word: s.span(i),
                    end: s.end,
                },
                limits,
                b,
            ) {
                return Some(Stop::Nodes);
            }
            stack.push((out.len() - 1, s.eq(i, "function")));
        } else if s.eq(i, "endfunction") || s.eq(i, "endmacro") {
            if let Some((index, function)) = stack.pop() {
                if function != s.eq(i, "endfunction") {
                    b.warning(
                        s.start(i)..s.end(i),
                        "definition terminator does not match its opening command",
                    );
                }
                if let Some(d) = out.get_mut(index) {
                    d.end = s.mate(i + 1).map_or(s.end(i), |j| s.end(j));
                }
            } else {
                b.warning(
                    s.start(i)..s.end(i),
                    "definition terminator has no opening command",
                );
            }
        }
        // Commands embedded in a command's arguments are data, not CMake commands.
        i = if s.is(i + 1, "(") {
            s.after_group(i + 1)
        } else {
            i + 1
        };
    }
    for (index, function) in stack {
        if let Some(d) = out.get(index) {
            b.warning(
                d.name.clone(),
                format!(
                    "expected '{}' before the end of the file",
                    if function { "endfunction" } else { "endmacro" }
                ),
            );
        }
    }
    None
}
