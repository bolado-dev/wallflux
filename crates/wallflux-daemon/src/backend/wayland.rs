//! Wayland backend, built on `wlr-layer-shell`.
//!
//! A wallpaper is a surface on the **background** layer with a zero exclusive
//! zone, anchored to all four edges of one output. The compositor then owns
//! the hard parts: the surface sits under every window, cannot take input
//! (we commit a null input region), and follows the output's geometry.
//!
//! The payoff compared to X11 is pacing. The compositor only sends a frame
//! callback when it actually needs a new frame, so a wallpaper hidden behind
//! a maximised window stops being asked to draw and the daemon's CPU use
//! falls to nothing — without the geometry heuristics the X11 backend needs.
//! This is why [`Backend::frame_driven`] exists.
//!
//! Requires a compositor implementing `zwlr_layer_shell_v1`: sway, Hyprland,
//! river, Wayfire, labwc and the rest of the wlroots family. GNOME does not
//! implement it, and there is no portable alternative; the daemon says so and
//! exits rather than pretending.

use std::collections::HashMap;
use std::ffi::{c_int, c_void};

use anyhow::{Context, Result, anyhow, bail};
use libloading::Library;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_output::WlOutput,
    wl_region::WlRegion, wl_registry::WlRegistry, wl_shm::WlShm, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use super::{Backend, BackendEvent, NativeWindow, OutputInfo};
use crate::render::{Egl, Platform};

/// `libwayland-egl` entry points, loaded dynamically for the same reasons as
/// libmpv and libEGL.
struct WaylandEgl {
    _library: Library,
    create: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> *mut c_void,
    destroy: unsafe extern "C" fn(*mut c_void),
    resize: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_int),
}

impl WaylandEgl {
    fn load() -> Result<Self> {
        // SAFETY: dlopens a fixed soname and reads three documented symbols.
        unsafe {
            let library = Library::new("libwayland-egl.so.1")
                .or_else(|_| Library::new("libwayland-egl.so"))
                .map_err(|err| {
                    anyhow!(
                        "cannot load libwayland-egl ({err}) — install it \
                         (Debian/Kali: `sudo apt install libwayland-egl1`)"
                    )
                })?;
            let create = *library
                .get(b"wl_egl_window_create\0")
                .context("libwayland-egl is missing wl_egl_window_create")?;
            let destroy = *library
                .get(b"wl_egl_window_destroy\0")
                .context("libwayland-egl is missing wl_egl_window_destroy")?;
            let resize = *library
                .get(b"wl_egl_window_resize\0")
                .context("libwayland-egl is missing wl_egl_window_resize")?;
            Ok(Self {
                _library: library,
                create,
                destroy,
                resize,
            })
        }
    }
}

/// Per-output Wayland objects. Dropping this tears the surface down in the
/// order the protocol wants: EGL window, then layer surface, then surface.
struct OutputSurface {
    /// Held so the proxy outlives the layer surface bound to it.
    _output: WlOutput,
    surface: WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    /// `wl_egl_window*`, non-null once the first configure has arrived.
    egl_window: *mut c_void,
    width: u32,
    height: u32,
    /// Set when the compositor asks for a frame; cleared when we draw one.
    frame_due: bool,
    /// True while a frame callback is outstanding, so we never queue two.
    frame_requested: bool,
    configured: bool,
}

/// What a `wl_output` told us so far. The protocol delivers geometry, mode,
/// scale and name as separate events, finished by `done`, so the fields fill
/// in gradually and only become usable as a set.
#[derive(Debug, Clone, Default)]
struct OutputDraft {
    name: Option<String>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    scale: f64,
    done: bool,
}

impl OutputDraft {
    fn to_info(&self) -> Option<OutputInfo> {
        if !self.done || self.width == 0 || self.height == 0 {
            return None;
        }
        Some(OutputInfo {
            // A compositor that predates wl_output v4 sends no name; fall back
            // to something stable so config keys still work.
            name: self.name.clone().unwrap_or_else(|| "wayland-0".to_string()),
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            scale: if self.scale > 0.0 { self.scale } else { 1.0 },
        })
    }
}

