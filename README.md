# Sinkon

**A dual-backend renderer for Minecraft: Java Edition on Android.**
One `libsinkon.so` carries a **Vulkan** path (`wgpu`), an **OpenGL ES 3.x** path
(`wgpu` GL backend) and a **GLES 2.0/2.1 legacy** path (`glow`), behind a single
backend abstraction, and plugs into a MojoLauncher fork as three selectable
renderers:

| Renderer in the launcher UI | Kotlin spec | `backendHint` |
| --- | --- | --- |
| **Sinkon (Vulkan)** | `SinkonVulkanRendererSpec` | `1` |
| **Sinkon (OpenGL)** | `SinkonGLRendererSpec` | `2` |
| **Sinkon (Auto)** | `SinkonAutoRendererSpec` | `0` |

`Sinkon (Auto)` walks **Vulkan → GLES 3.1/3.0 → GLES 2.0 legacy → hard fail**,
logging every rejection and exposing the whole decision in
`nativeGetCapabilityReport`. There is no silent fallback.

<p align="center">
  <img src="logo/png/sinkon-512.png" width="160" alt="Sinkon" />
</p>

---

## ⚠️ Build status

This tree was produced in a sandbox **without a Rust toolchain, without an NDK,
without Gradle and without a GPU**, so:

* the Rust sources have been syntax-checked but never compiled;
* the Kotlin module and the launcher patch have never been compiled;
* `./gradlew assembleDebug` has **not** been run;
* no performance number has been measured — every measured cell in
  `docs/PERFORMANCE.md` reads `NOT MEASURED — reason: …`.

Nothing is faked to hide that: `SINKON_STATUS.md` summarises the state per backend
and `docs/KNOWN_ISSUES.md` lists every gap with the
reason and the work needed to close it, and `docs/BACKEND_MATRIX.md`,
`docs/SHADER_MATRIX.md`, `docs/VERSION_MATRIX.md` mark every single cell as
*verified* / *best-effort* / *known-broken* accordingly.

## What is implemented

| Area | State |
| --- | --- |
| Backend trait + Vulkan / GLES3 / GLES2-legacy implementations | written (`src/backend/*`) |
| Backend negotiation with recorded attempts, forced hints, hard fail | written (`backend::select`, `selection_plan`) |
| Chunk meshing worker pool, lock-free return queue, LRU pool, frustum + visibility culling | written, unit-tested (`src/chunk.rs`) |
| Naga shader pipeline: GLSL → SPIR-V / GLSL ES 3.x / GLSL ES 1.00 | written (`src/shader/*`) |
| OptiFine + Iris pack parsing, `#include`, properties, uniforms, per-dimension passes | written, unit-tested (`shader::optifine`, `shader::iris`) |
| Hot reload (`nativeReloadShaders`, mtime poll) | written |
| Renderer: per-frame path, pooled uploads, live backend switch, capability/perf JSON | written (`src/renderer.rs`) |
| JNI bridge, 12 exports, direct-buffer zero-copy, no per-frame JNI objects | written (`src/jni_bridge.rs`) |
| Perf: tiers S/A/B/C, budgets per backend, histograms, counting allocator, tracing/Tracy | written (`src/perf/*`) |
| Criterion benches, per-backend ids, regression gate | written + gate harness exercised |
| Android module + three `RenderSpec`s + MojoLauncher patch | written, not compiled |
| Logo set (SVG + 31 PNGs + adaptive icon + 3 badges) | done and rendered |

Two deliberate scope limits are spelled out in `docs/KNOWN_ISSUES.md`: the
shaderpack pass graph is parsed/translated/validated but not yet executed at draw
time (SINKON-004), and the launcher needs a world-data hook to feed
`nativeBakeSection` (SINKON-006).

## Repository map

```
src/                Rust crate (cdylib + rlib)
  backend/          GraphicsBackend trait, wgpu_shared, vulkan, gles3, gles2_legacy
  shader/           naga frontend, spv+glsl emitters, optifine/iris parsing, builtins
  perf/             tier table + budgets, histograms, allocation counter, tracing
  chunk.rs          section meshing, worker pool, LRU pool, culling
  renderer.rs       SinkonRenderer: frame path, reports, backend switch
  jni_bridge.rs     the 12 `Java_org_sinkon_launcher_Sinkon_*` exports
android/            Gradle module `:sinkon-native` (Kotlin JNI bindings, probes, session)
launcher-fork/      MojoLauncher patch 0001 + the Kotlin renderer specs
benches/            criterion benches + pinned baseline schema
scripts/            build-android.sh, perf-gate.sh, perf-gate-ci.yml
docs/               ARCHITECTURE, JNI, PERFORMANCE, BACKEND/SHADER/VERSION matrices, KNOWN_ISSUES
logo/               source SVGs, render.sh, PNG set, store icon
```

## Get an APK

**No APK is committed to this repository** — nothing here was compiled (see the
status box above: no JDK, Gradle, NDK or Rust toolchain in the build sandbox). Two
ways to get one, both from this tree:

