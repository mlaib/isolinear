//! YAML structural index over retained source: block and flow collections, plain, quoted and
//! block scalars, comments and multiple documents.
//!
//! Hand-written and iterative (an explicit stack), so hostile nesting cannot overflow the call
//! stack. Nothing is resolved: anchors and tags are shown as written, and an alias is a node of its
//! own that is **never expanded** (no alias bombs). Plain scalars get a node kind from the YAML 1.2
//! core schema for display only; values are never converted.
//!
//! Not supported (reported as errors, the parsed prefix is kept): explicit `?` keys, flow
//! collections as keys and single-pair mappings inside flow sequences.
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Builder, Kind, Limits, Scalar, Stop as TreeStop, Tree, TreeFormat};

pub const FORMAT: Option<&'static dyn TreeFormat> = Some(&Yaml);

pub struct Yaml;

impl TreeFormat for Yaml {
    fn parse(&self, source: &str, limits: &Limits, cancel: &AtomicBool) -> Tree {
        let mut parts = false;
        loop {
            let mut p = Parser::new(source, limits, cancel, parts);
            let result = p.run();
            // A second document requires one restart with Part containers. Parsing
            // rather than looking for marker lines handles multiline quoted scalars.
            if matches!(result, Err(Fail::Stream)) {
                parts = true;
                continue;
            }
            let cancelled = matches!(result, Err(Fail::Cancelled));
            if let Err(Fail::Syntax(at, message)) = result {
                let end = source
                    .get(at..)
                    .and_then(|s| s.chars().next())
                    .map_or(at, |c| at + c.len_utf8());
                p.tree.fail(at as u32..end as u32, message);
            }
            let mut tree = p
                .tree
                .finish(p.last_end.max(p.pos).min(source.len()) as u32);
            if cancelled {
                tree.stopped = Some(TreeStop::Cancelled);
            }
            return tree;
        }
    }
}

#[derive(Clone, Copy)]
enum Value {
    Scalar(Scalar),
    Alias,
}

#[derive(Debug)]
enum Fail {
    Syntax(usize, String),
    Stopped,
    Cancelled,
    Stream,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ctx {
    Doc,
    /// Block mapping whose keys start at this column.
    Map(usize),
    /// Block sequence whose `-` indicators are at this column.
    Seq(usize),
    FlowMap,
    FlowSeq,
}

struct Open {
    node: Option<u32>,
    ctx: Ctx,
    /// Keys seen so far (mappings only).
    keys: HashSet<String>,
}

/// What introduced the value being waited for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Intro {
    /// The value starts on a line of its own.
    Line,
    /// After `- ` on the same line: compact nested collections are allowed.
    Dash,
    /// After `key:` on the same line.
    Colon,
    /// After `---` on the same line.
    DocStart,
}

/// A value position that has been opened (by a key, a `-` or a document start) but not filled.
#[derive(Clone, Copy)]
struct Slot {
    name: Option<(u32, u32)>,
    /// Indentation of the owning collection; -1 for a document's root.
    parent: isize,
    owner_map: bool,
    intro: Intro,
    /// Where an empty value is placed.
    at: usize,
    /// Properties (anchor, tag) read so far, as a source range.
    props: Option<(usize, usize)>,
    root: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    Eol,
    Comment,
    Colon(usize),
    Flow,
}

struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
    line_start: usize,
    scanned: usize,
    tree: Builder<'a>,
    stack: Vec<Open>,
    cancel: &'a AtomicBool,
    slot: Option<Slot>,
    last_end: usize,
    in_doc: bool,
    steps: usize,
    parts: bool,
    documents: usize,
}

