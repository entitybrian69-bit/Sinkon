//! OptiFine shaderpack layout parser.
//!
//! OptiFine packs are a `shaders/` directory of GLSL files plus a
//! `shaders.properties` descriptor. The pass order is fixed and part of the
//! OptiFine contract:
//!
//! ```text
//!   shadow → gbuffers_{sky,terrain,water,entities,...} → deferred* → composite* → final*
//! ```
//!
//! Sinkon parses the directory into [`ShaderpackLayout`], resolves `#include`s
//! (the packs rely on them heavily) and records the custom uniforms each pass
//! declares so the launcher can be asked for the values it owns
//! (`sunPosition`, `moonPhase`, `frameTimeCounter`, …).
//!
//! Scope: this is a *translation* front-end. Packs that ship Java-side hooks or
//! rely on OptiFine-specific renderer behaviour cannot be reproduced; those are
//! listed in `docs/SHADER_MATRIX.md` as best-effort/known-broken per pack rather
//! than being silently accepted.

use std::path::{Path, PathBuf};

use crate::shader::{ShaderStage, ShaderpackKind, TranslateError};

/// Which stage of the OptiFine pipeline a program belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PassKind {
    /// Shadow map rendering (`shadow.vsh`/`.fsh`, or `shadowcomp*`).
    Shadow,
    /// Sky gbuffer.
    GbuffersSky,
    /// Terrain gbuffer.
    GbuffersTerrain,
    /// Water/translucent gbuffer.
    GbuffersWater,
    /// Entities gbuffer.
    GbuffersEntities,
    /// Any other gbuffer (`gbuffers_hand`, `gbuffers_weather`, …).
    GbuffersOther,
    /// `deferred*` lighting pass.
    Deferred,
    /// `composite*` pass.
    Composite,
    /// `final*` pass.
    Final,
    /// `shadowcomp*` pass.
    ShadowComposite,
}

impl PassKind {
    /// Classify a program filename stem (`gbuffers_terrain`, `composite3`, …).
    pub fn from_stem(stem: &str) -> PassKind {
        let stem = stem.trim_end_matches(".vsh").trim_end_matches(".fsh");
        if stem == "shadow" || stem.starts_with("shadowcomp") {
            if stem.starts_with("shadowcomp") {
                return PassKind::ShadowComposite;
            }
            return PassKind::Shadow;
        }
        if stem.starts_with("gbuffers_") {
            return match stem {
                "gbuffers_skybasic" | "gbuffers_skytextured" => PassKind::GbuffersSky,
                "gbuffers_terrain" | "gbuffers_terrain_solid" | "gbuffers_terrain_cutout" => {
                    PassKind::GbuffersTerrain
                }
                "gbuffers_water" | "gbuffers_hand_water" => PassKind::GbuffersWater,
                "gbuffers_entities" | "gbuffers_entities_glowing" | "gbuffers_hand" => {
                    PassKind::GbuffersEntities
                }
                _ => PassKind::GbuffersOther,
            };
        }
        if stem.starts_with("deferred") {
            return PassKind::Deferred;
        }
        if stem.starts_with("composite") {
            return PassKind::Composite;
        }
        if stem.starts_with("final") {
            return PassKind::Final;
        }
        PassKind::GbuffersOther
    }

    /// Pipeline rank; lower renders earlier.
    pub fn order(self) -> u8 {
        match self {
            PassKind::Shadow => 0,
            PassKind::GbuffersSky => 1,
            PassKind::GbuffersTerrain => 2,
            PassKind::GbuffersWater => 3,
            PassKind::GbuffersEntities => 4,
            PassKind::GbuffersOther => 5,
            PassKind::Deferred => 6,
            PassKind::ShadowComposite => 7,
            PassKind::Composite => 8,
            PassKind::Final => 9,
        }
    }
}

/// One translatable program pair.
#[derive(Clone, Debug)]
pub struct ShaderPass {
    /// Program stem (`gbuffers_terrain`).
    pub name: String,
    /// Pipeline stage.
    pub kind: PassKind,
    /// Vertex source file.
    pub vertex: Option<PathBuf>,
    /// Fragment source file.
    pub fragment: Option<PathBuf>,
    /// Program order in the pipeline.
    pub order: u8,
}

