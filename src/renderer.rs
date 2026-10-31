//! The renderer: backend negotiation, chunk streaming, frame submission.
//!
//! One [`SinkonRenderer`] per `nativeInit`. Everything below this module is
//! backend-agnostic: the frame body only touches [`GraphicsBackend`] and the
//! CPU-side subsystems.
//!
//! # Frame shape
//!
//! ```text
//!   drain mesh results ─▶ upload to pooled VBOs ─▶ frustum + occlusion cull
//!      ─▶ build DrawBatch list (preallocated) ─▶ backend.submit ─▶ present
//! ```
//!
//! The `alloc-gate` feature measures Sinkon's own scope: `render_frame` reports
//! zero allocations for the drain/cull/batch logic. `backend.submit` is called
//! outside that gate because `wgpu` allocates its encoder and swapchain view per
//! frame; that number is reported separately as `backend_allocations` rather than
//! hidden. See `docs/PERFORMANCE.md` §5.

use std::path::PathBuf;
use std::time::Instant;

use serde::Serialize;

use crate::backend::{
    self, BackendError, BackendKind, BackendResult, BufferDesc, BufferId, BufferUsage,
    Capabilities, DrawBatch, FrameSubmission, GraphicsBackend, LayoutId, PipelineDesc, PipelineId,
    PixelFormat, PresentOutcome, SelectionRecord, ShaderCode, ShaderKey, TextureDesc, TextureId,
    Topology, BlendMode,
};
use crate::chunk::{
    cull_sections, CullScratch, MeshFormat, MeshJob, MeshPool, MeshPoolStats, MeshResult,
    MeshWorkers, SectionInstance, SectionKey, SECTION_BLOCKS, SECTION_SIZE,
};
use crate::perf::alloc;
use crate::perf::tier::{self, DeviceTier};
use crate::perf::PerfRegistry;
use crate::shader::{self, ShaderCache, ShaderStage, ShaderTarget, ShaderpackKind};
use crate::surface::{SurfaceHandle, SurfaceTarget};
use crate::version_profile::{self, VersionProfile};

/// Bytes reserved per pooled vertex buffer.
///
/// Sized for a realistic 16³ section (200–2 500 visible faces → 16–80 KiB at the
/// 20/24-byte vertex formats) with headroom for noisy terrain. A section whose
/// mesh exceeds this is **not** silently clipped: it is skipped, counted in
/// `PerfSnapshot`'s `dropped_sections` and logged at `warn` (SINKON-016).
pub const SLOT_VERTEX_BYTES: u64 = 96 * 1024;
/// Bytes reserved per pooled index buffer (6 `u32` indices per visible face).
pub const SLOT_INDEX_BYTES: u64 = 48 * 1024;

/// Total GPU bytes the chunk pool may pin, given the tier.
///
/// `slots × (SLOT_VERTEX_BYTES + SLOT_INDEX_BYTES)`: S 72 MB, A 54 MB, B 36 MB,
/// C 18 MB — inside the resident budgets from `docs/PERFORMANCE.md` §8 with room
/// for the texture atlas and driver overhead.
pub const fn pool_bytes(slots: usize) -> u64 {
    slots as u64 * (SLOT_VERTEX_BYTES + SLOT_INDEX_BYTES)
}

/// Everything the launcher can choose at init.
#[derive(Clone, Debug)]
pub struct RendererConfig {
    /// `mcVersion` string, exactly as the launcher passed it.
    pub mc_version: String,
    /// `backendHint`: 0 auto, 1 Vulkan, 2 GLES3, 3 GLES2 legacy.
    pub backend_hint: i32,
    /// Surface width in pixels.
    pub width: u32,
    /// Surface height in pixels.
    pub height: u32,
    /// Requested render distance in chunks (0 = pick from the tier).
    pub render_distance: u8,
    /// Optional shaderpack directory.
    pub shaderpack: Option<PathBuf>,
}

impl Default for RendererConfig {
    fn default() -> Self {
        RendererConfig {
            mc_version: "1.20.4".to_string(),
            backend_hint: 0,
            width: 1280,
            height: 720,
            render_distance: 0,
            shaderpack: None,
        }
    }
}

/// Camera state pushed by `nativeSetCamera`.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// Column-major view matrix.
    pub view: [[f32; 4]; 4],
    /// Column-major projection matrix.
    pub proj: [[f32; 4]; 4],
    /// Eye position in world space (taken from the view matrix translation).
    pub position: [f32; 3],
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            view: identity(),
            proj: identity(),
            position: [0.0; 3],
        }
    }
}

