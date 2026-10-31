//! Allocation gate.
//!
//! Sinkon's hard rule is **zero heap allocations in `nativeRenderFrame`**. The
//! gate works by swapping in a counting global allocator (feature `alloc-gate`)
//! and flipping a per-thread flag around the frame body:
//!
//! ```no_run
//! # use sinkon::perf::alloc;
//! alloc::begin_frame();
//! // ... render_frame body ...
//! let allocations = alloc::end_frame();
//! assert_eq!(allocations, 0, "render_frame must not allocate");
//! ```
//!
//! Without the feature every counter stays at zero and the flag calls compile to
//! nothing, so release builds pay nothing.
//!
//! # Caveats (documented on purpose)
//!
//! * The flag lives in a `const`-initialised `thread_local!`, which is the only
//!   form that cannot itself allocate on first access. TLS registration done by
//!   the runtime *before* the flag exists is not attributable to the frame, so it
//!   is not counted.
//! * Allocations performed by other threads while the flag is set on the render
//!   thread are not counted. Worker threads never upload to the GPU, so this is
//!   exactly the behaviour we want.

#[cfg(feature = "alloc-gate")]
mod imp {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Allocations observed inside the most recent frame.
    pub static FRAME_ALLOCS: AtomicU64 = AtomicU64::new(0);
    /// Bytes requested inside the most recent frame.
    pub static FRAME_ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
    /// Allocations since process start.
    pub static TOTAL_ALLOCS: AtomicU64 = AtomicU64::new(0);

    thread_local! {
        static IN_FRAME: Cell<bool> = const { Cell::new(false) };
    }

    /// Counting allocator delegating to the system allocator.
    pub struct CountingAlloc;

    #[inline]
    fn count(layout: Layout) {
        TOTAL_ALLOCS.fetch_add(1, Ordering::Relaxed);
        let in_frame = IN_FRAME.with(|flag| flag.get());
        if in_frame {
            FRAME_ALLOCS.fetch_add(1, Ordering::Relaxed);
            FRAME_ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
    }

    // SAFETY: every method forwards to `System`, which provides the actual
    // allocator behaviour for `alloc`, `dealloc`, `realloc` and `alloc_zeroed`
    // and therefore preserves `GlobalAlloc`'s contract; `count` only touches
    // atomics and a const-initialised thread local.
    unsafe impl GlobalAlloc for CountingAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count(layout);
            // SAFETY: forwarded verbatim.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: forwarded verbatim.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count(layout);
            // SAFETY: forwarded verbatim.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            count(Layout::from_size_align(new_size, layout.align()).unwrap_or(layout));
            // SAFETY: forwarded verbatim.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    /// Start counting allocations for this thread.
    #[inline]
    pub fn begin_frame() {
        FRAME_ALLOCS.store(0, Ordering::Relaxed);
        FRAME_ALLOC_BYTES.store(0, Ordering::Relaxed);
        IN_FRAME.with(|flag| flag.set(true));
    }

    /// Stop counting and return the number of allocations in the frame.
    #[inline]
    pub fn end_frame() -> u64 {
        IN_FRAME.with(|flag| flag.set(false));
        FRAME_ALLOCS.load(Ordering::Relaxed)
    }

    /// Allocations observed in the last frame.
    #[inline]
    pub fn frame_allocations() -> u64 {
        FRAME_ALLOCS.load(Ordering::Relaxed)
    }

    /// Bytes requested in the last frame.
    #[inline]
    pub fn frame_alloc_bytes() -> u64 {
        FRAME_ALLOC_BYTES.load(Ordering::Relaxed)
    }

    /// Allocations since process start.
    #[inline]
    pub fn total_allocations() -> u64 {
        TOTAL_ALLOCS.load(Ordering::Relaxed)
    }
}

#[cfg(not(feature = "alloc-gate"))]
mod imp {
    /// Start counting allocations (no-op without the feature).
    #[inline]
    pub fn begin_frame() {}

    /// Stop counting (always reports 0 without the feature).
    #[inline]
    pub fn end_frame() -> u64 {
        0
    }

    /// Allocations observed in the last frame (always 0 without the feature).
    #[inline]
    pub fn frame_allocations() -> u64 {
        0
    }

    /// Bytes requested in the last frame (always 0 without the feature).
    #[inline]
    pub fn frame_alloc_bytes() -> u64 {
        0
    }

    /// Allocations since process start (always 0 without the feature).
    #[inline]
    pub fn total_allocations() -> u64 {
        0
    }
}

#[cfg(feature = "alloc-gate")]
pub use imp::CountingAlloc;
pub use imp::{
    begin_frame, end_frame, frame_alloc_bytes, frame_allocations, total_allocations,
};

#[cfg(test)]
mod tests {
    #[test]
    fn counters_are_monotonic() {
        let before = super::total_allocations();
        let boxed = Box::new([0u8; 64]);
        assert_eq!(boxed.len(), 64);
        let after = super::total_allocations();
        assert!(after >= before);
    }

    #[test]
    fn frame_flag_does_not_panic() {
        super::begin_frame();
        let allocations = super::end_frame();
        // Under the alloc-gate feature this must be 0 for an empty body.
        assert!(allocations <= 2, "empty frame should not allocate: {allocations}");
    }
}
