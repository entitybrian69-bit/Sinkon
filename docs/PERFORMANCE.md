# Sinkon Performance Report

> **Status: ⚠️ nothing in this document has been measured.**
> The machine that produced this repository has no Android device, no GPU, no Rust
> toolchain and no NDK. Every measured cell therefore reads
> `NOT MEASURED — reason: …`. No number below is estimated, extrapolated or
> invented. See `docs/KNOWN_ISSUES.md` SINKON-001 and SINKON-009.

## 1. Reference Environment

| Field | Value |
| --- | --- |
| Device / SoC | NOT MEASURED — reason: no Android device available in the build sandbox |
| GPU / Driver (Vulkan) | NOT MEASURED — reason: no Vulkan-capable GPU available in the build sandbox |
| GPU / Driver (GLES) | NOT MEASURED — reason: no GLES-capable GPU available in the build sandbox |
| Android version | NOT MEASURED — reason: no device; the build target is API 26+ |
| Minecraft version(s) tested | NOT MEASURED — reason: no launcher runtime; the version matrix is design-only (`docs/VERSION_MATRIX.md`) |
| Renderer build (git sha) | NOT MEASURED — reason: no binary was built; sources live in this repository |
| Rust version | NOT MEASURED — reason: no Rust toolchain; `rust-toolchain.toml` pins the channel for the first real build |
| NDK version | NOT MEASURED — reason: no NDK; `android/app/build.gradle.kts` pins `26.1.10909125` |

## 2. Method

The harness is fully specified even though it has not been run:

* **Frame times** — `crate::perf::histogram::FrameHistogram` records submission and
  present durations per frame on the render thread and exposes P50/P95/P99/max
  through `nativeGetPerfStats` (`submission` / `present` objects). Spans are created
  with `perf::tracing` (`tracing` + Android logcat sink, `tracy` behind the `tracy`
  feature).
* **Allocations** — the `alloc-gate` feature installs a counting global allocator;
  `nativeRenderFrame` brackets the Sinkon scope with `alloc::begin_frame()` /
  `alloc::end_frame()` and stores the delta in `PerfSnapshot::frame_allocations`.
  `cargo test --features alloc-gate` fails if the value is non-zero.
* **JNI cost** — `std::time::Instant` around each `native*` entry point, aggregated
  in `PerfSnapshot::{jni_calls, jni_mean_ns}`.
* **CPU micro-benchmarks** — `benches/sinkon_benches.rs` (criterion), one group per
  backend, ids prefixed `vulkan_`, `gles3_`, `gles2_`.
* **Regression gate** — `scripts/perf-gate.sh` compares `cargo criterion
  --message-format=json` output against the per-backend baseline in
  `benches/baseline.json`.

Commands:

```sh
# CPU micro-benchmarks for all three backends
cargo bench --bench sinkon_benches

# Allocation gate (fails if nativeRenderFrame allocates)
cargo test --features alloc-gate

# Regression gate (per-backend baselines)
./scripts/perf-gate.sh --strict --backends=vulkan,gles3,gles2

# On-device: logcat with the Sinkon tag, then pull the report
adb logcat -s Sinkon | tee sinkon.logcat
adb shell am start -n git.artdeell.mojo/.MainActivity   # with a Sinkon RenderSpec selected
```

Backend selection during measurement: `nativeInit` is called with
`backendHint = 1` (Vulkan), `2` (GLES3) and `3` (GLES2 legacy) to measure each
backend in isolation; `0` (Auto) is measured separately to capture the selection
cost and the fallback path. The active backend is asserted from
`nativeGetCapabilityReport.active_backend` before any run is accepted.

## 3. Backend Selection & Fallback

| Condition | Selected Backend | Notes |
| --- | --- | --- |
| Vulkan adapter present, `compatibleDevice` passes | Vulkan | Expected default for Tier S/A; measured selection time NOT MEASURED — reason: no device |
| Vulkan absent, GLES ≥ 3.0 | GLES 3.x | `selection.degraded = true` + `warn` log line |
| Vulkan absent, GLES < 3.0, MC version Legacy | GLES 2.0 legacy (glow) | Only combination where the legacy path is allowed to lead |
| No backend available | Hard fail | `nativeInit` returns `0`; every attempt is logged and listed in the capability report |

## 4. Frame-Time Budgets (Target vs Measured, per backend)

⚠️ All measured columns: `NOT MEASURED — reason: no device, no build`.

### 4a. Vulkan

| Tier | Target FPS | Target main-thread ms | Measured P50 | Measured P95 | Measured P99 | Status |
| --- | --- | --- | --- | --- | --- | --- |
| S | 120 | ≤ 2.0 | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |
| A | 90 | ≤ 2.5 | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |
| B | 60 | ≤ 4.0 | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |
| C | 30 | ≤ 8.0 | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |

### 4b. GLES 3.x

| Tier | Target FPS | Target main-thread ms | Measured P50 | Measured P95 | Measured P99 | Status |
| --- | --- | --- | --- | --- | --- | --- |
| S | 90 | ≤ 2.5 | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |
| A | 70 | ≤ 3.0 | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |
| B | 45 | ≤ 5.0 | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |
| C | 30 | ≤ 8.0 | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |

