//! Command line: `isolinear [open] [--wait] [FILE…]`, and on Linux `--previewer` (the GNOME Files quick
//! preview service, see `previewer`) with `--enable-file-manager-preview` to register it.
//!
//! Every file gets a window of its own, in a process of its own. In a release build the
//! command returns at once (like `code FILE`): each window is started detached. `--wait`
//! keeps the first window in the calling process, for desktop launchers and scripts. Debug
//! builds always wait, so `npm run tauri dev` keeps its window. Without files, the welcome
//! window opens, detached too. `open` is accepted before the files, as in `isolinear open a.md`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub enum Start {
    /// Run a window for this file, or the welcome window.
    Window(Option<PathBuf>),
    /// Run as the file manager's quick preview (a D-Bus service, Linux).
    #[cfg(target_os = "linux")]
    Previewer,
    Exit(i32),
}

pub fn from_command_line() -> Start {
    let mut wait = cfg!(debug_assertions);
    let mut files = Vec::new();
    let mut missing = false;
    for (i, arg) in std::env::args_os().skip(1).enumerate() {
        if i == 0 && arg == "open" && !Path::new("open").is_file() {
            continue;
        }
        if arg == "--wait" {
            wait = true;
            continue;
        }
        if arg == "--help" || arg == "-h" {
            println!("usage: isolinear [open] [--wait] [FILE...]");
            println!("  Opens each file in a window of its own; without files, the start window.");
            println!("  --wait  keep the (first) window in this process instead of detaching");
            #[cfg(any(target_os = "linux", windows))]
            println!(
                "       isolinear --enable-file-manager-preview | --disable-file-manager-preview"
            );
            return Start::Exit(0);
        }
        #[cfg(target_os = "linux")]
        match arg.to_str() {
            Some("--previewer") => return Start::Previewer,
            Some("--enable-file-manager-preview") => {
                return Start::Exit(crate::previewer::configure(true));
            }
            Some("--disable-file-manager-preview") => {
                return Start::Exit(crate::previewer::configure(false));
            }
            _ => {}
        }
        #[cfg(windows)]
        match arg.to_str() {
            Some("--enable-file-manager-preview") => return Start::Exit(windows_preview(true)),
            Some("--disable-file-manager-preview") => return Start::Exit(windows_preview(false)),
            _ => {}
        }
        let path = PathBuf::from(&arg);
        match std::fs::metadata(&path) {
            Ok(m) if m.is_file() => files.push(std::fs::canonicalize(&path).unwrap_or(path)),
            Ok(_) => {
                let _ = writeln!(io::stderr(), "isolinear: {}: not a file", path.display());
                missing = true;
            }
            Err(e) => {
                let _ = writeln!(io::stderr(), "isolinear: {}: {e}", path.display());
                missing = true;
            }
        }
    }
    if files.is_empty() {
        if missing {
            return Start::Exit(1);
        }
        if wait {
            return Start::Window(None);
        }
        return match spawn(None) {
            Ok(()) => Start::Exit(0),
            Err(e) => {
                let _ = writeln!(io::stderr(), "isolinear: {e}");
                Start::Exit(1)
            }
        };
    }
    let here = if wait { Some(files.remove(0)) } else { None };
    for file in &files {
        if let Err(e) = spawn_window(file) {
            let _ = writeln!(io::stderr(), "isolinear: {}: {e}", file.display());
            missing = true;
        }
    }
    match here {
        Some(file) => Start::Window(Some(file)),
        None => Start::Exit(i32::from(missing)),
    }
}

/// Starts `isolinear --wait FILE` detached from this process and its terminal.
pub fn spawn_window(file: &Path) -> io::Result<()> {
    spawn(Some(file))
}

/// Starts `isolinear --wait [FILE]` detached from this process and its terminal.
fn spawn(file: Option<&Path>) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--wait");
    if let Some(file) = file {
        cmd.arg(file);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn().map(drop)
}

/// Registers or removes the Explorer preview pane handler, the DLL installed beside the program.
#[cfg(windows)]
fn windows_preview(enable: bool) -> i32 {
    let result = if enable {
        std::env::current_exe()
            .map_err(|e| e.to_string())
            .and_then(|exe| {
                let dir = exe.parent().unwrap_or(Path::new("."));
                let dll = [dir.to_path_buf(), dir.join("resources")]
                    .iter()
                    .map(|d| d.join("isolinear_preview_handler.dll"))
                    .find(|p| p.is_file())
                    .ok_or_else(|| {
                        format!("isolinear_preview_handler.dll is not in {}", dir.display())
                    })?;
                isolinear_preview_handler::register(&dll)
            })
    } else {
        isolinear_preview_handler::unregister()
    };
    match result {
        Ok(()) if enable => {
            println!("Explorer's preview pane (Alt+P) now shows these files with Isolinear.");
            0
        }
        Ok(()) => {
            println!("Isolinear no longer previews for Explorer.");
            0
        }
        Err(e) => {
            eprintln!("isolinear: {e}");
            1
        }
    }
}
