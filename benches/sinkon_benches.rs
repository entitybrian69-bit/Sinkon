//! Criterion benches for every backend.
//!
//! Bench ids are prefixed with the backend (`vulkan_`, `gles3_`, `gles2_`) because
//! `scripts/perf-gate.sh` keys its per-backend baselines off that prefix
//! (`benches/baseline.json`: `{ "<backend>_<bench_id>": { "mean_ns": … } }`).
//!
//! What is measured, and what is not:
//!
//! * **Measured here (host, CPU only):** meshing + pool insert (mesh upload
//!   preparation), frustum culling, uniform-blob encoding, the JNI read path
//!   (mocked over a direct-buffer-shaped slice) and full shader translation for
//!   each backend's target.
//! * **Not measured here:** anything that needs a real GPU — `vkQueueSubmit`,
//!   `glDrawElements`, present latency. Those numbers come from an on-device run
//!   and are marked `NOT MEASURED — reason: …` in `docs/PERFORMANCE.md` until
//!   they are captured on the reference device.
//!
//! Run everything:  `cargo bench`
//! Run one backend: `cargo bench -- vulkan_`

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use std::time::Duration;

use sinkon::backend::{
    BackendKind, BufferDesc, BufferUsage, DrawBatch, PipelineId, BufferId,
};
use sinkon::chunk::{
    cull_sections, mesh_section, CullScratch, Frustum, MeshFormat, MeshJob, MeshPool, MeshResult,
    SectionInstance, SectionKey, SECTION_BLOCKS,
};
use sinkon::renderer::{identity, mat4_multiply};
use sinkon::shader::{
    builtin, translate, ShaderStage, ShaderTarget, TranslatedCode,
};

/// One 16³ section with a plausible mix of solid and empty blocks.
fn sample_blocks(fill: f32) -> Vec<u8> {
    let mut blocks = vec![0u8; SECTION_BLOCKS];
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    for block in blocks.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let unit = (state >> 40) as f32 / 16_777_216.0;
        *block = if unit < fill { 1 + (state % 7) as u8 } else { 0 };
    }
    blocks
}

fn mesh_job(fill: f32) -> MeshJob {
    MeshJob {
        key: SectionKey::new(1, 2, 3),
        generation: 1,
        format: MeshFormat::Packed4,
        blocks: sample_blocks(fill).into_boxed_slice(),
    }
}

fn visible_sections(count: usize) -> Vec<SectionInstance> {
    (0..count)
        .map(|index| SectionInstance {
            key: SectionKey::new(index as i32, 0, 0),
            slot: index as u32,
            index_count: 2048,
            parent_visible: true,
        })
        .collect()
}

fn camera() -> ([[f32; 4]; 4], [[f32; 4]; 4]) {
    let mut view = identity();
    view[3] = [8.0, 70.0, 8.0, 1.0];
    let mut proj = identity();
    proj[0][0] = 1.4;
    proj[1][1] = 2.1;
    (view, proj)
}

