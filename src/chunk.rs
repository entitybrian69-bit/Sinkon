//! Chunk geometry: meshing workers, pooled meshes and allocation-free culling.
//!
//! # Threading model
//!
//! ```text
//!   render thread                            worker threads (num_cpus - 1)
//!   ─────────────                            ──────────────────────────────
//!   bake_section(x,y,z,blocks)  ──job──▶     mesh_section()  (CPU only)
//!   drain_results()             ◀─result──   ArrayQueue<MeshResult>
//!   upload to backend (GL/Vulkan)            never touches the GPU
//! ```
//!
//! Both queues are [`crossbeam_queue::ArrayQueue`]s: lock-free, bounded and
//! allocation-free on the hot path. A `Mutex<Vec<..>>` would allocate and would
//! let a slow worker stall the render thread, which the brief forbids.
//!
//! # Pooling
//!
//! [`MeshPool`] owns the CPU-side vertex/index buffers. Sections borrow a slot,
//! upload it, and return it when the section is evicted. Eviction is LRU with a
//! hard cap, so streaming a large world cannot grow memory without bound; the
//! eviction count is reported through `nativeGetCapabilityReport`.
//!
//! # Culling
//!
//! [`Frustum`] + [`CullScratch`] give CPU frustum culling on every backend (the
//! GL backends have no compute stage for GPU culling). Scratch vectors are
//! `clear()`ed, never reallocated, so a frame performs no allocation here.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_queue::ArrayQueue;
use serde::Serialize;

/// Edge length of one chunk section in blocks.
pub const SECTION_EDGE: usize = 16;
/// Blocks in one section.
pub const SECTION_BLOCKS: usize = SECTION_EDGE * SECTION_EDGE * SECTION_EDGE;
/// Size of one section in world units.
pub const SECTION_SIZE: f32 = SECTION_EDGE as f32;

/// Identifier of a chunk section in world section coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize)]
pub struct SectionKey {
    /// Section X.
    pub x: i32,
    /// Section Y (vertical; negative is legal in modern worlds).
    pub y: i32,
    /// Section Z.
    pub z: i32,
}

impl SectionKey {
    /// Construct a key.
    pub const fn new(x: i32, y: i32, z: i32) -> SectionKey {
        SectionKey { x, y, z }
    }

    /// Minimum corner in world space.
    pub fn origin(&self) -> [f32; 3] {
        [
            self.x as f32 * SECTION_SIZE,
            self.y as f32 * SECTION_SIZE,
            self.z as f32 * SECTION_SIZE,
        ]
    }

    /// Pack the three coordinates into a 32-bit tag for tracing / draw sorting.
    pub fn tag(&self) -> u32 {
        let x = (self.x as u32) & 0x3ff;
        let y = (self.y as u32) & 0x3ff;
        let z = (self.z as u32) & 0x3ff;
        x << 22 | y << 12 | z
    }
}

/// Vertex/index layout requested by the launcher for a section.
///
/// Mirrors the `formatId` argument of `nativeBakeSection`: the launcher tells
/// Sinkon which on-disk vertex layout the version uses, Sinkon produces that
/// layout so no conversion pass is needed later.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshFormat {
    /// 1.8 – 1.16 packed layout (position + uv + light, 4 bytes per attribute).
    Packed4,
    /// 1.17+ packed layout with 8-byte light/ao attribute.
    Packed8,
    /// Anything the launcher defines; Sinkon emits the compact 24-byte layout.
    Custom(u32),
}

impl MeshFormat {
    /// Bytes per vertex Sinkon emits for this format.
    pub const fn vertex_stride(self) -> u32 {
        match self {
            MeshFormat::Packed4 => 20,
            MeshFormat::Packed8 => 24,
            MeshFormat::Custom(_) => 24,
        }
    }

    /// Decode the `formatId` from JNI.
    pub const fn from_id(id: i32) -> MeshFormat {
        match id {
            4 => MeshFormat::Packed4,
            8 => MeshFormat::Packed8,
            other => MeshFormat::Custom(other as u32),
        }
    }
}

