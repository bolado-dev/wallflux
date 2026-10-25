//! X11 backend.
//!
//! One override-redirect window per monitor, stacked at the bottom and shaped
//! to accept no input. Override-redirect means the window manager never sees
//! the window at all: it does not occupy a tiling slot, never takes focus and
//! never shows up in a taskbar. It is also marked `_NET_WM_WINDOW_TYPE_DESKTOP`
//! so compositors apply their desktop rules (no shadow, no fade) to it.
//!
//! The interesting part is [`Occlusion`]: an X server renders whatever you ask
//! it to, visible or not, so the daemon has to work out for itself when
//! drawing is pointless.

use std::collections::HashMap;
use std::ffi::c_void;

use anyhow::{Context, Result, bail};
use x11rb::connection::{Connection as _, RequestConnection as _};
use x11rb::errors::ReplyOrIdError;
use x11rb::protocol::Event;
use x11rb::protocol::randr::{self, ConnectionExt as _, NotifyMask};
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    self, AtomEnum, ChangeWindowAttributesAux, ClipOrdering, ColormapAlloc, ConfigureWindowAux,
    ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Rectangle, StackMode, Visualid,
    Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::{Backend, BackendEvent, NativeWindow, OutputInfo};
use crate::render::{Egl, Platform};

x11rb::atom_manager! {
    pub Atoms: AtomsCookie {
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        _NET_WM_NAME,
        _NET_WM_STATE,
        _NET_WM_STATE_FULLSCREEN,
        _NET_ACTIVE_WINDOW,
        _NET_WM_WINDOW_OPACITY,
        _NET_CLIENT_LIST_STACKING,
        UTF8_STRING,
    }
}

/// One wallpaper window.
struct OutputWindow {
    window: Window,
    colormap: xproto::Colormap,
    info: OutputInfo,
    /// Last reported visibility, so we only emit events on change.
    visible: bool,
    fullscreen: bool,
}

pub struct X11Backend {
    conn: RustConnection,
    root: Window,
    screen_num: usize,
    atoms: Atoms,
    windows: HashMap<String, OutputWindow>,
    outputs: Vec<OutputInfo>,
    has_shape: bool,
    has_randr: bool,
    /// Set when something in the window tree changed; the recomputation is
    /// deferred to the end of an event batch so a burst of MapNotify/
    /// ConfigureNotify during a workspace switch costs one pass, not twenty.
    occlusion_dirty: bool,
}