fn blank(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n')
}
fn flow_indicator(c: u8) -> bool {
    matches!(c, b',' | b'[' | b']' | b'{' | b'}')
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, limits: &Limits, cancel: &'a AtomicBool, parts: bool) -> Self {
        let start = if source.starts_with('\u{FEFF}') { 3 } else { 0 };
        Parser {
            src: source,
            b: source.as_bytes(),
            pos: start,
            line_start: start,
            scanned: start,
            tree: Builder::new(source.len(), limits, cancel),
            stack: Vec::new(),
            cancel,
            slot: None,
            last_end: start,
            in_doc: false,
            steps: 0,
            parts,
            documents: 0,
        }
    }
    fn run(&mut self) -> Result<(), Fail> {
        loop {
            self.steps += 1;
            if (self.steps == 1 || self.steps.is_multiple_of(4096))
                && self.cancel.load(Ordering::Relaxed)
            {
                return Err(Fail::Cancelled);
            }
            self.skip_blank()?;
            let Some(&c) = self.b.get(self.pos) else {
                return self.end_document(self.last_end);
            };
            self.sync_line()?;
            let col = self.pos - self.line_start;
            let first = self.b[self.line_start..self.pos]
                .iter()
                .all(|&c| c == b' ' || c == b'\t');
            if first && col == 0 {
                if self.marker(self.pos, b"---") {
                    self.end_document(self.last_end)?;
                    self.start_document(self.pos, true)?;
                    self.pos += 3;
                    self.last_end = self.pos;
                    continue;
                }
                if self.marker(self.pos, b"...") {
                    self.end_document(self.pos + 3)?;
                    self.pos += 3;
                    self.last_end = self.pos;
                    continue;
                }
                if c == b'%' && !self.in_doc {
                    // Directive (%YAML, %TAG): shown in the source only.
                    self.pos = self.line_end(self.pos)?;
                    continue;
                }
            }
            if !self.in_doc {
                self.start_document(self.pos, false)?;
            }
            if first && self.b[self.line_start..self.pos].contains(&b'\t') {
                return Err(Fail::Syntax(
                    self.pos,
                    "Tab characters must not be used for indentation".into(),
                ));
            }
            match self.slot.take() {
                Some(slot) if !first => self.value(slot)?,
                Some(slot)
                    if col as isize > slot.parent
                        || (slot.owner_map
                            && col as isize == slot.parent
                            && self.dash_entry(self.pos)) =>
                {
                    self.value(Slot {
                        intro: Intro::Line,
                        ..slot
                    })?
                }
                Some(slot) => {
                    self.empty(slot)?;
                    self.block_entry(col)?;
                }
                None if !first => {
                    return Err(Fail::Syntax(
                        self.pos,
                        format!(
                            "Unexpected {} after the value on this line",
                            describe(self.src, self.pos)
                        ),
                    ));
                }
                None => self.block_entry(col)?,
            }
        }
    }

    fn checkpoint(&self) -> Result<(), Fail> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Fail::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Keep `line_start` at the start of the line holding `pos` (amortised linear).
    fn sync_line(&mut self) -> Result<(), Fail> {
        while self.scanned < self.pos {
            self.checkpoint()?;
            let end = (self.scanned + 4096).min(self.pos);
            if let Some(i) = self.b[self.scanned..end].iter().rposition(|&c| c == b'\n') {
                self.line_start = self.scanned + i + 1;
            }
            self.scanned = end;
        }
        Ok(())
    }

    fn line_end(&self, from: usize) -> Result<usize, Fail> {
        for (chunk, bytes) in self.b[from.min(self.b.len())..].chunks(4096).enumerate() {
            self.checkpoint()?;
            if let Some(i) = bytes.iter().position(|&c| c == b'\n') {
                return Ok(from + chunk * 4096 + i);
            }
        }
        Ok(self.b.len())
    }

    /// Skip spaces, line breaks and comments.
    fn skip_blank(&mut self) -> Result<(), Fail> {
        let b = self.b;
        loop {
            if self.pos.is_multiple_of(4096) {
                self.checkpoint()?;
            }
            match b.get(self.pos) {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.pos += 1,
                Some(b'#') if self.pos == 0 || blank(b[self.pos - 1]) => {
                    self.pos = self.line_end(self.pos)?;
                }
                Some(0xEF)
                    if self
                        .src
                        .get(self.pos..)
                        .is_some_and(|s| s.starts_with('\u{FEFF}')) =>
                {
                    self.pos += 3;
                }
                _ => return Ok(()),
            }
        }
    }

    fn skip_inline(&self, mut i: usize) -> Result<usize, Fail> {
        while matches!(self.b.get(i), Some(b' ' | b'\t')) {
            if i.is_multiple_of(4096) {
                self.checkpoint()?;
            }
            i += 1;
        }
        Ok(i)
    }

    fn skip_spaces(&self, mut i: usize) -> Result<usize, Fail> {
        while self.b.get(i) == Some(&b' ') {
            if i.is_multiple_of(4096) {
                self.checkpoint()?;
            }
            i += 1;
        }
        Ok(i)
    }

    fn blank_after(&self, i: usize) -> bool {
        self.b.get(i + 1).is_none_or(|&c| blank(c))
    }

    fn marker(&self, i: usize, m: &[u8]) -> bool {
        self.b[i..].starts_with(m) && self.b.get(i + 3).is_none_or(|&c| blank(c))
    }

    fn dash_entry(&self, i: usize) -> bool {
        self.b.get(i) == Some(&b'-') && self.blank_after(i)
    }

    fn push_node(
        &mut self,
        value: Value,
        start: usize,
        end: usize,
        name: Option<(u32, u32)>,
    ) -> Result<u32, Fail> {
        let (kind, scalar) = match value {
            Value::Scalar(s) => (Kind::Scalar, Some(s)),
            Value::Alias => (Kind::Alias, None),
        };
        let span_start = name.map_or(start as u32, |(s, _)| s);
        let node = self
            .tree
            .leaf(
                kind,
                scalar,
                span_start..end as u32,
                name.map(|(s, e)| s..e),
                Some(start as u32..end as u32),
            )
            .map_err(|_| Fail::Stopped)?;
        self.last_end = self.last_end.max(end);
        Ok(node)
    }

    fn open(
        &mut self,
        kind: Kind,
        ctx: Ctx,
        start: usize,
        end: usize,
        name: Option<(u32, u32)>,
    ) -> Result<(), Fail> {
        let span_start = name.map_or(start as u32, |(s, _)| s);
        let node = self
            .tree
            .open(kind, span_start, name.map(|(s, e)| s..e))
            .map_err(|_| Fail::Stopped)?;
        // Container values contain only the anchor/tag properties, never the opener.
        let props_end = if matches!(ctx, Ctx::FlowMap | Ctx::FlowSeq) {
            end.saturating_sub(1)
        } else {
            end
        };
        let props = self
            .src
            .get(start..props_end)
            .unwrap_or("")
            .trim_end_matches([' ', '\t', '\n', '\r']);
        if !props.is_empty() && kind != Kind::Part {
            self.tree
                .set_value(node, start as u32..(start + props.len()) as u32, None);
        }
        self.last_end = self.last_end.max(end);
        self.stack.push(Open {
            node: Some(node),
            ctx,
            keys: HashSet::new(),
        });
        Ok(())
    }

    fn pop(&mut self) {
        if let Some(o) = self.stack.pop()
            && let Some(node) = o.node
        {
            let at = self.last_end.max(self.tree.node(node).span.end as usize);
            self.tree.close(at as u32);
        }
    }

    fn start_document(&mut self, at: usize, explicit: bool) -> Result<(), Fail> {
        let end = if explicit { at + 3 } else { at };
        self.documents += 1;
        if self.documents > 1 && !self.parts {
            return Err(Fail::Stream);
        }
        if self.parts {
            self.open(Kind::Part, Ctx::Doc, at, end, None)?;
        } else {
            self.stack.push(Open {
                node: None,
                ctx: Ctx::Doc,
                keys: HashSet::new(),
            });
        }
        self.in_doc = true;
        self.slot = Some(Slot {
            name: None,
            parent: -1,
            owner_map: false,
            intro: if explicit {
                Intro::DocStart
            } else {
                Intro::Line
            },
            at: end,
            props: None,
            root: true,
        });
        Ok(())
    }

    fn end_document(&mut self, end: usize) -> Result<(), Fail> {
        if let Some(slot) = self.slot.take() {
            self.empty(slot)?;
        }
        while let Some(o) = self.stack.pop() {
            if let Some(node) = o.node {
                let mut at = self.last_end.max(self.tree.node(node).span.end as usize);
                if o.ctx == Ctx::Doc {
                    at = at.max(end);
                }
                self.tree.close(at as u32);
            }
        }
        self.in_doc = false;
        Ok(())
    }

    /// An absent value: a null node at the indicator (or covering its properties).
    fn empty(&mut self, slot: Slot) -> Result<(), Fail> {
        if slot.root && slot.props.is_none() {
            return Ok(());
        }
        let (s, e) = slot.props.unwrap_or((slot.at, slot.at));
        self.push_node(Value::Scalar(Scalar::Null), s, e, slot.name)?;
        Ok(())
    }

    /// A token starting its line with no value pending: a new entry of an open block collection.
    fn block_entry(&mut self, col: usize) -> Result<(), Fail> {
        loop {
            match self.stack.last().map(|o| o.ctx) {
                Some(Ctx::Map(i) | Ctx::Seq(i)) if i > col => self.pop(),
                Some(Ctx::Seq(i)) if i == col && !self.dash_entry(self.pos) => self.pop(),
                _ => break,
            }
        }
        match self.stack.last().map(|o| o.ctx) {
            Some(Ctx::Map(i)) if i == col => self.key_entry(col, self.pos),
            Some(Ctx::Seq(i)) if i == col => {
                self.pos += 1;
                self.last_end = self.pos;
                self.slot = Some(Slot {
                    name: None,
                    parent: col as isize,
                    owner_map: false,
                    intro: Intro::Dash,
                    at: self.pos,
                    props: None,
                    root: false,
                });
                Ok(())
            }
            Some(Ctx::Map(_) | Ctx::Seq(_)) => Err(Fail::Syntax(
                self.pos,
                "Unexpected indentation: this line is not part of the collection above".into(),
            )),
            _ => Err(Fail::Syntax(
                self.pos,
                "Unexpected content after the document's root node (a new document starts with '---')"
                    .into(),
            )),
        }
    }

    /// A block mapping key (with optional properties) at `start`, followed by `:`.
    fn key_entry(&mut self, col: usize, start: usize) -> Result<(), Fail> {
        let mut p = start;
        while matches!(self.b.get(p), Some(b'&' | b'!')) {
            p = self.skip_inline(self.prop_end(p, false)?)?;
        }
        if self.dash_entry(p) {
            return Err(Fail::Syntax(
                p,
                "A sequence entry is not allowed here: expected a mapping key".into(),
            ));
        }
        if self.b.get(p) == Some(&b'?') && self.blank_after(p) {
            return Err(Fail::Syntax(
                p,
                "Explicit '?' mapping keys are not supported".into(),
            ));
        }
        let Some((key_end, colon)) = self.implicit_key(p)? else {
            return Err(Fail::Syntax(
                p,
                "Expected a mapping key followed by ':'".into(),
            ));
        };
        self.add_key(p, key_end);
        self.pos = colon + 1;
        self.last_end = self.pos;
        self.slot = Some(Slot {
            name: Some((start as u32, key_end as u32)),
            parent: col as isize,
            owner_map: true,
            intro: Intro::Colon,
            at: self.pos,
            props: None,
            root: false,
        });
        Ok(())
    }

    fn add_key(&mut self, start: usize, end: usize) {
        let key = key_text(self.src.get(start..end).unwrap_or(""));
        let duplicate = match self.stack.last_mut() {
            Some(open) => !open.keys.insert(key.clone()),
            None => false,
        };
        if duplicate {
            let message = format!(
                "Duplicate key \"{}\" in mapping",
                key.chars().take(80).collect::<String>()
            );
            self.tree.warning(start as u32..end as u32, message);
        }
    }

    /// If a single-line implicit key starts at `p`, its end and the position of its `:`.
    fn implicit_key(&self, p: usize) -> Result<Option<(usize, usize)>, Fail> {
        let b = self.b;
        let Some(&first) = b.get(p) else {
            return Ok(None);
        };
        let key_end = match first {
            q @ (b'"' | b'\'') => {
                let mut i = p + 1;
                loop {
                    self.checkpoint()?;
                    let Some(&next) = b.get(i) else {
                        return Ok(None);
                    };
                    match next {
                        b'\n' => return Ok(None),
                        b'\\' if q == b'"' => i += 2,
                        c if c == q => {
                            if q == b'\'' && b.get(i + 1) == Some(&b'\'') {
                                i += 2;
                            } else {
                                break i + 1;
                            }
                        }
                        _ => i += 1,
                    }
                }
            }
            b'*' => self.alias_end(p)?,
            b'[' | b'{' | b'|' | b'>' | b'#' | b'%' | b'@' | b'`' | b',' | b']' | b'}' => {
                return Ok(None);
            }
            b'-' | b'?' | b':' if self.blank_after(p) => return Ok(None),
            _ => {
                let (e, stop) = self.plain_line(p, false)?;
                return Ok(match stop {
                    Stop::Colon(c) if e > p => Some((e, c)),
                    _ => None,
                });
            }
        };
        let q = self.skip_inline(key_end)?;
        Ok((b.get(q) == Some(&b':') && self.blank_after(q)).then_some((key_end, q)))
    }

    /// Fill a pending value starting at `self.pos`.
    fn value(&mut self, slot: Slot) -> Result<(), Fail> {
        let b = self.b;
        let start = self.pos;
        let mut p = start;
        let mut props = slot.props;
        while matches!(b.get(p), Some(b'&' | b'!')) {
            let e = self.prop_end(p, false)?;
            props = Some((props.map_or(p, |(s, _)| s), e));
            p = self.skip_inline(e)?;
        }
        let ends_line = match b.get(p) {
            None | Some(b'\n' | b'\r') => true,
            Some(b'#') => p > 0 && blank(b[p - 1]),
            _ => false,
        };
        if ends_line {
            // Properties alone: the value follows on later lines.
            self.pos = props.map_or(p, |(_, e)| e);
            self.last_end = self.last_end.max(self.pos);
            self.slot = Some(Slot { props, ..slot });
            return Ok(());
        }
        let c = b[p];
        let collection_start = !matches!(c, b'[' | b'{' | b'|' | b'>') && !self.dash_entry(p);
        if collection_start && let Some((_, colon)) = self.implicit_key(p)? {
            if matches!(slot.intro, Intro::Colon | Intro::DocStart) {
                return Err(Fail::Syntax(
                    colon,
                    "Mapping values are not allowed here: a nested mapping starts on a new line"
                        .into(),
                ));
            }
            // Properties on the key's own line belong to the key, earlier ones to the mapping.
            let col = start - self.line_start;
            let (s, e) = slot.props.unwrap_or((start, start));
            self.open(Kind::Object, Ctx::Map(col), s, e, slot.name)?;
            return self.key_entry(col, start);
        }
        let s = props.map_or(p, |(s, _)| s);
        match c {
            b'-' if self.dash_entry(p) => {
                if matches!(slot.intro, Intro::Colon | Intro::DocStart) {
                    return Err(Fail::Syntax(
                        p,
                        "A block sequence cannot start on the same line as its key".into(),
                    ));
                }
                let col = p - self.line_start;
                let (s, e) = props.unwrap_or((p, p));
                self.open(Kind::Array, Ctx::Seq(col), s, e, slot.name)?;
                self.pos = p + 1;
                self.last_end = self.pos;
                self.slot = Some(Slot {
                    name: None,
                    parent: col as isize,
                    owner_map: false,
                    intro: Intro::Dash,
                    at: self.pos,
                    props: None,
                    root: false,
                });
                Ok(())
            }
            b'[' | b'{' => {
                self.pos = p;
                self.flow(slot.name, props)
            }
            b'|' | b'>' => self.block_scalar(slot, s, p),
            b'*' => {
                let e = self.alias_end(p)?;
                self.push_node(Value::Alias, s, e, slot.name)?;
                self.pos = e;
                Ok(())
            }
            b'"' | b'\'' => {
                let e = self.quoted_end(p)?;
                self.push_node(Value::Scalar(Scalar::String), s, e, slot.name)?;
                self.pos = e;
                Ok(())
            }
            b'?' if self.blank_after(p) => Err(Fail::Syntax(
                p,
                "Explicit '?' mapping keys are not supported".into(),
            )),
            _ => {
                self.check_plain_start(p, false)?;
                let e = self.plain_end(p, slot.parent)?;
                let kind = plain_kind(&self.src[p..e]);
                self.push_node(kind, s, e, slot.name)?;
                self.pos = e;
                Ok(())
            }
        }
    }

    /// End of an anchor or tag starting at `p`.
    fn prop_end(&self, p: usize, flow: bool) -> Result<usize, Fail> {
        let b = self.b;
        if b[p..].starts_with(b"!<") {
            let mut i = p + 2;
            while i < b.len() && !matches!(b[i], b'>' | b'\n') {
                self.checkpoint()?;
                i += 1;
            }
            return Ok(if b.get(i) == Some(&b'>') { i + 1 } else { i });
        }
        let mut i = p + 1;
        while i < b.len() && !blank(b[i]) && !((flow || b[p] == b'&') && flow_indicator(b[i])) {
            self.checkpoint()?;
            i += 1;
        }
        Ok(i)
    }

    fn alias_end(&self, p: usize) -> Result<usize, Fail> {
        let b = self.b;
        let mut i = p + 1;
        while i < b.len() && !blank(b[i]) && !flow_indicator(b[i]) {
            self.checkpoint()?;
            i += 1;
        }
        Ok(i)
    }

    fn quoted_end(&self, p: usize) -> Result<usize, Fail> {
        let b = self.b;
        let q = b[p];
        let mut i = p + 1;
        loop {
            self.checkpoint()?;
            match b.get(i) {
                None => {
                    return Err(Fail::Syntax(
                        p,
                        "Unterminated quoted scalar: missing closing quote".into(),
                    ));
                }
                Some(&c) if c == q => {
                    if q == b'\'' && b.get(i + 1) == Some(&b'\'') {
                        i += 2;
                    } else {
                        return Ok(i + 1);
                    }
                }
                Some(b'\\') if q == b'"' => i += 2,
                _ => i += 1,
            }
        }
    }

    fn check_plain_start(&self, p: usize, flow: bool) -> Result<(), Fail> {
        let b = self.b;
        let c = b[p];
        let next_ends = self
            .b
            .get(p + 1)
            .is_none_or(|&n| blank(n) || (flow && flow_indicator(n)));
        let bad = match c {
            b',' | b'[' | b']' | b'{' | b'}' | b'%' | b'@' | b'`' | b'|' | b'>' | b'&' | b'*'
            | b'!' | b'"' | b'\'' => true,
            b'-' | b'?' | b':' => next_ends,
            _ => false,
        };
        if bad {
            Err(Fail::Syntax(
                p,
                format!(
                    "Unexpected {} where a value was expected",
                    describe(self.src, p)
                ),
            ))
        } else {
            Ok(())
        }
    }

    /// One line of a plain scalar from `start`: its trimmed end and what stopped it.
    fn plain_line(&self, start: usize, flow: bool) -> Result<(usize, Stop), Fail> {
        let b = self.b;
        let mut end = start;
        let mut i = start;
        while i < b.len() {
            self.checkpoint()?;
            let c = b[i];
            match c {
                b'\n' | b'\r' => return Ok((end, Stop::Eol)),
                b':' if b
                    .get(i + 1)
                    .is_none_or(|&n| blank(n) || (flow && flow_indicator(n))) =>
                {
                    return Ok((end, Stop::Colon(i)));
                }
                b'#' if i > start && matches!(b[i - 1], b' ' | b'\t') => {
                    return Ok((end, Stop::Comment));
                }
                b',' | b'[' | b']' | b'{' | b'}' if flow => return Ok((end, Stop::Flow)),
                b' ' | b'\t' => {}
                _ => end = i + 1,
            }
            i += 1;
        }
        Ok((end, Stop::Eol))
    }

    /// End of a block-context plain scalar, following continuation lines indented beyond `parent`.
    fn plain_end(&self, start: usize, parent: isize) -> Result<usize, Fail> {
        let b = self.b;
        let (mut end, stop) = self.plain_line(start, false)?;
        match stop {
            Stop::Eol => {}
            Stop::Colon(at) => {
                return Err(Fail::Syntax(
                    at,
                    "Mapping values are not allowed here".into(),
                ));
            }
            _ => return Ok(end),
        }
        let mut line = self.line_end(end)? + 1;
        while line < b.len() {
            self.checkpoint()?;
            let spaces = self.skip_spaces(line)?;
            let content = self.skip_inline(spaces)?;
            match b.get(content) {
                None => break,
                Some(b'\n') | Some(b'\r') => {
                    line = self.line_end(content)? + 1;
                    continue;
                }
                Some(b'#') => break,
                _ => {}
            }
            if (spaces - line) as isize <= parent
                || (spaces == line && (self.marker(line, b"---") || self.marker(line, b"...")))
            {
                break;
            }
            let (e, stop) = self.plain_line(content, false)?;
            match stop {
                Stop::Colon(at) => {
                    return Err(Fail::Syntax(
                        at,
                        "Mapping values are not allowed in a multi-line plain scalar".into(),
                    ));
                }
                Stop::Comment => {
                    if e > content {
                        end = e;
                    }
                    break;
                }
                _ => {
                    if e > content {
                        end = e;
                    }
                }
            }
            line = self.line_end(content)? + 1;
        }
        Ok(end)
    }

    /// Block scalar (`|` literal or `>` folded) with its header at `p`.
    fn block_scalar(&mut self, slot: Slot, start: usize, p: usize) -> Result<(), Fail> {
        let b = self.b;
        let mut i = p + 1;
        let mut explicit = None;
        for _ in 0..2 {
            match b.get(i) {
                Some(&d @ b'1'..=b'9') if explicit.is_none() => {
                    explicit = Some((d - b'0') as isize);
                    i += 1;
                }
                Some(b'+' | b'-') => i += 1,
                _ => break,
            }
        }
        let header_end = i;
        let after = self.skip_inline(i)?;
        match b.get(after) {
            None | Some(b'\n' | b'\r') => {}
            Some(b'#') if after > header_end => {}
            _ => {
                return Err(Fail::Syntax(
                    after,
                    "Invalid block scalar header: expected an indicator, a comment or a line break"
                        .into(),
                ));
            }
        }
        let parent = slot.parent;
        let mut line = self.line_end(after)? + 1;
        let mut indent = explicit.map(|d| (parent.max(0) + d) as usize);
        let mut end = header_end;
        while line < b.len() {
            self.checkpoint()?;
            let spaces = self.skip_spaces(line)? - line;
            let rest = line + spaces;
            let line_blank = matches!(b.get(rest), None | Some(b'\n' | b'\r'));
            if line_blank {
                line = self.line_end(rest)? + 1;
                continue;
            }
            let m = *indent.get_or_insert(spaces);
            if m as isize <= parent && explicit.is_none() {
                break;
            }
            if spaces < m
                || (spaces == 0 && (self.marker(line, b"---") || self.marker(line, b"...")))
            {
                break;
            }
            let le = self.line_end(rest)?;
            end = if le > 0 && b[le - 1] == b'\r' {
                le - 1
            } else {
                le
            };
            line = le + 1;
        }
        self.push_node(
            Value::Scalar(Scalar::String),
            start,
            end.max(header_end),
            slot.name,
        )?;
        self.pos = end.max(header_end);
        Ok(())
    }

    /// Skip blanks and comments inside a flow collection; document markers are an error.
    fn skip_flow_blank(&mut self) -> Result<(), Fail> {
        let b = self.b;
        loop {
            self.checkpoint()?;
            match b.get(self.pos) {
                Some(b' ' | b'\t' | b'\r') => self.pos += 1,
                Some(b'\n') => {
                    self.pos += 1;
                    if self.marker(self.pos, b"---") || self.marker(self.pos, b"...") {
                        return Err(Fail::Syntax(
                            self.pos,
                            "Document marker inside a flow collection: missing ']' or '}'".into(),
                        ));
                    }
                }
                Some(b'#') if self.pos == 0 || blank(b[self.pos - 1]) => {
                    self.pos = self.line_end(self.pos)?
                }
                _ => return Ok(()),
            }
        }
    }

    /// End of a flow-context plain scalar, which may continue on following lines.
    fn flow_plain_end(&self, start: usize) -> Result<usize, Fail> {
        let b = self.b;
        let (mut end, stop) = self.plain_line(start, true)?;
        if stop != Stop::Eol {
            return Ok(end);
        }
        let mut i = end;
        loop {
            self.checkpoint()?;
            while matches!(b.get(i), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                if i.is_multiple_of(4096) {
                    self.checkpoint()?;
                }
                i += 1;
            }
            let Some(&c) = b.get(i) else { break };
            let line_head = i == 0 || b[i - 1] == b'\n';
            if flow_indicator(c)
                || c == b'#'
                || (c == b':' && b.get(i + 1).is_none_or(|&n| blank(n) || flow_indicator(n)))
                || (line_head && (self.marker(i, b"---") || self.marker(i, b"...")))
            {
                break;
            }
            let (e, stop) = self.plain_line(i, true)?;
            if e > i {
                end = e;
            }
            if stop != Stop::Eol {
                break;
            }
            i = e.max(i + 1);
        }
        Ok(end)
    }

    /// A complete flow collection starting at `self.pos` (`[` or `{`), nested ones included.
    fn flow(
        &mut self,
        name: Option<(u32, u32)>,
        props: Option<(usize, usize)>,
    ) -> Result<(), Fail> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum St {
            Value,
            Entry,
            MapColon,
            After,
        }
        let b = self.b;
        let base = self.stack.len();
        let outer = self.pos;
        let mut name = name;
        let mut props = props;
        let mut st = St::Value;
        loop {
            self.steps += 1;
            if (self.steps == 1 || self.steps.is_multiple_of(4096))
                && self.cancel.load(Ordering::Relaxed)
            {
                return Err(Fail::Cancelled);
            }
            self.skip_flow_blank()?;
            let Some(&c) = b.get(self.pos) else {
                return Err(Fail::Syntax(
                    outer,
                    "Unclosed flow collection: missing ']' or '}' before the end of input".into(),
                ));
            };
            let in_map = matches!(self.stack.last().map(|o| o.ctx), Some(Ctx::FlowMap));
            match st {
                St::Value => {
                    if matches!(c, b'&' | b'!') {
                        let e = self.prop_end(self.pos, true)?;
                        props = Some((props.map_or(self.pos, |(s, _)| s), e));
                        self.pos = e;
                        continue;
                    }
                    let p = self.pos;
                    let s = props.map_or(p, |(s, _)| s);
                    match c {
                        b'[' | b'{' => {
                            let (kind, ctx) = if c == b'[' {
                                (Kind::Array, Ctx::FlowSeq)
                            } else {
                                (Kind::Object, Ctx::FlowMap)
                            };
                            self.open(kind, ctx, s, p + 1, name.take())?;
                            props = None;
                            self.pos = p + 1;
                            st = St::Entry;
                        }
                        b'*' => {
                            let e = self.alias_end(p)?;
                            self.push_node(Value::Alias, s, e, name.take())?;
                            props = None;
                            self.pos = e;
                            st = St::After;
                        }
                        b'"' | b'\'' => {
                            let e = self.quoted_end(p)?;
                            self.push_node(Value::Scalar(Scalar::String), s, e, name.take())?;
                            props = None;
                            self.pos = e;
                            st = St::After;
                        }
                        b',' | b']' | b'}' => {
                            let (s, e) = props.take().unwrap_or((p, p));
                            self.push_node(Value::Scalar(Scalar::Null), s, e, name.take())?;
                            st = St::After;
                        }
                        b'|' | b'>' => {
                            return Err(Fail::Syntax(
                                p,
                                "Block scalars are not allowed inside flow collections".into(),
                            ));
                        }
                        _ => {
                            self.check_plain_start(p, true)?;
                            let e = self.flow_plain_end(p)?;
                            if e <= p {
                                return Err(Fail::Syntax(
                                    p,
                                    format!("Unexpected {}", describe(self.src, p)),
                                ));
                            }
                            let kind = plain_kind(&self.src[p..e]);
                            self.push_node(kind, s, e, name.take())?;
                            props = None;
                            self.pos = e;
                            st = St::After;
                        }
                    }
                }
                St::Entry => {
                    if (c == b']' && !in_map) || (c == b'}' && in_map) {
                        if self.close_flow(base) {
                            return Ok(());
                        }
                        st = St::After;
                        continue;
                    }
                    if matches!(c, b',' | b']' | b'}') {
                        return Err(Fail::Syntax(
                            self.pos,
                            format!(
                                "Unexpected {} where an entry was expected",
                                describe(self.src, self.pos)
                            ),
                        ));
                    }
                    if !in_map {
                        st = St::Value;
                        continue;
                    }
                    let ks = self.pos;
                    let mut p = ks;
                    while matches!(b.get(p), Some(b'&' | b'!')) {
                        p = self.prop_end(p, true)?;
                        p = self.skip_inline(p)?;
                    }
                    let ke = match b.get(p) {
                        Some(b'"' | b'\'') => self.quoted_end(p)?,
                        Some(b'*') => self.alias_end(p)?,
                        Some(b'[' | b'{') => {
                            return Err(Fail::Syntax(
                                p,
                                "Flow collections as mapping keys are not supported".into(),
                            ));
                        }
                        Some(b'?') if b.get(p + 1).is_none_or(|&n| blank(n)) => {
                            return Err(Fail::Syntax(
                                p,
                                "Explicit '?' mapping keys are not supported".into(),
                            ));
                        }
                        Some(_) => {
                            self.check_plain_start(p, true)?;
                            self.flow_plain_end(p)?
                        }
                        None => p,
                    };
                    if ke <= p {
                        return Err(Fail::Syntax(p, "Expected a mapping key".into()));
                    }
                    self.add_key(p, ke);
                    name = Some((ks as u32, ke as u32));
                    self.last_end = self.last_end.max(ke);
                    self.pos = ke;
                    st = St::MapColon;
                }
                St::MapColon => match c {
                    b':' => {
                        self.pos += 1;
                        st = St::Value;
                    }
                    b',' | b'}' => {
                        self.push_node(
                            Value::Scalar(Scalar::Null),
                            self.pos,
                            self.pos,
                            name.take(),
                        )?;
                        st = St::After;
                    }
                    _ => {
                        return Err(Fail::Syntax(
                            self.pos,
                            format!(
                                "Unexpected {} where ':' was expected after the key",
                                describe(self.src, self.pos)
                            ),
                        ));
                    }
                },
                St::After => match c {
                    b',' => {
                        self.pos += 1;
                        st = St::Entry;
                    }
                    b']' if !in_map => {
                        if self.close_flow(base) {
                            return Ok(());
                        }
                    }
                    b'}' if in_map => {
                        if self.close_flow(base) {
                            return Ok(());
                        }
                    }
                    b':' if !in_map => {
                        return Err(Fail::Syntax(
                            self.pos,
                            "Single-pair mappings inside flow sequences are not supported".into(),
                        ));
                    }
                    _ => {
                        let expected = if in_map { "',' or '}'" } else { "',' or ']'" };
                        return Err(Fail::Syntax(
                            self.pos,
                            format!(
                                "Unexpected {} where {expected} was expected",
                                describe(self.src, self.pos)
                            ),
                        ));
                    }
                },
            }
        }
    }

    /// Close the innermost flow collection at `self.pos`; true when the outermost one closed.
    fn close_flow(&mut self, base: usize) -> bool {
        let end = self.pos + 1;
        if self.stack.pop().is_some() {
            self.tree.close(end as u32);
        }
        self.pos = end;
        self.last_end = self.last_end.max(end);
        self.stack.len() <= base
    }
}

