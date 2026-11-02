//! Metadata-first Parquet tables. Only the row groups covering a requested page are decoded.

use std::fs::{File, Metadata};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReaderBuilder,
};
use parquet::file::metadata::ParquetMetaDataReader;

use super::{Schema, TableSource, cut};
#[path = "columnar/mod.rs"]
pub(super) mod columnar;
use columnar::{Limits, append, charge, guard, page_end, unchanged};

pub const READY: bool = true;

pub fn open(path: &Path, _table: Option<&str>) -> Result<Box<dyn TableSource>, String> {
    guard(|| Parquet::open(path, Limits::default()).map(|s| Box::new(s) as Box<dyn TableSource>))
}

struct Parquet {
    file: File,
    stamp: Metadata,
    metadata: ArrowReaderMetadata,
    schema: Schema,
    limits: Limits,
}

impl Parquet {
    fn open(path: &Path, limits: Limits) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let stamp = file.metadata().map_err(|e| e.to_string())?;
        let size = stamp.len();
        if size < 12 {
            return Err("Not a Parquet file: too short".into());
        }
        let mut head = [0; 4];
        let mut tail = [0; 8];
        file.read_exact(&mut head).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::End(-8)).map_err(|e| e.to_string())?;
        file.read_exact(&mut tail).map_err(|e| e.to_string())?;
        if tail.get(4..) == Some(b"PARE") {
            return Err("Encrypted Parquet files are not supported".into());
        }
        if head != *b"PAR1" || tail.get(4..) != Some(b"PAR1") {
            return Err("Invalid Parquet signature: file is truncated or not Parquet".into());
        }
        let len = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) as usize;
        if len as u64 > size - 12 {
            return Err("Truncated Parquet footer".into());
        }
        if len > limits.metadata {
            return Err(format!(
                "Parquet metadata limit exceeded: {len} bytes (limit {})",
                limits.metadata
            ));
        }
        let footer_start = size - 8 - len as u64;
        let mut footer = vec![0; len];
        file.seek(SeekFrom::Start(footer_start))
            .map_err(|e| e.to_string())?;
        file.read_exact(&mut footer).map_err(|e| e.to_string())?;
        columnar::compact::footer(&footer, limits)?;
        let metadata =
            ParquetMetaDataReader::decode_metadata(&footer).map_err(|e| e.to_string())?;
        if metadata.num_row_groups() > limits.batches {
            return Err(format!("More than {} row groups", limits.batches));
        }
        let rows = u64::try_from(metadata.file_metadata().num_rows())
            .map_err(|_| "Negative Parquet row count")?;
        let mut total = 0u64;
        for group in metadata.row_groups() {
            total = total
                .checked_add(
                    u64::try_from(group.num_rows()).map_err(|_| "Negative row group length")?,
                )
                .ok_or("Row count overflows")?;
            for col in group.columns() {
                if col.file_path().is_some_and(|p| !p.is_empty()) {
                    return Err("External Parquet column files are not opened".into());
                }
                let offset = u64::try_from(
                    col.dictionary_page_offset()
                        .unwrap_or(col.data_page_offset()),
                )
                .map_err(|_| "Negative column offset")?;
                let len =
                    u64::try_from(col.compressed_size()).map_err(|_| "Negative column length")?;
                if offset < 4
                    || offset.checked_add(len).is_none_or(|end| end > footer_start)
                    || col.uncompressed_size() < 0
                    || col.num_values() < 0
                {
                    return Err("Parquet column chunk lies outside the data section".into());
                }
            }
        }
        if total != rows {
            return Err("Parquet row groups do not match the file's row count".into());
        }
        let metadata = ArrowReaderMetadata::try_new(
            std::sync::Arc::new(metadata),
            ArrowReaderOptions::default(),
        )
        .map_err(|e| e.to_string())?;
        let mut schema = columnar::schema(path, metadata.schema(), rows, limits)?;
        let md = metadata.metadata();
        let fmd = md.file_metadata();
        schema.details = vec![
            ("Rows".into(), rows.to_string()),
            ("Row groups".into(), md.num_row_groups().to_string()),
            ("Format version".into(), fmd.version().to_string()),
        ];
        if let Some(by) = fmd.created_by() {
            schema
                .details
                .push(("Created by".into(), cut(by.to_owned())));
        }
        for (i, col) in metadata.parquet_schema().columns().iter().enumerate() {
            let mut codecs = std::collections::BTreeSet::new();
            for rg in md.row_groups() {
                let c = rg
                    .columns()
                    .get(i)
                    .ok_or("Missing Parquet column metadata")?;
                codecs.insert(format!("{:?}", c.compression()));
            }
            schema.details.push((
                format!("Compression: {}", col.path().string()),
                codecs.into_iter().collect::<Vec<_>>().join(", "),
            ));
        }
        if let Some(kv) = fmd.key_value_metadata() {
            schema.details.extend(
                kv.iter()
                    .map(|e| (e.key.clone(), cut(e.value.clone().unwrap_or_default()))),
            );
        }
        let mut text = Vec::new();
        parquet::schema::printer::print_schema(&mut text, metadata.parquet_schema().root_schema());
        schema.text = Some(String::from_utf8_lossy(&text).into_owned());
        Ok(Self {
            file,
            stamp,
            metadata,
            schema,
            limits,
        })
    }

    fn read(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        unchanged(&self.file, &self.stamp)?;
        let end = page_end(start, count, self.schema.rows.unwrap_or(0), self.limits)?;
        if start >= end {
            return Ok(Vec::new());
        }
        let mut groups = Vec::new();
        let mut at = 0u64;
        let mut offset = 0;
        let mut decoded = 0;
        let mut encoded = 0;
        let mut pages = 0usize;
        for (i, group) in self.metadata.metadata().row_groups().iter().enumerate() {
            let next = at
                .checked_add(group.num_rows() as u64)
                .ok_or("Row count overflows")?;
            if next > start && at < end {
                if groups.is_empty() {
                    offset = start - at;
                }
                groups.push(i);
                for col in group.columns() {
                    let (mut pos, len) = col.byte_range();
                    let mut left = usize::try_from(len).map_err(|_| "Column length overflows")?;
                    charge(&mut encoded, left, self.limits.decoded)?;
                    while left > 0 {
                        pages += 1;
                        if pages > self.limits.batches {
                            return Err(format!(
                                "More than {} Parquet pages in one read",
                                self.limits.batches
                            ));
                        }
                        self.file
                            .seek(SeekFrom::Start(pos))
                            .map_err(|e| e.to_string())?;
                        let (header, body, bytes) = columnar::compact::page(
                            BufReader::new(&mut self.file),
                            left,
                            self.limits.decoded.saturating_sub(decoded),
                        )?;
                        charge(&mut decoded, bytes, self.limits.decoded)?;
                        let consumed = header.checked_add(body).ok_or("Page size overflows")?;
                        left = left
                            .checked_sub(consumed)
                            .ok_or("Page exceeds its column chunk")?;
                        pos = pos
                            .checked_add(consumed as u64)
                            .ok_or("Page offset overflows")?;
                    }
                }
            }
            at = next;
        }
        let reader = ParquetRecordBatchReaderBuilder::new_with_metadata(
            self.file.try_clone().map_err(|e| e.to_string())?,
            self.metadata.clone(),
        )
        .with_row_groups(groups)
        .with_offset(usize::try_from(offset).map_err(|_| "Row offset overflows")?)
        .with_limit((end - start) as usize)
        .with_batch_size(
            count
                .clamp(1, 1024)
                .min((self.limits.decoded / decoded.max(1)).max(1)),
        )
        .build()
        .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut actual = 0;
        let mut display = 0;
        for batch in reader {
            let batch = batch.map_err(|e| e.to_string())?;
            charge(
                &mut actual,
                batch.get_array_memory_size(),
                self.limits.decoded,
            )?;
            append(
                &batch,
                0,
                batch.num_rows(),
                &mut out,
                &mut display,
                self.limits,
            )?;
        }
        unchanged(&self.file, &self.stamp)?;
        Ok(out)
    }
}

impl TableSource for Parquet {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn rows(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        guard(|| self.read(start, count))
    }
}

#[cfg(test)]
#[path = "columnar/parquet_tests.rs"]
mod tests;