impl X11Backend {
    pub fn new() -> Result<Self> {
        let (conn, screen_num) = x11rb::connect(None).context(
            "cannot connect to the X server — is $DISPLAY set? \
             (set WALLFLUX_BACKEND=wayland to force the Wayland backend)",
        )?;
        let root = conn.setup().roots[screen_num].root;
        let atoms = Atoms::new(&conn)?.reply()?;

        let has_randr = conn
            .extension_information(randr::X11_EXTENSION_NAME)?
            .is_some();
        let has_shape = conn
            .extension_information(shape::X11_EXTENSION_NAME)?
            .is_some();
        if !has_shape {
            tracing::warn!(
                "the X SHAPE extension is missing, so wallpaper windows cannot be made \
                 click-through; clicks on the desktop will land on wallflux"
            );
        }

        // SubstructureNotify tells us when windows appear, move or vanish;
        // PropertyChange catches _NET_ACTIVE_WINDOW and fullscreen flips.
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::SUBSTRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE),
        )?
        .check()
        .context("cannot watch the root window for changes")?;

        if has_randr {
            randr::select_input(
                &conn,
                root,
                NotifyMask::SCREEN_CHANGE | NotifyMask::CRTC_CHANGE | NotifyMask::OUTPUT_CHANGE,
            )?
            .check()
            .context("cannot subscribe to RandR events")?;
        } else {
            tracing::warn!("RandR is missing; treating the screen as a single output");
        }

        let mut backend = Self {
            conn,
            root,
            screen_num,
            atoms,
            windows: HashMap::new(),
            outputs: Vec::new(),
            has_shape,
            has_randr,
            occlusion_dirty: true,
        };
        backend.outputs = backend.query_outputs()?;
        Ok(backend)
    }

    /// Monitors as RandR reports them, falling back to the whole screen.
    fn query_outputs(&self) -> Result<Vec<OutputInfo>> {
        if !self.has_randr {
            let screen = &self.conn.setup().roots[self.screen_num];
            return Ok(vec![OutputInfo {
                name: "screen".to_string(),
                x: 0,
                y: 0,
                width: u32::from(screen.width_in_pixels),
                height: u32::from(screen.height_in_pixels),
                scale: 1.0,
            }]);
        }

        // get_monitors(.., true) asks for active monitors only, which is what
        // "an output you can see a wallpaper on" means.
        let monitors = self
            .conn
            .randr_get_monitors(self.root, true)
            .context("RandR GetMonitors failed")?
            .reply()
            .context("RandR GetMonitors failed")?;

        // Names come back as atoms; resolve them in one batch of round trips.
        let cookies: Vec<_> = monitors
            .monitors
            .iter()
            .map(|monitor| self.conn.get_atom_name(monitor.name))
            .collect::<Result<Vec<_>, _>>()?;

        let mut outputs = Vec::with_capacity(monitors.monitors.len());
        for (monitor, cookie) in monitors.monitors.iter().zip(cookies) {
            let name = String::from_utf8_lossy(&cookie.reply()?.name).into_owned();
            if monitor.width == 0 || monitor.height == 0 {
                continue;
            }
            outputs.push(OutputInfo {
                name,
                x: i32::from(monitor.x),
                y: i32::from(monitor.y),
                width: u32::from(monitor.width),
                height: u32::from(monitor.height),
                // X11 has no per-output scale; resolution is the whole story.
                scale: 1.0,
            });
        }
        if outputs.is_empty() {
            bail!("RandR reports no active monitors");
        }
        Ok(outputs)
    }

    /// Finds the depth that goes with an EGL visual, since `create_window`
    /// needs both and a mismatch is a `BadMatch` at map time.
    fn depth_for_visual(&self, visual: Visualid) -> Result<u8> {
        let screen = &self.conn.setup().roots[self.screen_num];
        for depth in &screen.allowed_depths {
            if depth.visuals.iter().any(|v| v.visual_id == visual) {
                return Ok(depth.depth);
            }
        }
        bail!("the X server has no depth for EGL visual 0x{visual:x}")
    }

    fn set_window_properties(&self, window: Window, output: &str) -> Result<()> {
        let title = format!("wallflux ({output})");
        self.conn.change_property32(
            PropMode::REPLACE,
            window,
            self.atoms._NET_WM_WINDOW_TYPE,
            AtomEnum::ATOM,
            &[self.atoms._NET_WM_WINDOW_TYPE_DESKTOP],
        )?;
        self.conn.change_property8(
            PropMode::REPLACE,
            window,
            self.atoms._NET_WM_NAME,
            self.atoms.UTF8_STRING,
            title.as_bytes(),
        )?;
        self.conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        )?;
        // WM_CLASS is two NUL-terminated strings: instance then class. Picom
        // and friends match on it, so `class-g = "wallflux"` rules work.
        self.conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"wallflux\0wallflux\0",
        )?;
        Ok(())
    }

    /// Removes the window's input region so pointer events fall through to
    /// whatever is underneath — the root window, normally.
    fn make_click_through(&self, window: Window) -> Result<()> {
        if !self.has_shape {
            return Ok(());
        }
        // An empty rectangle list means "no input region at all".
        let empty: [Rectangle; 0] = [];
        self.conn
            .shape_rectangles(
                shape::SO::SET,
                shape::SK::INPUT,
                ClipOrdering::UNSORTED,
                window,
                0,
                0,
                &empty,
            )?
            .check()
            .context("cannot clear the window input region")?;
        Ok(())
    }

    fn create_output_window(&self, output: &OutputInfo, egl: &Egl) -> Result<(Window, u32)> {
        let visual = egl.native_visual_id() as Visualid;
        let depth = self.depth_for_visual(visual)?;

        let window = self.conn.generate_id()?;
        let colormap = self.conn.generate_id()?;
        self.conn
            .create_colormap(ColormapAlloc::NONE, colormap, self.root, visual)?
            .check()
            .context("cannot create a colormap for the EGL visual")?;

        let aux = CreateWindowAux::new()
            // The WM must not manage this window: no tiling slot, no focus,
            // no decorations, no workspace.
            .override_redirect(1)
            .colormap(colormap)
            // Required whenever depth differs from the root's, and harmless
            // when it does not.
            .border_pixel(0)
            .background_pixel(0)
            // VisibilityChange is a cheap hint from the server; the geometry
            // pass in `Occlusion` is the authority.
            .event_mask(EventMask::VISIBILITY_CHANGE | EventMask::STRUCTURE_NOTIFY);

        self.conn
            .create_window(
                depth,
                window,
                self.root,
                output.x as i16,
                output.y as i16,
                output.width as u16,
                output.height as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                visual,
                &aux,
            )
            .map_err(ReplyOrIdError::from)
            .context("cannot create the wallpaper window")?;

        self.set_window_properties(window, &output.name)?;
        self.make_click_through(window)?;
        self.conn.map_window(window)?;
        // Below everything else. Override-redirect windows are not restacked
        // by the WM, so this sticks.
        self.conn.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::BELOW),
        )?;
        // EGL runs on its own connection, so the window must exist on the
        // server before we hand over its id.
        self.conn.flush()?;
        Ok((window, colormap))
    }

    /// Recomputes, for every output, whether a wallpaper frame would be seen.
    fn recompute_occlusion(&mut self) -> Result<Vec<BackendEvent>> {
        self.occlusion_dirty = false;
        let report = Occlusion::probe(
            &self.conn,
            self.root,
            &self.atoms,
            &self.outputs,
            &self.own_windows(),
        )?;

        let mut events = Vec::new();
        for window in self.windows.values_mut() {
            let occluded = report.occluded.contains(&window.info.name);
            let fullscreen = report.fullscreen.contains(&window.info.name);
            let visible = !occluded;
            if visible != window.visible {
                window.visible = visible;
                events.push(BackendEvent::Visibility {
                    output: window.info.name.clone(),
                    visible,
                });
            }
            if fullscreen != window.fullscreen {
                window.fullscreen = fullscreen;
                events.push(BackendEvent::Fullscreen {
                    output: window.info.name.clone(),
                    active: fullscreen,
                });
            }
        }
        Ok(events)
    }

    fn own_windows(&self) -> Vec<Window> {
        self.windows.values().map(|w| w.window).collect()
    }

    fn window_for(&self, window: Window) -> Option<&OutputWindow> {
        self.windows.values().find(|w| w.window == window)
    }
}

