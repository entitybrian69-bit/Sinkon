//! naga front-end glue: preprocessing, GLSL parsing and validation.
//!
//! naga's GLSL front-end accepts core `#version 330`+ and GLSL ES 3.00/3.10.
//! Minecraft's older shaders and most OptiFine packs are written for
//! `#version 120`/`#version 150`, so they are upgraded textually first. The
//! upgrade is deliberately conservative: it rewrites only the constructs whose
//! meaning is unambiguous, and reports anything else to the caller so the shader
//! matrix can mark the pack as best-effort or broken instead of pretending.

use crate::shader::{ShaderStage, TranslateError};

/// Parse GLSL into a validated-cost naga module (validation is separate).
pub fn parse_glsl(source: &str, stage: ShaderStage) -> Result<naga::Module, TranslateError> {
    let mut frontend = naga::front::glsl::Frontend::default();
    let options = naga::front::glsl::Options {
        stage: stage.to_naga(),
        defines: naga::FastHashMap::default(),
    };
    frontend.parse(&options, source).map_err(|errors| {
        TranslateError::Parse(format!("{:?}", errors).chars().take(512).collect::<String>())
    })
}

/// Validate a parsed module and return its analysis info.
pub fn validate(module: &naga::Module) -> Result<naga::valid::ModuleInfo, TranslateError> {
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    );
    validator
        .validate(module)
        .map_err(|err| TranslateError::Validation(format!("{err:?}")))
}

/// Read the `#version` directive, if any.
pub fn version_of(source: &str) -> Option<(u32, bool)> {
    for line in source.lines().take(8) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("#version") {
            let is_es = rest.contains("es");
            let value = rest
                .split_whitespace()
                .find_map(|token| token.parse::<u32>().ok())?;
            return Some((value, is_es));
        }
    }
    None
}

/// Upgrade old GLSL to something naga can parse.
///
/// Handles, in order: `#version` normalisation, `attribute`/`varying`,
/// `gl_FragColor`/`gl_FragData`, `texture2D`-family calls and the implicit
/// fixed-function matrices. Anything else is left untouched for
/// [`validate`]/[`parse_glsl`] to reject loudly.
pub fn compat_upgrade(source: &str, stage: ShaderStage) -> String {
    let needs_upgrade = match version_of(source) {
        Some((version, is_es)) => {
            if is_es {
                version < 300
            } else {
                version < 330
            }
        }
        // No version directive: Minecraft vanilla core shaders always have one,
        // so treat a missing directive as legacy.
        None => true,
    };
    if !needs_upgrade {
        return source.to_string();
    }

    let mut text = String::with_capacity(source.len() + 256);
    let mut wrote_header = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#version") {
            if !wrote_header {
                text.push_str("#version 330 core\n");
                wrote_header = true;
            }
            continue;
        }
        text.push_str(line);
        text.push('\n');
    }
    if !wrote_header {
        let mut with_header = String::from("#version 330 core\n");
        with_header.push_str(&text);
        text = with_header;
    }

    // Stage-specific in/out keywords.
    let (from_kw, to_kw) = match stage {
        ShaderStage::Vertex => ("varying ", "out "),
        ShaderStage::Fragment => ("varying ", "in "),
        ShaderStage::Compute => ("varying ", "in "),
    };
    text = replace_word(&text, "attribute ", "in ");
    text = replace_word(&text, from_kw, to_kw);

    // Fixed-function matrices become uniforms provided by Sinkon.
    text = replace_word(&text, "gl_ModelViewProjectionMatrix", "u_mvp");
    text = replace_word(&text, "gl_ModelViewMatrix", "u_model_view");
    text = replace_word(&text, "gl_ProjectionMatrix", "u_projection");

    // Texture sampling renames.
    text = replace_word(&text, "texture2D(", "texture(");
    text = replace_word(&text, "texture2DLod(", "textureLod(");
    text = replace_word(&text, "textureCube(", "texture(");

    // Fragment output.
    if stage == ShaderStage::Fragment {
        text = replace_word(&text, "gl_FragColor", "sinkon_frag_color");
        if !text.contains("uniform mat4 u_mvp") {
            // Harmless if unused; keeps upgraded shaders that reference the old
            // implicit matrix compilable.
            text.push_str("uniform mat4 u_mvp;\n");
        }
        if text.contains("sinkon_frag_color") && !text.contains("out vec4 sinkon_frag_color") {
            text.push_str("out vec4 sinkon_frag_color;\n");
        }
        text = replace_word(&text, "gl_FragData[0]", "sinkon_frag_color");
    }

    text
}

