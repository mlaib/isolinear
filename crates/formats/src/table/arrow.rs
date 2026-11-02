//! Arrow IPC files, streams and Feather v2. Opening indexes headers; pages decode selected batches.

use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use arrow_ipc::{Block, MessageHeader, MetadataVersion, reader::FileDecoder};
use arrow_schema::Schema as ArrowSchema;

use super::parquet::columnar::{self, Limits, append, charge, guard, page_end, unchanged};
use super::{Schema, TableSource, cut};

pub const READY: bool = true;
const MAGIC: &[u8] = b"ARROW1";
const MESSAGE_MAX: usize = 16 << 20;

pub fn open(path: &Path, _table: Option<&str>) -> Result<Box<dyn TableSource>, String> {
    guard(|| Arrow::open(path, Limits::default()).map(|s| Box::new(s) as Box<dyn TableSource>))
}

struct Entry {
    block: Block,
    header: Vec<u8>,
    prefix: usize,
    first: u64,
    rows: Option<u64>,
}

struct Arrow {
    file: File,
    stamp: Metadata,
    schema: Schema,
    arrow: Arc<ArrowSchema>,
    version: MetadataVersion,
    entries: Vec<Entry>,
    limits: Limits,
}

fn read_at(file: &mut File, at: u64, bytes: &mut [u8]) -> Result<(), String> {
    file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    file.read_exact(bytes).map_err(|e| e.to_string())
}

fn schema(fb: arrow_ipc::Schema<'_>, limits: Limits) -> Result<ArrowSchema, String> {
    let fields = fb.fields().ok_or("Arrow schema has no fields")?;
    if fields.len() > limits.columns {
        return Err("Arrow schema exceeds the column limit".into());
    }
    let mut stack: Vec<_> = fields.iter().map(|f| (f, 1)).collect();
    let mut total = 0;
    while let Some((field, depth)) = stack.pop() {
        total += 1;
        if total > limits.columns || depth > 64 {
            return Err("Arrow schema exceeds the field or nesting limit".into());
        }
        if let Some(t) = field.type_as_fixed_size_list()
            && (t.listSize() < 0 || t.listSize() as usize > limits.decoded / 8)
        {
            return Err("Invalid or oversized fixed-size list".into());
        }
        if let Some(t) = field.type_as_fixed_size_binary()
            && (t.byteWidth() < 0 || t.byteWidth() as usize > limits.decoded)
        {
            return Err("Invalid or oversized fixed-size binary".into());
        }
        if let Some(children) = field.children() {
            if children.len() > limits.columns {
                return Err("Arrow schema exceeds the column limit".into());
            }
            stack.extend(children.iter().map(|f| (f, depth + 1)));
        }
    }
    arrow_ipc::convert::try_fb_to_schema(fb).map_err(|e| e.to_string())
}

/// Reads a bounded IPC message header and checks its body extent without allocating the body.
fn header(
    file: &mut File,
    at: u64,
    end: u64,
    budget: &mut usize,
    limits: Limits,
) -> Result<Option<(Vec<u8>, usize, u64)>, String> {
    if at == end {
        return Ok(None);
    }
    if at.checked_add(4).is_none_or(|p| p > end) {
        return Err("Truncated Arrow message length".into());
    }
    let mut first = [0; 4];
    read_at(file, at, &mut first)?;
    let mut bytes = first.to_vec();
    let prefix = if first == [0xff; 4] {
        if at.checked_add(8).is_none_or(|p| p > end) {
            return Err("Truncated Arrow continuation marker".into());
        }
        read_at(file, at + 4, &mut first)?;
        bytes.extend(first);
        8
    } else {
        4
    };
    let len = i32::from_le_bytes(first);
    if len == 0 {
        return Ok(None);
    }
    let len = usize::try_from(len).map_err(|_| "Negative Arrow message length")?;
    if len > MESSAGE_MAX {
        return Err("Arrow message exceeds the 16 MiB header limit".into());
    }
    let size = prefix + len;
    let body_start = at
        .checked_add(size as u64)
        .filter(|&p| p <= end)
        .ok_or("Arrow message extends beyond the file")?;
    charge(budget, size, limits.metadata).map_err(|_| "Arrow metadata limit exceeded")?;
    bytes.resize(size, 0);
    read_at(
        file,
        at + prefix as u64,
        bytes.get_mut(prefix..).ok_or("Invalid Arrow header")?,
    )?;
    let message = arrow_ipc::root_as_message(bytes.get(prefix..).ok_or("Invalid Arrow header")?)
        .map_err(|e| format!("Invalid Arrow message: {e}"))?;
    let body = u64::try_from(message.bodyLength()).map_err(|_| "Negative Arrow body length")?;
    if body_start.checked_add(body).is_none_or(|p| p > end) {
        return Err("Arrow body extends beyond the file".into());
    }
    Ok(Some((bytes, prefix, body)))
}