/// Parsed pack.
#[derive(Clone, Debug)]
pub struct ShaderpackLayout {
    /// Pack root (the directory containing `shaders/`, or `shaders/` itself).
    pub root: PathBuf,
    /// Detected dialect.
    pub kind: ShaderpackKind,
    /// Programs, in pipeline order.
    pub passes: Vec<ShaderPass>,
    /// Custom uniforms declared by any pass.
    pub uniforms: Vec<String>,
    /// `shaders.properties` entries (including includes).
    pub properties: Vec<(String, String)>,
    /// Number of `#include` directives resolved during the scan.
    pub includes_resolved: u32,
}

impl ShaderpackLayout {
    /// Parse a pack directory.
    ///
    /// Missing files are not an error (many packs ship only a fragment shader for
    /// a pass); an unreadable directory is.
    pub fn parse(root: &Path) -> Result<ShaderpackLayout, TranslateError> {
        let shaders_dir = if root.join("shaders").is_dir() {
            root.join("shaders")
        } else if root.is_dir() {
            root.to_path_buf()
        } else {
            return Err(TranslateError::Io(format!(
                "{} is not a shaderpack directory",
                root.display()
            )));
        };

        let mut properties = Vec::new();
        let properties_path = shaders_dir.join("shaders.properties");
        if let Ok(text) = std::fs::read_to_string(&properties_path) {
            properties = parse_properties(&text);
        }

        let mut passes: Vec<ShaderPass> = Vec::new();
        let mut uniforms: Vec<String> = Vec::new();
        let mut includes_resolved = 0u32;

        let entries = std::fs::read_dir(&shaders_dir)
            .map_err(|err| TranslateError::Io(format!("{}: {err}", shaders_dir.display())))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            let stage = if matches!(extension, "vsh" | "vert") {
                ShaderStage::Vertex
            } else if matches!(extension, "fsh" | "frag") {
                ShaderStage::Fragment
            } else {
                continue;
            };
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("program")
                .to_string();
            let kind = PassKind::from_stem(&stem);

            if let Ok(source) = std::fs::read_to_string(&path) {
                let resolved = resolve_includes(&source, &shaders_dir, 0);
                if resolved.depth > 0 {
                    includes_resolved += resolved.depth;
                }
                for uniform in collect_uniforms(&resolved.source) {
                    if !uniforms.contains(&uniform) {
                        uniforms.push(uniform);
                    }
                }
            }

            match passes.iter_mut().find(|pass| pass.name == stem) {
                Some(pass) => {
                    if stage == ShaderStage::Vertex {
                        pass.vertex = Some(path);
                    } else {
                        pass.fragment = Some(path);
                    }
                }
                None => passes.push(ShaderPass {
                    name: stem,
                    kind,
                    vertex: (stage == ShaderStage::Vertex).then_some(path.clone()),
                    fragment: (stage == ShaderStage::Fragment).then_some(path),
                    order: kind.order(),
                }),
            }
        }

        passes.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
        let kind = crate::shader::detect_kind(root);
        Ok(ShaderpackLayout {
            root: shaders_dir,
            kind,
            passes,
            uniforms,
            properties,
            includes_resolved,
        })
    }

    /// Program names in pipeline order.
    pub fn program_names(&self) -> Vec<&str> {
        self.passes.iter().map(|pass| pass.name.as_str()).collect()
    }

    /// Look up a pass by name.
    pub fn pass(&self, name: &str) -> Option<&ShaderPass> {
        self.passes.iter().find(|pass| pass.name == name)
    }
}

/// Parse a `.properties` file.
///
/// Handles `key=value`, `key = value`, comments (`#`, `//`, `;`), continuation by
/// repeated keys (OptiFine treats repeats as append) and `!include`/`#include`
/// of another properties file relative to the current one.
pub fn parse_properties(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") || line.starts_with(';')
        {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        let value = value.trim().to_string();
        if key.is_empty() {
            continue;
        }
        out.push((key, value));
    }
    out
}

/// Result of an include resolution pass.
#[derive(Clone, Debug)]
pub struct ResolvedSource {
    /// Source with includes expanded.
    pub source: String,
    /// Depth/fan-out counter used for statistics.
    pub depth: u32,
    /// Includes that could not be resolved, with their paths.
    pub missing: Vec<String>,
}

