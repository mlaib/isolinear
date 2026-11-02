//! Any file as bytes: hex rows read on demand, and the facts its header gives.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use isolinear_core::limits;
use serde::Serialize;

/// Bytes per hex row.
const ROW: u64 = 16;
/// Rows returned by one request at most.
const ROWS_MAX: u64 = 4096;

#[derive(Serialize)]
pub struct HexRows {
    /// Byte offset of the first row.
    offset: u64,
    /// The bytes, `ROW` per row (the last row may be shorter).
    bytes: Vec<u8>,
    size: u64,
}

#[tauri::command]
pub async fn hex_rows(path: String, start: u64, count: u64) -> Result<HexRows, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut file = File::open(&path).map_err(|e| e.to_string())?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        let offset = start.saturating_mul(ROW).min(size);
        let len = count.min(ROWS_MAX).saturating_mul(ROW).min(size - offset);
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(len as usize);
        file.take(len)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        Ok(HexRows {
            offset,
            bytes,
            size,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct Summary {
    facts: Vec<(String, String)>,
}

#[tauri::command]
pub async fn file_summary(path: String) -> Result<Summary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut file = File::open(&path).map_err(|e| e.to_string())?;
        let mut probe = Vec::with_capacity(limits::PROBE);
        (&mut file)
            .take(limits::PROBE as u64)
            .read_to_end(&mut probe)
            .map_err(|e| e.to_string())?;
        Ok(Summary {
            facts: isolinear_formats::binary::facts(&probe),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Bytes scanned for strings, from the start of the file.
const STRINGS_SCAN: u64 = 64 * 1024 * 1024;
/// Characters sent to the window at most; the strings after that are left out.
const STRINGS_CHARS: usize = 4_000_000;

#[derive(Serialize)]
pub struct Strings {
    /// Offset and text of each run, in file order.
    strings: Vec<(u64, String)>,
    /// Bytes scanned from the start of the file.
    scanned: u64,
    /// Whether the list stops early, at the string count or the character budget.
    cut: bool,
}

/// Readable ASCII, UTF-8 and UTF-16 runs of at least four characters in the first bytes of a file.
#[tauri::command]
pub async fn file_strings(path: String) -> Result<Strings, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        let mut strings = isolinear_formats::binary::strings(Path::new(&path), STRINGS_SCAN)?;
        let mut cut = strings.len() >= isolinear_formats::binary::MAX_STRINGS;
        let mut chars = 0;
        if let Some(at) = strings.iter().position(|(_, s)| {
            chars += s.chars().count();
            chars > STRINGS_CHARS
        }) {
            strings.truncate(at);
            cut = true;
        }
        Ok(Strings {
            strings,
            scanned: size.min(STRINGS_SCAN),
            cut,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Largest image shown, in bytes.
const IMAGE_MAX: u64 = 64 * 1024 * 1024;

/// An image file as a `data:` URI the page can show; nothing is decoded here.
#[tauri::command]
pub async fn image_data(path: String, format: String) -> Result<String, String> {
    let mime = match format.as_str() {
        "png" => "image/png",
        "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        // Decoded here: the web view cannot show TIFF.
        "tiff" => "image/png",
        _ => return Err("this format is not shown as an image".into()),
    };
    tauri::async_runtime::spawn_blocking(move || {
        let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        if size > IMAGE_MAX {
            return Err(format!(
                "images over {} MiB are not shown",
                IMAGE_MAX / 1024 / 1024
            ));
        }
        let mut bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        if format == "tiff" {
            bytes = isolinear_formats::tiff::to_png(&bytes)?;
        }
        Ok(format!(
            "data:{mime};base64,{}",
            isolinear_formats::base64(&bytes)
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Notebooks read whole up to this size.
const NOTEBOOK_MAX: u64 = 256 * 1024 * 1024;

/// A Jupyter notebook as cells and saved outputs; nothing is executed.
#[tauri::command]
pub async fn notebook(path: String) -> Result<isolinear_formats::notebook::Notebook, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        if size > NOTEBOOK_MAX {
            return Err("notebooks over 256 MiB are shown as text only".into());
        }
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        isolinear_formats::notebook::parse(&isolinear_core::encoding::decode(&bytes).text)
    })
    .await
    .map_err(|e| e.to_string())?
}
