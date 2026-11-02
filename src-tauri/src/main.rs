// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The desktop shell: one document per process and window, commands that run the core
//! pipeline on worker threads, and nothing else. Parsing and I/O never happen on the UI thread.

mod bytes;
mod documents;
mod formatted;
mod launch;
mod overview;
mod paged;
mod preview;
mod previewer;
mod structure;
mod table;
mod watch;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use isolinear_core::document;
use isolinear_core::encoding::{self, Encoding, LineEnding};
use isolinear_core::limits::Tier;
use isolinear_core::save::{self, EncodeError, Stamp};
use isolinear_formats::registry::Kind;
use isolinear_formats::{View, detect};
use isolinear_formats::{decoded, pretty};
use serde::Serialize;
use tauri::Manager;

use paged::Paged;
use structure::Structure;
use watch::Watcher;

/// Paths waiting for the window: from the command line, or sent by the system (macOS Finder).
#[derive(Default)]
struct Pending(Mutex<Vec<PathBuf>>);

#[derive(Serialize)]
struct FormatInfo {
    id: &'static str,
    name: &'static str,
    kind: Kind,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentInfo {
    path: String,
    name: String,
    size: u64,
    tier: Tier,
    stamp: Stamp,
    format: FormatInfo,
    language: Option<&'static str>,
    reason: String,
    views: Vec<View>,
    suggested: View,
    editable: bool,
    /// Decoded text of a file small enough to edit, LF-separated unless the file mixes line
    /// endings; `None` for bytes and for text that is paged.
    text: Option<String>,
    encoding: Option<Encoding>,
    bom: bool,
    line_ending: Option<LineEnding>,
    lossy: bool,
    /// Index id when the text is too large to load and is read a page at a time.
    paged: Option<u64>,
    /// Why the file has no view, when it has none.
    note: Option<String>,
    /// The industry standard the file is in, when one is recognized (its Overview comes first).
    standard: Option<&'static str>,
    /// Revision of the structure tree being parsed for this text, when the format has one.
    structure: Option<u64>,
    /// Revision of the table being indexed, for delimited formats.
    table: Option<u64>,
    /// A copy taken out of an archive by this window: read-only, and it has no folder of its own.
    extracted: bool,
    /// The text can be shown formatted, and should be by default.
    formattable: bool,
    hard_to_read: bool,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum SaveError {
    /// The file changed on disk since the window read it; nothing was written.
    Conflict {
        stamp: Option<Stamp>,
    },
    Unencodable {
        message: String,
    },
    Io {
        message: String,
    },
}

/// Paths the window should open, each returned once.
#[tauri::command]
fn take_paths(pending: tauri::State<'_, Pending>) -> Vec<String> {
    let mut paths = pending.0.lock().unwrap();
    paths.drain(..).map(|p| p.display().to_string()).collect()
}

/// Opens each file in a window of its own (a new process).
#[tauri::command]
fn open_in_new_windows(paths: Vec<String>) -> Result<(), String> {
    for p in paths {
        launch::spawn_window(Path::new(&p)).map_err(|e| format!("{p}: {e}"))?;
    }
    Ok(())
}

/// Reads, detects and decodes on a worker thread (async commands leave the main thread).
#[tauri::command]
async fn open_document(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    path: String,
) -> Result<DocumentInfo, String> {
    let (mut info, page) = tauri::async_runtime::spawn_blocking(move || read(Path::new(&path)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let paged = app.state::<Paged>();
    match page {
        Some((encoding, bom)) => {
            let id = paged
                .open(app.clone(), Path::new(&info.path), info.size, encoding, bom)
                .map_err(|e| e.to_string())?;
            info.paged = Some(id);
        }
        None => paged.close(),
    }
    let structure = app.state::<Structure>();
    match (
        isolinear_formats::tree::for_format(info.format.id, info.language),
        &info.text,
    ) {
        (Some(parser), Some(text)) => {
            info.structure = Some(structure.start(app.clone(), parser, text.clone()));
        }
        _ => structure.clear(),
    }
    let tables = app.state::<table::Tables>();
    match (&info.text, table::kind(info.format.id)) {
        (Some(text), Some(kind)) => {
            let source = table::Source::Text(text.clone());
            info.table = Some(tables.start(app.clone(), source, kind, None, None, None));
        }
        (None, Some(kind @ table::Kind::Binary(_))) => {
            let source = table::Source::File(PathBuf::from(&info.path));
            info.table = Some(tables.start(app.clone(), source, kind, None, None, None));
        }
        (None, Some(kind)) if info.size <= isolinear_core::limits::VIEW_MAX => {
            let source = table::Source::File(PathBuf::from(&info.path));
            info.table = Some(tables.start(app.clone(), source, kind, None, None, None));
        }
        _ => tables.clear(),
    }
    if documents::is_extracted(&app.state::<documents::Scratch>(), Path::new(&info.path)) {
        info.extracted = true;
        info.editable = false;
    }
    let _ = window.set_title(&format!("{} — Isolinear", info.name));
    app.state::<Watcher>()
        .watch(app.clone(), PathBuf::from(&info.path), Some(info.stamp));
    Ok(info)
}

/// Writes the editor text back in the file's encoding. Refuses when the file changed since
/// `expected` unless `force`; a missing file (deleted meanwhile) is recreated.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn save_document(
    app: tauri::AppHandle,
    path: String,
    text: String,
    encoding: Encoding,
    bom: bool,
    line_ending: LineEnding,
    expected: Option<Stamp>,
    force: bool,
) -> Result<Stamp, SaveError> {
    let target = PathBuf::from(&path);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let on_disk = match save::stamp(Path::new(&path)) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(SaveError::Io {
                    message: e.to_string(),
                });
            }
        };
        if !force && on_disk.is_some() && on_disk != expected {
            return Err(SaveError::Conflict { stamp: on_disk });
        }
        let bytes = save::encode(&text, encoding, bom, line_ending).map_err(|e| match e {
            EncodeError::Unencodable { .. } => SaveError::Unencodable {
                message: e.to_string(),
            },
        })?;
        save::write_atomic(Path::new(&path), &bytes).map_err(|e| SaveError::Io {
            message: e.to_string(),
        })
    })
    .await
    .map_err(|e| SaveError::Io {
        message: e.to_string(),
    })?;
    if let Ok(stamp) = &result {
        app.state::<Watcher>()
            .watch(app.clone(), target, Some(*stamp));
    }
    result
}

