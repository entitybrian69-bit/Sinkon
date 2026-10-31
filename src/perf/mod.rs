//! Performance instrumentation: device tier detection, frame-time histograms,
//! allocation gating and tracing spans.

pub mod alloc;
pub mod histogram;
pub mod tier;
pub mod tracing;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::Serialize;

use crate::backend::BackendKind;
use histogram::FrameHistogram;
use tier::{DeviceTier, TierBudget};

/// Static, per-backend frame budget for one tier.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct BackendBudget {
    /// Target frames per second.
    pub target_fps: u32,
    /// Main-thread submission budget in microseconds.
    pub main_thread_budget_us: u32,
    /// Recommended render distance in chunks (radius).
    pub render_distance: u8,
}

/// Everything the perf report needs for one frame.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct FrameCounters {
    /// Frames submitted.
    pub frames: u64,
    /// Draw batches submitted.
    pub batches: u64,
    /// Chunk sections uploaded.
    pub sections_uploaded: u64,
    /// Sections culled this frame (last value).
    pub sections_culled: u64,
    /// Sections drawn this frame (last value).
    pub sections_drawn: u64,
    /// Bytes uploaded to the GPU.
    pub bytes_uploaded: u64,
}

/// Owns every counter and histogram Sinkon exposes.
///
/// One instance lives for the lifetime of a `SinkonRenderer`; the counters are
/// plain atomics so worker threads can bump them without a lock and without
/// touching the allocator.
pub struct PerfRegistry {
    backend: BackendKind,
    tier: DeviceTier,
    budget: TierBudget,
    submission: FrameHistogram,
    present: FrameHistogram,
    counters: FrameCounters,
    frame_allocations: AtomicU64,
    jni_calls: AtomicU64,
    jni_total_ns: AtomicU64,
    bind_group_creations: AtomicU64,
    buffer_creations: AtomicU64,
    program_creations: AtomicU64,
    pipeline_cache_hits: AtomicU64,
    pipeline_cache_misses: AtomicU64,
    vram_bytes: AtomicU64,
    resident_sections: AtomicU64,
    peaks: FrameCounters,
    started: Instant,
}

impl PerfRegistry {
    /// Create the registry for an active backend/tier pair.
    pub fn new(backend: BackendKind, tier: DeviceTier) -> PerfRegistry {
        PerfRegistry {
            backend,
            tier,
            budget: tier.budget(),
            submission: FrameHistogram::new(),
            present: FrameHistogram::new(),
            counters: FrameCounters::default(),
            frame_allocations: AtomicU64::new(0),
            jni_calls: AtomicU64::new(0),
            jni_total_ns: AtomicU64::new(0),
            bind_group_creations: AtomicU64::new(0),
            buffer_creations: AtomicU64::new(0),
            program_creations: AtomicU64::new(0),
            pipeline_cache_hits: AtomicU64::new(0),
            pipeline_cache_misses: AtomicU64::new(0),
            vram_bytes: AtomicU64::new(0),
            resident_sections: AtomicU64::new(0),
            peaks: FrameCounters::default(),
            started: Instant::now(),
        }
    }

    /// Active backend.
    pub fn backend(&self) -> BackendKind {
        self.backend
    }

    /// Detected device tier.
    pub fn tier(&self) -> DeviceTier {
        self.tier
    }

    /// Budget for this tier.
    pub fn budget(&self) -> TierBudget {
        self.budget
    }

    /// Budget for the active backend at this tier, if the combination is
    /// supported at all.
    pub fn backend_budget(&self) -> Option<BackendBudget> {
        self.budget.for_backend(self.backend)
    }

    /// Record a completed frame: main-thread submission time and present time.
    pub fn record_frame(&mut self, submission_us: u32, present_us: u32) {
        self.submission.record(submission_us);
        self.present.record(present_us);
        self.counters.frames += 1;
        self.peaks.frames = self.counters.frames;
        if self.counters.frames == 1 {
            log::info!(
                "first frame presented {} ms after init (backend={}, tier={})",
                self.started.elapsed().as_millis(),
                self.backend,
                self.tier
            );
        }
    }

