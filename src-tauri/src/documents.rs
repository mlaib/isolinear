//! Documents shown by their readers: PDF pages as images, Word and OpenDocument text as HTML, and
//! files inside archives, copied to a private folder of this window to be opened like any file.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use isolinear_formats::{document, pdf, table::archive};
use serde::Serialize;

/// This window's private folder for files taken out of archives; removed when the window closes.
#[derive(Default)]
pub struct Scratch {
    dir: Mutex<Option<PathBuf>>,
    next: AtomicU64,
}

impl Scratch {
    /// A new, empty, owner-only folder inside this window's folder.
    fn fresh(&self) -> std::io::Result<PathBuf> {
        let mut dir = self.dir.lock().unwrap();
        if dir.is_none() {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let d = std::env::temp_dir().join(format!("isolinear-{}-{nanos}", std::process::id()));
            create_private(&d)?;
            *dir = Some(d);
        }
        let k = self.next.fetch_add(1, Ordering::Relaxed);
        let sub = dir.as_ref().unwrap().join(k.to_string());
        create_private(&sub)?;
        Ok(sub)
    }

    /// Removes the folder and everything copied into it.
    pub fn remove(&self) {
        if let Some(d) = self.dir.lock().unwrap().take() {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// Whether `path` is a copy made by this window (such files are read-only).
    fn owns(&self, path: &Path) -> bool {
        self.dir
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|d| path.starts_with(d))
    }
}

fn create_private(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[derive(Serialize)]
pub struct PdfInfo {
    pages: usize,
}

#[tauri::command]
pub async fn pdf_info(path: String) -> Result<PdfInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        pdf::page_count(Path::new(&path)).map(|pages| PdfInfo { pages })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Page `index` as a PNG `data:` URI.
#[tauri::command]
pub async fn pdf_page(path: String, index: usize, scale: f32) -> Result<String, String> {
    let scale = scale.clamp(0.1, 8.0);
    tauri::async_runtime::spawn_blocking(move || {
        let png = pdf::render_page(Path::new(&path), index, scale)?;
        Ok(format!(
            "data:image/png;base64,{}",
            isolinear_formats::base64(&png)
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A Word or OpenDocument text file as HTML built by the reader (no markup from the file).
#[tauri::command]
pub async fn document_html(path: String, format: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || document::to_html(&format, Path::new(&path)))
        .await
        .map_err(|e| e.to_string())?
}

/// Copies the entry on `row` of an archive's listing into this window's private folder and
/// returns its path, to be opened read-only.
#[tauri::command]
pub async fn archive_extract(
    scratch: tauri::State<'_, Scratch>,
    path: String,
    format: String,
    row: u64,
) -> Result<String, String> {
    let dir = scratch.fresh().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        archive::extract(&format, Path::new(&path), row, &dir).map(|p| p.display().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Whether a path is a copy taken out of an archive by this window.
pub fn is_extracted(scratch: &Scratch, path: &Path) -> bool {
    scratch.owns(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_folders_are_private_fresh_and_removed() {
        let s = Scratch::default();
        let a = s.fresh().unwrap();
        let b = s.fresh().unwrap();
        assert_ne!(a, b);
        assert!(s.owns(&a.join("x")));
        assert!(!s.owns(Path::new("/etc/passwd")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&a).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        let root = a.parent().unwrap().to_owned();
        s.remove();
        assert!(!root.exists());
    }
}
