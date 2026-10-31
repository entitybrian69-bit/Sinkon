//! Backend abstraction.
//!
//! # Why two traits instead of one
//!
//! The brief asks for a `GraphicsBackend` trait with
//! `create_surface` / `create_device_and_queues` / … on a single object. That
//! shape is not expressible for Vulkan or EGL: the surface must exist *before*
//! an adapter can be selected (`compatible_surface`), and the adapter must exist
//! before a device can be requested. A trait object therefore cannot exist until
//! device creation has already succeeded.
//!
//! Sinkon splits the contract in the only way that maps onto both APIs:
//!
//! * [`backend::create`] — the equivalent of `create_surface` +
//!   `create_device_and_queues`, one call per backend module. It returns the
//!   live [`GraphicsBackend`].
//! * [`GraphicsBackend`] — everything that happens *after* device creation:
//!   `create_pipeline`, `create_buffer`, `create_texture`, `submit`, `present`,
//!   `capabilities`, `destroy`.
//!
//! The ordering difference is documented in `docs/ARCHITECTURE.md` §"Trait
//! shape". Every other method name and semantic is exactly as specified.
//!
//! # Dispatch
//!
//! All higher layers (chunk upload, culling, shader emission, JNI bridge) talk
//! to `Box<dyn GraphicsBackend>` only. There is no `match backend_kind` outside
//! this module except for the documented capability differences in
//! [`BackendFeatures`].

use std::fmt;

use serde::Serialize;

use crate::surface::SurfaceTarget;
use crate::version_profile::VersionProfile;

pub mod gles2_legacy;
pub mod gles3;
pub mod vulkan;
mod wgpu_shared;

pub use wgpu_shared::{AdapterProbe, WgpuBackend};

/// Result alias used across the (fallible) device-creation paths.
pub type BackendResult<T> = std::result::Result<T, BackendError>;

/// Which rendering path is (or would be) in use.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// `wgpu::Backends::VULKAN` (Vulkan 1.1+).
    Vulkan,
    /// `wgpu::Backends::GL` (GLES 3.0/3.1 via EGL).
    Gles3,
    /// `glow` + EGL, GLES 2.0/2.1 fixed-function-era emulation.
    Gles2Legacy,
}

impl BackendKind {
    /// Every backend, in `Sinkon (Auto)` preference order for modern versions.
    pub const ALL: [BackendKind; 3] = [
        BackendKind::Vulkan,
        BackendKind::Gles3,
        BackendKind::Gles2Legacy,
    ];

