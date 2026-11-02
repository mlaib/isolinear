//! Structure trees: what the Tree view shows, built from source text by one parser per format.
//!
//! A tree never owns text. Every node holds byte spans into the source it was parsed from, so
//! selecting a node selects exactly its source and labels show the text as written. Parsers
//! emit nodes in source order through a [`Builder`], which enforces the depth and node limits
//! and polls cancellation. Malformed input keeps the part that parsed, plus diagnostics.
//!
//! Adding a format: implement [`TreeFormat`] in a module of this directory and return it from
//! [`for_format`]. See `json.rs` for the reference implementation and its tests.

use std::fmt::Write as _;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

pub mod bibtex;
pub mod code;
pub mod ini;
pub mod json;
pub mod jsonl;
pub mod markdown;
pub mod tex;
pub mod toml;
pub mod xml;
pub mod yaml;

/// Byte range into the source. Sources are at most 4 GiB.
pub type Span = Range<u32>;
pub type NodeId = u32;

const NONE: u32 = u32::MAX;

/// The parser for a format id of the registry, when it has a tree or outline view. Source code
/// (`code`) has one per language.
pub fn for_format(id: &str, language: Option<&str>) -> Option<&'static dyn TreeFormat> {
    match id {
        "code" => code::for_language(language?),
        // MessagePack and Protobuf are shown as the JSON text they decode to.
        "json" | "notebook" | "msgpack" | "protobuf" => json::FORMAT,
        "jsonl" => jsonl::FORMAT,
        "xml" => xml::FORMAT,
        "yaml" => yaml::FORMAT,
        "toml" => toml::FORMAT,
        "ini" => ini::FORMAT,
        "markdown" => markdown::FORMAT,
        "tex" => tex::FORMAT,
        "bibtex" => bibtex::FORMAT,
        _ => None,
    }
}

pub trait TreeFormat: Sync {
    /// Parses `source` into a tree. Never panics on any input; stops early (keeping what was
    /// built) at a fatal error, a limit, or when `cancel` is set.
    fn parse(&self, source: &str, limits: &Limits, cancel: &AtomicBool) -> Tree;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// The root; spans the whole source.
    Document,
    /// JSON object, YAML mapping, TOML inline table.
    Object,
    /// JSON array, YAML sequence, TOML array.
    Array,
    /// TOML `[table]` or `[[array of tables]]`, INI `[section]`.
    Table,
    /// A YAML document in a stream of several.
    Part,
    /// One JSON Lines record.
    Record,
    Element,
    Attribute,
    Text,
    CData,
    Comment,
    ProcessingInstruction,
    Doctype,
    /// A YAML alias (`*name`); never expanded.
    Alias,
    /// A leaf value; see [`Scalar`].
    Scalar,
    /// A heading and what follows it up to the next heading of the same or a higher level.
    Section,
    /// A definition in source code; `name` is its name, `value` the word that introduces it
    /// (`fn`, `class`, `def`, …).
    Symbol,
}

impl Kind {
    pub fn is_container(self) -> bool {
        matches!(
            self,
            Kind::Document
                | Kind::Object
                | Kind::Array
                | Kind::Table
                | Kind::Part
                | Kind::Record
                | Kind::Element
                | Kind::Section
                | Kind::Symbol
        )
    }
}

/// What a leaf value is, for colour; values are never converted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Scalar {
    String,
    Number,
    Boolean,
    Null,
    Date,
    Other,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: Kind,
    pub scalar: Option<Scalar>,
    /// The whole extent of the node in the source.
    pub span: Span,
    /// Key, tag or attribute name as written (with its quotes, if the source has them).
    pub name: Option<Span>,
    /// The value as written, for leaves and attributes (a string keeps its quotes).
    pub value: Option<Span>,
    parent: u32,
    first_child: u32,
    last_child: u32,
    next_sibling: u32,
    children: u32,
}

impl Node {
    pub fn parent(&self) -> Option<NodeId> {
        (self.parent != NONE).then_some(self.parent)
    }

