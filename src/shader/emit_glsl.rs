//! GLSL emission.
//!
//! Two distinct jobs live here:
//!
//! 1. **ES 3.00 / 3.10** via naga's GLSL writer. Used for diagnostics, shaderpack
//!    dumps and the `glow` ES3 fallback. The `wgpu` GL backend does its own GLSL
//!    emission internally, so this output is never fed to `wgpu`.
//! 2. **ES 1.00** for the `glow` GLES 2.0 path. naga cannot emit `#version 100`,
//!    so this module emits ES 3.00 and then runs
//!    [`downlevel_to_es100`], a textual compatibility pass that fails loudly on
//!    anything ES 1.00 cannot express instead of producing a shader that
//!    silently renders wrong.

use crate::shader::{ShaderStage, TranslateError};

/// Emit GLSL ES `version` (300 or 310) for `stage`.
pub fn emit_es(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
    stage: ShaderStage,
    entry_point: &str,
    version: u16,
) -> Result<String, TranslateError> {
    let mut out = String::with_capacity(2048);
    let options = naga::back::glsl::Options {
        version: naga::back::glsl::Version::Embedded {
            version,
            is_webgl: false,
        },
        writer_flags: naga::back::glsl::WriterFlags::ADJUST_COORDINATE_SPACE,
        binding_map: naga::back::glsl::BindingMap::default(),
        zero_initialize_workgroup_memory: true,
    };
    let pipeline_options = naga::back::glsl::PipelineOptions {
        shader_stage: stage.to_naga(),
        entry_point: entry_point.to_string(),
        multiview: None,
    };
    let mut writer = naga::back::glsl::Writer::new(
        &mut out,
        module,
        info,
        &options,
        &pipeline_options,
        naga::proc::BoundsCheckPolicies::default(),
    )
    .map_err(|err| TranslateError::Emit(format!("glsl es{version}: {err}")))?;
    writer
        .write()
        .map_err(|err| TranslateError::Emit(format!("glsl es{version}: {err}")))?;
    Ok(out)
}

/// Constructs GLSL ES 1.00 cannot express.
const ES100_BLOCKERS: &[(&str, &str)] = &[
    ("layout(std140)", "uniform blocks (UBOs)"),
    ("layout(std430)", "shader storage blocks (SSBOs)"),
    ("layout(location", "explicit attribute/fragment locations"),
    ("textureLod(", "textureLod (ES 1.00 has no LOD bias sampling)"),
    ("textureGrad(", "textureGrad"),
    ("gl_FragData[", "multiple render targets"),
    ("out vec", "multiple fragment outputs"),
    ("flat ", "flat interpolation qualifiers"),
    ("uint", "unsigned integer types"),
    ("isampler", "integer samplers"),
    ("usampler", "integer samplers"),
    ("#extension", "explicit extensions"),
    ("bitfieldExtract(", "bitfield operations"),
    ("packHalf2x16(", "packing intrinsics"),
];