    /// Stable lowercase name used in JSON, logs and bench ids.
    pub const fn as_str(self) -> &'static str {
        match self {
            BackendKind::Vulkan => "vulkan",
            BackendKind::Gles3 => "gles3",
            BackendKind::Gles2Legacy => "gles2",
        }
    }

    /// Prefix used for criterion bench ids (`vulkan_`, `gles3_`, `gles2_`).
    pub const fn bench_prefix(self) -> &'static str {
        match self {
            BackendKind::Vulkan => "vulkan_",
            BackendKind::Gles3 => "gles3_",
            BackendKind::Gles2Legacy => "gles2_",
        }
    }

    /// The `backendHint` value accepted by `nativeInit`.
    pub const fn hint(self) -> i32 {
        match self {
            BackendKind::Vulkan => 1,
            BackendKind::Gles3 => 2,
            BackendKind::Gles2Legacy => 3,
        }
    }

    /// Decode a `backendHint`. `0` (auto) returns `None`.
    pub fn from_hint(hint: i32) -> Option<Self> {
        match hint {
            1 => Some(BackendKind::Vulkan),
            2 => Some(BackendKind::Gles3),
            3 => Some(BackendKind::Gles2Legacy),
            _ => None,
        }
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Feature/capability surface that the rest of Sinkon is allowed to query.
///
/// This is the *only* thing the shader layer and the renderer branch on, which
/// is what keeps the per-frame code backend-agnostic.
#[derive(Clone, Debug, Serialize)]
pub struct BackendFeatures {
    /// Compute shaders usable for GPU culling / light culling.
    pub compute: bool,
    /// `draw_indirect` / `multi_draw_indirect` support.
    pub indirect_draw: bool,
    /// Push constants (per-draw transforms without a UBO bind).
    pub push_constants: bool,
    /// Timestamp queries (GPU frame timing).
    pub timestamp_query: bool,
    /// Backend can persist a compiled pipeline cache to disk.
    pub pipeline_cache: bool,
    /// Occlusion queries usable for chunk visibility.
    pub occlusion_query: bool,
    /// ASTC compressed textures can be sampled directly.
    pub astc: bool,
    /// BCn compressed textures can be sampled directly.
    pub bc: bool,
    /// Anisotropic filtering.
    pub anisotropy: bool,
    /// Storage buffers (compute or vertex-stage reads).
    pub storage_buffers: bool,
    /// `glMapBufferRange` with `MAP_UNSYNCHRONIZED_BIT`.
    pub map_unsynchronized: bool,
    /// Render bundles / display lists for static chunk geometry.
    pub render_bundles: bool,
}

impl BackendFeatures {
    /// A conservative feature set: everything off.
    pub const NONE: BackendFeatures = BackendFeatures {
        compute: false,
        indirect_draw: false,
        push_constants: false,
        timestamp_query: false,
        pipeline_cache: false,
        occlusion_query: false,
        astc: false,
        bc: false,
        anisotropy: false,
        storage_buffers: false,
        map_unsynchronized: false,
        render_bundles: false,
    };

    /// The GLES 2.0 legacy feature set: nothing above GL ES 2.0.
    pub const GLES2: BackendFeatures = BackendFeatures {
        compute: false,
        indirect_draw: false,
        push_constants: false,
        timestamp_query: false,
        pipeline_cache: false,
        occlusion_query: false,
        astc: false,
        bc: false,
        anisotropy: false,
        storage_buffers: false,
        map_unsynchronized: false,
        render_bundles: false,
    };
}

/// Capability JSON exposed to the launcher and the shader layer.
///
/// Serialised by `nativeGetCapabilityReport`; identical schema on all backends
/// so the Kotlin side never has to special-case.
#[derive(Clone, Debug, Serialize)]
pub struct Capabilities {
    /// Active backend.
    pub kind: BackendKind,
    /// Human readable API name, e.g. `Vulkan` / `OpenGL ES`.
    pub api_name: String,
    /// API version string, e.g. `1.3.0` / `3.2`.
    pub api_version: String,
    /// Desktop-GL version when the GL backend reports one.
    pub gl_version: Option<String>,
    /// GLES major.minor packed as `major * 10 + minor` (e.g. `32` = 3.2).
    pub gles_version: Option<u32>,
    /// Vulkan API version packed as `major << 22 | minor << 12` when known.
    pub vulkan_api: Option<u32>,
    /// GLSL dialect Sinkon emits for this backend (`450`, `320 es`, `100`).
    pub glsl_version: String,
    /// Adapter/GPU name.
    pub adapter_name: String,
    /// Driver string as reported by the adapter.
    pub driver_info: String,
    /// Largest 2D texture edge, in texels.
    pub max_texture_size: u32,
    /// Largest uniform buffer binding, in bytes.
    pub max_uniform_buffer_binding_size: u32,
    /// Max compute workgroup size per axis (all zero when unsupported).
    pub max_compute_workgroup_size: [u32; 3],
    /// Feature flags.
    pub supports: BackendFeatures,
    /// Extension/feature names advertised by the driver.
    pub extensions: Vec<String>,
    /// Tier S…C as detected at init; see `perf::tier`.
    pub device_tier: String,
    /// Total VRAM Sinkon currently owns, in bytes.
    pub vram_bytes: u64,
    /// Number of chunk sections resident in GPU buffers.
    pub resident_sections: u32,
}

impl Capabilities {
    /// Serialise to the JSON string returned by the JNI bridge.
    ///
    /// Serialisation failure cannot happen for this struct (no non-finite
    /// floats, no map keys), but the function is still total: it falls back to a
    /// minimal, valid JSON document.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                "{{\"active_backend\":\"{}\",\"error\":\"capability serialise failed\"}}",
                self.kind.as_str()
            )
        })
    }
}

