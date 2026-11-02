//! Tables: delimited files (CSV, TSV) and bibliographies (BibTeX). Indexed on a worker from the
//! editor's text (one revision at a time, like the structure tree) or from the file for
//! read-only files up to 256 MiB. Rows are sent a page at a time, in the order and with the
//! filter the user chose. Offsets crossing to the UI are UTF-16 code units.

use std::cmp::Ordering as Order;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use isolinear_core::encoding;
use isolinear_formats::bibtex;
use isolinear_formats::csv::{self, Table};
use isolinear_formats::table::{self as binary, TableSource};
use isolinear_formats::tree::Offsets;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

const ROWS_MAX: usize = 2000;

#[derive(Default)]
pub struct Tables {
    current: Mutex<Option<Arc<Indexed>>>,
    running: Mutex<Option<Arc<AtomicBool>>>,
    next_rev: AtomicU64,
}

/// What the rows are made of.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Delimited text; the byte is the delimiter a format implies (tab for TSV).
    Delimited(Option<u8>),
    Bibliography,
    /// A binary file read by the table reader of this format id.
    Binary(&'static str),
}

enum Content {
    Delimited(Table),
    /// Rows already built, with the source range of every cell.
    Rows {
        rows: Vec<Vec<String>>,
        spans: Vec<Vec<Range<usize>>>,
    },
    /// Rows read from a binary file on demand.
    Source(Mutex<Box<dyn TableSource>>),
}

struct Indexed {
    rev: u64,
    text: Arc<String>,
    content: Content,
    offsets: Offsets,
    /// Rows in the chosen order, filtered; `None` shows them all in file order.
    order: Mutex<Option<Arc<Vec<u32>>>>,
    /// For a binary file: the rows kept by the last sort or filter, with their cells, since
    /// reading single rows back out of order would decode a block per row.
    picked: Mutex<Option<Arc<Vec<Row>>>>,
}

impl Indexed {
    fn rows(&self) -> usize {
        match &self.content {
            Content::Delimited(t) => t.rows(),
            Content::Rows { rows, .. } => rows.len(),
            Content::Source(s) => s.lock().unwrap().schema().rows.unwrap_or(0) as usize,
        }
    }

    /// Cells of rows `start..start + count` in file order; `None` is null.
    fn page(&self, start: usize, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        match &self.content {
            Content::Source(s) => s.lock().unwrap().rows(start as u64, count),
            _ => Ok((start..(start + count).min(self.rows()))
                .map(|r| self.cells(r).into_iter().map(Some).collect())
                .collect()),
        }
    }

    fn cells(&self, row: usize) -> Vec<String> {
        match &self.content {
            Content::Delimited(t) => {
                let bytes = self.text.as_bytes();
                t.fields(bytes, Some(row))
                    .unwrap_or_default()
                    .iter()
                    .map(|f| csv::cell(bytes, f, &t.dialect))
                    .collect()
            }
            Content::Rows { rows, .. } => rows.get(row).cloned().unwrap_or_default(),
            Content::Source(_) => Vec::new(),
        }
    }

    fn cell_span(&self, row: Option<usize>, column: usize) -> Option<Range<usize>> {
        match &self.content {
            Content::Delimited(t) => t.fields(self.text.as_bytes(), row)?.get(column).cloned(),
            Content::Rows { spans, .. } => spans.get(row?)?.get(column).cloned(),
            Content::Source(_) => None,
        }
    }

    fn cell_at(&self, byte: usize) -> Option<(Option<usize>, usize)> {
        match &self.content {
            Content::Delimited(t) => t.cell_at(self.text.as_bytes(), byte),
            Content::Rows { spans, .. } => {
                let row = spans.iter().position(|cells| {
                    let first = cells.iter().map(|c| c.start).min().unwrap_or(0);
                    let last = cells.iter().map(|c| c.end).max().unwrap_or(0);
                    first <= byte && byte <= last
                })?;
                let column = spans[row]
                    .iter()
                    .position(|c| c.start <= byte && byte <= c.end)
                    .unwrap_or(0);
                Some((Some(row), column))
            }
            Content::Source(_) => None,
        }
    }
}