fn entry(
    at: u64,
    header: Vec<u8>,
    prefix: usize,
    body: u64,
    first: u64,
    version: MetadataVersion,
) -> Result<Entry, String> {
    let message = arrow_ipc::root_as_message(header.get(prefix..).ok_or("Invalid Arrow header")?)
        .map_err(|e| e.to_string())?;
    if message.version() != version {
        return Err("Arrow message version differs from its schema".into());
    }
    let rows = match message.header_type() {
        MessageHeader::RecordBatch => Some(
            u64::try_from(
                message
                    .header_as_record_batch()
                    .ok_or("Missing record batch header")?
                    .length(),
            )
            .map_err(|_| "Negative Arrow row count")?,
        ),
        MessageHeader::DictionaryBatch => {
            message
                .header_as_dictionary_batch()
                .and_then(|d| d.data())
                .ok_or("Missing dictionary batch data")?;
            None
        }
        _ => return Err("Expected an Arrow record batch or dictionary".into()),
    };
    let block = Block::new(
        i64::try_from(at).map_err(|_| "Arrow offset overflows")?,
        i32::try_from(header.len()).map_err(|_| "Arrow header size overflows")?,
        i64::try_from(body).map_err(|_| "Arrow body size overflows")?,
    );
    // Validate all buffer extents from headers, including batches that are not paged yet.
    let batch = message
        .header_as_record_batch()
        .or_else(|| message.header_as_dictionary_batch().and_then(|d| d.data()))
        .ok_or("Missing Arrow batch header")?;
    if let Some(buffers) = batch.buffers() {
        for buffer in buffers {
            let offset =
                u64::try_from(buffer.offset()).map_err(|_| "Negative Arrow buffer offset")?;
            let len = u64::try_from(buffer.length()).map_err(|_| "Negative Arrow buffer length")?;
            if offset.checked_add(len).is_none_or(|p| p > body) {
                return Err("Arrow buffer lies outside the batch body".into());
            }
        }
    }
    Ok(Entry {
        block,
        header,
        prefix,
        first,
        rows,
    })
}

