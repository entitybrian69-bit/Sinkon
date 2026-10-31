//! Native window / surface plumbing.
//!
//! The launcher hands Sinkon a `android.view.Surface` (or `SurfaceHolder`)
//! through JNI. Vulkan and GLES both need the underlying `ANativeWindow`, so
//! this module owns the conversion, the reference counting and the
//! `raw-window-handle` adapters used by `wgpu`.
//!
//! # Ownership
//!
//! `ANativeWindow_fromSurface` returns a *referenced* window: the caller owns one
//! reference and must call `ANativeWindow_release`. [`SurfaceHandle`] models that
//! with an `owned` flag and releases in `Drop`. The handle is `Send` + `Sync`
//! because `ANativeWindow` reference counting is itself atomic on Android.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;

/// Where a backend should render to.
#[derive(Clone, Copy, Debug)]
pub enum SurfaceTarget<'a> {
    /// A real Android window, already converted from a Java `Surface`.
    AndroidNativeWindow {
        /// Referenced `ANativeWindow*`.
        window: NonNull<c_void>,
        /// Initial width in pixels.
        width: u32,
        /// Initial height in pixels.
        height: u32,
        /// Keeps `SurfaceTarget` lifetime-parametric without owning the window.
        marker: PhantomData<&'a c_void>,
    },
    /// Headless offscreen target, used by the benches and by host tests.
    Offscreen {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
}

impl<'a> SurfaceTarget<'a> {
    /// Build a target from a raw `ANativeWindow*`.
    ///
    /// # Safety
    ///
    /// `window` must be a valid `ANativeWindow*` that stays alive for as long as
    /// the target is used (i.e. at least until the backend is destroyed), and the
    /// caller must hold a reference to it.
    pub unsafe fn from_native_window(
        window: NonNull<c_void>,
        width: u32,
        height: u32,
    ) -> SurfaceTarget<'a> {
        SurfaceTarget::AndroidNativeWindow {
            window,
            width,
            height,
            marker: PhantomData,
        }
    }

    /// Headless target for tests/benches.
    pub fn offscreen(width: u32, height: u32) -> SurfaceTarget<'static> {
        SurfaceTarget::Offscreen { width, height }
    }

    /// Initial size in pixels.
    pub fn size(&self) -> (u32, u32) {
        match self {
            SurfaceTarget::AndroidNativeWindow { width, height, .. } => (*width, *height),
            SurfaceTarget::Offscreen { width, height } => (*width, *height),
        }
    }

    /// Raw window pointer, if this is a real Android window.
    pub fn native_window(&self) -> Option<NonNull<c_void>> {
        match self {
            SurfaceTarget::AndroidNativeWindow { window, .. } => Some(*window),
            SurfaceTarget::Offscreen { .. } => None,
        }
    }

    /// Short description for logs and capability reports.
    pub fn describe(&self) -> String {
        match self {
            SurfaceTarget::AndroidNativeWindow { width, height, .. } => {
                format!("android-window {width}x{height}")
            }
            SurfaceTarget::Offscreen { width, height } => format!("offscreen {width}x{height}"),
        }
    }
}

/// A referenced `ANativeWindow`.
#[derive(Debug)]
pub struct SurfaceHandle {
    window: NonNull<c_void>,
    owned: bool,
}

impl SurfaceHandle {
    /// Take ownership of an already-referenced `ANativeWindow*`.
    ///
    /// # Safety
    ///
    /// `window` must be a valid `ANativeWindow*` with one reference owned by the
    /// caller; ownership of that reference transfers to the returned handle.
    pub unsafe fn from_owned_raw(window: NonNull<c_void>) -> SurfaceHandle {
        SurfaceHandle {
            window,
            owned: true,
        }
    }

    /// Borrow a window without taking ownership.
    ///
    /// # Safety
    ///
    /// `window` must remain valid for the lifetime of this handle.
    pub unsafe fn borrow_raw(window: NonNull<c_void>) -> SurfaceHandle {
        SurfaceHandle {
            window,
            owned: false,
        }
    }

    /// Raw pointer for FFI.
    pub fn as_ptr(&self) -> *mut c_void {
        self.window.as_ptr()
    }

    /// Non-null pointer for `raw-window-handle`.
    pub fn as_non_null(&self) -> NonNull<c_void> {
        self.window
    }
}

// SAFETY: `ANativeWindow` reference counting is atomic (`ANativeWindow_acquire`
// / `_release`), and Sinkon only dereferences the pointer from the render thread
// while it owns the backend. Moving the handle between threads is therefore sound.
unsafe impl Send for SurfaceHandle {}
// SAFETY: as above; shared access is read-only (the pointer value).
unsafe impl Sync for SurfaceHandle {}

impl Drop for SurfaceHandle {
    fn drop(&mut self) {
        if self.owned && cfg!(target_os = "android") {
            // SAFETY: `self.window` came from `ANativeWindow_fromSurface` or an
            // `ANativeWindow_acquire` and we still hold exactly one reference.
            unsafe { release_native_window(self.window.as_ptr()) }
        }
    }
}