/// Preallocated per-frame scratch. Nothing here allocates after `new`.
struct FrameScratch {
    sections: Vec<SectionInstance>,
    batches: Vec<DrawBatch>,
    cull: CullScratch,
    drained: Vec<MeshResult>,
    view_proj: [[f32; 4]; 4],
}

impl FrameScratch {
    fn new(capacity: usize) -> FrameScratch {
        FrameScratch {
            sections: Vec::with_capacity(capacity),
            batches: Vec::with_capacity(capacity),
            cull: CullScratch::with_capacity(capacity),
            drained: Vec::with_capacity(64),
            view_proj: identity(),
        }
    }
}

/// One pooled GPU buffer pair.
#[derive(Clone, Copy)]
struct GpuSlot {
    vertex: BufferId,
    index: BufferId,
    index_count: u32,
    format_index: usize,
}

/// Number of vertex layouts Sinkon builds pipelines for.
const FORMAT_SLOTS: usize = 2;

fn format_index(format: MeshFormat) -> usize {
    match format {
        MeshFormat::Packed4 => 0,
        _ => 1,
    }
}

/// Chunk streaming state: CPU pool, GPU buffers, worker pool.
struct ChunkManager {
    pool: MeshPool,
    workers: MeshWorkers,
    slots: Vec<GpuSlot>,
    sections: Vec<SectionInstance>,
    generation: u32,
    uploads: u64,
    dropped_overflow: u64,
    dropped_queue_full: u64,
    /// The legacy `glow` backend always reads a 24-byte vertex, so `Packed4`
    /// sections are re-meshed into the 24-byte layout instead of being uploaded
    /// in a format it cannot describe.
    force_stride24: bool,
}

impl ChunkManager {
    fn new(
        backend: &mut dyn GraphicsBackend,
        slot_count: usize,
        byte_cap: u64,
        workers: usize,
    ) -> BackendResult<ChunkManager> {
        let mut slots = Vec::with_capacity(slot_count);
        for _ in 0..slot_count {
            let vertex = backend.create_buffer(&BufferDesc {
                label: "sinkon.chunk.vertex",
                size: SLOT_VERTEX_BYTES,
                usage: BufferUsage::Geometry,
                persistent_map: false,
            })?;
            let index = backend.create_buffer(&BufferDesc {
                label: "sinkon.chunk.index",
                size: SLOT_INDEX_BYTES,
                usage: BufferUsage::Geometry,
                persistent_map: false,
            })?;
            slots.push(GpuSlot {
                vertex,
                index,
                index_count: 0,
                format_index: 0,
            });
        }
        Ok(ChunkManager {
            pool: MeshPool::new(slot_count, byte_cap),
            workers: MeshWorkers::spawn(workers),
            slots,
            sections: Vec::with_capacity(slot_count),
            generation: 1,
            uploads: 0,
            dropped_overflow: 0,
            dropped_queue_full: 0,
            force_stride24: backend.kind() == BackendKind::Gles2Legacy,
        })
    }

    fn queue(&mut self, key: SectionKey, blocks: Vec<u8>, format: MeshFormat) {
        let format = if self.force_stride24 && format == MeshFormat::Packed4 {
            MeshFormat::Custom(24)
        } else {
            format
        };
        let generation = self.generation;
        self.generation = self.generation.wrapping_add(1);
        let job = MeshJob {
            key,
            generation,
            format,
            blocks: blocks.into_boxed_slice(),
        };
        if self.workers.submit(job).is_err() {
            self.dropped_queue_full += 1;
            log::warn!("meshing queue full; section {key:?} dropped");
        }
    }

    fn drain(&mut self, backend: &mut dyn GraphicsBackend, drained: &mut Vec<MeshResult>) {
        drained.clear();
        self.workers.drain_into(drained);
        let count = drained.len();
        for index in 0..count {
            let result = &drained[index];
            if result.vertex_count == 0 && result.index_count == 0 {
                continue;
            }
            if result.vertices.len() as u64 > SLOT_VERTEX_BYTES
                || result.indices.len() as u64 > SLOT_INDEX_BYTES
            {
                self.dropped_overflow += 1;
                log::warn!(
                    "section {:?} mesh ({} B vertex / {} B index) exceeds the pooled buffer; \
                     tune SLOT_VERTEX_BYTES",
                    result.key,
                    result.vertices.len(),
                    result.indices.len()
                );
                continue;
            }
            let (slot, evicted) = self.pool.insert(result);
            if let Some(evicted) = evicted {
                self.sections.retain(|section| section.key != evicted);
            }
            let Some(gpu) = self.slots.get_mut(slot) else {
                continue;
            };
            if backend.write_buffer(gpu.vertex, 0, &result.vertices).is_err()
                || backend.write_buffer(gpu.index, 0, &result.indices).is_err()
            {
                continue;
            }
            gpu.index_count = result.index_count;
            gpu.format_index = format_index(result.format);
            self.uploads += 1;
            let instance = SectionInstance {
                key: result.key,
                slot: slot as u32,
                index_count: result.index_count,
                parent_visible: true,
            };
            match self.sections.iter_mut().find(|section| section.slot == slot as u32) {
                Some(existing) => *existing = instance,
                None => self.sections.push(instance),
            }
        }
    }

