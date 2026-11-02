//! Markdown preview. Images are read from local files only (next to the document, or an absolute
//! path), when their name says they are an image and they are at most 8 MiB; they reach the page
//! as `data:` URIs. Remote images are never fetched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use isolinear_formats::markdown;
use isolinear_formats::tree::Offsets;
use serde::Serialize;

const IMAGE_MAX: u64 = 8 * 1024 * 1024;
/// Image bytes embedded in one rendering at most.
const RENDER_BUDGET: u64 = 64 * 1024 * 1024;

#[derive(Default)]
pub struct Images(Mutex<HashMap<PathBuf, (SystemTime, Arc<String>)>>);

#[derive(Serialize)]
pub struct PreviewBlock {
    html: String,
    /// UTF-16 range of the block in the text.
    from: u32,
    to: u32,
}

#[tauri::command]
pub async fn render_markdown(
    images: tauri::State<'_, Images>,
    path: String,
    text: String,
) -> Result<Vec<PreviewBlock>, String> {
    let cache: HashMap<_, _> = images.0.lock().unwrap().clone();
    let (blocks, used) = tauri::async_runtime::spawn_blocking(move || {
        let base = Path::new(&path).parent().map(Path::to_owned);
        let budget = std::cell::Cell::new(RENDER_BUDGET);
        let used = std::cell::RefCell::new(HashMap::new());
        let resolve = |dest: &str| -> Option<String> {
            let file = local_path(dest, base.as_deref())?;
            let mime = mime(&file)?;
            let meta = std::fs::metadata(&file).ok()?;
            if !meta.is_file() || meta.len() > IMAGE_MAX || meta.len() > budget.get() {
                return None;
            }
            budget.set(budget.get() - meta.len());
            let modified = meta.modified().ok()?;
            if let Some((when, uri)) = cache.get(&file)
                && *when == modified
            {
                used.borrow_mut().insert(file, (modified, uri.clone()));
                return Some(uri.to_string());
            }
            let bytes = std::fs::read(&file).ok()?;
            let uri = Arc::new(format!(
                "data:{mime};base64,{}",
                isolinear_formats::base64(&bytes)
            ));
            used.borrow_mut().insert(file, (modified, uri.clone()));
            Some(uri.to_string())
        };
        let rendered = markdown::render(&text, &resolve);
        let offsets = Offsets::new(&text);
        let blocks: Vec<PreviewBlock> = rendered
            .blocks
            .into_iter()
            .map(|b| PreviewBlock {
                html: b.html,
                from: offsets.to_utf16(&text, b.span.start as u32),
                to: offsets.to_utf16(&text, b.span.end as u32),
            })
            .collect();
        (blocks, used.into_inner())
    })
    .await
    .map_err(|e| e.to_string())?;
    // Keep only the images this document uses.
    *images.0.lock().unwrap() = used;
    Ok(blocks)
}

/// A local file for an image destination; `None` for anything remote.
fn local_path(dest: &str, base: Option<&Path>) -> Option<PathBuf> {
    let dest = dest.trim();
    let lower = dest.to_ascii_lowercase();
    let path = if let Some(rest) = lower.strip_prefix("file://") {
        PathBuf::from(&dest[dest.len() - rest.len()..])
    } else if lower.contains("://") || lower.starts_with("data:") || lower.starts_with("//") {
        return None;
    } else {
        let decoded = percent_decode(dest);
        let p = PathBuf::from(decoded.split(['#', '?']).next().unwrap_or(""));
        if p.is_absolute() { p } else { base?.join(p) }
    };
    Some(path)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_paths_resolve() {
        let base = Path::new("/docs");
        assert_eq!(
            local_path("img/a.png", Some(base)),
            Some(PathBuf::from("/docs/img/a.png"))
        );
        assert_eq!(
            local_path("my%20pic.png#x", Some(base)),
            Some(PathBuf::from("/docs/my pic.png"))
        );
        assert_eq!(
            local_path("/abs/b.png", None),
            Some(PathBuf::from("/abs/b.png"))
        );
        assert_eq!(
            local_path("file:///abs/c.png", None),
            Some(PathBuf::from("/abs/c.png"))
        );
        for remote in [
            "https://x/y.png",
            "HTTP://x/y.png",
            "//cdn/y.png",
            "data:image/png;base64,AA",
            "ftp://h/f.png",
        ] {
            assert_eq!(local_path(remote, Some(base)), None, "{remote}");
        }
        assert_eq!(
            local_path("rel.png", None),
            None,
            "no base for a relative path"
        );
        assert_eq!(mime(Path::new("a.PNG")), Some("image/png"));
        assert_eq!(mime(Path::new("passwd")), None);
    }
}
