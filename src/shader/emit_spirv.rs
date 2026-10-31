//! SPIR-V emission (Vulkan, and the `wgpu` GL backend which re-emits GLSL itself).
//!
//! Both `wgpu` backends consume SPIR-V, so this is the only emitter on the
//! Vulkan and GLES 3.x paths. Emitting SPIR-V 1.0 keeps compatibility with older
//! Android Vulkan drivers (1.1 class hardware) — later versions buy nothing here
//! because Sinkon uses no SPIR-V 1.4+ capability.

use crate::shader::TranslateError;

/// SPIR-V version Sinkon targets: 1.0.
pub const LANG_VERSION: (u8, u8) = (1, 0);

/// Emit SPIR-V words for a validated module.
pub fn emit(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
) -> Result<Vec<u32>, TranslateError> {
    let options = options();
    naga::back::spv::write_vec(module, info, &options, None)
        .map_err(|err| TranslateError::Emit(format!("spirv: {err}")))
}

fn options() -> naga::back::spv::Options<'static> {
    naga::back::spv::Options {
        lang_version: LANG_VERSION,
        // ADJUST_COORDINATE_SPACE makes the emitted SPIR-V match GL's clip-space
        // convention (y up, z −1…1). Sinkon supplies GL-style matrices from the
        // launcher, so this keeps both backends consistent.
        flags: naga::back::spv::WriterFlags::ADJUST_COORDINATE_SPACE
            | naga::back::spv::WriterFlags::LABEL_VARYINGS
            | naga::back::spv::WriterFlags::CLAMP_FRAG_DEPTH,
        binding_map: naga::back::spv::BindingMap::default(),
        capabilities: None,
        bounds_check_policies: naga::proc::BoundsCheckPolicies::default(),
        zero_initialize_workgroup_memory:
            naga::back::spv::ZeroInitializeWorkgroupMemoryMode::Polyfill,
        debug_info: None,
    }
}

/// Human readable module summary for the debug overlay: entry points and
/// resource bindings, which is what shaderpack debugging needs 95% of the time.
pub fn describe(module: &naga::Module) -> String {
    let mut out = String::with_capacity(128);
    for (index, entry) in module.entry_points.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&entry.name);
    }
    format!(
        "{} entry point(s) [{}], {} global(s)",
        module.entry_points.len(),
        out,
        module.global_variables.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_version_is_stable() {
        assert_eq!(LANG_VERSION, (1, 0));
    }

    #[test]
    fn describe_handles_empty_module() {
        let module = naga::Module::default();
        assert!(describe(&module).contains("0 entry point(s)"));
    }
}