    fn gpu(&self, slot: u32) -> Option<GpuSlot> {
        self.slots.get(slot as usize).copied()
    }

    fn stats(&self) -> MeshPoolStats {
        self.pool.stats()
    }

    fn shutdown(&mut self) {
        self.workers.shutdown();
    }
}

/// Report body of `nativeGetCapabilityReport`.
#[derive(Clone, Debug, Serialize)]
pub struct CapabilityReport {
    /// Active backend name (`vulkan`, `gles3`, `gles2`).
    pub active_backend: String,
    /// Every backend that produced a device during negotiation.
    pub available_backends: Vec<String>,
    /// Backend-level capabilities.
    pub capabilities: Capabilities,
    /// How the backend was chosen, including fallbacks.
    pub selection: SelectionRecord,
    /// Detected device tier.
    pub tier: String,
    /// Version profile Sinkon runs this world under.
    pub version_profile: String,
    /// GLSL version emitted for the active backend.
    pub glsl_version: String,
    /// Mesh pool statistics.
    pub meshes: MeshPoolStats,
    /// Worker threads meshing chunks.
    pub worker_threads: usize,
    /// Render distance in sections.
    pub render_distance: u8,
    /// Shaderpack dialect in use.
    pub shaderpack: String,
    /// Compiled shader count.
    pub shaders: usize,
    /// Shader translation failures (pass + reason).
    pub shader_errors: Vec<String>,
    /// Sinkon version.
    pub sinkon_version: String,
}

/// The renderer instance handed to Kotlin as an opaque `jlong`.
pub struct SinkonRenderer {
    backend: Option<Box<dyn GraphicsBackend>>,
    capabilities: Capabilities,
    selection: SelectionRecord,
    profile: VersionProfile,
    tier: DeviceTier,
    render_distance: u8,
    perf: PerfRegistry,
    chunks: Option<ChunkManager>,
    scratch: FrameScratch,
    camera: Camera,
    window: SurfaceHandle,
    width: u32,
    height: u32,
    world_texture: Option<TextureId>,
    pipelines: [Option<PipelineId>; FORMAT_SLOTS],
    pipeline_ready: bool,
    shader_cache: ShaderCache,
    shaderpack_kind: ShaderpackKind,
    shaderpack_path: Option<PathBuf>,
    shader_errors: Vec<String>,
    frame_index: u64,
    backend_allocations: u64,
    started: Instant,
}

impl SinkonRenderer {
    /// Negotiate a backend and build the renderer.
    pub fn init(config: &RendererConfig, window: SurfaceHandle) -> BackendResult<SinkonRenderer> {
        let started = Instant::now();
        let (width, height) = (config.width.max(1), config.height.max(1));
        let target = unsafe {
            // SAFETY: `window` owns a live `ANativeWindow` reference that outlives
            // this renderer (it moves into the returned struct).
            SurfaceTarget::from_native_window(window.as_non_null(), width, height)
        };

        // The backend refines this from the adapter name; B is the neutral default.
        let (backend, selection) = backend::select(
            &target,
            config.backend_hint,
            &config.mc_version,
            DeviceTier::B,
        );
        let Some(backend) = backend else {
            let details: Vec<String> = selection
                .attempted
                .iter()
                .map(|attempt| format!("{}: {}", attempt.backend, attempt.reason))
                .collect();
            return Err(BackendError::Unavailable(format!(
                "no graphics backend could be created ({})",
                details.join("; ")
            )));
        };

        let capabilities = backend.capabilities().clone();
        let tier = DeviceTier::from_name(&capabilities.device_tier).unwrap_or(DeviceTier::B);
        let profile = version_profile::classify(&config.mc_version);
        let budget = tier.budget();
        let render_distance = if config.render_distance == 0 {
            budget
                .for_backend(backend.kind())
                .map(|b| b.render_distance)
                .unwrap_or(4)
        } else {
            config.render_distance
        };
        let (slots, workers) = pool_sizing(tier);
        let cpu_byte_cap = u64::from(budget.resident_memory_mb) * 1024 * 1024 / 2;

        let active = backend.kind();
        let mut renderer = SinkonRenderer {
            backend: Some(backend),
            capabilities,
            selection,
            profile,
            tier,
            render_distance,
            perf: PerfRegistry::new(active, tier),
            chunks: None,
            scratch: FrameScratch::new(slots),
            camera: Camera::default(),
            window,
            width,
            height,
            world_texture: None,
            pipelines: [None; FORMAT_SLOTS],
            pipeline_ready: false,
            shader_cache: ShaderCache::new(),
            shaderpack_kind: ShaderpackKind::Vanilla,
            shaderpack_path: None,
            shader_errors: Vec::new(),
            frame_index: 0,
            backend_allocations: 0,
            started,
        };

        if let Some(backend) = renderer.backend.as_mut() {
            match ChunkManager::new(backend, slots, cpu_byte_cap, workers) {
                Ok(chunks) => renderer.chunks = Some(chunks),
                Err(err) => {
                    log::error!("chunk buffers could not be created: {err}");
                    return Err(err);
                }
            }
        }

        if let Some(pack) = config.shaderpack.as_deref() {
            renderer.set_shaderpack(pack);
        }
        renderer.ensure_pipelines();

        log::info!(
            "Sinkon {} ready in {:.2} ms: backend={} tier={} profile={} rd={} slots={} workers={}",
            crate::VERSION,
            started.elapsed().as_secs_f64() * 1000.0,
            active,
            tier,
            profile,
            render_distance,
            slots,
            workers
        );
        Ok(renderer)
    }