/// Dispatch state. Separate from the backend so the borrow checker lets us
/// dispatch events while holding the backend's other fields.
#[derive(Default)]
struct State {
    drafts: HashMap<u32, OutputDraft>,
    outputs: HashMap<u32, OutputInfo>,
    /// Live `wl_output` proxies, needed to bind a layer surface to an output.
    output_proxies: HashMap<u32, WlOutput>,
    /// `(output id, width, height)` configures to apply.
    configures: Vec<(u32, u32, u32)>,
    /// Surfaces whose compositor asked them to go away.
    closed: Vec<u32>,
    /// Outputs with a frame callback that just fired.
    frames: Vec<u32>,
    events: Vec<BackendEvent>,
}

pub struct WaylandBackend {
    connection: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    compositor: WlCompositor,
    layer_shell: ZwlrLayerShellV1,
    wl_egl: WaylandEgl,
    /// Keyed by output name, like every other map in wallflux.
    surfaces: HashMap<String, OutputSurface>,
    /// wl_output protocol id per output name, to match events to surfaces.
    output_ids: HashMap<String, u32>,
}

impl WaylandBackend {
    pub fn new() -> Result<Self> {
        let connection = Connection::connect_to_env()
            .context("cannot connect to the Wayland compositor ($WAYLAND_DISPLAY)")?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection)
            .context("cannot read the Wayland registry")?;
        let qh = queue.handle();

        let compositor: WlCompositor = globals
            .bind(&qh, 4..=6, ())
            .context("the compositor does not expose wl_compositor v4+")?;
        let layer_shell: ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).map_err(|err| {
            anyhow!(
                "this compositor does not implement zwlr_layer_shell_v1 ({err}), which \
                 wallflux needs to place a wallpaper below your windows.\n\
                 Supported: sway, Hyprland, river, Wayfire, labwc and other wlroots \
                 compositors. GNOME does not implement it."
            )
        })?;

        let mut state = State::default();
        // Bind every output the registry already knows about. New ones arrive
        // as registry global events later.
        for global in globals.contents().clone_list() {
            if global.interface == WlOutput::interface().name {
                let version = global.version.min(4);
                let _: WlOutput = globals
                    .registry()
                    .bind(global.name, version, &qh, global.name);
                state.drafts.insert(global.name, OutputDraft::default());
            }
        }

        // Two round trips: one for the binds above, one for the burst of
        // geometry/mode/name/done events they trigger.
        queue.roundtrip(&mut state)?;
        queue.roundtrip(&mut state)?;

        let wl_egl = WaylandEgl::load()?;

        Ok(Self {
            connection,
            queue,
            qh,
            state,
            compositor,
            layer_shell,
            wl_egl,
            surfaces: HashMap::new(),
            output_ids: HashMap::new(),
        })
    }

    fn output_by_id(&self, id: u32) -> Option<&OutputInfo> {
        self.state.outputs.get(&id)
    }

    fn name_for_id(&self, id: u32) -> Option<String> {
        self.state
            .outputs
            .get(&id)
            .map(|output| output.name.clone())
    }

    /// Asks for one frame callback, which is how a Wayland client says "I have
    /// something to draw, tell me when you want it".
    fn request_frame(&mut self, name: &str) {
        if let Some(surface) = self.surfaces.get_mut(name)
            && !surface.frame_requested
        {
            let Some(&id) = self.output_ids.get(name) else {
                return;
            };
            surface.surface.frame(&self.qh, id);
            surface.frame_requested = true;
            surface.surface.commit();
        }
    }

    /// Marks a frame as consumed and queues the next callback.
    pub fn take_frame(&mut self, name: &str) {
        if let Some(surface) = self.surfaces.get_mut(name) {
            surface.frame_due = false;
        }
    }
}