    /// Bump the draw-batch counter for this frame.
    pub fn add_batches(&mut self, count: u32) {
        self.counters.batches += u64::from(count);
        self.peaks.batches = self.counters.batches;
    }

    /// Record one chunk upload.
    pub fn add_upload(&mut self, bytes: u64) {
        self.counters.sections_uploaded += 1;
        self.counters.bytes_uploaded += bytes;
        self.peaks.sections_uploaded = self.counters.sections_uploaded;
        self.peaks.bytes_uploaded = self.counters.bytes_uploaded;
    }

    /// Record the cull result for this frame.
    pub fn set_cull(&mut self, drawn: u32, culled: u32) {
        self.counters.sections_drawn = u64::from(drawn);
        self.counters.sections_culled = u64::from(culled);
        self.peaks.sections_drawn = self.counters.sections_drawn;
        self.peaks.sections_culled = self.counters.sections_culled;
    }

    /// Record allocations observed during the last `render_frame` call.
    pub fn set_frame_allocations(&mut self, allocations: u64) {
        self.frame_allocations
            .store(allocations, Ordering::Relaxed);
    }

    /// Count one JNI crossing and its duration.
    pub fn jni_call(&self, nanos: u64) {
        self.jni_calls.fetch_add(1, Ordering::Relaxed);
        self.jni_total_ns.fetch_add(nanos, Ordering::Relaxed);
    }

    /// Count a bind group / program / descriptor-set creation.
    pub fn bind_group_created(&self) {
        self.bind_group_creations.fetch_add(1, Ordering::Relaxed);
    }

    /// Count a buffer creation.
    pub fn buffer_created(&self) {
        self.buffer_creations.fetch_add(1, Ordering::Relaxed);
    }

    /// Count a shader/pipeline program creation.
    pub fn program_created(&self) {
        self.program_creations.fetch_add(1, Ordering::Relaxed);
    }

