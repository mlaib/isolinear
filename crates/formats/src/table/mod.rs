//! Read-only tables from binary files: columnar data, databases, spreadsheets, arrays.
//!
//! A reader opens a file, describes it ([`Schema`]: the tables it holds, the columns of the one
//! open, facts about the file) and hands out rows a page at a time as display text. Readers
//! read only what a page needs, check every length against the file before allocating, and never
//! execute anything (no SQL, no formulas, no embedded code).
//!
//! Adding a format: implement [`TableSource`] in a module of this directory and return it from
//! [`open`]. See `npy.rs` for the reference implementation.

use std::path::Path;

pub mod archive;
pub mod arrow;
pub mod avro;
pub mod hdf5;
pub mod npy;
pub mod npz;
pub mod orc;
pub mod parquet;
pub mod sqlite;
pub mod xlsx;

/// Archive format ids, listed by [`archive`].
pub const ARCHIVES: &[&str] = &["zip", "tar", "gzip", "zstd", "xz", "bzip2"];

/// Characters of a cell sent for display; longer values are cut with a size note.
pub const CELL_MAX: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// The type as the format names it (`int64`, `TEXT`, `<f8`, …).
    pub type_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    pub name: String,
    /// Row count when the format records it.
    pub rows: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schema {
    /// Tables, sheets or datasets of the file; one entry for single-table formats.
    pub tables: Vec<TableInfo>,
    /// Index of the open table in `tables`.
    pub table: usize,
    pub columns: Vec<Column>,
    /// Rows of the open table, when known without reading them all.
    pub rows: Option<u64>,
    /// Facts about the file, in display order (`Created by`, `Row groups`, …).
    pub details: Vec<(String, String)>,
    /// The full schema as the format writes it, when it has one worth showing.
    pub text: Option<String>,
}

pub trait TableSource: Send {
    fn schema(&self) -> &Schema;

    /// Rows `start..start + count` of the open table (fewer at the end), cells as display text,
    /// `None` for null.
    fn rows(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String>;
}

/// Whether a format id has a table reader.
pub fn supports(format: &str) -> bool {
    match format {
        "npy" => true,
        "npz" => npz::READY,
        "parquet" => parquet::READY,
        "arrow" => arrow::READY,
        "avro" => avro::READY,
        "orc" => orc::READY,
        "sqlite" => sqlite::READY,
        "xlsx" => xlsx::READY,
        "hdf5" => hdf5::READY,
        f if ARCHIVES.contains(&f) => archive::READY,
        _ => false,
    }
}

/// Opens `path` as a table of `format`; `table` names the table to open in a multi-table file
/// (the first when `None`). `None` when the format has no table reader.
pub fn open(
    format: &str,
    path: &Path,
    table: Option<&str>,
) -> Option<Result<Box<dyn TableSource>, String>> {
    let source: Result<Box<dyn TableSource>, String> = match format {
        "npy" => npy::Npy::open(path, table).map(|s| Box::new(s) as Box<dyn TableSource>),
        "npz" => npz::open(path, table),
        "parquet" => parquet::open(path, table),
        "arrow" => arrow::open(path, table),
        "avro" => avro::open(path, table),
        "orc" => orc::open(path, table),
        "sqlite" => sqlite::open(path, table),
        "xlsx" => xlsx::open(path, table),
        "hdf5" => hdf5::open(path, table),
        f if ARCHIVES.contains(&f) => archive::open(f, path, table),
        _ => return None,
    };
    Some(source)
}

/// A cell cut at [`CELL_MAX`] characters, with the full length noted.
pub fn cut(value: String) -> String {
    match value.char_indices().nth(CELL_MAX) {
        Some((at, _)) => format!("{}… ({} characters)", &value[..at], value.chars().count()),
        None => value,
    }
}
