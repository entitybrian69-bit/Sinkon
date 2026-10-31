//! Structured tracing.
//!
//! `tracing` spans are the primary instrumentation tool for Sinkon. On Android
//! the `Sinkon` log tag is installed by [`crate::init_logging`]; this module adds
//! a `tracing::Subscriber` that:
//!
//! * forwards trace *events* to `log` (so they land in logcat),
//! * times span lifetimes with lock-free atomics and aggregation, so the frame
//!   path can create spans without allocating,
//! * counts events per level for the debug overlay.
//!
//! With the `tracy` feature, `tracing-tracy` is installed instead and spans
//! appear in the Tracy timeline; the log forwarding then happens through Tracy's
//! own log integration.

use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// Number of span slots tracked concurrently.
const SLOTS: usize = 64;

/// Slot bookkeeping: start time in nanoseconds since process start.
static SPAN_START_NS: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
/// Slot bookkeeping: span name pointer (`&'static str` as ptr).
static SPAN_NAME_PTR: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
/// Slot bookkeeping: span name length.
static SPAN_NAME_LEN: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
/// Last span duration observed, in nanoseconds.
pub static LAST_SPAN_NS: AtomicU64 = AtomicU64::new(0);
/// Total span time observed, in nanoseconds.
pub static TOTAL_SPAN_NS: AtomicU64 = AtomicU64::new(0);
/// Trace events emitted.
pub static EVENT_COUNT: AtomicU64 = AtomicU64::new(0);
/// Warn/error events emitted.
pub static PROBLEM_COUNT: AtomicU64 = AtomicU64::new(0);

static PROCESS_START: once_cell::sync::Lazy<Instant> = once_cell::sync::Lazy::new(Instant::now);

fn now_ns() -> u64 {
    PROCESS_START.elapsed().as_nanos() as u64
}

fn slot_of(id: &Id) -> usize {
    (id.into_u64() as usize) & (SLOTS - 1)
}

/// Last span duration, for the debug overlay.
pub fn last_span() -> Duration {
    Duration::from_nanos(LAST_SPAN_NS.load(Ordering::Relaxed))
}

/// Accumulated span time.
pub fn total_span_time() -> Duration {
    Duration::from_nanos(TOTAL_SPAN_NS.load(Ordering::Relaxed))
}

/// Install the Sinkon subscriber.
///
/// Idempotent; a second call is a no-op. `level` is the maximum event level
/// forwarded to `log`.
pub fn init(level: log::Level) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        #[cfg(feature = "tracy")]
        {
            let _ = level;
            tracing_tracy::client::Client::start();
            log::info!("Tracy client started (feature \"tracy\")");
        }
        #[cfg(not(feature = "tracy"))]
        {
            let subscriber = LogSubscriber {
                max_level: level,
            };
            if tracing::subscriber::set_global_default(subscriber).is_err() {
                log::debug!("tracing subscriber already installed");
            }
        }
    });
}

/// Create a span for a subsystem.
///
/// ```no_run
/// let _span = sinkon::perf::tracing::span("chunk_upload");
/// ```
pub fn span(name: &'static str) -> tracing::Span {
    tracing::debug_span!("sinkon", subsystem = name)
}

/// Marker kept for API symmetry with the compiler's `Unpin` bounds.
struct LogSubscriber {
    max_level: log::Level,
}

/// Visitor that renders an event's fields into a `String`.
///
/// Only used for events (never for the per-frame span path), so the allocation
/// here cannot violate the zero-allocation rule.
struct FieldWriter {
    message: String,
}

impl Visit for FieldWriter {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.message.push_str(&format!(" {}={:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.message.push_str(&format!(" {}={}", field.name(), value));
        }
    }
}

