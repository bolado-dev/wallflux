//! The render loop and everything it owns.
//!
//! One thread, one EGL context, one mpv instance per output. A single thread
//! is a deliberate choice: the GL context can only be current on one thread at
//! a time, and the work per frame is a texture upload and a blit. Spreading
//! that across threads would buy nothing and cost a context per output.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use wallflux_core::config::{Config, PlaybackConfig};
use wallflux_core::ipc::{OutputState, PauseReason, Request, Response, Status};
use wallflux_core::library::{Library, Wallpaper};

use crate::backend::{Backend, BackendEvent, OutputInfo};
use crate::policy::{self, Decision, PauseGate, Signals};
use crate::power::PowerMonitor;
use crate::render::{Egl, Gl, Player, PlayerOptions, Surface};
use crate::wake::Waker;

/// Colour an output shows when it has no wallpaper. Black rather than the X11
/// default grey stipple, which looks like a crash.
const BLANK: [f32; 3] = [0.0, 0.0, 0.0];

/// How long a frame may be late before we stop trying to catch up. Without
/// this, a daemon that was suspended wakes up and renders a burst of frames to
/// "repay" the missed deadlines.
const MAX_FRAME_LAG: Duration = Duration::from_millis(250);

struct OutputRuntime {
    info: OutputInfo,
    surface: Surface,
    /// Created on demand: an output with no wallpaper holds no mpv instance,
    /// and therefore no decoder threads or video memory.
    player: Option<Player>,
    wallpaper: Option<String>,
    signals: Signals,
    pause_reason: Option<PauseReason>,
    /// Keeps a flickering window tree from pausing and resuming mpv twice a
    /// second. See [`PauseGate`].
    gate: PauseGate,
    /// Earliest time the next frame may be drawn, from the fps cap.
    next_frame: Instant,
    /// The configured cap, from the wallpaper manifest or the config.
    fps_cap: u32,
    /// The cap actually in force, after the power policy has had its say.
    /// Reported instead of `fps_cap` so a measured 15 against a battery cap
    /// of 15 reads as "on target" rather than "dropping frames".
    effective_fps_cap: u32,
    /// Force one frame even if mpv reports nothing new — after a resume, a
    /// resize, or a wallpaper change.
    needs_redraw: bool,
    frames: u32,
    window_start: Instant,
    measured_fps: f32,
}

impl OutputRuntime {
    fn paused(&self) -> bool {
        self.pause_reason.is_some()
    }

    fn to_state(&self) -> OutputState {
        OutputState {
            name: self.info.name.clone(),
            width: self.info.width,
            height: self.info.height,
            x: self.info.x,
            y: self.info.y,
            scale: self.info.scale,
            wallpaper: self.wallpaper.clone(),
            paused: self.paused(),
            pause_reason: self.pause_reason,
            measured_fps: self.measured_fps,
            fps_cap: self.effective_fps_cap,
        }
    }
}

pub struct Engine {
    backend: Box<dyn Backend>,
    egl: Egl,
    /// Loaded once a surface has been made current, since some drivers only
    /// resolve GL symbols after that.
    gl: OnceCell<Gl>,
    config: Config,
    library: Library,
    outputs: BTreeMap<String, OutputRuntime>,
    power: PowerMonitor,
    waker: Waker,
    started: Instant,
    shutdown: bool,
}

impl Engine {
    pub fn new(config: Config, waker: Waker) -> Result<Self> {
        let backend = crate::backend::autodetect()?;
        tracing::info!(backend = backend.name(), "backend selected");

        // SAFETY: the pointer comes from the backend we just created and lives
        // as long as it does; `Egl` is dropped before `backend` because struct
        // fields drop in declaration order.
        let egl = unsafe { Egl::new(backend.platform(), backend.native_display()) }?;

        let library = Library::load().unwrap_or_else(|err| {
            tracing::warn!(%err, "cannot read the wallpaper library");
            Library::default()
        });

        let mut engine = Self {
            backend,
            egl,
            gl: OnceCell::new(),
            config,
            library,
            outputs: BTreeMap::new(),
            power: PowerMonitor::default(),
            waker,
            started: Instant::now(),
            shutdown: false,
        };

        for info in engine.backend.outputs() {
            if let Err(err) = engine.add_output(&info) {
                tracing::error!(output = %info.name, "cannot set up output: {err:#}");
            }
        }
        Ok(engine)
    }

