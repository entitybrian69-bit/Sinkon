//! # Sinkon
//!
//! Sinkon is a dual-backend Minecraft Java Edition renderer for Android
//! launcher forks (PojavLauncher / MojoLauncher / Zalith Launcher).
//!
//! One `cdylib` (`libsinkon.so`) carries three rendering paths behind a single
//! backend abstraction:
//!
//! | Backend            | Implementation                     | Devices                        |
//! |--------------------|------------------------------------|--------------------------------|
//! | [`BackendKind::Vulkan`] | `wgpu` Vulkan 1.1+, `VK_KHR_*` where available | Vulkan-capable Android 8+ |
//! | [`BackendKind::Gles3`]  | `wgpu` GL/EGL (GLES 3.0/3.1)       | everything with GLES 3.x       |
//! | [`BackendKind::Gles2Legacy`] | `glow` + EGL (GLES 2.0/2.1)  | legacy devices, Alpha–1.6      |
//!
//! Everything above the backend trait — chunk meshing, culling, shader
//! translation, uniform layout, the JNI bridge — is written once and dispatched.
//!
//! ## Invariants
//!
//! * The per-frame path (`SinkonRenderer::render_frame` and everything it calls)
//!   performs **zero heap allocations**. Debug builds enforce this with the
//!   `alloc-gate` feature; see [`perf::alloc`].
//! * No per-frame JNI object creation. Every `JNIEnv` lookup that the bridge
//!   needs is resolved once in `nativeInit` and stored as a `GlobalRef`.
//! * No `unwrap()`/`expect()` at the JNI boundary: every failure is logged under
//!   the `Sinkon` tag and surfaced as a zero/`null` return with a recorded status.
//!
//! ## Safety
//!
//! `unsafe` is confined to: FFI declarations (`surface`, `backend::gles2_legacy`,
//! `jni_bridge`), GPU handle wrappers (`raw-window-handle` construction) and the
//! counting global allocator. Every block carries a `// SAFETY:` comment stating
//! the invariant it relies on. Per-backend invariants are listed in
//! `docs/ARCHITECTURE.md`.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod backend;
pub mod chunk;
pub mod perf;
pub mod renderer;
pub mod shader;
pub mod surface;
pub mod version_profile;

#[cfg(target_os = "android")]
pub mod jni_bridge;

/// Log tag used by every Sinkon log line and by the logcat filter
/// (`adb logcat -s Sinkon`).
pub const LOG_TAG: &str = "Sinkon";

/// Crate version, surfaced in capability reports.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Backend selector values accepted by `nativeInit` / `nativeSwitchBackend`.
pub mod hint {
    /// Let Sinkon pick the fastest working backend.
    pub const AUTO: i32 = 0;
    /// Force Vulkan (`wgpu::Backends::VULKAN`).
    pub const VULKAN: i32 = 1;
    /// Force GLES 3.x (`wgpu::Backends::GL`).
    pub const GLES3: i32 = 2;
    /// Force the GLES 2.0/2.1 legacy path (`glow`).
    pub const GLES2_LEGACY: i32 = 3;
}

/// Initialise the `Sinkon` log tag exactly once.
///
/// On Android this installs `android_logger` at `Info` level with the tag
/// `Sinkon`; on the host it installs nothing (tests bring their own subscriber)
/// so the function is safe to call from every entry point.
pub fn init_logging(level: log::LevelFilter) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        #[cfg(target_os = "android")]
        {
            android_logger::init_once(
                android_logger::Config::default()
                    .with_tag(LOG_TAG)
                    .with_max_level(level)
                    .with_filter(
                        android_logger::FilterBuilder::new()
                            .filter_level(level)
                            .filter_module("wgpu", log::LevelFilter::Warn)
                            .filter_module("naga", log::LevelFilter::Warn)
                            .build(),
                    ),
            );
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = level;
        }
    });
}

/// Allocation counting global allocator.
///
/// Only compiled in with `--features alloc-gate`; otherwise the system
/// allocator is used and the counters stay at zero. See [`perf::alloc`].
#[cfg(feature = "alloc-gate")]
#[global_allocator]
static SINKON_ALLOC: perf::alloc::CountingAlloc = perf::alloc::CountingAlloc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_round_trip() {
        for h in [hint::AUTO, hint::VULKAN, hint::GLES3, hint::GLES2_LEGACY] {
            if h == hint::AUTO {
                assert_eq!(BackendKind::from_hint(h), None);
            } else {
                let kind = BackendKind::from_hint(h).expect("known hint");
                assert_eq!(kind.hint(), h);
            }
        }
    }

    #[test]
    fn logging_is_idempotent() {
        init_logging(log::LevelFilter::Info);
        init_logging(log::LevelFilter::Info);
    }
}