    /// Count a pipeline-cache hit.
    pub fn pipeline_cache_hit(&self) {
        self.pipeline_cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Count a pipeline-cache miss.
    pub fn pipeline_cache_miss(&self) {
        self.pipeline_cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Publish VRAM usage.
    pub fn set_vram(&self, bytes: u64) {
        self.vram_bytes.store(bytes, Ordering::Relaxed);
    }

    /// Publish resident chunk-section count.
    pub fn set_resident_sections(&self, sections: u64) {
        self.resident_sections.store(sections, Ordering::Relaxed);
    }

    /// Snapshot for the JSON report.
    pub fn stats(&self) -> PerfSnapshot {
        let hit = self.pipeline_cache_hits.load(Ordering::Relaxed);
        let miss = self.pipeline_cache_misses.load(Ordering::Relaxed);
        PerfSnapshot {
            backend: self.backend,
            tier: self.tier,
            frames: self.counters.frames,
            target_fps: self.backend_budget().map(|b| b.target_fps).unwrap_or(0),
            main_thread_budget_us: self
                .backend_budget()
                .map(|b| b.main_thread_budget_us)
                .unwrap_or(0),
            submission: self.submission.snapshot(),
            present: self.present.snapshot(),
            counters: self.counters,
            frame_allocations: self.frame_allocations.load(Ordering::Relaxed),
            jni_calls: self.jni_calls.load(Ordering::Relaxed),
            jni_mean_ns: {
                let calls = self.jni_calls.load(Ordering::Relaxed);
                if calls == 0 {
                    0
                } else {
                    self.jni_total_ns.load(Ordering::Relaxed) / calls
                }
            },
            bind_group_creations: self.bind_group_creations.load(Ordering::Relaxed),
            buffer_creations: self.buffer_creations.load(Ordering::Relaxed),
            program_creations: self.program_creations.load(Ordering::Relaxed),
            pipeline_cache_hit_rate: if hit + miss == 0 {
                0.0
            } else {
                hit as f64 / (hit + miss) as f64
            },
            vram_bytes: self.vram_bytes.load(Ordering::Relaxed),
            resident_sections: self.resident_sections.load(Ordering::Relaxed),
            uptime_ms: self.started.elapsed().as_millis() as u64,
        }
    }

    /// JSON form returned by `nativeGetPerfStats`.
    pub fn stats_json(&self) -> String {
        let snapshot = self.stats();
        serde_json::to_string(&snapshot).unwrap_or_else(|_| {
            format!(
                "{{\"active_backend\":\"{}\",\"error\":\"perf serialise failed\"}}",
                self.backend.as_str()
            )
        })
    }
}

/// Flat snapshot consumed by `nativeGetPerfStats` and the debug overlay.
#[derive(Clone, Debug, Serialize)]
pub struct PerfSnapshot {
    /// Active backend.
    pub backend: BackendKind,
    /// Device tier.
    pub tier: DeviceTier,
    /// Frames presented since init.
    pub frames: u64,
    /// Tier target FPS for this backend (0 when the combination is unsupported).
    pub target_fps: u32,
    /// Tier main-thread budget in microseconds.
    pub main_thread_budget_us: u32,
    /// Main-thread submission histogram.
    pub submission: histogram::HistogramSnapshot,
    /// Present histogram.
    pub present: histogram::HistogramSnapshot,
    /// Aggregate counters.
    pub counters: FrameCounters,
    /// Allocations observed in the last `nativeRenderFrame` (0 is the target).
    pub frame_allocations: u64,
    /// JNI crossings counted.
    pub jni_calls: u64,
    /// Mean JNI crossing duration in nanoseconds.
    pub jni_mean_ns: u64,
    /// Bind group / descriptor set creations since init.
    pub bind_group_creations: u64,
    /// Buffer creations since init.
    pub buffer_creations: u64,
    /// Shader program / pipeline creations since init.
    pub program_creations: u64,
    /// Fraction of pipeline cache lookups that hit.
    pub pipeline_cache_hit_rate: f64,
    /// GPU bytes owned by Sinkon.
    pub vram_bytes: u64,
    /// Resident chunk sections.
    pub resident_sections: u64,
    /// Milliseconds since init.
    pub uptime_ms: u64,
}

/// Measures a scope in microseconds and hands the value to a histogram sink.
pub struct Scope<'a> {
    start: Instant,
    sink: Option<&'a mut FrameHistogram>,
}

impl<'a> Scope<'a> {
    /// Start a measurement.
    pub fn new(sink: Option<&'a mut FrameHistogram>) -> Scope<'a> {
        Scope {
            start: Instant::now(),
            sink,
        }
    }

    /// Elapsed microseconds without consuming the scope.
    pub fn elapsed_us(&self) -> u32 {
        self.start.elapsed().as_micros().min(u64::from(u32::MAX)) as u32
    }

    /// Finish and record.
    pub fn finish(self) -> u32 {
        let us = self.elapsed_us();
        if let Some(sink) = self.sink {
            sink.record(us);
        }
        us
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_serialises() {
        let mut registry = PerfRegistry::new(BackendKind::Vulkan, DeviceTier::S);
        registry.record_frame(900, 120);
        registry.add_batches(37);
        registry.add_upload(4096);
        registry.set_cull(30, 7);
        registry.jni_call(1_500);
        let json = registry.stats_json();
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["frames"], 1);
        assert_eq!(value["backend"], "vulkan");
        assert_eq!(value["submission"]["p50_us"], 900);
    }

    #[test]
    fn scope_measures_something() {
        let mut hist = FrameHistogram::new();
        let scope = Scope::new(Some(&mut hist));
        std::thread::yield_now();
        let us = scope.finish();
        assert_eq!(hist.count(), 1);
        assert_eq!(hist.max_us(), us);
    }
}
