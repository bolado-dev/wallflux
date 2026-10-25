//! EGL context management, shared by both backends.
//!
//! This is the seam that lets X11 and Wayland share one renderer: each
//! backend only provides a native display pointer and a native window handle,
//! and everything about contexts, configs and buffer swapping lives here.
//!
//! EGL is loaded dynamically for the same reason libmpv is — no build-time
//! dependency on the development packages, and a readable error when the
//! runtime library is missing.

use std::ffi::{CStr, c_char, c_void};

use anyhow::{Context, Result, bail};
use khronos_egl as egl;

/// Which window-system EGL should talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    X11,
    Wayland,
}

impl Platform {
    /// `EGL_PLATFORM_*_KHR` token for `eglGetPlatformDisplay`.
    fn token(self) -> egl::Enum {
        match self {
            Platform::X11 => 0x31D5,     // EGL_PLATFORM_X11_KHR
            Platform::Wayland => 0x31D8, // EGL_PLATFORM_WAYLAND_KHR
        }
    }
}

type Instance = egl::DynamicInstance<egl::EGL1_5>;

/// An EGL display, config and context. One per daemon: all outputs share a
/// single context, so mpv's shaders and textures are compiled once.
pub struct Egl {
    instance: Instance,
    display: egl::Display,
    config: egl::Config,
    context: egl::Context,
    /// Visual the X11 backend must use for its windows. Meaningless on Wayland.
    native_visual_id: u32,
}

/// A drawable tied to one output's native window.
pub struct Surface {
    surface: egl::Surface,
    width: u32,
    height: u32,
}

