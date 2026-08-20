use bytemuck::{Pod, Zeroable};
use glam::IVec3;

use crate::blocks::RenderLayer;
use crate::world::{CHUNK_SIZE, ChunkCoord};

pub(crate) mod greedy;
pub(crate) mod liquids;
pub(crate) mod neighborhood;
pub(crate) mod plants;
pub(crate) mod pool;

#[cfg(test)]
mod pool_regression_test;

#[cfg(test)]
pub(crate) use greedy::build_chunk_mesh_greedy;
pub(crate) use greedy::face_shade;
#[cfg(test)]
pub(crate) use liquids::{
    LIQUID_EPS, LIQUID_SOURCE_HEIGHT, effective_cell_height, fluid_surface_height,
    liquid_fill_height_at_point, surface_height, wall_height,
};
pub(crate) use liquids::{fluid_kind_at_point, liquid_height_at_point};
pub(crate) use neighborhood::{ChunkSnapshot, MeshingNeighborhoodScratch, NeighborhoodSnapshot};

// Export the new pooled meshing functions
pub(crate) use greedy::{build_chunk_mesh_greedy_layered, build_chunk_mesh_layered_naive};

/// Legacy vertex used for UI, sky, LOD and overlay meshes (44 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct Vertex {
    pub(crate) position: [f32; 3],
    pub(crate) uv: [f32; 2],
    pub(crate) tex_layer: u32,
    pub(crate) color: [f32; 3],
    pub(crate) tint_kind: u32,
    pub(crate) light_packed: u32,
}

/// Compact vertex for world chunk meshes (24 bytes — 45 % smaller than `Vertex`).
///
/// UV coordinates are fixed-point scaled by [`UV_SCALE`].
/// `shade` encodes baked face-darkening × AO as a u8 (0 = black, 255 = fully lit).
/// `tint_kind` and `light_packed` are the same semantic values as in `Vertex` but
/// stored as `u8` since their value ranges fit (tint ≤ 2, light ≤ 0xFF).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct WorldVertex {
    pub(crate) position: [f32; 3], // 12 bytes
    pub(crate) uv: [u16; 2],       //  4 bytes  (× UV_SCALE)
    pub(crate) tex_layer: u32,     //  4 bytes
    pub(crate) shade: u8,          //  1 byte   (0..255 → 0.0..1.0)
    pub(crate) tint_kind: u8,      //  1 byte
    pub(crate) light_packed: u8,   //  1 byte   (high nibble sky, low nibble block)
    pub(crate) _pad: u8,           //  1 byte   (struct total: 24 bytes)
}

/// Scale factor for encoding float UVs as u16.
/// Supports UVs up to 65535 / 4096 ≈ 16.0 (= CHUNK_SIZE), with ~0.00024 sub-texel precision.
pub(crate) const UV_SCALE: f32 = 4096.0;

#[inline(always)]
pub(crate) fn pack_uv(u: f32, v: f32) -> [u16; 2] {
    [
        (u * UV_SCALE).clamp(0.0, 65535.0) as u16,
        (v * UV_SCALE).clamp(0.0, 65535.0) as u16,
    ]
}

#[inline(always)]
pub(crate) fn pack_shade(shade: f32) -> u8 {
    (shade.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

pub(crate) const FULLBRIGHT_LIGHT_PACKED: u32 = 0xF0;

#[inline(always)]
pub(crate) const fn pack_vertex_light(skylight: u8, blocklight: u8) -> u32 {
    (((skylight & 0x0F) as u32) << 4) | ((blocklight & 0x0F) as u32)
}

/// Mesh data for UI, sky, LOD and overlay geometry (uses `Vertex`).
#[derive(Default)]
pub(crate) struct CpuMeshData {
    pub(crate) vertices: Vec<Vertex>,
    pub(crate) indices: Vec<u32>,
}

impl CpuMeshData {}

/// Mesh data for world chunk geometry (uses the compact `WorldVertex`).
#[derive(Default)]
pub(crate) struct WorldCpuMeshData {
    pub(crate) vertices: Vec<WorldVertex>,
    pub(crate) indices: Vec<u32>,
}

/// Per-layer geometry, each using the compact `WorldVertex`.
#[derive(Default)]
pub(crate) struct LayeredChunkGeometry {
    pub(crate) opaque: WorldCpuMeshData,
    pub(crate) cutout: WorldCpuMeshData,
    pub(crate) transparent: WorldCpuMeshData,
}

impl LayeredChunkGeometry {
    pub(crate) fn mesh_mut(&mut self, layer: RenderLayer) -> &mut WorldCpuMeshData {
        match layer {
            RenderLayer::Opaque => &mut self.opaque,
            RenderLayer::Cutout => &mut self.cutout,
            RenderLayer::Transparent => &mut self.transparent,
        }
    }

    pub(crate) fn take_mesh(&mut self, layer: RenderLayer) -> WorldCpuMeshData {
        match layer {
            RenderLayer::Opaque => std::mem::take(&mut self.opaque),
            RenderLayer::Cutout => std::mem::take(&mut self.cutout),
            RenderLayer::Transparent => std::mem::take(&mut self.transparent),
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MeshLayerMask(u8);

impl MeshLayerMask {
    pub(crate) const NONE: Self = Self(0);
    pub(crate) const OPAQUE: Self = Self(1 << 0);
    pub(crate) const CUTOUT: Self = Self(1 << 1);
    pub(crate) const TRANSPARENT: Self = Self(1 << 2);
    pub(crate) const ALL: Self = Self(Self::OPAQUE.0 | Self::CUTOUT.0 | Self::TRANSPARENT.0);

    pub(crate) const fn for_render_layer(layer: RenderLayer) -> Self {
        match layer {
            RenderLayer::Opaque => Self::OPAQUE,
            RenderLayer::Cutout => Self::CUTOUT,
            RenderLayer::Transparent => Self::TRANSPARENT,
        }
    }

    pub(crate) const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub(crate) const fn contains_render_layer(self, layer: RenderLayer) -> bool {
        self.contains(Self::for_render_layer(layer))
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) const fn bits(self) -> u8 {
        self.0
    }

    pub(crate) fn labels(self) -> &'static str {
        match self.0 {
            0 => "none",
            x if x == Self::OPAQUE.0 => "opaque",
            x if x == Self::CUTOUT.0 => "cutout",
            x if x == Self::TRANSPARENT.0 => "transparent",
            x if x == (Self::OPAQUE.0 | Self::CUTOUT.0) => "opaque|cutout",
            x if x == (Self::OPAQUE.0 | Self::TRANSPARENT.0) => "opaque|transparent",
            x if x == (Self::CUTOUT.0 | Self::TRANSPARENT.0) => "cutout|transparent",
            x if x == Self::ALL.0 => "opaque|cutout|transparent",
            _ => "mixed",
        }
    }
}

impl std::ops::BitOr for MeshLayerMask {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for MeshLayerMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn flatten_layered_geometry(
    mut layered: LayeredChunkGeometry,
) -> (Vec<WorldVertex>, Vec<u32>) {
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
