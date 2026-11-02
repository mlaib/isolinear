//! Shared pieces of the delimited EDI readers (UN/EDIFACT, ANSI X12, HL7 v2, Cargo-IMP):
//! segments split by a terminator, element and component separators with an optional release
//! character, values decoded as UTF-8 (or ISO 8859-1 when they are not UTF-8), count tallies,
//! GS1 check digits and hierarchies shown as outlines.
//!
//! Limits: at most [`MAX_SEGMENTS`] segments are read (the rest is reported as skipped), every
//! loop is bounded by the input, and the cancel flag is polled while splitting.
use crate::inspect::{Inspection, MAX_ITEMS, MAX_ROWS, OutlineNode, Row, Stage, State, Table};
use std::{collections::HashMap, ops::Range};

/// Segments read from one file; more are skipped and reported.
pub(crate) const MAX_SEGMENTS: usize = 1_000_000;
/// Characters of a segment shown as an outline value or table cell.
pub(crate) const SHOWN: usize = 300;
/// Deepest hierarchy level drawn (deeper levels are drawn at this depth).
const MAX_DEPTH: usize = 64;

/// Delimiters of a delimited EDI syntax.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Seps {
    pub elem: u8,
    pub comp: u8,
    pub rep: Option<u8>,
    /// Release (escape) character: the next byte is taken literally.
    pub rel: Option<u8>,
    pub term: u8,
}

/// One segment: its bytes (without the terminator) and its elements, the tag first.
pub(crate) struct Seg {
    pub start: u32,
    /// End of the segment content (exclusive), before the terminator.
    pub end: u32,
    /// End including the terminator.
    pub stop: u32,
    pub els: Vec<(u32, u32)>,
}

pub(crate) struct Doc<'a> {
    pub bytes: &'a [u8],
    pub seps: Seps,
    pub segs: Vec<Seg>,
    /// More than [`MAX_SEGMENTS`] segments: the rest was not read.
    pub truncated: bool,
    /// The last segment has no terminator (the file is cut or the terminator is missing).
    pub unterminated: bool,
}

/// Text of bytes: UTF-8 when valid, otherwise ISO 8859-1 (one character per byte).
pub(crate) fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// At most `n` characters of `s`, with an ellipsis when cut.
pub(crate) fn clip(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// Offset of the first byte after a UTF-8 byte order mark and ASCII white space.
pub(crate) fn content_start(bytes: &[u8]) -> usize {
    let mut i = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    };
    while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
        i += 1;
    }
    i
}

impl<'a> Doc<'a> {
    /// Split `bytes[from..]` into segments. White space between segments (line breaks after
    /// terminators) is skipped unless it is itself a separator.
    pub fn split(
        bytes: &'a [u8],
        from: usize,
        seps: Seps,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, String> {
        if bytes.len() > u32::MAX as usize {
            return Err("The file is too large for this reader".into());
        }
        let is_sep = |b: u8| b == seps.elem || b == seps.comp || b == seps.term;
        let mut segs = vec![];
        let mut pos = from;
        let mut truncated = false;
        let mut unterminated = false;
        let len = bytes.len();
        while pos < len {
            while pos < len && bytes[pos].is_ascii_whitespace() && !is_sep(bytes[pos]) {
                pos += 1;
            }
            if pos >= len {
                break;
            }
            if segs.len() >= MAX_SEGMENTS {
                truncated = true;
                break;
            }
            if segs.len() % 4096 == 4095 && cancelled() {
                return Err("Cancelled".into());
            }
            let start = pos;
            let mut els = vec![];
            let mut el_start = pos;
            let mut i = pos;
            let mut found = false;
            while i < len {
                let b = bytes[i];
                if Some(b) == seps.rel {
                    i += 2;
                    continue;
                }
                if b == seps.term {
                    found = true;
                    break;
                }
                if b == seps.elem {
                    els.push((el_start as u32, i as u32));
                    el_start = i + 1;
                }
                i += 1;
            }
            let i = i.min(len);
            let mut end = i;
            // A segment ended by a line feed may carry the carriage return of a CRLF.
            if end > el_start && bytes[end - 1] == b'\r' && !is_sep(b'\r') {
                end -= 1;
            }
            els.push((el_start as u32, end.max(el_start) as u32));
            if !found {
                unterminated = true;
            }
            let stop = if found { i + 1 } else { i };
            segs.push(Seg {
                start: start as u32,
                end: end as u32,
                stop: stop as u32,
                els,
            });
            pos = stop;
        }
        Ok(Self {
            bytes,
            seps,
            segs,
            truncated,
            unterminated,
        })
    }

    fn slice(&self, (a, b): (u32, u32)) -> &'a [u8] {
        self.bytes.get(a as usize..b as usize).unwrap_or(&[])
    }

