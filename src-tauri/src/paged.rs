//! The open large text file: its line index, built on a worker, and page and search requests.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use isolinear_core::encoding::Encoding;
use isolinear_core::paged::{self, Found, Line, LineIndex, Params, Progress};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

const PROGRESS_EVERY: Duration = Duration::from_millis(150);
/// Most lines returned by one page request.
const PAGE_MAX: usize = 4096;

#[derive(Default)]
pub struct Paged {
    current: Mutex<Option<Arc<Doc>>>,
    next_id: AtomicU64,
}

struct Doc {
    id: u64,
    index: LineIndex,
    /// Each search takes a new number; a running search stops when it is no longer the latest.
    search: AtomicU64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct IndexEvent {
    id: u64,
    #[serde(flatten)]
    progress: Progress,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinesReply {
    page: Vec<Line>,
    #[serde(flatten)]
    progress: Progress,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindReply {
    found: Option<Found>,
    cancelled: bool,
}

impl Paged {
    /// Starts indexing `path` and makes it the window's document; the previous one stops.
    pub fn open(
        &self,
        app: AppHandle,
        path: &Path,
        size: u64,
        encoding: Encoding,
        bom: bool,
    ) -> std::io::Result<u64> {
        let index = LineIndex::new(path, size, encoding, bom, Params::default())?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let doc = Arc::new(Doc {
            id,
            index,
            search: AtomicU64::new(0),
        });
        if let Some(old) = self.current.lock().unwrap().replace(doc.clone()) {
            old.index.cancel();
        }
        std::thread::Builder::new()
            .name("line-index".into())
            .spawn(move || {
                let mut last = Instant::now() - PROGRESS_EVERY;
                let emit = |progress: Progress, error: Option<String>| {
                    let _ = app.emit(
                        "index-progress",
                        IndexEvent {
                            id: doc.id,
                            progress,
                            error,
                        },
                    );
                };
                let result = doc.index.build(|p| {
                    if p.complete || last.elapsed() >= PROGRESS_EVERY {
                        last = Instant::now();
                        emit(p, None);
                    }
                });
                if let Err(e) = result {
                    emit(doc.index.progress(), Some(e.to_string()));
                }
            })?;
        Ok(id)
    }

    /// Forgets the current document (another kind of file was opened).
    pub fn close(&self) {
        if let Some(old) = self.current.lock().unwrap().take() {
            old.index.cancel();
        }
    }

    fn get(&self, id: u64) -> Result<Arc<Doc>, String> {
        match &*self.current.lock().unwrap() {
            Some(doc) if doc.id == id => Ok(doc.clone()),
            _ => Err("this file is no longer open".into()),
        }
    }
}

#[tauri::command]
pub async fn paged_lines(
    state: tauri::State<'_, Paged>,
    id: u64,
    start: u64,
    count: usize,
) -> Result<LinesReply, String> {
    let doc = state.get(id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let page = doc
            .index
            .lines(start, count.min(PAGE_MAX))
            .map_err(|e| e.to_string())?;
        Ok(LinesReply {
            page,
            progress: doc.index.progress(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Searches from the start of `from_line`, or from `from_offset` (one past a previous match).
#[tauri::command]
pub async fn paged_find(
    state: tauri::State<'_, Paged>,
    id: u64,
    query: String,
    from_line: Option<u64>,
    from_offset: Option<u64>,
    match_case: bool,
) -> Result<FindReply, String> {
    let doc = state.get(id)?;
    let ticket = doc.search.fetch_add(1, Ordering::Relaxed) + 1;
    tauri::async_runtime::spawn_blocking(move || {
        let from = match (from_offset, from_line) {
            (Some(o), _) => paged::From::Offset(o),
            (None, l) => paged::From::Line(l.unwrap_or(0)),
        };
        let stale = || doc.search.load(Ordering::Relaxed) != ticket;
        let found = doc
            .index
            .find(&query, from, match_case, &stale)
            .map_err(|e| e.to_string())?;
        Ok(FindReply {
            found: if stale() { None } else { found },
            cancelled: stale(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}