/// Opaque pipeline handle owned by a backend.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
pub struct PipelineId(pub u32);

/// Opaque buffer handle owned by a backend.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
pub struct BufferId(pub u32);

/// Opaque texture handle owned by a backend.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
pub struct TextureId(pub u32);

/// Opaque bind-group layout handle owned by a backend.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
pub struct LayoutId(pub u32);

/// Opaque shader-module handle owned by a backend.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
pub struct ShaderKey(pub u32);

/// Shader code handed to a backend.
///
/// `wgpu` backends (Vulkan and GL) consume SPIR-V: `wgpu-hal`'s GL backend runs
/// naga's GLSL writer internally. GLSL is only passed directly to the `glow`
/// legacy path, which cannot consume SPIR-V.
#[derive(Clone, Copy, Debug)]
pub enum ShaderCode<'a> {
    /// SPIR-V words produced by `shader::emit_spirv`.
    SpirV(&'a [u32]),
    /// GLSL source (`#version 300 es` / `#version 100`) for the `glow` path.
    Glsl(&'a str),
    /// WGSL source, used by unit-test fixtures and the debug overlay.
    Wgsl(&'a str),
}

/// Pixel formats Sinkon asks for. Backends translate these to their own enums.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    /// Linear RGBA8, the universal fallback.
    Rgba8Unorm,
    /// sRGB RGBA8 for albedo atlases.
    Rgba8UnormSrgb,
    /// BC7 (desktop-compatible blocks, Vulkan on Adreno).
    Bc7RgbaUnorm,
    /// ASTC 4x4 (Adreno/Mali/Xclipse).
    Astc4x4Unorm,
    /// Depth-only attachment.
    Depth32Float,
}

impl PixelFormat {
    /// Bytes per texel for uncompressed formats (0 for compressed/depth).
    pub const fn bytes_per_texel(self) -> u32 {
        match self {
            PixelFormat::Rgba8Unorm | PixelFormat::Rgba8UnormSrgb => 4,
            PixelFormat::Bc7RgbaUnorm | PixelFormat::Astc4x4Unorm => 1,
            PixelFormat::Depth32Float => 0,
        }
    }

    /// Smallest device page / copy alignment that matters for this format.
    pub const fn block_bytes(self) -> u32 {
        match self {
            PixelFormat::Bc7RgbaUnorm => 16,
            PixelFormat::Astc4x4Unorm => 16,
            PixelFormat::Depth32Float => 4,
            _ => 4,
        }
    }
}

/// How a buffer will be used. One variant per usage Sinkon actually needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BufferUsage {
    /// Vertex + index data for chunk geometry.
    Geometry,
    /// Per-frame uniform ring slot.
    Uniform,
    /// Indirect draw arguments written by GPU culling.
    Indirect,
    /// CPU staging for texture uploads.
    Staging,
    /// Storage buffer (compute queue).
    Storage,
}

/// Buffer creation request.
#[derive(Clone, Debug)]
pub struct BufferDesc {
    /// Debug label (static, never formatted per frame).
    pub label: &'static str,
    /// Size in bytes.
    pub size: u64,
    /// Usage class.
    pub usage: BufferUsage,
    /// Ask for a persistently mapped allocation when the backend can provide it.
    pub persistent_map: bool,
}

/// Texture creation request.
#[derive(Clone, Debug)]
pub struct TextureDesc {
    /// Debug label.
    pub label: &'static str,
    /// Width in texels.
    pub width: u32,
    /// Height in texels.
    pub height: u32,
    /// Pixel format.
    pub format: PixelFormat,
    /// Mip levels (>= 1). Mips are generated once at upload, never per frame.
    pub mip_levels: u32,
    /// Sampler uses linear filtering + mipmaps.
    pub linear: bool,
}

/// Depth/blend state preset for a pipeline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlendMode {
    /// No blending, depth write on.
    Opaque,
    /// Alpha blend over, depth write off.
    AlphaBlend,
    /// Additive blend (translucency/shadows).
    Additive,
}

