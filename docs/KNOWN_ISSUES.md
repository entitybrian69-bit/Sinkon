# Known issues

Every limitation that is visible in the code, the docs or the launcher patch has an
entry here. Nothing in this repository is a bare `TODO`: if it is not finished, it
is listed below with the reason and what would close it.

Severity: **blocker** (cannot ship), **major** (ships with a degraded experience),
**minor** (cosmetic or tooling).

---

## SINKON-001 — No on-device or on-host measurement (blocker for the perf claims)

**What:** every measured cell in `docs/PERFORMANCE.md` and
`SINKON_PERF_REPORT.md` reads `NOT MEASURED`.

**Why:** the sandbox that produced this repository has no Android device, no GPU, no
Rust toolchain (crates.io and `static.rust-lang.org` are unreachable from it) and no
NDK. `cargo`, `rustc`, `cargo-criterion`, `java` and `gradle` cannot be installed.

**Consequence:** the performance budgets in the brief (120 FPS Tier S on Vulkan,
≤ 50 µs JNI, ≤ 2.0 ms submission, 0 allocations/frame, ≤ 400 ms init) are
*design targets*, not results. The gate has no recorded baseline
(`benches/baseline.json` is `{}`), so `scripts/perf-gate.sh` soft-passes on the
first run by design.

**Closes with:** one run of the reproduction commands in `docs/PERFORMANCE.md` §13 on
a reference device, followed by `scripts/perf-gate.sh --accept-baseline`.

---

## SINKON-002 — `cargo build` / `./gradlew assembleDebug` not executed (blocker)

**What:** no compile of the Rust crate, the Kotlin module or the MojoLauncher fork
has been run. Rust sources were checked with a parser-based syntax check
(`/tmp/tools/rscheck.mjs`, a `jinx-rust` parse) which catches syntax errors only —
not type errors, borrow errors or missing symbols.

**Why:** no toolchain in the sandbox (see SINKON-001).

**Consequence:** treat the first `cargo check` on a real machine as a required
review step. The most likely failure classes are wgpu 22 API drift and Naga option
fields; the code was written against the wgpu 22.1.0 / naga 22.1.0 sources.

**Also not run:** `shellcheck` (same missing-tooling reason). `scripts/perf-gate.sh`
is written to POSIX sh rules (quoted expansions, `[ ]` tests, no bash arrays) and
was exercised end-to-end with a stub `cargo`, but it has not been linted by
`shellcheck`.

**Closes with:** `cargo check --all-targets`, `cargo test`,
`cargo clippy --all-targets -- -D warnings`, `cargo fmt --all`,
`shellcheck scripts/*.sh`, then `./gradlew assembleDebug` in the fork.

---

## SINKON-003 — Kotlin sources are not compiled either (major)

**What:** `android/app/src/main/java/org/sinkon/launcher/*.kt` and
`launcher-fork/**/*.kt` have never been through `kotlinc`.

**Why:** no JDK/Gradle in the sandbox.

**Closes with:** the Gradle build in SINKON-002; `SinkonSession`/`SinkonRenderLoop`
are self-contained and should compile without launcher classes, which isolates a
failure to the fork-only files.

---

## SINKON-004 — Shaderpack pass graph is not driven at draw time (major)

**What:** Sinkon parses OptiFine/Iris packs, translates every pass for the active
backend, validates it, watches the directory for hot reload and reports per-pass
errors. It does **not** yet execute the deferred/composite chain during
`render_frame`: the frame draws the built-in terrain pipeline only.

**Why:** executing the chain needs per-pass framebuffer sets, shadow maps with
per-dimension resolutions, and pack-provided uniform buffers — a render-graph
feature that is out of scope for the first end-to-end bring-up, and it cannot be
validated without a device (SINKON-001).

**Consequence:** selecting a shaderpack changes diagnostics (`shader_errors`,
`shaders` count in the capability report) but not the visible image.

