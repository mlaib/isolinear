//! Industry formats on top of the physical ones: an ISO 20022 statement is XML, an EDIFACT
//! invoice is text, a DICOM image is binary. A [`Handler`] recognizes one family from a cheap
//! [`Probe`] (extension, the first bytes, the physical format) and turns the file into an
//! [`Inspection`]: the standard and version, what was and was not checked, facts, record tables
//! and an outline, each linked to its byte range in the file, and optional visuals (a map, a
//! signal plot, a mesh). The app shows any inspection the same way, beside the raw views.
//!
//! Everything here is read-only and local: handlers get the file's bytes (bounded) and never
//! open other files, fetch anything or run anything. XML and JSON standards are described as
//! data ([`spec`]): paths to facts and to table rows, so most standards need no code.
use std::{ops::Range, path::Path};

pub mod jsondoc;
pub mod spec;
pub mod standards;
pub mod xmldoc;

/// Largest input handed to a handler in memory; larger files get their first part, marked.
pub const MAX_INPUT: u64 = 256 * 1024 * 1024;
/// Rows kept per table; the total is reported when there are more.
pub const MAX_ROWS: usize = 200_000;
/// Facts and outline entries kept.
pub const MAX_ITEMS: usize = 200_000;
/// Bytes of the file in a [`Probe`].
pub const PROBE: usize = 64 * 1024;

/// What a handler found: the standard, the checks, and what to show.
#[derive(Clone, Debug, Default)]
pub struct Inspection {
    /// Standard family: "ISO 20022", "UN/EDIFACT", "DICOM".
    pub family: String,
    /// The message or document type, readable: "camt.053 — bank-to-customer statement".
    pub kind: String,
    pub version: Option<String>,
    /// From recognizing to verifying signatures: what was done and what was not.
    pub checks: Vec<Check>,
    pub facts: Vec<Fact>,
    pub tables: Vec<Table>,
    pub outline: Vec<OutlineNode>,
    pub visuals: Vec<Visual>,
    /// Limitations and explanations shown under the facts.
    pub notes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The standard was identified (namespace, signature, structure).
    Recognized,
    /// The file was read according to the standard.
    Parsed,
    /// Checked against the standard's schema.
    Schema,
    /// Business rules (control sums, balances, counts).
    Semantic,
    /// Cryptographic signatures verified.
    Signature,
}
impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Recognized => "Recognized",
            Self::Parsed => "Parsed",
            Self::Schema => "Schema validation",
            Self::Semantic => "Business rules",
            Self::Signature => "Signature verification",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Passed,
    /// Done in part (some records read, some rules checked).
    Partial,
    Failed,
    /// Not attempted: Isolinear does not do this (yet).
    NotDone,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub stage: Stage,
    pub state: State,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fact {
    /// Heading the fact is listed under ("Message", "Debtor", "Totals").
    pub group: String,
    pub label: String,
    pub value: String,
    /// Bytes of the file this fact comes from.
    pub source: Option<Range<u64>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    pub name: String,
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    /// Rows in the file when more than `rows` were kept.
    pub total: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub cells: Vec<String>,
    pub source: Option<Range<u64>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutlineNode {
    pub depth: u16,
    pub label: String,
    pub value: String,
    pub source: Option<Range<u64>>,
}

/// Pictures of the content, drawn by the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Visual {
    /// Geographic features in longitude/latitude (or projected x/y when `geographic` is false).
    Map {
        features: Vec<Feature>,
        geographic: bool,
    },
    /// Time series: channels of samples at a rate.
    Plot { channels: Vec<Channel> },
    /// Triangles in 3D (x, y, z), with the indices of each triangle.
    Mesh {
        vertices: Vec<[f32; 3]>,
        triangles: Vec<[u32; 3]>,
    },
    /// 2D line drawing (CAD, toolpaths): polylines in drawing units.
    Drawing {
        paths: Vec<Vec<[f32; 2]>>,
        unit: String,
    },
    /// An encoded picture found in the file (PNG, JPEG, GIF, WebP, BMP, TIFF): a map tile, a
    /// notebook output, a slide image. The app decodes it under its image limits.
    Picture { bytes: Vec<u8>, caption: String },
    /// A greyscale or colour picture; `values` are one per pixel (grey) or RGBA quadruples.
    Image {
        width: u32,
        height: u32,
        /// Grey values (one f32 per pixel) for window/level, or `None` with `rgba` set.
        grey: Option<Vec<f32>>,
        rgba: Option<Vec<u8>>,
        caption: String,
    },
    /// Intervals on a time axis, one row each, split into phases: a waterfall of recorded web
    /// requests. `start` and the phase lengths are in `unit` from the start of the first row.
    Timeline {
        rows: Vec<Span>,
        phases: Vec<String>,
        unit: String,
    },
}

