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
    let mut i = 0;
    let mut statement = 0;
    let mut objective_end = 0;
    while i < s.tokens.len() {
        if i.is_multiple_of(1024) && cancel.load(Ordering::Relaxed) {
            return Some(Stop::Cancelled);
        }
        let found = if matches!(language, "javascript" | "jsx" | "typescript" | "tsx")
            && matches!(s.text(i), "const" | "let" | "var")
        {
            binding(s, i)
        } else if language == "r" && s.word(i) && (i == 0 || !matches!(s.text(i - 1), "." | ":")) {
            r_function(s, i)
        } else if language == "objective-c"
            && s.is(i, "@")
            && matches!(s.text(i + 1), "interface" | "implementation" | "protocol")
        {
            objective_type(s, i, &mut objective_end, cancel, b)
        } else if language == "objective-c"
            && matches!(s.text(i), "+" | "-")
            && s.line(i).is_some_and(|l| l.first == i)
        {
            objective_method(s, i)
        } else if keyword(language, s.text(i)) && (i == 0 || !matches!(s.text(i - 1), "." | ":")) {
            introduced(s, language, i)
        } else if s.is(i, "(") && method_language(language) {
            method(s, language, i, statement)
        } else {
            None
        };
        if found.is_none() && keyword(language, s.text(i)) && header_limited(s, i + 1) {
            b.warning(
                s.start(i)..s.end(i),
                "declaration header exceeds the 512-token outline limit; definition omitted",
            );
        }
        if let Some((definition, next)) = found {
            if !add(out, definition, limits, b) {
                return Some(Stop::Nodes);
            }
            i = next;
            statement = i;
            continue;
        }
        if matches!(s.text(i), ";" | "{" | "}") {
            statement = i + 1;
        }
        i += 1;
    }
    None
}

fn keyword(language: &str, word: &str) -> bool {
    let words: &[&str] = match language {
        "c" => &["struct", "enum", "union"],
        "cpp" => &["class", "struct", "enum", "union", "namespace"],
        "csharp" => &[
            "class",
            "struct",
            "interface",
            "enum",
            "record",
            "namespace",
        ],
        "java" => &["class", "interface", "enum", "record"],
        "javascript" | "jsx" => &["class", "function"],
        "typescript" | "tsx" => &[
            "class",
            "function",
            "interface",
            "enum",
            "namespace",
            "module",
            "type",
        ],
        "rust" => &[
            "fn", "struct", "enum", "trait", "impl", "mod", "type", "union",
        ],
        "go" => &["func", "type"],
        "kotlin" => &["fun", "class", "interface", "object", "typealias"],
        "swift" => &[
            "func",
            "class",
            "struct",
            "enum",
            "protocol",
            "extension",
            "actor",
            "typealias",
            "init",
            "deinit",
        ],
        "scala" => &["def", "class", "trait", "object", "enum", "type"],
        "dart" => &["class", "enum", "mixin", "extension", "typedef"],
        "php" => &["function", "class", "interface", "trait", "enum"],
        "groovy" => &["class", "interface", "enum", "trait", "def"],
        "shell" => &["function"],
        "powershell" => &["function", "filter", "class", "enum"],
        "perl" => &["sub", "package"],
        "protobuf" => &["message", "enum", "service", "rpc"],
        "objective-c" => &["struct", "enum", "union"],
        _ => &[],
    };
    words.iter().any(|k| {
        if language == "powershell" {
            k.eq_ignore_ascii_case(word)
        } else {
            *k == word
        }
    })
}

fn method_language(language: &str) -> bool {
    matches!(
        language,
        "c" | "cpp"
            | "csharp"
            | "java"
            | "javascript"
            | "jsx"
            | "typescript"
            | "tsx"
            | "dart"
            | "groovy"
            | "objective-c"
            | "shell"
    )
}

/// Find a body without wandering into the next declaration. Matched parameter and
/// attribute groups are skipped in constant time. Headers have an explicit token bound.
fn body(s: &Scan<'_>, from: usize) -> Option<(usize, bool)> {
    let mut j = from;
    while j < s.tokens.len() && j.saturating_sub(from) < HEADER {
        match s.text(j) {
            "{" => return Some((j, true)),
            ";" => return Some((j, false)),
            "}" => return None,
            "(" | "[" => {
                let mate = s.mate(j)?;
                j = mate + 1;
            }
            _ => j += 1,
        }
    }
    None
}

fn header_limited(s: &Scan<'_>, from: usize) -> bool {
    let mut j = from;
    while j < s.tokens.len() {
        if j.saturating_sub(from) >= HEADER {
            return true;
        }
        match s.text(j) {
            "{" | "}" | ";" => return false,
            "(" | "[" => {
                let Some(mate) = s.mate(j) else {
                    return false;
                };
                j = mate + 1;
            }
            _ => j += 1,
        }
    }
    false
}

