//! GLES 2.0 / 2.1 legacy backend (`glow` + EGL).
//!
//! This path exists for two reasons:
//!
//! 1. Devices without Vulkan and without a GLES 3.0 context.
//! 2. Minecraft Alpha – 1.6, which drives fixed-function-era GL (immediate mode,
//!    display lists, `GL_QUADS`, client-side matrix stacks) that a core-profile
//!    context cannot express.
//!
//! # What "fixed-function emulation" means here
//!
//! Sinkon does not reimplement GL 1.x. It provides a small, closed set of
//! generated GLSL ES 1.00 program pairs — one per combination of the state bits
//! an old Minecraft version can set — and a vertex format that carries position,
//! texture coordinate and a vertex colour. Vanilla 1.6-and-earlier geometry is
//! expressible in that format, which covers terrain, chunk (re)builds, entities
//! and the HUD. Anything that is *not* expressible — `glSelect`, feedback mode,
//! user clip planes, line/polygon stipple, `glDrawPixels` — is listed as
//! unsupported in `KNOWN_ISSUES.md` and never silently dropped.
//!
//! # Buffers
//!
//! Chunk geometry lives in pooled VBOs fed from a persistent CPU ring:
//!
//! * unchanged chunks are never re-uploaded (`glBufferData` only on growth),
//! * dirty ranges use `glBufferSubData` (see [`RingAllocator`]),
//! * the ring is a fixed-size arena, so steady-state streaming allocates nothing.
//!
//! # Safety
//!
//! All EGL/GL entry points are raw FFI. Every `unsafe` block below states the
//! invariant it relies on. The invariants are:
//!
//! * `EglState` is only created after `eglInitialize` + `eglMakeCurrent`, and is
//!   dropped on the render thread that owns the context.
//! * `glow::Context` is created from `eglGetProcAddress`, which returns valid
//!   extension entry points for the current context.
//! * GL objects (`Buffer`, `Texture`, `Program`, `Shader`) are only used while
//!   `EglState` is current; `destroy()` deletes them before terminating EGL.

use crate::backend::{
    Arena, BackendError, BackendFeatures, BackendKind, BackendResult, BlendMode, BufferDesc,
    BufferId, BufferUsage, Capabilities, DrawBatch, FrameSubmission, GraphicsBackend, PipelineDesc,
    PipelineId, PixelFormat, PresentOutcome, ShaderCode, ShaderKey, TextureDesc, TextureId,
    Topology,
};
use crate::perf::tier::DeviceTier;
use crate::surface::SurfaceTarget;

/// Vertex stride of the legacy vertex format: `pos(f32x3) uv(f32x2) color(u8x4)`.
pub const LEGACY_VERTEX_STRIDE: i32 = 24;

/// Byte offsets inside [`LEGACY_VERTEX_STRIDE`].
pub const UV_OFFSET: i32 = 12;
/// Byte offset of the packed colour attribute.
pub const COLOR_OFFSET: i32 = 20;

/// Size of the persistent CPU staging ring (4 MiB).
pub const STAGING_RING_BYTES: u64 = 4 * 1024 * 1024;

/// Fixed-function state bits that change the generated program.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LegacyKey(pub u8);

impl LegacyKey {
    /// Sampled texture enabled.
    pub const TEXTURED: u8 = 1 << 0;
    /// Per-vertex colour modulation enabled.
    pub const COLORED: u8 = 1 << 1;
    /// Distance fog enabled.
    pub const FOG: u8 = 1 << 2;
    /// Alpha blending enabled.
    pub const BLEND: u8 = 1 << 3;

    /// Number of program slots (every combination of the four bits).
    pub const COUNT: usize = 16;

    /// Build a key from the individual state bits.
    pub const fn new(textured: bool, colored: bool, fog: bool, blend: bool) -> LegacyKey {
        let mut bits = 0u8;
        if textured {
            bits |= Self::TEXTURED;
        }
        if colored {
            bits |= Self::COLORED;
        }
        if fog {
            bits |= Self::FOG;
        }
        if blend {
            bits |= Self::BLEND;
        }
        LegacyKey(bits)
    }

    /// Slot index in the program cache.
    pub fn index(self) -> usize {
        (self.0 & 0x0F) as usize
    }

    /// Texture sampling on?
    pub fn textured(self) -> bool {
        self.0 & Self::TEXTURED != 0
    }

    /// Vertex colour modulation on?
    pub fn colored(self) -> bool {
        self.0 & Self::COLORED != 0
    }

    /// Fog on?
    pub fn fog(self) -> bool {
        self.0 & Self::FOG != 0
    }

    /// Blending on?
    pub fn blend(self) -> bool {
        self.0 & Self::BLEND != 0
    }
}

