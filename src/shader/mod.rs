//! Shader pipeline: GLSL in, per-backend output, with hot reload.
//!
//! ```text
//!   vanilla core shader ─┐
//!   OptiFine shaderpack ─┼─▶ (preprocess) ─▶ naga front/glsl ─▶ validate ─┬─▶ SPIR-V      (Vulkan, wgpu GL)
//!   Iris shaderpack ─────┘                                                ├─▶ GLSL ES 3.x (diagnostics, glow)
//!                                                                         └─▶ GLSL ES 1.00 (glow, downlevel)
//! ```
//!
//! # Honest scope
//!
//! * naga's GLSL front-end accepts core GLSL 3.30+ and GLSL ES 3.00/3.10. It does
//!   **not** accept `#version 120`/`110`, so older shaders are upgraded by
//!   [`naga_translate::compat_upgrade`] before parsing. The upgrade covers
//!   `attribute`/`varying`, `gl_FragColor`, `texture2D` and the implicit
//!   `gl_ModelViewProjectionMatrix`. Anything beyond that is reported, not
//!   guessed.
//! * GLSL ES 1.00 output is produced by emitting ES 3.00 and running
//!   [`emit_glsl::downlevel_to_es100`], which rejects features ES 1.00 cannot
//!   express (uniform blocks, integer varyings, `textureLod`, MRT). The rejection
//!   list is what `docs/SHADER_MATRIX.md` publishes per shaderpack.
//! * Shaderpack support is a *translation* layer. Iris/OptiFine programs that call
//!   back into the Java-side renderer (custom Java hooks, compute-based voxel
//!   passes) cannot be translated and are listed in `KNOWN_ISSUES.md`.

pub mod builtin;
pub mod emit_glsl;
pub mod emit_spirv;
pub mod iris;
pub mod naga_translate;
pub mod optifine;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use crate::backend::BackendKind;

/// Shader stage.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShaderStage {
    /// Vertex stage.
    Vertex,
    /// Fragment stage.
    Fragment,
    /// Compute stage (Vulkan only).
    Compute,
}

impl ShaderStage {
    /// Filename suffixes that identify this stage in a shaderpack.
    pub const fn suffixes(self) -> &'static [&'static str] {
        match self {
            ShaderStage::Vertex => &[".vsh", ".vert", ".vs"],
            ShaderStage::Fragment => &[".fsh", ".frag", ".fs"],
            ShaderStage::Compute => &[".csh", ".comp", ".cs"],
        }
    }

    /// naga stage.
    pub fn to_naga(self) -> naga::ShaderStage {
        match self {
            ShaderStage::Vertex => naga::ShaderStage::Vertex,
            ShaderStage::Fragment => naga::ShaderStage::Fragment,
            ShaderStage::Compute => naga::ShaderStage::Compute,
        }
    }
}

/// Backend-specific emission target.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShaderTarget {
    /// SPIR-V 1.0, consumed by both `wgpu` backends.
    SpirV,
    /// GLSL ES 3.10 (`#version 310 es`) for inspection and the glow ES3 path.
    GlslEs310,
    /// GLSL ES 3.00.
    GlslEs300,
    /// GLSL ES 1.00 for the legacy `glow` path.
    GlslEs100,
}

impl ShaderTarget {
    /// Target for a backend.
    pub const fn for_backend(backend: BackendKind) -> ShaderTarget {
        match backend {
            BackendKind::Vulkan | BackendKind::Gles3 => ShaderTarget::SpirV,
            BackendKind::Gles2Legacy => ShaderTarget::GlslEs100,
        }
    }
}

/// Translated shader code.
#[derive(Clone, Debug)]
pub enum TranslatedCode {
    /// SPIR-V words.
    SpirV(Vec<u32>),
    /// GLSL source.
    Glsl(String),
}

impl TranslatedCode {
    /// Size in bytes of the produced code (SPIR-V words × 4).
    pub fn byte_len(&self) -> usize {
        match self {
            TranslatedCode::SpirV(words) => words.len() * 4,
            TranslatedCode::Glsl(text) => text.len(),
        }
    }
}

