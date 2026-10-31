# Version matrix

`mcVersion` is classified once in `nativeInit`
(`src/version_profile.rs::classify`) and drives both the shader dialect and the
backend preference order.

| Profile | Range | Shader dialect produced | Auto order |
| --- | --- | --- | --- |
| `Legacy` | Alpha – 1.6 | GLSL 1.20 → ES 1.00 | `gles2_legacy` → `gles3` → `vulkan` |
| `Transition` | 1.7 – 1.12 | GLSL 1.20/1.50 → SPIR-V / ES 3.x | `vulkan` → `gles3` |
| `Modern` | 1.13 – 1.16 | GLSL 3.30 core → SPIR-V / ES 3.x | `vulkan` → `gles3` |
| `Current` | 1.17 – latest + snapshots | GLSL 3.30+ core, Iris → SPIR-V / ES 3.1 | `vulkan` → `gles3` |

Snapshot strings (`24w14a`), pre-releases (`1.21.1-rc1`) and Alpha/Beta markers
(`a1.2.6`, `beta 1.7.3`) are handled by the same parser; an unparseable string
falls back to `Current` **and logs a warning**.

## Coverage

Legend: **verified** = reproduced on hardware; **best-effort** = implemented and
unit-tested, never executed on a device; **known-broken** = will not work, with the
reason in the last column.

| MC version | Vulkan | GLES 3.x | GLES 2.0 legacy | Reasons |
| --- | --- | --- | --- | --- |
| Alpha `a1.2.6` | best-effort | best-effort | best-effort | Fixed-function expectations; legacy path emulates them with generated ES 1.00 programs |
| Beta `b1.7.3` | best-effort | best-effort | best-effort | Same as Alpha; no shaders |
| 1.0 – 1.5.2 | best-effort | best-effort | best-effort | Pre-VBO world rendering |
| 1.6.4 | best-effort | best-effort | best-effort | Last Legacy-profile release |
| 1.7.10 | best-effort | best-effort | known-broken | Needs GL 2.1 VBO semantics that ES 1.00 emulation cannot express for the whole frame |
| 1.8 – 1.12.2 | best-effort | best-effort | known-broken | VBO-era GL 2.1/3.0; OptiFine shaderpacks appear here |
| 1.13 – 1.16.5 | best-effort | best-effort | known-broken | GL 3.2 core, chunk meshing rewrite, Sodium-compatible formats |
| 1.17 – 1.20.1 | best-effort | best-effort | known-broken | GL 3.3 core, Iris |
| 1.20.2 – 1.21.x | best-effort | best-effort | known-broken | GL 3.3 core + Iris; snapshot strings handled |
| Snapshots (`24w14a`, …) | best-effort | best-effort | known-broken | Treated as `Current` |

**Nothing in this table is `verified`.** No Android device was available while this
repository was produced (SINKON-001). The table records intent, not measurement;
the verification run is the first item on the roadmap in
`docs/PERFORMANCE.md` §12.