/// Primitive topology.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Topology {
    /// Triangle list (chunk geometry, terrain shaders).
    TriangleList,
    /// Triangle strip (fullscreen passes).
    TriangleStrip,
    /// Line list (debug overlays).
    LineList,
}

/// Pipeline creation request.
#[derive(Clone, Debug)]
pub struct PipelineDesc {
    /// Debug label.
    pub label: &'static str,
    /// Vertex entry point name.
    pub vertex_entry: &'static str,
    /// Fragment entry point name.
    pub fragment_entry: &'static str,
    /// Colour attachment formats, in order.
    pub color_formats: &'static [PixelFormat],
    /// Depth attachment format when depth-tested.
    pub depth_format: Option<PixelFormat>,
    /// Blend preset.
    pub blend: BlendMode,
    /// Topology.
    pub topology: Topology,
    /// Vertex stride in bytes (0 = fullscreen pass).
    pub vertex_stride: u64,
    /// Bind-group layout used by the pipeline.
    pub layout: LayoutId,
    /// Shader module to compile the pipeline from.
    pub shader: ShaderKey,
}

/// One draw call in a submitted frame.
#[derive(Clone, Copy, Debug)]
pub struct DrawBatch {
    /// Pipeline to bind.
    pub pipeline: PipelineId,
    /// Vertex buffer (offset 0).
    pub vertex_buffer: BufferId,
    /// Index buffer.
    pub index_buffer: BufferId,
    /// Indices in this batch.
    pub index_count: u32,
    /// Instances in this batch.
    pub instance_count: u32,
    /// First index.
    pub first_index: u32,
    /// Signed vertex offset.
    pub base_vertex: i32,
    /// Packed section key for tracing, `0` when unused.
    pub section_tag: u32,
}

/// Everything the render thread hands to the backend for one frame.
///
/// Borrowed, never owned: the submission itself allocates nothing.
pub struct FrameSubmission<'a> {
    /// Column-major view matrix.
    pub view: &'a [[f32; 4]; 4],
    /// Column-major projection matrix.
    pub proj: &'a [[f32; 4]; 4],
    /// Camera world position (xyz).
    pub camera_pos: [f32; 3],
    /// Batches to draw, already culled and sorted.
    pub batches: &'a [DrawBatch],
    /// Clear colour (linear).
    pub clear_color: [f32; 4],
    /// Frame index, used for ring-buffer slot rotation.
    pub frame_index: u64,
}

/// What `present` did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresentOutcome {
    /// Frame presented normally.
    Presented,
    /// Surface needs a reconfigure before the next frame (resize/rotation).
    Outdated,
    /// Surface was lost; the launcher must recreate it.
    Lost,
}

/// Errors that can cross the backend boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendError {
    /// Backend cannot run on this device at all.
    Unavailable(String),
    /// Surface (ANativeWindow/EGLSurface) creation failed.
    Surface(String),
    /// No adapter matched the requested backend.
    Adapter(String),
    /// Device/queue request failed.
    Device(String),
    /// Backend lacks a feature the caller asked for.
    Unsupported(String),
    /// Out of memory / pool exhausted.
    ResourceExhausted(String),
    /// `present` failed.
    Present(String),
    /// Device lost; the renderer must be torn down and re-created.
    DeviceLost(String),
    /// JNI-side failure (null handle, bad buffer, wrong thread).
    Jni(String),
    /// A caller supplied an argument Sinkon cannot accept (wrong size, bad id).
    InvalidArgument(String),
    /// Filesystem failure while loading a shaderpack or pipeline cache.
    Io(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::Unavailable(m) => write!(f, "backend unavailable: {m}"),
            BackendError::Surface(m) => write!(f, "surface error: {m}"),
            BackendError::Adapter(m) => write!(f, "adapter error: {m}"),
            BackendError::Device(m) => write!(f, "device error: {m}"),
            BackendError::Unsupported(m) => write!(f, "unsupported: {m}"),
            BackendError::ResourceExhausted(m) => write!(f, "out of resources: {m}"),
            BackendError::Present(m) => write!(f, "present error: {m}"),
            BackendError::DeviceLost(m) => write!(f, "device lost: {m}"),
            BackendError::Jni(m) => write!(f, "jni error: {m}"),
            BackendError::InvalidArgument(m) => write!(f, "invalid argument: {m}"),
            BackendError::Io(m) => write!(f, "io error: {m}"),
        }
    }
}