/// Replace `needle` when it appears as a standalone token.
///
/// `str::replace` alone would corrupt identifiers such as `my_varying_value`, so
/// occurrences are only accepted when the neighbouring characters are not
/// identifier characters. `needle` may end with a non-identifier character
/// (`texture2D(`), in which case the trailing character is re-appended.
fn replace_word(text: &str, needle: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(position) = rest.find(needle) {
        let before = &rest[..position];
        let after = &rest[position + needle.len()..];
        let before_ok = before
            .chars()
            .next_back()
            .map(|c| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(true);
        let after_ok = after
            .chars()
            .next()
            .map(|c| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(true);
        out.push_str(before);
        if before_ok && after_ok {
            out.push_str(replacement);
        } else {
            out.push_str(needle);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Does `source` declare a uniform block, which GLSL ES 1.00 cannot express?
pub fn uses_uniform_block(source: &str) -> bool {
    source.contains("layout(std140)") || source.contains("uniform ") && source.contains("{\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_detection() {
        assert_eq!(version_of("#version 120\nvoid main(){}"), Some((120, false)));
        assert_eq!(
            version_of("// header\n#version 300 es\nvoid main(){}"),
            Some((300, true))
        );
        assert_eq!(version_of("void main(){}"), None);
    }

    #[test]
    fn legacy_shader_is_upgraded() {
        let source = "#version 120\nattribute vec3 a_pos;\nvarying vec2 v_uv;\n\
                      void main(){ gl_Position = gl_ModelViewProjectionMatrix * vec4(a_pos,1.0); }\n";
        let upgraded = compat_upgrade(source, ShaderStage::Vertex);
        assert!(upgraded.starts_with("#version 330 core"));
        assert!(upgraded.contains("in vec3 a_pos"));
        assert!(upgraded.contains("out vec2 v_uv"));
        assert!(upgraded.contains("u_mvp * vec4(a_pos,1.0)"));
        assert!(!upgraded.contains("attribute"));
        assert!(!upgraded.contains("gl_ModelViewProjectionMatrix"));
    }

    #[test]
    fn fragment_shader_gets_output_variable() {
        let source = "#version 120\nvarying vec4 v_color;\nvoid main(){ gl_FragColor = v_color; }\n";
        let upgraded = compat_upgrade(source, ShaderStage::Fragment);
        assert!(upgraded.contains("out vec4 sinkon_frag_color;"));
        assert!(upgraded.contains("sinkon_frag_color = v_color"));
        assert!(!upgraded.contains("gl_FragColor"));
    }

    #[test]
    fn modern_shader_is_left_alone() {
        let source = "#version 330 core\nin vec3 a_pos;\nvoid main(){}\n";
        assert_eq!(compat_upgrade(source, ShaderStage::Vertex), source);
        let es = "#version 310 es\nvoid main(){}\n";
        assert_eq!(compat_upgrade(es, ShaderStage::Vertex), es);
    }

    #[test]
    fn word_replacement_respects_identifiers() {
        let text = "my_attribute_value attribute counter_texture2D(";
        let replaced = replace_word(text, "attribute ", "in ");
        assert!(replaced.contains("my_attribute_value"));
        assert!(replaced.contains("in counter_texture2D("));
    }

    #[test]
    fn texture_calls_are_renamed() {
        let source = "#version 120\nvoid main(){ vec4 c = texture2D(u_tex, v_uv); }\n";
        let upgraded = compat_upgrade(source, ShaderStage::Fragment);
        assert!(upgraded.contains("texture(u_tex, v_uv)"));
    }
}
