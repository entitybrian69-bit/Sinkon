# Known issues — index

The full entries (impact, reason, how to close, evidence) live in
[`docs/KNOWN_ISSUES.md`](docs/KNOWN_ISSUES.md). This file is the flat index so a
reader of the repository root sees every gap without opening the docs tree.

Nothing in this repository is a bare `TODO`: if it is unfinished, it is listed here.

| ID | Severity | Summary | Where |
| --- | --- | --- | --- |
| SINKON-001 | blocker | Nothing measured: no device, GPU, Rust toolchain or NDK in the build sandbox; all perf cells are `NOT MEASURED` | `docs/PERFORMANCE.md`, `SINKON_PERF_REPORT.md` |
| SINKON-002 | blocker | `cargo check/clippy/build` never ran; Rust sources are only syntax-checked | `docs/KNOWN_ISSUES.md` §002 |
| SINKON-003 | major | Kotlin sources never compiled (`kotlinc`/Gradle unavailable) | `android/`, `launcher-fork/` |
| SINKON-004 | major | Shaderpack pass graph is parsed/translated/validated but not executed at draw time | `src/renderer.rs`, `docs/SHADER_MATRIX.md` |
| SINKON-005 | blocker (fork) | Sinkon and the launcher can both present to the same `ANativeWindow` | `launcher-fork/` |
| SINKON-006 | blocker (fork) | No upstream hook feeds `nativeBakeSection`; a fresh fork renders an empty world | `launcher-fork/`, `SinkonSession.pushSection` |
| SINKON-007 | minor | `SinkonSurfaceBridge.minecraftVersion` is a default (`1.20.4`), the fork must set it | `launcher-fork/` |
| SINKON-008 | minor | Renderer badges exist (`RenderSpec.badge()`) but the stock settings list does not show them | `launcher-fork/` |
| SINKON-009 | minor | No recorded perf baseline (`benches/baseline.json` is `{}`), gate soft-passes | `benches/`, `scripts/perf-gate.sh` |
| SINKON-010 | major | No occlusion queries on any backend (wgpu 22); visibility graph + CPU fallback instead | `src/backend/mod.rs` |
| SINKON-011 | by design | GLES 2.0 legacy cannot run shaderpacks or 1.7+ worlds (no UBO/SSBO/MRT/`textureLod`, no `glSelect`/feedback/clip planes/stipple/`glDrawPixels`) | `docs/BACKEND_MATRIX.md`, `docs/SHADER_MATRIX.md` |
| SINKON-012 | minor | Async compute queue is best-effort; shadow work shares the graphics queue when no second family exists | `src/backend/wgpu_shared.rs` |
| SINKON-013 | major | Texture atlas path is RGBA8-only; ASTC/BC reported but unused → higher VRAM/bandwidth | `src/renderer.rs` |
| SINKON-014 | minor | GLES 3.x has no pipeline-cache persistence (platform limitation), logged once | `docs/BACKEND_MATRIX.md` |
| SINKON-015 | by design | Android only, no iOS; no Mojang assets, no auth code | repository-wide |
| SINKON-016 | major | Chunk pool slot sizes (96 KiB vertex / 48 KiB index) are a design estimate; oversized sections are skipped and counted | `src/renderer.rs`, `docs/KNOWN_ISSUES.md` §016 |

Unimplemented combinations are additionally tracked per cell in
[`docs/BACKEND_MATRIX.md`](docs/BACKEND_MATRIX.md),
[`docs/SHADER_MATRIX.md`](docs/SHADER_MATRIX.md) and
[`docs/VERSION_MATRIX.md`](docs/VERSION_MATRIX.md).