impl Arrow {
    fn open(path: &Path, limits: Limits) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let stamp = file.metadata().map_err(|e| e.to_string())?;
        let len = stamp.len();
        if len < 8 {
            return Err("Not an Arrow IPC file or stream: too short".into());
        }
        let mut signature = [0; 6];
        read_at(&mut file, 0, &mut signature)?;
        let mut budget = 0;
        let mut entries = Vec::new();
        let mut rows = 0u64;
        let mut details = Vec::new();
        let (arrow, version, kind) = if signature == MAGIC {
            if len < 18 {
                return Err("Truncated Arrow IPC file".into());
            }
            let mut tail = [0; 10];
            read_at(&mut file, len - 10, &mut tail)?;
            if tail.get(4..) != Some(MAGIC) {
                return Err("Truncated Arrow IPC closing signature".into());
            }
            let footer_len =
                usize::try_from(i32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]))
                    .map_err(|_| "Negative Arrow footer length")?;
            if footer_len as u64 > len - 18 {
                return Err("Arrow footer length extends beyond the file".into());
            }
            charge(&mut budget, footer_len, limits.metadata)
                .map_err(|_| "Arrow metadata limit exceeded")?;
            let data_end = len - 10 - footer_len as u64;
            let mut bytes = vec![0; footer_len];
            read_at(&mut file, data_end, &mut bytes)?;
            let footer = arrow_ipc::root_as_footer(&bytes)
                .map_err(|e| format!("Invalid Arrow footer: {e}"))?;
            let arrow = schema(footer.schema().ok_or("Arrow footer has no schema")?, limits)?;
            let version = footer.version();
            if let Some(metadata) = footer.custom_metadata() {
                details.extend(metadata.iter().filter_map(|kv| {
                    Some((
                        kv.key()?.to_owned(),
                        cut(kv.value().unwrap_or("").to_owned()),
                    ))
                }));
            }
            let mut blocks = Vec::new();
            for (list, dictionary) in [
                (footer.dictionaries(), true),
                (footer.recordBatches(), false),
            ] {
                if let Some(list) = list {
                    if list.len() > limits.batches
                        || list.len().saturating_add(blocks.len()) > limits.batches
                    {
                        return Err("Arrow batch count exceeds the limit".into());
                    }
                    for block in list {
                        blocks.push((*block, dictionary));
                    }
                }
            }
            blocks.sort_by_key(|(b, _)| b.offset());
            let mut previous_end = 8;
            for (block, dictionary) in blocks {
                let at =
                    u64::try_from(block.offset()).map_err(|_| "Negative Arrow block offset")?;
                let meta = u64::try_from(block.metaDataLength())
                    .map_err(|_| "Negative Arrow block metadata length")?;
                let body = u64::try_from(block.bodyLength())
                    .map_err(|_| "Negative Arrow block body length")?;
                let end = at
                    .checked_add(meta)
                    .and_then(|p| p.checked_add(body))
                    .ok_or("Arrow block extent overflows")?;
                if at < previous_end
                    || end > data_end
                    || !(4..=MESSAGE_MAX as u64 + 8).contains(&meta)
                {
                    return Err(
                        "Arrow footer block overlaps or lies outside the data section".into(),
                    );
                }
                let (header, prefix, actual_body) =
                    header(&mut file, at, data_end, &mut budget, limits)?
                        .ok_or("Expected an Arrow batch, found end of stream")?;
                if header.len() as u64 != meta || actual_body != body {
                    return Err("Arrow footer block lengths differ from the message header".into());
                }
                let e = entry(at, header, prefix, body, rows, version)?;
                if dictionary != e.rows.is_none() {
                    return Err("Arrow footer block has the wrong message type".into());
                }
                rows = rows
                    .checked_add(e.rows.unwrap_or(0))
                    .ok_or("Arrow row count overflows")?;
                entries.push(e);
                previous_end = end;
            }
            (arrow, version, "Arrow IPC file / Feather v2")
        } else {
            let (bytes, prefix, body) = header(&mut file, 0, len, &mut budget, limits)?
                .ok_or("Arrow stream has no schema")?;
            let message = arrow_ipc::root_as_message(
                bytes.get(prefix..).ok_or("Invalid Arrow schema header")?,
            )
            .map_err(|e| e.to_string())?;
            if body != 0 {
                return Err("Arrow schema message unexpectedly has a body".into());
            }
            let arrow = schema(
                message
                    .header_as_schema()
                    .ok_or("Expected an Arrow schema as the first stream message")?,
                limits,
            )?;
            let version = message.version();
            let mut at = bytes.len() as u64;
            while let Some((bytes, prefix, body)) = header(&mut file, at, len, &mut budget, limits)?
            {
                if entries.len() >= limits.batches {
                    return Err("Arrow batch count exceeds the limit".into());
                }
                let next = at
                    .checked_add(bytes.len() as u64)
                    .and_then(|p| p.checked_add(body))
                    .ok_or("Arrow message extent overflows")?;
                let e = entry(at, bytes, prefix, body, rows, version)?;
                rows = rows
                    .checked_add(e.rows.unwrap_or(0))
                    .ok_or("Arrow row count overflows")?;
                entries.push(e);
                at = next;
            }
            (arrow, version, "Arrow IPC stream")
        };
        let mut schema = columnar::schema(path, &arrow, rows, limits)?;
        schema.details = vec![
            ("Format".into(), kind.into()),
            ("Rows".into(), rows.to_string()),
            (
                "Batches".into(),
                entries
                    .iter()
                    .filter(|e| e.rows.is_some())
                    .count()
                    .to_string(),
            ),
        ];
        details.extend(
            arrow
                .metadata()
                .iter()
                .map(|(k, v)| (k.clone(), cut(v.clone()))),
        );
        details.sort();
        schema.details.extend(details);
        schema.text = Some(format!("{:#?}", arrow.fields()));
        Ok(Self {
            file,
            stamp,
            schema,
            arrow: Arc::new(arrow),
            version,
            entries,
            limits,
        })
    }

    fn read(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        unchanged(&self.file, &self.stamp)?;
        let end = page_end(start, count, self.schema.rows.unwrap_or(0), self.limits)?;
        if start >= end {
            return Ok(Vec::new());
        }
        let mut decoder = FileDecoder::new(self.arrow.clone(), self.version);
        let mut dictionaries = std::collections::HashMap::<i64, usize>::new();
        let mut encoded = 0;
        let mut decoded = 0;
        let mut actual = 0;
        let mut display = 0;
        let mut out = Vec::new();
        for entry in &self.entries {
            if entry.first >= end {
                break;
            }
            if let Some(n) = entry.rows
                && entry.first + n <= start
            {
                continue;
            }
            let body_len = usize::try_from(entry.block.bodyLength())
                .map_err(|_| "Arrow body length overflows")?;
            charge(&mut encoded, body_len, self.limits.decoded)?;
            let message = arrow_ipc::root_as_message(
                entry
                    .header
                    .get(entry.prefix..)
                    .ok_or("Invalid Arrow header")?,
            )
            .map_err(|e| e.to_string())?;
            let batch = message
                .header_as_record_batch()
                .or_else(|| message.header_as_dictionary_batch().and_then(|d| d.data()))
                .ok_or("Missing Arrow batch header")?;
            let body_start = entry.block.offset() as u64 + entry.header.len() as u64;
            if let Some(nodes) = batch.nodes() {
                for node in nodes {
                    if node.length() < 0
                        || node.length() as u64 > self.limits.decoded as u64 / 8
                        || node.null_count() < 0
                        || node.null_count() > node.length()
                    {
                        return Err("Arrow field length exceeds the value limit or has an invalid null count".into());
                    }
                }
            }
            let before = decoded;
            if let Some(buffers) = batch.buffers() {
                for buffer in buffers {
                    let len = buffer.length() as usize;
                    let size = if batch.compression().is_some() && len > 0 {
                        if len < 8 {
                            return Err("Compressed Arrow buffer has no length prefix".into());
                        }
                        let mut prefix = [0; 8];
                        read_at(
                            &mut self.file,
                            body_start + buffer.offset() as u64,
                            &mut prefix,
                        )?;
                        match i64::from_le_bytes(prefix) {
                            -1 => len - 8,
                            n => usize::try_from(n)
                                .map_err(|_| "Invalid Arrow decompressed buffer length")?,
                        }
                    } else {
                        len
                    };
                    charge(&mut decoded, size, self.limits.decoded)?;
                }
            }
            if let Some(dictionary) = message.header_as_dictionary_batch() {
                let mut bytes = decoded - before;
                if dictionary.isDelta() {
                    let prior = dictionaries.get(&dictionary.id()).copied().unwrap_or(0);
                    charge(&mut decoded, prior, self.limits.decoded)?;
                    bytes = bytes
                        .checked_add(prior)
                        .ok_or("Dictionary size overflows")?;
                }
                dictionaries.insert(dictionary.id(), bytes);
            }
            let length = entry
                .header
                .len()
                .checked_add(body_len)
                .ok_or("Arrow block size overflows")?;
            let mut bytes = Vec::with_capacity(length);
            bytes.extend_from_slice(&entry.header);
            bytes.resize(length, 0);
            read_at(
                &mut self.file,
                body_start,
                bytes
                    .get_mut(entry.header.len()..)
                    .ok_or("Invalid Arrow body range")?,
            )?;
            let bytes = bytes.into();
            if entry.rows.is_none() {
                decoder
                    .read_dictionary(&entry.block, &bytes)
                    .map_err(|e| e.to_string())?;
            } else {
                let batch = decoder
                    .read_record_batch(&entry.block, &bytes)
                    .map_err(|e| e.to_string())?
                    .ok_or("Expected an Arrow record batch")?;
                if Some(batch.num_rows() as u64) != entry.rows {
                    return Err("Arrow decoded row count differs from its header".into());
                }
                charge(
                    &mut actual,
                    batch.get_array_memory_size(),
                    self.limits.decoded,
                )?;
                let from = start.saturating_sub(entry.first) as usize;
                let to = (end - entry.first).min(batch.num_rows() as u64) as usize;
                append(&batch, from, to - from, &mut out, &mut display, self.limits)?;
            }
        }
        unchanged(&self.file, &self.stamp)?;
        Ok(out)
    }
}

impl TableSource for Arrow {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn rows(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        guard(|| self.read(start, count))
    }
}

#[cfg(test)]
#[path = "columnar/arrow_tests.rs"]
mod tests;