    /// The segment tag as written ("BGM"), or "?" when it is not text.
    pub fn tag(&self, s: &Seg) -> &'a str {
        let raw = s.els.first().map(|&r| self.slice(r)).unwrap_or(&[]);
        std::str::from_utf8(raw).unwrap_or("?")
    }

    /// Data elements after the tag.
    pub fn count(&self, s: &Seg) -> usize {
        s.els.len().saturating_sub(1)
    }

    /// Element `i` (1-based, after the tag) as written.
    pub fn raw(&self, s: &Seg, i: usize) -> &'a [u8] {
        s.els.get(i).map(|&r| self.slice(r)).unwrap_or(&[])
    }

    /// Split raw bytes at `sep`, honouring the release character.
    fn split_at(&self, raw: &'a [u8], sep: u8) -> Vec<&'a [u8]> {
        let mut out = vec![];
        let mut start = 0;
        let mut i = 0;
        while i < raw.len() {
            if Some(raw[i]) == self.seps.rel {
                i += 2;
                continue;
            }
            if raw[i] == sep {
                out.push(&raw[start..i]);
                start = i + 1;
            }
            i += 1;
        }
        out.push(raw.get(start..).unwrap_or(&[]));
        out
    }

    /// Bytes with release characters removed, decoded.
    pub fn unescape(&self, raw: &[u8]) -> String {
        match self.seps.rel {
            Some(rel) if raw.contains(&rel) => {
                let mut v = Vec::with_capacity(raw.len());
                let mut i = 0;
                while i < raw.len() {
                    if raw[i] == rel {
                        if let Some(&next) = raw.get(i + 1) {
                            v.push(next);
                        }
                        i += 2;
                    } else {
                        v.push(raw[i]);
                        i += 1;
                    }
                }
                decode(&v)
            }
            _ => decode(raw),
        }
    }

    /// Element `i` (1-based) with releases removed; component separators are kept as written.
    pub fn el(&self, s: &Seg, i: usize) -> String {
        self.unescape(self.raw(s, i)).trim_end().to_owned()
    }

    /// Component `j` (1-based) of element `i`; the first repetition when repeated.
    pub fn comp(&self, s: &Seg, i: usize, j: usize) -> String {
        let mut raw = self.raw(s, i);
        if let Some(rep) = self.seps.rep {
            raw = self.split_at(raw, rep).into_iter().next().unwrap_or(&[]);
        }
        self.split_at(raw, self.seps.comp)
            .get(j.wrapping_sub(1))
            .map(|c| self.unescape(c).trim_end().to_owned())
            .unwrap_or_default()
    }

    /// All components of element `i` (first repetition), unescaped.
    pub fn comps(&self, s: &Seg, i: usize) -> Vec<String> {
        let mut raw = self.raw(s, i);
        if let Some(rep) = self.seps.rep {
            raw = self.split_at(raw, rep).into_iter().next().unwrap_or(&[]);
        }
        self.split_at(raw, self.seps.comp)
            .into_iter()
            .map(|c| self.unescape(c).trim_end().to_owned())
            .collect()
    }

    /// Repetitions of element `i`, each as written with releases removed.
    pub fn reps(&self, s: &Seg, i: usize) -> Vec<String> {
        let raw = self.raw(s, i);
        match self.seps.rep {
            Some(rep) => self
                .split_at(raw, rep)
                .into_iter()
                .map(|r| self.unescape(r))
                .collect(),
            None => vec![self.unescape(raw)],
        }
    }

    /// The segment as written, without its terminator.
    pub fn text(&self, s: &Seg) -> String {
        decode(self.slice((s.start, s.end)))
    }

    pub fn src(&self, s: &Seg) -> Option<Range<u64>> {
        Some(s.start as u64..s.stop as u64)
    }

    /// From the start of segment `a` to the end of segment `b`.
    pub fn span(&self, a: &Seg, b: &Seg) -> Option<Range<u64>> {
        Some(a.start as u64..(b.stop.max(a.start)) as u64)
    }
}

