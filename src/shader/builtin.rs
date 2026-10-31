//! Built-in shaders.
//!
//! Sinkon renders with a small, self-contained shader pair until the launcher
//! hands over Minecraft's own core shaders. Keeping it in-tree means a fresh
//! install (or a launcher that has not yet extracted the game assets) still
//! produces visible geometry, and it gives the benchmarking harness something
//! deterministic to translate on every backend.
//!
//! The sources are Vulkan-flavoured GLSL 3.30: an explicit `set`/`binding`
//! camera block that matches the backend's uniform layout, plus a
//! `layout(location)` vertex input matching Sinkon's packed chunk vertex.

/// Vertex shader for chunk terrain.
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

/// Fragment shader for chunk terrain.
pub const TERRAIN_FRAGMENT: &str = r#"#version 330 core

in vec2 v_uv;
in vec4 v_color;

layout(location = 0) out vec4 frag_color;

void main() {
    // Cheap, deterministic shading: a UV gradient tinted by the vertex colour.
    // The real atlas sampling arrives with the material system (KNOWN_ISSUES KI-3).
    vec3 tint = mix(vec3(0.10, 0.55, 0.90), vec3(0.55, 0.25, 0.90), v_uv.y);
    frag_color = vec4(tint * (0.35 + 0.65 * v_color.a), 1.0);
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
        for source in [TERRAIN_VERTEX, OVERLAY_VERTEX] {
            assert!(source.contains("layout(set = 0, binding = 0)"));
        }
    }
}
