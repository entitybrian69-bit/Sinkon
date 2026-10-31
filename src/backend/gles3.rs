//! GLES 3.x backend (`wgpu::Backends::GL`), OpenGL ES 3.0/3.1 via EGL.
//!
//! Thin configuration of [`crate::backend::WgpuBackend`]. Sinkon does **not**
//! hand-write GLSL ES for this path: `wgpu`'s GL backend consumes SPIR-V and runs
//! naga's GLSL writer itself, which is both faster to maintain and the only way
//! to keep a single shader source for Vulkan and GLES 3.x. Hand-emitted GLSL is
//! reserved for the `glow` GLES 2.0 path, which cannot consume SPIR-V.
//!
//! GL is a "downlevel" backend in wgpu: no indirect draw, no compute, no pipeline
//! cache persistence. Those differences are reported through
//! [`Capabilities::supports`] and handled in the renderer rather than by
//! branching on the backend kind.

use crate::backend::{
    AdapterProbe, BackendError, BackendKind, BackendResult, Capabilities, GraphicsBackend,
    WgpuBackend, WgpuBackendConfig,
};
use crate::perf::tier::DeviceTier;
use crate::surface::SurfaceTarget;

/// GPU label used for debug names.
pub const LABEL: &str = "sinkon.gles3";

/// GL offers no optional wgpu feature Sinkon needs: everything it could use is
/// either core ES 3.0 (instancing) or emulated on the CPU.
pub const OPTIONAL_FEATURES: wgpu::Features = wgpu::Features::empty();

/// Create the GLES 3.x backend on `target`.
pub fn create(
    target: &SurfaceTarget<'_>,
    tier: DeviceTier,
) -> BackendResult<Box<dyn GraphicsBackend>> {
    let config = WgpuBackendConfig {
        backends: wgpu::Backends::GL,
        kind: BackendKind::Gles3,
        label: LABEL,
        optional_features: OPTIONAL_FEATURES,
    };
    let backend = WgpuBackend::create(target, config, tier)?;
    Ok(Box::new(backend))
}

/// Does this device satisfy the `SinkonGLRendererSpec` capability check?
///
/// Requires GLES 3.0+ (i.e. GLSL ES 3.00), which is what every profile from
/// 1.7 onwards needs.
pub fn compatible_device(caps: &Capabilities) -> bool {
    match caps.gles_version {
        Some(version) => {
            if version < 30 {
                log::warn!("gles3 compatibleDevice(): reported GLES {version} (< 30)");
                false
            } else {
                true
            }
        }
        None => {
            log::warn!(
                "gles3 compatibleDevice(): driver did not report a GLES version ({})",
                caps.driver_info
            );
            false
        }
    }
}

/// Enumerate GL adapters without creating a device.
pub fn probe() -> Vec<AdapterProbe> {
    WgpuBackend::probe(wgpu::Backends::GL)
}

/// Human readable backend summary for logs.
pub fn describe(caps: &Capabilities) -> String {
    format!(
        "GLES {} on \"{}\" (driver {}) tier {}",
        caps.gles_version.map(|v| format!("{}.{}", v / 10, v % 10)).unwrap_or_else(|| "?".into()),
        caps.adapter_name,
        caps.driver_info,
        caps.device_tier
    )
}

/// Error used when the launcher forces GL on a device that cannot run it.
pub fn unsupported() -> BackendError {
    BackendError::Unavailable("GLES 3.0+ context not available".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendFeatures;

    fn caps(gles: Option<u32>) -> Capabilities {
        Capabilities {
            kind: BackendKind::Gles3,
            api_name: "OpenGL ES".into(),
            api_version: "ES 3.2".into(),
            gl_version: Some("ES 3.2".into()),
            gles_version: gles,
            vulkan_api: None,
            glsl_version: "320 es".into(),
            adapter_name: "Adreno (TM) 640".into(),
            driver_info: "Qualcomm".into(),
            max_texture_size: 8192,
            max_uniform_buffer_binding_size: 16384,
            max_compute_workgroup_size: [0, 0, 0],
            supports: BackendFeatures::NONE,
            extensions: vec![],
            device_tier: "B".into(),
            vram_bytes: 0,
            resident_sections: 0,
        }
    }

    #[test]
    fn guard_requires_gles_3() {
        assert!(compatible_device(&caps(Some(30))));
        assert!(compatible_device(&caps(Some(32))));
        assert!(!compatible_device(&caps(Some(20))));
        assert!(!compatible_device(&caps(None)));
    }
}