/// Compare declared counts or control numbers with what was found, as one business-rule check.
pub(crate) struct Tally {
    what: String,
    ok: usize,
    first: Option<String>,
    bad: Vec<String>,
    bad_count: usize,
}

/// Whether two numbers written as text are equal ("000905" and "905" are).
pub(crate) fn same_number(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    if !a.is_empty()
        && a.bytes().all(|c| c.is_ascii_digit())
        && b.bytes().all(|c| c.is_ascii_digit())
    {
        let (x, y) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
        return x == y;
    }
    a == b
}

impl Tally {
    pub fn new(what: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            ok: 0,
            first: None,
            bad: vec![],
            bad_count: 0,
        }
    }
    /// One comparison; `ctx` names the item ("message ME001").
    pub fn compare(&mut self, ctx: &str, declared: &str, counted: &str) {
        let detail = format!("{ctx}: declared {declared}, found {counted}");
        if same_number(declared, counted) {
            self.ok += 1;
            self.first.get_or_insert(detail);
        } else {
            self.fail(detail);
        }
    }
    pub fn what(&self) -> &str {
        &self.what
    }
    pub fn pass(&mut self, detail: String) {
        self.ok += 1;
        self.first.get_or_insert(detail);
    }
    pub fn fail(&mut self, detail: String) {
        self.bad_count += 1;
        if self.bad.len() < 5 {
            self.bad.push(detail);
        }
    }
    pub fn emit(self, out: &mut Inspection) {
        let total = self.ok + self.bad_count;
        if total == 0 {
            return;
        }
        if self.bad_count == 0 {
            let detail = match (self.ok, self.first) {
                (1, Some(f)) => format!("{} ({f})", self.what),
                _ => format!("{}: all {} match", self.what, self.ok),
            };
            out.check(Stage::Semantic, State::Passed, detail);
        } else {
            let more = if self.bad_count > self.bad.len() {
                format!("; {} more", self.bad_count - self.bad.len())
            } else {
                String::new()
            };
            out.check(
                Stage::Semantic,
                State::Failed,
                format!(
                    "{}: {} of {} differ — {}{more}",
                    self.what,
                    self.bad_count,
                    total,
                    self.bad.join("; ")
                ),
            );
        }
    }
}

/// GS1 key (GTIN-8/12/13/14, GLN, SSCC) check digit: `None` when `s` is not shaped like one.
pub(crate) fn gs1_valid(s: &str) -> Option<bool> {
    let d = s.as_bytes();
    if !matches!(d.len(), 8 | 12 | 13 | 14 | 17 | 18) || !d.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let (body, check) = d.split_at(d.len() - 1);
    let sum: u32 = body
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &c)| u32::from(c - b'0') * if i % 2 == 0 { 3 } else { 1 })
        .sum();
    Some((10 - sum % 10) % 10 == u32::from(check[0] - b'0'))
}