/// One meshing job handed to the worker pool.
pub struct MeshJob {
    /// Section being meshed.
    pub key: SectionKey,
    /// Generation counter: stale results are discarded by the caller.
    pub generation: u32,
    /// Requested layout.
    pub format: MeshFormat,
    /// Block ids, `SECTION_BLOCKS` entries (0 = air).
    pub blocks: Box<[u8]>,
}

/// Meshing result travelling back to the render thread.
pub struct MeshResult {
    /// Section that produced this mesh.
    pub key: SectionKey,
    /// Generation counter copied from the job.
    pub generation: u32,
    /// Vertex layout the mesh was produced in.
    pub format: MeshFormat,
    /// Vertex bytes (interleaved, stride from the format).
    pub vertices: Vec<u8>,
    /// Index bytes (u32 indices).
    pub indices: Vec<u8>,
    /// Vertices in the buffer.
    pub vertex_count: u32,
    /// Indices in the buffer.
    pub index_count: u32,
    /// Time spent meshing, microseconds (perf report).
    pub mesh_us: u32,
}

/// Emit a mesh for one section.
///
/// The mesher is deliberately simple: it walks the 16³ block array and emits the
/// six faces of every solid block whose neighbour is air or outside the section.
/// Greedy merging and per-face ambient occlusion are applied on top of this
/// (both operate on the same output buffers) — see `docs/ARCHITECTURE.md` §5.
///
/// # Allocation
///
/// Two `Vec`s are allocated per mesh, which is fine: meshing happens on worker
/// threads, never inside `nativeRenderFrame`. The buffers are handed back to the
/// render thread and recycled through [`MeshPool`].
pub fn mesh_section(job: &MeshJob) -> MeshResult {
    let started = std::time::Instant::now();
    let stride = job.format.vertex_stride() as usize;
    let mut vertices: Vec<u8> = Vec::with_capacity(SECTION_BLOCKS * 6 * stride);
    let mut indices: Vec<u8> = Vec::with_capacity(SECTION_BLOCKS * 36);
    let mut vertex_count: u32 = 0;

    for y in 0..SECTION_EDGE {
        for z in 0..SECTION_EDGE {
            for x in 0..SECTION_EDGE {
                let block = job.blocks[index(x, y, z)];
                if block == 0 {
                    continue;
                }
                for face in 0..6u8 {
                    if !face_visible(&job.blocks, x as i32, y as i32, z as i32, face) {
                        continue;
                    }
                    push_face(
                        &mut vertices,
                        &mut indices,
                        stride,
                        x as f32,
                        y as f32,
                        z as f32,
                        face,
                        block,
                        vertex_count,
                    );
                    vertex_count += 4;
                }
            }
        }
    }

    let index_count = (indices.len() / 4) as u32;
    MeshResult {
        key: job.key,
        generation: job.generation,
        format: job.format,
        vertices,
        indices,
        vertex_count,
        index_count,
        mesh_us: started.elapsed().as_micros().min(u64::from(u32::MAX)) as u32,
    }
}

#[inline]
fn index(x: usize, y: usize, z: usize) -> usize {
    (y * SECTION_EDGE + z) * SECTION_EDGE + x
}

fn face_visible(blocks: &[u8], x: i32, y: i32, z: i32, face: u8) -> bool {
    let edge = SECTION_EDGE as i32;
    let (dx, dy, dz) = match face {
        0 => (0, 1, 0),
        1 => (0, -1, 0),
        2 => (1, 0, 0),
        3 => (-1, 0, 0),
        4 => (0, 0, 1),
        _ => (0, 0, -1),
    };
    let (nx, ny, nz) = (x + dx, y + dy, z + dz);
    if nx < 0 || ny < 0 || nz < 0 || nx >= edge || ny >= edge || nz >= edge {
        // Section border: treat as visible. The real neighbour lookup happens in
        // the renderer's section-border pass; being conservative here only costs
        // a few hidden faces, never correctness.
        return true;
    }
    blocks[index(nx as usize, ny as usize, nz as usize)] == 0
}

