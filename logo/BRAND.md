# Sinkon brand sheet

The Sinkon mark is a **glyph that reads as the letter S and as a GPU pipeline
arrow**. Two interleaved strokes stand for the two render paths (Vulkan and
OpenGL ES) meeting in one pipeline: the lower violet stroke is the draw stream, the
upper cyan stroke is the completion/return of the same pipeline, and the arrowhead
is where both exit.

Everything here is vector, drawn from scratch in this repository. No third-party
marks, no system font wordmark, no `<image>`, no `<foreignObject>`, no base64
payloads — every SVG is plain paths and shapes and validates as XML.

## Palette

| Token | Hex | Use |
| --- | --- | --- |
| Cyan (primary) | `#0EA5E9` | Vulkan path, primary stroke, "Sinkon" accent |
| Violet (secondary) | `#7C3AED` | OpenGL path, secondary stroke |
| Background | `#0B0F19` | app background, adaptive-icon background layer |
| Accent | `#F1F5F9` | arrowhead, highlights, monochrome foreground |
| Border | `#1E293B` | 1–2 px outline on badges, dividers |
| Glow | `#16203A` | underlay strokes that keep the glyph readable on dark surfaces |

Cyan and violet are used at equal weight: neither backend is "the" Sinkon colour.

## Geometry

Drawn on a 1024 × 1024 grid, stroke width **92**, underlay stroke width **108** in
the background colour, round caps and joins:

| Element | Path |
| --- | --- |
| Lower stroke (violet) | `M560 482 H294 A146 146 0 0 0 294 774 H616` |
| Upper stroke (cyan) | `M150 182 H606 A146 146 0 0 1 606 482 H500` |
| Arrowhead (accent) | `M612 702 L842 774 L612 846 Z` |

The two horizontals of the S are the two rails of the pipeline; the two arcs are
where a rail hands off to the next stage; the arrowhead closes the glyph on the
right so the mark reads as motion, not as a static letter.

## Files

| File | Purpose |
| --- | --- |
| `sinkon.svg` | primary mark, 1024² |
| `sinkon-monochrome.svg` | single-colour (accent) version for tinted surfaces |
| `sinkon-monochrome-inverse.svg` | for light surfaces |
| `sinkon-wordmark.svg` | mark + "SINKON" (custom geometry, not a system font) |
| `sinkon-icon-background.svg` | adaptive-icon background layer (solid `#0B0F19` + glow) |
| `sinkon-icon-foreground.svg` | adaptive-icon foreground layer (safe-zone aware) |
| `sinkon-badge-vulkan.svg` | renderer badge, Vulkan |
| `sinkon-badge-gl.svg` | renderer badge, OpenGL |
| `sinkon-badge-auto.svg` | renderer badge, Auto (both strokes, split fill) |
| `png/*.png` | rendered set: mark @24/48/192/512/1024, monochrome @256, monochrome-inverse @256, wordmark, 3 badges @128 |
| `play-store/icon-512.png` | store listing icon |
| `render.sh` | regenerates every PNG; `--check` verifies the committed set |

## Usage rules

* **Clear space** — at least the width of one stroke (92 units at 1024) on all sides;
  never crop into the arc.
* **Minimum sizes** — 24 px for the mark (below that use the monochrome version),
  48 px for badges.
* **Backgrounds** — use the mark on `#0B0F19` or the monochrome variant on light
  surfaces. Never place the two-colour mark on a light background below 96 px.
* **Do not** recolour one stroke only, rotate the mark, add drop shadows, outline the
  glyph in a third colour, stretch it non-uniformly, or place text inside the
  stroke area.
* **Launcher badges** (`sinkon_badge_vulkan`, `_gl`, `_auto`) are rendered at
  128 px and wired into `RenderSpec.badge()`; they are the only place where a
  single backend colour leads.

## Regenerating

```sh
cd logo && ./render.sh          # re-render every PNG
cd logo && ./render.sh --check  # exit 1 if a committed PNG is missing/missized
```

Rendering uses `@resvg/resvg-js` (installed as `logo/node_modules`, git-ignored;
`npm i @resvg/resvg-js` in `logo/` restores it); the SVG sources are the only
hand-edited artifacts.