    pub fn child_count(&self) -> u32 {
        self.children
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub span: Span,
    pub message: String,
}

/// Why a tree is incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Stop {
    /// A fatal syntax error; it is among the diagnostics.
    Error,
    Depth,
    Nodes,
    Cancelled,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_depth: u32,
    pub max_nodes: u32,
    pub max_diagnostics: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 256,
            max_nodes: 4_000_000,
            max_diagnostics: 1000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tree {
    nodes: Vec<Node>,
    pub diagnostics: Vec<Diagnostic>,
    /// Diagnostics beyond the limit, counted only.
    pub more_diagnostics: u32,
    pub stopped: Option<Stop>,
}

pub const ROOT: NodeId = 0;

impl Tree {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children {
            tree: self,
            next: self.nodes[id as usize].first_child,
        }
    }

    /// Root to the deepest node whose span contains `offset` (a span's end counts as inside,
    /// so a cursor just after a value still finds it).
    pub fn path_at(&self, offset: u32) -> Vec<NodeId> {
        // Nodes are in source order, so the last node starting at or before `offset` is the
        // deepest candidate; its ancestors are the rest of the path.
        let i = self.nodes.partition_point(|n| n.span.start <= offset);
        let mut id = i.saturating_sub(1) as u32;
        while id != ROOT && !contains(&self.nodes[id as usize].span, offset) {
            id = self.nodes[id as usize].parent;
        }
        let mut path = vec![id];
        while let Some(p) = self.nodes[*path.last().unwrap() as usize].parent() {
            path.push(p);
        }
        path.reverse();
        path
    }

    /// Position of `id` among its siblings.
    pub fn index_in_parent(&self, id: NodeId) -> u32 {
        let Some(parent) = self.node(id).parent() else {
            return 0;
        };
        self.children(parent).position(|c| c == id).unwrap_or(0) as u32
    }

    pub fn errors(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }
}

fn contains(span: &Span, offset: u32) -> bool {
    span.start <= offset && offset <= span.end
}

pub struct Children<'a> {
    tree: &'a Tree,
    next: u32,
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        if self.next == NONE {
            return None;
        }
        let id = self.next;
        self.next = self.tree.nodes[id as usize].next_sibling;
        Some(id)
    }
}

/// Parsing stopped (limit or cancellation); the parser returns `builder.finish(..)` at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped;

/// Builds a [`Tree`] in source order. Containers are opened and closed like a stack; leaves
/// are added to the innermost open container.
pub struct Builder<'c> {
    nodes: Vec<Node>,
    open: Vec<u32>,
    diagnostics: Vec<Diagnostic>,
    more_diagnostics: u32,
    stopped: Option<Stop>,
    limits: Limits,
    cancel: &'c AtomicBool,
    ops: u32,
    last_start: u32,
}

impl<'c> Builder<'c> {
    pub fn new(source_len: usize, limits: &Limits, cancel: &'c AtomicBool) -> Self {
        let root = Node {
            kind: Kind::Document,
            scalar: None,
            span: 0..source_len as u32,
            name: None,
            value: None,
            parent: NONE,
            first_child: NONE,
            last_child: NONE,
            next_sibling: NONE,
            children: 0,
        };
        Builder {
            nodes: vec![root],
            open: vec![ROOT],
            diagnostics: Vec::new(),
            more_diagnostics: 0,
            stopped: None,
            limits: *limits,
            cancel,
            ops: 0,
            last_start: 0,
        }
    }

    /// The innermost open container (the root when none is open).
    pub fn current(&self) -> NodeId {
        *self.open.last().unwrap()
    }

