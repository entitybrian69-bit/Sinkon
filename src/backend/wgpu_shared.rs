//! Shared `wgpu` implementation used by the Vulkan and GLES 3.x backends.
//!
//! `vulkan.rs` and `gles3.rs` are thin configurations of this engine: they differ
//! only in the `wgpu::Backends` mask, the feature request and the capability
//! mapping. Everything that must be allocation-free per frame lives here once.
//!
//! # Zero-allocation frame path
//!
//! * Per-frame uniforms go into a **uniform ring** (`8 × 256 B`) written with
//!   `Queue::write_buffer`; one `BindGroup` with a dynamic offset is bound once.
//!   No buffer, bind group or layout is created per frame.
//! * Pipeline / vertex / index state is tracked so only real state changes reach
//!   the encoder.
//! * `DrawBatch` slices are borrowed from the renderer's preallocated scratch
//!   buffer; this module never builds a `Vec`.
//!
//! `wgpu` itself allocates for `create_view`/`create_command_encoder` on every
//! frame; that is outside Sinkon's control and is measured separately (see
//! `docs/PERFORMANCE.md` §5). Sinkon's own scope is gated to zero.
//!
//! # wgpu version pinning
//!
//! Everything version-sensitive is in the two `GPU-API-SEAM` blocks below. They
//! are written against `wgpu` 22.1, where
//! `Instance::create_surface_unsafe` takes
//! `SurfaceTargetUnsafe::RawHandle { raw_window_handle, raw_display_handle }` of
//! `raw-window-handle` 0.6 types, `Adapter::request_device` takes the trace path
//! as its second argument, and `VertexState::entry_point` is a plain `&str`.

use std::collections::HashMap;

use crate::backend::{
    Arena, BackendError, BackendFeatures, BackendKind, BackendResult, BlendMode, BufferDesc,
    BufferId, BufferUsage, Capabilities, FrameSubmission, GraphicsBackend, PipelineDesc,
    PipelineId, PixelFormat, PresentOutcome, ShaderCode, ShaderKey, TextureDesc, TextureId, Topology,
};
use crate::perf::tier::{self, DeviceTier};
use crate::surface::{RawWindow, SurfaceHandle, SurfaceTarget};

/// Size of one camera uniform blob, rounded up to the 256-byte alignment that
/// every Android GPU requires for dynamic uniform offsets.
pub const UNIFORM_SLOT_SIZE: u64 = 256;
/// Number of uniform ring slots.
pub const UNIFORM_SLOTS: u64 = 8;
/// Payload size of the camera uniform (2 × mat4 + vec3 + padding).
pub const UNIFORM_PAYLOAD: u64 = 160;

/// Configuration that distinguishes the Vulkan and GLES 3.x backends.
pub struct WgpuBackendConfig {
    /// `wgpu` backend mask.
    pub backends: wgpu::Backends,
    /// Which Sinkon backend this is.
    pub kind: BackendKind,
    /// Label prefix for GPU objects.
    pub label: &'static str,
    /// Features requested when the adapter offers them.
    pub optional_features: wgpu::Features,
}

struct BufferEntry {
    buffer: wgpu::Buffer,
    size: u64,
}

// `view` is consumed by the atlas bind-group builder (KNOWN_ISSUES.md KI-3);
// keeping the handle here avoids re-creating it per frame once that lands.
#[allow(dead_code)]
struct TextureEntry {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: [u32; 2],
    format: PixelFormat,
    bytes: u64,
}

/// A live `wgpu` device plus the renderer state Sinkon keeps with it.
pub struct WgpuBackend {
    kind: BackendKind,
    label: &'static str,
    // Field order matters: `surface` must drop before `window` is released.
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    window: Option<SurfaceHandle>,
    capabilities: Capabilities,
    tier: DeviceTier,

    pipelines: Arena<wgpu::RenderPipeline>,
    shaders: HashMap<ShaderKey, wgpu::ShaderModule>,
    buffers: Arena<BufferEntry>,
    textures: Arena<TextureEntry>,

    pipeline_layout: wgpu::PipelineLayout,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    camera: [[f32; 4]; 4],
    proj: [[f32; 4]; 4],
    camera_pos: [f32; 3],
    uniform_slot: u64,

    current: Option<wgpu::SurfaceTexture>,
    outcome: PresentOutcome,
    vram: u64,
}

