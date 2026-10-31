//! Iris shaderpack layout parser.
//!
//! Iris packs are OptiFine-compatible plus:
//!
//! * per-dimension overrides in `shaders/world0/`, `world-1/`, `world_nether/`, …
//! * `iris_*` uniforms (`iris_FrameTime`, `iris_ShadowAngle`, …)
//! * optional compute programs (`.csh`) used by voxel-shading packs
//! * `begin`/`prepare`/`setup` programs (Iris 1.6+)
//!
//! Sinkon reuses the OptiFine scanner for the base directory and for each
//! dimension directory, then layers the Iris-specific information on top.
//!
//! Compute programs are parsed and reported but **cannot** run on either GL
//! backend; Vulkan runs them on the compute queue. `docs/SHADER_MATRIX.md` marks
//! the affected packs accordingly.

use std::path::{Path, PathBuf};

use super::optifine::{collect_uniforms, resolve_includes, PassKind, ShaderpackLayout};
use crate::shader::TranslateError;

/// Iris-specific program kinds on top of the OptiFine set.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IrisPassKind {
    /// A vanilla-style pass (`shadow`, `composite*`, …).
    Classic(PassKind),
    /// `begin` program (frame setup).
    Begin,
    /// `prepare` program.
    Prepare,
    /// `setup` program.
    Setup,
    /// Compute program (`.csh`).
    Compute,
}

impl IrisPassKind {
    /// Classify a program stem.
    pub fn from_stem(stem: &str) -> IrisPassKind {
        match stem {
            "begin" => IrisPassKind::Begin,
            "prepare" | "prepare1" | "prepare2" => IrisPassKind::Prepare,
            "setup" | "setup1" | "setup2" => IrisPassKind::Setup,
            other if other.starts_with("begin") => IrisPassKind::Begin,
            other if other.starts_with("setup") => IrisPassKind::Setup,
            other if other.starts_with("prepare") => IrisPassKind::Prepare,
            other => IrisPassKind::Classic(PassKind::from_stem(other)),
        }
    }
}

/// One program found in a pack (possibly dimension-scoped).
#[derive(Clone, Debug)]
pub struct IrisPass {
    /// Program stem.
    pub name: String,
    /// Kind.
    pub kind: IrisPassKind,
    /// Vertex source.
    pub vertex: Option<PathBuf>,
    /// Fragment source.
    pub fragment: Option<PathBuf>,
    /// Compute source.
    pub compute: Option<PathBuf>,
    /// Dimension directory this program overrides (`None` = base pack).
    pub dimension: Option<String>,
}

/// Parsed Iris pack.
#[derive(Clone, Debug)]
pub struct IrisLayout {
    /// Base `shaders/` directory.
    pub root: PathBuf,
    /// Dimension overrides found (`world0`, `world-1`, …).
    pub dimensions: Vec<String>,
    /// Base programs followed by dimension overrides.
    pub passes: Vec<IrisPass>,
    /// Every uniform declared by the pack (OptiFine + `iris_*`).
    pub uniforms: Vec<String>,
    /// True when the pack ships compute shaders.
    pub has_compute: bool,
    /// `shadowMapResolution` from the properties, when present.
    pub shadow_resolution: Option<u32>,
    /// Number of resolved includes.
    pub includes_resolved: u32,
}

impl IrisLayout {
    /// Parse a pack directory (the directory that contains `shaders/`).
    pub fn parse(root: &Path) -> Result<IrisLayout, TranslateError> {
        let base = ShaderpackLayout::parse(root)?;
        let shaders_dir = base.root.clone();

        let mut passes: Vec<IrisPass> = Vec::new();
        let mut uniforms = base.uniforms.clone();
        let mut has_compute = false;

        for pass in base.passes.iter() {
            passes.push(IrisPass {
                name: pass.name.clone(),
                kind: IrisPassKind::from_stem(&pass.name),
                vertex: pass.vertex.clone(),
                fragment: pass.fragment.clone(),
                compute: None,
                dimension: None,
            });
        }

        // Compute programs live beside the classic ones in Iris packs.
        if let Ok(entries) = std::fs::read_dir(&shaders_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("csh") {
                    has_compute = true;
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("compute")
                        .to_string();
                    if let Ok(source) = std::fs::read_to_string(&path) {
                        let resolved = resolve_includes(&source, &shaders_dir, 0);
                        for uniform in collect_uniforms(&resolved.source) {
                            if !uniforms.contains(&uniform) {
                                uniforms.push(uniform);
                            }
                        }
                    }
                    passes.push(IrisPass {
                        name,
                        kind: IrisPassKind::Compute,
                        vertex: None,
                        fragment: None,
                        compute: Some(path),
                        dimension: None,
                    });
                }
            }
        }

