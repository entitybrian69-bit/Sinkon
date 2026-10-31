# Sinkon architecture

Sinkon is one `cdylib` (`libsinkon.so`) that carries three rendering paths behind a
single dispatch boundary:

| Backend | Crate | API | Devices |
| --- | --- | --- | --- |
| `BackendKind::Vulkan` | `wgpu` 22 (`Backends::VULKAN`) | Vulkan 1.1+ | Android 8+ with a Vulkan driver |
| `BackendKind::Gles3` | `wgpu` 22 (`Backends::GL`) | GLES 3.0/3.1 via EGL | everything with GLES 3.x |
| `BackendKind::Gles2Legacy` | `glow` 0.14 + EGL FFI | GLES 2.0/2.1 | legacy devices, Alpha–1.6 profiles |

```
                        ┌──────────────────────────────┐
   Kotlin (RenderSpec)  │  src/jni_bridge.rs           │  Java_org_sinkon_launcher_Sinkon_*
                        └──────────────┬───────────────┘
                                       │
                        ┌──────────────▼───────────────┐
                        │  src/renderer.rs             │  SinkonRenderer: camera, frame
                        │  frame path (0 allocations)  │  scratch, pipelines, reports
                        └───┬───────────┬───────────┬──┘
                            │           │           │
             ┌──────────────▼──┐  ┌─────▼──────┐  ┌─▼──────────────┐
             │ src/chunk.rs    │  │src/shader/ │  │ src/perf/      │
             │ meshing, pool,  │  │ naga →     │  │ tier, hists,   │
             │ culling, workers│  │ SPIR-V/GLSL│  │ alloc gate     │
             └──────────────┬──┘  └─────┬──────┘  └────────────────┘
                            │           │
                        ┌───▼───────────▼──────────────────────────┐
                        │ src/backend/mod.rs — `GraphicsBackend`   │
                        ├──────────┬──────────────┬────────────────┤
                        │ vulkan.rs│ gles3.rs     │ gles2_legacy.rs│
                        │ (wgpu)   │ (wgpu)       │ (glow + EGL)   │
                        └──────────┴──────────────┴────────────────┘
```

## Trait deviation (documented on purpose)

The brief sketches `GraphicsBackend` with `create_surface(anw)` and
`create_device_and_queues()` as trait methods. On Android those steps are
inseparable from the `ANativeWindow` and the EGL display, and a *trait object*
cannot be created before the object that owns them exists. Sinkon therefore
splits the responsibility:

* free functions `backend::create(&SurfaceTarget, kind, tier)` and
  `backend::select(&SurfaceTarget, hint, mc_version, tier)` perform surface +
  device creation and hand back `Box<dyn GraphicsBackend>` (plus the
  `SelectionRecord` for `select`);
* the trait keeps the per-frame and per-resource surface:

```rust
pub trait GraphicsBackend: Send {
    fn kind(&self) -> BackendKind;
    fn capabilities(&self) -> &Capabilities;
    fn resize(&mut self, width: u32, height: u32) -> BackendResult<()>;
    fn create_shader(&mut self, key: ShaderKey, code: ShaderCode<'_>) -> BackendResult<()>;
    fn create_pipeline(&mut self, desc: &PipelineDesc) -> BackendResult<PipelineId>;
    fn create_buffer(&mut self, desc: &BufferDesc) -> BackendResult<BufferId>;
    fn write_buffer(&mut self, id: BufferId, offset: u64, data: &[u8]) -> BackendResult<()>;
    fn destroy_buffer(&mut self, id: BufferId);
    fn create_texture(&mut self, desc: &TextureDesc, data: Option<&[u8]>) -> BackendResult<TextureId>;
    fn destroy_texture(&mut self, id: TextureId);
    fn update_texture(&mut self, id: TextureId, origin: [u32; 2], size: [u32; 2], data: &[u8])
        -> BackendResult<()>;
    fn set_camera(&mut self, view: &[[f32; 4]; 4], proj: &[[f32; 4]; 4]) -> BackendResult<()>;
    fn submit(&mut self, frame: &FrameSubmission<'_>) -> BackendResult<()>;
    fn present(&mut self) -> BackendResult<PresentOutcome>;
    fn vram_bytes(&self) -> u64;
    fn reload_shaders(&mut self) -> BackendResult<()> {
        Ok(())
    }
    fn destroy(&mut self);
}
```

Everything above the trait (meshing, culling, uniform layout, shader translation,
reports, JNI) is written once and dispatched.

## Backend selection

`backend::selection_plan(hint, profile)` returns the ordered list of backends to
try; `backend::select` walks it and records every attempt in a `SelectionRecord`.

* `hint != 0` collapses the plan to the single forced backend — no fallback. If it
  fails, `nativeInit` returns `0` and the reason is in logcat.
* `hint == 0` (Sinkon (Auto)):
  * **Legacy** profile (Alpha–1.6): `gles2_legacy` → `gles3` → `vulkan`
  * everything else: `vulkan` → `gles3` → `gles2_legacy` (only when GLES < 3.0)
  * nothing works → hard fail with an explicit log line and a capability report
    that names every rejected backend and why.