fn definition(
    s: &Scan<'_>,
    intro: usize,
    name: super::Span,
    word: Option<super::Span>,
    body_at: usize,
    braced: bool,
) -> (Definition, usize) {
    let end = if braced {
        s.mate(body_at).map_or(s.end, |i| s.end(i))
    } else {
        s.end(body_at)
    };
    (
        Definition {
            start: s.line_start(intro),
            name,
            word,
            body: s.end(body_at),
            end,
        },
        body_at + 1,
    )
}

fn introduced(s: &Scan<'_>, language: &str, i: usize) -> Option<(Definition, usize)> {
    let word = s.text(i);
    let mut name_at = i + 1;
    if word == "func" && language == "go" && s.is(name_at, "(") {
        name_at = s.mate(name_at)? + 1;
    }
    if matches!(word, "function") && matches!(s.text(name_at), "*" | "&") {
        name_at += 1;
    }
    if word == "enum" && matches!(s.text(name_at), "class" | "struct") {
        name_at += 1;
    }
    if language == "swift" && matches!(word, "init" | "deinit") {
        name_at = i;
    }
    if word == "impl" {
        let (at, braces) = body(s, name_at)?;
        if !braces {
            return None;
        }
        let end = s.tokens.get(at.checked_sub(1)?)?.span.end;
        return Some(definition(s, i, s.start(i)..end, s.span(i), at, true));
    }
    if !s.word(name_at) {
        return None;
    }
    let mut name = s.span(name_at)?;
    let mut after = name_at + 1;
    if language == "powershell" || language == "shell" {
        while matches!(s.text(after), "-" | ":") && s.word(after + 1) {
            name.end = s.end(after + 1);
            after += 2;
        }
    }
    // A type keyword in a variable declaration is a reference, not a new type.
    if matches!(word, "struct" | "class" | "union" | "enum")
        && matches!(language, "c" | "cpp" | "objective-c")
        && !matches!(s.text(after), "{" | ":" | "<" | ";")
    {
        return None;
    }
    if matches!(word, "def" | "fun" | "fn" | "func" | "rpc")
        && !matches!(s.text(after), "(" | "<" | "[")
    {
        return None;
    }
    if matches!(word, "type" | "typealias" | "typedef")
        && !matches!(s.text(after), "struct" | "interface")
        && let Some(line) = s.line(i)
        && !(after..line.past).any(|j| s.is(j, "{"))
    {
        return Some((
            Definition {
                start: line.start,
                name,
                word: s.span(i),
                body: line.end,
                end: line.end.min(s.end),
            },
            line.past,
        ));
    }
    let (at, braced) = body(s, after)?;
    // A declaration without a semicolon ends at its physical line, not a later body.
    if s.line(at).map(|l| l.start) != s.line(i).map(|l| l.start) {
        for k in after..at.min(after + HEADER) {
            if s.line_start(k) > s.line_start(i) && keyword(language, s.text(k)) {
                return None;
            }
        }
    }
    Some(definition(s, i, name, s.span(i), at, braced))
}

fn binding(s: &Scan<'_>, i: usize) -> Option<(Definition, usize)> {
    if !s.word(i + 1) {
        return None;
    }
    let name = s.span(i + 1)?;
    let mut j = i + 2;
    while j < s.tokens.len() && j - i < HEADER && !s.is(j, "=") {
        if matches!(s.text(j), ";" | "{" | "}") {
            return None;
        }
        j += 1;
    }
    if !s.is(j, "=") {
        return None;
    }
    j += 1;
    if s.is(j, "async") {
        j += 1;
    }
    if s.is(j, "function") {
        let word = s.span(j);
        let (at, braced) = body(s, j + 1)?;
        return Some(definition(s, i, name, word, at, braced));
    }
    let mut at = j;
    while at < s.tokens.len() && at - j < HEADER {
        if s.is(at, "=") && s.is(at + 1, ">") {
            let first = at + 2;
            if s.is(first, "{") {
                return Some(definition(s, i, name, s.span(i), first, true));
            }
            let mut last = first;
            while last < s.tokens.len()
                && last - first < HEADER
                && !matches!(s.text(last), ";" | "}")
            {
                if s.line_start(last) > s.line_start(first) {
                    break;
                }
                if matches!(s.text(last), "(" | "[") {
                    last = s.after_group(last);
                } else {
                    last += 1;
                }
            }
            let end = if s.is(last, ";") {
                s.end(last)
            } else {
                s.end(last.saturating_sub(1))
            };
            return Some((
                Definition {
                    start: s.line_start(i),
                    name,
                    word: s.span(i),
                    body: s.start(first),
                    end,
                },
                if s.is(last, ";") { last + 1 } else { last },
            ));
        }
        if matches!(s.text(at), ";" | "{" | "}") {
            return None;
        }
        if matches!(s.text(at), "(" | "[") {
            at = s.after_group(at);
        } else {
            at += 1;
        }
    }
    None
}