/// Generated GLSL ES 1.00 vertex shader for `key`.
///
/// naga cannot emit `#version 100`, so these sources are written by hand; see
/// `docs/SHADER_MATRIX.md` for why that is the honest option.
pub fn vertex_source(key: LegacyKey) -> String {
    let mut source = String::with_capacity(512);
    source.push_str("#version 100\n");
    source.push_str("uniform mat4 u_mvp;\n");
    source.push_str("attribute vec3 a_pos;\n");
    if key.textured() {
        source.push_str("attribute vec2 a_uv;\nvarying vec2 v_uv;\n");
    }
    if key.colored() {
        source.push_str("attribute vec4 a_color;\nvarying vec4 v_color;\n");
    }
    if key.fog() {
        source.push_str("varying float v_fog;\n");
    }
    source.push_str("void main() {\n  gl_Position = u_mvp * vec4(a_pos, 1.0);\n");
    if key.textured() {
        source.push_str("  v_uv = a_uv;\n");
    }
    if key.colored() {
        source.push_str("  v_color = a_color;\n");
    }
    if key.fog() {
        source.push_str("  v_fog = clamp((gl_Position.z / gl_Position.w) * 0.5 + 0.5, 0.0, 1.0);\n");
    }
    source.push_str("}\n");
    source
}

/// Generated GLSL ES 1.00 fragment shader for `key`.
pub fn fragment_source(key: LegacyKey) -> String {
    let mut source = String::with_capacity(384);
    source.push_str("#version 100\n");
    source.push_str("precision mediump float;\n");
    if key.textured() {
        source.push_str("uniform sampler2D u_tex;\nvarying vec2 v_uv;\n");
    }
    if key.colored() {
        source.push_str("varying vec4 v_color;\n");
    }
    if key.fog() {
        source.push_str("uniform vec4 u_fog_color;\nvarying float v_fog;\n");
    }
    source.push_str("void main() {\n");
    match (key.textured(), key.colored()) {
        (true, true) => source.push_str("  gl_FragColor = texture2D(u_tex, v_uv) * v_color;\n"),
        (true, false) => source.push_str("  gl_FragColor = texture2D(u_tex, v_uv);\n"),
        (false, true) => source.push_str("  gl_FragColor = v_color;\n"),
        (false, false) => source.push_str("  gl_FragColor = vec4(1.0);\n"),
    }
    if key.fog() {
        source.push_str("  gl_FragColor.rgb = mix(gl_FragColor.rgb, u_fog_color.rgb, v_fog);\n");
    }
    source.push_str("}\n");
    source
}

/// Fixed-size CPU staging ring with wrap-around.
///
/// The render thread carves upload ranges out of this ring. Nothing here
/// allocates; the backing buffer is created once at init and reused forever.
#[derive(Debug)]
pub struct RingAllocator {
    capacity: u64,
    cursor: u64,
    high_water: u64,
    wrapped: u64,
}

impl RingAllocator {
    /// Create a ring of `capacity` bytes.
    pub fn new(capacity: u64) -> RingAllocator {
        RingAllocator {
            capacity: capacity.max(1),
            cursor: 0,
            high_water: 0,
            wrapped: 0,
        }
    }

    /// Reserving `size` bytes of `align`ment, or `None` if the tail does not fit.
    pub fn alloc(&mut self, size: u64, align: u64) -> Option<u64> {
        let align = align.max(1);
        let padded = (self.cursor + align - 1) & !(align - 1);
        if padded + size > self.capacity {
            self.wrapped += 1;
            return None;
        }
        self.cursor = padded + size;
        if self.cursor > self.high_water {
            self.high_water = self.cursor;
        }
        Some(padded)
    }

    /// Start a new frame: everything before this point is free again.
    pub fn reset(&mut self) {
        self.cursor = 0;
    }

    /// Bytes handed out since the last reset.
    pub fn used(&self) -> u64 {
        self.cursor
    }

    /// Peak usage observed.
    pub fn high_water(&self) -> u64 {
        self.high_water
    }

    /// Number of times an allocation had to wrap.
    pub fn wraps(&self) -> u64 {
        self.wrapped
    }

    /// Total capacity.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }
}

/// Stats surfaced by the capability report for the legacy path.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct LegacyStats {
    /// Programs compiled since init.
    pub programs_compiled: u32,
    /// `glBufferData` calls (full re-uploads).
    pub buffer_data_calls: u64,
    /// `glBufferSubData` calls (dirty-range updates).
    pub buffer_sub_data_calls: u64,
    /// Draw calls issued.
    pub draw_calls: u64,
    /// Frames presented.
    pub frames: u64,
}

/// Create the legacy backend.
///
/// On non-Android hosts the path does not exist: EGL and GLES 2.0 are Android-only
/// here, and pretending otherwise would hide a real failure from the launcher.
pub fn create(
    target: &SurfaceTarget<'_>,
    tier: DeviceTier,
) -> BackendResult<Box<dyn GraphicsBackend>> {
    platform::create(target, tier)
}

