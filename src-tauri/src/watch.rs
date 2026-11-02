//! Notices when the open file changes on disk, without a file-system notification dependency.
//!
//! A worker stats the file twice a second. A change is reported only once the size and time
//! have held still for one more check, so a file still being written is not read half-way.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use isolinear_core::save::{self, Stamp};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

const INTERVAL: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct Watcher {
    stop: Mutex<Option<Arc<AtomicBool>>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DiskChange {
    pub path: String,
    /// `None` when the file is gone.
    pub stamp: Option<Stamp>,
}

impl Watcher {
    /// Watches `path`, replacing any earlier watch. `known` is the state the window has.
    pub fn watch(&self, app: AppHandle, path: PathBuf, known: Option<Stamp>) {
        let stop = Arc::new(AtomicBool::new(false));
        if let Some(previous) = self.stop.lock().unwrap().replace(stop.clone()) {
            previous.store(true, Ordering::Relaxed);
        }
        std::thread::Builder::new()
            .name("disk-watch".into())
            .spawn(move || {
                let mut previous = save::stamp(&path).ok();
                let mut reported = known;
                loop {
                    std::thread::sleep(INTERVAL);
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let current = save::stamp(&path).ok();
                    if current == previous && current != reported {
                        reported = current;
                        let _ = app.emit(
                            "disk-changed",
                            DiskChange {
                                path: path.display().to_string(),
                                stamp: current,
                            },
                        );
                    }
                    previous = current;
                }
            })
            .expect("spawn disk watcher");
    }
}
