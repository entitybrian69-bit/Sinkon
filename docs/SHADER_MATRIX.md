# Shader matrix

Sinkon's shader pipeline: **GLSL source → Naga frontend → validation → per-backend
emitter**.

| Target | Emitter | Used by |
| --- | --- | --- |
| SPIR-V 1.0 | `shader::emit_spirv` (`naga::back::spv`) | `wgpu` Vulkan **and** `wgpu` GL |
| GLSL ES 3.10 / 3.00 | `shader::emit_glsl` (`naga::back::glsl`) | inspection, ES 3.x tooling |
| GLSL ES 1.00 | `shader::emit_glsl::downlevel_to_es100` | `glow` legacy path only |

`glow` never receives SPIR-V: on the legacy path the ES 1.00 emitter is the only
consumer of a GLSL string.

## Status legend

| Mark | Meaning |
| --- | --- |
| ✅ parse + translate + validate | Passes are discovered, translated to the backend's target and validated by Naga |
| ⚠️ passes not executed | Translation works, but the deferred/composite pass graph is not yet driven by `render_frame` (SINKON-004) |
| ❌ unsupported | A required construct cannot be expressed on that backend; the reason is logged per pass and listed in `nativeGetCapabilityReport.shader_errors` |

No shaderpack has been run on a device or a desktop GPU from this sandbox. Every
column therefore carries the ⚠️ qualification from `docs/KNOWN_ISSUES.md`
(SINKON-001). What *is* verified here is the parser and the emitter contract:
`src/shader/optifine.rs`, `src/shader/iris.rs` and `src/shader/emit_glsl.rs` have
unit tests for pass classification, include resolution, uniform collection and the
ES 1.00 rejection list.

## Packs

| shaderpack | MC version | Vulkan | GLES 3.x | GLES 2.0 legacy | notes |
| --- | --- | --- | --- | --- | --- |
| **BSL** | 1.16–1.21 | ✅ ⚠️ | ✅ ⚠️ | ❌ unsupported | Classic OptiFine layout (`gbuffers_terrain`, `deferred*`, `composite*`, `final`). Uses MRT + UBOs → the ES 1.00 downleveller rejects `layout(location`/`gl_FragData`. |
| **Complementary Reimagined** | 1.17–1.21 | ✅ ⚠️ | ✅ ⚠️ | ❌ unsupported | Iris layout with `iris_*` uniforms and per-dimension overrides; needs SSBOs on the shadow pass (Vulkan/ES 3.1 only). |
| **Complementary Unbound** | 1.16–1.21 | ✅ ⚠️ | ✅ ⚠️ | ❌ unsupported | Same layout as Reimagined with a larger pass graph; `composite` chain up to 15 passes. |
| **SEUS PTGI** | 1.14–1.20 | ⚠️ best-effort ⚠️ | ❌ unsupported | ❌ unsupported | Ray-traced GI: needs compute + SSBO + image load/store. Vulkan only in principle; on GLES 3.0 there is no compute, so the pack cannot be translated. |
| **Sildur's Vibrant** | 1.7–1.21 | ✅ ⚠️ | ✅ ⚠️ | ❌ unsupported | Transition-era pack; `gbuffers_*` + `composite` only, no compute. Best candidate for the first end-to-end pack bring-up. |
| **Chocapic13** | 1.7–1.20 | ✅ ⚠️ | ✅ ⚠️ | ❌ unsupported | Heavy `deferred`/`composite` usage; `shaders.properties` drives pass toggles (parsed by `optifine::parse_properties`). |
| **MakeUp Ultra Fast** | 1.8–1.21 | ✅ ⚠️ | ✅ ⚠️ | ❌ unsupported | Smallest pass graph of the set (2–3 composite passes); the recommended smoke test before BSL. |
| **Rethinking Voxels** | 1.17–1.21 | ⚠️ best-effort ⚠️ | ❌ unsupported | ❌ unsupported | Iris-only, compute-heavy voxel GI (`*.csh` detected by `iris::IrisPass::compute`); requires compute + SSBO, so GLES 3.0/3.1 without compute is out. |

## Feature → backend

| Shader feature | Vulkan | GLES 3.0 | GLES 3.1 | GLES 2.0 legacy |
| --- | --- | --- | --- | --- |
| `#version 120` … `460` input | ✅ Naga GLSL frontend | ✅ | ✅ | ✅ (parsed, then downlevelled) |
| Uniform blocks (`layout(std140)`) | ✅ | ✅ | ✅ | ❌ `ES100_BLOCKERS` |
| SSBOs (`layout(std430)`) | ✅ | ❌ | ✅ | ❌ |
| `layout(location = n)` | ✅ | ✅ | ✅ | ❌ |
| `textureLod` / `textureGrad` | ✅ | ✅ | ✅ | ❌ (no LOD-bias sampling in ES 1.00) |
| MRT (`gl_FragData[]`, multiple `out`) | ✅ | ✅ | ✅ | ❌ |
| Integer types / integer samplers | ✅ | ✅ | ✅ | ❌ |
| `flat` interpolation | ✅ | ✅ | ✅ | ❌ |
| Compute stages (`*.csh`) | ✅ | ❌ | ⚠️ driver dependent | ❌ |
| `#include` chains | ✅ depth cap 8 | ✅ | ✅ | ✅ |
| OptiFine `shaders.properties` | ✅ parsed | ✅ | ✅ | ✅ (ignored where it toggles unsupported passes) |
| Iris per-dimension overrides | ✅ | ✅ | ✅ | ✅ (parsed, vanilla fallback) |
| Hot reload (`nativeReloadShaders`) | ✅ | ✅ | ✅ | ✅ |

## OptiFine / Iris parsing coverage

| Item | Where | Status |
| --- | --- | --- |
| `shaders/` root detection (`shaders.properties` / `gbuffers_*`) | `optifine::ShaderpackLayout::parse` | ✅ unit-tested |
| Pass ordering (Shadow → Gbuffers → Deferred → Composite → Final) | `optifine::PassKind::order` | ✅ unit-tested |
| `#include` resolution with depth cap 8 + missing-file reporting | `optifine::resolve_includes` | ✅ unit-tested |
| Uniform collection (`collect_uniforms`) | `optifine::collect_uniforms` | ✅ unit-tested |
| `shaders.properties` parser | `optifine::parse_properties` | ✅ unit-tested |
| Iris `iris_*` uniform vocabulary (~38 entries) | `iris::IRIS_UNIFORMS` | ✅ unit-tested |
| Iris per-dimension folders (`world-1`, `world_nether`, …) | `iris::IrisLayout::passes_for_dimension` | ✅ unit-tested |
| Iris compute passes (`*.csh`) | `iris::IrisPass::compute` | ✅ detected, ❌ not executed |
| Custom uniforms added by packs | reported through `unsupported_uniforms` | ✅ |
