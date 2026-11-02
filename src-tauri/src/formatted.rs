//! Formatted text of the open document (display only) and the map between its offsets and the
//! source's. Offsets crossing to the UI are UTF-16 code units.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use isolinear_formats::pretty::{self, Map};
use isolinear_formats::tree::Offsets;
use serde::Serialize;

#[derive(Default)]
pub struct Formatted {
    current: Mutex<Option<Arc<Shown>>>,
    next_id: AtomicU64,
}

struct Shown {
    id: u64,
    source: String,
    source_offsets: Offsets,
    text: String,
    text_offsets: Offsets,
    map: Map,
}

#[derive(Serialize)]
pub struct FormatReply {
    id: u64,
    text: String,
}

impl Formatted {
    fn get(&self, id: u64) -> Result<Arc<Shown>, String> {
        match &*self.current.lock().unwrap() {
            Some(s) if s.id == id => Ok(s.clone()),
            _ => Err("stale".into()),
        }
    }
}

/// Formats `text` and keeps its map; the reply's id names it in the other calls.
#[tauri::command]
pub async fn format_text(
    state: tauri::State<'_, Formatted>,
    format: String,
    text: String,
) -> Result<FormatReply, String> {
    let id = state.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    let shown = tauri::async_runtime::spawn_blocking(move || {
        let out =
            pretty::format(&format, &text, &|| false).ok_or("this text cannot be formatted")?;
        Ok::<_, String>(Shown {
            id,
            source_offsets: Offsets::new(&text),
            text_offsets: Offsets::new(&out.text),
            source: text,
            text: out.text,
            map: out.map,
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    let reply = FormatReply {
        id,
        text: shown.text.clone(),
    };
    *state.current.lock().unwrap() = Some(Arc::new(shown));
    Ok(reply)
}

/// Source offsets to offsets in the formatted text.
#[tauri::command]
pub fn formatted_from_source(
    state: tauri::State<'_, Formatted>,
    id: u64,
    offsets: Vec<u32>,
) -> Result<Vec<u32>, String> {
    let s = state.get(id)?;
    Ok(offsets
        .into_iter()
        .map(|o| {
            let byte = s.source_offsets.to_byte(&s.source, o) as usize;
            let at = s.map.to_formatted(&s.text, byte);
            s.text_offsets.to_utf16(&s.text, at as u32)
        })
        .collect())
}

/// An offset in the formatted text to the source offset.
#[tauri::command]
pub fn formatted_to_source(
    state: tauri::State<'_, Formatted>,
    id: u64,
    offset: u32,
) -> Result<u32, String> {
    let s = state.get(id)?;
    let byte = s.text_offsets.to_byte(&s.text, offset) as usize;
    let at = s.map.to_source(byte).min(s.source.len());
    Ok(s.source_offsets.to_utf16(&s.source, at as u32))
}