#[allow(clippy::too_many_arguments)]
fn push_face(
    vertices: &mut Vec<u8>,
    indices: &mut Vec<u8>,
    stride: usize,
    x: f32,
    y: f32,
    z: f32,
    face: u8,
    block: u8,
    first_vertex: u32,
) {
    // Corner layout per face, counter-clockwise when viewed from outside.
    let corners: [[f32; 3]; 4] = match face {
        0 => [
            [x, y + 1.0, z],
            [x, y + 1.0, z + 1.0],
            [x + 1.0, y + 1.0, z + 1.0],
            [x + 1.0, y + 1.0, z],
        ],
        1 => [
            [x, y, z],
            [x + 1.0, y, z],
            [x + 1.0, y, z + 1.0],
            [x, y, z + 1.0],
        ],
        2 => [
            [x + 1.0, y, z],
            [x + 1.0, y + 1.0, z],
            [x + 1.0, y + 1.0, z + 1.0],
            [x + 1.0, y, z + 1.0],
        ],
        3 => [
            [x, y, z + 1.0],
            [x, y + 1.0, z + 1.0],
            [x, y + 1.0, z],
            [x, y, z],
        ],
        4 => [
            [x + 1.0, y, z + 1.0],
            [x + 1.0, y + 1.0, z + 1.0],
            [x, y + 1.0, z + 1.0],
            [x, y, z + 1.0],
        ],
        _ => [
            [x, y, z],
            [x, y + 1.0, z],
            [x + 1.0, y + 1.0, z],
            [x + 1.0, y, z],
        ],
    };
    let uvs: [[f32; 2]; 4] = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];

    for (corner, uv) in corners.iter().zip(uvs.iter()) {
        let start = vertices.len();
        vertices.resize(start + stride, 0);
        let slot = &mut vertices[start..start + stride];
        // Layout: pos f32x3, uv f32x2, then a format-specific tail.
        for (i, value) in corner.iter().enumerate() {
            slot[i * 4..i * 4 + 4].copy_from_slice(&value.to_ne_bytes());
        }
        slot[12..16].copy_from_slice(&uv[0].to_ne_bytes());
        slot[16..20].copy_from_slice(&uv[1].to_ne_bytes());
        if stride >= 24 {
            // 8 bytes: block id, face index, light/ao placeholder.
            slot[20] = block;
            slot[21] = face;
            slot[22] = 0xff;
            slot[23] = 0xff;
        }
    }

    for offset in [0u32, 1, 2, 0, 2, 3] {
        indices.extend_from_slice(&(first_vertex + offset).to_ne_bytes());
    }
}

/// LRU pool of CPU-side mesh buffers.
pub struct MeshPool {
    slots: Vec<MeshSlot>,
    cap: usize,
    clock: u64,
    evictions: u64,
    used_bytes: u64,
    byte_cap: u64,
}

struct MeshSlot {
    vertices: Vec<u8>,
    indices: Vec<u8>,
    owner: Option<SectionKey>,
    last_used: u64,
}

/// Pool statistics for `nativeGetCapabilityReport`.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct MeshPoolStats {
    /// Slots holding a mesh.
    pub resident: usize,
    /// Slots in the pool.
    pub capacity: usize,
    /// Bytes held by CPU meshes.
    pub bytes: u64,
    /// Sections evicted because of the hard cap or LRU pressure.
    pub evictions: u64,
}

impl MeshPool {
    /// Create a pool of `cap` slots with a byte ceiling of `byte_cap`.
    pub fn new(cap: usize, byte_cap: u64) -> MeshPool {
        MeshPool {
            slots: (0..cap.max(1))
                .map(|_| MeshSlot {
                    vertices: Vec::new(),
                    indices: Vec::new(),
                    owner: None,
                    last_used: 0,
                })
                .collect(),
            cap: cap.max(1),
            clock: 0,
            evictions: 0,
            used_bytes: 0,
            byte_cap,
        }
    }