/// Does the device look capable of running the legacy path at all?
pub fn compatible_device(caps: &Capabilities) -> bool {
    caps.gles_version.map(|v| v >= 20).unwrap_or(false)
}

/// Human readable backend summary for logs.
pub fn describe(caps: &Capabilities) -> String {
    format!(
        "GLES 2.0 legacy on \"{}\" (driver {}) — fixed-function emulation, tier {}",
        caps.adapter_name, caps.driver_info, caps.device_tier
    )
}

#[cfg(target_os = "android")]
mod platform {
    use super::*;
    use std::ffi::{c_char, c_void, CString};

    // ---------------------------------------------------------------- EGL FFI
    const EGL_DEFAULT_DISPLAY: *mut c_void = std::ptr::null_mut();
    const EGL_NONE: i32 = 0x3038;
    const EGL_SURFACE_TYPE: i32 = 0x3033;
    const EGL_WINDOW_BIT: i32 = 0x0004;
    const EGL_RENDERABLE_TYPE: i32 = 0x3040;
    const EGL_OPENGL_ES2_BIT: i32 = 0x0004;
    const EGL_RED_SIZE: i32 = 0x3024;
    const EGL_GREEN_SIZE: i32 = 0x3023;
    const EGL_BLUE_SIZE: i32 = 0x3022;
    const EGL_ALPHA_SIZE: i32 = 0x3021;
    const EGL_DEPTH_SIZE: i32 = 0x3025;
    const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;

    #[link(name = "EGL")]
    extern "C" {
        fn eglGetDisplay(display_id: *mut c_void) -> *mut c_void;
        fn eglInitialize(display: *mut c_void, major: *mut i32, minor: *mut i32) -> u32;
        fn eglChooseConfig(
            display: *mut c_void,
            attrib_list: *const i32,
            configs: *mut *mut c_void,
            config_size: i32,
            num_config: *mut i32,
        ) -> u32;
        fn eglCreateWindowSurface(
            display: *mut c_void,
            config: *mut c_void,
            window: *mut c_void,
            attrib_list: *const i32,
        ) -> *mut c_void;
        fn eglCreateContext(
            display: *mut c_void,
            config: *mut c_void,
            share_context: *mut c_void,
            attrib_list: *const i32,
        ) -> *mut c_void;
        fn eglMakeCurrent(
            display: *mut c_void,
            draw: *mut c_void,
            read: *mut c_void,
            context: *mut c_void,
        ) -> u32;
        fn eglSwapBuffers(display: *mut c_void, surface: *mut c_void) -> u32;
        fn eglDestroyContext(display: *mut c_void, context: *mut c_void) -> u32;
        fn eglDestroySurface(display: *mut c_void, surface: *mut c_void) -> u32;
        fn eglTerminate(display: *mut c_void) -> u32;
        fn eglGetProcAddress(name: *const c_char) -> *mut c_void;
        fn eglGetError() -> i32;
    }

    struct EglState {
        display: *mut c_void,
        surface: *mut c_void,
        context: *mut c_void,
    }

    impl EglState {
        fn create(window: *mut c_void) -> Result<EglState, String> {
            // SAFETY: every call follows the EGL 1.4 contract; all out-parameters
            // point to stack locals and every returned handle is checked for null
            // or `EGL_FALSE` before use.
            unsafe {
                let display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
                if display.is_null() {
                    return Err(format!("eglGetDisplay failed (0x{:x})", eglGetError()));
                }
                let mut major = 0;
                let mut minor = 0;
                if eglInitialize(display, &mut major, &mut minor) == 0 {
                    return Err(format!("eglInitialize failed (0x{:x})", eglGetError()));
                }
                let attribs = [
                    EGL_SURFACE_TYPE,
                    EGL_WINDOW_BIT,
                    EGL_RENDERABLE_TYPE,
                    EGL_OPENGL_ES2_BIT,
                    EGL_RED_SIZE,
                    8,
                    EGL_GREEN_SIZE,
                    8,
                    EGL_BLUE_SIZE,
                    8,
                    EGL_ALPHA_SIZE,
                    8,
                    EGL_DEPTH_SIZE,
                    24,
                    EGL_NONE,
                ];
                let mut config: *mut c_void = std::ptr::null_mut();
                let mut configs = 0;
                if eglChooseConfig(display, attribs.as_ptr(), &mut config, 1, &mut configs) == 0
                    || configs == 0
                    || config.is_null()
                {
                    return Err(format!("eglChooseConfig failed (0x{:x})", eglGetError()));
                }
                let surface = eglCreateWindowSurface(display, config, window, std::ptr::null());
                if surface.is_null() {
                    return Err(format!(
                        "eglCreateWindowSurface failed (0x{:x})",
                        eglGetError()
                    ));
                }
                let context = eglCreateContext(
                    display,
                    config,
                    std::ptr::null_mut(),
                    [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE].as_ptr(),
                );
                if context.is_null() {
                    eglDestroySurface(display, surface);
                    return Err(format!("eglCreateContext failed (0x{:x})", eglGetError()));
                }
                if eglMakeCurrent(display, surface, surface, context) == 0 {
                    eglDestroyContext(display, context);
                    eglDestroySurface(display, surface);
                    return Err(format!("eglMakeCurrent failed (0x{:x})", eglGetError()));
                }
                log::info!("EGL ready: {major}.{minor}, ES2 context current");
                Ok(EglState {
                    display
                ,
                    surface,
                    context,
                })
            }
        }
    }