    pub fn should_exit(&self) -> bool {
        self.shutdown
    }

    pub fn backend_fd(&self) -> Option<std::os::fd::RawFd> {
        self.backend.poll_fd()
    }

    // --- Output lifecycle ----------------------------------------------------

    fn add_output(&mut self, info: &OutputInfo) -> Result<()> {
        if self.outputs.contains_key(&info.name) {
            return Ok(());
        }
        let native = self.backend.create_window(info, &self.egl)?;
        // SAFETY: `native` is the handle the backend just created for this
        // platform, and the backend outlives the surface (destroyed in
        // remove_output and in Drop before the backend goes away).
        let surface = unsafe { self.egl.create_surface(native, info.width, info.height) }?;
        self.egl.make_current(&surface)?;

        if self.gl.get().is_none() {
            let gl = self.egl.load_gl()?;
            let _ = self.gl.set(gl);
            // Only now, with a surface current, will a driver accept this.
            // wallflux paces itself from the fps cap, so blocking in
            // eglSwapBuffers would just make one monitor's refresh rate
            // throttle every other output on the render thread.
            if let Err(err) = self.egl.set_swap_interval(0) {
                tracing::warn!(%err, "cannot disable vsync; frame pacing may be coarse");
            }
        }
        // Paint it black before anything else: a freshly mapped window shows
        // whatever was in that memory until something draws.
        if let Some(gl) = self.gl.get() {
            gl.clear(info.width, info.height, BLANK);
        }
        self.egl.swap_buffers(&surface)?;

        let playback = self.config.playback_for(&info.name);
        self.outputs.insert(
            info.name.clone(),
            OutputRuntime {
                info: info.clone(),
                surface,
                player: None,
                wallpaper: None,
                signals: Signals {
                    power: self.power.source(),
                    has_wallpaper: false,
                    ..Default::default()
                },
                pause_reason: Some(PauseReason::Idle),
                gate: PauseGate::default(),
                next_frame: Instant::now(),
                fps_cap: playback.fps_cap,
                effective_fps_cap: playback.fps_cap,
                needs_redraw: false,
                frames: 0,
                window_start: Instant::now(),
                measured_fps: 0.0,
            },
        );
        tracing::info!(output = %info.name, width = info.width, height = info.height,
                       "output ready");

        // Apply whatever the config asks for on this output.
        if let Some(id) = self.config.wallpaper_for(&info.name).map(str::to_string)
            && let Err(err) = self.apply_wallpaper(&info.name, &id)
        {
            tracing::error!(output = %info.name, wallpaper = %id,
                            "cannot apply configured wallpaper: {err:#}");
        }
        Ok(())
    }

    fn remove_output(&mut self, name: &str) {
        if let Some(runtime) = self.outputs.remove(name) {
            // Drop mpv first: its render context holds GL objects that belong
            // to the surface we are about to destroy.
            drop(runtime.player);
            if let Some(gl) = self.gl.get() {
                let _ = self.egl.make_current(&runtime.surface);
                gl.finish();
            }
            self.egl.destroy_surface(runtime.surface);
        }
        if let Err(err) = self.backend.destroy_window(name) {
            tracing::warn!(output = %name, %err, "cannot destroy the wallpaper window");
        }
        tracing::info!(output = %name, "output removed");
    }

    /// Rebuilds an output's surface after a geometry change. Cheaper than it
    /// looks: mpv keeps playing, only the surface is replaced.
    fn resize_output(&mut self, info: &OutputInfo) -> Result<()> {
        let Some(previous) = self.outputs.get(&info.name) else {
            return self.add_output(info);
        };
        let wallpaper = previous.wallpaper.clone();
        self.remove_output(&info.name);
        self.add_output(info)?;
        if let Some(id) = wallpaper {
            self.apply_wallpaper(&info.name, &id)?;
        }
        Ok(())
    }