impl Backend for X11Backend {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn platform(&self) -> Platform {
        Platform::X11
    }

    fn native_display(&self) -> *mut c_void {
        // EGL_DEFAULT_DISPLAY: let EGL open its own connection from $DISPLAY.
        // See the trait docs for why we do not share ours.
        std::ptr::null_mut()
    }

    fn outputs(&self) -> Vec<OutputInfo> {
        self.outputs.clone()
    }

    fn create_window(&mut self, output: &OutputInfo, egl: &Egl) -> Result<NativeWindow> {
        if let Some(existing) = self.windows.get(&output.name) {
            return Ok(existing.window as usize as NativeWindow);
        }
        let (window, colormap) = self.create_output_window(output, egl)?;
        self.windows.insert(
            output.name.clone(),
            OutputWindow {
                window,
                colormap,
                info: output.clone(),
                visible: true,
                fullscreen: false,
            },
        );
        self.occlusion_dirty = true;
        tracing::info!(output = %output.name, window, "wallpaper window created");
        // An X11 window id is an unsigned 32-bit server-side handle; EGL's
        // X11 platform takes it in a pointer-sized slot.
        Ok(window as usize as NativeWindow)
    }

    fn destroy_window(&mut self, output: &str) -> Result<()> {
        if let Some(window) = self.windows.remove(output) {
            self.conn.destroy_window(window.window)?;
            self.conn.free_colormap(window.colormap)?;
            self.conn.flush()?;
        }
        Ok(())
    }

    fn dispatch(&mut self) -> Result<Vec<BackendEvent>> {
        let mut events = Vec::new();
        let mut outputs_changed = false;

        while let Some(event) = self
            .conn
            .poll_for_event()
            .context("lost the connection to the X server")?
        {
            match event {
                Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => outputs_changed = true,

                // Any change in the window tree can change what covers an
                // output, including a window simply moving.
                Event::MapNotify(_)
                | Event::UnmapNotify(_)
                | Event::DestroyNotify(_)
                | Event::ConfigureNotify(_)
                | Event::ReparentNotify(_) => self.occlusion_dirty = true,

                Event::PropertyNotify(notify) => {
                    if notify.atom == self.atoms._NET_ACTIVE_WINDOW
                        || notify.atom == self.atoms._NET_CLIENT_LIST_STACKING
                        || notify.atom == self.atoms._NET_WM_STATE
                        || notify.atom == self.atoms._NET_WM_WINDOW_OPACITY
                    {
                        self.occlusion_dirty = true;
                    }
                }

                // The server's own verdict. Cheap, and a useful cross-check,
                // but it is unreliable under a compositor that redirects
                // windows offscreen, so it only marks the state dirty.
                Event::VisibilityNotify(notify) => {
                    if self.window_for(notify.window).is_some() {
                        self.occlusion_dirty = true;
                    }
                }

                Event::Error(err) => {
                    tracing::debug!(?err, "X11 error event");
                }
                _ => {}
            }
        }

        if outputs_changed {
            let fresh = self.query_outputs()?;
            if fresh != self.outputs {
                let old = std::mem::replace(&mut self.outputs, fresh);
                for output in &self.outputs {
                    match old.iter().find(|o| o.name == output.name) {
                        None => events.push(BackendEvent::OutputAdded(output.clone())),
                        Some(previous) if previous != output => {
                            events.push(BackendEvent::OutputChanged(output.clone()));
                        }
                        Some(_) => {}
                    }
                }
                for output in &old {
                    if !self.outputs.iter().any(|o| o.name == output.name) {
                        events.push(BackendEvent::OutputRemoved(output.name.clone()));
                    }
                }
                self.occlusion_dirty = true;
            }
        }

        if self.occlusion_dirty {
            events.extend(self.recompute_occlusion()?);
        }
        Ok(events)
    }