/// The document, and the encoding to page it with when its text is too large to load.
fn read(path: &Path) -> std::io::Result<(DocumentInfo, Option<(Encoding, bool)>)> {
    let opened = document::open(path)?;
    let stamp = save::stamp(&opened.path)?;
    let mut d = detect(&opened.name, &opened.probe);
    let standard = overview::recognize(&opened.path, &opened.probe, opened.size, d.format.id);
    if standard.is_some() {
        // The Overview comes first, except after a Preview or the Cells: a PDF, a picture or a
        // notebook is first seen as it is.
        d.views.retain(|v| *v != View::Overview);
        let at = usize::from(matches!(d.views.first(), Some(View::Preview | View::Cells)));
        d.views.insert(at, View::Overview);
        d.suggested = d.views[0];
    }
    let mut page = None;
    let mut note = None;
    let (text, encoding, bom, line_ending, lossy) = match (&opened.bytes, d.format.kind) {
        (Some(bytes), Kind::Text) => {
            let dec = encoding::decode(bytes);
            let text = save::normalize(&dec.text, dec.line_ending).into_owned();
            (
                Some(text),
                Some(dec.encoding),
                dec.bom,
                Some(dec.line_ending),
                dec.lossy,
            )
        }
        (None, Kind::Text) => {
            let dec = encoding::decode(&opened.probe);
            page = Some((dec.encoding, dec.bom));
            (
                None,
                Some(dec.encoding),
                dec.bom,
                Some(dec.line_ending),
                false,
            )
        }
        // MessagePack and Protobuf: the JSON they decode to, read-only.
        (Some(bytes), Kind::Binary) if decoded::supports(d.format.id) => {
            match decoded::to_json(d.format.id, bytes, &|| false) {
                Some(Ok((json, how))) => {
                    note = Some(how);
                    (
                        Some(json),
                        Some(Encoding::Utf8),
                        false,
                        Some(LineEnding::Lf),
                        false,
                    )
                }
                Some(Err(e)) => {
                    note = Some(format!("Cannot decode this file: {e}"));
                    d.views.retain(|v| !matches!(v, View::Tree | View::Source));
                    d.suggested = d.views[0];
                    (None, None, false, None, false)
                }
                None => (None, None, false, None, false),
            }
        }
        (None, Kind::Binary) if decoded::supports(d.format.id) => {
            note = Some("This file is too large to decode; its bytes are shown.".to_owned());
            d.views.retain(|v| !matches!(v, View::Tree | View::Source));
            d.suggested = d.views[0];
            (None, None, false, None, false)
        }
        (_, Kind::Binary) => (None, None, false, None, false),
    };
    // Text with bytes that could not be decoded is shown, never saved: saving would write
    // replacement characters in their place.
    let editable = d.editable && text.is_some() && !lossy;
    let formattable = text.is_some() && pretty::supports(d.format.id);
    let hard_to_read = formattable && text.as_deref().is_some_and(pretty::hard_to_read);
    let info = DocumentInfo {
        path: opened.path.display().to_string(),
        name: opened.name,
        size: opened.size,
        tier: opened.tier,
        stamp,
        format: FormatInfo {
            id: d.format.id,
            name: d.format.name,
            kind: d.format.kind,
        },
        language: d.language,
        reason: d.reason,
        views: d.views,
        suggested: d.suggested,
        editable,
        text,
        encoding,
        bom,
        line_ending,
        lossy,
        paged: None,
        note,
        standard,
        structure: None,
        table: None,
        extracted: false,
        formattable,
        hard_to_read,
    };
    Ok((info, page))
}

