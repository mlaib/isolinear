//! Bounded archive listings and explicit extraction of one regular file into a private folder.
//! ZIP opens only its central directory. Plain tar skips payloads on disk; compressed tar scans
//! at most 256 MiB. Single compressed streams are probed, then decoded only on extraction.

use std::fs::{File, OpenOptions};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::{Column, Schema, TableInfo, TableSource, cut};
use reader::{Compression, EXTRACT_LIMIT, Entry, Listing, TAR_SCAN_LIMIT};

pub(crate) mod reader;
#[cfg(test)]
mod tests;

pub const READY: bool = true;

/// The listing of the archive at `path`. Call from a worker; this function performs I/O.
pub fn open(
    format: &str,
    path: &Path,
    table: Option<&str>,
) -> Result<Box<dyn TableSource>, String> {
    open_with_cancel(format, path, table, &|| false)
}

/// Cancellable counterpart for workers that have a cancellation token.
pub fn open_with_cancel(
    format: &str,
    path: &Path,
    _table: Option<&str>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Box<dyn TableSource>, String> {
    let mut file = Checked {
        inner: File::open(path).map_err(|e| e.to_string())?,
        cancelled,
    };
    let (kind, listing) = listing(format, path, &mut file, cancelled).map_err(|e| e.to_string())?;
    let rows = listing.entries.len() as u64;
    let mut details = Vec::new();
    if let Some(count) = listing.declared {
        details.push(("Declared entries".into(), count.to_string()));
    }
    if !listing.comment.is_empty() {
        details.push(("Comment".into(), cut(listing.comment.clone())));
    }
    details.extend(
        listing
            .warnings
            .iter()
            .map(|w| ("Warning".into(), cut(w.clone()))),
    );
    if matches!(kind, Kind::Stream(_)) {
        details.push((
            "Size".into(),
            "Uncompressed size is unknown until extraction".into(),
        ));
    }
    let schema = Schema {
        tables: vec![TableInfo {
            name: file_name(path),
            rows: Some(rows),
        }],
        columns: ["Name", "Size", "Packed", "Modified", "Method", "Notes"]
            .into_iter()
            .map(|name| Column {
                name: name.into(),
                type_name: "text".into(),
            })
            .collect(),
        rows: Some(rows),
        details,
        ..Default::default()
    };
    Ok(Box::new(Archive {
        schema,
        listing,
        kind,
    }))
}

struct Archive {
    schema: Schema,
    listing: Listing,
    kind: Kind,
}
impl TableSource for Archive {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn rows(&mut self, start: u64, count: usize) -> Result<Vec<Vec<Option<String>>>, String> {
        let start = start.min(self.listing.entries.len() as u64) as usize;
        Ok(self
            .listing
            .entries
            .iter()
            .skip(start)
            .take(count)
            .map(|entry| {
                let mut notes = Vec::new();
                if entry.dir {
                    notes.push("folder");
                }
                if !entry.special.is_empty() {
                    notes.push(&entry.special);
                }
                if entry.encrypted {
                    notes.push("encrypted");
                }
                if entry.unsafe_path {
                    notes.push("unsafe path: absolute or '..' component");
                }
                if !entry.openable && !entry.dir && !entry.encrypted && entry.special.is_empty() {
                    notes.push("compression method not supported");
                }
                vec![
                    Some(cut(entry.name.clone())),
                    (!matches!(self.kind, Kind::Stream(_))).then(|| entry.size.to_string()),
                    entry.packed.map(|n| n.to_string()),
                    Some(entry.modified.clone()),
                    Some(entry.method.clone()),
                    Some(cut(notes.join("; "))),
                ]
            })
            .collect())
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Zip,
    Tar(Option<Compression>),
    Stream(Compression),
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| "archive".into(), |s| s.to_string_lossy().into_owned())
}

fn listing(
    format: &str,
    path: &Path,
    file: &mut Checked<'_, File>,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<(Kind, Listing)> {
    match format {
        "zip" => return reader::list_zip(file, cancelled).map(|l| (Kind::Zip, l)),
        "tar" => return reader::list_tar_file(file, cancelled).map(|l| (Kind::Tar(None), l)),
        _ => {}
    }
    let compression = match format {
        "gzip" => Compression::Gzip,
        "zstd" => Compression::Zstd,
        "xz" => Compression::Xz,
        "bzip2" => Compression::Bzip2,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown archive format: {format}"),
            ));
        }
    };
    let packed = file.inner.metadata()?.len();
    let mut name = path
        .file_stem()
        .map_or_else(|| "content".into(), |s| s.to_string_lossy().into_owned());
    let mut modified = String::new();
    if compression == Compression::Gzip {
        let decoder = flate2::read::MultiGzDecoder::new(&mut *file);
        if let Some(header) = decoder.header() {
            if let Some(recorded) = header.filename()
                && !recorded.is_empty()
            {
                name = String::from_utf8_lossy(recorded).into_owned();
            }
            if header.mtime() != 0 {
                modified = reader::utc(header.mtime() as i64);
            }
        }
        file.seek(SeekFrom::Start(0))?;
    }
    let mut decoded = Checked {
        inner: compression.reader(file)?,
        cancelled,
    };
    let mut prefix = Vec::new();
    decoded.by_ref().take(512).read_to_end(&mut prefix)?;
    let outer = file_name(path).to_ascii_lowercase();
    let tar_name = name.to_ascii_lowercase().ends_with(".tar")
        || [
            ".tar.gz", ".tar.zst", ".tar.xz", ".tar.bz2", ".tgz", ".tzst", ".txz", ".tbz", ".tbz2",
        ]
        .iter()
        .any(|suffix| outer.ends_with(suffix));
    if tar_name || reader::is_tar_header(&prefix) {
        let mut stream = Cursor::new(prefix).chain(decoded);
        return reader::list_tar(&mut stream, TAR_SCAN_LIMIT, cancelled)
            .map(|l| (Kind::Tar(Some(compression)), l));
    }
    let entry = Entry {
        unsafe_path: reader::unsafe_path(&name),
        name,
        packed: Some(packed),
        method: compression.name().into(),
        modified,
        openable: true,
        ..Default::default()
    };
    Ok((
        Kind::Stream(compression),
        Listing {
            entries: vec![entry],
            ..Default::default()
        },
    ))
}