    // --- Wallpapers ----------------------------------------------------------

    fn player_options(&self, wallpaper: &Wallpaper, playback: &PlaybackConfig) -> PlayerOptions {
        // Per-wallpaper manifest values win over the user's defaults: the
        // author of a wallpaper knows whether it is meant to be stretched.
        let scale_mode = wallpaper.scale_mode.unwrap_or(playback.scale_mode);
        PlayerOptions {
            mute: wallpaper.mute.unwrap_or(playback.mute),
            hwdec: playback.hwdec.clone(),
            panscan: scale_mode.mpv_panscan().to_string(),
            keepaspect: scale_mode.mpv_keepaspect().to_string(),
            loop_file: true,
        }
    }

    fn apply_wallpaper(&mut self, output: &str, id: &str) -> Result<()> {
        let wallpaper = self.library.get(id).cloned().ok_or_else(|| {
            anyhow!(
                "no wallpaper with id {id:?} in {}",
                self.library.root.display()
            )
        })?;
        let playback = self.config.playback_for(output);
        let options = self.player_options(&wallpaper, &playback);

        let runtime = self
            .outputs
            .get_mut(output)
            .ok_or_else(|| anyhow!("no such output {output:?}"))?;

        // Every mpv and GL call below needs this output's context current.
        self.egl.make_current(&runtime.surface)?;

        if runtime.player.is_none() {
            let (get_proc_address, ctx) = self.egl.mpv_get_proc_address();
            let player = Player::new(&options, get_proc_address, ctx, Some(self.waker.clone()))
                .context("cannot start mpv")?;
            runtime.player = Some(player);
        }
        let player = runtime.player.as_ref().expect("player just created");
        // Options that can change without a new instance.
        player.set_property("panscan", &options.panscan)?;
        player.set_property("keepaspect", &options.keepaspect)?;
        player.set_property("mute", if options.mute { "yes" } else { "no" })?;
        player.load(&wallpaper.source)?;
        player.set_paused(false)?;

        runtime.wallpaper = Some(wallpaper.id.clone());
        runtime.signals.has_wallpaper = true;
        runtime.signals.manual = false;
        runtime.fps_cap = wallpaper.fps_cap.unwrap_or(playback.fps_cap);
        runtime.needs_redraw = true;
        runtime.next_frame = Instant::now();
        tracing::info!(output, wallpaper = %wallpaper.id, source = %wallpaper.source.display(),
                       "wallpaper applied");
        Ok(())
    }

    fn clear_wallpaper(&mut self, output: &str) -> Result<()> {
        let runtime = self
            .outputs
            .get_mut(output)
            .ok_or_else(|| anyhow!("no such output {output:?}"))?;
        self.egl.make_current(&runtime.surface)?;
        // Dropping the player releases the decoder and its buffers; stopping
        // it first keeps mpv from logging an abort on teardown.
        if let Some(player) = &runtime.player {
            let _ = player.stop();
        }
        runtime.player = None;
        runtime.wallpaper = None;
        runtime.signals.has_wallpaper = false;
        if let Some(gl) = self.gl.get() {
            gl.clear(runtime.info.width, runtime.info.height, BLANK);
        }
        self.egl.swap_buffers(&runtime.surface)?;
        tracing::info!(output, "wallpaper cleared");
        Ok(())
    }

    /// Output names a request applies to: one, or all of them.
    fn targets(&self, output: Option<&str>) -> Vec<String> {
        match output {
            Some(name) => vec![name.to_string()],
            None => self.outputs.keys().cloned().collect(),
        }
    }

    // --- Events --------------------------------------------------------------