impl Backend for WaylandBackend {
    fn name(&self) -> &'static str {
        "wayland"
    }

    fn platform(&self) -> Platform {
        Platform::Wayland
    }

    fn native_display(&self) -> *mut c_void {
        self.connection.backend().display_ptr() as *mut c_void
    }

    fn outputs(&self) -> Vec<OutputInfo> {
        let mut outputs: Vec<_> = self.state.outputs.values().cloned().collect();
        outputs.sort_by(|a, b| a.name.cmp(&b.name));
        outputs
    }

    fn create_window(&mut self, output: &OutputInfo, _egl: &Egl) -> Result<NativeWindow> {
        if let Some(existing) = self.surfaces.get(&output.name)
            && !existing.egl_window.is_null()
        {
            return Ok(existing.egl_window);
        }

        let id = *self
            .state
            .outputs
            .iter()
            .find(|(_, info)| info.name == output.name)
            .map(|(id, _)| id)
            .ok_or_else(|| anyhow!("output {} disappeared", output.name))?;
        let wl_output = self
            .state
            .output_proxies
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow!("no wl_output proxy for {}", output.name))?;

        let surface = self.compositor.create_surface(&self.qh, ());
        let layer_surface = self.layer_shell.get_layer_surface(
            &surface,
            Some(&wl_output),
            Layer::Background,
            "wallflux".to_string(),
            &self.qh,
            id,
        );

        // Anchoring to all four edges with a zero size means "the whole
        // output, whatever its size is"; the compositor tells us the pixels in
        // the configure event.
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_size(0, 0);
        // A wallpaper must not push other surfaces around, so it claims no
        // exclusive zone, and -1 keeps it under exclusive zones others claim
        // (a panel's reserved strip still covers us, as it should).
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);

        // An empty input region makes the surface click-through, the Wayland
        // equivalent of the X11 SHAPE trick.
        let region = self.compositor.create_region(&self.qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();

        surface.commit();

        self.output_ids.insert(output.name.clone(), id);
        self.surfaces.insert(
            output.name.clone(),
            OutputSurface {
                _output: wl_output,
                surface,
                layer_surface,
                egl_window: std::ptr::null_mut(),
                width: output.width,
                height: output.height,
                frame_due: true,
                frame_requested: false,
                configured: false,
            },
        );

        // Block until the compositor has sized the surface: creating a
        // wl_egl_window before the first configure is a protocol error.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            self.queue.blocking_dispatch(&mut self.state)?;
            self.apply_configures()?;
            if let Some(surface) = self.surfaces.get(&output.name)
                && surface.configured
            {
                break;
            }
        }

        let surface = self
            .surfaces
            .get_mut(&output.name)
            .ok_or_else(|| anyhow!("surface for {} vanished", output.name))?;
        if !surface.configured {
            bail!(
                "the compositor never configured the layer surface for {}",
                output.name
            );
        }

        // SAFETY: the surface is configured, and the proxy pointer is valid
        // for as long as `surface.surface` lives, which outlives the window
        // (dropped together in destroy_window).
        let egl_window = unsafe {
            (self.wl_egl.create)(
                surface.surface.id().as_ptr() as *mut c_void,
                surface.width as c_int,
                surface.height as c_int,
            )
        };
        if egl_window.is_null() {
            bail!("wl_egl_window_create failed for {}", output.name);
        }
        surface.egl_window = egl_window;
        tracing::info!(output = %output.name, width = surface.width, height = surface.height,
                       "layer surface ready");
        Ok(egl_window)
    }

    fn destroy_window(&mut self, output: &str) -> Result<()> {
        if let Some(surface) = self.surfaces.remove(output) {
            if !surface.egl_window.is_null() {
                // SAFETY: the pointer came from wl_egl_window_create and is
                // destroyed exactly once, before its wl_surface.
                unsafe { (self.wl_egl.destroy)(surface.egl_window) };
            }
            surface.layer_surface.destroy();
            surface.surface.destroy();
            self.connection.flush()?;
        }
        self.output_ids.remove(output);
        Ok(())
    }

    fn dispatch(&mut self) -> Result<Vec<BackendEvent>> {
        self.connection.flush()?;
        // Read whatever has already arrived without blocking; the main loop
        // owns the waiting.
        if let Some(guard) = self.connection.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(err))
                    if err.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(err) => {
                    tracing::error!(%err, "lost the Wayland connection");
                    return Ok(vec![BackendEvent::Disconnected]);
                }
            }
        }
        self.queue.dispatch_pending(&mut self.state)?;

        self.apply_configures()?;

        let mut events = std::mem::take(&mut self.state.events);

        for id in std::mem::take(&mut self.state.frames) {
            if let Some(name) = self.name_for_id(id)
                && let Some(surface) = self.surfaces.get_mut(&name)
            {
                surface.frame_requested = false;
                surface.frame_due = true;
            }
        }

        for id in std::mem::take(&mut self.state.closed) {
            if let Some(name) = self.name_for_id(id) {
                tracing::info!(output = %name, "compositor closed the layer surface");
                self.destroy_window(&name)?;
                events.push(BackendEvent::OutputRemoved(name));
            }
        }

        Ok(events)
    }

    fn post_frame(&mut self, output: &str) -> Result<()> {
        // Consume the frame we were given and ask for the next one. Without
        // this the compositor stops driving us after the first frame.
        self.take_frame(output);
        self.request_frame(output);
        self.connection.flush()?;
        Ok(())
    }

    fn poll_fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::{AsFd, AsRawFd};
        Some(self.connection.as_fd().as_raw_fd())
    }

    fn frame_driven(&self) -> bool {
        true
    }

    fn frame_due(&self, output: &str) -> bool {
        self.surfaces
            .get(output)
            .map(|surface| surface.configured && surface.frame_due)
            .unwrap_or(false)
    }
}