fn r_function(s: &Scan<'_>, i: usize) -> Option<(Definition, usize)> {
    let (name, next) = s.qualified(i)?;
    let after = if s.is(next, "<") && s.is(next + 1, "-") {
        next + 2
    } else if s.is(next, "=") {
        next + 1
    } else {
        return None;
    };
    if !s.is(after, "function") {
        return None;
    }
    let (at, braced) = body(s, after + 1)?;
    Some(definition(s, i, name, s.span(after), at, braced))
}

fn method(s: &Scan<'_>, language: &str, i: usize, statement: usize) -> Option<(Definition, usize)> {
    let name_at = i.checked_sub(1)?;
    if !s.word(name_at) || controls(s.text(name_at)) {
        return None;
    }
    let end_params = s.mate(i)?;
    let (at, braced) = body(s, end_params + 1)?;
    if !braced {
        return None;
    }
    let line_first = s.line(name_at)?.first;
    let first = statement.max(line_first);
    if name_at.saturating_sub(first) > HEADER {
        return None;
    }
    for j in first..name_at {
        if matches!(
            s.text(j),
            "=" | "." | "return" | "throw" | "new" | "delete" | "case"
        ) || controls(s.text(j))
        {
            return None;
        }
    }
    // `call();` and `call()\nother() {` are not declarations of call.
    for j in end_params + 1..at.min(end_params + HEADER) {
        if s.is(j, "(")
            && !matches!(
                s.text(j.saturating_sub(1)),
                "noexcept" | "decltype" | "__attribute__"
            )
        {
            return None;
        }
    }
    let mut name = s.span(name_at)?;
    let mut n = name_at;
    while n >= 3 && s.is(n - 1, ":") && s.is(n - 2, ":") && s.word(n - 3) {
        n -= 3;
        name.start = s.start(n);
    }
    if n > 0 && s.is(n - 1, "~") {
        name.start = s.start(n - 1);
    }
    let word = (first..n)
        .find(|&j| {
            s.word(j)
                && !matches!(
                    s.text(j),
                    "public"
                        | "private"
                        | "protected"
                        | "static"
                        | "inline"
                        | "virtual"
                        | "extern"
                        | "async"
                        | "export"
                        | "final"
                        | "override"
                        | "const"
                )
        })
        .and_then(|j| s.span(j));
    let word = if language == "shell" { None } else { word };
    Some(definition(s, first, name, word, at, true))
}

fn controls(word: &str) -> bool {
    matches!(
        word,
        "if" | "else"
            | "for"
            | "foreach"
            | "while"
            | "do"
            | "switch"
            | "case"
            | "catch"
            | "try"
            | "finally"
            | "with"
            | "using"
            | "lock"
            | "synchronized"
            | "match"
            | "when"
            | "select"
            | "sizeof"
            | "typeof"
            | "decltype"
            | "alignof"
            | "assert"
            | "return"
            | "throw"
            | "new"
    )
}

fn objective_type(
    s: &Scan<'_>,
    i: usize,
    cached_end: &mut usize,
    cancel: &AtomicBool,
    b: &mut Builder<'_>,
) -> Option<(Definition, usize)> {
    if !s.word(i + 2) {
        return None;
    }
    // Cache the next @end so repeated malformed headers cannot cause quadratic scans.
    if *cached_end <= i {
        let mut j = i + 3;
        while j < s.tokens.len() {
            if j.is_multiple_of(1024) && cancel.load(Ordering::Relaxed) {
                return None;
            }
            if s.is(j, "@") && s.is(j + 1, "end") {
                break;
            }
            j += 1;
        }
        *cached_end = j;
    }
    let j = *cached_end;
    if j == s.tokens.len() {
        b.warning(
            s.start(i)..s.end(i + 2),
            "expected '@end' before the end of the file",
        );
    }
    Some((
        Definition {
            start: s.line_start(i),
            name: s.span(i + 2)?,
            word: s.span(i + 1),
            body: s.end(i + 2),
            end: if j < s.tokens.len() {
                s.end(j + 1)
            } else {
                s.end
            },
        },
        i + 3,
    ))
}

fn objective_method(s: &Scan<'_>, i: usize) -> Option<(Definition, usize)> {
    if !s.is(i + 1, "(") {
        return None;
    }
    let close = s.mate(i + 1)?;
    if !s.word(close + 1) {
        return None;
    }
    let (at, braced) = body(s, close + 2)?;
    if !braced {
        return None;
    }
    Some(definition(
        s,
        i,
        s.span(close + 1)?,
        s.span(i + 2),
        at,
        true,
    ))
}
