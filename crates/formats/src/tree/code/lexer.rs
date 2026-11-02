use std::sync::atomic::{AtomicBool, Ordering};

use super::super::{Builder, Limits, Span, Stop};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ITEMS: usize = 1_000_000;
pub(super) const HEADER: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    Word,
    Literal,
    Punct(u8),
}

#[derive(Debug)]
pub(super) struct Token {
    pub span: Span,
    pub line: usize,
    pub kind: Kind,
    pub mate: Option<usize>,
}

#[derive(Debug)]
pub(super) struct Line {
    pub start: u32,
    pub end: u32,
    pub indent: u32,
    pub first: usize,
    pub past: usize,
}

pub(super) struct Scan<'s> {
    pub source: &'s str,
    pub tokens: Vec<Token>,
    pub lines: Vec<Line>,
    pub end: u32,
    pub stopped: Option<Stop>,
}

impl Scan<'_> {
    pub fn text(&self, i: usize) -> &str {
        self.tokens
            .get(i)
            .and_then(|t| self.source.get(t.span.start as usize..t.span.end as usize))
            .unwrap_or("")
    }
    pub fn is(&self, i: usize, text: &str) -> bool {
        self.text(i) == text
    }
    pub fn eq(&self, i: usize, text: &str) -> bool {
        self.text(i).eq_ignore_ascii_case(text)
    }
    pub fn word(&self, i: usize) -> bool {
        self.tokens.get(i).is_some_and(|t| t.kind == Kind::Word)
    }
    pub fn span(&self, i: usize) -> Option<Span> {
        self.tokens.get(i).map(|t| t.span.clone())
    }
    pub fn start(&self, i: usize) -> u32 {
        self.tokens.get(i).map_or(self.end, |t| t.span.start)
    }
    pub fn end(&self, i: usize) -> u32 {
        self.tokens.get(i).map_or(self.end, |t| t.span.end)
    }
    pub fn line(&self, i: usize) -> Option<&Line> {
        self.tokens.get(i).and_then(|t| self.lines.get(t.line))
    }
    pub fn line_start(&self, i: usize) -> u32 {
        self.line(i).map_or(self.end, |l| l.start)
    }
    pub fn mate(&self, i: usize) -> Option<usize> {
        self.tokens.get(i).and_then(|t| t.mate)
    }
    pub fn after_group(&self, i: usize) -> usize {
        self.mate(i).map_or(i + 1, |m| m + 1)
    }
    pub fn qualified(&self, i: usize) -> Option<(Span, usize)> {
        let mut name = self.span(i)?;
        let mut j = i + 1;
        while (self.is(j, ".") || self.is(j, ":")) && self.word(j + 1) {
            name.end = self.end(j + 1);
            j += 2;
        }
        Some((name, j))
    }
}