impl std::error::Error for BackendError {}

impl BackendError {
    /// Log the error under the `Sinkon` tag and return it unchanged so callers
    /// can still propagate it with `?`.
    pub fn log(self, context: &str) -> Self {
        log::error!("[{context}] {self}");
        self
    }
}

/// The live backend.
///
/// Implementations are `Send` so the render thread can own them exclusively
/// while worker threads only ever touch CPU-side mesh data.
pub trait GraphicsBackend: Send {
    /// Which backend this is.
    fn kind(&self) -> BackendKind;

    /// Immutable capability snapshot for this backend.
    fn capabilities(&self) -> &Capabilities;

    /// Reconfigure the swapchain/surface to a new size.
    fn resize(&mut self, width: u32, height: u32) -> BackendResult<()>;

    /// Upload/compile a shader module under `key`. Never called on the frame path.
    fn create_shader(&mut self, key: ShaderKey, code: ShaderCode<'_>) -> BackendResult<()>;

    /// Create a pipeline. Never called on the frame path.
    fn create_pipeline(&mut self, desc: &PipelineDesc) -> BackendResult<PipelineId>;

    /// Create a buffer. Never called on the frame path for steady-state frames.
    fn create_buffer(&mut self, desc: &BufferDesc) -> BackendResult<BufferId>;

    /// Upload `data` into `offset` of an existing buffer.
    fn write_buffer(&mut self, id: BufferId, offset: u64, data: &[u8]) -> BackendResult<()>;

    /// Destroy a buffer and release its arena slot.
    fn destroy_buffer(&mut self, id: BufferId);

    /// Create a texture, optionally uploading mip 0 from `data` immediately.
    fn create_texture(&mut self, desc: &TextureDesc, data: Option<&[u8]>) -> BackendResult<TextureId>;

    /// Destroy a texture and release its VRAM accounting.
    fn destroy_texture(&mut self, id: TextureId);

    /// Update a rectangular region of an existing texture (hot path for atlas
    /// stitching; must not allocate).
    fn update_texture(
        &mut self,
        id: TextureId,
        origin: [u32; 2],
        size: [u32; 2],
        data: &[u8],
    ) -> BackendResult<()>;

    /// Push the camera matrices into the next uniform ring slot.
    fn set_camera(&mut self, view: &[[f32; 4]; 4], proj: &[[f32; 4]; 4]) -> BackendResult<()>;

    /// Record and submit one frame. This is the hot path: no allocations.
    fn submit(&mut self, frame: &FrameSubmission<'_>) -> BackendResult<()>;

    /// Present the submitted frame.
    fn present(&mut self) -> BackendResult<PresentOutcome>;

    /// Bytes of GPU memory Sinkon currently owns (textures + buffers).
    fn vram_bytes(&self) -> u64;

    /// Invalidate compiled shader state (hot reload).
    ///
    /// Backends that compile programs themselves override this; `wgpu` backends
    /// take new `create_shader` calls instead, so their default is a no-op.
    fn reload_shaders(&mut self) -> BackendResult<()> {
        Ok(())
    }

    /// Release every GPU resource. After this call the object must not be used.
    fn destroy(&mut self);
}

/// One probe result, recorded so the launcher can see *why* a backend was or was
/// not chosen.
#[derive(Clone, Debug, Serialize)]
pub struct BackendAttempt {
    /// Backend that was tried.
    pub backend: BackendKind,
    /// Whether it produced a usable device.
    pub ok: bool,
    /// Human readable reason (adapter name on success, error on failure).
    pub reason: String,
}

/// Record of the backend negotiation, embedded in `nativeGetCapabilityReport`.
#[derive(Clone, Debug, Serialize)]
pub struct SelectionRecord {
    /// `backendHint` the launcher passed.
    pub requested_hint: i32,
    /// Backend that ended up active (`None` means hard fail).
    pub active: Option<BackendKind>,
    /// Every backend that was tried, in order.
    pub attempted: Vec<BackendAttempt>,
    /// Version profile used to build the plan.
    pub version_profile: String,
    /// `mcVersion` string as passed by the launcher.
    pub mc_version: String,
    /// True when the active backend is not the first choice of the plan, i.e.
    /// Sinkon fell back. Always logged at `warn` level.
    pub degraded: bool,
}

impl SelectionRecord {
    /// Backends that successfully produced a device during negotiation.
    pub fn available(&self) -> Vec<BackendKind> {
        self.attempted
            .iter()
            .filter(|a| a.ok)
            .map(|a| a.backend)
            .collect()
    }
}

/// Build the ordered backend plan for a hint + version profile.
///
/// `hint` values other than `0` collapse the plan to a single forced backend.
///
/// For the **Legacy** profile (Alpha–1.6) the plan starts with the `glow` GLES
/// 2.0 path because those versions use fixed-function-era GL calls that a core
/// GLES 3.0 profile cannot express. For every other profile the plan follows the
/// `Sinkon (Auto)` order from the brief: Vulkan → GLES 3.x → GLES 2.0 legacy.
pub fn selection_plan(hint: i32, profile: VersionProfile) -> Vec<BackendKind> {
    if let Some(forced) = BackendKind::from_hint(hint) {
        return vec![forced];
    }
    match profile {
        VersionProfile::Legacy => vec![
            BackendKind::Gles2Legacy,
            BackendKind::Gles3,
            BackendKind::Vulkan,
        ],
        _ => BackendKind::ALL.to_vec(),
    }
}

/// Create a backend of a specific kind on `target`.
///
/// This is the `create_surface` + `create_device_and_queues` half of the
/// contract; see the module docs for why it is a free function.
pub fn create(
    target: &SurfaceTarget<'_>,
    kind: BackendKind,
    tier: crate::perf::tier::DeviceTier,
) -> BackendResult<Box<dyn GraphicsBackend>> {
    match kind {
        BackendKind::Vulkan => vulkan::create(target, tier),
        BackendKind::Gles3 => gles3::create(target, tier),
        BackendKind::Gles2Legacy => gles2_legacy::create(target, tier),
    }
}

/// Run the negotiation and return the first backend that works.
///
/// This never panics and never silently downgrades: every attempt is recorded in
/// the returned [`SelectionRecord`], and a fallback is logged at `warn` level
/// under the `Sinkon` tag.
pub fn select(
    target: &SurfaceTarget<'_>,
    hint: i32,
    mc_version: &str,
    tier: crate::perf::tier::DeviceTier,
) -> (Option<Box<dyn GraphicsBackend>>, SelectionRecord) {
    let profile = crate::version_profile::classify(mc_version);
    let plan = selection_plan(hint, profile);
    let mut record = SelectionRecord {
        requested_hint: hint,
        active: None,
        attempted: Vec::with_capacity(plan.len()),
        version_profile: profile.as_str().to_string(),
        mc_version: mc_version.to_string(),
        degraded: false,
    };

    let started = std::time::Instant::now();
    let mut backend: Option<Box<dyn GraphicsBackend>> = None;
    for (index, kind) in plan.iter().copied().enumerate() {
        match create(target, kind, tier) {
            Ok(created) => {
                let caps = created.capabilities();
                log::info!(
                    "backend selected: {} adapter=\"{}\" api={} glsl={}",
                    kind,
                    caps.adapter_name,
                    caps.api_version,
                    caps.glsl_version
                );
                record.attempted.push(BackendAttempt {
                    backend: kind,
                    ok: true,
                    reason: format!("{} | {}", caps.adapter_name, caps.api_version),
                });
                record.active = Some(kind);
                record.degraded = index > 0;
                if record.degraded {
                    log::warn!(
                        "backend fallback: first choice {} unavailable, running on {} (mcVersion={})",
                        plan[0],
                        kind,
                        mc_version
                    );
                }
                backend = Some(created);
                break;
            }
            Err(err) => {
                log::warn!("backend {kind} unavailable: {err}");
                record.attempted.push(BackendAttempt {
                    backend: kind,
                    ok: false,
                    reason: err.to_string(),
                });
            }
        }
    }

    let elapsed = started.elapsed();
    log::info!(
        "backend selection took {:.2} ms ({} candidate(s))",
        elapsed.as_secs_f64() * 1000.0,
        plan.len()
    );
    if record.active.is_none() {
        log::error!(
            "no usable graphics backend: vulkan/gles3/gles2 all failed — Sinkon cannot render on this device"
        );
    }
    (backend, record)
}

/// A tiny arena with a free list.
///
/// Used for backend resources (buffers, textures, pipelines) so that creating and
/// destroying them during chunk streaming never allocates on the frame path:
/// slots are reused and the backing `Vec` only grows during load.
#[derive(Debug)]
pub(crate) struct Arena<T> {
    slots: Vec<Option<T>>,
    free: Vec<u32>,
}

impl<T> Arena<T> {
    pub(crate) fn new() -> Self {
        Arena {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    pub(crate) fn insert(&mut self, value: T) -> u32 {
        if let Some(index) = self.free.pop() {
            self.slots[index as usize] = Some(value);
            index
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Some(value));
            index
        }
    }

    pub(crate) fn get(&self, index: u32) -> Option<&T> {
        self.slots.get(index as usize).and_then(|slot| slot.as_ref())
    }

    pub(crate) fn get_mut(&mut self, index: u32) -> Option<&mut T> {
        self.slots
            .get_mut(index as usize)
            .and_then(|slot| slot.as_mut())
    }

    pub(crate) fn remove(&mut self, index: u32) -> Option<T> {
        let slot = self.slots.get_mut(index as usize)?;
        let taken = slot.take();
        if taken.is_some() {
            self.free.push(index);
        }
        taken
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(|slot| slot.as_ref())
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_reuses_slots() {
        let mut arena: Arena<u32> = Arena::new();
        let a = arena.insert(1);
        let b = arena.insert(2);
        assert_eq!(arena.len(), 2);
        assert_eq!(arena.remove(a), Some(1));
        let c = arena.insert(3);
        assert_eq!(c, a, "freed slot must be reused");
        assert_eq!(arena.get(b), Some(&2));
        assert_eq!(arena.len(), 2);
    }

    #[test]
    fn forced_hint_collapses_plan() {
        let plan = selection_plan(1, VersionProfile::Legacy);
        assert_eq!(plan, vec![BackendKind::Vulkan]);
        let plan = selection_plan(2, VersionProfile::Current);
        assert_eq!(plan, vec![BackendKind::Gles3]);
    }

    #[test]
    fn legacy_plan_prefers_glow() {
        let plan = selection_plan(0, VersionProfile::Legacy);
        assert_eq!(plan[0], BackendKind::Gles2Legacy);
        let plan = selection_plan(0, VersionProfile::Current);
        assert_eq!(plan[0], BackendKind::Vulkan);
    }

    #[test]
    fn capability_json_is_valid() {
        let caps = Capabilities {
            kind: BackendKind::Vulkan,
            api_name: "Vulkan".into(),
            api_version: "1.3.0".into(),
            gl_version: None,
            gles_version: None,
            vulkan_api: Some(1_048_576),
            glsl_version: "450".into(),
            adapter_name: "Adreno (TM) 740".into(),
            driver_info: "Qualcomm".into(),
            max_texture_size: 16384,
            max_uniform_buffer_binding_size: 65536,
            max_compute_workgroup_size: [128, 1, 1],
            supports: BackendFeatures::NONE,
            extensions: vec!["VK_KHR_swapchain".into()],
            device_tier: "S".into(),
            vram_bytes: 0,
            resident_sections: 0,
        };
        let json = caps.to_json();
        assert!(json.contains("\"active_backend\"") || json.contains("\"kind\":\"vulkan\""));
        assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
    }
}