impl Surface {
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl Egl {
    /// Creates the display and context for `native_display`.
    ///
    /// # Safety
    ///
    /// `native_display` must be a live pointer of the kind `platform`
    /// describes: an `xcb_connection_t*`/`Display*` for X11, a `wl_display*`
    /// for Wayland. It must outlive this `Egl`.
    pub unsafe fn new(platform: Platform, native_display: *mut c_void) -> Result<Self> {
        // SAFETY: load_required only dlopens libEGL and reads its symbols.
        let instance = unsafe { Instance::load_required() }.map_err(|err| {
            anyhow::anyhow!(
                "cannot load libEGL ({err}) — install it (Debian/Kali: \
                 `sudo apt install libegl1`)"
            )
        })?;

        // eglGetPlatformDisplay is the correct call: eglGetDisplay has to
        // guess the platform from the pointer, which breaks when a system has
        // both X11 and Wayland available (exactly our case).
        let display = unsafe {
            instance.get_platform_display(platform.token(), native_display, &[egl::ATTRIB_NONE])
        }
        .or_else(|err| {
            tracing::debug!(%err, "eglGetPlatformDisplay failed, falling back to eglGetDisplay");
            // SAFETY: same contract as this function's; the fallback exists
            // for drivers predating EGL 1.5.
            unsafe { instance.get_display(native_display) }
                .ok_or_else(|| anyhow::anyhow!("eglGetDisplay returned no display"))
        })?;

        let (major, minor) = instance
            .initialize(display)
            .context("eglInitialize failed — is a GPU driver available?")?;
        tracing::debug!(major, minor, ?platform, "EGL initialised");

        instance
            .bind_api(egl::OPENGL_API)
            .context("this driver cannot provide desktop OpenGL via EGL")?;

        // Alpha is deliberately 0 bits: a wallpaper is opaque, and asking for
        // an alpha channel on X11 makes a compositor blend it against the root
        // window, which looks like a washed-out wallpaper.
        let attributes = [
            egl::SURFACE_TYPE,
            egl::WINDOW_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            0,
            egl::DEPTH_SIZE,
            0,
            egl::STENCIL_SIZE,
            0,
            egl::NONE,
        ];
        let config = instance
            .choose_first_config(display, &attributes)
            .context("eglChooseConfig failed")?
            .context("no EGL config with an 8/8/8 window surface is available")?;

        let native_visual_id =
            instance.get_config_attrib(display, config, egl::NATIVE_VISUAL_ID)? as u32;

        // mpv's OpenGL renderer needs 3.3 core at minimum for its default
        // shaders; asking for a compatibility profile would silently land on
        // GL 2.1 with llvmpipe.
        let context_attributes = [
            egl::CONTEXT_MAJOR_VERSION,
            3,
            egl::CONTEXT_MINOR_VERSION,
            3,
            egl::CONTEXT_OPENGL_PROFILE_MASK,
            egl::CONTEXT_OPENGL_CORE_PROFILE_BIT,
            egl::NONE,
        ];
        let context = instance
            .create_context(display, config, None, &context_attributes)
            .context("cannot create an OpenGL 3.3 core context")?;

        Ok(Self {
            instance,
            display,
            config,
            context,
            native_visual_id,
        })
    }

    /// X11 visual id matching the chosen config.
    pub fn native_visual_id(&self) -> u32 {
        self.native_visual_id
    }

    /// Wraps a backend's native window in an EGL surface.
    ///
    /// # Safety
    ///
    /// `native_window` must be a valid window handle for the platform this
    /// `Egl` was created with — an X11 `Window` id or a `wl_egl_window*` —
    /// and must outlive the returned [`Surface`].
    pub unsafe fn create_surface(
        &self,
        native_window: egl::NativeWindowType,
        width: u32,
        height: u32,
    ) -> Result<Surface> {
        // SAFETY: delegated to the caller's contract above.
        let surface = unsafe {
            self.instance
                .create_window_surface(self.display, self.config, native_window, None)
        }
        .context("eglCreateWindowSurface failed")?;
        Ok(Surface {
            surface,
            width,
            height,
        })
    }

    /// Makes `surface` current on the calling thread. Every GL and mpv render
    /// call must be preceded by this.
    pub fn make_current(&self, surface: &Surface) -> Result<()> {
        self.instance
            .make_current(
                self.display,
                Some(surface.surface),
                Some(surface.surface),
                Some(self.context),
            )
            .context("eglMakeCurrent failed")
    }

    pub fn swap_buffers(&self, surface: &Surface) -> Result<()> {
        self.instance
            .swap_buffers(self.display, surface.surface)
            .context("eglSwapBuffers failed")
    }

    /// `0` disables vsync, `1` enables it.
    ///
    /// wallflux renders with vsync off and paces itself from its own fps cap.
    /// With vsync on, a 120 Hz output would block the single render thread in
    /// `eglSwapBuffers` for every output in turn, so one monitor's refresh
    /// rate would throttle the others.
    pub fn set_swap_interval(&self, interval: i32) -> Result<()> {
        self.instance
            .swap_interval(self.display, interval)
            .context("eglSwapInterval failed")
    }

    pub fn destroy_surface(&self, surface: Surface) {
        if let Err(err) = self.instance.destroy_surface(self.display, surface.surface) {
            tracing::warn!(%err, "eglDestroySurface failed");
        }
    }

    /// Resolves a GL or EGL entry point by name.
    pub fn proc_address(&self, name: &str) -> *mut c_void {
        match self.instance.get_proc_address(name) {
            // A function pointer cannot be cast straight to a data pointer in
            // Rust; going through usize is the sanctioned route, and is what
            // every GL loader does.
            Some(func) => func as usize as *mut c_void,
            None => std::ptr::null_mut(),
        }
    }

    /// The `(callback, ctx)` pair mpv's render context wants. The context
    /// pointer is this `Egl`, so the callback must not outlive it.
    pub fn mpv_get_proc_address(
        &self,
    ) -> (
        unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
        *mut c_void,
    ) {
        (mpv_get_proc_address_impl, self as *const Egl as *mut c_void)
    }

    /// Loads the handful of GL functions wallflux itself calls.
    pub fn load_gl(&self) -> Result<Gl> {
        Gl::load(self)
    }
}

/// Trampoline handed to mpv. Resolves GL symbols through our EGL instance.
unsafe extern "C" fn mpv_get_proc_address_impl(
    ctx: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    if ctx.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: ctx is the &Egl passed in mpv_get_proc_address, alive for as
    // long as the render context; name is a NUL-terminated string from mpv.
    let egl = unsafe { &*(ctx as *const Egl) };
    let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else {
        return std::ptr::null_mut();
    };
    egl.proc_address(name)
}

impl Drop for Egl {
    fn drop(&mut self) {
        let _ = self.instance.make_current(self.display, None, None, None);
        if let Err(err) = self.instance.destroy_context(self.display, self.context) {
            tracing::warn!(%err, "eglDestroyContext failed");
        }
        if let Err(err) = self.instance.terminate(self.display) {
            tracing::warn!(%err, "eglTerminate failed");
        }
    }
}

/// The GL calls wallflux makes on its own; everything else is mpv's job.
pub struct Gl {
    clear_color: unsafe extern "C" fn(f32, f32, f32, f32),
    clear: unsafe extern "C" fn(u32),
    viewport: unsafe extern "C" fn(i32, i32, i32, i32),
    finish: unsafe extern "C" fn(),
    gen_framebuffers: unsafe extern "C" fn(i32, *mut u32),
    delete_framebuffers: unsafe extern "C" fn(i32, *const u32),
    bind_framebuffer: unsafe extern "C" fn(u32, u32),
    framebuffer_texture_2d: unsafe extern "C" fn(u32, u32, u32, u32, i32),
    check_framebuffer_status: unsafe extern "C" fn(u32) -> u32,
    blit_framebuffer: unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32, i32, u32, u32),
    gen_textures: unsafe extern "C" fn(i32, *mut u32),
    delete_textures: unsafe extern "C" fn(i32, *const u32),
    bind_texture: unsafe extern "C" fn(u32, u32),
    tex_image_2d: unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
    tex_parameteri: unsafe extern "C" fn(u32, u32, i32),
}

