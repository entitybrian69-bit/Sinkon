# SINKON_STATUS

One page, no exaggeration: what is built, what works, what is best-effort and what
is known-broken — per backend.

**Nothing has been executed.** No Rust toolchain, NDK, Gradle or Android device was
available while this repository was produced (`docs/KNOWN_ISSUES.md` SINKON-001,
SINKON-002). "Built" below therefore means *written and syntax-checked*, never
*compiled or run*.

## Backends

| | **Vulkan** (`wgpu::Backends::VULKAN`) | **GLES 3.x** (`wgpu::Backends::GL`) | **GLES 2.0 legacy** (`glow` + EGL) |
| --- | --- | --- | --- |
| Implementation | built | built | built |
| Device/queue creation | built (wgpu) | built (wgpu + EGL) | built (EGL + `glow`) |
| Surface + swapchain | built (`create_surface_unsafe`) | built | built (EGLSurface from `ANativeWindow`) |
| Terrain pipeline (2 vertex formats) | built | built | built (16 pre-compiled state programs) |
| Pooled chunk buffers + uploads | built | built | built (ring allocator, `glBufferSubData`) |
| Frustum + visibility-graph culling | built | built | built |
| Shader path | Naga → SPIR-V | Naga → SPIR-V (wgpu GL consumes SPIR-V) | Naga → GLSL ES 3.00 → ES 1.00 downleveller |
| Shaderpack parse/translate/validate | built | built | rejects (UBO/MRT/etc.) with named reasons |
| Shaderpack **rendering** (deferred chain) | not implemented (SINKON-004) | not implemented (SINKON-004) | not implemented (SINKON-004) |
| Hot reload | built | built | built |
| Pipeline cache persistence | built (async load/save) | ❌ n/a (SINKON-014) | ❌ |
| Indirect draws / render bundles / push constants | built | ❌ n/a | ❌ n/a |
| Occlusion queries | ❌ (SINKON-010) | ❌ (SINKON-010) | ❌ (SINKON-010) |
| Capability + perf JSON | built, identical schema | built, identical schema | built, identical schema |
| Zero per-frame allocations (Sinkon scope) | designed + gated by `alloc-gate` (not run) | same | same |
| **Verified on hardware** | ❌ never run | ❌ never run | ❌ never run |

## Platform

| Item | Status |
| --- | --- |
| `libsinkon.so` for aarch64 / armv7 / x86_64 | build script written (`scripts/build-android.sh`), never executed |
| JNI exports (12) matching `Java_org_sinkon_launcher_Sinkon_*` | written; parsed, not compiled |
| Kotlin JNI bindings, probes, session, render loop | written; never compiled |
| Three `RenderSpec`s + MojoLauncher patch | written; patch verified to apply cleanly to `v3_openjdk` |
| `./gradlew assembleDebug` | **NOT RUN** — no Gradle/JDK in the sandbox |
| Launcher integration end-to-end | blocked by SINKON-005 (surface ownership) and SINKON-006 (no world-data hook) |

## Performance

| | Vulkan | GLES 3.x | GLES 2.0 legacy |
| --- | --- | --- | --- |
| Frame-time budgets | targets defined (`perf/tier.rs`) | targets defined | targets defined |
| Measured P50/P95/P99 | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |
| Allocations/frame | NOT MEASURED — reason: no toolchain to run the gate | NOT MEASURED — reason: no toolchain | NOT MEASURED — reason: no toolchain |
| JNI µs/frame | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |
| Regression baseline | `{}` — the gate soft-passes (SINKON-009) | same | same |

## Shaders

| | Vulkan | GLES 3.x | GLES 2.0 legacy |
| --- | --- | --- | --- |
| Vanilla core shaders (built-ins) | built | built | built (ES 1.00 pair) |
| OptiFine packs (BSL, Chocapic13, MakeUp, Sildur's) | parse/translate/validate | parse/translate/validate | ❌ unsupported |
| Iris packs (Complementary Reimagined/Unbound, Rethinking Voxels) | parse/translate/validate | parse/translate/validate (compute excluded below ES 3.1) | ❌ unsupported |
| SEUS PTGI | best-effort (needs compute + SSBO) | ❌ unsupported | ❌ unsupported |
| Visible effect of selecting a pack | diagnostics only (SINKON-004) | diagnostics only | n/a |

## Versions

`docs/VERSION_MATRIX.md`: every version × backend is **best-effort**, none verified.
Legacy (Alpha–1.6) prefers the GLES 2.0 path; Transition/Modern/Current prefer
Vulkan. 1.7+ on the legacy path is known-broken (VBO-era GL semantics).

## Verdict

**Best-effort, unverified.** The architecture, the backends, the shader pipeline,
the perf harness and the launcher integration are complete on paper and internally
consistent; the next milestone is not more code — it is the first compile and the
first run on hardware (`docs/KNOWN_ISSUES.md` SINKON-001/002/003).
