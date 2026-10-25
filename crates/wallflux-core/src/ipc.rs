//! Control protocol between the daemon and its front-ends.
//!
//! Newline-delimited JSON over a Unix socket. JSON rather than a bespoke
//! binary format because the socket doubles as a debugging surface — you can
//! drive the daemon from the shell:
//!
//! ```sh
//! echo '{"kind":"status"}' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/wallflux/daemon.sock
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::library::Wallpaper;
use crate::paths;

/// A front-end that waits forever on a wedged daemon looks like a hang, so
/// every exchange is bounded.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Commands a front-end can send.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Liveness probe, also used to detect a version mismatch.
    Ping,
    Status,
    /// Wallpapers found in the library, including the ones that failed to load.
    ListWallpapers,
    /// Rescan the library directory from disk.
    RescanLibrary,
    /// Apply a wallpaper. `output: None` means every enabled output.
    SetWallpaper {
        id: String,
        #[serde(default)]
        output: Option<String>,
        /// Persist the choice to `config.toml`. The GUI sets this; a one-off
        /// `wallfluxctl` preview does not.
        #[serde(default = "default_true")]
        persist: bool,
    },
    /// Stop rendering an output and leave it blank.
    ClearWallpaper {
        #[serde(default)]
        output: Option<String>,
        #[serde(default = "default_true")]
        persist: bool,
    },
    /// Manual pause, independent of the automatic power policies.
    Pause {
        #[serde(default)]
        output: Option<String>,
    },
    Resume {
        #[serde(default)]
        output: Option<String>,
    },
    TogglePause {
        #[serde(default)]
        output: Option<String>,
    },
    GetConfig,
    /// Replace the config wholesale and apply it. The GUI reads, edits and
    /// writes back the whole document, which keeps the protocol small and
    /// makes every change atomic.
    SetConfig {
        config: Box<Config>,
    },
    /// Re-read `config.toml` from disk.
    ReloadConfig,
    Shutdown,
}

fn default_true() -> bool {
    true
}

/// Replies. Every request answers exactly one of these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// Acknowledgement for commands with nothing to report.
    Ok,
    Pong {
        /// Crate version of the running daemon.
        version: String,
        ipc_version: u32,
    },
    Status(Box<Status>),
    Wallpapers {
        wallpapers: Vec<Wallpaper>,
        /// `(path, reason)` for packages that could not be loaded.
        errors: Vec<(PathBuf, String)>,
        library_root: PathBuf,
    },
    Config(Box<Config>),
    Error {
        message: String,
    },
}

impl Response {
    /// Turns an `Error` reply into a Rust error so callers can use `?`.
    pub fn into_result(self) -> Result<Self> {
        match self {
            Response::Error { message } => bail!("{message}"),
            other => Ok(other),
        }
    }
}

/// Why an output is not currently drawing frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// Asked for explicitly through IPC.
    Manual,
    /// A fullscreen window is focused on this output.
    Fullscreen,
    /// Opaque windows cover the output.
    Occluded,
    /// Running on battery.
    Battery,
    /// No wallpaper assigned.
    Idle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputState {
    /// Connector name as the compositor reports it, e.g. `eDP-1`.
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Position in the global compositor space, for multi-monitor layouts.
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    /// Currently applied wallpaper id, if any.
    pub wallpaper: Option<String>,
    pub paused: bool,
    pub pause_reason: Option<PauseReason>,
    /// Frames per second actually rendered over the last second, which is what
    /// you want when checking whether the power policies are doing their job.
    pub measured_fps: f32,
    pub fps_cap: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    /// `"x11"` or `"wayland"`.
    pub backend: String,
    pub outputs: Vec<OutputState>,
    pub library_root: PathBuf,
    pub config_path: PathBuf,
    pub uptime_secs: u64,
}

/// A connected front-end. Short-lived: connect, ask, drop.
pub struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    /// Connects to the running daemon.
    pub fn connect() -> Result<Self> {
        let path = paths::socket_path()?;
        Self::connect_to(&path)
    }

    pub fn connect_to(path: &std::path::Path) -> Result<Self> {
        let stream = UnixStream::connect(path).with_context(|| {
            format!(
                "cannot reach the wallflux daemon at {} — is `wallflux-daemon` running?",
                path.display()
            )
        })?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self { stream, reader })
    }

    /// Sends one request and waits for its reply. An `Error` reply becomes an
    /// `Err`, so callers only handle the shapes they asked for.
    pub fn request(&mut self, request: &Request) -> Result<Response> {
        let mut line = serde_json::to_string(request).context("cannot encode request")?;
        line.push('\n');
        self.stream
            .write_all(line.as_bytes())
            .context("cannot write to the daemon socket")?;
        self.stream.flush()?;

        let mut reply = String::new();
        let read = self
            .reader
            .read_line(&mut reply)
            .context("cannot read from the daemon socket")?;
        if read == 0 {
            bail!("the daemon closed the connection without replying");
        }
        serde_json::from_str::<Response>(reply.trim_end())
            .with_context(|| format!("cannot decode daemon reply: {reply:?}"))?
            .into_result()
    }
}

/// True when a daemon is listening. Used by front-ends to decide whether to
/// offer a "start daemon" button instead of an error.
pub fn daemon_running() -> bool {
    paths::socket_path()
        .map(|path| UnixStream::connect(path).is_ok())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_tagged_by_kind() {
        let json = serde_json::to_string(&Request::Ping).unwrap();
        assert_eq!(json, r#"{"kind":"ping"}"#);
    }

    #[test]
    fn set_wallpaper_defaults_to_persisting_every_output() {
        let request: Request =
            serde_json::from_str(r#"{"kind":"set_wallpaper","id":"neon"}"#).unwrap();
        match request {
            Request::SetWallpaper {
                id,
                output,
                persist,
            } => {
                assert_eq!(id, "neon");
                assert_eq!(output, None);
                assert!(persist);
            }
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn error_replies_become_rust_errors() {
        let response = Response::Error {
            message: "no such wallpaper".into(),
        };
        let err = response.into_result().unwrap_err();
        assert_eq!(err.to_string(), "no such wallpaper");
    }

    #[test]
    fn responses_round_trip() {
        let status = Status {
            version: "0.1.0".into(),
            backend: "x11".into(),
            outputs: vec![OutputState {
                name: "eDP-1".into(),
                width: 2880,
                height: 1800,
                x: 0,
                y: 0,
                scale: 1.0,
                wallpaper: Some("neon".into()),
                paused: true,
                pause_reason: Some(PauseReason::Occluded),
                measured_fps: 0.0,
                fps_cap: 30,
            }],
            library_root: PathBuf::from("/tmp"),
            config_path: PathBuf::from("/tmp/config.toml"),
            uptime_secs: 7,
        };
        let json = serde_json::to_string(&Response::Status(Box::new(status.clone()))).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        match back {
            Response::Status(parsed) => assert_eq!(*parsed, status),
            other => panic!("parsed as {other:?}"),
        }
    }
}
