//! Built-in shaders.
//!
//! Sinkon renders with a small, self-contained shader pair until the launcher
//! hands over Minecraft's own core shaders. Keeping it in-tree means a fresh
//! install (or a launcher that has not yet extracted the game assets) still
//! produces visible geometry, and it gives the benchmarking harness something
//! deterministic to translate on every backend.
//!
//! The sources are Vulkan-flavoured GLSL 3.30: an explicit `set`/`binding`
//! camera block that matches the backend's uniform layout, plus
//! `layout(location)` vertex inputs matching Sinkon's packed chunk vertex
//! (`src/chunk.rs::push_face`):
//!
//! | location | attribute | offset | format |
//! | --- | --- | --- | --- |
//! | 0 | `a_pos` | 0 | `Float32x3` |
//! | 1 | `a_uv` | 12 | `Float32x2` |
//! | 2 | `a_color` | 20 | `Unorm8x4` (block id, face id, ao, light) — 24-byte layouts only |
//!
//! That is why there are two vertex/fragment pairs: the 20-byte `Packed4` layout
//! has no colour tail, and a pipeline must not declare an attribute the buffer
//! does not contain (wgpu rejects it at pipeline creation).

/// Vertex shader for 24-byte chunk terrain (`Packed8` / `Custom`).
pub const TERRAIN_VERTEX: &str = r#"#version 330 core

layout(set = 0, binding = 0) uniform SinkonCamera {
    mat4 u_mvp;
    mat4 u_proj;
    vec2 u_padding;
    vec2 u_padding2;
};

layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec2 a_uv;
layout(location = 2) in vec4 a_color;

out vec2 v_uv;
out vec4 v_color;

void main() {
    v_uv = a_uv;
    v_color = a_color;
    gl_Position = u_mvp * vec4(a_pos, 1.0);
}
"#;

/// Fragment shader for 24-byte chunk terrain.
pub const TERRAIN_FRAGMENT: &str = r#"#version 330 core

in vec2 v_uv;
in vec4 v_color;

layout(location = 0) out vec4 frag_color;

void main() {
    // Deterministic debug shading until the material/atlas system lands
    // (KNOWN_ISSUES SINKON-013): a UV gradient tinted by the packed vertex
    // colour, with the AO byte folded in.
    vec3 tint = mix(vec3(0.10, 0.55, 0.90), vec3(0.55, 0.25, 0.90), v_uv.y);
    float ao = mix(0.45, 1.0, v_color.z);
    frag_color = vec4(tint * (0.35 + 0.65 * ao) * (0.6 + 0.4 * v_color.x), 1.0);
}
"#;

/// Vertex shader for 20-byte chunk terrain (`Packed4`, no colour tail).
pub const TERRAIN_VERTEX_PACKED4: &str = r#"#version 330 core

layout(set = 0, binding = 0) uniform SinkonCamera {
    mat4 u_mvp;
    mat4 u_proj;
    vec2 u_padding;
    vec2 u_padding2;
};

layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec2 a_uv;

out vec2 v_uv;

void main() {
    v_uv = a_uv;
    gl_Position = u_mvp * vec4(a_pos, 1.0);
}
"#;

/// Fragment shader for 20-byte chunk terrain.
pub const TERRAIN_FRAGMENT_PACKED4: &str = r#"#version 330 core

in vec2 v_uv;

layout(location = 0) out vec4 frag_color;

void main() {
    vec3 tint = mix(vec3(0.10, 0.55, 0.90), vec3(0.55, 0.25, 0.90), v_uv.y);
    frag_color = vec4(tint, 1.0);
}
"#;

/// Debug overlay shader (unlit, used by the perf HUD).
pub const OVERLAY_VERTEX: &str = r#"#version 330 core

layout(set = 0, binding = 0) uniform SinkonCamera {
    mat4 u_mvp;
    mat4 u_proj;
    vec2 u_padding;
    vec2 u_padding2;
};

layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec2 a_uv;
out vec2 v_uv;

void main() {
    v_uv = a_uv;
    gl_Position = u_mvp * vec4(a_pos, 1.0);
}
"#;

/// Debug overlay fragment shader.
pub const OVERLAY_FRAGMENT: &str = r#"#version 330 core

in vec2 v_uv;
layout(location = 0) out vec4 frag_color;

void main() {
    frag_color = vec4(v_uv.x, v_uv.y, 0.0, 0.85);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_shaders_declare_a_version() {
        for source in [
            TERRAIN_VERTEX,
            TERRAIN_FRAGMENT,
            TERRAIN_VERTEX_PACKED4,
            TERRAIN_FRAGMENT_PACKED4,
            OVERLAY_VERTEX,
            OVERLAY_FRAGMENT,
        ] {
            assert!(source.trim_start().starts_with("#version 330 core"));
            assert!(source.contains("void main"));
        }
    }

    #[test]
    fn camera_block_matches_the_backend_layout() {
        // The backend binds group 0 / binding 0 with a 160-byte dynamic offset.
        for source in [
            TERRAIN_VERTEX,
            TERRAIN_VERTEX_PACKED4,
            OVERLAY_VERTEX,
        ] {
            assert!(source.contains("layout(set = 0, binding = 0)"));
        }
    }

    #[test]
    fn packed4_shader_has_no_colour_attribute() {
        // The 20-byte layout ends after the UVs; declaring a third attribute
        // would make wgpu reject the pipeline.
        assert!(!TERRAIN_VERTEX_PACKED4.contains("a_color"));
        assert!(TERRAIN_VERTEX.contains("layout(location = 2) in vec4 a_color"));
    }
}
