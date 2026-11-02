//! Shared bounds and display for the two columnar readers.

use std::fmt::Write;
use std::fs::{File, Metadata};
use std::path::Path;

use arrow_array::{Array, RecordBatch};
use arrow_cast::display::{
    ArrayFormatter, ArrayFormatterFactory, DisplayIndex, FormatOptions, FormatResult,
};
use arrow_schema::{DataType, Schema as ArrowSchema};

use super::super::{Column, Schema, TableInfo, cut};

pub(in crate::table) mod compact;

#[derive(Clone, Copy)]
pub(in crate::table) struct Limits {
    pub metadata: usize,
    pub columns: usize,
    pub batches: usize,
    pub decoded: usize,
    pub rows: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            metadata: 64 << 20,
            columns: 10_000,
            batches: 1_000_000,
            decoded: 256 << 20,
            rows: 2000,
        }
    }
}

pub(in crate::table) fn guard<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    // The upstream decoders can panic on malformed arrays; do not unwind into the worker.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err("The columnar data is damaged: the decoder rejected it".into()))
}

pub(in crate::table) fn unchanged(file: &File, before: &Metadata) -> Result<(), String> {
    let now = file.metadata().map_err(|e| e.to_string())?;
    if now.len() != before.len() || now.modified().ok() != before.modified().ok() {
        return Err("The file changed; reopen it before reading more rows".into());
    }
    Ok(())
}

pub(in crate::table) fn page_end(
    start: u64,
    count: usize,
    total: u64,
    limits: Limits,
) -> Result<u64, String> {
    if count > limits.rows {
        return Err(format!(
            "Requested {count} rows; a page can contain at most {}",
            limits.rows
        ));
    }
    Ok(start.saturating_add(count as u64).min(total))
}

pub(in crate::table) fn charge(used: &mut usize, amount: usize, max: usize) -> Result<(), String> {
    *used = used
        .checked_add(amount)
        .ok_or("The decoded size overflows")?;
    if *used > max {
        return Err(format!("Decoded data exceeds the {max}-byte table budget"));
    }
    Ok(())
}

pub(in crate::table) fn schema(
    path: &Path,
    arrow: &ArrowSchema,
    rows: u64,
    limits: Limits,
) -> Result<Schema, String> {
    if arrow.fields().len() > limits.columns {
        return Err(format!("More than {} columns", limits.columns));
    }
    // Bound the schema before display or downstream code traverses its nested fields.
    let mut stack: Vec<_> = arrow.fields().iter().map(|f| (f.data_type(), 1)).collect();
    let mut fields = 0usize;
    while let Some((ty, depth)) = stack.pop() {
        fields += 1;
        if depth > 64 || fields > limits.columns {
            return Err("The schema exceeds 64 levels or the column limit".into());
        }
        if let DataType::FixedSizeBinary(width) = ty
            && (*width < 0 || *width as usize > limits.decoded)
        {
            return Err("Fixed-size binary width exceeds the decoded byte limit".into());
        }
        if let DataType::FixedSizeList(_, width) = ty
            && (*width < 0 || *width as usize > limits.decoded / 8)
        {
            return Err("Fixed-size list length exceeds the value limit".into());
        }
        match ty {
            DataType::List(f)
            | DataType::LargeList(f)
            | DataType::FixedSizeList(f, _)
            | DataType::ListView(f)
            | DataType::LargeListView(f)
            | DataType::Map(f, _) => stack.push((f.data_type(), depth + 1)),
            DataType::Struct(fs) => stack.extend(fs.iter().map(|f| (f.data_type(), depth + 1))),
            DataType::Union(fs, _) => {
                stack.extend(fs.iter().map(|(_, f)| (f.data_type(), depth + 1)))
            }
            DataType::Dictionary(k, v) => {
                stack.push((k, depth + 1));
                stack.push((v, depth + 1));
            }
            DataType::RunEndEncoded(a, b) => {
                stack.push((a.data_type(), depth + 1));
                stack.push((b.data_type(), depth + 1));
            }
            _ => {}
        }
    }
    Ok(Schema {
        tables: vec![TableInfo {
            name: path
                .file_name()
                .map_or("data".into(), |s| s.to_string_lossy().into_owned()),
            rows: Some(rows),
        }],
        rows: Some(rows),
        columns: arrow
            .fields()
            .iter()
            .map(|f| Column {
                name: f.name().clone(),
                type_name: type_name(f.data_type()),
            })
            .collect(),
        ..Schema::default()
    })
}