    /// Active backend kind, if any.
    pub fn backend_kind(&self) -> Option<BackendKind> {
        self.backend.as_ref().map(|backend| backend.kind())
    }

    /// Capability snapshot.
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// Version profile in use.
    pub fn profile(&self) -> VersionProfile {
        self.profile
    }

    /// Selection record (requested hint, attempts, fallback flag).
    pub fn selection(&self) -> &SelectionRecord {
        &self.selection
    }

    /// Current surface size.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Resize the swapchain/surface.
    pub fn resize(&mut self, width: u32, height: u32) -> BackendResult<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        self.width = width;
        self.height = height;
        if let Some(backend) = self.backend.as_mut() {
            backend.resize(width, height)?;
        }
        Ok(())
    }

    /// Update the camera matrices (one JNI call per frame, reused FloatBuffers).
    pub fn set_camera(&mut self, view: &[[f32; 4]; 4], proj: &[[f32; 4]; 4]) {
        self.camera.view = *view;
        self.camera.proj = *proj;
        self.camera.position = [view[3][0], view[3][1], view[3][2]];
    }

    /// Record one JNI crossing duration so `PerfSnapshot::jni_mean_ns` is real.
    ///
    /// Called by the bridge around `nativeSetCamera` and `nativeRenderFrame`; it
    /// only touches atomics, so it is safe on the frame path.
    pub fn record_jni_call(&self, nanos: u64) {
        self.perf.jni_call(nanos);
    }

    /// Queue one chunk section for meshing + upload.
    pub fn bake_section(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        blocks: &[u8],
        format_id: i32,
    ) -> BackendResult<()> {
        if blocks.len() < SECTION_BLOCKS {
            return Err(BackendError::InvalidArgument(format!(
                "bake_section needs {SECTION_BLOCKS} block ids, got {}",
                blocks.len()
            )));
        }
        let Some(chunks) = self.chunks.as_mut() else {
            return Err(BackendError::Unsupported("renderer is not ready".to_string()));
        };
        let mut owned = vec![0u8; SECTION_BLOCKS];
        owned.copy_from_slice(&blocks[..SECTION_BLOCKS]);
        chunks.queue(
            SectionKey::new(x, y, z),
            owned,
            MeshFormat::from_id(format_id),
        );
        Ok(())
    }

    /// Upload (or update) the world texture atlas.
    pub fn upload_texture(&mut self, width: u32, height: u32, pixels: &[u8]) -> BackendResult<()> {
        let expected = (width as usize) * (height as usize) * 4;
        if pixels.len() < expected {
            return Err(BackendError::InvalidArgument(format!(
                "texture upload has {} bytes, expected {expected}",
                pixels.len()
            )));
        }
        let Some(backend) = self.backend.as_mut() else {
            return Err(BackendError::Unsupported("renderer has no backend".to_string()));
        };
        match self.world_texture {
            Some(id) => backend.update_texture(id, [0, 0], [width, height], &pixels[..expected]),
            None => {
                let id = backend.create_texture(
                    &TextureDesc {
                        label: "sinkon.world.atlas",
                        width,
                        height,
                        format: PixelFormat::Rgba8Unorm,
                        mip_levels: 1,
                        linear: false,
                    },
                    Some(&pixels[..expected]),
                )?;
                self.world_texture = Some(id);
                Ok(())
            }
        }
    }

    /// Point Sinkon at a shaderpack directory.
    pub fn set_shaderpack(&mut self, path: &std::path::Path) {
        let kind = shader::detect_kind(path);
        self.shaderpack_kind = kind;
        self.shaderpack_path = Some(path.to_path_buf());
        self.shader_errors.clear();
        let target = ShaderTarget::for_backend(self.backend_kind().unwrap_or(BackendKind::Vulkan));
        let shaders_dir = if path.join("shaders").is_dir() {
            path.join("shaders")
        } else {
            path.to_path_buf()
        };
        let (compiled, failed) = self.shader_cache.compile_dir(
            &shaders_dir,
            target,
            &[ShaderStage::Vertex, ShaderStage::Fragment],
        );
        for (name, reason) in failed {
            log::warn!("shaderpack pass {name} failed to compile: {reason}");
            self.shader_errors.push(format!("{name}: {reason}"));
        }
        log::info!(
            "shaderpack {} ({}) → {} shader(s) compiled, {} failed",
            path.display(),
            kind.as_str(),
            compiled.len(),
            self.shader_errors.len()
        );
    }

    /// Recompile every shader (hot reload entry point).
    pub fn reload_shaders(&mut self) -> ReloadSummary {
        let target = ShaderTarget::for_backend(self.backend_kind().unwrap_or(BackendKind::Vulkan));
        let Some(pack) = self.shaderpack_path.clone() else {
            return ReloadSummary {
                compiled_shaders: 0,
                failed: 0,
            };
        };
        let shaders_dir = if pack.join("shaders").is_dir() {
            pack.join("shaders")
        } else {
            pack
        };
        let (compiled, failed) = self.shader_cache.compile_dir(
            &shaders_dir,
            target,
            &[ShaderStage::Vertex, ShaderStage::Fragment],
        );
        self.shader_errors = failed
            .iter()
            .map(|(name, reason)| format!("{name}: {reason}"))
            .collect();
        if let Some(backend) = self.backend.as_mut() {
            let _ = backend.reload_shaders();
        }
        self.pipeline_ready = false;
        self.ensure_pipelines();
        ReloadSummary {
            compiled_shaders: compiled.len(),
            failed: self.shader_errors.len(),
        }
    }

    /// Poll shader mtimes; called every N frames by the JNI bridge.
    pub fn poll_shader_reload(&mut self) -> shader::ReloadOutcome {
        let target = ShaderTarget::for_backend(self.backend_kind().unwrap_or(BackendKind::Vulkan));
        let outcome = self.shader_cache.poll_reload(target);
        if !outcome.changed.is_empty() {
            self.pipeline_ready = false;
            self.ensure_pipelines();
        }
        outcome
    }

    /// Build the built-in terrain pipelines (one per vertex layout).
    ///
    /// Sinkon ships a small built-in shader so chunk geometry renders before the
    /// launcher supplies Minecraft's own core shaders, and so benches/device smoke
    /// tests exercise the whole translate → pipeline path.
    fn ensure_pipelines(&mut self) {
        if self.pipeline_ready {
            return;
        }
        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        let kind = backend.kind();

        // One shader pair per vertex layout: index 0 = 20-byte `Packed4`
        // (position + uv), index 1 = 24-byte `Packed8`/`Custom` (position + uv +
        // unorm8 colour). Declaring the colour attribute for a 20-byte buffer
        // would make wgpu reject the pipeline, and the legacy backend always
        // consumes the 24-byte layout (see `ChunkManager::force_stride24`).
        let mut shader_keys: [ShaderKey; FORMAT_SLOTS] = [ShaderKey(0); FORMAT_SLOTS];
        let strides: [u64; FORMAT_SLOTS];
        match kind {
            BackendKind::Vulkan | BackendKind::Gles3 => {
                strides = [20, 24];
                let pairs = [
                    (
                        shader::builtin::TERRAIN_VERTEX_PACKED4,
                        shader::builtin::TERRAIN_FRAGMENT_PACKED4,
                        ShaderKey(3),
                        ShaderKey(4),
                    ),
                    (
                        shader::builtin::TERRAIN_VERTEX,
                        shader::builtin::TERRAIN_FRAGMENT,
                        ShaderKey(1),
                        ShaderKey(2),
                    ),
                ];
                for (index, (vertex_src, fragment_src, vertex_key, fragment_key)) in
                    pairs.into_iter().enumerate()
                {
                    let mut words: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
                    let stages = [
                        (vertex_src, ShaderStage::Vertex),
                        (fragment_src, ShaderStage::Fragment),
                    ];
                    let mut failed = false;
                    for (slot, (source, stage)) in stages.into_iter().enumerate() {
                        match shader::translate(source, stage, ShaderTarget::SpirV) {
                            Ok(shader::TranslatedCode::SpirV(spirv)) => words[slot] = spirv,
                            Ok(_) => {
                                log::error!("built-in shader produced non-SPIR-V output");
                                failed = true;
                            }
                            Err(err) => {
                                log::error!("built-in {stage:?} shader failed: {err}");
                                self.shader_errors
                                    .push(format!("builtin {stage:?} (layout {index}): {err}"));
                                failed = true;
                            }
                        }
                    }
                    if failed {
                        return;
                    }
                    if backend
                        .create_shader(vertex_key, ShaderCode::SpirV(&words[0]))
                        .is_err()
                        || backend
                            .create_shader(fragment_key, ShaderCode::SpirV(&words[1]))
                            .is_err()
                    {
                        log::error!("backend rejected the built-in shaders");
                        return;
                    }
                    shader_keys[index] = vertex_key;
                    log::info!(
                        "built-in terrain layout {index} ({} B stride) → SPIR-V ({} vertex / {} fragment words)",
                        strides[index],
                        words[0].len(),
                        words[1].len()
                    );
                }
            }
            BackendKind::Gles2Legacy => {
                // The legacy backend compiles its own ES 1.00 program pair; the key
                // selects textured + coloured + fog-free terrain at a 24-byte stride.
                let key = crate::backend::gles2_legacy::LegacyKey::new(true, true, false, false);
                shader_keys = [ShaderKey(u32::from(key.0)); FORMAT_SLOTS];
                strides = [24, 24];
            }
        }

        for index in 0..FORMAT_SLOTS {
            let stride = strides[index];
            let mut desc = PipelineDesc {
                label: "sinkon.terrain",
                vertex_entry: "main",
                fragment_entry: "main",
                color_formats: &[],
                depth_format: None,
                blend: BlendMode::Opaque,
                topology: Topology::TriangleList,
                vertex_stride: stride,
                layout: LayoutId(0),
                shader: shader_keys[index],
            };
            desc.label = if index == 0 {
                "sinkon.terrain.packed4"
            } else {
                "sinkon.terrain.packed8"
            };
            match backend.create_pipeline(&desc) {
                Ok(id) => self.pipelines[index] = Some(id),
                Err(err) => log::error!("pipeline creation failed: {err}"),
            }
        }
        self.pipeline_ready = true;
    }

    /// Render and present one frame.
    pub fn render_frame(&mut self) -> BackendResult<PresentOutcome> {
        let submission_started = Instant::now();
        alloc::begin_frame();

        if self.backend.is_none() {
            alloc::end_frame();
            return Err(BackendError::Unsupported(
                "renderer has no backend".to_string(),
            ));
        }

        // 1. Mesh results → pooled buffers (Sinkon scope, allocation-free).
        if let Some(backend) = self.backend.as_mut() {
            if let Some(chunks) = self.chunks.as_mut() {
                chunks.drain(backend.as_mut(), &mut self.scratch.drained);
            }
        }

        // 2. Camera + frustum.
        self.scratch.view_proj = mat4_multiply(&self.camera.proj, &self.camera.view);
        let frustum = crate::chunk::Frustum::from_view_proj(&self.scratch.view_proj);

        // 3. Cull.
        self.scratch.sections.clear();
        if let Some(chunks) = self.chunks.as_ref() {
            self.scratch.sections.extend_from_slice(&chunks.sections);
        }
        let camera_position = self.camera.position;
        cull_sections(
            &self.scratch.sections,
            &frustum,
            camera_position,
            &mut self.scratch.cull,
        );

        // 4. Batches.
        self.scratch.batches.clear();
        let mut drawn = 0u32;
        if let Some(chunks) = self.chunks.as_ref() {
            let visible: &[(u32, f32)] = &self.scratch.cull.visible;
            for (slot, _distance) in visible.iter() {
                let Some(gpu) = chunks.gpu(*slot) else {
                    continue;
                };
                if gpu.index_count == 0 {
                    continue;
                }
                let Some(pipeline) = self.pipelines[gpu.format_index] else {
                    continue;
                };
                self.scratch.batches.push(DrawBatch {
                    pipeline,
                    vertex_buffer: gpu.vertex,
                    index_buffer: gpu.index,
                    index_count: gpu.index_count,
                    instance_count: 1,
                    first_index: 0,
                    base_vertex: 0,
                    section_tag: *slot,
                });
                drawn += 1;
            }
        }
        let tested = self.scratch.cull.tested;
        let sinkon_allocations = alloc::end_frame();

        // 5. Submit + present (backend scope; wgpu objects allocate here).
        let allocations_before = alloc::total_allocations();
        let frame = FrameSubmission {
            view: &self.camera.view,
            proj: &self.camera.proj,
            camera_pos: self.camera.position,
            batches: &self.scratch.batches,
            clear_color: [0.05, 0.07, 0.11, 1.0],
            frame_index: self.frame_index,
        };
        let submit_result = match self.backend.as_mut() {
            Some(backend) => backend.submit(&frame),
            None => Err(BackendError::Unsupported("no backend".to_string())),
        };
        let present_started = Instant::now();
        let outcome = match self.backend.as_mut() {
            Some(backend) => backend.present()?,
            None => PresentOutcome::Lost,
        };
        let present_us = present_started.elapsed().as_micros().min(u64::from(u32::MAX)) as u32;
        self.backend_allocations = alloc::total_allocations().saturating_sub(allocations_before);
        submit_result?;

        self.frame_index = self.frame_index.wrapping_add(1);
        let submission_us = submission_started
            .elapsed()
            .as_micros()
            .min(u64::from(u32::MAX)) as u32;
        self.perf.record_frame(submission_us, present_us);
        self.perf.add_batches(drawn);
        self.perf.set_cull(drawn, tested.saturating_sub(drawn));
        self.perf.set_frame_allocations(sinkon_allocations);
        if let Some(backend) = self.backend.as_ref() {
            self.perf.set_vram(backend.vram_bytes());
        }
        if let Some(chunks) = self.chunks.as_ref() {
            self.perf.set_resident_sections(chunks.stats().resident as u64);
        }

        match outcome {
            PresentOutcome::Outdated => {
                log::warn!(
                    "surface outdated; reconfiguring {}x{}",
                    self.width,
                    self.height
                );
                self.resize(self.width, self.height)?;
            }
            PresentOutcome::Lost => {
                log::error!(
                    "surface lost; the launcher must recreate it (Sinkon keeps its device)"
                );
            }
            PresentOutcome::Presented => {}
        }
        Ok(outcome)
    }

    /// Attempt a live backend switch.
    ///
    /// Returns 0 on success, non-zero on failure. The new backend is created
    /// *before* the old one is destroyed, so a failed switch leaves the launcher
    /// with a working renderer.
    pub fn switch_backend(&mut self, hint: i32) -> i32 {
        let target = unsafe {
            // SAFETY: the renderer still owns the ANativeWindow reference.
            SurfaceTarget::from_native_window(self.window.as_non_null(), self.width, self.height)
        };
        let (candidate, record) =
            backend::select(&target, hint, &self.selection.mc_version, self.tier);
        let Some(candidate) = candidate else {
            log::error!("backend switch to hint {hint} failed; keeping the current backend");
            return 1;
        };
        let new_kind = candidate.kind();
        let new_capabilities = candidate.capabilities().clone();
        if let Some(mut old) = self.backend.replace(candidate) {
            let old_kind = old.kind();
            old.destroy();
            log::warn!("backend switched {old_kind} → {new_kind}");
        }
        self.capabilities = new_capabilities;
        self.selection = record;
        self.perf = PerfRegistry::new(new_kind, self.tier);
        if let Some(mut chunks) = self.chunks.take() {
            chunks.shutdown();
        }
        let (slots, workers) = pool_sizing(self.tier);
        let byte_cap = u64::from(self.tier.budget().resident_memory_mb) * 1024 * 1024 / 2;
        if let Some(backend) = self.backend.as_mut() {
            match ChunkManager::new(backend, slots, byte_cap, workers) {
                Ok(chunks) => self.chunks = Some(chunks),
                Err(err) => {
                    log::error!("chunk buffers could not be recreated after the switch: {err}");
                    return 2;
                }
            }
        }
        self.pipelines = [None; FORMAT_SLOTS];
        self.pipeline_ready = false;
        self.ensure_pipelines();
        0
    }

    /// JSON body of `nativeGetCapabilityReport`.
    pub fn capability_report(&self) -> String {
        let report = CapabilityReport {
            active_backend: self
                .selection
                .active
                .map(|kind| kind.as_str().to_string())
                .unwrap_or_else(|| "none".to_string()),
            available_backends: self
                .selection
                .available()
                .iter()
                .map(|kind| kind.as_str().to_string())
                .collect(),
            capabilities: self.capabilities.clone(),
            selection: self.selection.clone(),
            tier: self.tier.as_str().to_string(),
            version_profile: self.profile.as_str().to_string(),
            glsl_version: self.capabilities.glsl_version.clone(),
            meshes: self
                .chunks
                .as_ref()
                .map(|chunks| chunks.stats())
                .unwrap_or_default(),
            worker_threads: self
                .chunks
                .as_ref()
                .map(|chunks| chunks.workers.workers())
                .unwrap_or(0),
            render_distance: self.render_distance,
            shaderpack: self.shaderpack_kind.as_str().to_string(),
            shaders: self.shader_cache.len(),
            shader_errors: self.shader_errors.clone(),
            sinkon_version: crate::VERSION.to_string(),
        };
        serde_json::to_string(&report).unwrap_or_else(|_| {
            format!(
                "{{\"active_backend\":\"{}\",\"error\":\"capability report serialise failed\"}}",
                report.active_backend
            )
        })
    }

    /// JSON body of `nativeGetPerfStats`.
    pub fn perf_stats(&self) -> String {
        let mut value: serde_json::Value =
            serde_json::from_str(&self.perf.stats_json()).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "backend_allocations".to_string(),
                serde_json::json!(self.backend_allocations),
            );
            object.insert(
                "uptime_ms".to_string(),
                serde_json::json!(self.started.elapsed().as_millis() as u64),
            );
            object.insert(
                "render_distance".to_string(),
                serde_json::json!(self.render_distance),
            );
            object.insert(
                "profile".to_string(),
                serde_json::json!(self.profile.as_str()),
            );
            object.insert(
                "queued_uploads".to_string(),
                serde_json::json!(
                    self.chunks
                        .as_ref()
                        .map(|chunks| chunks.uploads)
                        .unwrap_or(0)
                ),
            );
            object.insert(
                "dropped_sections".to_string(),
                serde_json::json!(self
                    .chunks
                    .as_ref()
                    .map(|chunks| chunks.dropped_overflow + chunks.dropped_queue_full)
                    .unwrap_or(0)),
            );
        }
        serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
    }

    /// Release everything.
    pub fn destroy(&mut self) {
        if let Some(mut chunks) = self.chunks.take() {
            chunks.shutdown();
        }
        if let Some(mut backend) = self.backend.take() {
            backend.destroy();
        }
        self.shader_cache = ShaderCache::new();
        log::info!(
            "Sinkon destroyed after {} frame(s), {} backend allocation(s) in the last frame",
            self.frame_index,
            self.backend_allocations
        );
    }
}

