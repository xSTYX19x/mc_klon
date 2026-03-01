use bytemuck::{Pod, Zeroable};
use glam::IVec3;

use crate::blocks::RenderLayer;
use crate::world::{CHUNK_SIZE, ChunkCoord};

pub(crate) mod greedy;
pub(crate) mod liquids;
pub(crate) mod plants;
pub(crate) mod pool;

#[cfg(test)]
mod pool_regression_test;

#[cfg(test)]
pub(crate) use greedy::build_chunk_mesh_greedy;
#[cfg(test)]
pub(crate) use liquids::{
    LIQUID_EPS, LIQUID_SOURCE_HEIGHT, effective_cell_height, fluid_surface_height,
    liquid_fill_height_at_point, surface_height, wall_height,
};
pub(crate) use liquids::{fluid_kind_at_point, liquid_height_at_point};

// Export the new pooled meshing functions
pub(crate) use greedy::{build_chunk_mesh_greedy_layered, build_chunk_mesh_layered_naive};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct Vertex {
    pub(crate) position: [f32; 3],
    pub(crate) uv: [f32; 2],
    pub(crate) tex_layer: u32,
    pub(crate) color: [f32; 3],
    pub(crate) tint_kind: u32,
    pub(crate) light_packed: u32, // high nibble sky, low nibble block
}

pub(crate) const FULLBRIGHT_LIGHT_PACKED: u32 = 0xF0;

#[inline(always)]
pub(crate) const fn pack_vertex_light(skylight: u8, blocklight: u8) -> u32 {
    (((skylight & 0x0F) as u32) << 4) | ((blocklight & 0x0F) as u32)
}

#[derive(Default)]
pub(crate) struct CpuMeshData {
    pub(crate) vertices: Vec<Vertex>,
    pub(crate) indices: Vec<u32>,
}

impl CpuMeshData {}

#[derive(Default)]
pub(crate) struct LayeredChunkGeometry {
    pub(crate) opaque: CpuMeshData,
    pub(crate) cutout: CpuMeshData,
    pub(crate) transparent: CpuMeshData,
}

impl LayeredChunkGeometry {
    pub(crate) fn mesh_mut(&mut self, layer: RenderLayer) -> &mut CpuMeshData {
        match layer {
            RenderLayer::Opaque => &mut self.opaque,
            RenderLayer::Cutout => &mut self.cutout,
            RenderLayer::Transparent => &mut self.transparent,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn total_vertex_count(&self) -> usize {
        self.opaque.vertices.len() + self.cutout.vertices.len() + self.transparent.vertices.len()
    }

    pub(crate) fn total_index_count(&self) -> usize {
        self.opaque.indices.len() + self.cutout.indices.len() + self.transparent.indices.len()
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn flatten_layered_geometry(
    mut layered: LayeredChunkGeometry,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(2048);
    let mut indices = Vec::with_capacity(3072);

    for mesh in [
        &mut layered.opaque,
        &mut layered.cutout,
        &mut layered.transparent,
    ] {
        let base = vertices.len() as u32;
        vertices.append(&mut mesh.vertices);
        indices.extend(mesh.indices.drain(..).map(|i| i + base));
    }

    (vertices, indices)
}

pub(crate) fn chunk_world_origin(coord: ChunkCoord) -> IVec3 {
    IVec3::new(
        coord.x * CHUNK_SIZE,
        coord.y * CHUNK_SIZE,
        coord.z * CHUNK_SIZE,
    )
}

pub(crate) fn chunk_offset(coord: ChunkCoord) -> [f32; 3] {
    [
        (coord.x * CHUNK_SIZE) as f32,
        (coord.y * CHUNK_SIZE) as f32,
        (coord.z * CHUNK_SIZE) as f32,
    ]
}