    /// Open containers, the root not counted.
    pub fn depth(&self) -> usize {
        self.open.len() - 1
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// Opens a container as the last child of the current one. Its span ends at [`close`].
    pub fn open(&mut self, kind: Kind, start: u32, name: Option<Span>) -> Result<NodeId, Stopped> {
        if self.depth() as u32 >= self.limits.max_depth {
            return self.stop(Stop::Depth, start, "nesting is deeper than the limit");
        }
        let id = self.push(kind, None, start..start, name, None)?;
        self.open.push(id);
        Ok(id)
    }

    /// Closes the innermost open container; its span ends at `end`.
    pub fn close(&mut self, end: u32) {
        if self.open.len() > 1 {
            let id = self.open.pop().unwrap();
            self.nodes[id as usize].span.end = end.max(self.nodes[id as usize].span.start);
        }
    }

    /// Adds a leaf to the current container.
    pub fn leaf(
        &mut self,
        kind: Kind,
        scalar: Option<Scalar>,
        span: Span,
        name: Option<Span>,
        value: Option<Span>,
    ) -> Result<NodeId, Stopped> {
        self.push(kind, scalar, span, name, value)
    }

    /// Sets the value span of a node already added (an attribute value read after its name).
    pub fn set_value(&mut self, id: NodeId, value: Span, scalar: Option<Scalar>) {
        let node = &mut self.nodes[id as usize];
        node.value = Some(value);
        node.scalar = scalar;
    }

    /// Extends a node's span (a leaf whose end is known later).
    pub fn set_end(&mut self, id: NodeId, end: u32) {
        let node = &mut self.nodes[id as usize];
        node.span.end = end.max(node.span.start);
    }

    pub fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostic(Severity::Error, span, message);
    }

    pub fn warning(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostic(Severity::Warning, span, message);
    }

    pub fn info(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostic(Severity::Info, span, message);
    }

    pub fn diagnostic(&mut self, severity: Severity, span: Span, message: impl Into<String>) {
        if self.diagnostics.len() as u32 >= self.limits.max_diagnostics {
            self.more_diagnostics += 1;
            return;
        }
        self.diagnostics.push(Diagnostic {
            severity,
            span,
            message: message.into(),
        });
    }

    /// Records a fatal error and marks the tree stopped. The parser then calls [`finish`].
    pub fn fail(&mut self, span: Span, message: impl Into<String>) {
        self.error(span, message);
        self.stopped.get_or_insert(Stop::Error);
    }

    /// Whether a limit, a fatal error or cancellation has stopped parsing.
    pub fn is_stopped(&self) -> bool {
        self.stopped.is_some()
    }

    /// The tree. Containers still open (unterminated input) end at `end`.
    pub fn finish(mut self, end: u32) -> Tree {
        while self.open.len() > 1 {
            self.close(end);
        }
        Tree {
            nodes: self.nodes,
            diagnostics: self.diagnostics,
            more_diagnostics: self.more_diagnostics,
            stopped: self.stopped,
        }
    }

    fn push(
        &mut self,
        kind: Kind,
        scalar: Option<Scalar>,
        span: Span,
        name: Option<Span>,
        value: Option<Span>,
    ) -> Result<NodeId, Stopped> {
        if self.stopped.is_some() {
            return Err(Stopped);
        }
        if self.nodes.len() as u32 >= self.limits.max_nodes {
            return self.stop(Stop::Nodes, span.start, "more nodes than the limit");
        }
        self.ops += 1;
        if self.ops.is_multiple_of(4096) && self.cancel.load(Ordering::Relaxed) {
            self.stopped = Some(Stop::Cancelled);
            return Err(Stopped);
        }
        debug_assert!(
            span.start >= self.last_start,
            "nodes must be added in source order"
        );
        self.last_start = span.start;
        let id = self.nodes.len() as u32;
        let parent = self.current();
        self.nodes.push(Node {
            kind,
            scalar,
            span,
            name,
            value,
            parent,
            first_child: NONE,
            last_child: NONE,
            next_sibling: NONE,
            children: 0,
        });
        let p = &mut self.nodes[parent as usize];
        let previous = p.last_child;
        p.last_child = id;
        p.children += 1;
        if previous == NONE {
            p.first_child = id;
        } else {
            self.nodes[previous as usize].next_sibling = id;
        }
        Ok(id)
    }

    fn stop<T>(&mut self, why: Stop, at: u32, message: &str) -> Result<T, Stopped> {
        if self.stopped.is_none() {
            self.stopped = Some(why);
            self.diagnostic(Severity::Error, at..at, message);
        }
        Err(Stopped)
    }
}

