// Pool module for memory-efficient meshing

use super::{CpuMeshData, Vertex};

/// Memory pool for mesh generation to avoid repeated allocations
/// Contains reusable buffers for all mesh generation operations
#[derive(Default)]
pub(crate) struct MeshingPool {
    /// Main vertex buffer (reused for each mesh layer)
    pub(crate) vertices: Vec<Vertex>,
    /// Main index buffer (reused for each mesh layer)
    pub(crate) indices: Vec<u32>,

    /// Greedy meshing scratch buffers
    pub(crate) greedy_mask: Vec<Option<super::greedy::MaskCell>>,
    /// Per-mask-cell AO/light fingerprint, parallel to `greedy_mask`. Pooled so
    /// the AO-aware merge check costs no allocation per chunk.
    pub(crate) greedy_mask_ao: Vec<u64>,
    /// Stop greedy merging at AO/light discontinuities. Off by default — see
    /// `greedy::greedy_ao_aware_merge_enabled` for the measured trade.
    pub(crate) ao_aware_merge: bool,

    /// Liquid meshing corner caches (already implemented in LiquidMeshingPool)
    pub(crate) liquid_pool: super::liquids::LiquidMeshingPool,

    /// Temporary position arrays for liquid/plant mesh generation
    pub(crate) temp_positions: Vec<[f32; 3]>,
    pub(crate) temp_uvs: Vec<[f32; 2]>,
    layered_hints: LayeredCapacityHints,
}

#[derive(Clone, Copy, Debug, Default)]
struct LayeredCapacityHints {
    opaque_vertices: usize,
    opaque_indices: usize,
    cutout_vertices: usize,
    cutout_indices: usize,
    transparent_vertices: usize,
    transparent_indices: usize,
}

impl MeshingPool {
    /// Create a new pool with reasonable initial capacity
    pub(crate) fn new() -> Self {
        Self {
            vertices: Vec::with_capacity(8192),
            indices: Vec::with_capacity(12288),
            greedy_mask: Vec::with_capacity(256), // CHUNK_SIZE * CHUNK_SIZE
            greedy_mask_ao: Vec::with_capacity(256),
            ao_aware_merge: false,
            liquid_pool: super::liquids::LiquidMeshingPool::with_capacity(128),
            temp_positions: Vec::with_capacity(256),
            temp_uvs: Vec::with_capacity(256),
            layered_hints: LayeredCapacityHints::default(),
        }
    }

    /// Reset all buffers for reuse while retaining allocated capacity
    pub(crate) fn reset(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.greedy_mask.clear();
        self.liquid_pool.clear_for_reuse();
        self.temp_positions.clear();
        self.temp_uvs.clear();
    }

    /// Get a CpuMeshData that borrows from this pool's buffers
    /// The returned mesh data will be moved/copied when needed
    pub(crate) fn take_mesh_data(&mut self) -> CpuMeshData {
        let vertices = std::mem::take(&mut self.vertices);
        let indices = std::mem::take(&mut self.indices);

        // Restore capacity for next use
        self.vertices = Vec::with_capacity(vertices.capacity().max(8192));
        self.indices = Vec::with_capacity(indices.capacity().max(12288));

        CpuMeshData { vertices, indices }
    }

    /// Get statistics for debugging allocation patterns
    pub(crate) fn allocation_stats(&self) -> MeshingPoolStats {
        MeshingPoolStats {
            vertices_capacity: self.vertices.capacity(),
            indices_capacity: self.indices.capacity(),
        }
    }

    pub(crate) fn scratch_buffers_reused(&self) -> u32 {
        u32::from(!self.greedy_mask.is_empty() || self.greedy_mask.capacity() > 0)
            + u32::from(self.liquid_pool.allocation_stats().total_capacity() > 0)
            + u32::from(!self.temp_positions.is_empty() || self.temp_positions.capacity() > 0)
            + u32::from(!self.temp_uvs.is_empty() || self.temp_uvs.capacity() > 0)
    }

    pub(crate) fn prepare_layered_geometry(
        &self,
        layered: &mut super::LayeredChunkGeometry,
    ) -> u32 {
        let mut reused = 0u32;
        reused += reserve_if_hint(
            &mut layered.opaque.vertices,
            self.layered_hints.opaque_vertices,
        );
        reused += reserve_if_hint(
            &mut layered.opaque.indices,
            self.layered_hints.opaque_indices,
        );
        reused += reserve_if_hint(
            &mut layered.cutout.vertices,
            self.layered_hints.cutout_vertices,
        );
        reused += reserve_if_hint(
            &mut layered.cutout.indices,
            self.layered_hints.cutout_indices,
        );
        reused += reserve_if_hint(
            &mut layered.transparent.vertices,
            self.layered_hints.transparent_vertices,
        );
        reused += reserve_if_hint(
            &mut layered.transparent.indices,
            self.layered_hints.transparent_indices,
        );
        reused
    }