    pub fn handle_backend_events(&mut self) -> Result<()> {
        for event in self.backend.dispatch()? {
            match event {
                BackendEvent::OutputAdded(info) => {
                    if let Err(err) = self.add_output(&info) {
                        tracing::error!(output = %info.name, "cannot add output: {err:#}");
                    }
                }
                BackendEvent::OutputChanged(info) => {
                    tracing::info!(output = %info.name, width = info.width, height = info.height,
                                   "output geometry changed");
                    if let Err(err) = self.resize_output(&info) {
                        tracing::error!(output = %info.name, "cannot resize output: {err:#}");
                    }
                }
                BackendEvent::OutputRemoved(name) => self.remove_output(&name),
                BackendEvent::Visibility { output, visible } => {
                    if let Some(runtime) = self.outputs.get_mut(&output) {
                        if runtime.signals.visible != visible {
                            tracing::debug!(output = %output, visible, "visibility changed");
                        }
                        runtime.signals.visible = visible;
                    }
                }
                BackendEvent::Fullscreen { output, active } => {
                    if let Some(runtime) = self.outputs.get_mut(&output) {
                        runtime.signals.fullscreen = active;
                    }
                }
                BackendEvent::Disconnected => {
                    tracing::warn!("the window system went away; shutting down");
                    self.shutdown = true;
                }
            }
        }
        Ok(())
    }

    pub fn handle_request(&mut self, request: Request) -> Response {
        match self.try_handle_request(request) {
            Ok(response) => response,
            Err(err) => Response::Error {
                message: format!("{err:#}"),
            },
        }
    }

    fn try_handle_request(&mut self, request: Request) -> Result<Response> {
        match request {
            Request::Ping => Ok(Response::Pong {
                version: env!("CARGO_PKG_VERSION").to_string(),
                ipc_version: wallflux_core::IPC_VERSION,
            }),

            Request::Status => Ok(Response::Status(Box::new(Status {
                version: env!("CARGO_PKG_VERSION").to_string(),
                backend: self.backend.name().to_string(),
                outputs: self.outputs.values().map(OutputRuntime::to_state).collect(),
                library_root: self.library.root.clone(),
                config_path: wallflux_core::paths::config_file()?,
                uptime_secs: self.started.elapsed().as_secs(),
            }))),

            Request::ListWallpapers => Ok(Response::Wallpapers {
                wallpapers: self.library.wallpapers.clone(),
                errors: self.library.errors.clone(),
                library_root: self.library.root.clone(),
            }),

            Request::RescanLibrary => {
                self.library = Library::load()?;
                tracing::info!(count = self.library.wallpapers.len(), "library rescanned");
                Ok(Response::Wallpapers {
                    wallpapers: self.library.wallpapers.clone(),
                    errors: self.library.errors.clone(),
                    library_root: self.library.root.clone(),
                })
            }

            Request::SetWallpaper {
                id,
                output,
                persist,
            } => {
                let targets = self.targets(output.as_deref());
                if targets.is_empty() {
                    anyhow::bail!("no outputs are available");
                }
                for name in &targets {
                    self.apply_wallpaper(name, &id)?;
                }
                if persist {
                    for name in &targets {
                        let entry = self.config.outputs.entry(name.clone()).or_default();
                        entry.wallpaper = Some(id.clone());
                        entry.disabled = false;
                    }
                    self.config.save()?;
                }
                Ok(Response::Ok)
            }

            Request::ClearWallpaper { output, persist } => {
                for name in self.targets(output.as_deref()) {
                    self.clear_wallpaper(&name)?;
                    if persist {
                        let entry = self.config.outputs.entry(name).or_default();
                        entry.wallpaper = None;
                    }
                }
                if persist {
                    self.config.save()?;
                }
                Ok(Response::Ok)
            }

            Request::Pause { output } => {
                self.set_manual_pause(output.as_deref(), Some(true));
                Ok(Response::Ok)
            }
            Request::Resume { output } => {
                self.set_manual_pause(output.as_deref(), Some(false));
                Ok(Response::Ok)
            }
            Request::TogglePause { output } => {
                self.set_manual_pause(output.as_deref(), None);
                Ok(Response::Ok)
            }

            Request::GetConfig => Ok(Response::Config(Box::new(self.config.clone()))),

            Request::SetConfig { config } => {
                self.config = *config;
                self.config.save()?;
                self.apply_config()?;
                Ok(Response::Ok)
            }

            Request::ReloadConfig => {
                self.config = Config::load()?;
                self.apply_config()?;
                tracing::info!("config reloaded");
                Ok(Response::Ok)
            }

            Request::Shutdown => {
                self.shutdown = true;
                Ok(Response::Ok)
            }
        }
    }

