//! `wallfluxctl` — talks to the daemon over its control socket.
//!
//! Every subcommand is one request and one reply, so this is a thin shell
//! around [`wallflux_core::ipc`]. The output is written for a terminal by
//! default and for a script with `--json`.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use wallflux_core::gallery::{self, GalleryItem, Source};
use wallflux_core::ipc::{Client, PauseReason, Request, Response, Status};
use wallflux_core::steam_workshop::{self, Sort as WorkshopSort};

#[derive(Parser, Debug)]
#[command(
    name = "wallfluxctl",
    version,
    about = "Controla el daemon de fondos wallflux"
)]
struct Args {
    /// Imprime JSON en crudo en vez de un resumen legible.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Muestra el estado del daemon: salidas, fondos y fps medidos.
    Status,
    /// Lista los fondos de la biblioteca.
    #[command(visible_alias = "ls")]
    List,
    /// Aplica un fondo por su id.
    Set {
        /// Id del fondo, tal como lo imprime `wallfluxctl list`.
        id: String,
        /// Solo esta salida. Por defecto, todas.
        #[arg(long, short)]
        output: Option<String>,
        /// Aplica sin guardar la elección en config.toml.
        #[arg(long)]
        no_persist: bool,
    },
    /// Quita el fondo y deja la salida en negro.
    Clear {
        #[arg(long, short)]
        output: Option<String>,
        #[arg(long)]
        no_persist: bool,
    },
    /// Deja de dibujar.
    Pause {
        #[arg(long, short)]
        output: Option<String>,
    },
    /// Vuelve a dibujar.
    Resume {
        #[arg(long, short)]
        output: Option<String>,
    },
    /// Pausa si va, reanuda si está pausado. Cómodo en un atajo.
    Toggle {
        #[arg(long, short)]
        output: Option<String>,
    },
    /// Relee el directorio de la biblioteca desde disco.
    Rescan,
    /// Imprime la configuración en uso como TOML.
    Config,
    /// Hace que el daemon relea config.toml.
    Reload,
    /// Pide al daemon que termine.
    Shutdown,
    /// Comprueba que hay un daemon escuchando.
    Ping,
    /// Busca y descarga fondos libres de la galería en línea.
    Gallery(GalleryArgs),
    /// Explora el taller de Wallpaper Engine en Steam, e importa con tu cuenta.
    Workshop(WorkshopArgs),
}