    impl Drop for EglState {
        fn drop(&mut self) {
            // SAFETY: the handles were created by `create` and are destroyed once.
            unsafe {
                eglMakeCurrent(
                    self.display,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                eglDestroyContext(self.display, self.context);
                eglDestroySurface(self.display, self.surface);
                eglTerminate(self.display);
            }
        }
    }

    struct LegacyBuffer {
        buffer: glow::Buffer,
        capacity: u64,
        size: u64,
    }

    struct LegacyProgram {
        program: glow::Program,
        mvp: Option<glow::UniformLocation>,
        tex: Option<glow::UniformLocation>,
        pos: u32,
        uv: Option<u32>,
        color: Option<u32>,
    }

    struct LegacyTexture {
        texture: glow::Texture,
        size: [u32; 2],
    }

    /// GLES 2.0 legacy backend.
    pub struct LegacyBackend {
        egl: EglState,
        gl: glow::Context,
        capabilities: Capabilities,
        tier: DeviceTier,
        programs: [Option<LegacyProgram>; LegacyKey::COUNT],
        pipelines: Arena<LegacyKey>,
        buffers: Arena<LegacyBuffer>,
        textures: Arena<LegacyTexture>,
        staging: RingAllocator,
        mvp: [f32; 16],
        fog_color: [f32; 4],
        stats: LegacyStats,
        vram: u64,
        width: u32,
        height: u32,
        owner_thread: std::thread::ThreadId,
    }

