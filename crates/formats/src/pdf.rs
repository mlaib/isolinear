//! PDF pages drawn by the previous release's pure-Rust renderer. Only the requested page is
//! rasterized; actions, JavaScript, links and forms never run. Missing fonts and CMaps come
//! from compiled-in data, never the system or network. The shell calls these functions on workers.

use hayro::hayro_interpret::{InterpreterSettings, util::TransformExt};
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{RenderCache, RenderSettings};
use std::fs::File;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

pub(crate) mod png;
mod preflight;

pub const READY: bool = true;
const MAX_FILE: u64 = 512 * 1024 * 1024;
const MAX_PAGES: usize = 20_000;
const MAX_PIXELS: u64 = 40_000_000;
// The renderer uses u16 dimensions. A smaller side limit also bounds very thin pages.
const MAX_SIDE: u16 = 16_384;
const PROTECTED: &str = "The PDF is protected by encryption; its pages cannot be shown";

/// Pages of the document, or why it cannot be shown (encrypted, damaged or over the limit).
pub fn page_count(path: &Path) -> Result<usize, String> {
    let data = read(path)?;
    guarded(|| open(data).map(|pdf| pdf.pages().len()))
}

/// Page `index` (from 0) as PNG bytes, `scale` times its size in points (1.0 = 72 dpi).
/// Large scales are reduced before allocating the raster, to at most 40 megapixels.
pub fn render_page(path: &Path, index: usize, scale: f32) -> Result<Vec<u8>, String> {
    validate_scale(scale)?;
    let data = read(path)?;
    guarded(|| {
        let pdf = open(data)?;
        render(&pdf, index, scale)
    })
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| format!("The PDF could not be opened: {e}"))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("The PDF's size could not be read: {e}"))?;
    if !metadata.is_file() {
        return Err("Expected a PDF file, not a directory or device".into());
    }
    if metadata.len() > MAX_FILE {
        return Err("PDF pages can be shown for files up to 512 MiB".into());
    }
    let mut data = Vec::new();
    // A bounded reader also handles files that grow after the metadata check.
    file.take(MAX_FILE + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("The PDF could not be read: {e}"))?;
    if data.len() as u64 > MAX_FILE {
        return Err("The PDF grew beyond the 512 MiB file limit while being read".into());
    }
    Ok(data)
}

fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err("The PDF reader failed on damaged document data".into()))
}

fn open(data: Vec<u8>) -> Result<Pdf, String> {
    if data.len() as u64 > MAX_FILE {
        return Err("PDF pages can be shown for files up to 512 MiB".into());
    }
    preflight::check(&data)?;
    let pdf = Pdf::new(data).map_err(|e| match e {
        LoadPdfError::Decryption(_) => PROTECTED.to_owned(),
        LoadPdfError::Invalid => "The PDF's structure could not be read".to_owned(),
    })?;
    if pdf.pages().len() > MAX_PAGES {
        return Err(format!(
            "The PDF has {} pages; at most {MAX_PAGES} pages can be shown",
            pdf.pages().len()
        ));
    }
    if pdf.pages().is_empty() {
        return Err("The PDF contains no readable pages".into());
    }
    Ok(pdf)
}

fn validate_scale(scale: f32) -> Result<(), String> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err("Expected a finite PDF scale greater than zero".into());
    }
    Ok(())
}

/// Compute dimensions with f64 arithmetic, then allocate exactly that canvas. Clamping an
/// intermediate page size or putting a lower bound on scale can exceed the pixel budget.
fn canvas(size: (f32, f32), wanted: f32) -> Result<(u16, u16, f64), String> {
    validate_scale(wanted)?;
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
        return Err("Expected finite, positive PDF page dimensions".into());
    }
    let scale = f64::from(wanted)
        .min(f64::from(MAX_SIDE) / w.max(h))
        .min((MAX_PIXELS as f64 / (w * h)).sqrt());
    let width = (w * scale).floor().clamp(1.0, f64::from(MAX_SIDE)) as u16;
    let mut height = (h * scale).floor().clamp(1.0, f64::from(MAX_SIDE)) as u16;
    // Guard the final integer product too, including floating-point rounding at the boundary.
    height = height.min((MAX_PIXELS / u64::from(width)).min(u64::from(MAX_SIDE)) as u16);
    Ok((width, height, scale))
}

fn render(pdf: &Pdf, index: usize, scale: f32) -> Result<Vec<u8>, String> {
    let page = pdf.pages().get(index).ok_or_else(|| {
        format!(
            "PDF page index {index} is outside this document's {} pages (indices start at zero)",
            pdf.pages().len()
        )
    })?;
    let (width, height, scale) = canvas(page.render_dimensions(), scale)?;
    let mut context = RenderContext::new(width, height);
    let transform = hayro::kurbo::Affine::scale(scale) * page.initial_transform(true).to_kurbo();
    hayro::render_into(
        page,
        &RenderCache::new(),
        // These defaults only resolve compiled-in standard fonts and CMaps. They do not
        // install callbacks that can read files or perform network requests.
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &mut context,
        transform,
    );
    context.flush();
    let mut pixmap = Pixmap::new(width, height);
    context.render_with(
        &mut pixmap,
        &mut Resources::default(),
        RasterizerSettings {
            target_init: TargetInit::Clear(hayro::vello_cpu::color::palette::css::WHITE),
            ..Default::default()
        },
    );
    // Premultiplied over opaque white is already straight RGBA. Encode without copying the
    // full raster, and release the renderer's working buffers before compression.
    drop(context);
    png::encode(width, height, pixmap.data_as_u8_slice())
}

#[cfg(test)]
mod tests;