/// GS1 identifiers found in a file, checked and listed together.
#[derive(Default)]
pub(crate) struct Gs1 {
    rows: Vec<Row>,
    ok: usize,
    bad: Vec<String>,
    bad_count: usize,
}
impl Gs1 {
    /// `kind` is "GLN", "GTIN" or "SSCC"; `place` where it was found ("NAD+BY").
    pub fn add(&mut self, kind: &str, value: &str, place: &str, source: Option<Range<u64>>) {
        let value = value.trim();
        if value.is_empty() {
            return;
        }
        let state = match gs1_valid(value) {
            Some(true) => {
                self.ok += 1;
                "valid"
            }
            Some(false) => {
                self.bad_count += 1;
                if self.bad.len() < 5 {
                    self.bad
                        .push(format!("{kind} {value} ({place}) has a wrong check digit"));
                }
                "wrong check digit"
            }
            None => {
                self.bad_count += 1;
                if self.bad.len() < 5 {
                    self.bad
                        .push(format!("{kind} {value} ({place}) is not a GS1 key length"));
                }
                "not a GS1 key"
            }
        };
        if self.rows.len() < MAX_ROWS {
            self.rows.push(Row {
                cells: vec![kind.into(), value.into(), place.into(), state.into()],
                source,
            });
        }
    }
    pub fn emit(self, out: &mut Inspection) {
        let total = self.ok + self.bad_count;
        if total == 0 {
            return;
        }
        out.tables.push(Table {
            name: "GS1 identifiers".into(),
            columns: vec![
                "Kind".into(),
                "Identifier".into(),
                "Found in".into(),
                "Check digit".into(),
            ],
            rows: self.rows,
            total: (total > MAX_ROWS).then_some(total as u64),
        });
        if self.bad_count == 0 {
            out.check(
                Stage::Semantic,
                State::Passed,
                format!("GS1 check digits: all {total} identifiers valid"),
            );
        } else {
            out.check(
                Stage::Semantic,
                State::Failed,
                format!(
                    "GS1 check digits: {} of {total} identifiers fail — {}",
                    self.bad_count,
                    self.bad.join("; ")
                ),
            );
        }
    }
}

/// Outline entries, capped at [`MAX_ITEMS`].
pub(crate) fn node(
    out: &mut Inspection,
    depth: usize,
    label: impl Into<String>,
    value: impl Into<String>,
    source: Option<Range<u64>>,
) -> bool {
    if out.outline.len() >= MAX_ITEMS {
        return false;
    }
    out.outline.push(OutlineNode {
        depth: depth.min(u16::MAX as usize) as u16,
        label: label.into(),
        value: value.into(),
        source,
    });
    true
}

/// A level of a hierarchy given by parent links (X12 HL, EDIFACT CPS).
pub(crate) struct Level {
    pub id: String,
    pub parent: String,
    pub label: String,
    pub value: String,
    pub source: Option<Range<u64>>,
}

/// The levels as an outline under `title`, children under their parents. Levels whose parent
/// is unknown are roots; cycles are cut; depth is capped.
pub(crate) fn hierarchy(out: &mut Inspection, title: &str, levels: &[Level]) {
    if levels.is_empty() {
        return;
    }
    node(out, 0, title, format!("{} levels", levels.len()), None);
    let mut first: HashMap<&str, usize> = HashMap::new();
    for (i, l) in levels.iter().enumerate() {
        first.entry(l.id.as_str()).or_insert(i);
    }
    let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut roots = vec![];
    for (i, l) in levels.iter().enumerate() {
        let parent = l.parent.trim();
        if parent.is_empty() || parent == l.id || !first.contains_key(parent) {
            roots.push(i);
        } else {
            children.entry(parent).or_default().push(i);
        }
    }
    let mut seen = vec![false; levels.len()];
    let emit = |start: usize, out: &mut Inspection, seen: &mut Vec<bool>| {
        let mut stack = vec![(start, 1usize)];
        while let Some((i, depth)) = stack.pop() {
            if seen[i] {
                continue;
            }
            seen[i] = true;
            let l = &levels[i];
            if !node(
                out,
                depth.min(MAX_DEPTH),
                &l.label,
                &l.value,
                l.source.clone(),
            ) {
                return;
            }
            // Only the first level with an id owns its children (ids should be unique).
            if first.get(l.id.as_str()) == Some(&i)
                && let Some(kids) = children.get(l.id.as_str())
            {
                for &k in kids.iter().rev() {
                    stack.push((k, depth + 1));
                }
            }
        }
    };
    for r in roots {
        emit(r, out, &mut seen);
    }
    // Levels caught in a parent cycle.
    for i in 0..levels.len() {
        if !seen[i] {
            emit(i, out, &mut seen);
        }
    }
}

