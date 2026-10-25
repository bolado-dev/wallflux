//! Shared vocabulary for the wallflux daemon, CLI and GUI.
//!
//! Nothing in here touches X11, Wayland or mpv: it is the part of wallflux
//! that every process agrees on, so the daemon and the front-ends can evolve
//! independently as long as the types below stay compatible.

pub mod config;
pub mod gallery;
pub mod ipc;
pub mod library;
pub mod paths;
pub mod steam_workshop;

pub use config::{Config, GalleryConfig, PlaybackConfig, PowerConfig, ScaleMode, WorkshopConfig};
pub use ipc::{OutputState, Request, Response, Status};
pub use library::{Library, Wallpaper, WallpaperKind};

/// Bumped whenever the on-disk config layout changes in a way that needs
/// migrating. Written to `config.toml` so an older daemon refuses a newer file
/// instead of silently losing fields.
pub const CONFIG_VERSION: u32 = 1;

/// Bumped on breaking IPC changes so a stale CLI reports a clear error.
pub const IPC_VERSION: u32 = 1;
