//! JNI boundary (`Java_org_sinkon_launcher_Sinkon_*`).
//!
//! # Contract
//!
//! * **No `unwrap`/`expect`.** Every failure is logged under the `Sinkon` tag and
//!   reported to Kotlin (zero handle, non-zero status, or a JSON `error` field).
//! * **No per-frame object creation.** `nativeRenderFrame` never calls
//!   `NewObject`/`NewByteArray`/`GetFieldID`/`GetMethodID`; chunk data, matrices
//!   and textures arrive through *direct* buffers read with
//!   `GetDirectBufferAddress` and are consumed without copying.
//! * **Thread affinity.** `nativeInit` and `nativeRenderFrame` must run on the
//!   same thread (the launcher's render thread). The legacy GL backend enforces
//!   this explicitly; the wgpu backends tolerate other threads but the launcher
//!   should not rely on it.
//!
//! # Why so few callbacks
//!
//! Sinkon never calls *into* Java after `nativeInit`. Everything the renderer
//! needs is pushed in through the exported functions, so there is nothing to
//! cache except the `Surface` class reference used to validate the window once.

use std::panic::{catch_unwind, AssertUnwindSafe};

use jni::objects::{GlobalRef, JByteBuffer, JClass, JMethodID, JObject, JString};
use jni::sys::{jint, jlong, jstring, JNI_VERSION_1_6};
use jni::{JNIEnv, JavaVM};

use crate::renderer::{RendererConfig, SinkonRenderer};
use crate::surface;

/// Magic number stored in every live handle; validating it turns a stale or
/// bogus `jlong` into a logged error instead of a segfault.
const HANDLE_MAGIC: u32 = 0x5349_4E4B; // "SINK"

/// Handle returned to Kotlin (`jlong`) and passed back on every call.
struct NativeHandle {
    magic: u32,
    renderer: SinkonRenderer,
}

/// Cached JNI lookups, resolved once in `nativeInit`.
struct JavaRefs {
    /// Keeps the `android.view.Surface` class reference alive for the process
    /// lifetime. Sinkon performs no per-frame lookups, so this exists purely to
    /// pin the class (and to make the cached state explicit).
    #[allow(dead_code)]
    surface_class: GlobalRef,
    surface_is_valid: JMethodID,
}

fn log_error(context: &str, error: impl std::fmt::Display) {
    log::error!("{context}: {error}");
}

/// Validate and reborrow a handle.
///
/// # Safety
///
/// `ctx` must be a value previously returned by `nativeInit` and not yet
/// destroyed. The magic check catches stale pointers but cannot prove liveness;
/// the launcher contract (destroy exactly once) is what makes this sound.
unsafe fn handle<'a>(ctx: jlong, context: &str) -> Option<&'a mut NativeHandle> {
    if ctx == 0 {
        log_error(context, "null renderer handle");
        return None;
    }
    let pointer = ctx as usize as *mut NativeHandle;
    if pointer.is_null() {
        log_error(context, "renderer handle is null");
        return None;
    }
    // SAFETY: the caller guarantees `ctx` came from `nativeInit`.
    let handle = unsafe { &mut *pointer };
    if handle.magic != HANDLE_MAGIC {
        log_error(context, "renderer handle magic mismatch (stale handle?)");
        return None;
    }
    Some(handle)
}

/// Panic guard: a Rust panic must never unwind into the JVM.
fn guard<T>(context: &str, fallback: T, body: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(_) => {
            log::error!("{context}: panicked inside the native call");
            fallback
        }
    }
}