/// "20261101" -> "20261101 (2026-11-01)", "202611011030" -> "… (2026-11-01 10:30)"; other
/// values as written.
pub(crate) fn date(raw: &str) -> String {
    let r = raw.trim();
    let digits = r.bytes().all(|b| b.is_ascii_digit());
    if !digits || !matches!(r.len(), 8 | 12 | 14) {
        return raw.to_owned();
    }
    let (y, m, d) = (&r[..4], &r[4..6], &r[6..8]);
    let ok = matches!(m.parse::<u8>(), Ok(1..=12)) && matches!(d.parse::<u8>(), Ok(1..=31));
    if !ok {
        return raw.to_owned();
    }
    let mut s = format!("{y}-{m}-{d}");
    if r.len() >= 12 {
        s.push_str(&format!(" {}:{}", &r[8..10], &r[10..12]));
    }
    if r.len() == 14 {
        s.push_str(&format!(":{}", &r[12..14]));
    }
    format!("{r} ({s})")
}

/// "code" or "code (name)" from a lookup.
pub(crate) fn coded(code: &str, name: Option<&str>) -> String {
    match name {
        Some(n) if !code.is_empty() => format!("{code} ({n})"),
        _ => code.to_owned(),
    }
}

/// A table with rows capped at [`MAX_ROWS`] and the total kept.
pub(crate) struct Rows {
    pub table: Table,
    count: u64,
}
impl Rows {
    pub fn new(name: &str, columns: &[&str]) -> Self {
        Self {
            table: Table {
                name: name.into(),
                columns: columns.iter().map(|c| (*c).into()).collect(),
                rows: vec![],
                total: None,
            },
            count: 0,
        }
    }
    pub fn push(&mut self, cells: Vec<String>, source: Option<Range<u64>>) {
        self.count += 1;
        if self.table.rows.len() < MAX_ROWS {
            self.table.rows.push(Row { cells, source });
        }
    }
    pub fn len(&self) -> u64 {
        self.count
    }
    pub fn emit(mut self, out: &mut Inspection) {
        if self.table.rows.is_empty() {
            return;
        }
        if self.count > self.table.rows.len() as u64 {
            self.table.total = Some(self.count);
        }
        out.tables.push(self.table);
    }
}

/// `-x` for a decimal written as text.
pub(crate) fn negate(x: &str) -> String {
    let x = x.trim();
    match x.strip_prefix('-') {
        Some(rest) => rest.to_owned(),
        None => format!("-{}", x.trim_start_matches('+')),
    }
}

/// The segments as an outline (envelopes nesting what they enclose, each segment with its
/// non-empty elements numbered "BGM01") and as a table.
pub(crate) fn segments_view(
    doc: &Doc,
    out: &mut Inspection,
    opens: &[&str],
    closes: &[&str],
    name: fn(&str) -> Option<&'static str>,
) {
    let mut table = Rows::new("Segments", &["#", "Tag", "Name", "Segment"]);
    let mut level = 0usize;
    let mut full = false;
    for (n, s) in doc.segs.iter().enumerate() {
        let tag = doc.tag(s);
        let label = match name(tag) {
            Some(nm) => format!("{tag} — {nm}"),
            None => tag.to_owned(),
        };
        let text = clip(&doc.text(s), SHOWN);
        table.push(
            vec![
                (n + 1).to_string(),
                tag.to_owned(),
                name(tag).unwrap_or("").to_owned(),
                text.clone(),
            ],
            doc.src(s),
        );
        let depth = level;
        if opens.contains(&tag) {
            level = (level + 1).min(MAX_DEPTH);
        } else if closes.contains(&tag) {
            level = level.saturating_sub(1);
        }
        let depth = if closes.contains(&tag) {
            depth.max(1)
        } else {
            depth
        };
        if full {
            continue;
        }
        if !node(out, depth, label, text, doc.src(s)) {
            full = true;
            continue;
        }
        for i in 1..=doc.count(s) {
            let raw = doc.raw(s, i);
            if raw.is_empty() {
                continue;
            }
            let (a, b) = s.els[i];
            let v = clip(&doc.unescape(raw), SHOWN);
            if !node(
                out,
                depth + 1,
                format!("{tag}{i:02}"),
                v,
                Some(a as u64..b as u64),
            ) {
                full = true;
                break;
            }
        }
    }
    if full {
        out.notes.push(format!(
            "The outline lists the first {MAX_ITEMS} segments and elements"
        ));
    }
    table.emit(out);
}