/// Why a shader could not be translated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranslateError {
    /// naga could not parse the source.
    Parse(String),
    /// naga parsed it but validation failed.
    Validation(String),
    /// The target backend cannot express a feature the shader uses.
    UnsupportedFeature(String),
    /// Backend emission failed.
    Emit(String),
    /// Filesystem error while loading a shaderpack.
    Io(String),
}

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranslateError::Parse(m) => write!(f, "parse error: {m}"),
            TranslateError::Validation(m) => write!(f, "validation error: {m}"),
            TranslateError::UnsupportedFeature(m) => write!(f, "unsupported feature: {m}"),
            TranslateError::Emit(m) => write!(f, "emit error: {m}"),
            TranslateError::Io(m) => write!(f, "io error: {m}"),
        }
    }
}

impl std::error::Error for TranslateError {}

/// Translate one GLSL source to `target`.
pub fn translate(
    source: &str,
    stage: ShaderStage,
    target: ShaderTarget,
) -> Result<TranslatedCode, TranslateError> {
    let prepared = naga_translate::compat_upgrade(source, stage);
    let module = naga_translate::parse_glsl(&prepared, stage)?;
    let info = naga_translate::validate(&module)?;
    match target {
        ShaderTarget::SpirV => emit_spirv::emit(&module, &info).map(TranslatedCode::SpirV),
        ShaderTarget::GlslEs310 => {
            emit_glsl::emit_es(&module, &info, stage, "main", 310).map(TranslatedCode::Glsl)
        }
        ShaderTarget::GlslEs300 => {
            emit_glsl::emit_es(&module, &info, stage, "main", 300).map(TranslatedCode::Glsl)
        }
        ShaderTarget::GlslEs100 => {
            emit_glsl::emit_es(&module, &info, stage, "main", 300)
                .and_then(|es300| emit_glsl::downlevel_to_es100(&es300, stage))
                .map(TranslatedCode::Glsl)
        }
    }
}

/// Which shaderpack dialect a directory holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShaderpackKind {
    /// No shaderpack: vanilla core shaders only.
    Vanilla,
    /// OptiFine `shaders/` layout.
    OptiFine,
    /// Iris layout (`shaders/` + `world*/`, `iris_*` uniforms).
    Iris,
}

impl ShaderpackKind {
    /// Stable lowercase name.
    pub const fn as_str(self) -> &'static str {
        match self {
            ShaderpackKind::Vanilla => "vanilla",
            ShaderpackKind::OptiFine => "optifine",
            ShaderpackKind::Iris => "iris",
        }
    }
}

/// A shader that has been compiled and is ready to be handed to the backend.
#[derive(Clone, Debug)]
pub struct CompiledShader {
    /// Debug name (`gbuffers_terrain.vsh`).
    pub name: String,
    /// Stage.
    pub stage: ShaderStage,
    /// Emitted code.
    pub code: TranslatedCode,
    /// Source modification time, for hot reload.
    pub modified: Option<SystemTime>,
}

/// Cached translation results with mtime-based hot reload.
///
/// Nothing here runs on the frame path except [`ShaderCache::poll_reload`], which
/// is called every N frames and only `stat`s a handful of paths.
#[derive(Default)]
pub struct ShaderCache {
    entries: HashMap<PathBuf, CompiledShader>,
    reloads: u64,
}

/// Outcome of a hot-reload poll.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ReloadOutcome {
    /// Files that changed and were recompiled.
    pub changed: Vec<String>,
    /// Files that failed to recompile (the old program stays live).
    pub failed: Vec<String>,
    /// Total reload cycles performed since init.
    pub reloads: u64,
}

impl ShaderCache {
    /// Empty cache.
    pub fn new() -> ShaderCache {
        ShaderCache::default()
    }