Degradations are never silent: `SelectionRecord::degraded` is `true`, a `warn` line
is logged, and the JSON report exposes `selection.attempted[]`.

## Frame path

`SinkonRenderer::render_frame` is allocation-free by construction:

1. drain finished meshes from the lock-free return queue
   (`crossbeam_queue::ArrayQueue<MeshResult>`) into a preallocated
   `Vec<MeshResult>` (`Vec::clear`, never freed);
2. upload at most `upload_budget` mesh buffers into pooled GPU slots, reusing slot
   buffers (`write_buffer`, never `create_buffer`);
3. build the draw-batch list into a preallocated `Vec<DrawBatch>`;
4. cull with `CullScratch` (preallocated), sorted front-to-back;
5. write the camera uniform blob into a preallocated 160-byte array;
6. `backend.submit(&frame)` then `backend.present()`;
7. record timings into the histogram.

No `Vec::new`, `String`, `format!`, `Box`, mutex lock or JNI object creation happens
in steps 1–7. The `alloc-gate` feature wraps the frame in a counting allocator and
records any allocation into `PerfSnapshot::frame_allocations`, which is asserted to
be `0` by `cargo test --features alloc-gate`.

## Threading

| Work | Thread | Mechanism |
| --- | --- | --- |
| Meshing | worker pool, `num_cpus - 1` (Tier S/A), `min(4, num_cpus)` (B), `2` (C) | `std::thread` + `crossbeam_queue::ArrayQueue<MeshJob>` |
| Mesh return | workers → render thread | `crossbeam_queue::ArrayQueue<MeshResult>`, never `Mutex<Vec>` |
| GPU upload | render thread only | workers build CPU buffers, render thread uploads |
| Pipeline cache | background thread at init, `destroy` writes it | `wgpu::PipelineCache` (Vulkan only) |

The legacy backend is additionally guarded by an owner-thread check: every method
that touches `glow` compares the current `ThreadId` with the one captured at
context creation and returns `BackendError::Jni("legacy GLES2 called from a
non-owner thread")` instead of corrupting GL state.

## `unsafe` invariants per backend

Every `unsafe` block in the crate carries a `// SAFETY:` comment; the per-module
invariants are:

### `src/surface.rs`
* `ANativeWindow_fromSurface` returns a **referenced** window; `SurfaceHandle`
  releases it exactly once in `Drop`. `borrow_raw` is used only for surfaces the
  caller keeps alive (offscreen tests) and never releases.
* `RawWindow` implements `HasWindowHandle`/`HasDisplayHandle` by returning the raw
  `ANativeWindow*`; the pointer is valid as long as the owning `SurfaceHandle` is
  alive, and wgpu only dereferences it during `create_surface_unsafe`.

### `src/backend/wgpu_shared.rs`
* `create_surface_unsafe(SurfaceTargetUnsafe::RawHandle { … })` is called with a
  window handle borrowed from a live `SurfaceHandle`; the resulting `wgpu::Surface`
  is dropped before the handle.
* Buffer writes go through `Queue::write_buffer`, whose `&[u8]` is only read during
  the call.
* Texture uploads map exactly `bytes_per_row * height` bytes from the caller's
  slice; the row stride is validated before the copy.
* All resource handles are Sinkon-owned IDs validated against generation-checked
  arenas, so a stale ID returns `BackendError::InvalidArgument` instead of indexing
  out of bounds.

### `src/backend/gles2_legacy.rs`
* EGL entry points are declared with `#[link(name = "EGL")]`; every returned
  pointer is null-checked before use, and a null return becomes a `BackendError`.
* `glow::Context::from_loader_function` is built from `eglGetProcAddress`; a missing
  symbol fails the context creation, never a null call.
* The `RingAllocator` hands out 4 MiB of persistently mapped VBO space; the
  returned range is validated against the arena before being turned into a slice.
* Legacy program creation only happens on first use of a `LegacyKey` bit pattern;
  the 16-entry table is fully populated before the first `render_frame`.

### `src/jni_bridge.rs`
* The `jlong` handle is a `Box<NativeHandle>` with a magic word; `handle()` rejects
  `0`, null and magic mismatches before dereferencing, so a stale handle returns an
  error instead of a segfault.
* `GetDirectBufferAddress` pointers are wrapped in slices bounded by
  `GetDirectBufferCapacity` and never outlive the JNI call.
* `nativeDestroy` is the only place that reconstitutes the `Box`; a second destroy
  sees a zeroed magic and refuses to free.

### `src/perf/alloc.rs`
* The counting allocator wraps `System` and only increments atomics; it never
  allocates itself (thread-local flag read is `Relaxed`).

## Error handling

`BackendError` is the only error type crossing module boundaries:
`Unavailable(String)`, `Surface(String)`, `Adapter(String)`, `Device(String)`,
`Unsupported(String)`, `ResourceExhausted(String)`, `Present(String)`,
`DeviceLost(String)`, `Jni(String)`, `InvalidArgument(String)`, `Io(String)`.
The JNI boundary maps them to log lines plus a sentinel return value; nothing
panics across FFI (the bridge is additionally wrapped in `catch_unwind`, which is a
belt-and-braces measure because release builds use `panic = "abort"`).