### 4c. GLES 2.0 legacy

| Tier | Target FPS | Target main-thread ms | Measured P50 | Measured P95 | Measured P99 | Status |
| --- | --- | --- | --- | --- | --- | --- |
| B | 30 | ≤ 8.0 | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |
| C | 20 | ≤ 12.0 | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |

## 5. Allocations per Frame (per backend)

| Backend | Path | Target | Measured | Status |
| --- | --- | --- | --- | --- |
| Vulkan | `nativeRenderFrame` | 0 | NOT MEASURED — reason: no Rust toolchain to run `cargo test --features alloc-gate` | ⚠️ |
| Vulkan | `nativeBakeSection` (hot path) | 0 (hot path) | NOT MEASURED — reason: no Rust toolchain / no device | ⚠️ |
| Vulkan | `nativeUploadTexture` (hot path) | 0 (hot path) | NOT MEASURED — reason: no Rust toolchain / no device | ⚠️ |
| GLES 3.x | `nativeRenderFrame` | 0 | NOT MEASURED — reason: no Rust toolchain to run `cargo test --features alloc-gate` | ⚠️ |
| GLES 3.x | `nativeBakeSection` (hot path) | 0 (hot path) | NOT MEASURED — reason: no Rust toolchain / no device | ⚠️ |
| GLES 2.0 | `nativeRenderFrame` | 0 | NOT MEASURED — reason: no Rust toolchain to run `cargo test --features alloc-gate` | ⚠️ |

Design note: `nativeBakeSection` copies the 4096-byte block array once into an owned
`Vec<u8>` that the job queue owns — that is one allocation per *section*, not per
frame, and it is unavoidable because the Java buffer must be released before the
worker finishes. The render-thread frame path allocates nothing.

## 6. JNI Overhead (per backend)

| Backend | Subsystem | Target | Measured | Status |
| --- | --- | --- | --- | --- |
| Vulkan | Per-frame total | ≤ 50 µs | NOT MEASURED — reason: no device | ⚠️ |
| GLES 3.x | Per-frame total | ≤ 50 µs | NOT MEASURED — reason: no device | ⚠️ |
| GLES 2.0 | Per-frame total | ≤ 50 µs | NOT MEASURED — reason: no device | ⚠️ |
| (all) | `nativeSetCamera` | ≤ 10 µs | NOT MEASURED — reason: no device | ⚠️ |
| (all) | `nativeResize` | ≤ 5 µs | NOT MEASURED — reason: no device | ⚠️ |

## 7. GPU-side

### 7a. Vulkan

| Metric | Target | Measured | Status |
| --- | --- | --- | --- |
| Bind group creations / frame | 0 | NOT MEASURED — reason: no device | ⚠️ |
| Buffer creations / frame | 0 | NOT MEASURED — reason: no device | ⚠️ |
| Pipeline cache hit rate | ≥ 95% | NOT MEASURED — reason: no device, no pipeline-cache file | ⚠️ |
| VRAM (textures + buffers) | ≤ 512 MB | NOT MEASURED — reason: no device; atlas path is RGBA8-only (SINKON-013) | ⚠️ |

### 7b. GLES 3.x

| Metric | Target | Measured | Status |
| --- | --- | --- | --- |
| Buffer creations / frame | 0 | NOT MEASURED — reason: no device | ⚠️ |
| Shader program compiles / frame | 0 | NOT MEASURED — reason: no device | ⚠️ |
| VRAM (textures + buffers) | ≤ 512 MB | NOT MEASURED — reason: no device | ⚠️ |

Design note: the GL backend has no on-disk pipeline cache (`wgpu` exposes none, GL
has no serialisable program cache). Programs are created once at init, never per
frame; the delta versus Vulkan is a longer first-frame cost, not a per-frame cost.
See SINKON-014.

### 7c. GLES 2.0 legacy

| Metric | Target | Measured | Status |
| --- | --- | --- | --- |
| `glBufferData` calls / frame (unchanged chunks) | 0 | NOT MEASURED — reason: no device | ⚠️ |
| Shader program compiles / frame | 0 | NOT MEASURED — reason: no device | ⚠️ |

## 8. Memory (both backends)

| Metric | Target | Measured (Vulkan) | Measured (GLES 3.x) | Measured (GLES 2.0) | Status |
| --- | --- | --- | --- | --- | --- |
| Resident (Tier S, RD 12) | ≤ 250 MB | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: n/a (Tier S has no legacy path) | ⚠️ |
| Resident (Tier B, RD 8) | ≤ 120 MB | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |
| Chunk buffer growth bounded (LRU) | bounded | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |

Design note: the LRU chunk pool is hard-capped (`MeshPool::new(capacity,
byte_capacity)`), evictions are counted in `MeshPoolStats::evictions`, and the
resident count is exposed as `meshes.resident` in the capability report.

## 9. Startup Latency (per backend)