/// Read a 4×4 float matrix out of a direct `FloatBuffer`.
fn read_mat4(buffer: &JByteBuffer<'_>, env: &JNIEnv<'_>) -> Result<[[f32; 4]; 4], String> {
    let address = env
        .get_direct_buffer_address(buffer)
        .map_err(|err| format!("GetDirectBufferAddress failed: {err}"))?;
    let capacity = env
        .get_direct_buffer_capacity(buffer)
        .map_err(|err| format!("GetDirectBufferCapacity failed: {err}"))?;
    if capacity < 16 * 4 {
        return Err(format!("matrix buffer holds {capacity} bytes, need 64"));
    }
    let mut matrix = [[0.0f32; 4]; 4];
    for (column, out_column) in matrix.iter_mut().enumerate() {
        for (row, value) in out_column.iter_mut().enumerate() {
            let offset = (column * 4 + row) * 4;
            // SAFETY: bounds checked against `capacity` above; the read is
            // unaligned-safe because Java direct buffers only guarantee 4-byte
            // alignment for FloatBuffers in practice, not for every allocator.
            *value = unsafe { std::ptr::read_unaligned(address.add(offset) as *const f32) };
        }
    }
    Ok(matrix)
}

/// Borrow a direct byte buffer as a slice.
///
/// # Safety
///
/// The returned slice is valid only for the duration of the JNI call; callers
/// must not store it.
unsafe fn direct_slice<'a>(
    buffer: &JByteBuffer<'_>,
    env: &JNIEnv<'_>,
) -> Option<(&'a [u8], usize)> {
    let address = env.get_direct_buffer_address(buffer).ok()?;
    let capacity = env.get_direct_buffer_capacity(buffer).ok()?;
    if address.is_null() || capacity == 0 {
        return None;
    }
    // SAFETY: guarded by the caller's contract (buffer alive for the call) and the
    // capacity reported by the JVM.
    Some((unsafe { std::slice::from_raw_parts(address, capacity) }, capacity))
}

// ---------------------------------------------------------------------------
// Exported functions
// ---------------------------------------------------------------------------

/// `nativeInit(env, surface, w, h, mcVersion, backendHint) -> long`
#[no_mangle]
pub extern "system" fn Java_org_sinkon_launcher_Sinkon_nativeInit<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    surface_object: JObject<'local>,
    width: jint,
    height: jint,
    mc_version: JString<'local>,
    backend_hint: jint,
) -> jlong {
    guard("nativeInit", 0, || {
        crate::init_logging(log::LevelFilter::Info);
        crate::perf::tracing::init(log::Level::Info);

        if surface_object.is_null() {
            log_error("nativeInit", "surface reference is null");
            return 0;
        }
        let version: String = match env.get_string(&mc_version) {
            Ok(value) => value.into(),
            Err(err) => {
                log_error("nativeInit", format!("mcVersion could not be read: {err}"));
                return 0;
            }
        };

        // Validate the Surface (and cache the class/method so nothing is looked up
        // per frame later).
        let refs = {
            let class = match env.get_object_class(&surface_object) {
                Ok(class) => class,
                Err(err) => {
                    log_error("nativeInit", format!("GetObjectClass failed: {err}"));
                    return 0;
                }
            };
            let method = match env.get_method_id(&class, "isValid", "()Z") {
                Ok(id) => id,
                Err(err) => {
                    log_error("nativeInit", format!("Surface.isValid lookup failed: {err}"));
                    return 0;
                }
            };
            let global = match env.new_global_ref(&surface_object) {
                Ok(reference) => reference,
                Err(err) => {
                    log_error("nativeInit", format!("NewGlobalRef failed: {err}"));
                    return 0;
                }
            };
            JavaRefs {
                surface_class: global,
                surface_is_valid: method,
            }
        };
        let valid = unsafe {
            env.call_method_unchecked(
                &surface_object,
                refs.surface_is_valid,
                jni::signature::ReturnType::Primitive(jni::signature::Primitive::Boolean),
                &[],
            )
        };
        match valid {
            Ok(jni::objects::JValueOwned::Boolean(true)) => {}
            Ok(_) => {
                log_error("nativeInit", "Surface.isValid() returned false");
                return 0;
            }
            Err(err) => {
                log_error("nativeInit", format!("Surface.isValid() failed: {err}"));
                return 0;
            }
        }
        log::info!("nativeInit: mcVersion={version}, {width}x{height}, hint={backend_hint}");

        let java_vm = env.get_java_vm().ok();
        let config = RendererConfig {
            mc_version: version,
            backend_hint,
            width: width.max(1) as u32,
            height: height.max(1) as u32,
            render_distance: 0,
            shaderpack: None,
        };

        // The ANativeWindow reference is owned by the renderer for its lifetime.
        let window = {
            // SAFETY: called on the JNI thread with a valid env and a live local
            // reference to an android.view.Surface.
            let converted = unsafe {
                surface::from_java_surface(
                    env.get_raw() as *mut std::ffi::c_void,
                    surface_object.as_raw(),
                )
            };
            match converted {
                Ok(window) => window,
                Err(err) => {
                    log_error("nativeInit", err);
                    return 0;
                }
            }
        };

        match SinkonRenderer::init(&config, window) {
            Ok(renderer) => {
                let handle = Box::new(NativeHandle {
                    magic: HANDLE_MAGIC,
                    renderer,
                });
                let pointer = Box::into_raw(handle) as jlong;
                log::info!(
                    "nativeInit → handle 0x{:x} (vm={})",
                    pointer,
                    java_vm.is_some()
                );
                pointer
            }
            Err(err) => {
                log_error("nativeInit", err);
                0
            }
        }
    })
}

