# Backend matrix

What each backend can do, what Sinkon does about it, and what has actually been
verified.

**Verification legend**

| Mark | Meaning |
| --- | --- |
| ✅ implemented | Code path exists and is exercised by unit tests / compile-time checks in this repository |
| ⚠️ not verified | Implemented, but never executed on real hardware — no Android device or NDK/Gradle toolchain was available in the build sandbox |
| ❌ unsupported | Not available on that backend; Sinkon reports it in the capability JSON and falls back |

> Every row in the "verified on device" sense is ⚠️. The sandbox that produced this
> repository has no Rust toolchain, no NDK, no Gradle and no GPU, so nothing has
> been run on a phone yet. See `docs/KNOWN_ISSUES.md` (SINKON-001, SINKON-002).

## Feature surface

| Capability | Vulkan (wgpu) | GLES 3.x (wgpu GL) | GLES 2.0 legacy (glow) |
| --- | --- | --- | --- |
| Capability JSON | ✅ identical schema | ✅ identical schema | ✅ identical schema |
| Compute shaders | ✅ `supports.compute` | ⚠️ only when the driver exposes compute (ES 3.1) | ❌ |
| Indirect draws | ✅ `supports.indirect_draw` | ❌ (GL has no `draw_indirect`) | ❌ |
| Push constants | ✅ `supports.push_constants` | ❌ | ❌ |
| Render bundles | ✅ `supports.render_bundles` | ❌ | ❌ |
| Pipeline cache on disk | ✅ `wgpu::PipelineCache`, async load/save | ❌ logged `warn`, `supports.pipeline_cache=false` | ❌ |
| Storage buffers | ✅ | ⚠️ ES 3.1 only | ❌ |
| `map_unsynchronized` (persistent uniforms) | ✅ | ⚠️ only where the driver advertises it | ❌ (ring + `glBufferSubData`) |
| Occlusion queries | ❌ wgpu 22 has no portable API | ❌ | ❌ |
| Visibility fallback | ✅ chunk visibility graph + CPU frustum | ✅ same | ✅ same |
| Multi-draw / instancing | ✅ instanced batches | ✅ instanced batches | ⚠️ instanced draw only where `EXT_draw_instanced` exists |
| MRT (multiple render targets) | ✅ | ✅ ES 3.0 | ❌ (`gl_FragData`, rejected by the ES 1.00 downleveller) |
| Uniform blocks (UBOs) | ✅ | ✅ ES 3.0 | ❌ (emulated with loose uniforms) |
| Texture compression | ⚠️ ASTC/BC flags reported from wgpu features; the atlas upload path is RGBA8-only today (SINKON-013) | ⚠️ same | ❌ RGBA8 only |
| Async compute queue | ⚠️ second queue family used when present, else graphics queue (SINKON-012) | ❌ | ❌ |
| Shader hot reload | ✅ `nativeReloadShaders` + mtime polling | ✅ | ✅ |
| Zero per-frame allocations (Sinkon scope) | ✅ enforced by `alloc-gate` test | ✅ | ✅ |
| Zero per-frame buffer/pipeline creation | ✅ | ✅ | ✅ (program table pre-built for all 16 keys) |

## Legacy-path limits (GLES 2.0 via `glow`)

The legacy path exists so Alpha–1.6 worlds render on GLES 2.0-only devices. It
speaks a deliberately small dialect:

| Feature | Legacy path | Reason / consequence |
| --- | --- | --- |
| `glSelect`, selection/feedback buffer | ❌ | Not exposed by GLES 2.0 and not needed by the renderer; the launcher's picking must stay on the CPU |
| Feedback rendering (`GL_FEEDBACK`) | ❌ | Same as above |
| User clip planes | ❌ | No `gl_ClipDistance` in ES 1.00; water/fog use fragment-side emulation |
| Polygon stipple | ❌ | Removed from GLES 2.0 |
| `glDrawPixels` / `glBitmap` | ❌ | Never used; all 2D goes through the texture path |
| UBOs / SSBOs / MRT / integer types / `textureLod` | ❌ | Rejected by `emit_glsl::downlevel_to_es100` with a named reason |
| Deferred / composite shaderpack passes | ❌ | Need MRT + UBOs; shaderpacks fall back to vanilla on this backend |

## Behaviour parity decisions

| Situation | Vulkan | GLES 3.x | GLES 2.0 legacy |
| --- | --- | --- | --- |
| Surface lost | `BackendError::Present` → launcher recreates the surface, device kept | same | `eglSwapBuffers` failure → surface recreated on next `resize` |
| Outdated swapchain | `resize()` reconfigures and logs | same | buffers re-geometry'd, no device work |
| Device lost | renderer marks the backend dead, `nativeRenderFrame` returns the error, `nativeInit` must be called again | same | n/a (no device object) |
| Forced backend fails | `nativeInit` returns `0` — no silent fallback | same | same |
| Auto with no usable backend | hard fail + explicit log line + report listing every attempt | same | same |
| Wrong-thread call | allowed (wgpu is thread-safe) | allowed | rejected with `BackendError::Jni` |
