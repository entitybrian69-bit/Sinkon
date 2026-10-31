//! Vulkan backend (`wgpu::Backends::VULKAN`), Vulkan 1.1+.
//!
//! Thin configuration of [`crate::backend::WgpuBackend`]. This module owns the
//! Vulkan-specific parts: the backend mask, the optional feature request, the
//! `compatibleDevice()` guard and the capability probes used by
//! `SinkonVulkanRendererSpec` on the Kotlin side.

use crate::backend::{
    AdapterProbe, BackendError, BackendKind, BackendResult, Capabilities, GraphicsBackend,
    WgpuBackend, WgpuBackendConfig,
};
use crate::perf::tier::DeviceTier;
use crate::surface::SurfaceTarget;

/// GPU label used for debug names.
pub const LABEL: &str = "sinkon.vulkan";

/// Features requested when the adapter offers them. None are required: a device
/// that offers none of them still runs the backend, just with CPU culling and no
/// GPU timestamps.
pub const OPTIONAL_FEATURES: wgpu::Features = wgpu::Features::TIMESTAMP_QUERY
    .union(wgpu::Features::TEXTURE_COMPRESSION_ASTC)
    .union(wgpu::Features::TEXTURE_COMPRESSION_BC)
    .union(wgpu::Features::ANISOTROPIC_FILTERING)
    .union(wgpu::Features::STORAGE_RESOURCE_BINDING_ARRAY);

/// Create the Vulkan backend on `target`.
pub fn create(
    target: &SurfaceTarget<'_>,
    tier: DeviceTier,
) -> BackendResult<Box<dyn GraphicsBackend>> {
    let config = WgpuBackendConfig {
        backends: wgpu::Backends::VULKAN,
        kind: BackendKind::Vulkan,
        label: LABEL,
        optional_features: OPTIONAL_FEATURES,
    };
    let backend = WgpuBackend::create(target, config, tier)?;
    Ok(Box::new(backend))
}

/// `compatibleDevice()` guard for the Kotlin `SinkonVulkanRendererSpec`.
///
/// The launcher may pre-check Vulkan support before offering the renderer; Sinkon
/// repeats the check here so a wrong hint can never select a broken path.
pub fn compatible_device(caps: &Capabilities) -> bool {
    let api_ok = caps.vulkan_api.map(|api| api >= (1 << 22)).unwrap_or(false);
    if !api_ok {
        log::warn!(
            "vulkan compatibleDevice(): loader reports {:?} (< 1.1)",
            caps.vulkan_api.map(|api| format!("{}.{}", api >> 22, (api >> 12) & 0x3ff))
        );
    }
    api_ok
}

/// Enumerate Vulkan adapters without creating a device.
pub fn probe() -> Vec<AdapterProbe> {
    WgpuBackend::probe(wgpu::Backends::VULKAN)
}

/// Human readable backend summary for logs.
pub fn describe(caps: &Capabilities) -> String {
    format!(
        "Vulkan {} on \"{}\" (driver {}) tier {}",
        caps.api_version, caps.adapter_name, caps.driver_info, caps.device_tier
    )
}

/// Error used when the launcher forces Vulkan on a device that cannot run it.
pub fn unsupported() -> BackendError {
    BackendError::Unavailable("Vulkan 1.1+ adapter not present".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendFeatures;

    fn caps(vulkan_api: Option<u32>) -> Capabilities {
        Capabilities {
            kind: BackendKind::Vulkan,
            api_name: "Vulkan".into(),
            api_version: "1.3.0".into(),
            gl_version: None,
            gles_version: None,
            vulkan_api,
            glsl_version: "450".into(),
            adapter_name: "Adreno (TM) 740".into(),
            driver_info: "Qualcomm".into(),
            max_texture_size: 16384,
            max_uniform_buffer_binding_size: 65536,
            max_compute_workgroup_size: [128, 1, 1],
            supports: BackendFeatures::NONE,
            extensions: vec![],
            device_tier: "S".into(),
            vram_bytes: 0,
            resident_sections: 0,
        }
    }

    #[test]
    fn guard_requires_vulkan_1_1() {
        assert!(compatible_device(&caps(Some((1 << 22) | (1 << 12)))));
        assert!(!compatible_device(&caps(Some(1 << 22))));
        assert!(!compatible_device(&caps(None)));
    }
}