    /// Number of cached shaders.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Compile (or recompile) every GLSL file in `dir` that matches `stages`.
    ///
    /// Failures are collected per file so one broken shader does not take the
    /// whole pack down.
    pub fn compile_dir(
        &mut self,
        dir: &Path,
        target: ShaderTarget,
        stages: &[ShaderStage],
    ) -> (Vec<String>, Vec<(String, String)>) {
        let mut compiled = Vec::new();
        let mut failed = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return (compiled, vec![(dir.display().to_string(), "not a directory".into())]);
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("shader")
                .to_string();
            let Some(stage) = stages.iter().copied().find(|stage| {
                stage
                    .suffixes()
                    .iter()
                    .any(|suffix| suffix.trim_start_matches('.') == extension)
            }) else {
                continue;
            };
            match std::fs::read_to_string(&path) {
                Ok(source) => match translate(&source, stage, target) {
                    Ok(code) => {
                        self.entries.insert(
                            path.clone(),
                            CompiledShader {
                                name: name.clone(),
                                stage,
                                code,
                                modified: entry.metadata().ok().and_then(|m| m.modified().ok()),
                            },
                        );
                        compiled.push(name);
                    }
                    Err(err) => failed.push((name, err.to_string())),
                },
                Err(err) => failed.push((name, err.to_string())),
            }
        }
        (compiled, failed)
    }

    /// Recompile files whose mtime changed.
    pub fn poll_reload(&mut self, target: ShaderTarget) -> ReloadOutcome {
        let mut outcome = ReloadOutcome::default();
        let mut updates: Vec<(PathBuf, ShaderStage, String, Option<SystemTime>)> = Vec::new();
        for (path, shader) in self.entries.iter() {
            let Ok(metadata) = std::fs::metadata(path) else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            if shader.modified == Some(modified) {
                continue;
            }
            if let Ok(source) = std::fs::read_to_string(path) {
                updates.push((path.clone(), shader.stage, source, Some(modified)));
            }
        }
        for (path, stage, source, modified) in updates {
            match translate(&source, stage, target) {
                Ok(code) => {
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("shader")
                        .to_string();
                    if let Some(entry) = self.entries.get_mut(&path) {
                        entry.code = code;
                        entry.modified = modified;
                    }
                    outcome.changed.push(name);
                }
                Err(err) => {
                    let name = path.display().to_string();
                    log::warn!("hot reload failed for {name}: {err}");
                    outcome.failed.push(name);
                }
            }
        }
        if !outcome.changed.is_empty() {
            self.reloads += 1;
            log::info!(
                "shader hot reload: {} file(s) recompiled, {} failed",
                outcome.changed.len(),
                outcome.failed.len()
            );
        }
        outcome.reloads = self.reloads;
        outcome
    }

    /// Look up a compiled shader by name.
    pub fn get(&self, name: &str) -> Option<&CompiledShader> {
        self.entries.values().find(|shader| shader.name == name)
    }
}

/// Detect which dialect a shaderpack directory uses.
pub fn detect_kind(root: &Path) -> ShaderpackKind {
    let shaders = root.join("shaders");
    let base = if shaders.is_dir() { shaders } else { root.to_path_buf() };
    let Ok(entries) = std::fs::read_dir(&base) else {
        return ShaderpackKind::Vanilla;
    };
    let mut has_world = false;
    let mut has_properties = false;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name.starts_with("world") && name.len() <= 6 && entry.path().is_dir() {
            has_world = true;
        }
        if name == "shaders.properties" || name.ends_with(".properties") {
            has_properties = true;
        }
    }
    if has_world {
        ShaderpackKind::Iris
    } else if has_properties {
        ShaderpackKind::OptiFine
    } else {
        ShaderpackKind::Vanilla
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_follow_backends() {
        assert_eq!(
            ShaderTarget::for_backend(BackendKind::Vulkan),
            ShaderTarget::SpirV
        );
        assert_eq!(
            ShaderTarget::for_backend(BackendKind::Gles3),
            ShaderTarget::SpirV
        );
        assert_eq!(
            ShaderTarget::for_backend(BackendKind::Gles2Legacy),
            ShaderTarget::GlslEs100
        );
    }

    #[test]
    fn empty_cache_is_empty() {
        let cache = ShaderCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn missing_directory_reports_instead_of_panicking() {
        let mut cache = ShaderCache::new();
        let (compiled, failed) = cache.compile_dir(
            Path::new("/definitely/not/here"),
            ShaderTarget::SpirV,
            &[ShaderStage::Vertex],
        );
        assert!(compiled.is_empty());
        assert_eq!(failed.len(), 1);
    }

    #[test]
    fn stage_suffixes_are_disjoint_enough() {
        assert!(ShaderStage::Vertex.suffixes().contains(&".vsh"));
        assert!(ShaderStage::Fragment.suffixes().contains(&".fsh"));
        assert!(ShaderStage::Compute.suffixes().contains(&".csh"));
    }
}