/// Downlevel an ES 3.00 shader to ES 1.00.
///
/// Returns [`TranslateError::UnsupportedFeature`] naming the construct when the
/// shader cannot be expressed; the message is what `docs/SHADER_MATRIX.md`
/// publishes for the GLES 2.0 column.
pub fn downlevel_to_es100(es300: &str, stage: ShaderStage) -> Result<String, TranslateError> {
    for (needle, description) in ES100_BLOCKERS {
        if es300.contains(needle) {
            // `out vec` is legal in a vertex shader only as a varying, which is
            // rewritten below; reject it for fragments only.
            if *needle == "out vec" && stage != ShaderStage::Fragment {
                continue;
            }
            return Err(TranslateError::UnsupportedFeature(format!(
                "GLSL ES 1.00 cannot express {description} (found `{needle}`)"
            )));
        }
    }

    let mut out = String::with_capacity(es300.len() + 256);
    let mut fragment_output: Option<String> = None;
    let mut body = String::with_capacity(es300.len());

    for line in es300.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#version") {
            out.push_str("#version 100\n");
            continue;
        }
        if trimmed.starts_with("#extension") {
            continue;
        }
        if trimmed.is_empty() {
            body.push('\n');
            continue;
        }
        let mut line_out = line.to_string();

        // Fragment outputs become gl_FragColor.
        if stage == ShaderStage::Fragment {
            if let Some(rest) = trimmed.strip_prefix("out ") {
                let name = rest
                    .split(|c: char| c.is_whitespace() || c == ';')
                    .filter(|token| !token.is_empty())
                    .last()
                    .unwrap_or("frag_color")
                    .trim_end_matches(';')
                    .to_string();
                fragment_output = Some(name);
                continue;
            }
        }

        // `in`/`out` become attribute/varying per stage.
        line_out = replace_qualified(&line_out, "in ", "attribute ", stage == ShaderStage::Vertex);
        if stage == ShaderStage::Vertex {
            line_out = replace_qualified(&line_out, "out ", "varying ", true);
        } else {
            line_out = replace_qualified(&line_out, "in ", "varying ", true);
        }

        // ES 1.00 sampling functions.
        line_out = line_out
            .replace("texture(", "texture2D(")
            .replace("textureLod(", "texture2D(");
        // `textureProj` has a direct ES 1.00 equivalent.
        line_out = line_out.replace("textureProj2D(", "texture2DProj(");
        body.push_str(&line_out);
        body.push('\n');
    }

    if let Some(name) = fragment_output {
        let rewritten = body.replace(&format!("{name} ="), "gl_FragColor =");
        body = rewritten.replace(&name, "gl_FragColor");
    }
    if stage == ShaderStage::Fragment && !body.contains("precision ") {
        out.push_str("precision mediump float;\n");
    }
    out.push_str(&body);
    Ok(out)
}

/// Replace `from` when it appears at the start of a declaration.
fn replace_qualified(line: &str, from: &str, to: &str, enabled: bool) -> String {
    if !enabled {
        return line.to_string();
    }
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix(from) {
        let indent = &line[..line.len() - trimmed.len()];
        return format!("{indent}{to}{rest}");
    }
    line.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn es100_rewrites_attributes() {
        let es300 = "#version 300 es\nin vec3 a_pos;\nout vec2 v_uv;\nvoid main(){ v_uv = a_pos.xy; }\n";
        let es100 = downlevel_to_es100(es300, ShaderStage::Vertex).expect("downlevels");
        assert!(es100.starts_with("#version 100"));
        assert!(es100.contains("attribute vec3 a_pos;"));
        assert!(es100.contains("varying vec2 v_uv;"));
        assert!(!es100.contains("in vec3"));
    }

    #[test]
    fn es100_uses_frag_color() {
        let es300 = "#version 300 es\nprecision mediump float;\nin vec4 v_color;\nout vec4 frag_color;\nvoid main(){ frag_color = v_color; }\n";
        let es100 = downlevel_to_es100(es300, ShaderStage::Fragment).expect("downlevels");
        assert!(es100.contains("gl_FragColor = v_color"));
        assert!(!es100.contains("out vec4"));
        assert!(es100.contains("varying vec4 v_color;"));
    }

    #[test]
    fn es100_rejects_uniform_blocks() {
        let es300 = "#version 300 es\nlayout(std140) uniform Camera { mat4 mvp; };\nvoid main(){}\n";
        let error = downlevel_to_es100(es300, ShaderStage::Vertex).expect_err("must reject");
        assert!(matches!(error, TranslateError::UnsupportedFeature(_)));
        assert!(error.to_string().contains("uniform blocks"));
    }

    #[test]
    fn es100_rejects_mrt() {
        let es300 = "#version 300 es\nlayout(location=0) out vec4 a;\nlayout(location=1) out vec4 b;\nvoid main(){}\n";
        let error = downlevel_to_es100(es300, ShaderStage::Fragment).expect_err("must reject");
        assert!(error.to_string().contains("explicit attribute"));
    }

    #[test]
    fn es100_adds_precision_for_fragments() {
        let es300 = "#version 300 es\nvoid main(){ gl_FragColor = vec4(1.0); }\n";
        let es100 = downlevel_to_es100(es300, ShaderStage::Fragment).expect("downlevels");
        assert!(es100.contains("precision mediump float;"));
    }
}