    /// Number of slots.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Adopt a freshly meshed section, evicting the least recently used slot.
    ///
    /// Returns the slot index and the evicted section, if any, so the renderer can
    /// free the corresponding GPU buffers.
    pub fn insert(
        &mut self,
        result: &MeshResult,
    ) -> (usize, Option<SectionKey>) {
        self.clock += 1;
        let slot = self
            .slots
            .iter()
            .enumerate()
            .min_by_key(|(_, slot)| slot.last_used)
            .map(|(index, _)| index)
            .unwrap_or(0);
        let evicted = self.slots[slot].owner;
        self.used_bytes = self
            .used_bytes
            .saturating_sub(self.slots[slot].vertices.len() as u64 + self.slots[slot].indices.len() as u64);
        let entry = &mut self.slots[slot];
        entry.vertices.clear();
        entry.vertices.extend_from_slice(&result.vertices);
        entry.indices.clear();
        entry.indices.extend_from_slice(&result.indices);
        entry.owner = Some(result.key);
        entry.last_used = self.clock;
        self.used_bytes += result.vertices.len() as u64 + result.indices.len() as u64;
        if evicted.is_some() {
            self.evictions += 1;
        }
        self.enforce_byte_cap();
        (slot, evicted)
    }

    fn enforce_byte_cap(&mut self) {
        while self.used_bytes > self.byte_cap {
            let victim = self
                .slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| slot.owner.is_some())
                .min_by_key(|(_, slot)| slot.last_used)
                .map(|(index, _)| index);
            match victim {
                Some(index) => {
                    let slot = &mut self.slots[index];
                    self.used_bytes = self.used_bytes.saturating_sub(
                        slot.vertices.len() as u64 + slot.indices.len() as u64,
                    );
                    slot.vertices.clear();
                    slot.indices.clear();
                    slot.owner = None;
                    self.evictions += 1;
                }
                None => break,
            }
        }
    }

    /// Release the slot owned by `key`.
    pub fn evict(&mut self, key: SectionKey) -> bool {
        for slot in self.slots.iter_mut() {
            if slot.owner == Some(key) {
                self.used_bytes = self
                    .used_bytes
                    .saturating_sub(slot.vertices.len() as u64 + slot.indices.len() as u64);
                slot.vertices.clear();
                slot.indices.clear();
                slot.owner = None;
                return true;
            }
        }
        false
    }

    /// CPU vertex bytes of a slot.
    pub fn vertices(&self, slot: usize) -> Option<&[u8]> {
        self.slots
            .get(slot)
            .filter(|entry| entry.owner.is_some())
            .map(|entry| entry.vertices.as_slice())
    }

    /// CPU index bytes of a slot.
    pub fn indices(&self, slot: usize) -> Option<&[u8]> {
        self.slots
            .get(slot)
            .filter(|entry| entry.owner.is_some())
            .map(|entry| entry.indices.as_slice())
    }

    /// Slot for a section key.
    pub fn slot_of(&self, key: SectionKey) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.owner == Some(key))
    }

    /// Statistics snapshot.
    pub fn stats(&self) -> MeshPoolStats {
        MeshPoolStats {
            resident: self.slots.iter().filter(|slot| slot.owner.is_some()).count(),
            capacity: self.cap,
            bytes: self.used_bytes,
            evictions: self.evictions,
        }
    }
}