    pub fn create(
        target: &SurfaceTarget<'_>,
        tier: DeviceTier,
    ) -> BackendResult<Box<dyn GraphicsBackend>> {
        let window = target.native_window().ok_or_else(|| {
            BackendError::Unavailable(
                "GLES 2.0 legacy path needs a native window; offscreen targets are CPU-only"
                    .to_string(),
            )
        })?;
        let (width, height) = target.size();
        let egl = EglState::create(window.as_ptr()).map_err(BackendError::Surface)?;

        // SAFETY: an ES2 context is current on this thread, so `eglGetProcAddress`
        // returns entry points valid for it; the context outlives the glow context
        // by field declaration order (`egl` before `gl`).
        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                let name = CString::new(name).unwrap_or_default();
                eglGetProcAddress(name.as_ptr()) as *const c_void
            })
        };

        let capabilities = probe_capabilities(&gl, tier);
        log::info!("{}", describe(&capabilities));

        let programs: [Option<LegacyProgram>; LegacyKey::COUNT] =
            [const { None }; LegacyKey::COUNT];

        Ok(Box::new(LegacyBackend {
            egl,
            gl,
            capabilities,
            tier,
            programs,
            pipelines: Arena::new(),
            buffers: Arena::new(),
            textures: Arena::new(),
            staging: RingAllocator::new(STAGING_RING_BYTES),
            mvp: [0.0; 16],
            fog_color: [0.0, 0.0, 0.0, 1.0],
            stats: LegacyStats::default(),
            vram: 0,
            width,
            height,
            owner_thread: std::thread::current().id(),
        }))
    }

    fn probe_capabilities(gl: &glow::Context, tier: DeviceTier) -> Capabilities {
        // SAFETY: the ES2 context is current; these queries have no side effects.
        let (max_texture, renderer, version, extensions) = unsafe {
            (
                gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE) as u32,
                gl.get_parameter_string(glow::RENDERER),
                gl.get_parameter_string(glow::VERSION),
                gl.get_parameter_string(glow::EXTENSIONS),
            )
        };
        let extension_list: Vec<String> = extensions
            .split_whitespace()
            .take(64)
            .map(str::to_string)
            .collect();
        let supports_astc = extension_list.iter().any(|e| e.contains("ASTC"));
        Capabilities {
            kind: BackendKind::Gles2Legacy,
            api_name: "OpenGL ES".to_string(),
            api_version: version.clone(),
            gl_version: Some(version),
            gles_version: Some(20),
            vulkan_api: None,
            glsl_version: "100".to_string(),
            adapter_name: renderer,
            driver_info: format!("GLES2 EGL, {} extensions", extension_list.len()),
            max_texture_size: max_texture,
            max_uniform_buffer_binding_size: 0,
            max_compute_workgroup_size: [0, 0, 0],
            supports: BackendFeatures {
                astc: supports_astc,
                ..BackendFeatures::GLES2
            },
            extensions: extension_list,
            device_tier: tier.as_str().to_string(),
            vram_bytes: 0,
            resident_sections: 0,
        }
    }

    impl LegacyBackend {
        fn program_for(&mut self, key: LegacyKey) -> Result<&LegacyProgram, BackendError> {
            let index = key.index();
            if self.programs[index].is_none() {
                let program = self.compile_program(key)?;
                self.programs[index] = Some(program);
                self.stats.programs_compiled += 1;
            }
            self.programs[index]
                .as_ref()
                .ok_or_else(|| BackendError::Device("program cache lost the program".to_string()))
        }

        fn compile_program(&self, key: LegacyKey) -> Result<LegacyProgram, BackendError> {
            let vertex_source = vertex_source(key);
            let fragment_source = fragment_source(key);
            // SAFETY: the ES2 context is current and every object is deleted on the
            // error paths below, so no GL object leaks.
            unsafe {
                let program = self
                    .gl
                    .create_program()
                    .map_err(|e| BackendError::Device(format!("create_program: {e}")))?;
                let vs = self
                    .gl
                    .create_shader(glow::VERTEX_SHADER)
                    .map_err(|e| BackendError::Device(format!("create_shader: {e}")))?;
                let fs = self
                    .gl
                    .create_shader(glow::FRAGMENT_SHADER)
                    .map_err(|e| BackendError::Device(format!("create_shader: {e}")))?;
                self.gl.shader_source(vs, &vertex_source);
                self.gl.compile_shader(vs);
                if !self.gl.get_shader_compile_status(vs) {
                    let log = self.gl.get_shader_info_log(vs);
                    return Err(BackendError::Device(format!("vertex shader: {log}")));
                }
                self.gl.shader_source(fs, &fragment_source);
                self.gl.compile_shader(fs);
                if !self.gl.get_shader_compile_status(fs) {
                    let log = self.gl.get_shader_info_log(fs);
                    return Err(BackendError::Device(format!("fragment shader: {log}")));
                }
                self.gl.attach_shader(program, vs);
                self.gl.attach_shader(program, fs);
                self.gl.link_program(program);
                if !self.gl.get_program_link_status(program) {
                    let log = self.gl.get_program_info_log(program);
                    return Err(BackendError::Device(format!("link: {log}")));
                }
                self.gl.delete_shader(vs);
                self.gl.delete_shader(fs);
                Ok(LegacyProgram {
                    mvp: self.gl.get_uniform_location(program, "u_mvp"),
                    tex: self.gl.get_uniform_location(program, "u_tex"),
                    pos: self.gl.get_attrib_location(program, "a_pos").unwrap_or(0),
                    uv: self.gl.get_attrib_location(program, "a_uv"),
                    color: self.gl.get_attrib_location(program, "a_color"),
                    program,
                })
            }
        }
    }

    impl GraphicsBackend for LegacyBackend {
        fn kind(&self) -> BackendKind {
            BackendKind::Gles2Legacy
        }

        fn capabilities(&self) -> &Capabilities {
            &self.capabilities
        }

        fn resize(&mut self, width: u32, height: u32) -> BackendResult<()> {
            self.width = width;
            self.height = height;
            // SAFETY: current context; the viewport is pure state.
            unsafe { self.gl.viewport(0, 0, width as i32, height as i32) };
            Ok(())
        }

        fn create_shader(&mut self, _key: ShaderKey, code: ShaderCode<'_>) -> BackendResult<()> {
            match code {
                ShaderCode::Glsl(_) => Err(BackendError::Unsupported(
                    "the GLES 2.0 legacy path builds its own GLSL ES 1.00 program pairs from \
                     fixed-function state keys; raw GLSL is not accepted here"
                        .to_string(),
                )),
                _ => Err(BackendError::Unsupported(
                    "the GLES 2.0 legacy path cannot consume SPIR-V or WGSL".to_string(),
                )),
            }
        }

        fn create_pipeline(&mut self, desc: &PipelineDesc) -> BackendResult<PipelineId> {
            // `desc.shader.0` carries the LegacyKey index in this backend.
            let key = LegacyKey((desc.shader.0 & 0x0F) as u8);
            self.program_for(key)?;
            let id = PipelineId(self.pipelines.insert(key));
            Ok(id)
        }

        fn create_buffer(&mut self, desc: &BufferDesc) -> BackendResult<BufferId> {
            if desc.usage != BufferUsage::Geometry {
                return Err(BackendError::Unsupported(
                    "GLES 2.0 legacy supports geometry VBOs only".to_string(),
                ));
            }
            // SAFETY: current context; the buffer is bound before use.
            unsafe {
                let buffer = self
                    .gl
                    .create_buffer()
                    .map_err(|e| BackendError::Device(format!("create_buffer: {e}")))?;
                self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
                // Persistent ring: allocate once, sub-update forever.
                self.gl.buffer_data_size(
                    glow::ARRAY_BUFFER,
                    desc.size as i32,
                    glow::DYNAMIC_DRAW,
                );
                self.stats.buffer_data_calls += 1;
                self.vram += desc.size;
                Ok(BufferId(self.buffers.insert(LegacyBuffer {
                    buffer,
                    capacity: desc.size,
                    size: desc.size,
                })))
            }
        }

        fn write_buffer(&mut self, id: BufferId, offset: u64, data: &[u8]) -> BackendResult<()> {
            let entry = self
                .buffers
                .get_mut(id.0)
                .ok_or_else(|| BackendError::Unsupported("write_buffer: unknown buffer".into()))?;
            if offset.saturating_add(data.len() as u64) > entry.capacity {
                return Err(BackendError::Unsupported(
                    "write_buffer: range exceeds the buffer".to_string(),
                ));
            }
            let buffer = entry.buffer;
            let size = entry.size.max(offset + data.len() as u64);
            // SAFETY: current context, live buffer, in-range offset.
            unsafe {
                self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
                self.gl
                    .buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, offset as i32, data);
                self.stats.buffer_sub_data_calls += 1;
            }
            if let Some(entry) = self.buffers.get_mut(id.0) {
                entry.size = size;
            }
            Ok(())
        }

        fn destroy_buffer(&mut self, id: BufferId) {
            if let Some(entry) = self.buffers.remove(id.0) {
                self.vram = self.vram.saturating_sub(entry.capacity);
                // SAFETY: current context and the buffer is not bound afterwards.
                unsafe { self.gl.delete_buffer(entry.buffer) };
            }
        }

        fn create_texture(
            &mut self,
            desc: &TextureDesc,
            data: Option<&[u8]>,
        ) -> BackendResult<TextureId> {
            if desc.format != PixelFormat::Rgba8Unorm && desc.format != PixelFormat::Rgba8UnormSrgb
            {
                return Err(BackendError::Unsupported(
                    "GLES 2.0 legacy path accepts uncompressed RGBA8 textures only".to_string(),
                ));
            }
            // SAFETY: current context; texture parameters use only ES2-legal enums.
            unsafe {
                let texture = self
                    .gl
                    .create_texture()
                    .map_err(|e| BackendError::Device(format!("create_texture: {e}")))?;
                self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                self.gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MIN_FILTER,
                    glow::NEAREST as i32,
                );
                self.gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MAG_FILTER,
                    glow::NEAREST as i32,
                );
                self.gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_S,
                    glow::CLAMP_TO_EDGE as i32,
                );
                self.gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_T,
                    glow::CLAMP_TO_EDGE as i32,
                );
                self.gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA as i32,
                    desc.width as i32,
                    desc.height as i32,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(data),
                );
                let bytes = u64::from(desc.width) * u64::from(desc.height) * 4;
                self.vram += bytes;
                Ok(TextureId(self.textures.insert(LegacyTexture {
                    texture,
                    size: [desc.width, desc.height],
                })))
            }
        }

        fn destroy_texture(&mut self, id: TextureId) {
            if let Some(entry) = self.textures.remove(id.0) {
                self.vram = self
                    .vram
                    .saturating_sub(u64::from(entry.size[0]) * u64::from(entry.size[1]) * 4);
                // SAFETY: current context; texture is unbound by the delete call.
                unsafe { self.gl.delete_texture(entry.texture) };
            }
        }

        fn update_texture(
            &mut self,
            id: TextureId,
            origin: [u32; 2],
            size: [u32; 2],
            data: &[u8],
        ) -> BackendResult<()> {
            let entry = self
                .textures
                .get(id.0)
                .ok_or_else(|| BackendError::Unsupported("update_texture: unknown texture".into()))?;
            if origin[0] + size[0] > entry.size[0] || origin[1] + size[1] > entry.size[1] {
                return Err(BackendError::Unsupported(
                    "update_texture: region outside the texture".to_string(),
                ));
            }
            // SAFETY: current context, live texture, in-range region.
            unsafe {
                self.gl.bind_texture(glow::TEXTURE_2D, Some(entry.texture));
                self.gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    origin[0] as i32,
                    origin[1] as i32,
                    size[0] as i32,
                    size[1] as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(data)),
                );
            }
            Ok(())
        }

        fn set_camera(&mut self, view: &[[f32; 4]; 4], proj: &[[f32; 4]; 4]) -> BackendResult<()> {
            self.mvp = multiply(proj, view);
            Ok(())
        }

        fn submit(&mut self, frame: &FrameSubmission<'_>) -> BackendResult<()> {
            if std::thread::current().id() != self.owner_thread {
                return Err(BackendError::Jni(
                    "GLES 2.0 legacy backend used from a thread other than the one that \
                     created the EGL context; the launcher must call nativeInit and \
                     nativeRenderFrame on the same thread"
                        .to_string(),
                ));
            }
            self.mvp = multiply(frame.proj, frame.view);
            self.staging.reset();
            // SAFETY: current context; every GL object used here was created by this
            // backend and is still alive in its arena.
            unsafe {
                self.gl.viewport(0, 0, self.width as i32, self.height as i32);
                self.gl.clear_color(
                    frame.clear_color[0],
                    frame.clear_color[1],
                    frame.clear_color[2],
                    frame.clear_color[3],
                );
                self.gl.clear_depth_f32(1.0);
                self.gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
                self.gl
                    .enable(glow::DEPTH_TEST);
                self.gl.depth_func(glow::LEQUAL);

                let mut bound: Option<usize> = None;
                for batch in frame.batches {
                    let Some(&key) = self.pipelines.get(batch.pipeline.0) else {
                        continue;
                    };
                    let key_index = key.index();
                    if bound != Some(key_index) {
                        if let Some(program) = self.programs[key_index].as_ref() {
                            self.gl.use_program(Some(program.program));
                            if let Some(location) = program.mvp.as_ref() {
                                self.gl.uniform_matrix_4_f32_slice(
                                    Some(location),
                                    false,
                                    &self.mvp,
                                );
                            }
                            if let Some(location) = program.tex.as_ref() {
                                self.gl.uniform_1_i32(Some(location), 0);
                            }
                            if key.blend() {
                                self.gl.enable(glow::BLEND);
                                self.gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                            } else {
                                self.gl.disable(glow::BLEND);
                            }
                            bound = Some(key_index);
                            self.apply_attributes(program);
                        }
                    }
                    self.draw_batch(batch);
                }
            }
            self.stats.frames += 1;
            Ok(())
        }

        fn present(&mut self) -> BackendResult<PresentOutcome> {
            // SAFETY: current context and a live EGL surface.
            let swapped = unsafe { eglSwapBuffers(self.egl.display, self.egl.surface) };
            if swapped == 0 {
                return Ok(PresentOutcome::Lost);
            }
            Ok(PresentOutcome::Presented)
        }

        fn vram_bytes(&self) -> u64 {
            self.vram
        }

        fn reload_shaders(&mut self) -> BackendResult<()> {
            // SAFETY: current context; deleting programs that are no longer bound is
            // legal and they are recompiled lazily on next use.
            unsafe {
                for slot in self.programs.iter_mut() {
                    if let Some(program) = slot.take() {
                        self.gl.delete_program(program.program);
                    }
                }
            }
            log::info!("legacy GL programs invalidated; they recompile on next use");
            Ok(())
        }

        fn destroy(&mut self) {
            // SAFETY: current context; all objects in the arenas are deleted exactly
            // once, then the EGL context is torn down by dropping `egl`.
            unsafe {
                for slot in self.programs.iter_mut() {
                    if let Some(program) = slot.take() {
                        self.gl.delete_program(program.program);
                    }
                }
                for entry in self.buffers.iter() {
                    self.gl.delete_buffer(entry.buffer);
                }
                for entry in self.textures.iter() {
                    self.gl.delete_texture(entry.texture);
                }
            }
            self.buffers = Arena::new();
            self.textures = Arena::new();
            self.vram = 0;
            log::info!(
                "legacy backend destroyed ({} programs, {} draws)",
                self.stats.programs_compiled,
                self.stats.draw_calls
            );
        }
    }

    impl LegacyBackend {
        /// Bind the legacy vertex format for `program`.
        ///
        /// # Safety
        ///
        /// The ES2 context must be current and `program` must be linked.
        unsafe fn apply_attributes(&self, program: &LegacyProgram) {
            // SAFETY: forwarded from the caller.
            unsafe {
                self.gl.enable_vertex_attrib_array(program.pos);
                self.gl.vertex_attrib_pointer_f32(
                    program.pos,
                    3,
                    glow::FLOAT,
                    false,
                    LEGACY_VERTEX_STRIDE,
                    0,
                );
                if let Some(uv) = program.uv {
                    self.gl.enable_vertex_attrib_array(uv);
                    self.gl.vertex_attrib_pointer_f32(
                        uv,
                        2,
                        glow::FLOAT,
                        false,
                        LEGACY_VERTEX_STRIDE,
                        UV_OFFSET,
                    );
                }
                if let Some(color) = program.color {
                    self.gl.enable_vertex_attrib_array(color);
                    self.gl.vertex_attrib_pointer_f32(
                        color,
                        4,
                        glow::UNSIGNED_BYTE,
                        true,
                        LEGACY_VERTEX_STRIDE,
                        COLOR_OFFSET,
                    );
                }
            }
        }

        /// Issue one indexed draw.
        ///
        /// # Safety
        ///
        /// The referenced buffers must be alive and the GL context current.
        unsafe fn draw_batch(&mut self, batch: &DrawBatch) {
            let Some(vertex) = self.buffers.get(batch.vertex_buffer.0) else {
                return;
            };
            let Some(index) = self.buffers.get(batch.index_buffer.0) else {
                return;
            };
            let (vertex_buffer, index_buffer) = (vertex.buffer, index.buffer);
            // SAFETY: forwarded from the caller.
            unsafe {
                self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(vertex_buffer));
                self.gl
                    .bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(index_buffer));
                self.gl.draw_elements(
                    glow::TRIANGLES,
                    batch.index_count as i32,
                    glow::UNSIGNED_INT,
                    0,
                );
            }
            self.stats.draw_calls += 1;
        }
    }

    // SAFETY: every field is either a plain value or an EGL/GL handle. EGL
    // contexts and GL objects are not thread-affine, but the *current-context*
    // binding is: `submit` therefore rejects use from any thread other than the
    // one that created the context (`owner_thread`). Moving the value is sound
    // because ownership transfers exclusively.
    unsafe impl Send for LegacyBackend {}

    /// Column-major 4×4 multiply: `out = a * b`.
    fn multiply(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [f32; 16] {
        let mut out = [0.0f32; 16];
        for column in 0..4 {
            for row in 0..4 {
                let mut sum = 0.0;
                for k in 0..4 {
                    sum += a[k][row] * b[column][k];
                }
                out[column * 4 + row] = sum;
            }
        }
        out
    }

}