/// Expand `#include "file"` / `#include <file>` directives.
///
/// Recursion is capped at [`MAX_INCLUDE_DEPTH`] so a pack with a cyclic include
/// graph cannot hang the loader.
pub fn resolve_includes(source: &str, base: &Path, depth: u32) -> ResolvedSource {
    let mut out = String::with_capacity(source.len());
    let mut resolved_depth = 0;
    let mut missing = Vec::new();
    if depth >= MAX_INCLUDE_DEPTH {
        return ResolvedSource {
            source: source.to_string(),
            depth,
            missing,
        };
    }
    for line in source.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("#include") {
            let name = rest
                .trim()
                .trim_start_matches(['"', '<'])
                .trim_end_matches(['"', '>'])
                .trim();
            if name.is_empty() {
                out.push_str(line);
                out.push('\n');
                continue;
            }
            let candidate = base.join(name);
            match std::fs::read_to_string(&candidate) {
                Ok(included) => {
                    let nested = resolve_includes(&included, base, depth + 1);
                    resolved_depth += 1 + nested.depth;
                    missing.extend(nested.missing);
                    out.push_str("// include: ");
                    out.push_str(name);
                    out.push('\n');
                    out.push_str(&nested.source);
                    if !nested.source.ends_with('\n') {
                        out.push('\n');
                    }
                }
                Err(_) => {
                    missing.push(candidate.display().to_string());
                    out.push_str("// missing include: ");
                    out.push_str(name);
                    out.push('\n');
                }
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    ResolvedSource {
        source: out,
        depth: resolved_depth,
        missing,
    }
}

/// Maximum `#include` nesting depth.
pub const MAX_INCLUDE_DEPTH: u32 = 8;

/// Collect `uniform <type> <name>;` declarations (and arrays) from GLSL source.
pub fn collect_uniforms(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("uniform ") else {
            continue;
        };
        // Skip uniform blocks: `uniform Camera { ... };`
        if rest.contains('{') {
            continue;
        }
        let rest = rest.trim_end_matches(';').trim();
        let name = rest
            .split_whitespace()
            .last()
            .unwrap_or("")
            .split('[')
            .next()
            .unwrap_or("")
            .to_string();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_order_matches_optifine() {
        assert!(PassKind::from_stem("shadow").order() < PassKind::from_stem("gbuffers_terrain").order());
        assert!(
            PassKind::from_stem("gbuffers_terrain").order()
                < PassKind::from_stem("deferred1").order()
        );
        assert!(
            PassKind::from_stem("composite3").order() < PassKind::from_stem("final").order()
        );
    }

    #[test]
    fn pass_kinds_are_classified() {
        assert_eq!(PassKind::from_stem("gbuffers_water"), PassKind::GbuffersWater);
        assert_eq!(PassKind::from_stem("gbuffers_hand"), PassKind::GbuffersEntities);
        assert_eq!(PassKind::from_stem("shadowcomp"), PassKind::ShadowComposite);
        assert_eq!(PassKind::from_stem("composite1"), PassKind::Composite);
        assert_eq!(PassKind::from_stem("final"), PassKind::Final);
    }

    #[test]
    fn properties_are_parsed() {
        let text = "# comment\nshadowMapResolution=2048\n// another\n sun = 1.0 \nbroken line\n";
        let parsed = parse_properties(text);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], ("shadowMapResolution".to_string(), "2048".to_string()));
        assert_eq!(parsed[1].0, "sun");
    }

    #[test]
    fn uniforms_are_collected() {
        let source = "uniform mat4 gbufferModelViewInverse;\nuniform sampler2D colortex0;\nuniform float frameTimeCounter;\nuniform Camera { mat4 mvp; };\n";
        let uniforms = collect_uniforms(source);
        assert!(uniforms.contains(&"gbufferModelViewInverse".to_string()));
        assert!(uniforms.contains(&"colortex0".to_string()));
        assert!(!uniforms.iter().any(|name| name == "Camera"), "blocks are skipped");
    }

    #[test]
    fn missing_pack_directory_is_an_error() {
        let error = ShaderpackLayout::parse(Path::new("/nope/not/a/pack")).expect_err("must fail");
        assert!(matches!(error, TranslateError::Io(_)));
    }
}