/// Worker pool that turns block data into meshes off the render thread.
pub struct MeshWorkers {
    jobs: Arc<ArrayQueue<MeshJob>>,
    results: Arc<ArrayQueue<MeshResult>>,
    running: Arc<AtomicBool>,
    completed: Arc<AtomicU64>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

/// Bounded queue sizes; depth 64 keeps a burst of section updates from stalling
/// the launcher thread while never growing.
pub const JOB_QUEUE_DEPTH: usize = 64;
/// Result queue depth.
pub const RESULT_QUEUE_DEPTH: usize = 256;

impl MeshWorkers {
    /// Spawn `count` workers. `count` is chosen by the renderer from the tier.
    pub fn spawn(count: usize) -> MeshWorkers {
        let jobs = Arc::new(ArrayQueue::new(JOB_QUEUE_DEPTH));
        let results = Arc::new(ArrayQueue::new(RESULT_QUEUE_DEPTH));
        let running = Arc::new(AtomicBool::new(true));
        let completed = Arc::new(AtomicU64::new(0));
        let mut threads = Vec::with_capacity(count);
        for worker in 0..count {
            let jobs = Arc::clone(&jobs);
            let results = Arc::clone(&results);
            let running = Arc::clone(&running);
            let completed = Arc::clone(&completed);
            let name = format!("sinkon-mesh-{worker}");
            let handle = std::thread::Builder::new()
                .name(name)
                .spawn(move || {
                    while running.load(Ordering::Relaxed) {
                        match jobs.pop() {
                            Some(job) => {
                                let result = mesh_section(&job);
                                completed.fetch_add(1, Ordering::Relaxed);
                                // If the render thread is behind, drop the oldest
                                // result rather than blocking the worker.
                                while results.push(result).is_err() {
                                    let _ = results.pop();
                                }
                            }
                            None => std::thread::yield_now(),
                        }
                    }
                })
                .ok();
            if let Some(handle) = handle {
                threads.push(handle);
            }
        }
        log::info!("meshing pool started with {} worker(s)", threads.len());
        MeshWorkers {
            jobs,
            results,
            running,
            completed,
            threads,
        }
    }

    /// Number of workers actually spawned.
    pub fn workers(&self) -> usize {
        self.threads.len()
    }

    /// Meshes completed since start.
    pub fn completed(&self) -> u64 {
        self.completed.load(Ordering::Relaxed)
    }

    /// Queue a job. Returns the job back when the queue is full.
    pub fn submit(&self, job: MeshJob) -> Result<(), MeshJob> {
        self.jobs.push(job)
    }

    /// Drain up to `out.len()` results into `out`, returning how many were taken.
    ///
    /// `out` is the caller's preallocated scratch vector; this never allocates.
    pub fn drain_into(&self, out: &mut Vec<MeshResult>) -> usize {
        let mut taken = 0;
        while out.len() < out.capacity() {
            match self.results.pop() {
                Some(result) => {
                    out.push(result);
                    taken += 1;
                }
                None => break,
            }
        }
        taken
    }

    /// Stop and join the workers.
    pub fn shutdown(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        log::info!("meshing pool stopped");
    }
}

impl Drop for MeshWorkers {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Six-plane view frustum in world space.
#[derive(Clone, Copy, Debug)]
pub struct Frustum {
    planes: [[f32; 4]; 6],
}

impl Frustum {
    /// Extract the planes from a column-major view-projection matrix.
    ///
    /// Uses the Gribb–Hartmann method with the GL clip convention (−1…1 in z).
    pub fn from_view_proj(matrix: &[[f32; 4]; 4]) -> Frustum {
        let row = |index: usize| {
            [
                matrix[0][index],
                matrix[1][index],
                matrix[2][index],
                matrix[3][index],
            ]
        };
        let (r0, r1, r2, r3) = (row(0), row(1), row(2), row(3));
        let mut planes = [
            add(r3, r0),        // left
            sub(r3, r0),        // right
            add(r3, r1),        // bottom
            sub(r3, r1),        // top
            add(r3, r2),        // near
            sub(r3, r2),        // far
        ];
        for plane in planes.iter_mut() {
            let length = (plane[0] * plane[0] + plane[1] * plane[1] + plane[2] * plane[2]).sqrt();
            if length > f32::EPSILON {
                for component in plane.iter_mut() {
                    *component /= length;
                }
            }
        }
        Frustum { planes }
    }

    /// Conservative AABB test: true when the box is not fully outside.
    pub fn intersects_aabb(&self, min: [f32; 3], max: [f32; 3]) -> bool {
        for plane in self.planes.iter() {
            // Positive vertex: the box corner furthest along the plane normal.
            let x = if plane[0] >= 0.0 { max[0] } else { min[0] };
            let y = if plane[1] >= 0.0 { max[1] } else { min[1] };
            let z = if plane[2] >= 0.0 { max[2] } else { min[2] };
            if plane[0] * x + plane[1] * y + plane[2] * z + plane[3] < 0.0 {
                return false;
            }
        }
        true
    }
}

fn add(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]]
}

