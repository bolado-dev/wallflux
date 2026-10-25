//! Tauri backend for the wallflux GUI.
//!
//! Every command here is a thin translation between the frontend and the
//! daemon's control socket — the GUI holds no state of its own, so a wallpaper
//! changed from `wallfluxctl` shows up here on the next refresh, and vice
//! versa. The two exceptions are thumbnail generation and importing files,
//! which are filesystem work the daemon has no reason to do.

mod thumbnails;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use wallflux_core::ipc::{Client, Request, Response, Status};
use wallflux_core::library::Wallpaper;
use wallflux_core::wallpaper_engine::{self, Item as EngineItem};
use std::os::unix::process::CommandExt as _;

use tauri::Manager as _;
use wallflux_core::{Config, paths};

/// Errors cross into JavaScript as strings; the frontend shows them verbatim,
/// so they are written to be read by a person.
type CmdResult<T> = Result<T, String>;

fn stringify(err: impl std::fmt::Display) -> String {
    format!("{err:#}")
}

/// One request, one reply. A fresh connection per call keeps this stateless
/// and costs almost nothing on a Unix socket.
fn ask(request: Request) -> CmdResult<Response> {
    let mut client = Client::connect().map_err(stringify)?;
    client.request(&request).map_err(stringify)
}

#[derive(Serialize)]
pub struct DaemonInfo {
    running: bool,
    version: Option<String>,
    /// Set when the daemon is reachable but speaks a different protocol.
    ipc_mismatch: bool,
}

#[tauri::command]
fn daemon_info() -> DaemonInfo {
    match ask(Request::Ping) {
        Ok(Response::Pong {
            version,
            ipc_version,
        }) => DaemonInfo {
            running: true,
            version: Some(version),
            ipc_mismatch: ipc_version != wallflux_core::IPC_VERSION,
        },
        _ => DaemonInfo {
            running: false,
            version: None,
            ipc_mismatch: false,
        },
    }
}

#[tauri::command]
fn status() -> CmdResult<Status> {
    match ask(Request::Status)? {
        Response::Status(status) => Ok(*status),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

#[derive(Serialize)]
pub struct LibraryView {
    wallpapers: Vec<Wallpaper>,
    errors: Vec<(PathBuf, String)>,
    library_root: PathBuf,
}

#[tauri::command]
fn list_wallpapers(rescan: bool) -> CmdResult<LibraryView> {
    let request = if rescan {
        Request::RescanLibrary
    } else {
        Request::ListWallpapers
    };
    match ask(request)? {
        Response::Wallpapers {
            wallpapers,
            errors,
            library_root,
        } => Ok(LibraryView {
            wallpapers,
            errors,
            library_root,
        }),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

#[tauri::command]
fn set_wallpaper(id: String, output: Option<String>) -> CmdResult<()> {
    expect_ok(ask(Request::SetWallpaper {
        id,
        output,
        persist: true,
    })?)
}

#[tauri::command]
fn clear_wallpaper(output: Option<String>) -> CmdResult<()> {
    expect_ok(ask(Request::ClearWallpaper {
        output,
        persist: true,
    })?)
}

#[tauri::command]
fn toggle_pause(output: Option<String>) -> CmdResult<()> {
    expect_ok(ask(Request::TogglePause { output })?)
}

#[tauri::command]
fn get_config() -> CmdResult<Config> {
    match ask(Request::GetConfig)? {
        Response::Config(config) => Ok(*config),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

#[tauri::command]
fn set_config(config: Config) -> CmdResult<()> {
    expect_ok(ask(Request::SetConfig {
        config: Box::new(config),
    })?)
}

fn expect_ok(response: Response) -> CmdResult<()> {
    match response {
        Response::Ok => Ok(()),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

/// Absolute path to a still image for a wallpaper, generating one if needed.
/// The frontend turns it into an asset URL with `convertFileSrc`.
#[tauri::command]
async fn thumbnail(id: String) -> CmdResult<PathBuf> {
    tauri::async_runtime::spawn_blocking(move || thumbnails::thumbnail_for(&id).map_err(stringify))
        .await
        .map_err(stringify)?
}

/// Copies a video into the library as a new wallpaper package and returns its
/// id. Importing by copy rather than by reference is deliberate: a library
/// full of symlinks into `~/Downloads` breaks the first time you tidy up.
#[tauri::command]
async fn import_wallpaper(path: PathBuf, title: Option<String>) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        thumbnails::import(&path, title.as_deref()).map_err(stringify)
    })
    .await
    .map_err(stringify)?
}

/// Starts the daemon and waits for it to answer.
///
/// Opening wallflux from an application menu is the one case where the daemon
/// reliably is not running yet, and telling someone to go and type a command
/// is a poor answer to "I clicked the icon".
#[tauri::command]
async fn start_daemon() -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(|| {
        if wallflux_core::ipc::daemon_running() {
            return Ok(());
        }
        let binary = daemon_binary()?;
        // Detached: the daemon must outlive this window, so it gets its own
        // session and its output goes nowhere rather than into a pipe that
        // fills up and blocks it.
        Command::new(&binary)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|err| format!("cannot start {}: {err}", binary.display()))?;

        // Wait for the socket rather than guessing at a sleep.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if wallflux_core::ipc::daemon_running() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!(
            "{} started but never opened its control socket — run it in a \
             terminal to see why",
            binary.display()
        ))
    })
    .await
    .map_err(stringify)?
}

/// Looks for `wallflux-daemon` next to this binary first, so a build run from
/// a checkout starts its own daemon rather than one installed system-wide.
fn daemon_binary() -> Result<PathBuf, String> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("wallflux-daemon");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    // Otherwise trust PATH and let the spawn report a sensible error.
    Ok(PathBuf::from("wallflux-daemon"))
}