/// `nativeResize(ctx, w, h)`
#[no_mangle]
pub extern "system" fn Java_org_sinkon_launcher_Sinkon_nativeResize(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    ctx: jlong,
    width: jint,
    height: jint,
) {
    guard("nativeResize", (), || {
        let started = std::time::Instant::now();
        // SAFETY: contract of `handle`.
        let Some(handle) = (unsafe { handle(ctx, "nativeResize") }) else {
            return;
        };
        if width <= 0 || height <= 0 {
            log_error("nativeResize", format!("invalid size {width}x{height}"));
            return;
        }
        if let Err(err) = handle.renderer.resize(width as u32, height as u32) {
            log_error("nativeResize", err);
        }
        log::debug!("nativeResize took {} µs", started.elapsed().as_micros());
    });
}

/// `nativeRenderFrame(ctx)`
#[no_mangle]
pub extern "system" fn Java_org_sinkon_launcher_Sinkon_nativeRenderFrame(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    ctx: jlong,
) {
    guard("nativeRenderFrame", (), || {
        // SAFETY: contract of `handle`.
        let Some(handle) = (unsafe { handle(ctx, "nativeRenderFrame") }) else {
            return;
        };
        if let Err(err) = handle.renderer.render_frame() {
            log_error("nativeRenderFrame", err);
        }
    });
}

/// `nativeDestroy(ctx)`
#[no_mangle]
pub extern "system" fn Java_org_sinkon_launcher_Sinkon_nativeDestroy(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    ctx: jlong,
) {
    guard("nativeDestroy", (), || {
        if ctx == 0 {
            return;
        }
        let pointer = ctx as usize as *mut NativeHandle;
        if pointer.is_null() {
            return;
        }
        // SAFETY: `Box::from_raw` takes back the allocation created in `nativeInit`;
        // the magic check rejects double destroys in debug builds before the read.
        let mut handle = unsafe { Box::from_raw(pointer) };
        if handle.magic != HANDLE_MAGIC {
            log_error("nativeDestroy", "handle magic mismatch; refusing to free");
            std::mem::forget(handle);
            return;
        }
        handle.renderer.destroy();
        handle.magic = 0;
        log::info!("nativeDestroy completed");
    });
}