pub(super) fn scan<'s>(
    source: &'s str,
    language: &str,
    limits: &Limits,
    cancel: &AtomicBool,
    b: &mut Builder<'_>,
) -> Scan<'s> {
    let mut end = source.len().min(MAX_BYTES);
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = Scan {
        source,
        tokens: Vec::new(),
        lines: Vec::new(),
        end: end as u32,
        stopped: None,
    };
    if end < source.len() {
        b.error(
            end as u32..end as u32,
            "outline source limit: only the first 32 MiB are scanned",
        );
        out.stopped = Some(Stop::Nodes);
    }
    let bytes = source.as_bytes();
    let mut start = 0;
    let mut indent = 0u32;
    let mut leading = true;
    for (i, &c) in bytes.iter().take(end).enumerate() {
        if i.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
            out.stopped = Some(Stop::Cancelled);
            out.end = start as u32;
            return out;
        }
        if leading {
            match c {
                b' ' => indent += 1,
                b'\t' => indent = (indent / 8 + 1) * 8,
                _ => leading = false,
            }
        }
        if c == b'\n' {
            out.lines.push(Line {
                start: start as u32,
                end: i as u32,
                indent,
                first: 0,
                past: 0,
            });
            start = i + 1;
            indent = 0;
            leading = true;
            if out.lines.len() >= MAX_ITEMS {
                end = start;
                out.end = end as u32;
                out.stopped = Some(Stop::Nodes);
                b.error(
                    end as u32..end as u32,
                    "outline line limit: only the first 1,000,000 lines are scanned",
                );
                break;
            }
        }
    }
    if start < end || out.lines.is_empty() {
        out.lines.push(Line {
            start: start as u32,
            end: end as u32,
            indent,
            first: 0,
            past: 0,
        });
    }
    let mut lex = Lexer {
        bytes: &bytes[..end],
        source,
        language,
        pos: 0,
        cancel,
        cancelled: false,
        unterminated: false,
    };
    let mut line = 0;
    while lex.pos < end {
        if cancel.load(Ordering::Relaxed) {
            lex.cancelled = true;
            break;
        }
        let start = lex.pos;
        let c = bytes.get(start).copied().unwrap_or_default();
        if c.is_ascii_whitespace() {
            lex.pos += 1;
            continue;
        }
        if lex.comment(&out.tokens) {
            if lex.unterminated {
                b.warning(
                    start as u32..lex.pos as u32,
                    "expected the closing comment delimiter before the end of the file",
                );
            }
            continue;
        }
        let kind = if lex.literal(&out.tokens) {
            Kind::Literal
        } else if c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 {
            lex.pos += 1;
            while bytes
                .get(lex.pos)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c >= 0x80)
                && lex.pos < end
            {
                if !lex.step() {
                    break;
                }
            }
            Kind::Word
        } else {
            lex.pos += 1;
            Kind::Punct(c)
        };
        if lex.cancelled {
            break;
        }
        if lex.unterminated {
            b.warning(
                start as u32..lex.pos as u32,
                "expected the closing string delimiter before the end of the file",
            );
            lex.unterminated = false;
        }
        while out
            .lines
            .get(line + 1)
            .is_some_and(|l| l.start <= start as u32)
        {
            line += 1;
        }
        let index = out.tokens.len();
        if index >= MAX_ITEMS {
            out.end = start as u32;
            out.stopped = Some(Stop::Nodes);
            b.error(
                out.end..out.end,
                "outline token limit: only the first 1,000,000 tokens are scanned",
            );
            break;
        }
        out.tokens.push(Token {
            span: start as u32..lex.pos as u32,
            line,
            kind,
            mate: None,
        });
        if let Some(l) = out.lines.get_mut(line) {
            if l.first == l.past {
                l.first = index;
            }
            l.past = index + 1;
        }
    }
    if lex.cancelled {
        out.stopped = Some(Stop::Cancelled);
        return out;
    }
    let mut stack: Vec<(usize, u8)> = Vec::new();
    for i in 0..out.tokens.len() {
        if i.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
            out.stopped = Some(Stop::Cancelled);
            return out;
        }
        let Some(token) = out.tokens.get(i) else {
            break;
        };
        match token.kind {
            Kind::Punct(c @ (b'(' | b'[' | b'{')) => {
                if stack.len() >= (limits.max_depth as usize).min(4096) {
                    out.end = token.span.start;
                    out.stopped = Some(Stop::Depth);
                    b.error(
                        token.span.clone(),
                        "syntax nesting is deeper than the outline limit",
                    );
                    out.tokens.truncate(i);
                    break;
                }
                stack.push((i, c));
            }
            Kind::Punct(c @ (b')' | b']' | b'}')) => {
                let expected = match c {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if stack.last().is_some_and(|&(_, open)| open == expected) {
                    if let Some((j, _)) = stack.pop() {
                        if let Some(t) = out.tokens.get_mut(j) {
                            t.mate = Some(i);
                        }
                        if let Some(t) = out.tokens.get_mut(i) {
                            t.mate = Some(j);
                        }
                    }
                } else {
                    b.warning(
                        token.span.clone(),
                        "closing delimiter has no matching opening delimiter",
                    );
                }
            }
            _ => {}
        }
    }
    for (i, c) in stack {
        if let Some(t) = out.tokens.get(i) {
            b.warning(
                t.span.clone(),
                format!(
                    "expected '{}' before the end of the file",
                    match c {
                        b'(' => ')',
                        b'[' => ']',
                        _ => '}',
                    }
                ),
            );
        }
    }
    out
}

