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
use wallflux_core::gallery::{self, GalleryItem};
use wallflux_core::library::Wallpaper;
use wallflux_core::steam_workshop::{
    self, InstallOption, LoginCheck, Sort as WorkshopSort, WorkshopItem,
};
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
        let child = Command::new(&binary)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|err| format!("cannot start {}: {err}", binary.display()))?;

        // Reap it. The daemon runs in its own process group and outlives
        // this window, but it is still our child: without a wait() it turns
        // into a zombie the moment it exits, and sits in the process table
        // for as long as the GUI is open. Observed in the wild as
        // `[wallflux-daemon] <defunct>` parented to wallflux-gui.
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
        });

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

/// Lo que la galería tiene para enseñar, y con qué fuentes cuenta.
#[derive(Serialize)]
pub struct GalleryView {
    items: Vec<GalleryItem>,
    /// Cierto cuando hay clave de Pexels, para que la interfaz ofrezca
    /// ponerla en vez de callarse que existe esa opción.
    pexels_enabled: bool,
    /// Aviso no fatal, como que Pexels haya fallado pero el catálogo no.
    warning: Option<String>,
}

#[tauri::command]
async fn gallery_browse(query: String) -> CmdResult<GalleryView> {
    tauri::async_runtime::spawn_blocking(move || {
        let config = Config::load().unwrap_or_default();
        let mut warning = None;

        let mut items = gallery::fetch_catalog(&config.gallery.catalog_url).map_err(stringify)?;
        if !query.trim().is_empty() {
            let needle = query.to_lowercase();
            items.retain(|item| {
                item.title.to_lowercase().contains(&needle)
                    || item
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&needle))
            });
        }

        let key = config.gallery.pexels_api_key.trim().to_string();
        let pexels_enabled = !key.is_empty();
        if pexels_enabled {
            // Que Pexels falle no puede dejar sin galería: el catálogo ya
            // está cargado y es lo que hace que esto funcione sin cuenta.
            match gallery::search_pexels(&key, &query, 30) {
                Ok(found) => items.extend(found),
                Err(err) => warning = Some(format!("Pexels: {err:#}")),
            }
        }

        Ok(GalleryView {
            items,
            pexels_enabled,
            warning,
        })
    })
    .await
    .map_err(stringify)?
}

/// Descarga un fondo de la galería. Devuelve su id local.
#[tauri::command]
async fn gallery_download(item: GalleryItem) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let library = gallery::library_root().map_err(stringify)?;
        gallery::download(&item, &library, &mut gallery::no_progress()).map_err(stringify)
    })
    .await
    .map_err(stringify)?
}

/// Lo que el taller devuelve, más el estado del entorno de descarga, para
/// que la interfaz sepa si puede ofrecer el botón o tiene que explicar algo.
#[derive(Serialize)]
pub struct WorkshopView {
    items: Vec<WorkshopItem>,
    /// Cierto si steamcmd está disponible en el sistema.
    steamcmd: bool,
    /// Qué hacer si no lo está.
    steamcmd_help: String,
    /// Cuenta guardada en la configuración, si la hay.
    steam_user: String,
}

#[tauri::command]
async fn workshop_browse(query: String, sort: String, page: u32) -> CmdResult<WorkshopView> {
    tauri::async_runtime::spawn_blocking(move || {
        let sort = match sort.as_str() {
            "recent" => WorkshopSort::MostRecent,
            "subscribed" => WorkshopSort::MostSubscribed,
            _ => WorkshopSort::Trend,
        };
        let items = steam_workshop::browse(&query, sort, page).map_err(stringify)?;
        let config = Config::load().unwrap_or_default();
        Ok(WorkshopView {
            items,
            steamcmd: steam_workshop::steamcmd_path().is_some(),
            steamcmd_help: steam_workshop::steamcmd_help().to_string(),
            steam_user: config.workshop.steam_user,
        })
    })
    .await
    .map_err(stringify)?
}

/// Descarga un elemento con la cuenta del usuario y lo importa.
#[tauri::command]
async fn workshop_import(item_id: String, steam_user: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let staging = paths::cache_dir().map_err(stringify)?.join("steamcmd");
        let library = paths::library_dir().map_err(stringify)?;
        let downloaded =
            steam_workshop::download_with_steamcmd(&item_id, &steam_user, &staging)
                .map_err(stringify)?;
        let id = steam_workshop::import_downloaded(&downloaded.path, &library)
            .map_err(stringify)?;

        // Recordar la cuenta solo cuando ha funcionado: guardar un nombre
        // que Steam acaba de rechazar sería recordar un error.
        let mut config = Config::load().unwrap_or_default();
        if config.workshop.steam_user != steam_user {
            config.workshop.steam_user = steam_user;
            if let Err(err) = config.save() {
                eprintln!("no se ha podido guardar la cuenta de Steam: {err:#}");
            }
        }
        Ok(id)
    })
    .await
    .map_err(stringify)?
}

/// Todo lo que el asistente de Steam necesita saber de una vez, para que la
/// interfaz no tenga que encadenar tres llamadas antes de pintar nada.
#[derive(Serialize)]
pub struct SteamSetup {
    /// Ruta de steamcmd, si está instalado.
    steamcmd_path: Option<String>,
    /// Formas de instalarlo, con la de esta distro primero.
    install_options: Vec<InstallOption>,
    /// Cuenta guardada en la configuración.
    steam_user: String,
}

#[tauri::command]
async fn steam_setup() -> CmdResult<SteamSetup> {
    tauri::async_runtime::spawn_blocking(|| {
        let config = Config::load().unwrap_or_default();
        Ok(SteamSetup {
            steamcmd_path: steam_workshop::steamcmd_path()
                .map(|path| path.display().to_string()),
            install_options: steam_workshop::install_options(),
            steam_user: config.workshop.steam_user,
        })
    })
    .await
    .map_err(stringify)?
}

/// Comprueba si steamcmd ya tiene sesión para esta cuenta, y la guarda si la
/// tiene. Puede tardar: arranca steamcmd de verdad.
#[tauri::command]
async fn steam_check_login(user: String) -> CmdResult<LoginCheck> {
    tauri::async_runtime::spawn_blocking(move || {
        let check = steam_workshop::check_login(&user).map_err(stringify)?;
        if check.logged_in {
            let mut config = Config::load().unwrap_or_default();
            let trimmed = user.trim();
            if config.workshop.steam_user != trimmed {
                config.workshop.steam_user = trimmed.to_string();
                let _ = config.save();
            }
        }
        Ok(check)
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
            gallery_browse,
            gallery_download,
            workshop_browse,
            workshop_import,
            steam_setup,
            steam_check_login,
        ])
        .run(tauri::generate_context!())
        .expect("error while running wallflux GUI");
}