/// `GL_COLOR_BUFFER_BIT`
const COLOR_BUFFER_BIT: u32 = 0x0000_4000;
const FRAMEBUFFER: u32 = 0x8D40;
const READ_FRAMEBUFFER: u32 = 0x8CA8;
const DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const COLOR_ATTACHMENT0: u32 = 0x8CE0;
const FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const TEXTURE_2D: u32 = 0x0DE1;
const TEXTURE_MIN_FILTER: u32 = 0x2801;
const TEXTURE_MAG_FILTER: u32 = 0x2800;
const LINEAR: i32 = 0x2601;
const RGBA8: i32 = 0x8058;
const RGBA: u32 = 0x1908;
const UNSIGNED_BYTE: u32 = 0x1401;

impl Gl {
    fn load(egl: &Egl) -> Result<Self> {
        fn resolve<T>(egl: &Egl, name: &str) -> Result<T> {
            let ptr = egl.proc_address(name);
            if ptr.is_null() {
                bail!("the GL driver does not provide {name}");
            }
            // SAFETY: eglGetProcAddress returned a function pointer for this
            // core GL entry point; T is declared to match its signature.
            Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&ptr) })
        }
        // Some drivers only resolve core GL symbols once a context is
        // current, so the caller does that before calling us.
        Ok(Self {
            clear_color: resolve(egl, "glClearColor")?,
            clear: resolve(egl, "glClear")?,
            viewport: resolve(egl, "glViewport")?,
            finish: resolve(egl, "glFinish")?,
            gen_framebuffers: resolve(egl, "glGenFramebuffers")?,
            delete_framebuffers: resolve(egl, "glDeleteFramebuffers")?,
            bind_framebuffer: resolve(egl, "glBindFramebuffer")?,
            framebuffer_texture_2d: resolve(egl, "glFramebufferTexture2D")?,
            check_framebuffer_status: resolve(egl, "glCheckFramebufferStatus")?,
            blit_framebuffer: resolve(egl, "glBlitFramebuffer")?,
            gen_textures: resolve(egl, "glGenTextures")?,
            delete_textures: resolve(egl, "glDeleteTextures")?,
            bind_texture: resolve(egl, "glBindTexture")?,
            tex_image_2d: resolve(egl, "glTexImage2D")?,
            tex_parameteri: resolve(egl, "glTexParameteri")?,
        })
    }

    /// Paints the whole surface one colour. Used for outputs with no
    /// wallpaper, and once before the first frame so a new window never shows
    /// uninitialised memory.
    pub fn clear(&self, width: u32, height: u32, rgb: [f32; 3]) {
        // SAFETY: a GL context is current (the caller's contract) and these
        // are core GL calls with in-range arguments.
        unsafe {
            (self.viewport)(0, 0, width as i32, height as i32);
            (self.clear_color)(rgb[0], rgb[1], rgb[2], 1.0);
            (self.clear)(COLOR_BUFFER_BIT);
        }
    }

    /// Blocks until the GPU has finished. Used when tearing down a surface, so
    /// we do not destroy a window the driver is still drawing into.
    pub fn finish(&self) {
        // SAFETY: core GL call, context current.
        unsafe { (self.finish)() }
    }
}