struct Lexer<'a> {
    bytes: &'a [u8],
    source: &'a str,
    language: &'a str,
    pos: usize,
    cancel: &'a AtomicBool,
    cancelled: bool,
    unterminated: bool,
}
impl Lexer<'_> {
    fn at(&self, s: &[u8]) -> bool {
        self.bytes.get(self.pos..).is_some_and(|b| b.starts_with(s))
    }
    fn step(&mut self) -> bool {
        if self.pos.is_multiple_of(4096) && self.cancel.load(Ordering::Relaxed) {
            self.cancelled = true;
            self.pos = self.bytes.len();
            return false;
        }
        self.pos += 1;
        true
    }
    fn line_comment(&mut self) {
        while self.pos < self.bytes.len() && self.bytes.get(self.pos) != Some(&b'\n') {
            if !self.step() {
                break;
            }
        }
    }
    fn block(&mut self, open: &[u8], close: &[u8], nested: bool) {
        self.pos += open.len();
        let mut depth = 1usize;
        while self.pos < self.bytes.len() {
            if self.cancel.load(Ordering::Relaxed) {
                self.cancelled = true;
                self.pos = self.bytes.len();
                break;
            }
            if self.at(close) {
                self.pos += close.len();
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if nested && self.at(open) {
                self.pos += open.len();
                depth += 1;
            } else if !self.step() {
                break;
            }
        }
        if depth != 0 && !self.cancelled {
            self.unterminated = true;
        }
    }
    fn bracket(&mut self, prefix: usize) -> bool {
        let start = self.pos + prefix;
        if self.bytes.get(start) != Some(&b'[') {
            return false;
        }
        let mut j = start + 1;
        while self.bytes.get(j) == Some(&b'=') && j - start <= 64 {
            j += 1;
        }
        if self.bytes.get(j) != Some(&b'[') {
            return false;
        }
        let count = j - start - 1;
        self.pos = j + 1;
        while self.pos < self.bytes.len() {
            if self.bytes.get(self.pos) == Some(&b']')
                && self
                    .bytes
                    .get(self.pos + 1..self.pos + 1 + count)
                    .is_some_and(|s| s.iter().all(|&c| c == b'='))
                && self.bytes.get(self.pos + 1 + count) == Some(&b']')
            {
                self.pos += count + 2;
                return true;
            }
            if !self.step() {
                break;
            }
        }
        self.unterminated = !self.cancelled;
        true
    }
    fn comment(&mut self, _tokens: &[Token]) -> bool {
        let hash = matches!(
            self.language,
            "python"
                | "ruby"
                | "elixir"
                | "julia"
                | "r"
                | "shell"
                | "powershell"
                | "perl"
                | "php"
                | "makefile"
                | "dockerfile"
                | "cmake"
                | "c"
                | "cpp"
                | "objective-c"
        );
        if self.language == "julia" && self.at(b"#=") {
            self.block(b"#=", b"=#", true);
            return true;
        }
        if self.language == "powershell" && self.at(b"<#") {
            self.block(b"<#", b"#>", false);
            return true;
        }
        if self.language == "cmake" && self.at(b"#[") && self.bracket(1) {
            return true;
        }
        if self.language == "lua" && self.at(b"--") {
            if !self.bracket(2) {
                self.line_comment();
            }
            return true;
        }
        if self.language == "sql" && self.at(b"--") {
            self.line_comment();
            return true;
        }
        if hash && self.at(b"#") {
            self.line_comment();
            return true;
        }
        if !matches!(
            self.language,
            "python"
                | "ruby"
                | "elixir"
                | "julia"
                | "r"
                | "shell"
                | "makefile"
                | "dockerfile"
                | "cmake"
                | "lua"
        ) {
            if self.at(b"//") {
                self.line_comment();
                return true;
            }
            if self.at(b"/*") {
                self.block(b"/*", b"*/", true);
                return true;
            }
        }
        if self.language == "ruby" && self.at(b"=begin") {
            self.block(b"=begin", b"=end", false);
            return true;
        }
        false
    }
    fn quoted(&mut self, quote: u8, triple: bool, verbatim: bool) {
        let width = if triple { 3 } else { 1 };
        self.pos += width;
        let mut steps = 0usize;
        while self.pos < self.bytes.len() {
            steps += 1;
            if steps.is_multiple_of(4096) && self.cancel.load(Ordering::Relaxed) {
                self.cancelled = true;
                self.pos = self.bytes.len();
                break;
            }
            if self.bytes.get(self.pos) == Some(&quote) {
                if triple {
                    if self.bytes.get(self.pos..self.pos + 3) == Some(&[quote; 3]) {
                        self.pos += 3;
                        return;
                    }
                } else if verbatim && self.bytes.get(self.pos + 1) == Some(&quote) {
                    self.pos += 2;
                    continue;
                } else {
                    self.pos += 1;
                    return;
                }
            }
            if self.bytes.get(self.pos) == Some(&b'\\') && !verbatim {
                self.pos += 1;
                if let Some(c) = self.source.get(self.pos..).and_then(|s| s.chars().next()) {
                    self.pos = (self.pos + c.len_utf8()).min(self.bytes.len());
                }
            } else {
                self.pos += 1;
            }
        }
        self.unterminated = !self.cancelled;
    }
    fn heredoc(&mut self) -> bool {
        if !self.at(b"<<") || (self.language != "php" && self.at(b"<<<")) {
            return false;
        }
        let mut j = self.pos
            + if self.language == "php" && self.at(b"<<<") {
                3
            } else {
                2
            };
        if self.bytes.get(j) == Some(&b'-') {
            j += 1;
        }
        while matches!(self.bytes.get(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        if matches!(self.bytes.get(j), Some(b'\"' | b'\'')) {
            j += 1;
        }
        let start = j;
        while self
            .bytes
            .get(j)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            && j - start < 128
        {
            j += 1;
        }
        if j == start {
            return false;
        }
        let tag = self.bytes.get(start..j).unwrap_or_default();
        self.pos = j;
        self.line_comment();
        if self.pos < self.bytes.len() {
            self.pos += 1;
        }
        while self.pos < self.bytes.len() {
            let line_start = self.pos;
            self.line_comment();
            if self.cancelled {
                return true;
            }
            let line = self.bytes.get(line_start..self.pos).unwrap_or_default();
            let line = line.trim_ascii();
            if line == tag || (self.language == "php" && line.strip_suffix(b";") == Some(tag)) {
                if self.pos < self.bytes.len() {
                    self.pos += 1;
                }
                return true;
            }
            if self.pos < self.bytes.len() {
                self.pos += 1;
            }
        }
        self.unterminated = true;
        true
    }

    fn literal(&mut self, tokens: &[Token]) -> bool {
        if matches!(self.language, "shell" | "php" | "dockerfile") && self.heredoc() {
            return true;
        }
        if self.language == "powershell"
            && (self.at(b"@\"\n") || self.at(b"@'\n") || self.at(b"@\"\r\n") || self.at(b"@'\r\n"))
        {
            let quote = self.bytes.get(self.pos + 1).copied().unwrap_or(b'"');
            self.pos += 2;
            while self.pos < self.bytes.len() {
                if self.bytes.get(self.pos.wrapping_sub(1)) == Some(&b'\n')
                    && self.bytes.get(self.pos..self.pos + 2) == Some(&[quote, b'@'])
                {
                    self.pos += 2;
                    return true;
                }
                if !self.step() {
                    return true;
                }
            }
            self.unterminated = true;
            return true;
        }
        if self.language == "perl" {
            let prefix = ["qq", "qr", "qx", "qw", "q", "m"].into_iter().find(|p| {
                self.at(p.as_bytes())
                    && self.bytes.get(self.pos + p.len()).is_some_and(|c| {
                        !c.is_ascii_alphanumeric() && !c.is_ascii_whitespace() && *c != b'_'
                    })
            });
            if let Some(prefix) = prefix {
                self.pos += prefix.len();
                let open = self.bytes.get(self.pos).copied().unwrap_or(b'/');
                let close = match open {
                    b'{' => b'}',
                    b'(' => b')',
                    b'[' => b']',
                    b'<' => b'>',
                    _ => open,
                };
                if close != open {
                    self.block(&[open], &[close], true);
                } else {
                    self.quoted(open, false, false);
                }
                return true;
            }
        }
        if matches!(self.language, "lua" | "cmake") && self.bracket(0) {
            return true;
        }
        // Rust raw strings and C++ raw strings have source-defined closing delimiters.
        if self.language == "rust"
            && (self.at(b"r\"") || self.at(b"r#") || self.at(b"br\"") || self.at(b"br#"))
        {
            let begin = self.pos;
            let mut j = begin + if self.at(b"br") { 2 } else { 1 };
            while self.bytes.get(j) == Some(&b'#') && j - begin < 258 {
                j += 1;
            }
            if self.bytes.get(j) == Some(&b'"') {
                let hashes = j - begin - if self.at(b"br") { 2 } else { 1 };
                self.pos = j + 1;
                while self.pos < self.bytes.len() {
                    if self.at(b"\"")
                        && self
                            .bytes
                            .get(self.pos + 1..self.pos + 1 + hashes)
                            .is_some_and(|s| s.iter().all(|&c| c == b'#'))
                    {
                        self.pos += hashes + 1;
                        break;
                    }
                    if !self.step() {
                        break;
                    }
                }
                return true;
            }
        }
        if self.language == "cpp" && self.at(b"R\"") {
            let begin = self.pos + 2;
            let mut j = begin;
            while j - begin <= 16
                && self
                    .bytes
                    .get(j)
                    .is_some_and(|&c| c != b'(' && !c.is_ascii_whitespace())
            {
                j += 1;
            }
            if self.bytes.get(j) == Some(&b'(') {
                let tag = self.bytes.get(begin..j).unwrap_or_default();
                self.pos = j + 1;
                while self.pos < self.bytes.len() {
                    if self.at(b")")
                        && self.bytes.get(self.pos + 1..self.pos + 1 + tag.len()) == Some(tag)
                        && self.bytes.get(self.pos + 1 + tag.len()) == Some(&b'"')
                    {
                        self.pos += tag.len() + 2;
                        break;
                    }
                    if !self.step() {
                        break;
                    }
                }
                return true;
            }
        }
        if self.language == "sql" && self.at(b"$") {
            let begin = self.pos;
            let mut j = begin + 1;
            while self
                .bytes
                .get(j)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
                && j - begin < 128
            {
                j += 1;
            }
            if self.bytes.get(j) == Some(&b'$') {
                let tag = self.bytes.get(begin..j + 1).unwrap_or_default();
                self.pos = j + 1;
                while self.pos < self.bytes.len() {
                    if self.at(tag) {
                        self.pos += tag.len();
                        break;
                    }
                    if !self.step() {
                        break;
                    }
                }
                return true;
            }
        }
        let Some(&quote) = self.bytes.get(self.pos) else {
            return false;
        };
        if matches!(quote, b'\'' | b'"' | b'`') {
            if quote == b'\'' && self.language == "rust" {
                let next = self
                    .source
                    .get(self.pos + 1..)
                    .and_then(|s| s.chars().next());
                if next.is_some_and(|c| {
                    c != '\\' && self.bytes.get(self.pos + 1 + c.len_utf8()) != Some(&b'\'')
                }) {
                    return false;
                }
            }
            let triple = self.bytes.get(self.pos..self.pos + 3) == Some(&[quote; 3]);
            let verbatim = self.language == "sql"
                || (self.language == "csharp"
                    && self.pos > 0
                    && self.bytes.get(self.pos - 1) == Some(&b'@'))
                || (self.language == "go" && quote == b'`');
            self.quoted(quote, triple, verbatim);
            return true;
        }
        if matches!(
            self.language,
            "javascript" | "jsx" | "typescript" | "tsx" | "perl" | "ruby"
        ) && quote == b'/'
        {
            let previous = tokens
                .last()
                .and_then(|t| self.source.get(t.span.start as usize..t.span.end as usize));
            if previous
                .is_none_or(|s| matches!(s, "=" | "(" | "[" | "," | ":" | "return" | ">" | "~"))
            {
                self.pos += 1;
                let mut class = false;
                while self.pos < self.bytes.len() {
                    let c = self.bytes.get(self.pos).copied().unwrap_or_default();
                    if c == b'/' && !class {
                        self.pos += 1;
                        break;
                    }
                    if c == b'[' {
                        class = true;
                    } else if c == b']' {
                        class = false;
                    }
                    if c == b'\\' {
                        self.pos += 1;
                    }
                    if !self.step() {
                        break;
                    }
                }
                self.pos = self.pos.min(self.bytes.len());
                return true;
            }
        }
        false
    }
}