    fn set_manual_pause(&mut self, output: Option<&str>, paused: Option<bool>) {
        for name in self.targets(output) {
            if let Some(runtime) = self.outputs.get_mut(&name) {
                let manual = paused.unwrap_or(!runtime.signals.manual);
                runtime.signals.manual = manual;
                if !manual {
                    runtime.needs_redraw = true;
                }
            }
        }
    }

    /// Re-applies the whole config to the running outputs.
    fn apply_config(&mut self) -> Result<()> {
        let names: Vec<_> = self.outputs.keys().cloned().collect();
        for name in names {
            let wanted = self.config.wallpaper_for(&name).map(str::to_string);
            let current = self.outputs.get(&name).and_then(|rt| rt.wallpaper.clone());
            match (wanted, current) {
                (Some(id), current) if current.as_deref() != Some(id.as_str()) => {
                    if let Err(err) = self.apply_wallpaper(&name, &id) {
                        tracing::error!(output = %name, "cannot apply wallpaper: {err:#}");
                    }
                }
                (None, Some(_)) => {
                    if let Err(err) = self.clear_wallpaper(&name) {
                        tracing::error!(output = %name, "cannot clear wallpaper: {err:#}");
                    }
                }
                _ => {}
            }
            // Caps and scaling can change without touching the wallpaper.
            let playback = self.config.playback_for(&name);
            if let Some(runtime) = self.outputs.get_mut(&name) {
                runtime.fps_cap = playback.fps_cap;
                runtime.needs_redraw = true;
            }
        }
        Ok(())
    }

    // --- Rendering -----------------------------------------------------------

    /// Applies the pause policy, renders whatever is due, and reports how long
    /// the caller may sleep before the next frame is owed.
    pub fn tick(&mut self) -> Result<Option<Duration>> {
        let (power, power_changed) = self.power.poll();
        if power_changed {
            for runtime in self.outputs.values_mut() {
                runtime.signals.power = power;
                runtime.needs_redraw = true;
            }
        }

        let now = Instant::now();
        let mut next_deadline: Option<Instant> = None;
        let names: Vec<_> = self.outputs.keys().cloned().collect();

        for name in names {
            let Some(runtime) = self.outputs.get_mut(&name) else {
                continue;
            };

            // 1. Decide whether this output should be drawing at all. The
            // gate delays pauses just long enough to ignore a window tree
            // that is mid-change; resumes go through immediately.
            let wanted = policy::pause_reason(&runtime.signals, &self.config.power);
            match runtime.gate.evaluate(wanted, runtime.pause_reason, now) {
                Decision::Apply(reason) => {
                    tracing::debug!(output = %name, ?reason, "pause state changed");
                    runtime.pause_reason = reason;
                    // Tell mpv as well as skipping the draw: a paused mpv
                    // stops decoding, which is where the CPU actually goes.
                    if let Some(player) = &runtime.player {
                        let _ = player.set_paused(reason.is_some());
                    }
                    if reason.is_none() {
                        runtime.needs_redraw = true;
                        runtime.next_frame = now;
                    }
                    runtime.measured_fps = 0.0;
                }
                Decision::Wait(deadline) => {
                    // Keep rendering while we wait, and make sure the loop
                    // wakes up to re-check even if nothing else happens.
                    next_deadline = Some(match next_deadline {
                        Some(current) => current.min(deadline),
                        None => deadline,
                    });
                }
                Decision::Unchanged => {}
            }

            // 2. Drain mpv's event queue so errors reach the log.
            if let Some(player) = &runtime.player
                && !player.pump_events(&name)
            {
                tracing::warn!(output = %name, "mpv asked to shut down");
            }

            if runtime.paused() || runtime.player.is_none() {
                continue;
            }

            // 3. Pace against the effective cap.
            let cap = policy::fps_cap(runtime.fps_cap, &runtime.signals, &self.config.power);
            runtime.effective_fps_cap = cap;
            let interval = Duration::from_secs_f64(1.0 / f64::from(cap.max(1)));
            if now < runtime.next_frame {
                next_deadline = Some(match next_deadline {
                    Some(deadline) => deadline.min(runtime.next_frame),
                    None => runtime.next_frame,
                });
                continue;
            }

            // 4. Only draw when there is something new, or we owe a frame.
            //
            // On a frame-driven backend the compositor asking for a frame is
            // itself a reason to draw: it does that when a surface becomes
            // visible again, and it expects content back even if mpv is
            // sitting on the same frame as before.
            let player = runtime.player.as_ref().expect("checked above");
            let has_frame = player.frame_pending();
            let compositor_wants_one = self.backend.frame_driven() && self.backend.frame_due(&name);
            if !has_frame && !runtime.needs_redraw && !compositor_wants_one {
                continue;
            }

            self.render(&name, interval, now)?;
            if let Some(runtime) = self.outputs.get(&name) {
                next_deadline = Some(match next_deadline {
                    Some(deadline) => deadline.min(runtime.next_frame),
                    None => runtime.next_frame,
                });
            }
        }

        Ok(next_deadline.map(|deadline| deadline.saturating_duration_since(Instant::now())))
    }