/// Copies one regular entry into the existing private `dir`, using only its last name component.
/// Existing files and symlinks are never overwritten. An unsuccessful copy is removed.
pub fn extract(format: &str, path: &Path, row: u64, dir: &Path) -> Result<PathBuf, String> {
    extract_with_cancel(format, path, row, dir, &|| false)
}

/// Cancellable extraction; cancellation also deletes the partial copy.
pub fn extract_with_cancel(
    format: &str,
    path: &Path,
    row: u64,
    dir: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    let run = || -> io::Result<PathBuf> {
        let mut file = Checked {
            inner: File::open(path)?,
            cancelled,
        };
        let (kind, listing) = listing(format, path, &mut file, cancelled)?;
        let entry = usize::try_from(row)
            .ok()
            .and_then(|row| listing.entries.get(row))
            .ok_or_else(|| io::Error::other("the entry was not found in the archive listing"))?;
        if !entry.openable {
            return Err(io::Error::other(
                "only regular files with supported compression can be opened; folders, links and encrypted entries cannot",
            ));
        }
        if !matches!(kind, Kind::Stream(_)) && entry.size > EXTRACT_LIMIT {
            return Err(io::Error::other(
                "the entry exceeds the 256 MiB extraction limit",
            ));
        }
        let destination = dir.join(safe_name(&entry.name));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut out = Checked {
            inner: options.open(&destination)?,
            cancelled,
        };
        let result = (|| -> io::Result<()> {
            file.seek(SeekFrom::Start(0))?;
            match kind {
                Kind::Zip => {
                    reader::extract_zip(&mut file, entry, &mut out)?;
                }
                Kind::Tar(None) => {
                    file.seek(SeekFrom::Start(entry.offset))?;
                    let copied = io::copy(&mut file.take(entry.size), &mut out)?;
                    if copied != entry.size {
                        return Err(io::Error::other("the tar entry is cut short"));
                    }
                }
                Kind::Tar(Some(compression)) => {
                    let mut decoded = Checked {
                        inner: compression.reader(file)?,
                        cancelled,
                    };
                    reader::extract_tar(
                        &mut decoded,
                        row as usize,
                        TAR_SCAN_LIMIT,
                        &mut out,
                        cancelled,
                    )?;
                }
                Kind::Stream(compression) => {
                    reader::extract_stream(compression, file, &mut out)?;
                }
            }
            out.flush()
        })();
        drop(out);
        if let Err(error) = result {
            std::fs::remove_file(&destination).map_err(|cleanup| {
                io::Error::other(format!(
                    "{error}; could not remove the partial copy: {cleanup}"
                ))
            })?;
            return Err(error);
        }
        Ok(destination)
    };
    run().map_err(|e| e.to_string())
}

fn safe_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = last
        .chars()
        .filter(|c| !c.is_control() && !"/\\:<>\"|?*".contains(*c))
        .collect();
    let clean = clean.trim_end_matches(['.', ' ']);
    let device = clean.split('.').next().unwrap_or("").to_ascii_uppercase();
    if clean.is_empty()
        || matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (device.len() == 4
            && (device.starts_with("COM") || device.starts_with("LPT"))
            && device.as_bytes()[3].is_ascii_digit())
    {
        "entry".into()
    } else {
        clean.to_owned()
    }
}

/// Check between bounded reads/writes as well as headers, including during decompression.
struct Checked<'a, R> {
    inner: R,
    cancelled: &'a dyn Fn() -> bool,
}
impl<R> Checked<'_, R> {
    fn check(&self) -> io::Result<()> {
        if (self.cancelled)() {
            Err(io::Error::other("cancelled"))
        } else {
            Ok(())
        }
    }
}
impl<R: Read> Read for Checked<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        let len = buf.len().min(64 * 1024);
        self.inner.read(&mut buf[..len])
    }
}
impl<R: Seek> Seek for Checked<'_, R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.check()?;
        self.inner.seek(pos)
    }
}
impl<R: Write> Write for Checked<'_, R> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.check()?;
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        self.inner.flush()
    }
}