fn main() {
    // WebKit's media stack would start a helper program to scan its plugins on a first run;
    // scanning in this process keeps the promise that no other program is started.
    #[cfg(target_os = "linux")]
    if std::env::var_os("GST_REGISTRY_FORK").is_none() {
        // SAFETY: nothing else runs yet: no thread reads the environment at this point.
        unsafe { std::env::set_var("GST_REGISTRY_FORK", "no") };
    }
    let (first, previewer) = match launch::from_command_line() {
        launch::Start::Exit(code) => std::process::exit(code),
        launch::Start::Window(first) => (first, false),
        #[cfg(target_os = "linux")]
        launch::Start::Previewer => (None, true),
    };
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(Pending(Mutex::new(first.into_iter().collect())))
        .manage(Watcher::default())
        .manage(Paged::default())
        .manage(Structure::default())
        .manage(formatted::Formatted::default())
        .manage(table::Tables::default())
        .manage(preview::Images::default())
        .manage(documents::Scratch::default())
        .manage(previewer::Preview::new(previewer))
        .setup(move |app| {
            #[cfg(target_os = "linux")]
            if previewer {
                previewer::start(app.handle());
            }
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            take_paths,
            open_in_new_windows,
            open_document,
            save_document,
            paged::paged_lines,
            paged::paged_find,
            structure::structure_update,
            structure::tree_children,
            structure::tree_path,
            structure::tree_diagnostics,
            formatted::format_text,
            formatted::formatted_from_source,
            formatted::formatted_to_source,
            table::table_update,
            table::table_rows,
            table::table_arrange,
            table::table_switch,
            table::table_cell_span,
            table::table_cell_at,
            preview::render_markdown,
            bytes::hex_rows,
            bytes::file_summary,
            bytes::file_strings,
            bytes::image_data,
            overview::inspect_document,
            bytes::notebook,
            documents::pdf_info,
            documents::pdf_page,
            documents::document_html,
            documents::archive_extract,
            previewer::preview_mode,
            previewer::preview_hide,
            previewer::preview_select,
        ])
        .build(tauri::generate_context!())
        .expect("error while starting Isolinear");
    app.run(|app, event| {
        if let tauri::RunEvent::Exit = event {
            app.state::<documents::Scratch>().remove();
        }
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Opened { urls } = event {
            let paths: Vec<PathBuf> = urls.iter().filter_map(|u| u.to_file_path().ok()).collect();
            app.state::<Pending>().0.lock().unwrap().extend(paths);
            let _ = tauri::Emitter::emit(app, "open-paths", ());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_reads_without_writing_and_undecodable_text_is_not_editable() {
        let dir = std::env::temp_dir().join(format!("isolinear-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("notes.md");
        std::fs::write(&good, "# Title\r\ncaf\u{e9}\r\n").unwrap();
        // UTF-16 with an unpaired surrogate: the text cannot be written back as it was.
        let broken = dir.join("broken.txt");
        std::fs::write(&broken, [0xFF, 0xFE, b'a', 0, 0x00, 0xD8, b'b', 0]).unwrap();
        for (path, editable) in [(&good, true), (&broken, false)] {
            let before = std::fs::read(path).unwrap();
            let stamp = save::stamp(path).unwrap();
            let (info, _) = read(path).unwrap();
            assert_eq!(info.editable, editable, "{}", path.display());
            assert_eq!(std::fs::read(path).unwrap(), before);
            assert!(save::stamp(path).unwrap() == stamp);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
