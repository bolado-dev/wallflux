//! Window-system backends.
//!
//! A backend has one job: own the surfaces wallflux draws into, and report
//! what the renderer cannot know on its own — which outputs exist, how big
//! they are, and whether drawing to them is currently worth the CPU.
//!
//! The two implementations answer that last question very differently, and the
//! difference is worth knowing about:
//!
//! * **X11** has to work it out. The daemon watches the window tree and
//!   compares window geometry against each output, because an X server will
//!   happily let you render frames nobody can see. See [`x11::Occlusion`].
//! * **Wayland** gets it for free. A `wlr-layer-shell` surface is only sent
//!   frame callbacks while the compositor actually needs a new frame, so an
//!   occluded output simply stops asking. No heuristics.

use std::ffi::c_void;

use anyhow::Result;

use crate::render::{Egl, Platform};

pub mod wayland;
pub mod x11;

/// A monitor as the window system describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputInfo {
    /// Connector name, e.g. `eDP-1`. Used as the key in config and IPC.
    pub name: String,
    /// Position in the global layout, for multi-monitor setups.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Fractional on Wayland, always 1.0 on X11, which scales by resolution.
    pub scale: f64,
}

/// Something the backend noticed that the engine has to react to.
#[derive(Debug, Clone, PartialEq)]
pub enum BackendEvent {
    OutputAdded(OutputInfo),
    /// Geometry or scale changed; the engine recreates the surface.
    OutputChanged(OutputInfo),
    OutputRemoved(String),
    /// Whether rendering this output is currently worth doing. The backend
    /// only reports what it can observe cheaply; policy lives in the engine.
    Visibility {
        output: String,
        visible: bool,
    },
    /// A fullscreen window took over this output.
    Fullscreen {
        output: String,
        active: bool,
    },
    /// The compositor or X server went away; the daemon should exit.
    Disconnected,
}

/// A native window handle in the form EGL expects: an X11 window id widened to
/// pointer size, or a `wl_egl_window*`.
pub type NativeWindow = *mut c_void;

pub trait Backend {
    /// Short name reported over IPC: `"x11"` or `"wayland"`.
    fn name(&self) -> &'static str;

    fn platform(&self) -> Platform;

    /// Native display pointer for `eglGetPlatformDisplay`.
    ///
    /// The X11 backend returns null (`EGL_DEFAULT_DISPLAY`) on purpose: EGL
    /// opens its own connection from `$DISPLAY` rather than sharing ours, so
    /// the protocol side can stay pure Rust with no libxcb linkage. Window ids
    /// are server-side objects, so a surface created on EGL's connection for a
    /// window created on ours is perfectly legal — it only requires that we
    /// flush before handing the id over.
    fn native_display(&self) -> *mut c_void;

    fn outputs(&self) -> Vec<OutputInfo>;

    /// Creates the wallpaper window for `output` and returns its native
    /// handle, ready to be wrapped in an EGL surface.
    fn create_window(&mut self, output: &OutputInfo, egl: &Egl) -> Result<NativeWindow>;

    fn destroy_window(&mut self, output: &str) -> Result<()>;

    /// Drains pending events without blocking.
    fn dispatch(&mut self) -> Result<Vec<BackendEvent>>;

    /// Called immediately after `eglSwapBuffers`, so a backend that needs to
    /// commit or re-arm a frame callback can do it. No-op on X11.
    fn post_frame(&mut self, output: &str) -> Result<()>;

    /// File descriptor the main loop can wait on, so an idle daemon blocks in
    /// `poll` instead of spinning.
    fn poll_fd(&self) -> Option<std::os::fd::RawFd>;

    /// True when the backend drives its own pacing through frame callbacks, as
    /// Wayland does. The engine then renders on demand instead of to a timer.
    fn frame_driven(&self) -> bool {
        false
    }

    /// Whether the window system currently wants a frame for this output.
    /// Always true on X11, where nothing asks us for anything; on Wayland it
    /// is the compositor's frame callback, and the reason an occluded output
    /// costs nothing there.
    fn frame_due(&self, _output: &str) -> bool {
        true
    }
}

/// Picks a backend from the environment: Wayland when `$WAYLAND_DISPLAY` is
/// set, otherwise X11. `$WALLFLUX_BACKEND` forces one, which is how you test
/// the X11 path from inside a Wayland session running Xwayland.
pub fn autodetect() -> Result<Box<dyn Backend>> {
    let forced = std::env::var("WALLFLUX_BACKEND").ok();
    match forced.as_deref() {
        Some("x11") => return Ok(Box::new(x11::X11Backend::new()?)),
        Some("wayland") => return Ok(Box::new(wayland::WaylandBackend::new()?)),
        Some(other) => anyhow::bail!("unknown WALLFLUX_BACKEND={other:?} (use x11 or wayland)"),
        None => {}
    }

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        match wayland::WaylandBackend::new() {
            Ok(backend) => return Ok(Box::new(backend)),
            Err(err) => {
                tracing::warn!(
                    "WAYLAND_DISPLAY is set but the Wayland backend failed ({err:#}); \
                     trying X11"
                );
            }
        }
    }
    Ok(Box::new(x11::X11Backend::new()?))
}