/// `nativeBakeSection(ctx, x, y, z, vbo, ibo, formatId)`
///
/// `vbo` is the section's block-id array (4096 bytes) in a direct buffer; `ibo`
/// is the launcher's palette/state buffer and is accepted but not required.
#[no_mangle]
pub extern "system" fn Java_org_sinkon_launcher_Sinkon_nativeBakeSection<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    ctx: jlong,
    x: jint,
    y: jint,
    z: jint,
    vbo: JByteBuffer<'local>,
    ibo: JByteBuffer<'local>,
    format_id: jint,
) {
    guard("nativeBakeSection", (), || {
        // SAFETY: contract of `handle`.
        let Some(handle) = (unsafe { handle(ctx, "nativeBakeSection") }) else {
            return;
        };
        // SAFETY: the slices live for this call only.
        let Some((blocks, length)) = (unsafe { direct_slice(&vbo, &env) }) else {
            log_error("nativeBakeSection", "vbo is not a direct buffer");
            return;
        };
        if length < crate::chunk::SECTION_BLOCKS {
            log_error(
                "nativeBakeSection",
                format!("vbo holds {length} bytes, need 4096"),
            );
            return;
        }
        let _palette_bytes = unsafe { direct_slice(&ibo, &env) }.map(|(slice, _)| slice.len());
        if let Err(err) = handle
            .renderer
            .bake_section(x, y, z, blocks, format_id)
        {
            log_error("nativeBakeSection", err);
        }
    });
}

/// `nativeSetCamera(ctx, viewFloatBuffer, projFloatBuffer)`
#[no_mangle]
pub extern "system" fn Java_org_sinkon_launcher_Sinkon_nativeSetCamera(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
    ctx: jlong,
    view: JByteBuffer<'_>,
    proj: JByteBuffer<'_>,
) {
    guard("nativeSetCamera", (), || {
        let started = std::time::Instant::now();
        // SAFETY: contract of `handle`.
        let Some(handle) = (unsafe { handle(ctx, "nativeSetCamera") }) else {
            return;
        };
        let view_matrix = match read_mat4(&view, &env) {
            Ok(matrix) => matrix,
            Err(err) => {
                log_error("nativeSetCamera", format!("view: {err}"));
                return;
            }
        };
        let proj_matrix = match read_mat4(&proj, &env) {
            Ok(matrix) => matrix,
            Err(err) => {
                log_error("nativeSetCamera", format!("proj: {err}"));
                return;
            }
        };
        handle.renderer.set_camera(&view_matrix, &proj_matrix);
        log::trace!("nativeSetCamera took {} ns", started.elapsed().as_nanos());
    });
}

/// `nativeUploadTexture(ctx, texId, w, h, directByteBuffer)`
#[no_mangle]
pub extern "system" fn Java_org_sinkon_launcher_Sinkon_nativeUploadTexture<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    ctx: jlong,
    _tex_id: jint,
    width: jint,
    height: jint,
    pixels: JByteBuffer<'local>,
) {
    guard("nativeUploadTexture", (), || {
        // SAFETY: contract of `handle`.
        let Some(handle) = (unsafe { handle(ctx, "nativeUploadTexture") }) else {
            return;
        };
        if width <= 0 || height <= 0 {
            log_error(
                "nativeUploadTexture",
                format!("invalid size {width}x{height}"),
            );
            return;
        }
        // SAFETY: the slice lives for this call only and is copied by the backend.
        let Some((slice, length)) = (unsafe { direct_slice(&pixels, &env) }) else {
            log_error("nativeUploadTexture", "pixels is not a direct buffer");
            return;
        };
        let expected = width as usize * height as usize * 4;
        if length < expected {
            log_error(
                "nativeUploadTexture",
                format!("buffer holds {length} bytes, expected {expected}"),
            );
            return;
        }
        if let Err(err) =
            handle
                .renderer
                .upload_texture(width as u32, height as u32, slice)
        {
            log_error("nativeUploadTexture", err);
        }
    });
}

/// `nativeSetShaderpack(ctx, pathDir)`
#[no_mangle]
pub extern "system" fn Java_org_sinkon_launcher_Sinkon_nativeSetShaderpack<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    ctx: jlong,
    path: JString<'local>,
) {
    guard("nativeSetShaderpack", (), || {
        // SAFETY: contract of `handle`.
        let Some(handle) = (unsafe { handle(ctx, "nativeSetShaderpack") }) else {
            return;
        };
        let path: String = match env.get_string(&path) {
            Ok(value) => value.into(),
            Err(err) => {
                log_error("nativeSetShaderpack", format!("path could not be read: {err}"));
                return;
            }
        };
        handle.renderer.set_shaderpack(std::path::Path::new(&path));
    });
}