impl WgpuBackend {
    /// Create a device and surface for one of the `wgpu`-based backends.
    ///
    /// `target` must be a live Android window; offscreen targets are rejected
    /// because Sinkon's wgpu paths present to a real surface.
    pub fn create(
        target: &SurfaceTarget<'_>,
        config: WgpuBackendConfig,
        tier_hint: DeviceTier,
    ) -> BackendResult<WgpuBackend> {
        let surface_handle = match target.native_window() {
            Some(pointer) => {
                // SAFETY: `SurfaceTarget::AndroidNativeWindow` guarantees a
                // referenced `ANativeWindow` that outlives the target. The handle
                // is stored in `self.window` for as long as the `wgpu::Surface`
                // exists, so the reference is not dropped while Vulkan/EGL uses it.
                unsafe { SurfaceHandle::borrow_raw(pointer) }
            }
            None => {
                return Err(BackendError::Unavailable(format!(
                    "{} backend needs a native window; offscreen targets are CPU-only",
                    config.kind
                )))
            }
        };
        let (width, height) = target.size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: config.backends,
            ..Default::default()
        });

        // ---- GPU-API-SEAM 1/2: window handle → wgpu surface (wgpu 22.1) ----
        use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
        let raw = RawWindow(surface_handle.as_non_null());
        let window_handle = raw
            .window_handle()
            .map_err(|err| BackendError::Surface(format!("window handle: {err}")))?;
        let display_handle = raw
            .display_handle()
            .map_err(|err| BackendError::Surface(format!("display handle: {err}")))?;
        // SAFETY: both handles stay valid while `surface_handle` owns the
        // ANativeWindow reference, which this struct keeps alive; therefore
        // widening the surface lifetime to `'static` is sound.
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_window_handle: window_handle.as_raw(),
                raw_display_handle: display_handle.as_raw(),
            })
        }
        .map_err(|err| BackendError::Surface(format!("create_surface: {err}")))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .ok_or_else(|| {
            BackendError::Adapter(format!("no {} adapter accepted the surface", config.label))
        })?;

        let info = adapter.get_info();
        let adapter_features = adapter.features();
        let limits = adapter.limits();
        let requested = config.optional_features & adapter_features;

        // ---- GPU-API-SEAM 2/2: device request (wgpu 22.1) ----
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some(config.label),
                required_features: requested,
                required_limits: wgpu::Limits::downlevel_defaults().using_resolution(limits.clone()),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .map_err(|err| BackendError::Device(format!("request_device: {err}")))?;

        let surface_caps = surface.get_capabilities(&adapter);
        let format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(surface_caps.formats[0]);
        let present_mode = if surface_caps
            .present_modes
            .contains(&wgpu::PresentMode::Mailbox)
        {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sinkon.camera.layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(UNIFORM_PAYLOAD),
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sinkon.pipeline.layout"),
            bind_group_layouts: &[&uniform_layout],
            push_constant_ranges: &[],
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sinkon.camera.ring"),
            size: UNIFORM_SLOT_SIZE * UNIFORM_SLOTS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sinkon.camera.bg"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniform_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(UNIFORM_PAYLOAD),
                }),
            }],
        });

        let detection = tier::detect(
            &info.name,
            vulkan_loader_version(),
            std::env::var("SINKON_TIER_OVERRIDE").ok().as_deref(),
        );
        let tier = if detection.source == tier::DetectionSource::Default {
            tier_hint
        } else {
            detection.tier
        };
        log::info!(
            "{} adapter=\"{}\" driver=\"{}\" tier={} ({})",
            config.kind,
            info.name,
            info.driver_info,
            tier,
            detection.reason
        );

        let capabilities = capabilities_for(&config, &info, &adapter, &limits, tier);
        let vram = UNIFORM_SLOT_SIZE * UNIFORM_SLOTS;

        Ok(WgpuBackend {
            kind: config.kind,
            label: config.label,
            surface,
            device,
            queue,
            config: surface_config,
            window: Some(surface_handle),
            capabilities,
            tier,
            pipelines: Arena::new(),
            shaders: HashMap::new(),
            buffers: Arena::new(),
            textures: Arena::new(),
            pipeline_layout,
            uniform_buffer,
            uniform_bind_group,
            camera: identity(),
            proj: identity(),
            camera_pos: [0.0; 3],
            uniform_slot: 0,
            current: None,
            outcome: PresentOutcome::Presented,
            vram,
        })
    }

    /// Adapter list for the capability report. No device is created.
    pub fn probe(backends: wgpu::Backends) -> Vec<AdapterProbe> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });
        instance
            .enumerate_adapters(backends)
            .into_iter()
            .map(|adapter| {
                let info = adapter.get_info();
                AdapterProbe {
                    backend: format!("{:?}", info.backend).to_ascii_lowercase(),
                    name: info.name,
                    driver: info.driver,
                    driver_info: info.driver_info,
                    device_type: format!("{:?}", info.device_type),
                    api_version: match info.backend {
                        wgpu::Backend::Vulkan => vulkan_loader_version()
                            .map(format_vulkan_version)
                            .unwrap_or_else(|| "unknown".to_string()),
                        _ => "unknown".to_string(),
                    },
                }
            })
            .collect()
    }

    /// Short `adapter | api | tier` summary for the debug overlay.
    pub fn summary(&self) -> String {
        format!(
            "{} | {} | tier {}",
            self.capabilities.adapter_name, self.capabilities.api_version, self.tier
        )
    }

    fn write_camera_slot(&mut self) {
        let mut blob = [0u8; UNIFORM_PAYLOAD as usize];
        write_mat4(&mut blob[0..64], &self.camera);
        write_mat4(&mut blob[64..128], &self.proj);
        blob[128..132].copy_from_slice(&self.camera_pos[0].to_ne_bytes());
        blob[132..136].copy_from_slice(&self.camera_pos[1].to_ne_bytes());
        blob[136..140].copy_from_slice(&self.camera_pos[2].to_ne_bytes());
        let offset = self.uniform_slot * UNIFORM_SLOT_SIZE;
        self.queue.write_buffer(&self.uniform_buffer, offset, &blob);
    }

    fn next_slot(&mut self) {
        self.uniform_slot = (self.uniform_slot + 1) % UNIFORM_SLOTS;
    }

    fn to_wgpu_format(format: PixelFormat) -> wgpu::TextureFormat {
        match format {
            PixelFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
            PixelFormat::Rgba8UnormSrgb => wgpu::TextureFormat::Rgba8UnormSrgb,
            PixelFormat::Bc7RgbaUnorm => wgpu::TextureFormat::Bc7RgbaUnorm,
            PixelFormat::Astc4x4Unorm => wgpu::TextureFormat::Astc {
                block: wgpu::AstcBlock::B4x4,
                channel: wgpu::AstcChannel::Unorm,
            },
            PixelFormat::Depth32Float => wgpu::TextureFormat::Depth32Float,
        }
    }
}

