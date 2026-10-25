//! A thin, hand-written binding to the pieces of libmpv wallflux needs.
//!
//! Two deliberate choices here.
//!
//! **Dynamic loading.** libmpv is opened with `dlopen` at startup instead of
//! being linked at build time. The daemon therefore builds without
//! `libmpv-dev` present, ships as a binary that works against whatever
//! `libmpv.so.2` the distro provides, and reports a readable error
//! ("install libmpv2") rather than failing to start with a loader message.
//!
//! **The render API, not a window.** `mpv_render_context_render` draws into a
//! framebuffer *we* own, which is the whole reason the X11 and Wayland
//! backends can share one renderer: each backend only has to hand over an EGL
//! surface. The alternative, `--wid` embedding, is X11-only.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use libloading::Library;

/// Candidates tried in order. The unversioned name comes last: it only exists
/// when the -dev package is installed, and we prefer the runtime soname.
const SONAMES: &[&str] = &["libmpv.so.2", "libmpv.so.1", "libmpv.so"];

// --- Raw types ---------------------------------------------------------------

#[repr(C)]
struct MpvHandle {
    _opaque: [u8; 0],
}

#[repr(C)]
struct MpvRenderContext {
    _opaque: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MpvRenderParam {
    type_: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvOpenglInitParams {
    get_proc_address:
        Option<unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void>,
    get_proc_address_ctx: *mut c_void,
    /// Removed from mpv's header in 0.36 but harmless to keep: older builds
    /// read it as the deprecated `extra_exts` and a null pointer means "none".
    /// Passing a struct that is one pointer too long is safe; one too short
    /// would not be.
    _legacy_extra_exts: *const c_char,
}

#[repr(C)]
struct MpvOpenglFbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventLogMessage {
    prefix: *const c_char,
    level: *const c_char,
    text: *const c_char,
    log_level: c_int,
}

// mpv_render_param_type
const PARAM_INVALID: c_int = 0;
const PARAM_API_TYPE: c_int = 1;
const PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const PARAM_OPENGL_FBO: c_int = 3;
const PARAM_FLIP_Y: c_int = 4;
const PARAM_ADVANCED_CONTROL: c_int = 10;

// mpv_event_id
const EVENT_NONE: c_int = 0;
const EVENT_SHUTDOWN: c_int = 1;
const EVENT_LOG_MESSAGE: c_int = 2;
const EVENT_END_FILE: c_int = 7;
const EVENT_FILE_LOADED: c_int = 8;

/// `MPV_RENDER_UPDATE_FRAME`: a new frame is ready to be drawn.
const UPDATE_FRAME: u64 = 1;

// --- Symbol table ------------------------------------------------------------

type FnCreate = unsafe extern "C" fn() -> *mut MpvHandle;
type FnInitialize = unsafe extern "C" fn(*mut MpvHandle) -> c_int;
type FnTerminateDestroy = unsafe extern "C" fn(*mut MpvHandle);
type FnSetOptionString =
    unsafe extern "C" fn(*mut MpvHandle, *const c_char, *const c_char) -> c_int;
type FnSetPropertyString =
    unsafe extern "C" fn(*mut MpvHandle, *const c_char, *const c_char) -> c_int;
type FnCommand = unsafe extern "C" fn(*mut MpvHandle, *mut *const c_char) -> c_int;
type FnErrorString = unsafe extern "C" fn(c_int) -> *const c_char;
type FnWaitEvent = unsafe extern "C" fn(*mut MpvHandle, f64) -> *mut MpvEvent;
type FnRequestLogMessages = unsafe extern "C" fn(*mut MpvHandle, *const c_char) -> c_int;
type FnRenderContextCreate =
    unsafe extern "C" fn(*mut *mut MpvRenderContext, *mut MpvHandle, *mut MpvRenderParam) -> c_int;
type FnRenderContextRender =
    unsafe extern "C" fn(*mut MpvRenderContext, *mut MpvRenderParam) -> c_int;
type FnRenderContextSetUpdateCallback = unsafe extern "C" fn(
    *mut MpvRenderContext,
    Option<unsafe extern "C" fn(*mut c_void)>,
    *mut c_void,
);
type FnRenderContextUpdate = unsafe extern "C" fn(*mut MpvRenderContext) -> u64;
type FnRenderContextReportSwap = unsafe extern "C" fn(*mut MpvRenderContext);
type FnRenderContextFree = unsafe extern "C" fn(*mut MpvRenderContext);

/// Resolved libmpv entry points. Kept alive for the whole process: unloading
/// the library while a render context exists would be a use-after-free.
struct Symbols {
    _library: Library,
    create: FnCreate,
    initialize: FnInitialize,
    terminate_destroy: FnTerminateDestroy,
    set_option_string: FnSetOptionString,
    set_property_string: FnSetPropertyString,
    command: FnCommand,
    error_string: FnErrorString,
    wait_event: FnWaitEvent,
    request_log_messages: FnRequestLogMessages,
    render_context_create: FnRenderContextCreate,
    render_context_render: FnRenderContextRender,
    render_context_set_update_callback: FnRenderContextSetUpdateCallback,
    render_context_update: FnRenderContextUpdate,
    render_context_report_swap: FnRenderContextReportSwap,
    render_context_free: FnRenderContextFree,
}

static SYMBOLS: std::sync::OnceLock<Result<Symbols, String>> = std::sync::OnceLock::new();

fn symbols() -> Result<&'static Symbols> {
    match SYMBOLS.get_or_init(|| load_symbols().map_err(|err| format!("{err:#}"))) {
        Ok(symbols) => Ok(symbols),
        Err(message) => bail!("{message}"),
    }
}

fn load_symbols() -> Result<Symbols> {
    let mut attempts = Vec::new();
    let library = SONAMES
        .iter()
        .find_map(|soname| {
            // SAFETY: loading libmpv runs its initialisers, which is what any
            // dynamically linked mpv front-end does. The path is a fixed
            // soname, never user input.
            match unsafe { Library::new(soname) } {
                Ok(library) => {
                    tracing::debug!(soname, "loaded libmpv");
                    Some(library)
                }
                Err(err) => {
                    attempts.push(format!("{soname}: {err}"));
                    None
                }
            }
        })
        .ok_or_else(|| {
            anyhow!(
                "cannot load libmpv — install it (Debian/Kali: `sudo apt install libmpv2`, \
                 Arch: `pacman -S mpv`). Tried:\n  {}",
                attempts.join("\n  ")
            )
        })?;

    // SAFETY: every symbol below is part of libmpv's stable public C API and
    // is typed to match client.h / render_gl.h exactly. A missing symbol
    // surfaces as an error here rather than a crash later.
    unsafe {
        macro_rules! sym {
            ($name:literal, $ty:ty) => {
                *library
                    .get::<$ty>(concat!($name, "\0").as_bytes())
                    .with_context(|| format!("libmpv is missing {}", $name))?
            };
        }
        Ok(Symbols {
            create: sym!("mpv_create", FnCreate),
            initialize: sym!("mpv_initialize", FnInitialize),
            terminate_destroy: sym!("mpv_terminate_destroy", FnTerminateDestroy),
            set_option_string: sym!("mpv_set_option_string", FnSetOptionString),
            set_property_string: sym!("mpv_set_property_string", FnSetPropertyString),
            command: sym!("mpv_command", FnCommand),
            error_string: sym!("mpv_error_string", FnErrorString),
            wait_event: sym!("mpv_wait_event", FnWaitEvent),
            request_log_messages: sym!("mpv_request_log_messages", FnRequestLogMessages),
            render_context_create: sym!("mpv_render_context_create", FnRenderContextCreate),
            render_context_render: sym!("mpv_render_context_render", FnRenderContextRender),
            render_context_set_update_callback: sym!(
                "mpv_render_context_set_update_callback",
                FnRenderContextSetUpdateCallback
            ),
            render_context_update: sym!("mpv_render_context_update", FnRenderContextUpdate),
            render_context_report_swap: sym!(
                "mpv_render_context_report_swap",
                FnRenderContextReportSwap
            ),
            render_context_free: sym!("mpv_render_context_free", FnRenderContextFree),
            _library: library,
        })
    }
}

/// True when libmpv can be loaded. Lets the daemon fail at startup with a
/// clear message instead of when the first wallpaper is applied.
pub fn probe() -> Result<()> {
    symbols().map(|_| ())
}

fn check(code: c_int, what: &str) -> Result<()> {
    if code >= 0 {
        return Ok(());
    }
    let symbols = symbols()?;
    // SAFETY: mpv_error_string returns a static NUL-terminated string for any
    // input, including unknown codes.
    let message = unsafe { CStr::from_ptr((symbols.error_string)(code)) }
        .to_string_lossy()
        .into_owned();
    bail!("{what}: {message} (mpv error {code})")
}

// --- Player ------------------------------------------------------------------

/// Options applied to a player before `initialize`.
#[derive(Debug, Clone)]
pub struct PlayerOptions {
    pub mute: bool,
    pub hwdec: String,
    /// mpv `--panscan`, derived from the scale mode.
    pub panscan: String,
    /// mpv `--keepaspect`.
    pub keepaspect: String,
    /// `--loop-file=inf` when true, which is what a wallpaper wants.
    pub loop_file: bool,
}

impl Default for PlayerOptions {
    fn default() -> Self {
        Self {
            mute: true,
            hwdec: "auto-safe".into(),
            panscan: "1.0".into(),
            keepaspect: "yes".into(),
            loop_file: true,
        }
    }
}

/// One mpv instance rendering one output.
///
/// Not `Clone`/`Sync` on purpose: the render context must only be used from
/// the thread holding the GL context.
pub struct Player {
    handle: *mut MpvHandle,
    render: *mut MpvRenderContext,
    /// Set from mpv's update callback, which may fire on any thread.
    frame_ready: Arc<AtomicBool>,
    /// Kept alive while the callback can still fire.
    _callback_data: Box<CallbackData>,
}

struct CallbackData {
    frame_ready: Arc<AtomicBool>,
    /// Interrupts the render loop's `poll` so a ready frame is drawn
    /// immediately instead of at the next timer tick.
    waker: Option<crate::wake::Waker>,
}

/// Called by mpv when a new frame may be available. Runs on mpv's thread, so
/// it does the minimum possible: flip an atomic and return.
unsafe extern "C" fn on_update(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: ctx is the `CallbackData` we passed to
    // mpv_render_context_set_update_callback, alive until the context is freed.
    let data = unsafe { &*(ctx as *const CallbackData) };
    data.frame_ready.store(true, Ordering::Release);
    if let Some(waker) = &data.waker {
        waker.wake();
    }
}

impl Player {
    /// Creates a player and its render context.
    ///
    /// `get_proc_address` must resolve GL symbols for the context that is
    /// current on this thread; `proc_ctx` is handed back to it unchanged.
    pub fn new(
        options: &PlayerOptions,
        get_proc_address: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
        proc_ctx: *mut c_void,
        waker: Option<crate::wake::Waker>,
    ) -> Result<Self> {
        let symbols = symbols()?;

        // SAFETY: mpv_create returns an owned handle or null.
        let handle = unsafe { (symbols.create)() };
        if handle.is_null() {
            bail!("mpv_create failed (out of memory?)");
        }
        let mut player = PartialPlayer {
            handle,
            symbols,
            render: ptr::null_mut(),
        };

        // Options that must be set before initialize.
        let set = |name: &str, value: &str| -> Result<()> {
            let c_name = CString::new(name)?;
            let c_value = CString::new(value)?;
            // SAFETY: both strings are NUL-terminated and outlive the call.
            let code =
                unsafe { (symbols.set_option_string)(handle, c_name.as_ptr(), c_value.as_ptr()) };
            check(code, &format!("cannot set mpv option {name}={value}"))
        };

        // A wallpaper is a silent, input-less, infinite video surface. Most of
        // these options exist to switch off the parts of mpv that assume a
        // user is watching a file they chose.
        set("config", "no")?;
        set("input-default-bindings", "no")?;
        set("input-vo-keyboard", "no")?;
        set("osc", "no")?;
        set("osd-level", "0")?;
        set("terminal", "no")?;
        set("audio", if options.mute { "no" } else { "auto" })?;
        set("vo", "libmpv")?;
        set("hwdec", &options.hwdec)?;
        set("panscan", &options.panscan)?;
        set("keepaspect", &options.keepaspect)?;
        set("video-sync", "audio")?;
        // Reading ahead is pointless for a looping clip and costs memory per
        // output; a small cache keeps a 4K loop from pinning hundreds of MB.
        set("cache", "no")?;
        set("demuxer-max-bytes", "32MiB")?;
        if options.loop_file {
            set("loop-file", "inf")?;
        }
        // Idle keeps the instance alive between wallpapers so switching does
        // not pay for a fresh mpv every time.
        set("idle", "yes")?;

        // SAFETY: handle is a freshly created, not yet initialised instance.
        check(unsafe { (symbols.initialize)(handle) }, "mpv_initialize")?;

        let log_level = CString::new("warn")?;
        // SAFETY: handle is initialised; the level string is a valid CStr.
        unsafe {
            (symbols.request_log_messages)(handle, log_level.as_ptr());
        }

        let frame_ready = Arc::new(AtomicBool::new(false));
        let callback_data = Box::new(CallbackData {
            frame_ready: Arc::clone(&frame_ready),
            waker,
        });

        let mut init_params = MpvOpenglInitParams {
            get_proc_address: Some(get_proc_address),
            get_proc_address_ctx: proc_ctx,
            _legacy_extra_exts: ptr::null(),
        };
        let api_type = CString::new("opengl")?;
        let mut advanced: c_int = 1;
        let mut params = [
            MpvRenderParam {
                type_: PARAM_API_TYPE,
                data: api_type.as_ptr() as *mut c_void,
            },
            MpvRenderParam {
                type_: PARAM_OPENGL_INIT_PARAMS,
                data: &mut init_params as *mut _ as *mut c_void,
            },
            // Advanced control lets mpv tell us *when* a frame is due instead
            // of us polling it blindly, which is what makes the fps cap and
            // the pause policies cheap.
            MpvRenderParam {
                type_: PARAM_ADVANCED_CONTROL,
                data: &mut advanced as *mut _ as *mut c_void,
            },
            MpvRenderParam {
                type_: PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];

        let mut render: *mut MpvRenderContext = ptr::null_mut();
        // SAFETY: params is a terminated array living across the call, and a
        // GL context is current on this thread (the caller's contract).
        let code =
            unsafe { (symbols.render_context_create)(&mut render, handle, params.as_mut_ptr()) };
        check(code, "mpv_render_context_create")?;
        player.render = render;

        // SAFETY: callback_data outlives the render context, which is freed in
        // Drop before the box is dropped.
        unsafe {
            (symbols.render_context_set_update_callback)(
                render,
                Some(on_update),
                &*callback_data as *const CallbackData as *mut c_void,
            );
        }

        let handle = player.handle;
        let render = player.render;
        std::mem::forget(player); // ownership moves into Self below

        Ok(Self {
            handle,
            render,
            frame_ready,
            _callback_data: callback_data,
        })
    }

    /// Starts playing `path`, replacing whatever was playing.
    pub fn load(&self, path: &std::path::Path) -> Result<()> {
        let symbols = symbols()?;
        let path = path
            .to_str()
            .context("video path is not valid UTF-8, which mpv cannot accept")?;
        let command = CString::new("loadfile")?;
        let file = CString::new(path)?;
        let replace = CString::new("replace")?;
        let mut argv = [
            command.as_ptr(),
            file.as_ptr(),
            replace.as_ptr(),
            ptr::null(),
        ];
        // SAFETY: argv is NULL-terminated and all strings outlive the call.
        let code = unsafe { (symbols.command)(self.handle, argv.as_mut_ptr()) };
        check(code, &format!("cannot load {path}"))
    }

    pub fn set_paused(&self, paused: bool) -> Result<()> {
        self.set_property("pause", if paused { "yes" } else { "no" })
    }

    pub fn set_property(&self, name: &str, value: &str) -> Result<()> {
        let symbols = symbols()?;
        let c_name = CString::new(name)?;
        let c_value = CString::new(value)?;
        // SAFETY: both strings are NUL-terminated and outlive the call.
        let code = unsafe {
            (symbols.set_property_string)(self.handle, c_name.as_ptr(), c_value.as_ptr())
        };
        check(code, &format!("cannot set mpv property {name}={value}"))
    }

    /// Clears the current file, releasing decoder memory while an output has
    /// no wallpaper.
    pub fn stop(&self) -> Result<()> {
        let symbols = symbols()?;
        let command = CString::new("stop")?;
        let mut argv = [command.as_ptr(), ptr::null()];
        // SAFETY: argv is NULL-terminated and the string outlives the call.
        let code = unsafe { (symbols.command)(self.handle, argv.as_mut_ptr()) };
        check(code, "mpv stop")
    }

    /// True when mpv has signalled a new frame since the last [`Player::render`].
    pub fn frame_pending(&self) -> bool {
        if self.frame_ready.swap(false, Ordering::AcqRel) {
            return true;
        }
        let Ok(symbols) = symbols() else {
            return false;
        };
        // SAFETY: render is a live context owned by self.
        let flags = unsafe { (symbols.render_context_update)(self.render) };
        flags & UPDATE_FRAME != 0
    }

    /// Draws the current frame into the bound default framebuffer.
    ///
    /// `flip_y` is true for EGL window surfaces, whose origin is top-left
    /// while GL's is bottom-left.
    pub fn render(&self, width: u32, height: u32, flip_y: bool) -> Result<()> {
        let symbols = symbols()?;
        let mut fbo = MpvOpenglFbo {
            fbo: 0,
            w: width as c_int,
            h: height as c_int,
            // 0 means "whatever the surface is", which is what we want.
            internal_format: 0,
        };
        let mut flip: c_int = flip_y as c_int;
        let mut params = [
            MpvRenderParam {
                type_: PARAM_OPENGL_FBO,
                data: &mut fbo as *mut _ as *mut c_void,
            },
            MpvRenderParam {
                type_: PARAM_FLIP_Y,
                data: &mut flip as *mut _ as *mut c_void,
            },
            MpvRenderParam {
                type_: PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        // SAFETY: the caller guarantees the GL context is current; params is
        // terminated and lives across the call.
        let code = unsafe { (symbols.render_context_render)(self.render, params.as_mut_ptr()) };
        check(code, "mpv_render_context_render")
    }

    /// Tells mpv the frame reached the screen, which it uses for its own
    /// timing. Called right after `eglSwapBuffers`.
    pub fn report_swap(&self) {
        let Ok(symbols) = symbols() else { return };
        // SAFETY: render is a live context owned by self.
        unsafe { (symbols.render_context_report_swap)(self.render) }
    }

    /// Drains mpv's event queue, logging anything interesting. Returns false
    /// when mpv asked to shut down.
    pub fn pump_events(&self, output: &str) -> bool {
        let Ok(symbols) = symbols() else {
            return true;
        };
        loop {
            // SAFETY: handle is live; a 0 timeout makes this non-blocking and
            // the returned event is owned by mpv and valid until the next call.
            let event = unsafe { (symbols.wait_event)(self.handle, 0.0) };
            if event.is_null() {
                return true;
            }
            // SAFETY: non-null events point at a valid mpv_event.
            let event = unsafe { &*event };
            match event.event_id {
                EVENT_NONE => return true,
                EVENT_SHUTDOWN => return false,
                EVENT_LOG_MESSAGE if !event.data.is_null() => {
                    // SAFETY: for log events, data is an mpv_event_log_message.
                    let message = unsafe { &*(event.data as *const MpvEventLogMessage) };
                    let text = cstr_or_empty(message.text);
                    let prefix = cstr_or_empty(message.prefix);
                    tracing::warn!(output, "mpv/{}: {}", prefix, text.trim_end());
                }
                EVENT_END_FILE => {
                    if event.error < 0 {
                        tracing::warn!(output, "mpv stopped playing: error {}", event.error);
                    }
                }
                EVENT_FILE_LOADED => tracing::debug!(output, "mpv loaded the file"),
                _ => {}
            }
        }
    }
}

fn cstr_or_empty(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: mpv gives NUL-terminated strings valid for the event's lifetime.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// Guards the handle between `mpv_create` and the end of construction, so an
/// early `?` cannot leak an mpv instance.
struct PartialPlayer {
    handle: *mut MpvHandle,
    render: *mut MpvRenderContext,
    symbols: &'static Symbols,
}

impl Drop for PartialPlayer {
    fn drop(&mut self) {
        // SAFETY: both pointers are either null or owned by us, and freeing
        // the render context before the handle is the documented order.
        unsafe {
            if !self.render.is_null() {
                (self.symbols.render_context_free)(self.render);
            }
            if !self.handle.is_null() {
                (self.symbols.terminate_destroy)(self.handle);
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let Ok(symbols) = symbols() else { return };
        // SAFETY: the render context must be freed first — it holds a
        // reference to the GL context and to our update callback — and only
        // then the handle. After this, no callback can fire, so dropping
        // _callback_data is safe.
        unsafe {
            (symbols.render_context_set_update_callback)(self.render, None, ptr::null_mut());
            (symbols.render_context_free)(self.render);
            (symbols.terminate_destroy)(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soname_list_prefers_versioned_libraries() {
        assert_eq!(SONAMES[0], "libmpv.so.2");
        assert_eq!(*SONAMES.last().unwrap(), "libmpv.so");
    }

    #[test]
    fn init_params_struct_is_three_pointers_wide() {
        // mpv reads the first two fields; the third keeps pre-0.36 builds from
        // reading past our struct. If this ever shrinks, the comment on
        // `_legacy_extra_exts` is wrong.
        assert_eq!(
            std::mem::size_of::<MpvOpenglInitParams>(),
            3 * std::mem::size_of::<*const c_void>()
        );
    }

    #[test]
    fn render_param_is_pointer_aligned_and_padded() {
        // C: { int type; void *data; } — padded out to two pointer widths on
        // any ABI we support. A mismatch here would corrupt every render call.
        assert_eq!(
            std::mem::size_of::<MpvRenderParam>(),
            2 * std::mem::size_of::<*mut c_void>()
        );
        assert_eq!(
            std::mem::align_of::<MpvRenderParam>(),
            std::mem::align_of::<*mut c_void>()
        );
    }

    #[test]
    fn fbo_struct_is_four_ints() {
        assert_eq!(
            std::mem::size_of::<MpvOpenglFbo>(),
            4 * std::mem::size_of::<c_int>()
        );
    }
}