fn type_name(ty: &DataType) -> String {
    enum Part<'a> {
        Type(&'a DataType),
        Text(String),
    }
    let mut stack = vec![Part::Type(ty)];
    let mut out = String::new();
    while let Some(p) = stack.pop() {
        let Part::Type(ty) = p else {
            if let Part::Text(s) = p {
                out.push_str(&s);
            }
            continue;
        };
        match ty {
            DataType::Timestamp(unit, tz) => {
                let unit = match unit {
                    arrow_schema::TimeUnit::Second => "s",
                    arrow_schema::TimeUnit::Millisecond => "ms",
                    arrow_schema::TimeUnit::Microsecond => "us",
                    arrow_schema::TimeUnit::Nanosecond => "ns",
                };
                out.push_str(&format!(
                    "timestamp[{unit}{}]",
                    tz.as_ref().map_or(String::new(), |tz| format!(", {tz}"))
                ));
            }
            DataType::List(f) | DataType::LargeList(f) | DataType::FixedSizeList(f, _) => {
                out.push_str(match ty {
                    DataType::LargeList(_) => "large_list<",
                    DataType::FixedSizeList(_, _) => "fixed_size_list<",
                    _ => "list<",
                });
                stack.push(Part::Text(match ty {
                    DataType::FixedSizeList(_, size) => format!(", {size}>"),
                    _ => ">".into(),
                }));
                stack.push(Part::Type(f.data_type()));
            }
            DataType::Struct(fs) => {
                out.push_str("struct<");
                stack.push(Part::Text(">".into()));
                for (i, f) in fs.iter().enumerate().rev() {
                    stack.push(Part::Type(f.data_type()));
                    stack.push(Part::Text(format!(
                        "{}{}: ",
                        if i == 0 { "" } else { ", " },
                        f.name()
                    )));
                }
            }
            DataType::Dictionary(k, v) => {
                out.push_str("dictionary<");
                stack.push(Part::Text(">".into()));
                stack.push(Part::Type(v));
                stack.push(Part::Text(", ".into()));
                stack.push(Part::Type(k));
            }
            DataType::Map(..)
            | DataType::Union(..)
            | DataType::RunEndEncoded(..)
            | DataType::ListView(..)
            | DataType::LargeListView(..) => out.push_str(&ty.to_string()),
            _ => out.push_str(&ty.to_string().to_ascii_lowercase()),
        }
    }
    out
}

/// Resolve dictionary keys before testing nulls or applying the binary preview limit.
fn dictionary_value(mut a: &dyn Array, mut row: usize) -> Result<(&dyn Array, usize), String> {
    use arrow_array::{DictionaryArray, types::*};
    for _ in 0..64 {
        let DataType::Dictionary(key, _) = a.data_type() else {
            return Ok((a, row));
        };
        if a.is_null(row) {
            return Ok((a, row));
        }
        macro_rules! resolve {
            ($ty:ty) => {{
                let d = a
                    .as_any()
                    .downcast_ref::<DictionaryArray<$ty>>()
                    .ok_or("Invalid dictionary array")?;
                row = d.key(row).ok_or("Invalid dictionary key")?;
                a = d.values().as_ref();
                if row >= a.len() {
                    return Err("Dictionary key is outside its values".into());
                }
            }};
        }
        match key.as_ref() {
            DataType::Int8 => resolve!(Int8Type),
            DataType::Int16 => resolve!(Int16Type),
            DataType::Int32 => resolve!(Int32Type),
            DataType::Int64 => resolve!(Int64Type),
            DataType::UInt8 => resolve!(UInt8Type),
            DataType::UInt16 => resolve!(UInt16Type),
            DataType::UInt32 => resolve!(UInt32Type),
            DataType::UInt64 => resolve!(UInt64Type),
            _ => return Err("Unsupported dictionary key type".into()),
        }
    }
    Err("Dictionary nesting exceeds 64 levels".into())
}

fn binary(a: &dyn Array, row: usize) -> Option<&[u8]> {
    use arrow_array::{BinaryArray, BinaryViewArray, FixedSizeBinaryArray, LargeBinaryArray};
    if let Some(a) = a.as_any().downcast_ref::<BinaryArray>() {
        Some(a.value(row))
    } else if let Some(a) = a.as_any().downcast_ref::<LargeBinaryArray>() {
        Some(a.value(row))
    } else if let Some(a) = a.as_any().downcast_ref::<FixedSizeBinaryArray>() {
        Some(a.value(row))
    } else {
        a.as_any()
            .downcast_ref::<BinaryViewArray>()
            .map(|a| a.value(row))
    }
}

/// UTC is a zero offset, but Arrow's offset-only timezone parser does not accept its name.
/// Normalize only the formatter's array; the schema and source retain their original timezone.
#[derive(Debug)]
struct PreviewFactory;