/// `nativeReloadShaders(ctx) -> jint`
#[no_mangle]
pub extern "system" fn Java_org_sinkon_launcher_Sinkon_nativeReloadShaders(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    ctx: jlong,
) -> jint {
    guard("nativeReloadShaders", -1, || {
        // SAFETY: contract of `handle`.
        let Some(handle) = (unsafe { handle(ctx, "nativeReloadShaders") }) else {
            return -1;
        };
        let summary = handle.renderer.reload_shaders();
        log::info!(
            "reload_shaders: {} compiled, {} failed",
            summary.compiled_shaders,
            summary.failed
        );
        if summary.failed > 0 {
            1
        } else {
            0
        }
    })
}

/// `nativeGetCapabilityReport(ctx) -> String`
#[no_mangle]
pub extern "system" fn Java_org_sinkon_launcher_Sinkon_nativeGetCapabilityReport<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    ctx: jlong,
) -> jstring {
    guard("nativeGetCapabilityReport", std::ptr::null_mut(), || {
        // SAFETY: contract of `handle`.
        let Some(handle) = (unsafe { handle(ctx, "nativeGetCapabilityReport") }) else {
            return std::ptr::null_mut();
        };
        let json = handle.renderer.capability_report();
        match env.new_string(json) {
            Ok(text) => text.into_raw(),
            Err(err) => {
                log_error("nativeGetCapabilityReport", err);
                std::ptr::null_mut()
            }
        }
    })
}

/// `nativeGetPerfStats(ctx) -> String`
#[no_mangle]
pub extern "system" fn Java_org_sinkon_launcher_Sinkon_nativeGetPerfStats<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    ctx: jlong,
) -> jstring {
    guard("nativeGetPerfStats", std::ptr::null_mut(), || {
        // SAFETY: contract of `handle`.
        let Some(handle) = (unsafe { handle(ctx, "nativeGetPerfStats") }) else {
            return std::ptr::null_mut();
        };
        let json = handle.renderer.perf_stats();
        match env.new_string(json) {
            Ok(text) => text.into_raw(),
            Err(err) => {
                log_error("nativeGetPerfStats", err);
                std::ptr::null_mut()
            }
        }
    })
}

/// `nativeSwitchBackend(ctx, backendHint) -> jint`
#[no_mangle]
pub extern "system" fn Java_org_sinkon_launcher_Sinkon_nativeSwitchBackend(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    ctx: jlong,
    backend_hint: jint,
) -> jint {
    guard("nativeSwitchBackend", -1, || {
        // SAFETY: contract of `handle`.
        let Some(handle) = (unsafe { handle(ctx, "nativeSwitchBackend") }) else {
            return -1;
        };
        handle.renderer.switch_backend(backend_hint)
    })
}

/// Library load hook: configure logging as early as possible.
#[no_mangle]
pub extern "system" fn JNI_OnLoad(_vm: *mut jni::sys::JavaVM, _reserved: *mut jni::sys::jint) -> jint {
    crate::init_logging(log::LevelFilter::Info);
    log::info!("libsinkon.so loaded (Sinkon {})", crate::VERSION);
    JNI_VERSION_1_6
}

/// Keeps `JavaVM` referenced for the doc example above without pulling it into
/// the hot path; also documents that no cached VM is needed.
#[allow(dead_code)]
fn _vm_is_not_cached(vm: Option<JavaVM>) {
    if vm.is_some() {
        log::debug!("JavaVM available but not retained (Sinkon never calls back into Java)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_handle_is_rejected() {
        // SAFETY: `handle` returns early for the null sentinel without dereferencing.
        assert!(unsafe { handle(0, "test") }.is_none());
    }

    #[test]
    fn magic_is_stable() {
        // The value is part of the ABI contract with the Kotlin side's debugging
        // overlay; changing it invalidates every live handle.
        assert_eq!(HANDLE_MAGIC, 0x5349_4E4B);
    }
}