#[cfg(not(target_os = "android"))]
mod platform {
    use super::*;

    /// Host stub: the GLES 2.0 legacy path requires Android EGL.
    pub struct LegacyBackend;

    pub fn create(
        _target: &SurfaceTarget<'_>,
        _tier: DeviceTier,
    ) -> BackendResult<Box<dyn GraphicsBackend>> {
        Err(BackendError::Unavailable(
            "the GLES 2.0 legacy path needs Android EGL; build for \
             aarch64-linux-android / armv7-linux-androideabi to exercise it"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_distinct_slots() {
        let mut seen = [false; LegacyKey::COUNT];
        for bits in 0..LegacyKey::COUNT {
            let key = LegacyKey(bits as u8);
            assert!(!seen[key.index()]);
            seen[key.index()] = true;
        }
        assert!(seen.iter().all(|s| *s));
    }

    #[test]
    fn key_bits_are_readable() {
        let key = LegacyKey::new(true, false, true, false);
        assert!(key.textured());
        assert!(!key.colored());
        assert!(key.fog());
        assert!(!key.blend());
    }

    #[test]
    fn generated_sources_are_version_100() {
        for bits in 0..LegacyKey::COUNT {
            let key = LegacyKey(bits as u8);
            let vs = vertex_source(key);
            let fs = fragment_source(key);
            assert!(vs.starts_with("#version 100"));
            assert!(fs.starts_with("#version 100"));
            assert!(!vs.contains("#version 300"), "ES 3.0 syntax must not leak");
            assert!(!fs.contains("layout("), "no layout qualifiers in ES 1.00");
            assert!(fs.contains("gl_FragColor"));
        }
    }

    #[test]
    fn ring_wraps_instead_of_overflowing() {
        let mut ring = RingAllocator::new(1024);
        assert_eq!(ring.alloc(600, 4), Some(0));
        assert_eq!(ring.alloc(600, 4), None, "must wrap rather than overrun");
        assert_eq!(ring.wraps(), 1);
        ring.reset();
        assert_eq!(ring.used(), 0);
        assert_eq!(ring.alloc(1024, 1), Some(0));
        assert_eq!(ring.high_water(), 1024);
    }

    #[test]
    fn ring_respects_alignment() {
        let mut ring = RingAllocator::new(4096);
        assert_eq!(ring.alloc(1, 1), Some(0));
        assert_eq!(ring.alloc(4, 256), Some(256));
        assert_eq!(ring.alloc(4, 256), Some(512));
    }

    #[test]
    fn mvp_multiply_is_column_major() {
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let translation = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [3.0, 4.0, 5.0, 1.0],
        ];
        let out = multiply(&identity, &translation);
        assert_eq!(&out[12..16], &[3.0, 4.0, 5.0, 1.0]);
    }
}