/// Summary returned by [`SinkonRenderer::reload_shaders`].
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ReloadSummary {
    /// Shaders recompiled.
    pub compiled_shaders: usize,
    /// Shaders that failed.
    pub failed: usize,
}

/// Pool sizing for a tier: `(slots, worker threads)`.
pub fn pool_sizing(tier: DeviceTier) -> (usize, usize) {
    let slots = match tier {
        DeviceTier::S => 512,
        DeviceTier::A => 384,
        DeviceTier::B => 256,
        DeviceTier::C => 128,
    };
    let workers = match tier {
        DeviceTier::S | DeviceTier::A => num_cpus::get().saturating_sub(1).max(1),
        DeviceTier::B => num_cpus::get().clamp(1, 4),
        DeviceTier::C => 2,
    };
    (slots, workers)
}

/// Column-major 4×4 multiply, `out = a * b`.
pub fn mat4_multiply(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for (column, out_column) in out.iter_mut().enumerate() {
        for (row, value) in out_column.iter_mut().enumerate() {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k][row] * b[column][k];
            }
            *value = sum;
        }
    }
    out
}

/// Identity matrix.
pub fn identity() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

impl Drop for SinkonRenderer {
    fn drop(&mut self) {
        self.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_multiply_is_identity() {
        let id = identity();
        assert_eq!(mat4_multiply(&id, &id), id);
    }

    #[test]
    fn translation_is_applied_on_the_left() {
        let mut translation = identity();
        translation[3] = [10.0, 20.0, 30.0, 1.0];
        assert_eq!(mat4_multiply(&translation, &identity())[3], [10.0, 20.0, 30.0, 1.0]);
    }

    #[test]
    fn section_size_matches_minecraft() {
        assert_eq!(SECTION_SIZE, 16.0);
        assert_eq!(SECTION_BLOCKS, 4096);
    }

    #[test]
    fn default_config_is_sane() {
        let config = RendererConfig::default();
        assert_eq!(config.backend_hint, 0);
        assert!(config.width > 0 && config.height > 0);
        assert!(config.shaderpack.is_none());
    }

    #[test]
    fn pool_sizing_shrinks_with_the_tier() {
        let (big, _) = pool_sizing(DeviceTier::S);
        let (small, _) = pool_sizing(DeviceTier::C);
        assert!(big > small);
        assert_eq!(small, 64);
    }

    #[test]
    fn format_slots_cover_both_layouts() {
        assert_eq!(format_index(MeshFormat::Packed4), 0);
        assert_eq!(format_index(MeshFormat::Packed8), 1);
        assert_eq!(format_index(MeshFormat::Custom(9)), 1);
    }
}