fn describe(source: &str, pos: usize) -> String {
    match source.get(pos..).and_then(|s| s.chars().next()) {
        Some(c) if c.is_control() => format!("U+{:04X}", c as u32),
        Some(c) => format!("'{c}'"),
        None => "end of input".into(),
    }
}

/// Node kind of a plain scalar under the YAML 1.2 core schema (display only).
fn plain_kind(text: &str) -> Value {
    match text {
        "~" | "null" | "Null" | "NULL" => Value::Scalar(Scalar::Null),
        "true" | "True" | "TRUE" | "false" | "False" | "FALSE" => Value::Scalar(Scalar::Boolean),
        _ if is_core_number(text) => Value::Scalar(Scalar::Number),
        _ => Value::Scalar(Scalar::String),
    }
}

fn is_core_number(t: &str) -> bool {
    let digits = |s: &str, f: fn(&u8) -> bool| !s.is_empty() && s.as_bytes().iter().all(f);
    if let Some(h) = t.strip_prefix("0x") {
        return digits(h, u8::is_ascii_hexdigit);
    }
    if let Some(o) = t.strip_prefix("0o") {
        return digits(o, |c| (b'0'..=b'7').contains(c));
    }
    if matches!(t, ".nan" | ".NaN" | ".NAN") {
        return true;
    }
    let u = t.strip_prefix(['-', '+']).unwrap_or(t);
    if matches!(u, ".inf" | ".Inf" | ".INF") {
        return true;
    }
    let (mantissa, exponent) = match u.find(['e', 'E']) {
        Some(i) => (&u[..i], Some(&u[i + 1..])),
        None => (u, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((a, b)) => {
            (a.is_empty() || digits(a, u8::is_ascii_digit))
                && (b.is_empty() || digits(b, u8::is_ascii_digit))
                && !(a.is_empty() && b.is_empty())
        }
        None => digits(mantissa, u8::is_ascii_digit),
    };
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        digits(e, u8::is_ascii_digit)
    });
    mantissa_ok && exponent_ok
}