**Closes with:** a `RenderGraph` stage table built from
`shader::optifine::ShaderpackLayout::passes` / `shader::iris::IrisLayout::passes`,
one render target per `PassKind`, and shadow-map allocation per `IrisLayout`.

---

## SINKON-005 — Sinkon and the launcher both present to the same `ANativeWindow` (blocker for the fork)

**What:** the fork patch attaches Sinkon to the game surface
(`SinkonSurfaceBridge`) while MojoLauncher's own `Platform.updateSurface` still
hands the same surface to the launcher's EGL/GLFW path.

**Why:** replacing the launcher's present path end-to-end requires changes inside
`mojoexec`'s native code, which this patch series does not touch.

**Consequence:** on a device the two presenters can fight over the buffer queue; the
symptom is a flickering or black surface. Sinkon must own the window exclusively.

**Closes with:** routing `Platform.updateSurface` through Sinkon when a Sinkon
`RenderSpec` is active (returning before the launcher's EGL context is created), or
presenting Sinkon into an `AHardwareBuffer`/`SurfaceTexture` that the launcher
composites.

---

## SINKON-006 — No upstream hook pushes world data into Sinkon (blocker for the fork)

**What:** MojoLauncher has no hook where chunk sections are handed to a renderer, so
nothing calls `nativeBakeSection` in a running launcher.

**Why:** the Java-side renderer (LWJGL + Sodium of the launched MC instance) owns
meshing; Sinkon's `bakeSection` contract expects the launcher to forward its block
data.

**Closes with:** a `SinkonWorldBridge` on the Java side that observes the game's
chunk section build (Sodium's `ChunkBuilder` hook or a mixin) and forwards 4096-byte
block-id arrays plus the camera matrices. `SinkonSession.pushSection` is the
prepared entry point.

---

## SINKON-007 — `minecraftVersion` for the bridge is a default, not a query (minor)

**What:** `SinkonSurfaceBridge.minecraftVersion` defaults to `"1.20.4"`; the fork
must set it from its own launch path.

**Why:** MojoLauncher keeps the selected version in its launch arguments, not in a
readable static.

**Closes with:** setting the field where the launcher builds the JVM arguments.

---

## SINKON-008 — Renderer badges are not consumed by the launcher UI (minor)

**What:** patch 0001 adds `RenderSpec.badge()` and the three specs return
`R.drawable.sinkon_badge_{vulkan,gl,auto}`, but the stock settings list still shows
its generic renderer icon.

**Why:** the list renders names only (`RendererCache.rendererDisplayNames`); adding a
badge needs a custom adapter in the settings UI, which is outside the renderer
patch.

**Closes with:** a `RendererPreferenceAdapter` that resolves `badge()` per tag.

---

## SINKON-009 — Perf gate has no baseline (minor)

**What:** `benches/baseline.json` is `{}`.

**Why:** criterion has never run (SINKON-001). Fabricating numbers is explicitly
forbidden by the brief.

**Consequence:** `scripts/perf-gate.sh` prints
`No baseline found … run with --accept-baseline` and exits `0`; with `--strict` it
exits `1`. CI therefore runs the gate in non-strict mode until a baseline exists.

**Closes with:** `./scripts/perf-gate.sh --accept-baseline` on the reference machine.

---

## SINKON-010 — No occlusion queries on any backend (major)

**What:** `supports.occlusion_query` is `false` everywhere.

**Why:** wgpu 22 exposes no occlusion-query API, and GLES 2.0 has none at all.
Sinkon uses a chunk visibility graph (`SectionInstance.parent_visible`) plus a
conservative CPU frustum test instead.

**Consequence:** overdraw in dense caves is higher than a hardware-queried culler
would allow; this is the first thing to revisit when wgpu grows the API (or via a
`wgpu-hal` escape hatch on Vulkan).

---

## SINKON-011 — GLES 2.0 legacy cannot run shaderpacks or 1.7+ worlds (by design)

**What:** the `glow` path rejects UBOs, SSBOs, MRT, integer types, `textureLod`,
`textureGrad`, `flat`, `#extension` and bitfield intrinsics
(`emit_glsl::ES100_BLOCKERS`), and it does not implement `glSelect`, feedback mode,
user clip planes, polygon stipple or `glDrawPixels`.

**Why:** none of those exist in GLES 2.0; emulating them would be a GL4ES-class
project, which the brief explicitly leaves to the launcher's own GL wrappers.

**Consequence:** documented in `docs/BACKEND_MATRIX.md` and
`docs/SHADER_MATRIX.md`; the selector only picks this backend for the Legacy profile
or GLES-2.x-only devices, and every rejection is logged with the offending pass
name.

---

## SINKON-012 — Async compute queue is best-effort (minor)

**What:** `wgpu` exposes one queue per device through its public API. Sinkon
requests a second queue family only when it exists and the driver reports compute;
otherwise lighting/shadow work stays on the graphics queue.

**Consequence:** on devices without a dedicated compute queue the shadow pass
serialises with the main pass. Recorded as a design limit, not a bug.

---

## SINKON-013 — Texture atlas path is RGBA8-only (major)

**What:** `nativeUploadTexture` and `upload_texture` create `PixelFormat::Rgba8Unorm`
textures; ASTC/BC support is reported in capabilities but not used for the atlas.

**Why:** compressed upload needs transcoding on the launcher side plus per-format
row-pitch handling, and it cannot be validated without a device.

**Consequence:** VRAM and bandwidth are higher than the 512 MB atlas budget assumes
on devices that would otherwise use ASTC.

**Closes with:** an `atlas_format` field in `nativeUploadTexture` (or a separate
`nativeUploadCompressedTexture`), `PixelFormat::{Astc4x4, Bc1, …}` in the backend
trait, and a fallback chain ASTC → BC → RGBA8.

---

## SINKON-014 — GLES 3.x has no pipeline-cache persistence (minor, by platform)

**What:** `supports.pipeline_cache` is `false` on the GL backend and a `warn` line is
logged once.

**Why:** GL has no serialisable pipeline cache; `glProgramBinary` is driver- and
extension-dependent and wgpu does not expose it.

**Consequence:** shader-program compilation happens on first use after every launch
(never per frame — the programs are created once and cached in the 16-entry
legacy/backend table).

---

## SINKON-015 — No iOS target, no Mojang assets (by design)

Sinkon is Android-only (`Android 8.0+`, `aarch64` primary, `armv7`/`x86_64`
secondary). It ships no Minecraft assets, no Mojang trademarks, no authentication
code and no MS-account bypass; the launcher continues to own all of that.

---

## SINKON-016 — Chunk pool sizing is a design estimate, not a measurement (major)

**What:** each pooled slot owns a fixed 96 KiB vertex buffer + 48 KiB index buffer,
and the pool holds 512 / 384 / 256 / 128 slots for Tier S / A / B / C
(`renderer::SLOT_VERTEX_BYTES`, `renderer::pool_bytes`, `renderer::pool_sizing`).
That pins 72 / 54 / 36 / 18 MB of GPU memory for chunk geometry.

**Why it matters:** a 16³ section that meshes to more than 96 KiB of vertices (very
noisy terrain, glass/leaf-heavy sections) cannot be uploaded into its slot. Sinkon
does not clip it: the section is skipped, `dropped_sections` is incremented and a
`warn` line names the section and its size.

**Why it is estimated:** no device was available to measure the real section-size
distribution, and a render distance of 12 needs far more sections resident than 512
slots hold — the pool is a cache with LRU eviction, not a guarantee.

**Closes with:** a size histogram from a real world (log `result.vertices.len()` in
`ChunkManager::drain`), then either per-slot growth (create a larger buffer on
demand, counting the extra `create_buffer` outside the steady-state budget) or a
size-class pool (small/medium/large slots).