    fn render(&mut self, name: &str, interval: Duration, now: Instant) -> Result<()> {
        // The compositor may not want a frame yet (Wayland); on X11 this is
        // always true and the fps cap above is the only pacing.
        if !self.backend.frame_due(name) {
            return Ok(());
        }

        let Some(runtime) = self.outputs.get_mut(name) else {
            return Ok(());
        };
        let (width, height) = runtime.surface.size();
        self.egl.make_current(&runtime.surface)?;

        match &runtime.player {
            Some(player) => {
                // flip_y: the default framebuffer's origin is bottom-left,
                // which is upside down as far as mpv is concerned.
                player.render(width, height, true)?;
                self.egl.swap_buffers(&runtime.surface)?;
                player.report_swap();
            }
            None => {
                if let Some(gl) = self.gl.get() {
                    gl.clear(width, height, BLANK);
                }
                self.egl.swap_buffers(&runtime.surface)?;
            }
        }

        runtime.needs_redraw = false;
        // Schedule from the deadline, not from now, so a steady stream of
        // frames keeps a steady rate — but never let the schedule fall more
        // than MAX_FRAME_LAG behind, or a stalled daemon would try to catch up
        // in a burst.
        runtime.next_frame = (runtime.next_frame + interval).max(now - MAX_FRAME_LAG);
        if runtime.next_frame < now {
            runtime.next_frame = now + interval;
        }

        // Rolling one-second frame counter, reported over IPC.
        runtime.frames += 1;
        let elapsed = runtime.window_start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            runtime.measured_fps = runtime.frames as f32 / elapsed.as_secs_f32();
            runtime.frames = 0;
            runtime.window_start = Instant::now();
        }

        self.backend.post_frame(name)?;
        Ok(())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Order matters: players, then surfaces, then the backend's windows.
        let names: Vec<_> = self.outputs.keys().cloned().collect();
        for name in names {
            self.remove_output(&name);
        }
    }
}

/// Sanity checks for the pure bits of the engine. The rendering path needs a
/// display server and lives in `tests/` behind an ignore.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::power::PowerSource;

    #[test]
    fn blank_is_opaque_black() {
        assert_eq!(BLANK, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn frame_lag_is_bounded_to_a_quarter_second() {
        // A longer bound would let a resumed daemon render a visible burst.
        assert!(MAX_FRAME_LAG <= Duration::from_millis(500));
    }

    #[test]
    fn an_idle_output_reports_idle_not_a_pause_the_user_asked_for() {
        let signals = Signals {
            has_wallpaper: false,
            ..Default::default()
        };
        let reason = policy::pause_reason(&signals, &wallflux_core::config::PowerConfig::default());
        assert_eq!(reason, Some(PauseReason::Idle));
    }

    #[test]
    fn power_source_default_is_mains() {
        assert_eq!(PowerMonitor::default().source(), PowerSource::Ac);
    }
}