/// Where the text of a table comes from.
pub enum Source {
    Text(String),
    File(PathBuf),
    /// The text of the current table, indexed again with another dialect.
    Same,
}

/// Rows of a binary table read for a sort or a filter, at most: the first ones of the file.
const PICK_ROWS: usize = 100_000;
/// Text of the cells kept for a sort or a filter, at most.
const PICK_BYTES: usize = 128 << 20;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Arranged {
    shown: usize,
    /// Only this many rows from the start of the file were sorted and filtered.
    within: Option<usize>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Ready {
    rev: u64,
    rows: usize,
    columns: usize,
    header: Option<Vec<String>>,
    /// Delimiter and header choice; `None` for tables that are not delimited text.
    dialect: Option<DialectInfo>,
    ragged: bool,
    /// UTF-16 offset of a quote that is never closed.
    unterminated: Option<u32>,
    error: Option<String>,
    /// Tables of a multi-table file (sheets, datasets) and the one open.
    tables: Vec<TableEntry>,
    table: usize,
    /// Column types, when the format has them.
    types: Option<Vec<String>>,
    /// Facts about the file and its schema as text, for the schema panel.
    details: Vec<(String, String)>,
    schema_text: Option<String>,
    /// Rows can be sorted and filtered (a binary file over its first rows).
    arrangeable: bool,
}

#[derive(Serialize, Clone)]
struct TableEntry {
    name: String,
    rows: Option<u64>,
}

impl Ready {
    fn new(rev: u64) -> Ready {
        Ready {
            rev,
            rows: 0,
            columns: 0,
            header: None,
            dialect: None,
            ragged: false,
            unterminated: None,
            error: None,
            tables: Vec::new(),
            table: 0,
            types: None,
            details: Vec::new(),
            schema_text: None,
            arrangeable: true,
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DialectInfo {
    delimiter: char,
    has_header: bool,
}

#[derive(Serialize, Clone)]
pub struct Row {
    /// Position in the file's order.
    n: usize,
    /// `None` is null.
    cells: Vec<Option<String>>,
}

#[derive(Serialize)]
pub struct Rows {
    rows: Vec<Row>,
}

#[derive(Serialize)]
pub struct CellAt {
    /// Data row; `None` on the header.
    row: Option<usize>,
    column: usize,
    /// Where the row is in the current order; `None` when filtered out.
    position: Option<usize>,
}

const BIB_COLUMNS: [&str; 5] = ["Key", "Type", "Author", "Year", "Title"];

fn bibliography(text: &str) -> Content {
    let bib = bibtex::parse(text);
    let mut rows = Vec::with_capacity(bib.entries.len());
    let mut spans = Vec::with_capacity(bib.entries.len());
    for e in &bib.entries {
        let field = |names: &[&str]| names.iter().find_map(|n| e.field(text, n));
        let key = e.key.clone().unwrap_or(e.kind_span.clone());
        let mut cells = vec![text[key.clone()].to_owned(), e.kind.clone()];
        let mut ranges = vec![key, e.kind_span.clone()];
        for names in [&["author", "editor"][..], &["year", "date"], &["title"]] {
            match field(names) {
                Some(f) => {
                    cells.push(bibtex::display(text, &f.value));
                    ranges.push(f.value.clone());
                }
                None => {
                    cells.push(String::new());
                    ranges.push(e.span.start..e.span.start);
                }
            }
        }
        rows.push(cells);
        spans.push(ranges);
    }
    Content::Rows { rows, spans }
}

impl Tables {
    /// Indexes on a worker; `table` is emitted when ready. `delimiter` and `header` override
    /// what sniffing suggests for delimited text; `table` names the table to open in a
    /// multi-table binary file.
    pub fn start(
        &self,
        app: AppHandle,
        source: Source,
        kind: Kind,
        delimiter: Option<u8>,
        header: Option<bool>,
        table: Option<String>,
    ) -> u64 {
        let rev = self.next_rev.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(old) = self.running.lock().unwrap().replace(cancel.clone()) {
            old.store(true, Ordering::Relaxed);
        }
        let previous = self
            .current
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.text.clone());
        let spawned = std::thread::Builder::new()
            .name("table".into())
            .spawn(move || {
                let built = build(
                    rev, source, kind, delimiter, header, table, previous, &cancel,
                );
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                match built {
                    Ok((indexed, ready)) => {
                        *app.state::<Tables>().current.lock().unwrap() = Some(Arc::new(indexed));
                        let _ = app.emit("table", ready);
                    }
                    Err(error) => {
                        let mut ready = Ready::new(rev);
                        ready.error = Some(error);
                        let _ = app.emit("table", ready);
                    }
                }
            });
        if spawned.is_err() {
            return 0;
        }
        rev
    }

    pub fn clear(&self) {
        if let Some(old) = self.running.lock().unwrap().take() {
            old.store(true, Ordering::Relaxed);
        }
        self.current.lock().unwrap().take();
    }

    fn get(&self, rev: u64) -> Result<Arc<Indexed>, String> {
        match &*self.current.lock().unwrap() {
            Some(t) if t.rev == rev => Ok(t.clone()),
            _ => Err("stale".into()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build(
    rev: u64,
    source: Source,
    kind: Kind,
    delimiter: Option<u8>,
    header: Option<bool>,
    table: Option<String>,
    previous: Option<Arc<String>>,
    cancel: &AtomicBool,
) -> Result<(Indexed, Ready), String> {
    let mut ready = Ready::new(rev);
    if let Kind::Binary(format) = kind {
        let Source::File(path) = source else {
            return Err("a binary table is read from its file".into());
        };
        let reader = binary::open(format, &path, table.as_deref())
            .ok_or("this format has no table reader")??;
        let schema = reader.schema();
        ready.rows = schema.rows.unwrap_or(0) as usize;
        ready.columns = schema.columns.len();
        ready.header = Some(schema.columns.iter().map(|c| c.name.clone()).collect());
        ready.types = Some(schema.columns.iter().map(|c| c.type_name.clone()).collect());
        ready.tables = schema
            .tables
            .iter()
            .map(|t| TableEntry {
                name: t.name.clone(),
                rows: t.rows,
            })
            .collect();
        ready.table = schema.table;
        ready.details = schema.details.clone();
        ready.schema_text = schema.text.clone();
        let indexed = Indexed {
            rev,
            text: Arc::new(String::new()),
            content: Content::Source(Mutex::new(reader)),
            offsets: Offsets::new(""),
            order: Mutex::new(None),
            picked: Mutex::new(None),
        };
        return Ok((indexed, ready));
    }
    let text = match source {
        Source::Text(t) => Arc::new(t),
        Source::Same => previous.ok_or("no table is open")?,
        Source::File(path) => {
            Arc::new(encoding::decode(&std::fs::read(&path).map_err(|e| e.to_string())?).text)
        }
    };
    let bytes = text.as_bytes();
    let offsets = Offsets::new(&text);
    let content = match kind {
        Kind::Delimited(hint) => {
            let mut d = csv::sniff(&bytes[..bytes.len().min(64 * 1024)], hint);
            if let Some(c) = delimiter {
                d.delimiter = c;
            }
            if let Some(h) = header {
                d.has_header = h;
            }
            let table = Table::index(bytes, d, cancel);
            ready.rows = table.rows();
            ready.columns = table.max_fields;
            ready.header = table
                .fields(bytes, None)
                .map(|f| f.iter().map(|s| csv::cell(bytes, s, &d)).collect());
            ready.dialect = Some(DialectInfo {
                delimiter: d.delimiter as char,
                has_header: d.has_header,
            });
            ready.ragged = table.ragged;
            ready.unterminated = table.unterminated.map(|o| offsets.to_utf16(&text, o));
            Content::Delimited(table)
        }
        Kind::Bibliography => {
            let content = bibliography(&text);
            if let Content::Rows { rows, .. } = &content {
                ready.rows = rows.len();
            }
            ready.columns = BIB_COLUMNS.len();
            ready.header = Some(BIB_COLUMNS.iter().map(|c| c.to_string()).collect());
            content
        }
        Kind::Binary(_) => unreachable!("handled above"),
    };
    let indexed = Indexed {
        rev,
        text,
        content,
        offsets,
        order: Mutex::new(None),
        picked: Mutex::new(None),
    };
    Ok((indexed, ready))
}

/// The table kind of a format id, if it has a table.
pub fn kind(format: &str) -> Option<Kind> {
    match format {
        "csv" => Some(Kind::Delimited(None)),
        "tsv" => Some(Kind::Delimited(Some(b'\t'))),
        "bibtex" => Some(Kind::Bibliography),
        f if binary::supports(f) => {
            isolinear_formats::registry::by_id(f).map(|spec| Kind::Binary(spec.id))
        }
        _ => None,
    }
}

/// Indexes again: the editor's text after an edit, or the same text with another dialect.
#[tauri::command]
pub fn table_update(
    app: AppHandle,
    state: tauri::State<'_, Tables>,
    format: String,
    text: Option<String>,
    delimiter: Option<char>,
    header: Option<bool>,
) -> Result<u64, String> {
    let kind = kind(&format).ok_or("this format has no table")?;
    let source = text.map_or(Source::Same, Source::Text);
    let delimiter = delimiter.filter(char::is_ascii).map(|c| c as u8);
    Ok(state.start(app, source, kind, delimiter, header, None))
}

/// Opens another table of a multi-table binary file.
#[tauri::command]
pub fn table_switch(
    app: AppHandle,
    state: tauri::State<'_, Tables>,
    format: String,
    path: String,
    table: String,
) -> Result<u64, String> {
    let kind = kind(&format).ok_or("this format has no table")?;
    let source = Source::File(PathBuf::from(path));
    Ok(state.start(app, source, kind, None, None, Some(table)))
}

/// Orders rows by a column (numbers by value, then text without regard to case) and keeps the
/// rows where some cell contains `filter` (ignoring case). A binary file is sorted and filtered
/// over its first [`PICK_ROWS`] rows.
#[tauri::command]
pub async fn table_arrange(
    state: tauri::State<'_, Tables>,
    rev: u64,
    column: Option<usize>,
    descending: bool,
    filter: String,
) -> Result<Arranged, String> {
    let t = state.get(rev)?;
    tauri::async_runtime::spawn_blocking(move || arrange(&t, column, descending, &filter))
        .await
        .map_err(|e| e.to_string())?
}

fn arrange(
    t: &Indexed,
    column: Option<usize>,
    descending: bool,
    filter: &str,
) -> Result<Arranged, String> {
    {
        let needle = filter.trim().to_lowercase();
        if column.is_none() && needle.is_empty() {
            *t.order.lock().unwrap() = None;
            *t.picked.lock().unwrap() = None;
            return Ok(Arranged {
                shown: t.rows(),
                within: None,
            });
        }
        let keep = |cells: &[Option<String>]| {
            needle.is_empty()
                || cells
                    .iter()
                    .flatten()
                    .any(|c| c.to_lowercase().contains(&needle))
        };
        let sort = |keyed: &mut Vec<(Option<String>, usize)>| {
            if column.is_some() {
                keyed.sort_by(|a, b| {
                    let (x, y) = (a.0.as_deref().unwrap_or(""), b.0.as_deref().unwrap_or(""));
                    // Empty cells stay last in both directions.
                    match (x.trim().is_empty(), y.trim().is_empty()) {
                        (false, false) if descending => compare(x, y).reverse(),
                        _ => compare(x, y),
                    }
                });
            }
        };
        if matches!(t.content, Content::Source(_)) {
            let total = t.rows();
            let limit = total.min(PICK_ROWS);
            let (mut rows, mut bytes, mut at) = (Vec::new(), 0usize, 0usize);
            while at < limit && bytes < PICK_BYTES {
                let page = t.page(at, ROWS_MAX.min(limit - at))?;
                if page.is_empty() {
                    break;
                }
                let read = page.len();
                for (i, cells) in page.into_iter().enumerate() {
                    if keep(&cells) {
                        bytes += cells.iter().flatten().map(String::len).sum::<usize>();
                        rows.push(Row { n: at + i, cells });
                    }
                }
                at += read;
            }
            let within = (at < total).then_some(at);
            let mut keyed: Vec<(Option<String>, usize)> = rows
                .iter()
                .enumerate()
                .map(|(i, r)| (column.and_then(|c| r.cells.get(c).cloned().flatten()), i))
                .collect();
            sort(&mut keyed);
            let mut slots: Vec<Option<Row>> = rows.into_iter().map(Some).collect();
            let picked: Vec<Row> = keyed
                .into_iter()
                .filter_map(|(_, i)| slots[i].take())
                .collect();
            let shown = picked.len();
            *t.picked.lock().unwrap() = Some(Arc::new(picked));
            return Ok(Arranged { shown, within });
        }
        let mut keyed: Vec<(Option<String>, usize)> = Vec::new();
        for r in 0..t.rows() {
            let cells = t.cells(r);
            if !needle.is_empty() && !cells.iter().any(|c| c.to_lowercase().contains(&needle)) {
                continue;
            }
            keyed.push((column.map(|c| cells.get(c).cloned().unwrap_or_default()), r));
        }
        sort(&mut keyed);
        let order: Vec<u32> = keyed.into_iter().map(|(_, r)| r as u32).collect();
        let shown = order.len();
        *t.order.lock().unwrap() = Some(Arc::new(order));
        Ok(Arranged {
            shown,
            within: None,
        })
    }
}

/// Numbers before text, numbers by value, text ignoring case; empty cells last.
fn compare(a: &str, b: &str) -> Order {
    let (a, b) = (a.trim(), b.trim());
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Order::Equal,
        (true, false) => return Order::Greater,
        (false, true) => return Order::Less,
        _ => {}
    }
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(Order::Equal),
        (Ok(_), Err(_)) => Order::Less,
        (Err(_), Ok(_)) => Order::Greater,
        _ => a.to_lowercase().cmp(&b.to_lowercase()),
    }
}

#[tauri::command]
pub async fn table_rows(
    state: tauri::State<'_, Tables>,
    rev: u64,
    start: usize,
    count: usize,
) -> Result<Rows, String> {
    let t = state.get(rev)?;
    tauri::async_runtime::spawn_blocking(move || {
        let count = count.min(ROWS_MAX);
        if let Some(picked) = t.picked.lock().unwrap().clone() {
            let end = start.saturating_add(count).min(picked.len());
            return Ok(Rows {
                rows: picked[start.min(end)..end].to_vec(),
            });
        }
        let order = t.order.lock().unwrap().clone();
        let rows = match order {
            None => t
                .page(start, count)?
                .into_iter()
                .enumerate()
                .map(|(i, cells)| Row {
                    n: start + i,
                    cells,
                })
                .collect(),
            Some(o) => {
                let end = start.saturating_add(count).min(o.len());
                (start.min(end)..end)
                    .map(|p| {
                        let n = o[p] as usize;
                        Row {
                            n,
                            cells: t.cells(n).into_iter().map(Some).collect(),
                        }
                    })
                    .collect()
            }
        };
        Ok(Rows { rows })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// UTF-16 range of a cell in the text (a CSV cell with its quotes, a BibTeX value as written).
#[tauri::command]
pub fn table_cell_span(
    state: tauri::State<'_, Tables>,
    rev: u64,
    row: Option<usize>,
    column: usize,
) -> Result<(u32, u32), String> {
    let t = state.get(rev)?;
    let f = t.cell_span(row, column).ok_or("no such cell")?;
    Ok((
        t.offsets.to_utf16(&t.text, f.start as u32),
        t.offsets.to_utf16(&t.text, f.end as u32),
    ))
}

/// The cell at a UTF-16 offset of the text.
#[tauri::command]
pub fn table_cell_at(
    state: tauri::State<'_, Tables>,
    rev: u64,
    offset: u32,
) -> Result<Option<CellAt>, String> {
    let t = state.get(rev)?;
    let byte = t.offsets.to_byte(&t.text, offset) as usize;
    let order = t.order.lock().unwrap().clone();
    Ok(t.cell_at(byte).map(|(row, column)| CellAt {
        row,
        column,
        position: match (&order, row) {
            (None, r) => r,
            (Some(o), Some(r)) => o.iter().position(|&x| x as usize == r),
            (Some(_), None) => None,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary table of `n` rows: the row number, and its value modulo 7.
    struct Counting {
        schema: isolinear_formats::table::Schema,
    }

    impl TableSource for Counting {
        fn schema(&self) -> &isolinear_formats::table::Schema {
            &self.schema
        }
        fn rows(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
            let end = (start + count as u64).min(self.schema.rows.unwrap());
            Ok((start..end)
                .map(|i| {
                    vec![
                        Some(i.to_string()),
                        (i % 7 != 0).then(|| (i % 7).to_string()),
                    ]
                })
                .collect())
        }
    }

    fn counting(n: u64) -> Indexed {
        let schema = isolinear_formats::table::Schema {
            rows: Some(n),
            ..Default::default()
        };
        Indexed {
            rev: 1,
            text: Arc::new(String::new()),
            content: Content::Source(Mutex::new(Box::new(Counting { schema }))),
            offsets: Offsets::new(""),
            order: Mutex::new(None),
            picked: Mutex::new(None),
        }
    }

    #[test]
    fn binary_tables_sort_and_filter_over_their_first_rows() {
        let t = counting(250_000);
        let a = arrange(&t, Some(1), true, "").unwrap();
        assert_eq!((a.shown, a.within), (PICK_ROWS, Some(PICK_ROWS)));
        let picked = t.picked.lock().unwrap().clone().unwrap();
        // Descending by the second column, nulls (multiples of 7) last.
        assert_eq!(picked[0].cells[1].as_deref(), Some("6"));
        assert_eq!(picked.last().unwrap().cells[1], None);
        let a = arrange(&t, None, false, "99999").unwrap();
        assert_eq!(a.shown, 1);
        assert_eq!(t.picked.lock().unwrap().as_ref().unwrap()[0].n, 99_999);
        // Clearing both shows the file again, in its order.
        let a = arrange(&t, None, false, " ").unwrap();
        assert_eq!((a.shown, a.within), (250_000, None));
        assert!(t.picked.lock().unwrap().is_none());
        // A small file is covered whole.
        let small = counting(10);
        assert_eq!(arrange(&small, Some(0), true, "").unwrap().within, None);
    }

    #[test]
    fn comparison_puts_numbers_first_and_blanks_last() {
        let mut v = vec!["b", "", "10", "A", "9", "2.5"];
        v.sort_by(|a, b| compare(a, b));
        assert_eq!(v, ["2.5", "9", "10", "A", "b", ""]);
    }

    #[test]
    fn bibliography_rows_and_spans() {
        let text = "@article{k1, author={Ann}, year=2001, title={T}}\n@misc{k2}\n";
        let Content::Rows { rows, spans } = bibliography(text) else {
            panic!()
        };
        assert_eq!(rows[0], ["k1", "article", "Ann", "2001", "T"]);
        assert_eq!(rows[1], ["k2", "misc", "", "", ""]);
        assert_eq!(&text[spans[0][2].clone()], "{Ann}");
    }
}