impl Subscriber for LogSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        let level = match *metadata.level() {
            tracing::Level::ERROR => log::Level::Error,
            tracing::Level::WARN => log::Level::Warn,
            tracing::Level::INFO => log::Level::Info,
            tracing::Level::DEBUG => log::Level::Debug,
            tracing::Level::TRACE => log::Level::Trace,
        };
        level <= self.max_level
    }

    fn new_span(&self, span: &Attributes<'_>) -> Id {
        let id = Id::from_u64(next_span_id());
        let slot = slot_of(&id);
        let name = span.metadata().name();
        SPAN_NAME_PTR[slot].store(name.as_ptr() as usize, Ordering::Relaxed);
        SPAN_NAME_LEN[slot].store(name.len(), Ordering::Relaxed);
        SPAN_START_NS[slot].store(now_ns(), Ordering::Relaxed);
        id
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        EVENT_COUNT.fetch_add(1, Ordering::Relaxed);
        let metadata = event.metadata();
        let mut writer = FieldWriter {
            message: String::with_capacity(64),
        };
        event.record(&mut writer);
        let target = metadata.target();
        match *metadata.level() {
            tracing::Level::ERROR => {
                PROBLEM_COUNT.fetch_add(1, Ordering::Relaxed);
                log::error!(target: crate::LOG_TAG, "[{target}] {}", writer.message);
            }
            tracing::Level::WARN => {
                PROBLEM_COUNT.fetch_add(1, Ordering::Relaxed);
                log::warn!(target: crate::LOG_TAG, "[{target}] {}", writer.message);
            }
            tracing::Level::INFO => log::info!(target: crate::LOG_TAG, "[{target}] {}", writer.message),
            tracing::Level::DEBUG => log::debug!(target: crate::LOG_TAG, "[{target}] {}", writer.message),
            tracing::Level::TRACE => log::trace!(target: crate::LOG_TAG, "[{target}] {}", writer.message),
        }
    }

    fn enter(&self, span: &Id) {
        let slot = slot_of(span);
        SPAN_START_NS[slot].store(now_ns(), Ordering::Relaxed);
    }

    fn exit(&self, span: &Id) {
        let slot = slot_of(span);
        let start = SPAN_START_NS[slot].load(Ordering::Relaxed);
        let now = now_ns();
        let elapsed = now.saturating_sub(start);
        LAST_SPAN_NS.store(elapsed, Ordering::Relaxed);
        TOTAL_SPAN_NS.fetch_add(elapsed, Ordering::Relaxed);
        let ptr = SPAN_NAME_PTR[slot].load(Ordering::Relaxed) as *const u8;
        let len = SPAN_NAME_LEN[slot].load(Ordering::Relaxed);
        if !ptr.is_null() && len > 0 {
            // SAFETY: the pointer was stored from a `&'static str` in `new_span`
            // and `tracing` keeps metadata alive for the process lifetime, so the
            // bytes are valid, initialised and not mutated.
            let name = unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, len)) };
            log::trace!(target: crate::LOG_TAG, "span {name} took {elapsed} ns");
        }
    }
}

static NEXT_SPAN_ID: AtomicU64 = AtomicU64::new(1);

fn next_span_id() -> u64 {
    NEXT_SPAN_ID.fetch_add(1, Ordering::Relaxed)
}

/// Read a `&'static str` back out of a slot (used by the debug overlay).
pub fn span_name(slot: usize) -> Option<&'static str> {
    let ptr = SPAN_NAME_PTR[slot % SLOTS].load(Ordering::Relaxed) as *const u8;
    let len = SPAN_NAME_LEN[slot % SLOTS].load(Ordering::Relaxed);
    if ptr.is_null() || len == 0 {
        return None;
    }
    // SAFETY: as in `exit` — the pointer is a leaked `&'static str`.
    Some(unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, len)) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent() {
        init(log::Level::Debug);
        init(log::Level::Debug);
    }

    #[test]
    fn spans_and_events_are_counted() {
        init(log::Level::Trace);
        let before = EVENT_COUNT.load(Ordering::Relaxed);
        {
            let _span = span("unit_test");
            tracing::debug!("hello from a test");
        }
        assert!(EVENT_COUNT.load(Ordering::Relaxed) > before);
        assert!(total_span_time() > Duration::from_nanos(0));
    }
}