#[derive(clap::Args, Debug)]
struct WorkshopArgs {
    /// Texto a buscar en el taller.
    query: Option<String>,
    /// Orden del listado.
    #[arg(long, value_enum, default_value = "trend")]
    sort: SortArg,
    /// Página del listado, empezando en 1.
    #[arg(long, default_value_t = 1)]
    page: u32,
    /// Descarga e importa estos ids. Necesita steamcmd y --steam-user.
    #[arg(long, short, num_args = 1..)]
    get: Vec<String>,
    /// Cuenta de Steam que posee Wallpaper Engine.
    #[arg(long)]
    steam_user: Option<String>,
    /// Muestra también los que no se pueden importar.
    #[arg(long)]
    all: bool,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum SortArg {
    Trend,
    Recent,
    Subscribed,
}

impl From<SortArg> for WorkshopSort {
    fn from(value: SortArg) -> Self {
        match value {
            SortArg::Trend => WorkshopSort::Trend,
            SortArg::Recent => WorkshopSort::MostRecent,
            SortArg::Subscribed => WorkshopSort::MostSubscribed,
        }
    }
}

#[derive(clap::Args, Debug)]
struct GalleryArgs {
    /// Texto a buscar. Sin clave de Pexels solo filtra el catálogo curado.
    query: Option<String>,
    /// Descarga estos ids en vez de solo listarlos.
    #[arg(long, short, num_args = 1..)]
    get: Vec<String>,
    /// Descarga todo lo que salga en el listado. Úsalo con cabeza.
    #[arg(long)]
    all: bool,
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("wallfluxctl: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<()> {
    // La galería es red y disco: no necesita un daemon en marcha, y el
    // momento más probable de usarla es antes de tener ninguno.
    if let Command::Gallery(gallery_args) = &args.command {
        return gallery_command(gallery_args, args.json);
    }
    if let Command::Workshop(workshop_args) = &args.command {
        return workshop_command(workshop_args, args.json);
    }

    let mut client = Client::connect()?;
    let json = args.json;

    let request = match &args.command {
        Command::Status => Request::Status,
        Command::List => Request::ListWallpapers,
        Command::Set {
            id,
            output,
            no_persist,
        } => Request::SetWallpaper {
            id: id.clone(),
            output: output.clone(),
            persist: !no_persist,
        },
        Command::Clear { output, no_persist } => Request::ClearWallpaper {
            output: output.clone(),
            persist: !no_persist,
        },
        Command::Pause { output } => Request::Pause {
            output: output.clone(),
        },
        Command::Resume { output } => Request::Resume {
            output: output.clone(),
        },
        Command::Toggle { output } => Request::TogglePause {
            output: output.clone(),
        },
        Command::Rescan => Request::RescanLibrary,
        Command::Config => Request::GetConfig,
        Command::Reload => Request::ReloadConfig,
        Command::Shutdown => Request::Shutdown,
        Command::Ping => Request::Ping,
        // Atendidos arriba, antes de conectar con el daemon.
        Command::Gallery(_) | Command::Workshop(_) => unreachable!(),
    };

    let response = client.request(&request)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    match response {
        Response::Ok => println!("hecho"),
        Response::Pong {
            version,
            ipc_version,
        } => {
            println!("wallflux-daemon {version} (IPC v{ipc_version})");
            if ipc_version != wallflux_core::IPC_VERSION {
                bail!(
                    "este wallfluxctl habla IPC v{} — reinicia el daemon para que coincidan",
                    wallflux_core::IPC_VERSION
                );
            }
        }
        Response::Status(status) => print_status(&status),
        Response::Wallpapers {
            wallpapers,
            errors,
            library_root,
        } => {
            if wallpapers.is_empty() && errors.is_empty() {
                println!("No hay fondos en {}.", library_root.display());
                println!("Suelta un vídeo ahí y ejecuta `wallfluxctl rescan`.");
            }
            for wallpaper in &wallpapers {
                let author = wallpaper
                    .author
                    .as_deref()
                    .map(|author| format!("  de {author}"))
                    .unwrap_or_default();
                let implicit = if wallpaper.implicit {
                    "  (sin manifiesto)"
                } else {
                    ""
                };
                println!("{:<24} {}{author}{implicit}", wallpaper.id, wallpaper.title);
            }
            for (path, reason) in &errors {
                eprintln!("roto: {} — {reason}", path.display());
            }
        }
        Response::Config(config) => {
            print!(
                "{}",
                toml::to_string_pretty(&*config)
                    .context("no se puede serializar la configuración a TOML")?
            );
        }
        Response::Error { message } => bail!("{message}"),
    }
    Ok(())
}

/// Lista o descarga fondos de la galería en línea.
fn gallery_command(args: &GalleryArgs, json: bool) -> Result<()> {
    let config = wallflux_core::Config::load().unwrap_or_default();
    let query = args.query.as_deref().unwrap_or_default();

    let mut items = gallery::fetch_catalog(&config.gallery.catalog_url)?;
    if !query.is_empty() {
        let needle = query.to_lowercase();
        items.retain(|item| {
            item.title.to_lowercase().contains(&needle)
                || item
                    .tags
                    .iter()
                    .any(|tag| tag.to_lowercase().contains(&needle))
        });
    }

    // Pexels solo entra si el usuario ha puesto su clave; es un extra, no un
    // requisito, y sin ella la galería sigue funcionando.
    if !config.gallery.pexels_api_key.trim().is_empty() {
        match gallery::search_pexels(&config.gallery.pexels_api_key, query, 24) {
            Ok(found) => items.extend(found),
            Err(err) => eprintln!("aviso: la búsqueda en Pexels ha fallado: {err:#}"),
        }
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    let chosen: Vec<&GalleryItem> = if args.all {
        items.iter().collect()
    } else if !args.get.is_empty() {
        let mut chosen = Vec::new();
        for id in &args.get {
            chosen.push(
                items
                    .iter()
                    .find(|item| &item.id == id)
                    .with_context(|| format!("no hay ningún fondo con id {id}"))?,
            );
        }
        chosen
    } else {
        // Sin --get ni --all, solo se lista.
        if items.is_empty() {
            println!("La galería no ha devuelto nada para «{query}».");
            return Ok(());
        }
        println!("{:<22} {:<44} {:<14} ORIGEN", "ID", "TÍTULO", "LICENCIA");
        for item in &items {
            let origin = match item.source {
                Source::Curated => "catálogo",
                Source::Pexels => "pexels",
            };
            println!(
                "{:<22} {:<44} {:<14} {origin}",
                item.id,
                truncate(&item.title, 44),
                truncate(&item.license, 14)
            );
        }
        println!(
            "\n{} fondo(s). Descarga con: wallfluxctl gallery --get <id>",
            items.len()
        );
        if config.gallery.pexels_api_key.trim().is_empty() {
            println!(
                "Para buscar también en Pexels, pon tu clave gratuita en \
                 gallery.pexels_api_key dentro de config.toml."
            );
        }
        return Ok(());
    };

    let library = gallery::library_root()?;
    let mut downloaded = 0;
    for item in chosen {
        print!("descargando {}… ", item.title);
        use std::io::Write;
        let _ = std::io::stdout().flush();
        match gallery::download(item, &library, &mut gallery::no_progress()) {
            Ok(id) => {
                println!("como {id}");
                downloaded += 1;
            }
            Err(err) => println!("error: {err:#}"),
        }
    }
    println!("\n{downloaded} fondo(s) en {}", library.display());

    if wallflux_core::ipc::daemon_running()
        && let Ok(mut client) = Client::connect()
        && client.request(&Request::RescanLibrary).is_ok()
    {
        println!("el daemon en marcha ha reescaneado su biblioteca");
    }
    Ok(())
}

/// Explora el taller de Steam y, con una cuenta que tenga la app, importa.
fn workshop_command(args: &WorkshopArgs, json: bool) -> Result<()> {
    // Descargar necesita cuenta; listar no. Por eso el listado va primero y
    // sin pedir nada.
    if !args.get.is_empty() {
        return workshop_download(args);
    }

    let query = args.query.as_deref().unwrap_or_default();
    let items = steam_workshop::browse(query, args.sort.into(), args.page)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    if items.is_empty() {
        println!("El taller no ha devuelto nada para «{query}».");
        return Ok(());
    }

    let shown: Vec<_> = if args.all {
        items.iter().collect()
    } else {
        items.iter().filter(|item| item.kind.importable()).collect()
    };

    println!(
        "{:<12} {:<44} {:<12} {:<8} ESTADO",
        "ID", "TÍTULO", "RESOLUCIÓN", "TAMAÑO"
    );
    for item in &shown {
        let status = if item.kind.importable() {
            "se puede importar".to_string()
        } else {
            item.kind.reason().to_string()
        };
        println!(
            "{:<12} {:<44} {:<12} {:<8} {status}",
            item.id,
            truncate(&item.title, 44),
            item.resolution.as_deref().unwrap_or("—"),
            human_size(item.file_size),
        );
    }

    let importable = items.iter().filter(|item| item.kind.importable()).count();
    println!(
        "\n{importable} de {} son de vídeo e importables{}.",
        items.len(),
        if args.all {
            ""
        } else {
            " (usa --all para ver el resto)"
        }
    );
    println!(
        "Descarga con: wallfluxctl workshop --get <id> --steam-user <tu cuenta>\n\
         Necesita steamcmd y una cuenta que posea Wallpaper Engine: Steam no \
         publica descarga directa de este contenido."
    );
    Ok(())
}

fn workshop_download(args: &WorkshopArgs) -> Result<()> {
    let user = args
        .steam_user
        .as_deref()
        .context("hace falta --steam-user: la descarga usa tu propia cuenta de Steam")?;
    if steam_workshop::steamcmd_path().is_none() {
        bail!("{}", steam_workshop::steamcmd_help());
    }

    let staging = wallflux_core::paths::cache_dir()?.join("steamcmd");
    let library = wallflux_core::paths::library_dir()?;
    let mut imported = 0;

    for id in &args.get {
        println!("descargando {id} con la cuenta {user}…");
        let downloaded = match steam_workshop::download_with_steamcmd(id, user, &staging) {
            Ok(downloaded) => downloaded,
            Err(err) => {
                eprintln!("{err:#}");
                continue;
            }
        };
        match steam_workshop::import_downloaded(&downloaded.path, &library) {
            Ok(local) => {
                println!("  importado como {local}");
                imported += 1;
            }
            Err(err) => eprintln!("  no se ha importado: {err:#}"),
        }
    }

    println!("\n{imported} fondo(s) en {}", library.display());
    if wallflux_core::ipc::daemon_running()
        && let Ok(mut client) = Client::connect()
        && client.request(&Request::RescanLibrary).is_ok()
    {
        println!("el daemon en marcha ha reescaneado su biblioteca");
    }
    Ok(())
}

fn human_size(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if bytes == 0 {
        "—".to_string()
    } else if bytes >= MB {
        format!("{} MB", bytes / MB)
    } else {
        format!("{} KB", bytes / 1024)
    }
}

/// Recorta a `width` caracteres — no bytes: un corte por bytes revienta con
/// cualquier título acentuado.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn print_status(status: &Status) {
    println!(
        "wallflux {} en {}, lleva {}",
        status.version,
        status.backend,
        format_uptime(status.uptime_secs)
    );
    println!("config:     {}", status.config_path.display());
    println!("biblioteca: {}", status.library_root.display());
    if status.outputs.is_empty() {
        println!("\nNo hay salidas.");
        return;
    }
    println!();
    println!(
        "{:<12} {:<12} {:<20} {:<9} ESTADO",
        "SALIDA", "TAMAÑO", "FONDO", "FPS"
    );
    for output in &status.outputs {
        let size = format!("{}x{}", output.width, output.height);
        let wallpaper = output.wallpaper.as_deref().unwrap_or("—");
        let fps = if output.paused {
            "—".to_string()
        } else {
            format!("{:.0}/{}", output.measured_fps, output.fps_cap)
        };
        let state = match (output.paused, output.pause_reason) {
            (false, _) => "reproduciendo".to_string(),
            (true, Some(reason)) => format!("en pausa: {}", describe(reason)),
            (true, None) => "en pausa".to_string(),
        };
        println!(
            "{:<12} {size:<12} {wallpaper:<20} {fps:<9} {state}",
            output.name
        );
    }
}

/// Why a wallpaper is not moving, in words a user can act on.
fn describe(reason: PauseReason) -> &'static str {
    match reason {
        PauseReason::Manual => "a petición",
        PauseReason::Fullscreen => "ventana a pantalla completa",
        PauseReason::Occluded => "tapado por ventanas",
        PauseReason::Battery => "con batería",
        PauseReason::Idle => "sin fondo asignado",
    }
}

fn format_uptime(seconds: u64) -> String {
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (hours, rest) = (rest / 3_600, rest % 3_600);
    let (minutes, seconds) = (rest / 60, rest % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_reads_naturally_at_each_scale() {
        assert_eq!(format_uptime(45), "45s");
        assert_eq!(format_uptime(125), "2m 5s");
        assert_eq!(format_uptime(7_300), "2h 1m");
        assert_eq!(format_uptime(180_000), "2d 2h");
    }

    #[test]
    fn every_pause_reason_has_a_description() {
        for reason in [
            PauseReason::Manual,
            PauseReason::Fullscreen,
            PauseReason::Occluded,
            PauseReason::Battery,
            PauseReason::Idle,
        ] {
            assert!(!describe(reason).is_empty());
        }
    }

    #[test]
    fn long_titles_are_truncated_by_characters_not_bytes() {
        assert_eq!(truncate("corto", 10), "corto");
        assert_eq!(truncate("un título larguísimo de fondo", 10), "un título…");
        // Por bytes esto entraría en pánico a mitad de un carácter.
        assert_eq!(truncate("ñññññññññññ", 5), "ññññ…");
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(0), "—");
        assert_eq!(human_size(37_104_544), "35 MB");
        assert_eq!(human_size(4096), "4 KB");
    }

    #[test]
    fn the_cli_parses_its_own_help() {
        use clap::CommandFactory;
        Args::command().debug_assert();
    }
}
