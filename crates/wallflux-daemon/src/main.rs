//! wallflux daemon: draws animated wallpapers under your windows.
//!
//! The whole program is one thread doing one thing in a loop — wait for
//! something to happen, decide whether a frame is owed, draw it — plus a
//! listener thread for the control socket. Everything interesting is in
//! [`engine`] and [`policy`].

mod backend;
mod engine;
mod ipc_server;
mod policy;
mod power;
mod render;
mod wake;

use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use wallflux_core::Config;

use crate::engine::Engine;
use crate::ipc_server::IpcServer;

/// Longest the loop will sleep with nothing to do. Not a polling interval —
/// every real event wakes us immediately — just a backstop so a missed wakeup
/// cannot wedge the daemon forever.
const MAX_SLEEP: Duration = Duration::from_secs(5);

#[derive(Parser, Debug)]
#[command(
    name = "wallflux-daemon",
    version,
    about = "Daemon de fondos animados para X11 y compositores wlroots"
)]
struct Args {
    /// Nivel de registro: error, warn, info, debug, trace.
    #[arg(long, default_value = "info", env = "WALLFLUX_LOG")]
    log: String,

    /// Comprueba el entorno (libmpv, EGL, un backend usable) y termina.
    #[arg(long)]
    check: bool,
}

/// Write end of the wakeup pipe, for the signal handler. A signal handler may
/// only touch `volatile sig_atomic_t`-shaped state and async-signal-safe
/// calls, which rules out anything nicer than this.
static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);
static SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_signal(_signal: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
    let fd = SIGNAL_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = 1u8;
        // SAFETY: write(2) is async-signal-safe, and the descriptor stays open
        // for the process lifetime once installed.
        unsafe {
            libc::write(fd, &byte as *const u8 as *const libc::c_void, 1);
        }
    }
}

fn install_signal_handlers() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: installing a handler that only stores a flag and writes one
        // byte to a pipe.
        unsafe {
            libc::signal(signal, on_signal as *const () as libc::sighandler_t);
        }
    }
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    init_logging(&args.log);

    match run(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            // `{:#}` prints the whole anyhow chain, which is where the
            // actionable part usually is ("install libmpv2").
            tracing::error!("{err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn init_logging(level: &str) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

fn run(args: Args) -> Result<()> {
    if args.check {
        return check_environment();
    }

    // Fail early and legibly rather than when the first wallpaper is applied.
    render::mpv::probe().context("libmpv is not usable")?;

    let config = Config::load()?;
    tracing::debug!(?config, "config loaded");

    let (waker, wake_receiver) = wake::pipe()?;
    SIGNAL_FD.store(waker.raw_fd(), Ordering::SeqCst);
    install_signal_handlers();

    // The socket bind doubles as the single-instance lock, so it goes up
    // before the engine claims any window.
    let server = IpcServer::start(waker.clone())?;
    tracing::info!(socket = %server.socket_path().display(), "control socket ready");

    let mut engine = Engine::new(config, waker)?;

    while !SHUTDOWN.load(Ordering::SeqCst) && !engine.should_exit() {
        engine.handle_backend_events()?;

        for pending in server.pending() {
            let request = pending.request.clone();
            tracing::debug!(?request, "IPC request");
            let response = engine.handle_request(request);
            pending.answer(response);
        }

        let wait = engine.tick()?.unwrap_or(MAX_SLEEP).min(MAX_SLEEP);
        if engine.should_exit() || SHUTDOWN.load(Ordering::SeqCst) {
            break;
        }
        wait_for_work(&wake_receiver, engine.backend_fd(), wait);
        wake_receiver.drain();
    }

    tracing::info!("shutting down");
    Ok(())
}

/// Reports what wallflux can and cannot do here, without starting anything.
///
/// Deliberately keeps going after a failure and reports every line, because
/// the whole point is to show which piece is missing — stopping at the first
/// one would hide the rest.
fn check_environment() -> Result<()> {
    let mut problems = 0;

    match render::mpv::probe() {
        Ok(()) => println!("libmpv:      ok"),
        Err(err) => {
            problems += 1;
            println!("libmpv:      FALTA\n             {err:#}");
        }
    }

    match backend::autodetect() {
        Ok(backend) => {
            println!("backend:     {}", backend.name());
            for output in backend.outputs() {
                println!(
                    "salida:      {} {}x{} en {},{} (escala {})",
                    output.name, output.width, output.height, output.x, output.y, output.scale
                );
            }
        }
        Err(err) => {
            problems += 1;
            println!("backend:     NO DISPONIBLE\n             {err:#}");
        }
    }

    println!(
        "config:      {}",
        wallflux_core::paths::config_file()?.display()
    );
    let library = wallflux_core::paths::library_dir()?;
    match wallflux_core::Library::load_from(&library) {
        Ok(found) => println!(
            "biblioteca:  {} ({} fondo(s), {} con errores)",
            library.display(),
            found.wallpapers.len(),
            found.errors.len()
        ),
        Err(err) => println!("biblioteca:  {} (ilegible: {err:#})", library.display()),
    }
    println!(
        "socket:      {}",
        wallflux_core::paths::socket_path()?.display()
    );

    if problems > 0 {
        anyhow::bail!("{problems} problema(s) encontrados");
    }
    Ok(())
}

/// Blocks until the window system has events, a front-end sends a request,
/// mpv has a frame, a signal arrives, or `timeout` elapses.
fn wait_for_work(
    wake: &wake::WakeReceiver,
    backend_fd: Option<std::os::fd::RawFd>,
    timeout: Duration,
) {
    let mut fds = Vec::with_capacity(2);
    fds.push(libc::pollfd {
        fd: wake.raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    });
    if let Some(fd) = backend_fd {
        fds.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }

    // poll takes whole milliseconds; round up so a sub-millisecond wait does
    // not become a busy spin.
    let millis = timeout
        .as_millis()
        .min(i32::MAX as u128)
        .max(if timeout.is_zero() { 0 } else { 1 }) as i32;

    // SAFETY: fds is a live slice of pollfd whose length we pass, and the
    // descriptors are owned by the receiver and the backend.
    let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
    if result < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            tracing::warn!(%err, "poll failed");
            // Avoid a hot loop if poll keeps failing for an unexpected reason.
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
