//! The quick preview of GNOME Files: Space on a file asks the `org.gnome.NautilusPreviewer`
//! D-Bus service to show it. `isolinear --previewer` is that service: one resident process
//! whose window is shown, given another file, and hidden again, so a preview opens at once.
//!
//! Nothing is registered by installing the app: `--enable-file-manager-preview` writes the
//! user's D-Bus service file (it takes precedence over a system one, such as Sushi's) and
//! `--disable-file-manager-preview` removes it.

// Only the hiding and the mode are used outside Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};

const BUS_NAME: &str = "org.gnome.NautilusPreviewer";
const OBJECT_PATH: &str = "/org/gnome/NautilusPreviewer";
/// A hidden previewer exits after this long; D-Bus starts it again on the next Space.
const IDLE_EXIT: Duration = Duration::from_secs(10 * 60);

/// Whether this process is the previewer, and what it shows.
#[derive(Default)]
pub struct Preview {
    pub on: bool,
    visible: AtomicBool,
    current: Mutex<Option<PathBuf>>,
    hidden_since: Mutex<Option<Instant>>,
}

impl Preview {
    pub fn new(on: bool) -> Self {
        Self {
            on,
            ..Self::default()
        }
    }
}

fn show(app: &AppHandle, path: PathBuf, close_if_shown: bool) {
    let preview = app.state::<Preview>();
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let same = preview.current.lock().unwrap().as_ref() == Some(&path);
    if close_if_shown && same && preview.visible.load(Ordering::SeqCst) {
        hide(app);
        return;
    }
    if !same {
        *preview.current.lock().unwrap() = Some(path.clone());
        app.state::<crate::Pending>().0.lock().unwrap().push(path);
        let _ = app.emit("open-paths", ());
    }
    preview.visible.store(true, Ordering::SeqCst);
    *preview.hidden_since.lock().unwrap() = None;
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

fn hide(app: &AppHandle) {
    let preview = app.state::<Preview>();
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    preview.visible.store(false, Ordering::SeqCst);
    *preview.hidden_since.lock().unwrap() = Some(Instant::now());
}

/// Hides the preview window (Escape, or its close button); the process stays for the next file.
#[tauri::command]
pub fn preview_hide(app: AppHandle) {
    hide(&app);
}

/// Whether this window is the file manager's quick preview.
#[tauri::command]
pub fn preview_mode(preview: tauri::State<'_, Preview>) -> bool {
    preview.on
}

#[cfg(target_os = "linux")]
struct Service {
    app: AppHandle,
}

#[cfg(target_os = "linux")]
#[zbus::interface(name = "org.gnome.NautilusPreviewer2")]
impl Service {
    /// Shows the file at `uri`; when `close_if_shown` and it is already shown, hides it.
    fn show_file(
        &self,
        uri: &str,
        _window_handle: &str,
        close_if_shown: bool,
    ) -> zbus::fdo::Result<()> {
        let path = tauri::Url::parse(uri)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("not a local file: {uri}")))?;
        let app = self.app.clone();
        let _ = self
            .app
            .run_on_main_thread(move || show(&app, path, close_if_shown));
        Ok(())
    }

    fn close(&self) {
        let app = self.app.clone();
        let _ = self.app.run_on_main_thread(move || hide(&app));
    }

    #[zbus(property)]
    fn visible(&self) -> bool {
        self.app.state::<Preview>().visible.load(Ordering::SeqCst)
    }

    /// Asks the file manager to select the next file in a direction (GTK's: 2 up, 3 down,
    /// 4 left, 5 right); it then shows that file here.
    #[zbus(signal)]
    async fn selection_event(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        direction: u32,
    ) -> zbus::Result<()>;
}

/// An arrow key no view used: the file manager moves its selection, and shows the next file.
#[tauri::command]
pub async fn preview_select(app: AppHandle, direction: String) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let code: u32 = match direction.as_str() {
            "up" => 2,
            "down" => 3,
            "left" => 4,
            "right" => 5,
            _ => return Err(format!("no direction {direction}")),
        };
        let Some(bus) = app.try_state::<Bus>() else {
            return Ok(());
        };
        let iface = bus
            .0
            .object_server()
            .interface::<_, Service>(OBJECT_PATH)
            .map_err(|e| e.to_string())?;
        Service::selection_event(iface.signal_emitter(), code)
            .await
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (app, direction);
    Ok(())
}

#[cfg(target_os = "linux")]
/// Owns the bus name and serves the previewer interface; the window starts hidden. Exits when
/// another previewer owns the name.
pub fn start(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    *app.state::<Preview>().hidden_since.lock().unwrap() = Some(Instant::now());
    let connection = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.name(BUS_NAME))
        .and_then(|b| b.serve_at(OBJECT_PATH, Service { app: app.clone() }))
        .and_then(|b| b.build());
    match connection {
        Ok(c) => {
            app.manage(Bus(c));
        }
        Err(e) => {
            eprintln!("isolinear: cannot serve the file manager preview: {e}");
            app.exit(1);
            return;
        }
    }
    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(30));
            let idle = app
                .state::<Preview>()
                .hidden_since
                .lock()
                .unwrap()
                .is_some_and(|t| t.elapsed() > IDLE_EXIT);
            if idle {
                app.exit(0);
                return;
            }
        }
    });
}

#[cfg(target_os = "linux")]
/// Keeps the D-Bus connection (and so the name) for the life of the process.
struct Bus(zbus::blocking::Connection);

fn service_file() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(
        data.join("dbus-1/services")
            .join(format!("{BUS_NAME}.service")),
    )
}

/// `--enable-file-manager-preview` / `--disable-file-manager-preview`: registers or removes
/// this program as the user's file manager preview. Returns the exit code.
pub fn configure(enable: bool) -> i32 {
    let Some(file) = service_file() else {
        eprintln!("isolinear: no home folder to register the preview in");
        return 1;
    };
    let result = if enable {
        std::env::current_exe().and_then(|exe| {
            std::fs::create_dir_all(file.parent().unwrap_or(&file))?;
            std::fs::write(
                &file,
                format!(
                    "[D-BUS Service]\nName={BUS_NAME}\nExec={} --previewer\n",
                    exe.display()
                ),
            )
        })
    } else {
        match std::fs::read_to_string(&file) {
            Ok(text) if text.contains("isolinear") => std::fs::remove_file(&file),
            Ok(_) => {
                eprintln!(
                    "isolinear: {} belongs to another previewer; left as it is",
                    file.display()
                );
                return 1;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    };
    match result {
        Ok(()) if enable => {
            println!(
                "Space in GNOME Files now previews with Isolinear ({}).",
                file.display()
            );
            0
        }
        Ok(()) => {
            println!("Isolinear no longer previews for GNOME Files.");
            0
        }
        Err(e) => {
            eprintln!("isolinear: {}: {e}", file.display());
            1
        }
    }
}