/// One row of a [`Visual::Timeline`].
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub label: String,
    pub start: f64,
    /// Length of each phase, in the timeline's order of phases (0 when absent).
    pub parts: Vec<f64>,
    /// Bytes of the file the row comes from.
    pub source: Option<Range<u64>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Feature {
    pub name: String,
    pub geometry: Geometry,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Geometry {
    Points(Vec<[f64; 2]>),
    Lines(Vec<Vec<[f64; 2]>>),
    Polygons(Vec<Vec<[f64; 2]>>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub name: String,
    pub unit: String,
    pub rate: f64,
    pub samples: Vec<f32>,
}

impl Inspection {
    pub fn new(family: &str, kind: &str) -> Self {
        Self {
            family: family.into(),
            kind: kind.into(),
            checks: vec![Check {
                stage: Stage::Recognized,
                state: State::Passed,
                detail: String::new(),
            }],
            ..Self::default()
        }
    }
    pub fn check(&mut self, stage: Stage, state: State, detail: impl Into<String>) {
        self.checks.push(Check {
            stage,
            state,
            detail: detail.into(),
        });
    }
    /// The usual honest ending: read, not validated, signatures not verified.
    pub fn not_validated(&mut self) {
        if !self.checks.iter().any(|c| c.stage == Stage::Schema) {
            self.check(
                Stage::Schema,
                State::NotDone,
                "Not checked against the standard's schema",
            );
        }
    }
    pub fn fact(
        &mut self,
        group: &str,
        label: &str,
        value: impl Into<String>,
        source: Option<Range<u64>>,
    ) {
        let value = value.into();
        if value.is_empty() || self.facts.len() >= MAX_ITEMS {
            return;
        }
        self.facts.push(Fact {
            group: group.into(),
            label: label.into(),
            value,
            source,
        });
    }
}

/// What a handler may look at to recognize a file: cheap, no parsing of the whole file.
pub struct Probe<'a> {
    pub path: &'a Path,
    /// Lower-case extension, and the one before it for compressed names ("vcf.gz" -> "vcf", "gz").
    pub ext: String,
    pub inner_ext: Option<String>,
    /// The first [`PROBE`] bytes.
    pub head: &'a [u8],
    pub size: u64,
    /// The physical format Isolinear detected (a registry id: `xml`, `json`, `text`, `binary`, …).
    pub format: &'a str,
}
impl Probe<'_> {
    pub fn ext_is(&self, list: &[&str]) -> bool {
        list.contains(&self.ext.as_str())
    }
    /// The head as text, when it is UTF-8 (a cut character at the end is ignored).
    pub fn text(&self) -> Option<&str> {
        match std::str::from_utf8(self.head) {
            Ok(s) => Some(s),
            Err(e) if e.valid_up_to() + 4 > self.head.len() => {
                std::str::from_utf8(&self.head[..e.valid_up_to()]).ok()
            }
            Err(_) => None,
        }
    }
}

/// The file's content handed to a handler.
pub struct Input<'a> {
    pub path: &'a Path,
    /// The whole file, or its first [`MAX_INPUT`] bytes when `complete` is false.
    pub bytes: &'a [u8],
    pub complete: bool,
    pub size: u64,
}
impl Input<'_> {
    pub fn text(&self) -> Result<std::borrow::Cow<'_, str>, String> {
        match std::str::from_utf8(self.bytes) {
            Ok(s) => Ok(std::borrow::Cow::Borrowed(s)),
            Err(_) => Ok(String::from_utf8_lossy(self.bytes)),
        }
    }
}

/// A family of formats.
pub trait Handler: Sync {
    /// Short name for diagnostics and documentation.
    fn name(&self) -> &'static str;
    /// Whether this handler is for the file. Cheap: no full parse.
    fn recognize(&self, probe: &Probe) -> bool;
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String>;
}

/// Every handler, in the order they are asked (specific before general).
pub fn handlers() -> impl Iterator<Item = &'static dyn Handler> {
    standards::handlers()
}

/// The handler for this file, if any.
pub fn recognize(probe: &Probe) -> Option<&'static dyn Handler> {
    handlers().find(|h| h.recognize(probe))
}

/// Recognize and inspect `path` (worker). `None` when no handler is for it.
pub fn inspect_file(
    path: &Path,
    format: &str,
    cancelled: &dyn Fn() -> bool,
) -> Option<Result<Inspection, String>> {
    use std::io::Read;
    let size = std::fs::metadata(path).ok()?.len();
    let mut head = vec![0u8; PROBE.min(size as usize)];
    std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    let probe = probe(path, &head, size, format);
    let handler = recognize(&probe)?;
    let take = size.min(MAX_INPUT);
    let mut bytes = Vec::with_capacity(take as usize);
    let read = std::fs::File::open(path)
        .and_then(|f| f.take(take).read_to_end(&mut bytes))
        .map_err(|e| e.to_string());
    Some(read.and_then(|_| {
        let input = Input {
            path,
            bytes: &bytes,
            complete: take == size,
            size,
        };
        let mut inspection = guard(|| handler.inspect(&input, cancelled))?;
        if !input.complete {
            inspection.notes.push(format!(
                "Only the first {} MiB of the file were inspected",
                MAX_INPUT >> 20
            ));
        }
        Ok(inspection)
    }))
}

/// A handler that panics on a hostile file reports an error instead of taking the window down.
fn guard(f: impl FnOnce() -> Result<Inspection, String>) -> Result<Inspection, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err("The file could not be interpreted (internal error)".into()))
}

pub fn probe<'a>(path: &'a Path, head: &'a [u8], size: u64, format: &'a str) -> Probe<'a> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut parts = name.rsplit('.');
    let ext = parts
        .next()
        .filter(|_| name.contains('.'))
        .unwrap_or("")
        .to_owned();
    let inner_ext = parts
        .next()
        .filter(|_| name.matches('.').count() >= 2)
        .map(str::to_owned);
    Probe {
        path,
        ext,
        inner_ext,
        head,
        size,
        format,
    }
}

/// Byte range of `s` within `whole` (both slices of the same text), for sources.
pub fn span_of(whole: &str, s: &str) -> Option<Range<u64>> {
    let start = (s.as_ptr() as usize).checked_sub(whole.as_ptr() as usize)?;
    (start + s.len() <= whole.len()).then(|| start as u64..(start + s.len()) as u64)
}