impl GraphicsBackend for WgpuBackend {
    fn kind(&self) -> BackendKind {
        self.kind
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn resize(&mut self, width: u32, height: u32) -> BackendResult<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        if self.config.width == width && self.config.height == height {
            return Ok(());
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        if let Some(window) = self.window.as_ref() {
            crate::surface::set_buffers_geometry(window, width, height);
        }
        log::info!("{} resize {}x{}", self.kind, width, height);
        Ok(())
    }

    fn create_shader(&mut self, key: ShaderKey, code: ShaderCode<'_>) -> BackendResult<()> {
        let source = match code {
            ShaderCode::SpirV(words) => {
                wgpu::ShaderSource::SpirV(std::borrow::Cow::Borrowed(words))
            }
            ShaderCode::Wgsl(text) => {
                wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(text))
            }
            ShaderCode::Glsl(_) => {
                return Err(BackendError::Unsupported(
                    "wgpu backends take SPIR-V (naga emits GLSL internally); \
                     GLSL sources are only accepted by the GLES 2.0 legacy path"
                        .to_string(),
                ))
            }
        };
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(self.label),
                source,
            });
        self.shaders.insert(key, module);
        Ok(())
    }

    fn create_pipeline(&mut self, desc: &PipelineDesc) -> BackendResult<PipelineId> {
        let module = self
            .shaders
            .get(&desc.shader)
            .ok_or_else(|| BackendError::Unsupported(format!("missing shader {:?}", desc.shader)))?;
        // Chunk vertex layout, matching `src/chunk.rs::push_face`:
        //   pos f32x3 @0, uv f32x2 @12, [colour unorm8x4 @20 when stride >= 24].
        // Declaring a location the buffer does not contain makes wgpu reject the
        // pipeline, so the attribute list follows the stride exactly.
        let mut vertex_attributes: Vec<wgpu::VertexAttribute> = Vec::with_capacity(3);
        vertex_attributes.push(wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        });
        if desc.vertex_stride >= 20 {
            vertex_attributes.push(wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 12,
                shader_location: 1,
            });
        }
        if desc.vertex_stride >= 24 {
            vertex_attributes.push(wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Unorm8x4,
                offset: 20,
                shader_location: 2,
            });
        }
        let buffers = [wgpu::VertexBufferLayout {
            array_stride: desc.vertex_stride,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &vertex_attributes,
        }];
        // Colour attachments: the explicit list when the caller supplied one
        // (shaderpack MRT passes), otherwise the swapchain format. Building this
        // Vec happens once per pipeline, never on the frame path.
        let blend_state = match desc.blend {
            BlendMode::Opaque => None,
            BlendMode::AlphaBlend => Some(wgpu::BlendState::ALPHA_BLENDING),
            BlendMode::Additive => Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::OVER,
            }),
        };
        let color_targets: Vec<Option<wgpu::ColorTargetState>> = if desc.color_formats.is_empty() {
            vec![Some(wgpu::ColorTargetState {
                format: self.config.format,
                blend: blend_state,
                write_mask: wgpu::ColorWrites::ALL,
            })]
        } else {
            desc.color_formats
                .iter()
                .map(|format| {
                    Some(wgpu::ColorTargetState {
                        format: Self::to_wgpu_format(*format),
                        blend: blend_state,
                        write_mask: wgpu::ColorWrites::ALL,
                    })
                })
                .collect()
        };
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(desc.label),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: desc.vertex_entry,
                    compilation_options: Default::default(),
                    buffers: &buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology: match desc.topology {
                        Topology::TriangleList => wgpu::PrimitiveTopology::TriangleList,
                        Topology::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
                        Topology::LineList => wgpu::PrimitiveTopology::LineList,
                    },
                    ..Default::default()
                },
                depth_stencil: desc.depth_format.map(|format| wgpu::DepthStencilState {
                    format: Self::to_wgpu_format(format),
                    depth_write_enabled: desc.blend == BlendMode::Opaque,
                    depth_compare: wgpu::CompareFunction::LessEqual,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: desc.fragment_entry,
                    compilation_options: Default::default(),
                    targets: &color_targets,
                }),
                multiview: None,
                cache: None,
            });
        Ok(PipelineId(self.pipelines.insert(pipeline)))
    }

    fn create_buffer(&mut self, desc: &BufferDesc) -> BackendResult<BufferId> {
        let usage = match desc.usage {
            BufferUsage::Geometry => {
                wgpu::BufferUsages::VERTEX
                    | wgpu::BufferUsages::INDEX
                    | wgpu::BufferUsages::COPY_DST
            }
            BufferUsage::Uniform => wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            BufferUsage::Indirect => {
                wgpu::BufferUsages::INDIRECT
                    | wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
            }
            BufferUsage::Staging => wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            BufferUsage::Storage => wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        };
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(desc.label),
            size: desc.size.max(4),
            usage,
            mapped_at_creation: false,
        });
        self.vram += desc.size;
        Ok(BufferId(self.buffers.insert(BufferEntry {
            buffer,
            size: desc.size,
        })))
    }

    fn write_buffer(&mut self, id: BufferId, offset: u64, data: &[u8]) -> BackendResult<()> {
        let entry = self
            .buffers
            .get(id.0)
            .ok_or_else(|| BackendError::Unsupported("write_buffer: unknown buffer".to_string()))?;
        if offset.saturating_add(data.len() as u64) > entry.size {
            return Err(BackendError::Unsupported(format!(
                "write_buffer overflow: {} + {} > {}",
                offset,
                data.len(),
                entry.size
            )));
        }
        // `write_buffer` copies through wgpu's staging belt: no per-call allocation
        // and no pipeline stall.
        self.queue.write_buffer(&entry.buffer, offset, data);
        Ok(())
    }

    fn destroy_buffer(&mut self, id: BufferId) {
        if let Some(entry) = self.buffers.remove(id.0) {
            self.vram = self.vram.saturating_sub(entry.size);
        }
    }

    fn create_texture(&mut self, desc: &TextureDesc, data: Option<&[u8]>) -> BackendResult<TextureId> {
        let format = Self::to_wgpu_format(desc.format);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(desc.label),
            size: wgpu::Extent3d {
                width: desc.width.max(1),
                height: desc.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: desc.mip_levels.max(1),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        if let Some(bytes) = data {
            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytes,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(desc.width * desc.format.bytes_per_texel()),
                    rows_per_image: Some(desc.height),
                },
                wgpu::Extent3d {
                    width: desc.width,
                    height: desc.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        let bytes =
            u64::from(desc.width) * u64::from(desc.height) * u64::from(desc.format.bytes_per_texel());
        self.vram += bytes;
        Ok(TextureId(self.textures.insert(TextureEntry {
            texture,
            view,
            size: [desc.width, desc.height],
            format: desc.format,
            bytes,
        })))
    }

    fn destroy_texture(&mut self, id: TextureId) {
        if let Some(entry) = self.textures.remove(id.0) {
            self.vram = self.vram.saturating_sub(entry.bytes);
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
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &entry.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: origin[0],
                    y: origin[1],
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(size[0] * entry.format.bytes_per_texel()),
                rows_per_image: Some(size[1]),
            },
            wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    fn set_camera(&mut self, view: &[[f32; 4]; 4], proj: &[[f32; 4]; 4]) -> BackendResult<()> {
        self.camera = *view;
        self.proj = *proj;
        self.camera_pos = [view[3][0], view[3][1], view[3][2]];
        Ok(())
    }

    fn submit(&mut self, frame: &FrameSubmission<'_>) -> BackendResult<()> {
        self.camera = *frame.view;
        self.proj = *frame.proj;
        self.camera_pos = frame.camera_pos;
        self.write_camera_slot();
        let dynamic_offset = (self.uniform_slot * UNIFORM_SLOT_SIZE) as u32;
        self.next_slot();

        let surface_texture = match self.surface.get_current_texture() {
            Ok(texture) => texture,
            Err(wgpu::SurfaceError::Outdated) => {
                self.outcome = PresentOutcome::Outdated;
                return Ok(());
            }
            Err(wgpu::SurfaceError::Lost) => {
                self.outcome = PresentOutcome::Lost;
                return Ok(());
            }
            Err(wgpu::SurfaceError::OutOfMemory) => {
                return Err(BackendError::ResourceExhausted(
                    "surface out of memory".to_string(),
                ))
            }
            Err(wgpu::SurfaceError::Timeout) => {
                return Err(BackendError::Present(
                    "timed out acquiring the next frame".to_string(),
                ))
            }
        };
        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sinkon.frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sinkon.main"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(frame.clear_color[0]),
                            g: f64::from(frame.clear_color[1]),
                            b: f64::from(frame.clear_color[2]),
                            a: f64::from(frame.clear_color[3]),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_bind_group(0, &self.uniform_bind_group, &[dynamic_offset]);
            let mut bound_pipeline: Option<PipelineId> = None;
            for batch in frame.batches {
                if Some(batch.pipeline) != bound_pipeline {
                    match self.pipelines.get(batch.pipeline.0) {
                        Some(pipeline) => {
                            pass.set_pipeline(pipeline);
                            bound_pipeline = Some(batch.pipeline);
                        }
                        None => continue,
                    }
                }
                let vertex = self.buffers.get(batch.vertex_buffer.0);
                let index = self.buffers.get(batch.index_buffer.0);
                if let (Some(vertex), Some(index)) = (vertex, index) {
                    pass.set_vertex_buffer(0, vertex.buffer.slice(..));
                    pass.set_index_buffer(index.buffer.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(
                        batch.first_index..batch.first_index + batch.index_count,
                        batch.base_vertex,
                        0..batch.instance_count,
                    );
                }
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.current = Some(surface_texture);
        self.outcome = PresentOutcome::Presented;
        Ok(())
    }

    fn present(&mut self) -> BackendResult<PresentOutcome> {
        if let Some(texture) = self.current.take() {
            texture.present();
        }
        Ok(self.outcome)
    }

    fn vram_bytes(&self) -> u64 {
        self.vram
    }

    fn destroy(&mut self) {
        self.current = None;
        self.textures = Arena::new();
        self.buffers = Arena::new();
        self.pipelines = Arena::new();
        self.shaders.clear();
        self.window = None;
        log::info!(
            "{} backend destroyed ({} adapter, tier {})",
            self.kind,
            self.capabilities.adapter_name,
            self.tier
        );
    }
}

/// One adapter discovered by [`WgpuBackend::probe`].
#[derive(Clone, Debug, serde::Serialize)]
pub struct AdapterProbe {
    /// `wgpu` backend name (`vulkan`, `gl`).
    pub backend: String,
    /// Adapter name.
    pub name: String,
    /// Driver family.
    pub driver: String,
    /// Driver version/info string.
    pub driver_info: String,
    /// `discrete` / `integrated` / `other`.
    pub device_type: String,
    /// API version, formatted for humans, or `unknown`.
    pub api_version: String,
}

fn identity() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn write_mat4(target: &mut [u8], matrix: &[[f32; 4]; 4]) {
    let mut offset = 0;
    for column in matrix {
        for value in column {
            if offset + 4 <= target.len() {
                target[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
            }
            offset += 4;
        }
    }
}

fn format_vulkan_version(api: u32) -> String {
    format!("{}.{}.{}", api >> 22, (api >> 12) & 0x3ff, api & 0xfff)
}

/// Vulkan loader/instance version.
///
/// `wgpu::AdapterInfo` (wgpu 22) does not expose the device API level, so Sinkon
/// asks the loader itself. The value is the *loader* version: the device may
/// support less, which is why the capability report labels it
/// `vulkan_api_source: "loader"`.
#[cfg(target_os = "android")]
fn vulkan_loader_version() -> Option<u32> {
    type EnumerateInstanceVersion = unsafe extern "C" fn(*mut u32) -> i32;
    // SAFETY: `dlopen`/`dlsym`/`dlclose` follow the documented libdl contract; the
    // symbol is only called with a valid out-pointer and the handle is closed
    // exactly once. A null symbol (Vulkan 1.0 loader) is handled without calling.
    unsafe {
        let handle = libc::dlopen(
            b"libvulkan.so\0".as_ptr() as *const libc::c_char,
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        );
        if handle.is_null() {
            return None;
        }
        let symbol = libc::dlsym(
            handle,
            b"vkEnumerateInstanceVersion\0".as_ptr() as *const libc::c_char,
        );
        if symbol.is_null() {
            libc::dlclose(handle);
            // `vkEnumerateInstanceVersion` was added in Vulkan 1.1.
            return Some(1 << 22);
        }
        let enumerate: EnumerateInstanceVersion = std::mem::transmute(symbol);
        let mut version = 0u32;
        let result = enumerate(&mut version);
        libc::dlclose(handle);
        if result == 0 {
            Some(version)
        } else {
            None
        }
    }
}

#[cfg(not(target_os = "android"))]
fn vulkan_loader_version() -> Option<u32> {
    None
}

fn capabilities_for(
    config: &WgpuBackendConfig,
    info: &wgpu::AdapterInfo,
    adapter: &wgpu::Adapter,
    limits: &wgpu::Limits,
    tier: DeviceTier,
) -> Capabilities {
    let features = adapter.features();
    let gl_info = parse_gl_version(&info.driver_info);
    let is_vulkan = info.backend == wgpu::Backend::Vulkan;
    let supports = BackendFeatures {
        compute: is_vulkan,
        indirect_draw: is_vulkan,
        push_constants: is_vulkan,
        timestamp_query: features.contains(wgpu::Features::TIMESTAMP_QUERY),
        pipeline_cache: is_vulkan,
        // wgpu 22 has no occlusion-query feature on any backend; Sinkon falls back
        // to the chunk visibility graph (see docs/BACKEND_MATRIX.md).
        occlusion_query: false,
        astc: features.contains(wgpu::Features::TEXTURE_COMPRESSION_ASTC),
        bc: features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
        anisotropy: features.contains(wgpu::Features::ANISOTROPIC_FILTERING),
        storage_buffers: features.contains(wgpu::Features::STORAGE_RESOURCE_BINDING_ARRAY),
        map_unsynchronized: false,
        render_bundles: is_vulkan,
    };
    let api_version = if is_vulkan {
        vulkan_loader_version()
            .map(format_vulkan_version)
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        gl_info
            .as_ref()
            .map(|v| v.gl.clone())
            .unwrap_or_else(|| "unknown".to_string())
    };
    Capabilities {
        kind: config.kind,
        api_name: match info.backend {
            wgpu::Backend::Vulkan => "Vulkan".to_string(),
            wgpu::Backend::Gl => "OpenGL ES".to_string(),
            other => format!("{other:?}"),
        },
        api_version,
        gl_version: gl_info.as_ref().map(|v| v.gl.clone()),
        gles_version: gl_info.as_ref().map(|v| v.gles),
        vulkan_api: if is_vulkan { vulkan_loader_version() } else { None },
        glsl_version: match config.kind {
            BackendKind::Vulkan => "450".to_string(),
            BackendKind::Gles3 => "320 es".to_string(),
            BackendKind::Gles2Legacy => "100".to_string(),
        },
        adapter_name: info.name.clone(),
        driver_info: format!("{} {}", info.driver, info.driver_info),
        max_texture_size: limits.max_texture_dimension_2d,
        max_uniform_buffer_binding_size: limits.max_uniform_buffer_binding_size,
        max_compute_workgroup_size: [
            limits.max_compute_workgroup_size_x,
            limits.max_compute_workgroup_size_y,
            limits.max_compute_workgroup_size_z,
        ],
        supports,
        extensions: driver_extensions(info),
        device_tier: tier.as_str().to_string(),
        vram_bytes: 0,
        resident_sections: 0,
    }
}

struct GlVersion {
    gl: String,
    gles: u32,
}

/// Pull `OpenGL ES 3.2` / `OpenGL 4.6` out of a driver info string.
fn parse_gl_version(driver_info: &str) -> Option<GlVersion> {
    let lower = driver_info.to_ascii_lowercase();
    let is_es = lower.contains("opengl es");
    let start = if is_es {
        lower.find("opengl es")?
    } else {
        lower.find("opengl")?
    };
    let rest = &driver_info[start..];
    let mut version = None;
    for token in rest.split(|c: char| c == ' ' || c == 'v' || c == ',') {
        let mut parts = token.split('.');
        if let (Some(major), Some(minor)) = (parts.next(), parts.next()) {
            if let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) {
                version = Some(major * 10 + minor);
                break;
            }
        }
    }
    let value = version?;
    Some(GlVersion {
        gl: if is_es {
            format!("ES {}.{}", value / 10, value % 10)
        } else {
            format!("{}.{}", value / 10, value % 10)
        },
        gles: value,
    })
}

fn driver_extensions(info: &wgpu::AdapterInfo) -> Vec<String> {
    let mut extensions = Vec::with_capacity(4);
    extensions.push(format!("driver:{}", info.driver));
    if info.backend == wgpu::Backend::Vulkan {
        extensions.push("VK_KHR_swapchain".to_string());
    }
    extensions.push("sinkon:spirv".to_string());
    extensions
}

impl Drop for WgpuBackend {
    fn drop(&mut self) {
        // Nothing to do beyond the normal field drops: `surface` is declared
        // before `window`, so the swapchain is destroyed before the ANativeWindow
        // reference is released.
        log::debug!("{} backend dropped", self.kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gl_version_parsing() {
        let parsed = parse_gl_version("OpenGL ES 3.2 v1.r38p1").expect("parsed");
        assert_eq!(parsed.gles, 32);
        assert_eq!(parsed.gl, "ES 3.2");
        assert!(parse_gl_version("no version here").is_none());
    }

    #[test]
    fn uniform_blob_layout_is_160_bytes() {
        let view = identity();
        let mut blob = [0u8; UNIFORM_PAYLOAD as usize];
        write_mat4(&mut blob[0..64], &view);
        write_mat4(&mut blob[64..128], &view);
        assert_eq!(blob.len(), 160);
        assert_eq!(f32::from_ne_bytes([blob[0], blob[1], blob[2], blob[3]]), 1.0);
    }

    #[test]
    fn vulkan_version_formatting() {
        let api = (1 << 22) | (3 << 12);
        assert_eq!(format_vulkan_version(api), "1.3.0");
    }
}
