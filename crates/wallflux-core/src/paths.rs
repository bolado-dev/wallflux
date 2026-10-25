//! XDG locations wallflux uses. Every path is overridable through an
//! environment variable so a test or a packaged build can redirect them.

use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::{BaseDirs, ProjectDirs};

const APP: &str = "wallflux";

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("dev", "bolado", APP).context("cannot determine XDG directories")
}

/// `$WALLFLUX_CONFIG_DIR`, else `~/.config/wallflux`.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("WALLFLUX_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(project_dirs()?.config_dir().to_path_buf())
}

/// `config.toml` inside [`config_dir`].
pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Where wallpaper packages live: `$WALLFLUX_LIBRARY_DIR`, else
/// `~/.local/share/wallflux/wallpapers`.
pub fn library_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("WALLFLUX_LIBRARY_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(project_dirs()?.data_dir().join("wallpapers"))
}

/// Generated preview thumbnails, safe to delete at any time.
pub fn cache_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("WALLFLUX_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(project_dirs()?.cache_dir().to_path_buf())
}

/// Control socket. Prefers `$XDG_RUNTIME_DIR` (tmpfs, per-user, cleaned on
/// logout); falls back to `/tmp/wallflux-$UID` when the session does not set
/// one, which happens in bare `startx` sessions.
pub fn socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("WALLFLUX_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    let base = BaseDirs::new()
        .and_then(|dirs| dirs.runtime_dir().map(PathBuf::from))
        .unwrap_or_else(|| {
            // SAFETY: getuid cannot fail and only reads the real user id.
            let uid = unsafe { libc::getuid() };
            PathBuf::from(format!("/tmp/wallflux-{uid}"))
        });
    Ok(base.join(APP).join("daemon.sock"))
}

/// Creates `dir` and every missing parent, with a message naming the path when
/// it fails: a bare `PermissionDenied` is useless in a log.
pub fn ensure_dir(dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))
}
