//! Private content model; it cannot carry input HTML.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Style {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    pub underline: bool,
    /// -1 subscript, 0 baseline, 1 superscript.
    pub position: i8,
    pub note: bool,
    pub link: Option<std::sync::Arc<str>>,
    pub image: bool,
    /// An archive part name, never an external path or URL.
    pub image_part: Option<std::sync::Arc<str>>,
}
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Span {
    pub text: String,
    pub style: Style,
}
#[derive(Clone, Debug)]
pub(super) struct Cell {
    pub spans: Vec<Span>,
    pub colspan: usize,
    pub rowspan: usize,
    pub continuation: bool,
    pub merge_start: bool,
    pub covered: bool,
}
#[derive(Clone, Debug)]
pub(super) enum Block {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph {
        spans: Vec<Span>,
    },
    Code {
        spans: Vec<Span>,
    },
    Quote {
        blocks: Vec<Block>,
    },
    List {
        start: Option<u64>,
        items: Vec<Item>,
    },
    Table {
        header: Vec<Cell>,
        rows: Vec<Vec<Cell>>,
    },
}
#[derive(Clone, Debug)]
pub(super) struct Item {
    pub blocks: Vec<Block>,
}
pub(super) struct Document {
    pub blocks: Vec<Block>,
    pub truncated: bool,
}