impl WaylandBackend {
    /// Applies pending `configure` events: acknowledge the serial, resize the
    /// EGL window, and report the new geometry to the engine.
    fn apply_configures(&mut self) -> Result<()> {
        for (id, width, height) in std::mem::take(&mut self.state.configures) {
            let Some(name) = self.name_for_id(id) else {
                continue;
            };
            let Some(surface) = self.surfaces.get_mut(&name) else {
                continue;
            };
            let changed = surface.width != width || surface.height != height;
            surface.width = width.max(1);
            surface.height = height.max(1);
            surface.configured = true;
            if !surface.egl_window.is_null() && changed {
                // SAFETY: a live wl_egl_window from create_window; resizing is
                // the documented way to follow a configure.
                unsafe {
                    (self.wl_egl.resize)(
                        surface.egl_window,
                        surface.width as c_int,
                        surface.height as c_int,
                        0,
                        0,
                    )
                };
            }
            if changed && let Some(info) = self.output_by_id(id).cloned() {
                self.state
                    .events
                    .push(BackendEvent::OutputChanged(OutputInfo {
                        width,
                        height,
                        ..info
                    }));
            }
        }
        Ok(())
    }
}

impl Drop for WaylandBackend {
    fn drop(&mut self) {
        let names: Vec<_> = self.surfaces.keys().cloned().collect();
        for name in names {
            let _ = self.destroy_window(&name);
        }
    }
}

// --- Dispatch ----------------------------------------------------------------

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wayland_client::protocol::wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_registry::Event;
        match event {
            Event::Global {
                name,
                interface,
                version,
            } if interface == WlOutput::interface().name => {
                let _: WlOutput = registry.bind(name, version.min(4), qh, name);
                state.drafts.insert(name, OutputDraft::default());
            }
            Event::GlobalRemove { name } => {
                state.drafts.remove(&name);
                if let Some(output) = state.outputs.remove(&name) {
                    state.events.push(BackendEvent::OutputRemoved(output.name));
                }
                state.output_proxies.remove(&name);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        output: &WlOutput,
        event: wayland_client::protocol::wl_output::Event,
        &id: &u32,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_output::Event;
        state.output_proxies.insert(id, output.clone());
        let draft = state.drafts.entry(id).or_default();
        match event {
            Event::Geometry { x, y, .. } => {
                draft.x = x;
                draft.y = y;
            }
            Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                // Only the current mode describes the output we draw on.
                if flags
                    .into_result()
                    .map(|flags| flags.contains(wayland_client::protocol::wl_output::Mode::Current))
                    .unwrap_or(false)
                {
                    draft.width = width.max(0) as u32;
                    draft.height = height.max(0) as u32;
                }
            }
            Event::Scale { factor } => draft.scale = f64::from(factor),
            Event::Name { name } => draft.name = Some(name),
            Event::Done => {
                draft.done = true;
                if let Some(info) = draft.to_info() {
                    let previous = state.outputs.insert(id, info.clone());
                    match previous {
                        None => state.events.push(BackendEvent::OutputAdded(info)),
                        Some(old) if old != info => {
                            state.events.push(BackendEvent::OutputChanged(info));
                        }
                        Some(_) => {}
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, u32> for State {
    fn event(
        state: &mut Self,
        layer_surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        &id: &u32,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                // Acknowledging is mandatory; the compositor will not show the
                // surface until we do.
                layer_surface.ack_configure(serial);
                state.configures.push((id, width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed.push(id),
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, u32> for State {
    fn event(
        state: &mut Self,
        _callback: &WlCallback,
        event: wayland_client::protocol::wl_callback::Event,
        &id: &u32,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = event {
            state.frames.push(id);
        }
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlRegion);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore ZwlrLayerShellV1);
