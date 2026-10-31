# SINKON_PERF_REPORT

**Build:** `sinkon` 0.1.0 · branch `arena/37fccb25-sinkon` · target `aarch64-linux-android`
(primary), `armv7-linux-androideabi`, `x86_64-linux-android` (secondary)

**Verdict: ⚠️ no number in this report was measured.** This repository was produced
in a sandbox with no Android device, no GPU, no Rust toolchain (crates.io and
`static.rust-lang.org` are unreachable from it) and no NDK/Gradle. Every field that
needs a run reads `NOT MEASURED — reason: …`. Nothing is estimated; nothing is
extrapolated. The *harness* that will produce these numbers is implemented, and its
correctness is described at the end of this file.

---

## 1. Frame time per tier, per backend

Targets come from `src/perf/tier.rs::budget()` (Tier S/A/B/C × backend). Measured
columns are filled by `PerfSnapshot::submission` / `::present`
(`nativeGetPerfStats`).

### Vulkan

| Tier | Target FPS | Budget (main thread) | P50 | P95 | P99 |
| --- | --- | --- | --- | --- | --- |
| S | 120 | 2000 µs | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |
| A | 90 | 2500 µs | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |
| B | 60 | 4000 µs | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |
| C | 30 | 8000 µs | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |

### GLES 3.x

| Tier | Target FPS | Budget (main thread) | P50 | P95 | P99 |
| --- | --- | --- | --- | --- | --- |
| S | 90 | 2500 µs | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |
| A | 70 | 3000 µs | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |
| B | 45 | 5000 µs | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |
| C | 30 | 8000 µs | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |

### GLES 2.0 legacy

| Tier | Target FPS | Budget (main thread) | P50 | P95 | P99 |
| --- | --- | --- | --- | --- | --- |
| B | 30 | 8000 µs | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |
| C | 20 | 12000 µs | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |

## 2. Allocations per frame

| Backend | `nativeRenderFrame` | `nativeBakeSection` | `nativeUploadTexture` |
| --- | --- | --- | --- |
| Vulkan | NOT MEASURED — reason: `cargo test --features alloc-gate` cannot run (no toolchain) | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |
| GLES 3.x | NOT MEASURED — reason: no toolchain | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |
| GLES 2.0 | NOT MEASURED — reason: no toolchain | NOT MEASURED — reason: no device | NOT MEASURED — reason: no device |

The per-frame path is allocation-free *by construction*: every buffer it touches
(`FrameScratch::{sections, batches, drained}`, `CullScratch::visible`, the 160-byte
uniform blob) is preallocated at init and only `clear()`/`push()`ed afterwards; the
`alloc-gate` feature turns any violation into a failing test. "By construction" is
not a measurement, hence the row above.

## 3. JNI calls per frame

| Backend | Calls | Mean per call | Budget |
| --- | --- | --- | --- |
| Vulkan | 2 (`nativeSetCamera`, `nativeRenderFrame`) | NOT MEASURED — reason: no device | ≤ 50 µs/frame total |
| GLES 3.x | 2 | NOT MEASURED — reason: no device | ≤ 50 µs/frame total |
| GLES 2.0 | 2 | NOT MEASURED — reason: no device | ≤ 50 µs/frame total |

Two crossings per frame is the design: camera push + frame render. Chunk data is
batched outside the frame (one `nativeBakeSection` per section change, never per
frame), textures are pushed only when the atlas changes, and `nativeResize` only on
surface changes. No `GetFieldID`/`GetMethodID`/`NewObject` happens after
`nativeInit`, and every buffer is a *direct* buffer consumed with
`GetDirectBufferAddress`.

## 4. VRAM

| Backend | Budget | Measured |
| --- | --- | --- |
| Vulkan | ≤ 512 MB (atlas ≤ 512 MB, mesh pool per tier) | NOT MEASURED — reason: no device; atlas path is RGBA8-only (SINKON-013) |
| GLES 3.x | ≤ 512 MB | NOT MEASURED — reason: no device |
| GLES 2.0 | not budgeted | NOT MEASURED — reason: no device |

`capabilities.vram_bytes` and `meshes.resident` are the counters; the chunk pool is
hard-capped with LRU eviction (`MeshPoolStats::evictions`).

## 5. Startup latency

| Backend | Init → first frame | Budget |
| --- | --- | --- |
| Vulkan (Tier S / B) | NOT MEASURED — reason: no device | ≤ 400 ms / ≤ 900 ms |
| GLES 3.x (Tier S) | NOT MEASURED — reason: no device | ≤ 400 ms |
| GLES 2.0 (Tier B) | NOT MEASURED — reason: no device | ≤ 900 ms |
| Backend selection | NOT MEASURED — reason: no device | ≤ 100 ms |

## 6. Gate status

```
$ ./scripts/perf-gate.sh --backends=vulkan,gles3,gles2
[sinkon-perf-gate] No baseline found at benches/baseline.json — run with --accept-baseline to create one.
$ echo $?
0
```

That is the real, current output: `benches/baseline.json` is `{}` because criterion
has never run. The gate script itself is verified — see §7.

## 7. What *is* verified (tooling, not hardware)

The gate and the harness were exercised in the sandbox with a stub `cargo` that
emits `cargo-criterion`-shaped NDJSON (synthetic means; **not** performance data):

| Scenario | Expected | Actual |
| --- | --- | --- |
| No baseline, no `--strict` | message + exit 0 | ✅ |
| No baseline with `--strict` | exit 1 | ✅ |
| `--accept-baseline` | writes `{"<id>": {"mean_ns": <u64>}}` per backend, exit 0 | ✅ |
| +10 % slower than baseline | every row FAIL, exit 1 | ✅ |
| +3 % slower (threshold 5) | rows WARN, exit 0 | ✅ |
| −10 % (faster) | rows PASS, exit 0 | ✅ |
| bench in current, absent from baseline | row NEW | ✅ |
| bench in baseline, absent from current | row REMOVED | ✅ |
| `--backends=vulkan` | gles2/gles3 rows SKIP, exit 0 | ✅ |
| missing `jq` / `cargo` | exit 2 with a clear message | ✅ |
| unknown option | exit 2 | ✅ |
| `--help` | full option list, exit 0 | ✅ |
| report file | Markdown table with the six contract columns | ✅ |

## 8. Reproduction

```sh
# host, no device required
cargo bench --bench sinkon_benches -- vulkan_
cargo bench --bench sinkon_benches -- gles3_
cargo bench --bench sinkon_benches -- gles2_
cargo test --features alloc-gate
./scripts/perf-gate.sh --strict --backends=vulkan,gles3,gles2
./scripts/perf-gate.sh --accept-baseline --backends=vulkan,gles3,gles2

# device
./scripts/build-android.sh
cd android && ./gradlew :sinkon-native:assembleDebug
adb logcat -s Sinkon | tee sinkon.logcat
# read SinkonSession.perfStats() / capabilityReport() from the debug overlay
```

Full detail — including the exact table structure, method and known bottlenecks —
is in `docs/PERFORMANCE.md`.