    fn post_frame(&mut self, _output: &str) -> Result<()> {
        // Nothing to do: eglSwapBuffers is the whole story on X11.
        Ok(())
    }

    fn poll_fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        Some(self.conn.stream().as_raw_fd())
    }
}

/// Which outputs are covered, and which have a fullscreen window on them.
///
/// The test is deliberately conservative: an output counts as occluded only
/// when a *single* mapped, opaque window covers it completely. A union of
/// several windows is not treated as coverage.
///
/// That is the right call under a tiling WM rather than a limitation to
/// apologise for. bspwm with gaps leaves the wallpaper visible between
/// windows, so "two windows together cover the screen" usually means the
/// wallpaper is still partly on show. The cases that matter — a maximised
/// window, a monocle layout, a fullscreen video — are all single windows.
pub struct Occlusion {
    pub occluded: Vec<String>,
    pub fullscreen: Vec<String>,
}

impl Occlusion {
    fn probe(
        conn: &RustConnection,
        root: Window,
        atoms: &Atoms,
        outputs: &[OutputInfo],
        own: &[Window],
    ) -> Result<Self> {
        let tree = conn.query_tree(root)?.reply()?;

        // Pipeline the round trips: ask for everything, then read everything.
        // Doing it window-by-window would be one blocking round trip each,
        // and this runs on every workspace switch.
        let mut pending = Vec::with_capacity(tree.children.len());
        for &child in &tree.children {
            if own.contains(&child) {
                continue;
            }
            pending.push((
                child,
                conn.get_window_attributes(child)?,
                conn.get_geometry(child)?,
                conn.get_property(
                    false,
                    child,
                    atoms._NET_WM_WINDOW_OPACITY,
                    AtomEnum::CARDINAL,
                    0,
                    1,
                )?,
            ));
        }

        let active = conn
            .get_property(
                false,
                root,
                atoms._NET_ACTIVE_WINDOW,
                AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()
            .ok()
            .and_then(|reply| reply.value32().and_then(|mut values| values.next()))
            .filter(|&window| window != 0);

        let mut covers: Vec<(Window, Rect)> = Vec::new();
        for (window, attributes, geometry, opacity) in pending {
            // A window that is unmapped, or mapped but not viewable because an
            // ancestor is unmapped, hides nothing. Reply errors are normal
            // here: windows die between the query and the reply.
            let Ok(attributes) = attributes.reply() else {
                continue;
            };
            if attributes.map_state != xproto::MapState::VIEWABLE {
                continue;
            }
            // InputOnly windows have no pixels at all, so they cannot hide
            // anything however big they are. This matters more than it sounds:
            // bspwm covers each monitor with an InputOnly "root" window and
            // the whole screen with an InputOnly pointer-motion recorder, and
            // without this check wallflux decides it is permanently occluded
            // and never draws a frame.
            if attributes.class == xproto::WindowClass::INPUT_ONLY {
                let _ = geometry.reply();
                let _ = opacity.reply();
                continue;
            }
            // Other override-redirect windows are usually tooltips, menus and
            // our own kind. None of them should stand in for coverage.
            if attributes.override_redirect {
                let _ = geometry.reply();
                let _ = opacity.reply();
                continue;
            }
            let Ok(geometry) = geometry.reply() else {
                continue;
            };
            // A translucent window shows the wallpaper through it, so a
            // compositor's opacity rules must not fool us into pausing.
            if let Ok(reply) = opacity.reply()
                && let Some(value) = reply.value32().and_then(|mut values| values.next())
                && value < u32::MAX
            {
                continue;
            }

            let border = i32::from(geometry.border_width);
            covers.push((
                window,
                Rect {
                    x: i32::from(geometry.x) - border,
                    y: i32::from(geometry.y) - border,
                    width: u32::from(geometry.width) + 2 * border as u32,
                    height: u32::from(geometry.height) + 2 * border as u32,
                },
            ));
        }

        let fullscreen_window = match active {
            Some(window) => is_fullscreen(conn, atoms, window)
                .unwrap_or(false)
                .then_some(window),
            None => None,
        };

        let mut occluded = Vec::new();
        let mut fullscreen = Vec::new();
        for output in outputs {
            let area = Rect {
                x: output.x,
                y: output.y,
                width: output.width,
                height: output.height,
            };
            if covers.iter().any(|(_, rect)| rect.contains(&area)) {
                occluded.push(output.name.clone());
            }
            if let Some(window) = fullscreen_window
                && let Some((_, rect)) = covers.iter().find(|(id, _)| *id == window)
                && rect.overlaps_mostly(&area)
            {
                fullscreen.push(output.name.clone());
            }
        }
        Ok(Self {
            occluded,
            fullscreen,
        })
    }
}

fn is_fullscreen(conn: &RustConnection, atoms: &Atoms, window: Window) -> Result<bool> {
    let reply = conn
        .get_property(false, window, atoms._NET_WM_STATE, AtomEnum::ATOM, 0, 32)?
        .reply()?;
    Ok(reply
        .value32()
        .map(|mut values| values.any(|atom| atom == atoms._NET_WM_STATE_FULLSCREEN))
        .unwrap_or(false))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl Rect {
    fn right(&self) -> i32 {
        self.x + self.width as i32
    }

    fn bottom(&self) -> i32 {
        self.y + self.height as i32
    }

    /// True when `self` covers every pixel of `other`.
    fn contains(&self, other: &Rect) -> bool {
        self.x <= other.x
            && self.y <= other.y
            && self.right() >= other.right()
            && self.bottom() >= other.bottom()
    }

    /// True when the overlap is most of `other`. Used for "which output is
    /// this fullscreen window on", where exact containment is too strict: a
    /// fullscreen window can be one pixel off, or span a shared edge.
    fn overlaps_mostly(&self, other: &Rect) -> bool {
        let overlap_w = (self.right().min(other.right()) - self.x.max(other.x)).max(0) as u64;
        let overlap_h = (self.bottom().min(other.bottom()) - self.y.max(other.y)).max(0) as u64;
        let overlap = overlap_w * overlap_h;
        let area = u64::from(other.width) * u64::from(other.height);
        area > 0 && overlap * 2 > area
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: Rect = Rect {
        x: 0,
        y: 0,
        width: 2880,
        height: 1800,
    };

    #[test]
    fn exact_cover_occludes() {
        assert!(OUTPUT.contains(&OUTPUT));
    }

    #[test]
    fn a_window_with_gaps_does_not_occlude() {
        // bspwm with window_gap = 20: the wallpaper shows around the edges.
        let tiled = Rect {
            x: 20,
            y: 20,
            width: 2840,
            height: 1760,
        };
        assert!(!tiled.contains(&OUTPUT));
    }

    #[test]
    fn an_oversized_window_occludes() {
        let big = Rect {
            x: -10,
            y: -10,
            width: 3000,
            height: 1900,
        };
        assert!(big.contains(&OUTPUT));
    }

    #[test]
    fn a_window_on_another_monitor_does_not_occlude() {
        let other = Rect {
            x: 2880,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert!(!other.contains(&OUTPUT));
        assert!(!other.overlaps_mostly(&OUTPUT));
    }

    #[test]
    fn mostly_overlapping_counts_for_fullscreen() {
        let nearly = Rect {
            x: 1,
            y: 1,
            width: 2879,
            height: 1799,
        };
        assert!(nearly.overlaps_mostly(&OUTPUT));
        // ...but not for occlusion, which demands every pixel.
        assert!(!nearly.contains(&OUTPUT));
    }

    #[test]
    fn input_only_is_the_class_we_skip() {
        // Guards the fix for bspwm's screen-sized InputOnly helpers, which had
        // wallflux convinced it was permanently occluded. If this constant
        // ever changes meaning, the occlusion pass silently breaks.
        assert_eq!(u16::from(xproto::WindowClass::INPUT_ONLY), 2);
        assert_ne!(
            xproto::WindowClass::INPUT_ONLY,
            xproto::WindowClass::INPUT_OUTPUT
        );
    }

    #[test]
    fn half_overlap_is_not_mostly() {
        let half = Rect {
            x: 1440,
            y: 0,
            width: 1440,
            height: 1800,
        };
        assert!(!half.overlaps_mostly(&OUTPUT));
    }
}