    pub(crate) fn record_layered_geometry_usage(
        &mut self,
        layered: &super::LayeredChunkGeometry,
    ) -> u32 {
        let mut growths = 0u32;
        growths += update_hint(
            &mut self.layered_hints.opaque_vertices,
            layered.opaque.vertices.capacity(),
        );
        growths += update_hint(
            &mut self.layered_hints.opaque_indices,
            layered.opaque.indices.capacity(),
        );
        growths += update_hint(
            &mut self.layered_hints.cutout_vertices,
            layered.cutout.vertices.capacity(),
        );
        growths += update_hint(
            &mut self.layered_hints.cutout_indices,
            layered.cutout.indices.capacity(),
        );
        growths += update_hint(
            &mut self.layered_hints.transparent_vertices,
            layered.transparent.vertices.capacity(),
        );
        growths += update_hint(
            &mut self.layered_hints.transparent_indices,
            layered.transparent.indices.capacity(),
        );
        growths
    }
}

#[inline(always)]
fn reserve_if_hint<T>(buffer: &mut Vec<T>, hint: usize) -> u32 {
    if hint == 0 {
        return 0;
    }
    // `Vec::reserve` is additive on top of the current length, so passing the
    // absolute hint asked for `len + hint` and made the buffer ratchet upward
    // across chunks. Reserve only the shortfall, and only if there is one.
    let additional = hint.saturating_sub(buffer.len());
    if additional == 0 {
        return 0;
    }
    buffer.reserve_exact(additional);
    1
}

/// Tracks the working-set size for the next chunk. The hint decays toward the
/// most recent usage instead of latching onto the largest chunk ever meshed,
/// which otherwise kept every later chunk's buffers at that peak.
#[inline(always)]
fn update_hint(current: &mut usize, capacity: usize) -> u32 {
    if capacity > *current {
        *current = capacity;
        1
    } else {
        // Ease back down so a single dense chunk does not pin the hint forever.
        *current -= (*current - capacity) / 4;
        0
    }
}

/// Statistics for tracking memory pool usage
#[derive(Debug, Clone)]
pub(crate) struct MeshingPoolStats {
    pub(crate) vertices_capacity: usize,
    pub(crate) indices_capacity: usize,
}

// Thread-local storage for mesh pools to avoid contention between workers
thread_local! {
    static MESHING_POOL: std::cell::RefCell<MeshingPool> = std::cell::RefCell::new(MeshingPool::new());
}

/// Execute meshing operation with pooled memory
pub(crate) fn with_meshing_pool<F, R>(f: F) -> R
where
    F: FnOnce(&mut MeshingPool) -> R,
{
    MESHING_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        pool.reset();
        f(&mut pool)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_reuse() {
        let mut pool = MeshingPool::new();

        // First use
        pool.vertices.push(Vertex {
            position: [0.0, 0.0, 0.0],
            uv: [0.0, 0.0],
            tex_layer: 0,
            color: [1.0, 1.0, 1.0],
            tint_kind: 0,
            light_packed: 0,
        });
        pool.indices.push(0);

        let initial_vertex_cap = pool.vertices.capacity();
        let initial_index_cap = pool.indices.capacity();

        // Reset and reuse
        pool.reset();
        assert_eq!(pool.vertices.len(), 0);
        assert_eq!(pool.indices.len(), 0);
        assert_eq!(pool.vertices.capacity(), initial_vertex_cap);
        assert_eq!(pool.indices.capacity(), initial_index_cap);
    }

    #[test]
    fn test_take_mesh_data() {
        let mut pool = MeshingPool::new();

        pool.vertices.push(Vertex {
            position: [1.0, 2.0, 3.0],
            uv: [0.5, 0.5],
            tex_layer: 1,
            color: [0.8, 0.8, 0.8],
            tint_kind: 0,
            light_packed: 0xFF,
        });
        pool.indices.extend_from_slice(&[0, 1, 2]);

        let mesh = pool.take_mesh_data();
        assert_eq!(mesh.vertices.len(), 1);
        assert_eq!(mesh.indices.len(), 3);
        assert_eq!(mesh.vertices[0].position, [1.0, 2.0, 3.0]);

        // Pool should be empty but retain capacity
        assert_eq!(pool.vertices.len(), 0);
        assert_eq!(pool.indices.len(), 0);
        assert!(pool.vertices.capacity() >= 8192);
        assert!(pool.indices.capacity() >= 12288);
    }
}