1. **CI (recommended).** Push this branch; `.github/workflows/build-apk.yml` builds
   `libsinkon.so` for all three ABIs, assembles `:sample:assembleDebug` and uploads
   **`sinkon-sample-apk`** as a workflow artifact — that artifact is the download
   link. Pushing a `v*` tag additionally attaches the APK to a GitHub Release, which
   gives a stable public URL.
2. **Locally** (needs JDK 17, Android SDK, NDK r26+, Rust + `cargo-ndk`):

   ```sh
   ./scripts/build-android.sh
   cd android && gradle :sample:assembleDebug
   adb install -r sample/build/outputs/apk/debug/sample-debug.apk
   adb shell am start -n org.sinkon.sample/.MainActivity --ei backend 1   # 0 auto, 1 Vulkan, 2 GLES3, 3 GLES2
   ```

`android/sample` is a **debug harness**, not the launcher: it pushes synthetic
sections and shows the capability/perf JSON on screen (SINKON-017). The real APK is
the MojoLauncher fork build in `launcher-fork/`.

## Build

### Native library (all three ABIs)

```sh
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk
export ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/26.1.10909125
./scripts/build-android.sh               # → android/app/src/main/jniLibs/<abi>/libsinkon.so
```

### Kotlin module

```sh
cd android
gradle wrapper --gradle-version 8.9     # once, if ./gradlew is missing
./gradlew :sinkon-native:assembleDebug
```

### Into a MojoLauncher fork

```sh
git clone https://github.com/MojoLauncher/MojoLauncher.git && cd MojoLauncher
git checkout v3_openjdk
git apply /path/to/Sinkon/launcher-fork/patches/0001-register-sinkon-renderers.patch
cp -r /path/to/Sinkon/android/app ./sinkon-native
cp /path/to/Sinkon/launcher-fork/app_mojo/src/main/java/.../impl/Sinkon*.kt \
   app_mojo/src/main/java/git/artdeell/mojo/game/renderer/impl/
cp /path/to/Sinkon/launcher-fork/app_mojo/src/main/res/values/strings_sinkon.xml \
   app_mojo/src/main/res/values/
./gradlew assembleDebug
```

The patch adds `RenderSpec.backendHint()`/`badge()`, the three tags in
`Renderers`, the three cases in `GameRenderer.getKnownRenderer`, the three entries
in `RendererCache`, and three one-line surface hooks in `GameView`. Details and the
known integration risks (surface ownership, world-data hook) are in
`launcher-fork/README.md` and `docs/KNOWN_ISSUES.md`.

## Performance harness

```sh
cargo bench --bench sinkon_benches               # per-backend criterion groups
cargo test --features alloc-gate                 # fails if the frame path allocates
./scripts/perf-gate.sh --strict                  # per-backend regression gate
./scripts/perf-gate.sh --accept-baseline          # record a new baseline
```

Bench ids are prefixed `vulkan_`, `gles3_`, `gles2_`; `benches/baseline.json` is
`{ "<backend>_<bench>": { "mean_ns": <u64> } }`. CI runs the gate and uploads the
report (`.github/workflows/perf-gate-ci.yml`).

## Documentation

| File | Contents |
| --- | --- |
| `docs/ARCHITECTURE.md` | module map, trait deviation, frame path, threading, per-backend `unsafe` invariants |
| `docs/JNI.md` | all 12 exports + capability/perf JSON schemas |
| `docs/PERFORMANCE.md` | the fixed performance template, all cells honestly `NOT MEASURED` for now |
| `SINKON_PERF_REPORT.md` | condensed perf report + what *is* verified (the gate harness) |
| `docs/BACKEND_MATRIX.md` | feature × backend support, legacy-path limits |
| `docs/SHADER_MATRIX.md` | BSL / Complementary (Reimagined + Unbound) / SEUS PTGI / Sildur's / Chocapic13 / MakeUp / Rethinking Voxels |
| `docs/VERSION_MATRIX.md` | Alpha → 1.21 + snapshots × backend |
| `docs/KNOWN_ISSUES.md` | 16 numbered issues with impact, reason and close condition |
| `KNOWN_ISSUES.md` | flat index of the same 16 issues, for readers of the repository root |
| `SINKON_STATUS.md` | built / works / best-effort / known-broken per backend, one page |
| `logo/BRAND.md` | palette, glyph geometry, clear-space rules, file inventory |

## Logo

`logo/sinkon.svg` is the source of truth: an **S** whose two interleaved strokes are
also the two rails of a GPU **pipeline arrow** (cyan `#0EA5E9` = Vulkan path, violet
`#7C3AED` = OpenGL path, background `#0B0F19`, accent `#F1F5F9`). No text, no
`<image>`, no `<foreignObject>`, no base64. `./logo/render.sh` regenerates every PNG
(`--check` verifies the committed set is up to date); adaptive icon pieces and the
three renderer badges are wired into the launcher resources.

## Licence

LGPL-3.0-only — see `LICENSE`. No Mojang code or assets are included or
redistributed; Sinkon is an independent renderer and does not implement
authentication of any kind.