        // Dimension overrides.
        let mut dimensions = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&shaders_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if !(name.starts_with("world") || name == "dimension") {
                    continue;
                }
                dimensions.push(name.clone());
                match ShaderpackLayout::parse(&path) {
                    Ok(overrides) => {
                        for pass in overrides.passes.iter() {
                            passes.push(IrisPass {
                                name: pass.name.clone(),
                                kind: IrisPassKind::from_stem(&pass.name),
                                vertex: pass.vertex.clone(),
                                fragment: pass.fragment.clone(),
                                compute: None,
                                dimension: Some(name.clone()),
                            });
                        }
                        for uniform in overrides.uniforms {
                            if !uniforms.contains(&uniform) {
                                uniforms.push(uniform);
                            }
                        }
                    }
                    Err(err) => log::warn!("iris: dimension {name} could not be scanned: {err}"),
                }
            }
        }
        dimensions.sort();

        let shadow_resolution = base
            .properties
            .iter()
            .find(|(key, _)| key == "shadowMapResolution")
            .and_then(|(_, value)| value.parse::<u32>().ok());

        Ok(IrisLayout {
            root: shaders_dir,
            dimensions,
            passes,
            uniforms,
            has_compute,
            shadow_resolution,
            includes_resolved: base.includes_resolved,
        })
    }

    /// `iris_*` uniforms the pack consumes, which the launcher must supply.
    pub fn iris_uniforms(&self) -> Vec<&str> {
        self.uniforms
            .iter()
            .map(String::as_str)
            .filter(|name| name.starts_with("iris_"))
            .collect()
    }

    /// Programs that override a specific dimension.
    pub fn passes_for_dimension(&self, dimension: &str) -> Vec<&IrisPass> {
        self.passes
            .iter()
            .filter(|pass| pass.dimension.as_deref() == Some(dimension))
            .collect()
    }
}

/// Every `iris_*` uniform Sinkon knows how to feed.
///
/// Values come from the launcher (it owns the world clock, weather and camera),
/// which is why they are a fixed contract rather than free-form strings.
pub const IRIS_UNIFORMS: &[&str] = &[
    "iris_FrameTime",
    "iris_FrameCounter",
    "iris_ViewMatrix",
    "iris_ProjectionMatrix",
    "iris_ModelViewMatrix",
    "iris_ModelViewMatrixInverse",
    "iris_ViewMatrixInverse",
    "iris_ShadowModelView",
    "iris_ShadowProjection",
    "iris_CameraPosition",
    "iris_ShadowLightPosition",
    "iris_SunPosition",
    "iris_MoonPosition",
    "iris_UpPosition",
    "iris_ShadowAngle",
    "iris_SunAngle",
    "iris_EyeBrightness",
    "iris_NightVision",
    "iris_ScreenBrightness",
    "iris_PlayerMood",
    "iris_BottomScreenCenterDepth",
    "iris_FogColor",
    "iris_FogDensity",
    "iris_FogStart",
    "iris_DayTime",
    "iris_DayMix",
    "iris_NightMix",
    "iris_TwilightMix",
    "iris_SunriseSunsetMix",
    "iris_SunriseSunsetGlow",
    "iris_RainStrength",
    "iris_Wetness",
    "iris_IsEyeInWater",
    "iris_Blindness",
    "iris_DarknessFactor",
    "iris_HideGUI",
    "iris_AtlasSize",
    "iris_Constants",
];

/// Is `name` an `iris_*` uniform Sinkon can supply?
pub fn is_supported_uniform(name: &str) -> bool {
    IRIS_UNIFORMS.contains(&name)
}

/// `iris_*` uniforms a pack asks for that Sinkon cannot supply.
///
/// Non-empty means the pack will render with wrong lighting unless the shaderpack
/// tolerates the zero default; it is reported, never silently ignored.
pub fn unsupported_uniforms(layout: &IrisLayout) -> Vec<String> {
    layout
        .iris_uniforms()
        .into_iter()
        .filter(|name| !is_supported_uniform(name))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iris_kinds_are_classified() {
        assert_eq!(IrisPassKind::from_stem("begin"), IrisPassKind::Begin);
        assert_eq!(IrisPassKind::from_stem("setup1"), IrisPassKind::Setup);
        assert!(matches!(
            IrisPassKind::from_stem("composite2"),
            IrisPassKind::Classic(PassKind::Composite)
        ));
        assert!(matches!(
            IrisPassKind::from_stem("gbuffers_water"),
            IrisPassKind::Classic(PassKind::GbuffersWater)
        ));
    }

    #[test]
    fn uniform_list_is_sorted_and_unique() {
        let mut names = IRIS_UNIFORMS.to_vec();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "IRIS_UNIFORMS must not repeat entries");
        assert!(is_supported_uniform("iris_FrameTime"));
        assert!(!is_supported_uniform("iris_NotAThing"));
    }

    #[test]
    fn missing_pack_reports_io_error() {
        let error = IrisLayout::parse(Path::new("/nope/iris")).expect_err("must fail");
        assert!(matches!(error, TranslateError::Io(_)));
    }
}
