//! `wallfluxctl` — talks to the daemon over its control socket.
//!
//! Every subcommand is one request and one reply, so this is a thin shell
//! around [`wallflux_core::ipc`]. The output is written for a terminal by
//! default and for a script with `--json`.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use wallflux_core::ipc::{Client, PauseReason, Request, Response, Status};
use wallflux_core::wallpaper_engine;

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
    /// Importa fondos de vídeo de una instalación de Wallpaper Engine.
    #[command(name = "import-engine")]
    ImportEngine(ImportEngineArgs),
}

#[derive(clap::Args, Debug)]
struct ImportEngineArgs {
    /// Ids del taller a importar. Omítelos con --all para coger todos los de vídeo.
    ids: Vec<String>,
    /// Lista lo encontrado sin importar nada.
    #[arg(long, short)]
    list: bool,
    /// Importa todos los elementos importables.
    #[arg(long, short)]
    all: bool,
    /// Busca aquí en vez de en las rutas habituales de Steam.
    #[arg(long)]
    from: Option<std::path::PathBuf>,
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
    // Importing is filesystem work, so it does not need a running daemon —
    // and the most likely moment to run it is before you have started one.
    if let Command::ImportEngine(engine) = &args.command {
        return import_engine(engine, args.json);
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
        // Handled above, before the daemon connection.
        Command::ImportEngine(_) => unreachable!(),
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

/// Lists or imports Wallpaper Engine workshop items.
fn import_engine(args: &ImportEngineArgs, json: bool) -> Result<()> {
    let items = match &args.from {
        Some(dir) => wallpaper_engine::scan(dir)?,
        None => wallpaper_engine::scan_all()?,
    };

    if items.is_empty() {
        let searched = match &args.from {
            Some(dir) => vec![dir.clone()],
            None => wallpaper_engine::workshop_dirs(),
        };
        if searched.is_empty() {
            bail!(
                "no se ha encontrado la carpeta del taller de Wallpaper Engine. \
                 Si tu Steam está en un sitio poco habitual, indícalo con --from <carpeta>"
            );
        }
        bail!("no hay elementos del taller en: {searched:?}");
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    if args.list || (args.ids.is_empty() && !args.all) {
        println!(
            "{:<14} {:<12} {:<34} ESTADO",
            "ID DEL TALLER", "TIPO", "TÍTULO"
        );
        for item in &items {
            let kind = format!("{:?}", item.kind).to_lowercase();
            let status = if item.kind.importable() {
                "se puede importar".to_string()
            } else {
                item.kind.reason().to_string()
            };
            println!(
                "{:<14} {kind:<12} {:<34} {status}",
                item.workshop_id,
                truncate(&item.title, 34)
            );
        }
        let importable = items.iter().filter(|item| item.kind.importable()).count();
        println!(
            "\n{importable} de {} elemento(s) se pueden importar.",
            items.len()
        );
        if !args.list {
            println!("Impórtalos con: wallfluxctl import-engine --all");
        }
        return Ok(());
    }

    let library = wallflux_core::paths::library_dir()?;
    let chosen: Vec<_> = if args.all {
        items.iter().filter(|item| item.kind.importable()).collect()
    } else {
        let mut chosen = Vec::new();
        for id in &args.ids {
            let item = items
                .iter()
                .find(|item| &item.workshop_id == id)
                .with_context(|| format!("no hay ningún elemento del taller con id {id}"))?;
            chosen.push(item);
        }
        chosen
    };

    let mut imported = 0;
    for item in chosen {
        match wallpaper_engine::import(item, &library) {
            Ok(id) => {
                println!("importado {} como {id}", item.title);
                imported += 1;
            }
            Err(err) => eprintln!("omitido {}: {err:#}", item.title),
        }
    }
    println!("\n{imported} fondo(s) importados en {}", library.display());

    // Tell a running daemon to pick them up; it is fine if none is running.
    if wallflux_core::ipc::daemon_running()
        && let Ok(mut client) = Client::connect()
        && client.request(&Request::RescanLibrary).is_ok()
    {
        println!("el daemon en marcha ha reescaneado su biblioteca");
    }
    Ok(())
}

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
    use wallflux_core::wallpaper_engine::ItemKind;

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
    fn long_titles_are_truncated_with_an_ellipsis() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a very long wallpaper title", 10), "a very lo…");
        // Counted in characters, not bytes: a byte slice would panic here.
        assert_eq!(truncate("ñññññññññññ", 5), "ññññ…");
    }

    #[test]
    fn every_engine_item_kind_explains_itself() {
        for kind in [
            ItemKind::Video,
            ItemKind::Scene,
            ItemKind::Web,
            ItemKind::Application,
            ItemKind::Unknown,
        ] {
            assert!(!kind.reason().is_empty());
        }
        assert!(ItemKind::Video.importable());
        assert!(!ItemKind::Scene.importable());
    }

    #[test]
    fn the_cli_parses_its_own_help() {
        use clap::CommandFactory;
        Args::command().debug_assert();
    }
}