/// An offscreen colour buffer, for rendering below the output's resolution.
///
/// This is what `render_scale` is: mpv draws into a texture of
/// `scale × size`, and the result is stretched onto the window with a
/// filtered blit. Decoding and all of mpv's per-pixel work then happen at
/// the smaller size, which is where the saving comes from — at 0.5 it is a
/// quarter of the pixels.
pub struct Offscreen {
    framebuffer: u32,
    texture: u32,
    width: u32,
    height: u32,
}

impl Offscreen {
    /// Creates a colour buffer of exactly `width × height`.
    pub fn new(gl: &Gl, width: u32, height: u32) -> Result<Self> {
        let width = width.max(1);
        let height = height.max(1);
        let mut texture = 0u32;
        let mut framebuffer = 0u32;

        // SAFETY: a GL context is current (the caller's contract) and every
        // call below is core GL with in-range arguments. The ids come back
        // from GL itself.
        unsafe {
            (gl.gen_textures)(1, &mut texture);
            (gl.bind_texture)(TEXTURE_2D, texture);
            (gl.tex_image_2d)(
                TEXTURE_2D,
                0,
                RGBA8,
                width as i32,
                height as i32,
                0,
                RGBA,
                UNSIGNED_BYTE,
                std::ptr::null(),
            );
            // Linear on both: the whole point is that this gets stretched.
            (gl.tex_parameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, LINEAR);
            (gl.tex_parameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, LINEAR);
            (gl.bind_texture)(TEXTURE_2D, 0);

            (gl.gen_framebuffers)(1, &mut framebuffer);
            (gl.bind_framebuffer)(FRAMEBUFFER, framebuffer);
            (gl.framebuffer_texture_2d)(FRAMEBUFFER, COLOR_ATTACHMENT0, TEXTURE_2D, texture, 0);
            let status = (gl.check_framebuffer_status)(FRAMEBUFFER);
            (gl.bind_framebuffer)(FRAMEBUFFER, 0);

            if status != FRAMEBUFFER_COMPLETE {
                (gl.delete_framebuffers)(1, &framebuffer);
                (gl.delete_textures)(1, &texture);
                bail!("el framebuffer fuera de pantalla no está completo (0x{status:x})");
            }
        }

        Ok(Self {
            framebuffer,
            texture,
            width,
            height,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The framebuffer id to hand to mpv.
    pub fn framebuffer(&self) -> u32 {
        self.framebuffer
    }

    pub fn bind(&self, gl: &Gl) {
        // SAFETY: core GL call, context current, id owned by self.
        unsafe { (gl.bind_framebuffer)(FRAMEBUFFER, self.framebuffer) }
    }

    /// Stretches this buffer onto the window and leaves the default
    /// framebuffer bound.
    pub fn blit_to_window(&self, gl: &Gl, width: u32, height: u32) {
        // SAFETY: both framebuffers exist and the rectangles are within
        // their sizes; LINEAR is valid for a colour-only blit.
        unsafe {
            (gl.bind_framebuffer)(READ_FRAMEBUFFER, self.framebuffer);
            (gl.bind_framebuffer)(DRAW_FRAMEBUFFER, 0);
            (gl.blit_framebuffer)(
                0,
                0,
                self.width as i32,
                self.height as i32,
                0,
                0,
                width as i32,
                height as i32,
                COLOR_BUFFER_BIT,
                LINEAR as u32,
            );
            (gl.bind_framebuffer)(FRAMEBUFFER, 0);
        }
    }

    /// Frees the GL objects. Takes `gl` rather than implementing `Drop`
    /// because releasing them needs the right context to be current, and a
    /// destructor cannot guarantee that.
    pub fn delete(self, gl: &Gl) {
        // SAFETY: ids owned by self, deleted exactly once.
        unsafe {
            (gl.delete_framebuffers)(1, &self.framebuffer);
            (gl.delete_textures)(1, &self.texture);
        }
    }
}