#[cfg(target_os = "android")]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    extern "C" {
        pub fn ANativeWindow_fromSurface(
            env: *mut c_void,
            surface: *mut c_void,
        ) -> *mut c_void;
        pub fn ANativeWindow_acquire(window: *mut c_void);
        pub fn ANativeWindow_release(window: *mut c_void);
        pub fn ANativeWindow_setBuffersGeometry(
            window: *mut c_void,
            width: c_int,
            height: c_int,
            format: c_int,
        ) -> c_int;
    }

    /// `WINDOW_FORMAT_RGBA_8888`.
    pub const WINDOW_FORMAT_RGBA_8888: c_int = 1;

    /// Best-effort logging helper for FFI strings; unused values are not read.
    pub fn cstr(ptr: *const c_char) -> &'static str {
        if ptr.is_null() {
            return "";
        }
        // SAFETY: callers only pass pointers to NUL-terminated, static driver
        // strings owned by the GL/Vulkan driver for the process lifetime.
        unsafe { std::ffi::CStr::from_ptr(ptr) }.to_str().unwrap_or("")
    }
}

#[cfg(target_os = "android")]
pub use ffi::WINDOW_FORMAT_RGBA_8888;

/// Convert a Java `Surface` into a referenced [`SurfaceHandle`].
///
/// # Safety
///
/// Must be called on a thread with a valid JNI `JNIEnv` that matches `env`, with
/// a live local/global reference to an `android.view.Surface`.
#[cfg(target_os = "android")]
pub unsafe fn from_java_surface(
    env: *mut c_void,
    surface: *mut c_void,
) -> Result<SurfaceHandle, String> {
    if surface.is_null() {
        return Err("surface reference is null".to_string());
    }
    // SAFETY: documented by this function; null is checked above.
    let window = unsafe { ffi::ANativeWindow_fromSurface(env, surface) };
    match NonNull::new(window) {
        // SAFETY: `ANativeWindow_fromSurface` returns a referenced window.
        Some(ptr) => Ok(unsafe { SurfaceHandle::from_owned_raw(ptr) }),
        None => Err("ANativeWindow_fromSurface returned null".to_string()),
    }
}

/// Hint the compositor about the buffer geometry/dimensions.
///
/// This is advisory only: the surface size is authoritative from
/// `nativeResize` / the swapchain extent.
pub fn set_buffers_geometry(handle: &SurfaceHandle, width: u32, height: u32) {
    #[cfg(target_os = "android")]
    {
        // SAFETY: `handle` holds a live `ANativeWindow*`.
        let rc = unsafe {
            ffi::ANativeWindow_setBuffersGeometry(
                handle.as_ptr(),
                width as i32,
                height as i32,
                ffi::WINDOW_FORMAT_RGBA_8888,
            )
        };
        if rc != 0 {
            log::warn!("ANativeWindow_setBuffersGeometry failed (rc={rc})");
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (handle, width, height);
    }
}

#[cfg(target_os = "android")]
fn release_native_window(window: *mut c_void) {
    // SAFETY: caller (Drop) holds exactly one reference to a valid window.
    unsafe { ffi::ANativeWindow_release(window) }
}

#[cfg(not(target_os = "android"))]
fn release_native_window(_window: *mut c_void) {}

/// `HasWindowHandle`/`HasDisplayHandle` adapter handed to `wgpu`.
///
/// Kept in one place so the (version-sensitive) `raw-window-handle` construction
/// is easy to audit against the pinned `wgpu` release.
#[derive(Clone, Copy, Debug)]
pub struct RawWindow(pub NonNull<c_void>);

// SAFETY: `ANativeWindow` is reference-counted with atomics; moving the pointer
// between threads is exactly what the NDK expects (`eglCreateWindowSurface`,
// `vkCreateAndroidSurfaceKHR`).
unsafe impl Send for RawWindow {}
// SAFETY: see above; `RawWindow` is an immutable pointer wrapper.
unsafe impl Sync for RawWindow {}

#[cfg(target_os = "android")]
impl raw_window_handle::HasWindowHandle for RawWindow {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        let handle = raw_window_handle::AndroidNdkWindowHandle::new(self.0);
        let raw = raw_window_handle::RawWindowHandle::AndroidNdk(handle);
        // SAFETY: the pointer is a live `ANativeWindow` owned by the caller for
        // the duration of the borrow.
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
    }
}

#[cfg(target_os = "android")]
impl raw_window_handle::HasDisplayHandle for RawWindow {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        let raw = raw_window_handle::RawDisplayHandle::Android(
            raw_window_handle::AndroidDisplayHandle::empty(),
        );
        // SAFETY: the Android display handle is a unit struct.
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(raw) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offscreen_target_describes_itself() {
        let target = SurfaceTarget::offscreen(640, 480);
        assert_eq!(target.size(), (640, 480));
        assert!(target.native_window().is_none());
        assert_eq!(target.describe(), "offscreen 640x480");
    }
}