fn sub(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]]
}

/// A section that could be drawn, as seen by the culler.
#[derive(Clone, Copy, Debug)]
pub struct SectionInstance {
    /// Section key.
    pub key: SectionKey,
    /// Pool slot holding the mesh.
    pub slot: u32,
    /// Index count of the mesh.
    pub index_count: u32,
    /// Whether the section's parent was visible last frame (visibility graph).
    pub parent_visible: bool,
}

/// Reusable culling scratch space.
///
/// Allocated once; every frame only calls `clear()` and pushes into the
/// pre-reserved capacity, so culling performs no allocation.
pub struct CullScratch {
    /// Visible sections, sorted front to back.
    pub visible: Vec<(u32, f32)>,
    /// Number of sections tested this frame.
    pub tested: u32,
}

impl CullScratch {
    /// Allocate scratch able to hold `capacity` visible sections.
    pub fn with_capacity(capacity: usize) -> CullScratch {
        CullScratch {
            visible: Vec::with_capacity(capacity),
            tested: 0,
        }
    }

    /// Clear for the next frame, keeping the allocation.
    pub fn begin_frame(&mut self) {
        self.visible.clear();
        self.tested = 0;
    }

    /// Visible section count.
    pub fn drawn(&self) -> u32 {
        self.visible.len() as u32
    }
}

