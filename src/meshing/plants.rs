use crate::blocks::{BlockModel, BlockTextureLookup, Face, TINT_NONE};
use crate::world::CHUNK_SIZE;
use crate::world::chunk::Chunk;

use super::{CpuMeshData, FULLBRIGHT_LIGHT_PACKED, Vertex};

pub(super) fn append_cross_plants_in_chunk(
    chunk: &Chunk,
    chunk_offset: [f32; 3],
    texture_lookup: &BlockTextureLookup,
    cutout_mesh: &mut CpuMeshData,
) {
    append_cross_plants_in_chunk_pooled(chunk, chunk_offset, texture_lookup, cutout_mesh, None)
}

pub(super) fn append_cross_plants_in_chunk_pooled(
    chunk: &Chunk,
    chunk_offset: [f32; 3],
    texture_lookup: &BlockTextureLookup,
    cutout_mesh: &mut CpuMeshData,
    _pool: Option<&mut super::pool::MeshingPool>,
) {
    for y in 0..CHUNK_SIZE {
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let id = chunk.get(x, y, z);
                if !id.is_voxel_filled() || texture_lookup.model_for(id) != BlockModel::CrossPlant {
                    continue;
                }
                let tex_layer = texture_lookup
                    .plant_layer_for(id)
                    .unwrap_or(texture_lookup.layer_for(id, Face::North));
                let base = [
                    x as f32 + chunk_offset[0],
                    y as f32 + chunk_offset[1],
                    z as f32 + chunk_offset[2],
                ];
                push_cross_plant(
                    base,
                    tex_layer,
                    &mut cutout_mesh.vertices,
                    &mut cutout_mesh.indices,
                );
            }
        }
    }
}

fn push_cross_plant(
    base: [f32; 3],
    tex_layer: u32,
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
) {
    let x = base[0];
    let y = base[1];
    let z = base[2];
    let color = [1.0, 1.0, 1.0];
    let uv0 = [0.0, 1.0];
    let uv1 = [1.0, 1.0];
    let uv2 = [1.0, 0.0];
    let uv3 = [0.0, 0.0];

    let quad_a = [
        [x + 0.0, y + 0.0, z + 0.0],
        [x + 1.0, y + 0.0, z + 1.0],
        [x + 1.0, y + 1.0, z + 1.0],
        [x + 0.0, y + 1.0, z + 0.0],
    ];
    push_double_sided_quad(
        quad_a,
        [uv0, uv1, uv2, uv3],
        tex_layer,
        color,
        vertices,
        indices,
    );

    let quad_b = [
        [x + 1.0, y + 0.0, z + 0.0],
        [x + 0.0, y + 0.0, z + 1.0],
        [x + 0.0, y + 1.0, z + 1.0],
        [x + 1.0, y + 1.0, z + 0.0],
    ];
    push_double_sided_quad(
        quad_b,
        [uv0, uv1, uv2, uv3],
        tex_layer,
        color,
        vertices,
        indices,
    );
}

fn push_double_sided_quad(
    positions: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    tex_layer: u32,
    color: [f32; 3],
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
) {
    let base = vertices.len() as u32;
    for i in 0..4 {
        vertices.push(Vertex {
            position: positions[i],
            uv: uvs[i],
            tex_layer,
            color,
            tint_kind: TINT_NONE,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
        });
    }
    // Front
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    // Back
    indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
}