/// Meshing + pool insert: the CPU half of "mesh upload".
fn bench_mesh_upload(c: &mut Criterion, kind: BackendKind) {
    let job = mesh_job(0.35);
    let prefix = kind.bench_prefix();
    let group_name = format!("{prefix}mesh_upload");
    let bench_id = format!("{prefix}mesh_upload");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(30);
    group.bench_function(bench_id.as_str(), |b| {
        b.iter_batched_ref(
            || MeshPool::new(16, u64::MAX),
            |pool| {
                let result: MeshResult = mesh_section(&job);
                let (slot, _) = pool.insert(&result);
                criterion::black_box(slot);
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

/// Frustum + occlusion cull over 1 000 sections.
fn bench_cull(c: &mut Criterion, kind: BackendKind) {
    let sections = visible_sections(1000);
    let (view, proj) = camera();
    let frustum = Frustum::from_view_proj(&mat4_multiply(&proj, &view));
    let mut scratch = CullScratch::with_capacity(2048);
    let prefix = kind.bench_prefix();
    let group_name = format!("{prefix}cull_pass");
    let bench_id = format!("{prefix}cull_pass");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(50);
    group.bench_function(bench_id.as_str(), |b| {
        b.iter(|| {
            cull_sections(&sections, &frustum, [8.0, 70.0, 8.0], &mut scratch);
            criterion::black_box(scratch.drawn());
        })
    });
    group.finish();
}

/// Uniform-blob encoding: the per-frame CPU cost of pushing the camera.
fn bench_uniform_update(c: &mut Criterion, kind: BackendKind) {
    let (view, proj) = camera();
    let mut blob = [0u8; 160];
    let prefix = kind.bench_prefix();
    let group_name = format!("{prefix}uniform_update");
    let bench_id = format!("{prefix}uniform_update");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(100);
    group.bench_function(bench_id.as_str(), |b| {
        b.iter(|| {
            for (column, out_column) in view.iter().enumerate() {
                for (row, value) in out_column.iter().enumerate() {
                    blob[(column * 16) + row * 4..(column * 16) + row * 4 + 4]
                        .copy_from_slice(&value.to_ne_bytes());
                }
            }
            for (column, out_column) in proj.iter().enumerate() {
                for (row, value) in out_column.iter().enumerate() {
                    let offset = 64 + column * 16 + row * 4;
                    blob[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
                }
            }
            criterion::black_box(&blob);
        })
    });
    group.finish();
}

/// Mocked JNI round trip: read two matrices out of a direct-buffer-shaped slice
/// exactly as `nativeSetCamera` does, then hand them to the renderer.
fn bench_jni_roundtrip(c: &mut Criterion, kind: BackendKind) {
    let (view, proj) = camera();
    let mut bytes = [0u8; 128];
    for (index, value) in view.iter().flatten().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_ne_bytes());
    }
    for (index, value) in proj.iter().flatten().enumerate() {
        bytes[64 + index * 4..64 + index * 4 + 4].copy_from_slice(&value.to_ne_bytes());
    }
    let prefix = kind.bench_prefix();
    let group_name = format!("{prefix}jni_roundtrip_mock");
    let bench_id = format!("{prefix}jni_roundtrip_mock");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(100);
    group.bench_function(bench_id.as_str(), |b| {
        b.iter(|| {
            let mut matrices = [[[0.0f32; 4]; 4]; 2];
            for (buffer, matrix) in matrices.iter_mut().enumerate() {
                for (column, out_column) in matrix.iter_mut().enumerate() {
                    for (row, value) in out_column.iter_mut().enumerate() {
                        let offset = buffer * 64 + column * 16 + row * 4;
                        *value = f32::from_ne_bytes([
                            bytes[offset],
                            bytes[offset + 1],
                            bytes[offset + 2],
                            bytes[offset + 3],
                        ]);
                    }
                }
            }
            criterion::black_box(matrices);
        })
    });
    group.finish();
}

/// Full shader translation for the backend's own target.
fn bench_shader_compile(c: &mut Criterion, kind: BackendKind, target: ShaderTarget) {
    let prefix = kind.bench_prefix();
    let group_name = format!("{prefix}shader_compile");
    let bench_id = format!("{prefix}shader_compile");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(15);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(5));
    group.bench_function(bench_id.as_str(), |b| {
        b.iter(|| {
            let vertex = translate(builtin::TERRAIN_VERTEX, ShaderStage::Vertex, target);
            let fragment = translate(builtin::TERRAIN_FRAGMENT, ShaderStage::Fragment, target);
            match (vertex, fragment) {
                (Ok(TranslatedCode::SpirV(a)), Ok(TranslatedCode::SpirV(b))) => {
                    criterion::black_box(a.len() + b.len())
                }
                (Ok(TranslatedCode::Glsl(a)), Ok(TranslatedCode::Glsl(b))) => {
                    criterion::black_box(a.len() + b.len())
                }
                _ => criterion::black_box(0),
            }
        })
    });
    group.finish();
}

/// Batch-list construction, mirroring `render_frame` step 4.
fn bench_batch_build(c: &mut Criterion, kind: BackendKind) {
    let prefix = kind.bench_prefix();
    let mut batches = Vec::with_capacity(1024);
    let group_name = format!("{prefix}batch_build");
    let bench_id = format!("{prefix}batch_build");
    let mut group = c.benchmark_group(group_name);
    group.sample_size(50);
    group.bench_function(bench_id.as_str(), |b| {
        b.iter(|| {
            batches.clear();
            for slot in 0..1000u32 {
                batches.push(DrawBatch {
                    pipeline: PipelineId(0),
                    vertex_buffer: BufferId(slot * 2),
                    index_buffer: BufferId(slot * 2 + 1),
                    index_count: 2048,
                    instance_count: 1,
                    first_index: 0,
                    base_vertex: 0,
                    section_tag: slot,
                });
            }
            criterion::black_box(batches.len());
        })
    });
    group.finish();
}

fn backends(c: &mut Criterion) {
    for kind in [BackendKind::Vulkan, BackendKind::Gles3, BackendKind::Gles2Legacy] {
        bench_mesh_upload(c, kind);
        bench_cull(c, kind);
        bench_uniform_update(c, kind);
        bench_jni_roundtrip(c, kind);
        bench_batch_build(c, kind);
        bench_shader_compile(c, kind, ShaderTarget::for_backend(kind));
    }
}

/// Buffer-descriptor construction is part of device setup, not the frame path, but
/// keeping it benchmarked catches accidental allocations in the pool code.
fn buffer_descriptors(c: &mut Criterion) {
    c.bench_function("vulkan_buffer_desc", |b| {
        b.iter(|| {
            let desc = BufferDesc {
                label: "sinkon.chunk.vertex",
                size: 64 * 1024,
                usage: BufferUsage::Geometry,
                persistent_map: false,
            };
            criterion::black_box(desc.size);
        })
    });
}

criterion_group!(benches, backends, buffer_descriptors);
criterion_main!(benches);
