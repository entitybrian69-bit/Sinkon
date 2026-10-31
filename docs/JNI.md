# Sinkon JNI contract

All exports live in one library, `libsinkon.so`, and are prefixed
`Java_org_sinkon_launcher_Sinkon_`. The Kotlin mirror is
`android/app/src/main/java/org/sinkon/launcher/Sinkon.kt`; the Rust side is
`src/jni_bridge.rs`.

Nothing here is looked up per frame: the bridge resolves what it needs in
`nativeInit` and keeps it in a `GlobalRef`. Sinkon never calls back into Java, so
there is no cached `jclass`/`jmethodID` on the hot path at all.

## Exports

| Export | Signature | Thread | Notes |
| --- | --- | --- | --- |
| `nativeInit` | `(Surface, int w, int h, String mcVersion, int backendHint) → long` | render thread | Returns an opaque handle, `0` on failure. `backendHint`: 0 auto, 1 Vulkan, 2 GLES3, 3 GLES2 legacy. |
| `nativeResize` | `(long ctx, int w, int h)` | render thread | Recreates the swapchain/surface configuration. Budget ≤ 5 µs. |
| `nativeRenderFrame` | `(long ctx)` | render thread | Zero allocations, zero JNI object creation, zero pipeline/bind-group creation. Budget ≤ 50 µs of JNI time. |
| `nativeDestroy` | `(long ctx)` | render thread | Frees the handle exactly once; a second call is rejected by the handle magic. |
| `nativeBakeSection` | `(long ctx, int x, int y, int z, ByteBuffer vbo, ByteBuffer ibo, int formatId)` | any | `vbo` is 4096 block ids (one byte each, direct). `ibo` is the launcher's palette/state buffer; accepted but optional. `formatId`: 0 = Packed4 (20 B vertex), 1 = Packed8 (24 B), ≥ 2 = Custom(24 B). |
| `nativeSetCamera` | `(long ctx, FloatBuffer view, FloatBuffer proj)` | render thread | 16 floats each, column-major, direct buffers, reused across frames. Budget ≤ 10 µs. |
| `nativeUploadTexture` | `(long ctx, int texId, int w, int h, ByteBuffer pixels)` | render thread | RGBA8, `w*h*4` bytes, direct. No intermediate `Vec` on the Java side; the backend copies straight from the mapped buffer. |
| `nativeSetShaderpack` | `(long ctx, String pathDir)` | any | `null`/empty clears the pack. |
| `nativeReloadShaders` | `(long ctx) → int` | any | `0` = all compiled, `1` = partial failure, `-1` = invalid handle. |
| `nativeGetCapabilityReport` | `(long ctx) → String` | any | JSON, see below. `null` on failure. |
| `nativeGetPerfStats` | `(long ctx) → String` | any | JSON, see below. `null` on failure. |
| `nativeSwitchBackend` | `(long ctx, int backendHint) → int` | render thread | `0` success, `1` no backend produced a device, `2` device OK but resources could not be rebuilt. |

`JNI_OnLoad` installs the `Sinkon`-tagged Android logger before any other call and
returns `JNI_VERSION_1_6`.

## `nativeGetCapabilityReport`

Identical field set on every backend — the launcher and the shader layer never
branch on the backend name for capability decisions.

```json
{
  "active_backend": "vulkan",
  "available_backends": ["vulkan", "gles3"],
  "capabilities": {
    "kind": "Vulkan",
    "api_name": "Vulkan",
    "api_version": "1.3.0",
    "gl_version": null,
    "gles_version": null,
    "vulkan_api": 4206592,
    "glsl_version": "450",
    "adapter_name": "Adreno (TM) 740",
    "driver_info": "Mesa 24.1 / Turnip",
    "max_texture_size": 16384,
    "max_uniform_buffer_binding_size": 65536,
    "max_compute_workgroup_size": [1024, 1024, 64],
    "supports": {
      "compute": true,
      "indirect_draw": true,
      "push_constants": true,
      "timestamp_query": true,
      "pipeline_cache": true,
      "occlusion_query": false,
      "astc": true,
      "bc": false,
      "anisotropy": true,
      "storage_buffers": true,
      "map_unsynchronized": false,
      "render_bundles": true
    },
    "extensions": ["VK_KHR_swapchain", "VK_EXT_robustness2"],
    "device_tier": "S",
    "vram_bytes": 0,
    "resident_sections": 0
  },
  "selection": {
    "requested_hint": 0,
    "active": "Vulkan",
    "attempted": [{"backend": "Vulkan", "ok": true, "reason": "adapter created"}],
    "version_profile": "Current",
    "mc_version": "1.21.1",
    "degraded": false
  },
  "tier": "S",
  "version_profile": "Current",
  "glsl_version": "450",
  "meshes": {"resident": 430, "capacity": 512, "bytes": 0, "evictions": 0},
  "worker_threads": 7,
  "render_distance": 12,
  "shaderpack": "vanilla",
  "shaders": 6,
  "shader_errors": [],
  "sinkon_version": "0.1.0"
}
```

`occlusion_query` is `false` on **every** backend: wgpu 22 does not expose
occlusion queries through its portable API, so Sinkon uses a chunk visibility
graph with a conservative CPU fallback (see `docs/KNOWN_ISSUES.md`).

`selection.degraded = true` means the active backend is not the first choice of the
plan. Every degradation is also written to logcat at `warn` level.

## `nativeGetPerfStats`

```json
{
  "backend": "Vulkan",
  "tier": "S",
  "frames": 5120,
  "target_fps": 120,
  "main_thread_budget_us": 2000,
  "submission": {"count": 512, "mean_us": 842.5, "p50_us": 780, "p95_us": 1290, "p99_us": 1740, "max_us": 2260},
  "present": {"count": 512, "mean_us": 861.0, "p50_us": 810, "p95_us": 1330, "p99_us": 1810, "max_us": 2400},
  "counters": {
    "frames": 5120,
    "batches": 2140,
    "sections_uploaded": 212,
    "sections_culled": 1480,
    "sections_drawn": 936,
    "bytes_uploaded": 33554432
  },
  "frame_allocations": 0,
  "jni_calls": 15360,
  "jni_mean_ns": 1800,
  "bind_group_creations": 3,
  "buffer_creations": 258,
  "program_creations": 6,
  "pipeline_cache_hit_rate": 1.0,
  "vram_bytes": 0,
  "resident_sections": 212,
  "backend_allocations": 0,
  "uptime_ms": 42000,
  "render_distance": 12,
  "profile": "Current",
  "queued_uploads": 0,
  "dropped_sections": 0
}
```

`frame_allocations` counts allocations on the render thread inside Sinkon's own
scope (enabled by the `alloc-gate` feature). `backend_allocations` counts
driver-layer allocations during the frame on the wgpu/glow paths — it is reported
separately because a third-party driver allocator cannot be forced to zero; Sinkon
never allocates there itself (no pipeline, bind group or buffer creation happens in
`render_frame`).