impl ArrayFormatterFactory for PreviewFactory {
    fn create_array_formatter<'a>(
        &self,
        array: &'a dyn Array,
        options: &FormatOptions<'a>,
        _field: Option<&'a arrow_schema::Field>,
    ) -> Result<Option<ArrayFormatter<'a>>, arrow_schema::ArrowError> {
        if let DataType::Timestamp(unit, Some(tz)) = array.data_type()
            && matches!(tz.as_ref(), "UTC" | "Etc/UTC" | "GMT" | "Etc/GMT" | "Z")
        {
            macro_rules! utc {
                ($ty:ty) => {
                    std::sync::Arc::new(
                        array
                            .as_any()
                            .downcast_ref::<$ty>()
                            .ok_or_else(|| {
                                arrow_schema::ArrowError::InvalidArgumentError(
                                    "Invalid timestamp array".into(),
                                )
                            })?
                            .clone()
                            .with_timezone("+00:00"),
                    ) as arrow_array::ArrayRef
                };
            }
            let array = match unit {
                arrow_schema::TimeUnit::Second => utc!(arrow_array::TimestampSecondArray),
                arrow_schema::TimeUnit::Millisecond => utc!(arrow_array::TimestampMillisecondArray),
                arrow_schema::TimeUnit::Microsecond => utc!(arrow_array::TimestampMicrosecondArray),
                arrow_schema::TimeUnit::Nanosecond => utc!(arrow_array::TimestampNanosecondArray),
            };
            return Ok(Some(ArrayFormatter::new(
                Box::new(UtcDisplay {
                    array,
                    options: options.clone(),
                }),
                options.safe(),
            )));
        }
        if matches!(
            array.data_type(),
            DataType::Binary
                | DataType::LargeBinary
                | DataType::BinaryView
                | DataType::FixedSizeBinary(_)
        ) {
            return Ok(Some(ArrayFormatter::new(
                Box::new(BinaryDisplay {
                    array,
                    null: options.null(),
                }),
                options.safe(),
            )));
        }
        Ok(None)
    }
}

struct UtcDisplay<'a> {
    array: arrow_array::ArrayRef,
    options: FormatOptions<'a>,
}
impl DisplayIndex for UtcDisplay<'_> {
    fn write(&self, row: usize, out: &mut dyn Write) -> FormatResult {
        ArrayFormatter::try_new(self.array.as_ref(), &self.options)?
            .value(row)
            .write(out)?;
        Ok(())
    }
}

struct BinaryDisplay<'a> {
    array: &'a dyn Array,
    null: &'a str,
}
impl DisplayIndex for BinaryDisplay<'_> {
    fn write(&self, row: usize, out: &mut dyn Write) -> FormatResult {
        if self.array.is_null(row) {
            out.write_str(self.null)?;
        } else if let Some(bytes) = binary(self.array, row) {
            for b in bytes.iter().take(64) {
                write!(out, "{b:02x}")?;
            }
            if bytes.len() > 64 {
                write!(out, "… ({} bytes)", bytes.len())?;
            }
        }
        Ok(())
    }
}

struct DisplayText {
    text: String,
    remaining: usize,
}
impl Write for DisplayText {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.remaining = self.remaining.checked_sub(s.len()).ok_or(std::fmt::Error)?;
        self.text.push_str(s);
        Ok(())
    }
}

pub(in crate::table) fn append(
    batch: &RecordBatch,
    from: usize,
    count: usize,
    out: &mut Vec<Vec<Option<String>>>,
    display_bytes: &mut usize,
    limits: Limits,
) -> Result<(), String> {
    let end = from
        .checked_add(count)
        .filter(|&end| end <= batch.num_rows())
        .ok_or("Invalid batch row range")?;
    let slots = count
        .checked_mul(batch.num_columns())
        .and_then(|n| n.checked_mul(std::mem::size_of::<Option<String>>()))
        .ok_or("Page cell count overflows")?;
    charge(display_bytes, slots, limits.decoded)?;
    let options = FormatOptions::default()
        .with_display_error(true)
        .with_null("null")
        .with_formatter_factory(Some(&PreviewFactory));
    for row in from..end {
        let mut cells = Vec::with_capacity(batch.num_columns());
        for col in batch.columns() {
            let (array, index) = dictionary_value(col.as_ref(), row)?;
            if array.is_null(index) {
                cells.push(None);
                continue;
            }
            let value = if let Some(bytes) = binary(array, index) {
                let mut s = String::new();
                for b in bytes.iter().take(64) {
                    let _ = write!(s, "{b:02x}");
                }
                if bytes.len() > 64 {
                    let _ = write!(s, "… ({} bytes)", bytes.len());
                }
                s
            } else {
                let fmt = PreviewFactory
                    .create_array_formatter(array, &options, None)
                    .and_then(|f| {
                        f.map(Ok)
                            .unwrap_or_else(|| ArrayFormatter::try_new(array, &options))
                    })
                    .map_err(|e| e.to_string())?;
                let mut text = DisplayText {
                    text: String::new(),
                    remaining: limits.decoded.saturating_sub(*display_bytes),
                };
                write!(text, "{}", fmt.value(index))
                    .map_err(|_| "Cell display exceeds the table byte budget")?;
                text.text
            };
            charge(display_bytes, value.len(), limits.decoded)?;
            cells.push(Some(cut(value)));
        }
        out.push(cells);
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::table) mod tests;