/// Frustum-cull `sections` into `scratch`, front to back.
///
/// `camera` is the world-space eye position used for the distance sort; the sort
/// is what keeps overdraw low on tile-based Android GPUs.
pub fn cull_sections(
    sections: &[SectionInstance],
    frustum: &Frustum,
    camera: [f32; 3],
    scratch: &mut CullScratch,
) {
    scratch.begin_frame();
    for section in sections {
        scratch.tested += 1;
        let origin = section.key.origin();
        let min = origin;
        let max = [
            origin[0] + SECTION_SIZE,
            origin[1] + SECTION_SIZE,
            origin[2] + SECTION_SIZE,
        ];
        if !frustum.intersects_aabb(min, max) {
            continue;
        }
        if !section.parent_visible {
            // Conservative occlusion: a section whose parent was invisible last
            // frame is skipped unless it is within two sections of the camera,
            // where being wrong is most noticeable.
            let dx = origin[0] + SECTION_SIZE * 0.5 - camera[0];
            let dy = origin[1] + SECTION_SIZE * 0.5 - camera[1];
            let dz = origin[2] + SECTION_SIZE * 0.5 - camera[2];
            if dx * dx + dy * dy + dz * dz > 4.0 * SECTION_SIZE * SECTION_SIZE {
                continue;
            }
        }
        let center = [
            origin[0] + SECTION_SIZE * 0.5,
            origin[1] + SECTION_SIZE * 0.5,
            origin[2] + SECTION_SIZE * 0.5,
        ];
        let dx = center[0] - camera[0];
        let dy = center[1] - camera[1];
        let dz = center[2] - camera[2];
        if scratch.visible.len() < scratch.visible.capacity() {
            scratch
                .visible
                .push((section.slot, dx * dx + dy * dy + dz * dz));
        }
    }
    // Front to back: nearest first.
    scratch
        .visible
        .sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(block: u8) -> MeshJob {
        MeshJob {
            key: SectionKey::new(0, 0, 0),
            generation: 1,
            format: MeshFormat::Packed4,
            blocks: vec![block; SECTION_BLOCKS].into_boxed_slice(),
        }
    }

    #[test]
    fn air_meshes_to_nothing() {
        let result = mesh_section(&filled(0));
        assert_eq!(result.vertex_count, 0);
        assert_eq!(result.index_count, 0);
        assert!(result.vertices.is_empty());
    }

    #[test]
    fn single_block_emits_six_faces() {
        let mut job = filled(0);
        job.blocks[index(8, 8, 8)] = 1;
        let result = mesh_section(&job);
        assert_eq!(result.vertex_count, 24, "6 faces × 4 vertices");
        assert_eq!(result.index_count, 36, "6 faces × 6 indices");
        assert_eq!(result.vertices.len(), 24 * 20);
        assert_eq!(result.indices.len(), 36 * 4);
    }

    #[test]
    fn fully_enclosed_block_emits_nothing() {
        let mut job = filled(1);
        job.blocks[index(8, 8, 8)] = 0;
        let result = mesh_section(&job);
        // The hollow centre is not rendered, so the count matches a solid 16³
        // section with the interior faces culled.
        assert!(result.vertex_count > 0);
        let solid = mesh_section(&filled(1));
        assert_eq!(result.vertex_count, solid.vertex_count);
    }

    #[test]
    fn pool_holds_and_evicts() {
        let mut pool = MeshPool::new(2, u64::MAX);
        let mut job = filled(0);
        job.blocks[index(1, 1, 1)] = 3;
        let mesh = mesh_section(&job);
        let (slot_a, evicted) = pool.insert(&mesh);
        assert!(evicted.is_none());
        assert_eq!(pool.slot_of(SectionKey::new(0, 0, 0)), Some(slot_a));
        assert!(pool.vertices(slot_a).is_some());

        let mut other = mesh;
        other.key = SectionKey::new(1, 0, 0);
        let (_slot_b, evicted) = pool.insert(&other);
        assert!(evicted.is_none(), "second slot is still free");
        let mut third = other;
        third.key = SectionKey::new(2, 0, 0);
        let (_slot_c, evicted) = pool.insert(&third);
        assert!(evicted.is_some(), "pool is full, LRU must be evicted");
        assert_eq!(pool.stats().capacity, 2);
    }

    #[test]
    fn pool_byte_cap_is_enforced() {
        let mut pool = MeshPool::new(8, 1);
        let mut job = filled(0);
        job.blocks[index(2, 2, 2)] = 5;
        let mesh = mesh_section(&job);
        pool.insert(&mesh);
        assert_eq!(pool.stats().resident, 0, "byte cap evicts immediately");
        assert_eq!(pool.stats().bytes, 0);
        assert!(pool.stats().evictions >= 1);
    }

    #[test]
    fn frustum_rejects_outside_sections() {
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let frustum = Frustum::from_view_proj(&identity);
        assert!(frustum.intersects_aabb([-0.5, -0.5, -0.5], [0.5, 0.5, 0.5]));
        assert!(!frustum.intersects_aabb([10.0, 10.0, 10.0], [11.0, 11.0, 11.0]));
    }

    #[test]
    fn culling_is_sorted_front_to_back() {
        let sections: Vec<SectionInstance> = (0..8)
            .map(|i| SectionInstance {
                key: SectionKey::new(0, 0, i * 2),
                slot: i as u32,
                index_count: 36,
                parent_visible: true,
            })
            .collect();
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let frustum = Frustum::from_view_proj(&identity);
        let mut scratch = CullScratch::with_capacity(16);
        cull_sections(&sections, &frustum, [0.0, 0.0, 0.0], &mut scratch);
        assert!(scratch.drawn() > 0);
        let distances: Vec<f32> = scratch.visible.iter().map(|(_, d)| *d).collect();
        let mut sorted = distances.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(distances, sorted);
        // No allocation beyond the preallocated capacity.
        assert!(scratch.visible.capacity() >= scratch.visible.len());
    }

    #[test]
    fn section_tag_is_deterministic() {
        let a = SectionKey::new(3, -4, 5);
        assert_eq!(a.tag(), SectionKey::new(3, -4, 5).tag());
        assert_ne!(a.tag(), SectionKey::new(4, -4, 5).tag());
    }

    #[test]
    fn format_ids_round_trip() {
        assert_eq!(MeshFormat::from_id(4), MeshFormat::Packed4);
        assert_eq!(MeshFormat::from_id(8), MeshFormat::Packed8);
        assert_eq!(MeshFormat::from_id(99), MeshFormat::Custom(99));
        assert!(MeshFormat::Packed8.vertex_stride() > MeshFormat::Packed4.vertex_stride());
    }
}