/// What a scan of the Wallpaper Engine workshop turned up.
#[derive(Serialize)]
pub struct EngineLibrary {
    items: Vec<EngineItem>,
    /// Directories that were looked at, so the empty state can say where.
    searched: Vec<PathBuf>,
}

#[tauri::command]
async fn engine_scan() -> CmdResult<EngineLibrary> {
    tauri::async_runtime::spawn_blocking(|| {
        let searched = wallpaper_engine::workshop_dirs();
        let items = wallpaper_engine::scan_all().map_err(stringify)?;
        Ok(EngineLibrary { items, searched })
    })
    .await
    .map_err(stringify)?
}

/// Copies one workshop item into the library. Returns its new wallflux id.
#[tauri::command]
async fn engine_import(workshop_id: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let items = wallpaper_engine::scan_all().map_err(stringify)?;
        let item = items
            .iter()
            .find(|item| item.workshop_id == workshop_id)
            .ok_or_else(|| format!("no existe el elemento {workshop_id}"))?;
        let library = paths::library_dir().map_err(stringify)?;
        wallpaper_engine::import(item, &library).map_err(stringify)
    })
    .await
    .map_err(stringify)?
}

#[tauri::command]
fn library_path() -> CmdResult<PathBuf> {
    paths::library_dir().map_err(stringify)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // The asset protocol is what lets the webview load thumbnails and
            // play a wallpaper on hover, and it only serves paths inside its
            // scope. tauri.conf.json can only name the default XDG locations,
            // so the real ones are added here: a user who sets XDG_DATA_HOME
            // or WALLFLUX_LIBRARY_DIR would otherwise get a grid of broken
            // images with nothing explaining why.
            let scope = app.asset_protocol_scope();
            for dir in [paths::library_dir(), paths::cache_dir()] {
                match dir {
                    Ok(dir) => {
                        if let Err(err) = scope.allow_directory(&dir, true) {
                            eprintln!("cannot grant asset access to {}: {err}", dir.display());
                        }
                    }
                    Err(err) => eprintln!("cannot resolve a wallflux directory: {err:#}"),
                }
            }
            // Read-only, and only so the Wallpaper Engine browser can show
            // the previews that already sit next to each workshop item.
            for dir in wallpaper_engine::workshop_dirs() {
                if let Err(err) = scope.allow_directory(&dir, true) {
                    eprintln!("cannot grant asset access to {}: {err}", dir.display());
                }
            }
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            daemon_info,
            status,
            list_wallpapers,
            set_wallpaper,
            clear_wallpaper,
            toggle_pause,
            get_config,
            set_config,
            thumbnail,
            import_wallpaper,
            library_path,
            start_daemon,
            engine_scan,
            engine_import,
        ])
        .run(tauri::generate_context!())
        .expect("error while running wallflux GUI");
}