| Backend | Metric | Target | Measured | Status |
| --- | --- | --- | --- | --- |
| Vulkan | Init → first frame (Tier S) | ≤ 400 ms | NOT MEASURED — reason: no device | ⚠️ |
| Vulkan | Init → first frame (Tier B) | ≤ 900 ms | NOT MEASURED — reason: no device | ⚠️ |
| GLES 3.x | Init → first frame (Tier S) | ≤ 400 ms | NOT MEASURED — reason: no device | ⚠️ |
| GLES 2.0 | Init → first frame (Tier B) | ≤ 900 ms | NOT MEASURED — reason: no device | ⚠️ |
| (all) | Backend selection time | ≤ 100 ms | NOT MEASURED — reason: no device | ⚠️ |

## 10. Version & Shader Coverage vs Perf (per backend)

Cross-reference `docs/VERSION_MATRIX.md` and `docs/SHADER_MATRIX.md`. No FPS @ RD
number can be recorded yet, because neither the version matrix nor the shader matrix
has been run on hardware.

| MC Version | Shaderpack | Backend | FPS @ RD | Status |
| --- | --- | --- | --- | --- |
| NOT MEASURED — reason: no device | NOT MEASURED — reason: shaderpack pass graph not executed (SINKON-004) | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | ⚠️ |

## 11. Regression Gate

`scripts/perf-gate.sh` compares `cargo criterion --message-format=json` output
against `benches/baseline.json` (per-backend keys `{ "vulkan_<bench>": { "mean_ns":
… }, … }`), writes a Markdown table to `benches/perf-gate-report.md` and exits `0`
(no FAIL), `1` (any regression above `THRESHOLD`, default 5 %) or `2` (tooling
error). CI policy: `.github/workflows/perf-gate-ci.yml` runs the gate on every push
that touches `src/`, `benches/` or `scripts/`, uploads the report as an artifact and
fails the job on exit `1`/`2`.

Current gate output (this is the *actual* output — the baseline is empty, so the
gate soft-passes by contract):

```
[sinkon-perf-gate] No baseline found at benches/baseline.json — run with --accept-baseline to create one.
```

Recorded in this form because no criterion run is possible here; see SINKON-009.

## 12. Known Bottlenecks & Mitigations (per backend)

| Backend | Unmet budget | Evidence | Mitigation |
| --- | --- | --- | --- |
| Vulkan | All budgets unmeasured | SINKON-001 | Run §13 on a reference device; the histogram is already exposed through `nativeGetPerfStats` |
| Vulkan | Occlusion culling (`supports.occlusion_query = false`) | SINKON-010 | Visibility graph + CPU frustum today; revisit when wgpu exposes VK queries or via `wgpu-hal` |
| Vulkan | Atlas is RGBA8-only, above the VRAM model | SINKON-013 | Add `PixelFormat::{Astc,Bc}` + `nativeUploadCompressedTexture` |
| GLES 3.x | No pipeline cache, no indirect draws, no GPU culling | SINKON-014, `docs/BACKEND_MATRIX.md` | CPU batch building (already allocation-free) and one program per format |
| GLES 3.x | Compute-dependent packs cannot run below ES 3.1 | `docs/SHADER_MATRIX.md` | Report `Unsupported` per pass instead of failing the pack |
| GLES 2.0 legacy | Fixed-function-era features absent | SINKON-011 | Delegated to the launcher's own GL wrappers (GL4ES/NG-GL4ES); Sinkon only covers its own renderer |

Vulkan-only features absent on GL: render bundles, push constants, indirect draws,
pipeline-cache persistence, storage buffers on ES 3.0, async compute.
GL-only behaviour absent on Vulkan: nothing Sinkon relies on; the legacy path is
*more* restricted, never more capable.

## 13. Reproduction

```sh
# 1. Build the native library for all three ABIs (needs NDK r26 + cargo-ndk)
./scripts/build-android.sh

# 2. Build the Kotlin module / the fork
cd android && ./gradlew :sinkon-native:assembleDebug        # module only
# or, inside a MojoLauncher fork with patch 0001 applied:
./gradlew assembleDebug

# 3. Install and select a backend with the launcher's renderer picker:
#    Sinkon (Vulkan) / Sinkon (OpenGL) / Sinkon (Auto)

# 4. Capture the histogram + JSON reports
adb logcat -s Sinkon | tee sinkon.logcat
#    (or read SinkonSession.capabilityReport() / .perfStats() from the debug overlay)

# 5. CPU side micro-benchmarks and the alloc gate (host, no device needed)
cargo bench --bench sinkon_benches -- vulkan_
cargo bench --bench sinkon_benches -- gles3_
cargo bench --bench sinkon_benches -- gles2_
cargo test --features alloc-gate

# 6. Regression gate against the accepted baseline
./scripts/perf-gate.sh --strict --backends=vulkan,gles3,gles2
./scripts/perf-gate.sh --accept-baseline --backends=vulkan,gles3,gles2   # record a new one
```

Each command maps to one table above: step 4 → §4/§6/§7/§9, step 5 → §5 and the
`criterion` medians used by §11, step 6 → §11.