/// Converts byte offsets of a source to UTF-16 offsets (the editor's unit) and back. It does
/// not hold the source: every call takes the same text it was built from.
pub struct Offsets {
    /// UTF-16 length of each preceding 1 KiB block of the source.
    blocks: Vec<u32>,
}

const BLOCK: usize = 1024;

impl Offsets {
    pub fn new(source: &str) -> Self {
        let bytes = source.as_bytes();
        let mut blocks = Vec::with_capacity(bytes.len() / BLOCK + 1);
        let mut units = 0u32;
        blocks.push(0);
        for chunk in bytes.chunks(BLOCK) {
            units += utf16_units(chunk);
            blocks.push(units);
        }
        Offsets { blocks }
    }

    pub fn to_utf16(&self, source: &str, byte: u32) -> u32 {
        let byte = (byte as usize).min(source.len());
        let block = byte / BLOCK;
        self.blocks[block] + utf16_units(&source.as_bytes()[block * BLOCK..byte])
    }

    pub fn to_byte(&self, source: &str, utf16: u32) -> u32 {
        let block = self
            .blocks
            .partition_point(|&u| u <= utf16)
            .saturating_sub(1);
        let mut units = self.blocks[block];
        let mut byte = block * BLOCK;
        let bytes = source.as_bytes();
        // A block can start inside a character counted in the previous block.
        while byte < bytes.len() && (0x80..0xC0).contains(&bytes[byte]) {
            byte += 1;
        }
        while byte < bytes.len() && units < utf16 {
            let width = utf8_width(bytes[byte]);
            units += if width == 4 { 2 } else { 1 };
            byte += width;
        }
        byte.min(bytes.len()) as u32
    }
}

/// UTF-16 units of UTF-8 bytes: one per character start, two for 4-byte characters.
fn utf16_units(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0xBF => 0,
            0xF0..=0xFF => 2,
            _ => 1,
        })
        .sum()
}