/// Strip leading anchor/tag properties from a node token.
fn strip_props(token: &str) -> &str {
    let mut t = token;
    loop {
        let s = t.trim_start();
        if s.starts_with('&') || s.starts_with('!') {
            let e = s.find([' ', '\t', '\n', '\r']).unwrap_or(s.len());
            t = &s[e..];
        } else {
            return s;
        }
    }
}

/// Text used to compare keys for duplicates: quoted keys decoded, others as written.
fn key_text(token: &str) -> String {
    let t = strip_props(token);
    if t.starts_with('"') || t.starts_with('\'') {
        decode_scalar(t)
    } else {
        t.to_owned()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    Plain,
    Single,
    Double,
}

/// Decode a scalar token as written in the source (properties are skipped).
fn decode_scalar(token: &str) -> String {
    let t = strip_props(token);
    if let Some(rest) = t.strip_prefix('"') {
        fold(rest.strip_suffix('"').unwrap_or(rest), Style::Double)
    } else if let Some(rest) = t.strip_prefix('\'') {
        fold(rest.strip_suffix('\'').unwrap_or(rest), Style::Single)
    } else {
        fold(t, Style::Plain)
    }
}

/// Line folding and escapes of flow scalars (plain, single- and double-quoted).
fn fold(inner: &str, style: Style) -> String {
    let b = inner.as_bytes();
    let mut out = String::with_capacity(inner.len());
    let mut i = 0;
    let skip_break = |i: usize| {
        if b.get(i) == Some(&b'\r') && b.get(i + 1) == Some(&b'\n') {
            i + 2
        } else {
            i + 1
        }
    };
    let skip_ws = |mut i: usize| {
        while matches!(b.get(i), Some(b' ' | b'\t')) {
            i += 1;
        }
        i
    };
    while i < b.len() {
        let c = b[i];
        if c == b'\\' && style == Style::Double {
            let simple = match b.get(i + 1) {
                Some(b'\n' | b'\r') => {
                    i = skip_ws(skip_break(i + 1));
                    continue;
                }
                Some(b'0') => Some('\0'),
                Some(b'a') => Some('\u{7}'),
                Some(b'b') => Some('\u{8}'),
                Some(b't' | b'\t') => Some('\t'),
                Some(b'n') => Some('\n'),
                Some(b'v') => Some('\u{b}'),
                Some(b'f') => Some('\u{c}'),
                Some(b'r') => Some('\r'),
                Some(b'e') => Some('\u{1b}'),
                Some(b' ') => Some(' '),
                Some(b'"') => Some('"'),
                Some(b'/') => Some('/'),
                Some(b'\\') => Some('\\'),
                Some(b'N') => Some('\u{85}'),
                Some(b'_') => Some('\u{a0}'),
                Some(b'L') => Some('\u{2028}'),
                Some(b'P') => Some('\u{2029}'),
                _ => None,
            };
            if let Some(ch) = simple {
                out.push(ch);
                i += 2;
                continue;
            }
            let width = match b.get(i + 1) {
                Some(b'x') => 2,
                Some(b'u') => 4,
                Some(b'U') => 8,
                _ => 0,
            };
            let code = inner
                .get(i + 2..i + 2 + width)
                .filter(|_| width > 0)
                .and_then(|h| u32::from_str_radix(h, 16).ok());
            match code {
                Some(code) => {
                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    i += 2 + width;
                }
                None => {
                    out.push('\\');
                    i += 1;
                }
            }
            continue;
        }
        if c == b'\'' && style == Style::Single && b.get(i + 1) == Some(&b'\'') {
            out.push('\'');
            i += 2;
            continue;
        }
        if c == b'\n' || c == b'\r' {
            let trimmed = out.trim_end_matches([' ', '\t']).len();
            out.truncate(trimmed);
            i = skip_ws(skip_break(i));
            let mut breaks = 0;
            while matches!(b.get(i), Some(b'\n' | b'\r')) {
                breaks += 1;
                i = skip_ws(skip_break(i));
            }
            if breaks == 0 {
                out.push(' ');
            } else {
                out.extend(std::iter::repeat_n('\n', breaks));
            }
            continue;
        }
        match inner.get(i..).and_then(|s| s.chars().next()) {
            Some(ch) => {
                out.push(ch);
                i += ch.len_utf8();
            }
            None => i += 1,
        }
    }
    if style == Style::Plain {
        let trimmed = out.trim_end_matches([' ', '\t']).len();
        out.truncate(trimmed);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{ROOT, Severity, dump};
    use super::*;

    const FIXTURES: &[(&str, &str)] = &[
        (
            "kubernetes",
            include_str!("../../../../tests/fixtures/tree/yaml/kubernetes.yaml"),
        ),
        (
            "workflow",
            include_str!("../../../../tests/fixtures/tree/yaml/workflow.yaml"),
        ),
        (
            "stream",
            include_str!("../../../../tests/fixtures/tree/yaml/stream.yaml"),
        ),
        (
            "anchors",
            include_str!("../../../../tests/fixtures/tree/yaml/anchors.yaml"),
        ),
        (
            "alias-bomb",
            include_str!("../../../../tests/fixtures/tree/yaml/alias-bomb.yaml"),
        ),
        (
            "block-scalars",
            include_str!("../../../../tests/fixtures/tree/yaml/block-scalars.yaml"),
        ),
        (
            "flow",
            include_str!("../../../../tests/fixtures/tree/yaml/flow.yaml"),
        ),
        (
            "malformed-indent",
            include_str!("../../../../tests/fixtures/tree/yaml/malformed-indent.yaml"),
        ),
        (
            "malformed-quote",
            include_str!("../../../../tests/fixtures/tree/yaml/malformed-quote.yaml"),
        ),
        (
            "malformed-flow",
            include_str!("../../../../tests/fixtures/tree/yaml/malformed-flow.yaml"),
        ),
    ];

    fn parse(s: &str) -> Tree {
        Yaml.parse(s, &Limits::default(), &AtomicBool::new(false))
    }

    fn text<'s>(s: &'s str, span: &super::super::Span) -> &'s str {
        &s[span.start as usize..span.end as usize]
    }

    fn named<'a>(s: &str, t: &'a Tree, key: &str) -> &'a super::super::Node {
        (0..t.len() as u32)
            .map(|id| t.node(id))
            .find(|n| n.name.as_ref().is_some_and(|span| text(s, span) == key))
            .unwrap()
    }

    fn check_spans(s: &str, t: &Tree) {
        let mut last = 0;
        for id in 0..t.len() as u32 {
            let n = t.node(id);
            assert!(n.span.start >= last, "{s:?}: {n:?}");
            last = n.span.start;
            for span in std::iter::once(&n.span)
                .chain(n.name.iter())
                .chain(n.value.iter())
            {
                assert!(
                    span.start <= span.end && span.end as usize <= s.len(),
                    "{s:?}: {n:?}"
                );
                assert!(
                    s.is_char_boundary(span.start as usize)
                        && s.is_char_boundary(span.end as usize),
                    "{s:?}: {n:?}"
                );
                assert!(
                    n.span.start <= span.start && span.end <= n.span.end,
                    "{s:?}: {n:?}"
                );
            }
            if let Some(parent) = n.parent() {
                assert!(parent < id);
                let p = t.node(parent);
                assert!(
                    p.span.start <= n.span.start && n.span.end <= p.span.end,
                    "{s:?}: {n:?}, parent {p:?}"
                );
                assert!(t.children(parent).any(|child| child == id));
            }
            assert_eq!(t.children(id).count() as u32, n.child_count());
        }
        for d in &t.diagnostics {
            assert!(
                s.get(d.span.start as usize..d.span.end as usize).is_some(),
                "{s:?}: {d:?}"
            );
        }
    }

    #[test]
    fn fixtures_keep_valid_trees_and_malformed_prefixes() {
        for (name, s) in FIXTURES {
            let t = parse(s);
            check_spans(s, &t);
            if name.starts_with("malformed") {
                assert_eq!(t.stopped, Some(TreeStop::Error), "{name}");
                assert_eq!(t.errors(), 1);
                assert!(t.len() > 2);
            } else {
                assert_eq!(t.stopped, None, "{name}: {:?}", t.diagnostics);
                assert!(t.diagnostics.is_empty(), "{name}: {:?}", t.diagnostics);
            }
        }
    }

    #[test]
    fn dump_preserves_names_quotes_properties_and_member_extents() {
        let s = "a: &b [1, 'x']\nref: *b\nempty:\n";
        assert_eq!(
            dump(&parse(s), s),
            "\
Document [1] @0..30
  Object [3] @0..29
    Array a = &b [2] @0..14
      Scalar(Number) = 1 @7..8
      Scalar(String) = 'x' @10..13
    Alias ref = *b @15..22
    Scalar(Null) empty =  @23..29
"
        );
    }

    #[test]
    fn block_document_spans_point_at_source_text() {
        let s = "# config\nname: Isolinear  # trailing\nversion: 1.5\nempty:\ntags:\n  - viewer\n  - \"rust\"\n  - 'it''s'\nnested:\n  ok: true\n  none: ~\n  list:\n  - a\n  - b\nurl: http://example.org/x\n";
        let t = parse(s);
        assert!(t.diagnostics.is_empty());
        check_spans(s, &t);
        let entries: Vec<_> = (1..t.len() as u32)
            .map(|id| {
                let n = t.node(id);
                (
                    n.name.as_ref().map(|r| text(s, r)),
                    n.value.as_ref().map(|r| text(s, r)).unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            entries,
            [
                (None, ""),
                (Some("name"), "Isolinear"),
                (Some("version"), "1.5"),
                (Some("empty"), ""),
                (Some("tags"), ""),
                (None, "viewer"),
                (None, "\"rust\""),
                (None, "'it''s'"),
                (Some("nested"), ""),
                (Some("ok"), "true"),
                (Some("none"), "~"),
                (Some("list"), ""),
                (None, "a"),
                (None, "b"),
                (Some("url"), "http://example.org/x")
            ]
        );
        assert_eq!(t.node(1).child_count(), 6);
        assert_eq!(
            text(s, &named(s, &t, "tags").span),
            "tags:\n  - viewer\n  - \"rust\"\n  - 'it''s'"
        );
        assert!(text(s, &named(s, &t, "nested").span).ends_with("- b"));
        assert_eq!(named(s, &t, "ok").scalar, Some(Scalar::Boolean));
    }

    #[test]
    fn flow_collections_and_compact_nesting() {
        let s = FIXTURES[6].1;
        let t = parse(s);
        let a = named(s, &t, "a");
        assert_eq!(a.kind, Kind::Array);
        assert_eq!(a.child_count(), 4);
        assert_eq!(text(s, &a.span), "a: [1, \"two\", {k: v, \"q\":x}, []]");
        assert_eq!(text(s, named(s, &t, "\"q\"").value.as_ref().unwrap()), "x");
        assert_eq!(named(s, &t, "z").scalar, Some(Scalar::Null));
        assert_eq!(named(s, &t, "k2").scalar, Some(Scalar::String));
        let c = (1..t.len() as u32)
            .find(|&id| t.node(id).name.as_ref().is_some_and(|n| text(s, n) == "c"))
            .unwrap();
        assert_eq!(t.node(c).child_count(), 2);
        let first = t.children(c).next().unwrap();
        assert_eq!(t.node(first).kind, Kind::Array);
        assert_eq!(t.node(first).child_count(), 2);
    }

    #[test]
    fn scalars_block_and_multiline_stay_raw() {
        let s = FIXTURES[5].1;
        let t = parse(s);
        for (key, raw) in [
            ("lit", "|\n  line one\n    indented\n  line three"),
            ("fold", ">-\n  a\n  b\n\n  c"),
            ("plain", "first\n  second\n\n  third"),
            ("dq", "\"x\\ty\n  z\""),
            ("explicit", "|2-\n  literal"),
            ("next", "1"),
        ] {
            assert_eq!(text(s, named(s, &t, key).value.as_ref().unwrap()), raw);
        }
    }

    #[test]
    fn core_schema_classifies_without_converting() {
        for (raw, scalar) in [
            ("~", Scalar::Null),
            ("null", Scalar::Null),
            ("NULL", Scalar::Null),
            ("true", Scalar::Boolean),
            ("False", Scalar::Boolean),
            ("TRUE", Scalar::Boolean),
            ("12", Scalar::Number),
            ("-12", Scalar::Number),
            ("+1.2e-3", Scalar::Number),
            (".5", Scalar::Number),
            ("1.", Scalar::Number),
            (".inf", Scalar::Number),
            ("-.INF", Scalar::Number),
            (".NaN", Scalar::Number),
            ("0xFF", Scalar::Number),
            ("0o17", Scalar::Number),
            ("012", Scalar::Number),
            ("0x", Scalar::String),
            ("0o8", Scalar::String),
            ("1e", Scalar::String),
            ("1_000", Scalar::String),
            ("yes", Scalar::String),
            ("on", Scalar::String),
            ("2026-10-30", Scalar::String),
            ("'null'", Scalar::String),
            ("\"12\"", Scalar::String),
        ] {
            let t = parse(raw);
            assert_eq!(t.stopped, None, "{raw}");
            assert_eq!(t.node(1).scalar, Some(scalar), "{raw}");
            assert_eq!(text(raw, t.node(1).value.as_ref().unwrap()), raw);
        }
    }

    #[test]
    fn anchors_tags_and_aliases_are_never_expanded() {
        let s = FIXTURES[3].1;
        let t = parse(s);
        for (key, raw, kind) in [
            ("base", "&base !!map", Kind::Object),
            ("tagged", "&a !!str 12", Kind::Scalar),
            ("list", "&l", Kind::Array),
            ("other", "*base", Kind::Alias),
        ] {
            let n = named(s, &t, key);
            assert_eq!(n.kind, kind);
            assert_eq!(text(s, n.value.as_ref().unwrap()), raw);
        }
        assert_eq!(named(s, &t, "tagged").scalar, Some(Scalar::Number));
        assert_eq!(named(s, &t, "other").child_count(), 0);
        let t = parse(FIXTURES[4].1);
        assert_eq!(t.len(), 34);
        let mut bomb = String::from("a: &a [x, x, x, x, x, x, x, x, x]\n");
        for i in 1..30 {
            let prev = if i == 1 {
                "a".into()
            } else {
                format!("k{}", i - 1)
            };
            bomb.push_str(&format!(
                "k{i}: &k{i} [{}]\n",
                vec![format!("*{prev}"); 9].join(", ")
            ));
        }
        let t = parse(&bomb);
        assert_eq!(t.stopped, None);
        assert_eq!(t.len(), 302);
        check_spans(&bomb, &t);
    }

    #[test]
    fn multiple_documents_and_directives() {
        let s = FIXTURES[2].1;
        let t = parse(s);
        let docs: Vec<_> = t.children(ROOT).collect();
        assert_eq!(docs.len(), 4);
        for &id in &docs {
            assert_eq!(t.node(id).kind, Kind::Part);
        }
        assert_eq!(text(s, &t.node(docs[0]).span), "---\na: 1\n...");
        assert_eq!(t.node(docs[2]).child_count(), 0);
        assert_eq!(
            text(
                s,
                t.node(t.children(docs[3]).next().unwrap())
                    .value
                    .as_ref()
                    .unwrap()
            ),
            "plain"
        );
        assert_eq!(parse("---\nk: v\n").node(1).kind, Kind::Object);
        assert!(parse("# nothing\n\n").is_empty());
        assert!(parse("---\n").is_empty());
        // Marker-looking lines inside quoted scalars do not introduce documents.
        let t = parse("\"a\n---\nb\"\n");
        assert_eq!(t.node(1).kind, Kind::Scalar);
        let t = parse("a: 1\n---\nb: 2\n");
        assert_eq!(t.node(ROOT).child_count(), 2);
        assert_eq!(t.node(1).kind, Kind::Part);
    }

    #[test]
    fn duplicate_keys_are_reported_not_hidden() {
        let s = "a: 1\nb: 2\na: 3\n\"b\": 4\nm: {x: 1, x: 2}\n";
        let t = parse(s);
        assert_eq!(t.stopped, None);
        assert_eq!(t.diagnostics.len(), 3);
        for (d, raw) in t.diagnostics.iter().zip(["a", "\"b\"", "x"]) {
            assert_eq!(d.severity, Severity::Warning);
            assert_eq!(text(s, &d.span), raw);
            assert!(d.message.contains("Duplicate key"));
        }
        assert_eq!(
            t.diagnostics[0].span.start as usize,
            s.find("a: 3").unwrap()
        );
        assert_eq!(
            t.diagnostics[1].span.start as usize,
            s.find("\"b\"").unwrap()
        );
        let names: Vec<_> = (1..t.len() as u32)
            .filter_map(|id| t.node(id).name.as_ref().map(|r| text(s, r)))
            .collect();
        assert_eq!(names, ["a", "b", "a", "\"b\"", "m", "x", "x"]);
        for s in [
            "é: 1\n\"\\u00e9\": 2\n",
            "'it''s': 1\n\"it's\": 2\n",
            "a: {x: 1}\nb: {x: 2}\n",
        ] {
            let t = parse(s);
            assert_eq!(t.diagnostics.len(), usize::from(!s.starts_with("a:")));
        }
        assert_eq!(decode_scalar("\"\\u00e9\\x41\\\\\""), "éA\\");
    }

    #[test]
    fn malformed_inputs_keep_prefix_and_report_position() {
        for (s, at, message) in [
            ("a: 1\nb: [1, 2\nc: 3\n", ": 3", "Single-pair"),
            ("a: 1\nb: \"open\n", "\"open", "Unterminated quoted"),
            ("a: b: c\n", ": c", "Mapping values"),
            ("a:\n  b: 1\n c: 2\n", "c: 2", "Unexpected indentation"),
            ("a: 1\n- x\n", "- x", "sequence entry"),
            ("a: 1\n\tb: 2\n", "b: 2", "Tab characters"),
            ("a: {x: 1]\n", "]", "',' or '}'"),
            ("k: v\nplain\n", "plain", "mapping key followed"),
            ("a: @x\n", "@x", "value was expected"),
            ("? complex\n: value\n", "? complex", "Explicit '?'"),
            ("a: |x\n  t\n", "x\n", "Invalid block scalar"),
            ("a: {[x]: y}", "[x]", "Flow collections as mapping keys"),
            ("a: [x: y]", ": y", "Single-pair mappings"),
            ("a: [x, y", "[x", "Unclosed flow collection"),
        ] {
            let t = parse(s);
            assert_eq!(t.stopped, Some(TreeStop::Error), "{s:?}");
            assert_eq!(t.errors(), 1, "{s:?}");
            let d = &t.diagnostics[0];
            assert_eq!(d.span.start as usize, s.find(at).unwrap(), "{s:?}: {d:?}");
            assert!(d.message.contains(message), "{s:?}: {d:?}");
            check_spans(s, &t);
        }
    }

    #[test]
    fn depth_node_and_diagnostic_limits_are_enforced() {
        let flow = "[".repeat(300) + &"]".repeat(300);
        let block: String = (0..300).map(|i| format!("{}k:\n", " ".repeat(i))).collect();
        for s in [flow, block] {
            let t = parse(&s);
            assert_eq!(t.stopped, Some(TreeStop::Depth));
            check_spans(&s, &t);
        }
        let s = format!("[{}]", "a, ".repeat(100));
        let limits = Limits {
            max_depth: 10,
            max_nodes: 50,
            max_diagnostics: 2,
        };
        let t = Yaml.parse(&s, &limits, &AtomicBool::new(false));
        assert_eq!(t.len(), 50);
        assert_eq!(t.stopped, Some(TreeStop::Nodes));
        check_spans(&s, &t);
        let s = "a: 1\n".repeat(10);
        let t = Yaml.parse(&s, &limits, &AtomicBool::new(false));
        assert_eq!(t.diagnostics.len(), 2);
        assert_eq!(t.more_diagnostics, 7);
        for max_depth in [0, 1, 2] {
            let t = Yaml.parse(
                "[[]]",
                &Limits {
                    max_depth,
                    ..Limits::default()
                },
                &AtomicBool::new(false),
            );
            assert_eq!(
                t.stopped,
                if max_depth < 2 {
                    Some(TreeStop::Depth)
                } else {
                    None
                }
            );
        }
    }

    #[test]
    fn stream_restart_keeps_diagnostic_and_node_budgets() {
        let source = "a: 1\na: 2\n---\nb: 3\n";
        let t = parse(source);
        assert_eq!(
            t.diagnostics.len(),
            1,
            "the first document's warning appears once"
        );
        assert_eq!(t.node(ROOT).child_count(), 2);
        let limits = Limits {
            max_nodes: 6,
            ..Limits::default()
        };
        let t = Yaml.parse(source, &limits, &AtomicBool::new(false));
        assert_eq!(t.stopped, Some(TreeStop::Nodes));
        assert_eq!(t.len(), 6);
        assert_eq!(t.node(ROOT).child_count(), 2);
        check_spans(source, &t);
    }

    #[test]
    fn cancellation_stops_before_parsing() {
        for s in ["", "a: 1\nb: 2\n", "# nothing\n", "[a, b]"] {
            let t = Yaml.parse(s, &Limits::default(), &AtomicBool::new(true));
            assert_eq!(t.stopped, Some(TreeStop::Cancelled));
            assert_eq!(t.len(), 1);
            assert!(t.diagnostics.is_empty());
        }
    }

    #[test]
    fn cancellation_interrupts_token_and_whitespace_scanners() {
        let cancel = AtomicBool::new(false);
        let limits = Limits::default();
        let source = " ".repeat(16_384);
        let mut p = Parser::new(&source, &limits, &cancel, false);
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(p.skip_blank(), Err(Fail::Cancelled)));
        assert!(matches!(p.skip_inline(0), Err(Fail::Cancelled)));
        assert!(matches!(p.skip_spaces(0), Err(Fail::Cancelled)));
        assert!(matches!(p.skip_flow_blank(), Err(Fail::Cancelled)));
        assert!(matches!(p.line_end(0), Err(Fail::Cancelled)));
        for source in [
            "\"long quoted scalar\"",
            "long plain scalar",
            "&anchor",
            "*alias",
            "!<tag>",
        ] {
            let p = Parser::new(source, &limits, &cancel, false);
            assert!(matches!(p.quoted_end(0), Err(Fail::Cancelled)));
            assert!(matches!(p.plain_end(0, -1), Err(Fail::Cancelled)));
            assert!(matches!(p.flow_plain_end(0), Err(Fail::Cancelled)));
            assert!(matches!(p.prop_end(0, false), Err(Fail::Cancelled)));
            assert!(matches!(p.alias_end(0), Err(Fail::Cancelled)));
            assert!(matches!(p.implicit_key(0), Err(Fail::Cancelled)));
        }
    }

    #[test]
    fn crlf_bom_unicode_and_cursor_paths() {
        let s = "\u{FEFF}a: 1\r\nb:\r\n  - 日😀\r\n";
        let t = parse(s);
        assert_eq!(t.stopped, None);
        check_spans(s, &t);
        let path = t.path_at(s.find('日').unwrap() as u32);
        assert_eq!(path.len(), 4);
        assert_eq!(
            text(s, t.node(*path.last().unwrap()).value.as_ref().unwrap()),
            "日😀"
        );
        assert_eq!(text(s, &named(s, &t, "a").span), "a: 1");
    }

    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    #[test]
    fn random_and_mutated_input_never_panics() {
        let alphabet = [
            "-", "- ", ":", ": ", "?", "[", "]", "{", "}", ",", "#", " #", "&a", "*a", "!t",
            "!!str", "|", ">", "|-", ">+2", "\"", "'", "\\", "\n", "\n  ", "\r\n", "\t", " ", "  ",
            "a", "k", "1", "~", "---", "...", "%YAML", "é", "日", "😀", "\u{FEFF}", "\\u00", "''",
        ];
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for round in 0..30_000 {
            let mut s: String = if round % 2 == 0 {
                FIXTURES[rng.below(FIXTURES.len())].1.into()
            } else {
                (0..rng.below(32) + 1)
                    .map(|_| alphabet[rng.below(alphabet.len())])
                    .collect()
            };
            for _ in 0..rng.below(4) {
                let boundaries: Vec<_> = s
                    .char_indices()
                    .map(|(i, _)| i)
                    .chain(std::iter::once(s.len()))
                    .collect();
                let i = rng.below(boundaries.len());
                let at = boundaries[i];
                if rng.below(2) == 0 && i + 1 < boundaries.len() {
                    s.replace_range(at..boundaries[i + 1], "");
                } else {
                    s.insert_str(at, alphabet[rng.below(alphabet.len())]);
                }
            }
            let t = parse(&s);
            check_spans(&s, &t);
            let _ = dump(&t, &s);
            let _ = t.path_at(rng.below(s.len() + 1) as u32);
        }
    }
}