fn utf8_width(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Longest label or value preview, in characters.
pub const PREVIEW: usize = 120;

/// Source text on one line: runs of whitespace become one space, cut at [`PREVIEW`].
pub fn preview(source: &str, span: &Span) -> String {
    let text = source
        .get(span.start as usize..span.end as usize)
        .unwrap_or("");
    let mut out = String::new();
    let mut space = false;
    for (n, c) in text.chars().enumerate() {
        if n >= PREVIEW * 4 || out.chars().count() >= PREVIEW {
            out.push('…');
            return out;
        }
        if c.is_whitespace() {
            space = !out.is_empty();
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    out
}

/// A plain-text rendering for tests and the CLI: one node per line, indented by depth, with
/// its kind, name and value as written.
pub fn dump(tree: &Tree, source: &str) -> String {
    let mut out = String::new();
    let mut stack = vec![(ROOT, 0usize)];
    while let Some((id, depth)) = stack.pop() {
        let n = tree.node(id);
        let _ = write!(out, "{}{:?}", "  ".repeat(depth), n.kind);
        if let Some(s) = n.scalar {
            let _ = write!(out, "({s:?})");
        }
        if let Some(name) = &n.name {
            let _ = write!(out, " {}", preview(source, name));
        }
        if let Some(value) = &n.value {
            let _ = write!(out, " = {}", preview(source, value));
        }
        if n.kind.is_container() {
            let _ = write!(out, " [{}]", n.children);
        }
        let _ = writeln!(out, " @{}..{}", n.span.start, n.span.end);
        let kids: Vec<_> = tree.children(id).collect();
        for k in kids.into_iter().rev() {
            stack.push((k, depth + 1));
        }
    }
    for d in &tree.diagnostics {
        let _ = writeln!(
            out,
            "{:?} @{}..{}: {}",
            d.severity, d.span.start, d.span.end, d.message
        );
    }
    if let Some(s) = tree.stopped {
        let _ = writeln!(out, "stopped: {s:?}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn builder_links_children_in_order() {
        let cancel = never();
        let mut b = Builder::new(20, &Limits::default(), &cancel);
        let obj = b.open(Kind::Object, 0, None).unwrap();
        let a = b
            .leaf(
                Kind::Scalar,
                Some(Scalar::Number),
                1..3,
                Some(1..2),
                Some(2..3),
            )
            .unwrap();
        let c = b
            .leaf(
                Kind::Scalar,
                Some(Scalar::Number),
                4..6,
                Some(4..5),
                Some(5..6),
            )
            .unwrap();
        b.close(7);
        let t = b.finish(20);
        assert_eq!(t.children(ROOT).collect::<Vec<_>>(), [obj]);
        assert_eq!(t.children(obj).collect::<Vec<_>>(), [a, c]);
        assert_eq!(t.node(obj).span, 0..7);
        assert_eq!(t.node(obj).child_count(), 2);
        assert_eq!(t.index_in_parent(c), 1);
        assert_eq!(t.path_at(5), [ROOT, obj, c]);
        assert_eq!(t.path_at(3), [ROOT, obj, a], "a span's end is inside");
        assert_eq!(t.path_at(15), [ROOT]);
    }

    #[test]
    fn limits_stop_the_builder() {
        let cancel = never();
        let limits = Limits {
            max_depth: 2,
            max_nodes: 4,
            max_diagnostics: 1,
        };
        let mut b = Builder::new(10, &limits, &cancel);
        b.open(Kind::Array, 0, None).unwrap();
        b.open(Kind::Array, 1, None).unwrap();
        assert_eq!(b.open(Kind::Array, 2, None), Err(Stopped));
        assert!(b.is_stopped());
        let t = b.finish(10);
        assert_eq!(t.stopped, Some(Stop::Depth));
        assert_eq!(t.diagnostics.len(), 1);

        let mut b = Builder::new(10, &limits, &cancel);
        for i in 0..3 {
            b.leaf(Kind::Scalar, None, i..i + 1, None, None).unwrap();
        }
        assert!(b.leaf(Kind::Scalar, None, 5..6, None, None).is_err());
        assert_eq!(b.finish(10).stopped, Some(Stop::Nodes));
    }

    #[test]
    fn unclosed_containers_end_where_parsing_stopped() {
        let cancel = never();
        let mut b = Builder::new(10, &Limits::default(), &cancel);
        let a = b.open(Kind::Array, 0, None).unwrap();
        b.fail(4..5, "boom");
        let t = b.finish(4);
        assert_eq!(t.node(a).span, 0..4);
        assert_eq!(t.stopped, Some(Stop::Error));
        assert_eq!(t.errors(), 1);
    }

    #[test]
    fn diagnostics_are_capped_and_counted() {
        let cancel = never();
        let limits = Limits {
            max_diagnostics: 2,
            ..Limits::default()
        };
        let mut b = Builder::new(1, &limits, &cancel);
        for _ in 0..5 {
            b.warning(0..1, "w");
        }
        let t = b.finish(1);
        assert_eq!(t.diagnostics.len(), 2);
        assert_eq!(t.more_diagnostics, 3);
    }

    #[test]
    fn utf16_offsets_round_trip() {
        let mut s = String::new();
        for i in 0..2000 {
            s.push_str(match i % 4 {
                0 => "a",
                1 => "é",
                2 => "€",
                _ => "😀",
            });
        }
        let o = Offsets::new(&s);
        let mut units = 0u32;
        for (byte, c) in s.char_indices() {
            assert_eq!(o.to_utf16(&s, byte as u32), units, "byte {byte}");
            assert_eq!(o.to_byte(&s, units), byte as u32);
            units += c.len_utf16() as u32;
        }
        assert_eq!(o.to_utf16(&s, s.len() as u32), units);
        assert_eq!(o.to_byte(&s, units), s.len() as u32);
    }

    #[test]
    fn previews_are_one_line_and_bounded() {
        let s = "  a\n\n  b\tc  ";
        assert_eq!(preview(s, &(0..s.len() as u32)), "a b c");
        let long = "x".repeat(500);
        let p = preview(&long, &(0..500));
        assert_eq!(p.chars().count(), PREVIEW + 1);
        assert!(p.ends_with('…'));
    }
}
