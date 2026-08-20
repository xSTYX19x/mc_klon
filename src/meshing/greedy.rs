use std::sync::atomic::{AtomicBool, Ordering};

use glam::{IVec3, Vec4};

use crate::blocks::{
    BlockId, BlockModel, BlockTextureLookup, CullGroup, Face, FaceCoverage, OcclusionShape,
};
#[cfg(test)]
use crate::world::World;
use crate::world::fluids::fluid_kind;
use crate::world::{BlockAccess, CHUNK_SIZE, ChunkCoord};

#[cfg(test)]
use super::flatten_layered_geometry;
use super::liquids::fluid_surface_height_at;
use super::neighborhood::{MeshingNeighborhood, PADDED_CHUNK_STRIDE_Y, PADDED_CHUNK_STRIDE_Z};
use super::{
    WorldCpuMeshData, WorldVertex, FULLBRIGHT_LIGHT_PACKED, LayeredChunkGeometry, MeshLayerMask,
    chunk_offset, chunk_world_origin, pack_uv, pack_shade, pack_vertex_light,
};

static DEBUG_DISABLE_AO: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_debug_disable_ao(disable: bool) {
    DEBUG_DISABLE_AO.store(disable, Ordering::Relaxed);
}

#[inline(always)]
fn debug_disable_ao() -> bool {
    DEBUG_DISABLE_AO.load(Ordering::Relaxed)
}

/// UV-Basis pro Face:
/// - U läuft nach rechts, wenn man von außen auf die Face schaut.
/// - V läuft nach unten (V=0 ist die obere Texturkante).
#[inline]
fn face_uv_basis(face: Face) -> ([i32; 3], [i32; 3]) {
    match face {
        Face::North => ([1, 0, 0], [0, -1, 0]),
        Face::South => ([-1, 0, 0], [0, -1, 0]),
        Face::West => ([0, 0, 1], [0, -1, 0]),
        Face::East => ([0, 0, -1], [0, -1, 0]),
        Face::Up => ([1, 0, 0], [0, 0, 1]),
        Face::Down => ([1, 0, 0], [0, 0, -1]),
    }
}

#[inline(always)]
fn project_axis(p: [i32; 3], axis: [i32; 3]) -> i32 {
    p[0] * axis[0] + p[1] * axis[1] + p[2] * axis[2]
}

/// SIMD-optimized UV calculation that processes all 4 corners of a quad simultaneously
/// Uses Vec4 to compute U and V projections in parallel
fn face_uvs_for_quad_simd(face: Face, corners: [[i32; 3]; 4]) -> [[f32; 2]; 4] {
    let (u_axis, v_axis) = face_uv_basis(face);

    // Pack corner coordinates into SIMD vectors
    let x_coords = Vec4::new(
        corners[0][0] as f32,
        corners[1][0] as f32,
        corners[2][0] as f32,
        corners[3][0] as f32,
    );
    let y_coords = Vec4::new(
        corners[0][1] as f32,
        corners[1][1] as f32,
        corners[2][1] as f32,
        corners[3][1] as f32,
    );
    let z_coords = Vec4::new(
        corners[0][2] as f32,
        corners[1][2] as f32,
        corners[2][2] as f32,
        corners[3][2] as f32,
    );

    // Pack UV basis vectors
    let u_axis_x = Vec4::splat(u_axis[0] as f32);
    let u_axis_y = Vec4::splat(u_axis[1] as f32);
    let u_axis_z = Vec4::splat(u_axis[2] as f32);

    let v_axis_x = Vec4::splat(v_axis[0] as f32);
    let v_axis_y = Vec4::splat(v_axis[1] as f32);
    let v_axis_z = Vec4::splat(v_axis[2] as f32);

    // Compute projections using SIMD dot products
    let u_projections = x_coords * u_axis_x + y_coords * u_axis_y + z_coords * u_axis_z;
    let v_projections = x_coords * v_axis_x + y_coords * v_axis_y + z_coords * v_axis_z;

    // Find min values for normalization
    let u_min = u_projections.min_element();
    let v_min = v_projections.min_element();

    // Normalize and convert to final UV coordinates
    let u_min_vec = Vec4::splat(u_min);
    let v_min_vec = Vec4::splat(v_min);

    let u_final = u_projections - u_min_vec;
    let v_final = v_projections - v_min_vec;

    // Extract results from SIMD vectors
    [
        [u_final.x, v_final.x],
        [u_final.y, v_final.y],
        [u_final.z, v_final.z],
        [u_final.w, v_final.w],
    ]
}

/// Original scalar implementation - kept for compatibility and testing
/// Berechnet UVs aus der Face-Basis für beliebige Quad-Ecken.
/// Damit bleiben Side-Texturen auf North/South/East/West konsistent orientiert
/// und Greedy-Quads behalten Tiling über die UV-Range 0..N.
fn face_uvs_for_quad_scalar(face: Face, corners: [[i32; 3]; 4]) -> [[f32; 2]; 4] {
    let (u_axis, v_axis) = face_uv_basis(face);
    let mut uvs = [[0.0; 2]; 4];
    let mut projected = [(0_i32, 0_i32); 4];
    let mut u_min = i32::MAX;
    let mut v_min = i32::MAX;

    for (i, corner) in corners.iter().enumerate() {
        let u = project_axis(*corner, u_axis);
        let v = project_axis(*corner, v_axis);
        projected[i] = (u, v);
        u_min = u_min.min(u);
        v_min = v_min.min(v);
    }

    for (i, (u, v)) in projected.iter().enumerate() {
        uvs[i] = [(*u - u_min) as f32, (*v - v_min) as f32];
    }

    uvs
}

/// Batch UV calculation for multiple quads using SIMD
/// This is where SIMD really shines - processing multiple quads simultaneously
pub(super) fn batch_face_uvs_for_quads_simd(
    face: Face,
    quads_corners: &[[[i32; 3]; 4]],
) -> Vec<[[f32; 2]; 4]> {
    let mut results = Vec::with_capacity(quads_corners.len());

    // Process quads in batches that fit efficiently into SIMD lanes
    for quad_corners in quads_corners.iter() {
        // For now, call the optimized single-quad SIMD function
        // In a real optimization, we'd process multiple quads in parallel
        results.push(face_uvs_for_quad_simd(face, *quad_corners));
    }

    results
}

/// Chooses the optimal UV calculation implementation
/// For small quad calculations, scalar is actually faster due to overhead
#[inline]
fn face_uvs_for_quad(face: Face, corners: [[i32; 3]; 4]) -> [[f32; 2]; 4] {
    // For single quad calculations, scalar is faster
    face_uvs_for_quad_scalar(face, corners)
}

#[inline(always)]
fn neighbor_covers_boundary(
    neighbor_shape: OcclusionShape,
    neighbor_face: Face,
    requested: FaceCoverage,
) -> bool {
    neighbor_shape
        .boundary_coverage(neighbor_face)
        .covers(requested)
}

#[inline]
fn should_emit_cube_face(
    world: &impl BlockAccess,
    texture_lookup: &BlockTextureLookup,
    current_pos: IVec3,
    current: BlockId,
    face: Face,
) -> bool {
    let normal = face.normal_i32();
    let neighbor_pos = current_pos + IVec3::new(normal[0], normal[1], normal[2]);
    let neighbor = world.get_block(neighbor_pos);
    if !neighbor.is_voxel_filled() {
        return true;
    }

    let neighbor_info = texture_lookup.info_for(neighbor);
    let current_group = texture_lookup.cull_group_for(current);
    let neighbor_group = neighbor_info.cull_group;
    if current_group != CullGroup::None && current_group == neighbor_group {
        if let Some(kind) = fluid_kind(current) {
            if matches!(face, Face::North | Face::South | Face::East | Face::West) {
                let h_current = fluid_surface_height_at(world, current_pos, kind);
                let h_neighbor = fluid_surface_height_at(world, neighbor_pos, kind);
                return h_neighbor + 1e-6 < h_current;
            }
        }
        if neighbor_covers_boundary(
            neighbor_info.occlusion_shape,
            face.opposite(),
            FaceCoverage::Full,
        ) {
            return false;
        }
    }

    !neighbor_covers_boundary(
        neighbor_info.occlusion_shape,
        face.opposite(),
        FaceCoverage::Full,
    )
}

#[inline]
fn should_emit_bottom_slab_face(
    world: &impl BlockAccess,
    texture_lookup: &BlockTextureLookup,
    current_pos: IVec3,
    current: BlockId,
    face: Face,
) -> bool {
    if face == Face::Up {
        return true;
    }

    let normal = face.normal_i32();
    let neighbor_pos = current_pos + IVec3::new(normal[0], normal[1], normal[2]);
    let neighbor = world.get_block(neighbor_pos);
    if !neighbor.is_voxel_filled() {
        return true;
    }
    if matches!(
        texture_lookup.model_for(current),
        BlockModel::LayerBottom { .. }
    ) && current == neighbor
        && matches!(face, Face::North | Face::South | Face::East | Face::West)
    {
        return false;
    }

    let neighbor_info = texture_lookup.info_for(neighbor);
    let required = match face {
        Face::Down => FaceCoverage::Full,
        Face::North | Face::South | Face::East | Face::West => FaceCoverage::LowerHalf,
        Face::Up => FaceCoverage::None,
    };
    let current_group = texture_lookup.cull_group_for(current);
    if current_group != CullGroup::None
        && current_group == neighbor_info.cull_group
        && neighbor_covers_boundary(neighbor_info.occlusion_shape, face.opposite(), required)
    {
        return false;
    }

    !neighbor_covers_boundary(neighbor_info.occlusion_shape, face.opposite(), required)
}

fn append_bottom_slabs_in_chunk(
    world: &impl BlockAccess,
    chunk: &crate::world::Chunk,
    chunk_origin: IVec3,
    chunk_offset: [f32; 3],
    texture_lookup: &BlockTextureLookup,
    requested_layers: MeshLayerMask,
    layered: &mut LayeredChunkGeometry,
) {
    let faces: [(Face, [[i32; 3]; 4]); 6] = [
        (Face::South, [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]]),
        (Face::North, [[1, 0, 0], [0, 0, 0], [0, 1, 0], [1, 1, 0]]),
        (Face::East, [[1, 0, 1], [1, 0, 0], [1, 1, 0], [1, 1, 1]]),
        (Face::West, [[0, 0, 0], [0, 0, 1], [0, 1, 1], [0, 1, 0]]),
        (Face::Up, [[0, 1, 1], [1, 1, 1], [1, 1, 0], [0, 1, 0]]),
        (Face::Down, [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]]),
    ];

    for y in 0..CHUNK_SIZE {
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let id = chunk.get(x, y, z);
                let layer_height = match texture_lookup.model_for(id) {
                    BlockModel::SlabBottom => 8u8,
                    BlockModel::LayerBottom { height_px } => height_px,
                    _ => continue,
                };
                if !id.is_voxel_filled() {
                    continue;
                }
                let render_layer = texture_lookup.render_layer_for(id);
                if !requested_layers.contains_render_layer(render_layer) {
                    continue;
                }
                let layer_height_f = layer_height as f32 / 16.0;
                let render_bounds = RenderBounds::bottom_layer(layer_height_f);

                let world_pos = chunk_origin + IVec3::new(x, y, z);
                for (face, quad) in faces {
                    if !should_emit_bottom_slab_face(world, texture_lookup, world_pos, id, face) {
                        continue;
                    }

                    let mesh = layered.mesh_mut(render_layer);
                    let base_index = mesh.vertices.len() as u32;
                    let tex_layer = texture_lookup.layer_for(id, face);
                    let tint_kind = texture_lookup.tint_kind_for(id, face);
                    let mut quad_uvs = face_uvs_for_quad(face, quad);
                    if matches!(face, Face::North | Face::South | Face::East | Face::West) {
                        for uv in &mut quad_uvs {
                            uv[1] *= layer_height_f;
                        }
                    }

                    for (corner_index, p) in quad.iter().enumerate() {
                        let vertex_local = [p[0] as f32, p[1] as f32 * layer_height_f, p[2] as f32];
                        let (shade, light_packed) = face_vertex_shade_and_light(
                            world,
                            world_pos,
                            face,
                            render_bounds,
                            vertex_local,
                        );
                        mesh.vertices.push(WorldVertex {
                            position: [
                                x as f32 + vertex_local[0] + chunk_offset[0],
                                y as f32 + vertex_local[1] + chunk_offset[1],
                                z as f32 + vertex_local[2] + chunk_offset[2],
                            ],
                            uv: pack_uv(quad_uvs[corner_index][0], quad_uvs[corner_index][1]),
                            tex_layer,
                            shade: pack_shade(shade),
                            tint_kind: tint_kind as u8,
                            light_packed: light_packed as u8,
                            _pad: 0,
                        });
                    }

                    mesh.indices.extend_from_slice(&[
                        base_index,
                        base_index + 1,
                        base_index + 2,
                        base_index,
                        base_index + 2,
                        base_index + 3,
                    ]);
                }
            }
        }
    }
}

/// Baked face darkening, shared with the inventory/hotbar item previews so both
/// use the same shading model.
#[inline(always)]
pub(crate) fn face_shade(face: Face) -> f32 {
    match face {
        Face::Up => 1.0,
        Face::North | Face::South => 0.8,
        Face::East | Face::West => 0.6,
        Face::Down => 0.5,
    }
}

#[derive(Clone, Copy)]
struct RenderBounds {
    min: [f32; 3],
    max: [f32; 3],
}

impl RenderBounds {
    const UNIT_CUBE: Self = Self {
        min: [0.0, 0.0, 0.0],
        max: [1.0, 1.0, 1.0],
    };
    const LOWER_SLAB: Self = Self {
        min: [0.0, 0.0, 0.0],
        max: [1.0, 0.5, 1.0],
    };

    #[inline(always)]
    fn bottom_layer(height: f32) -> Self {
        Self {
            min: [0.0, 0.0, 0.0],
            max: [1.0, height.clamp(0.0, 1.0), 1.0],
        }
    }

    #[inline(always)]
    fn midpoint(self, axis: usize) -> f32 {
        (self.min[axis] + self.max[axis]) * 0.5
    }
}

#[inline(always)]
fn face_axes_indices(face: Face) -> (usize, usize) {
    match face {
        Face::North | Face::South => (0, 1),
        Face::East | Face::West => (2, 1),
        Face::Up | Face::Down => (0, 2),
    }
}

#[inline(always)]
fn face_normal_offset(face: Face) -> isize {
    match face {
        Face::East => 1,
        Face::West => -1,
        Face::Up => PADDED_CHUNK_STRIDE_Y as isize,
        Face::Down => -(PADDED_CHUNK_STRIDE_Y as isize),
        Face::South => PADDED_CHUNK_STRIDE_Z as isize,
        Face::North => -(PADDED_CHUNK_STRIDE_Z as isize),
    }
}

#[inline(always)]
fn face_normal_vec(face: Face) -> IVec3 {
    let normal = face.normal_i32();
    IVec3::new(normal[0], normal[1], normal[2])
}

#[inline(always)]
fn axis_offset(axis: usize, sign: i32) -> isize {
    match axis {
        0 => sign as isize,
        1 => sign as isize * PADDED_CHUNK_STRIDE_Y as isize,
        _ => sign as isize * PADDED_CHUNK_STRIDE_Z as isize,
    }
}

#[inline(always)]
fn offset_index(index: usize, offset: isize) -> usize {
    ((index as isize) + offset) as usize
}

#[inline(always)]
fn axis_sign_from_vertex_local(
    axis: usize,
    render_bounds: RenderBounds,
    vertex_local: [f32; 3],
) -> i32 {
    if vertex_local[axis] >= render_bounds.midpoint(axis) {
        1
    } else {
        -1
    }
}

#[inline]
fn axis_vec(axis: usize, sign: i32) -> IVec3 {
    match axis {
        0 => IVec3::new(sign, 0, 0),
        1 => IVec3::new(0, sign, 0),
        _ => IVec3::new(0, 0, sign),
    }
}

#[inline(always)]
fn sample_local_point(vertex_local: [f32; 3], cell_offset: IVec3) -> [f32; 3] {
    [
        vertex_local[0] - cell_offset.x as f32,
        vertex_local[1] - cell_offset.y as f32,
        vertex_local[2] - cell_offset.z as f32,
    ]
}

#[inline(always)]
fn occlusion_offsets_for_vertex(
    face: Face,
    render_bounds: RenderBounds,
    vertex_local: [f32; 3],
) -> [IVec3; 4] {
    let (u_axis, v_axis) = face_axes_indices(face);
    let su = axis_sign_from_vertex_local(u_axis, render_bounds, vertex_local);
    let sv = axis_sign_from_vertex_local(v_axis, render_bounds, vertex_local);
    let outer = face_normal_vec(face);
    let side1 = outer + axis_vec(u_axis, su);
    let side2 = outer + axis_vec(v_axis, sv);
    let corner = side1 + axis_vec(v_axis, sv);
    [outer, side1, side2, corner]
}

#[inline(always)]
fn validated_ao_factor(side1: bool, side2: bool, corner: bool) -> f32 {
    const MIN_AO: f32 = 0.5;
    let ao = if side1 && side2 {
        MIN_AO
    } else {
        match side1 as u8 + side2 as u8 + corner as u8 {
            0 => 1.0,
            1 => 0.85,
            2 => 0.7,
            _ => MIN_AO,
        }
    };
    if ao.is_finite() {
        ao.clamp(MIN_AO, 1.0)
    } else {
        1.0
    }
}

#[inline(always)]
fn averaged_vertex_light_from_samples(raw_lights: [u8; 4], occluded: [bool; 4]) -> u32 {
    let mut fallback_sky = 0u8;
    let mut fallback_blk = 0u8;
    let mut has_visible = false;

    for (packed, is_occluded) in raw_lights.iter().zip(occluded) {
        if is_occluded {
            continue;
        }
        has_visible = true;
        fallback_sky = fallback_sky.max((packed >> 4) & 0x0F);
        fallback_blk = fallback_blk.max(packed & 0x0F);
    }

    if !has_visible {
        fallback_sky = (raw_lights[0] >> 4) & 0x0F;
        fallback_blk = raw_lights[0] & 0x0F;
    }

    let mut sky_sum = 0u32;
    let mut blk_sum = 0u32;
    for (packed, is_occluded) in raw_lights.iter().zip(occluded) {
        let (sky, blk) = if is_occluded {
            (fallback_sky as u32, fallback_blk as u32)
        } else {
            (((packed >> 4) & 0x0F) as u32, (packed & 0x0F) as u32)
        };
        sky_sum += sky;
        blk_sum += blk;
    }

    pack_vertex_light(((sky_sum + 2) / 4) as u8, ((blk_sum + 2) / 4) as u8)
}

fn face_vertex_shade_and_light(
    world: &impl BlockAccess,
    block_pos: IVec3,
    face: Face,
    render_bounds: RenderBounds,
    vertex_local: [f32; 3],
) -> (f32, u32) {
    let sample_offsets = occlusion_offsets_for_vertex(face, render_bounds, vertex_local);
    let mut occluded = [false; 4];
    let mut raw_lights = [0u8; 4];
    for (index, cell_offset) in sample_offsets.iter().enumerate() {
        let sample_pos = block_pos + *cell_offset;
        let local_point = sample_local_point(vertex_local, *cell_offset);
        occluded[index] = world
            .get_block(sample_pos)
            .occlusion_shape()
            .occludes_ao_sample(local_point);
        raw_lights[index] = world.get_light_packed(sample_pos);
    }

    let side1_occ = occluded[1];
    let side2_occ = occluded[2];
    let corner_occ = occluded[3];
    let ao = if debug_disable_ao() {
        1.0
    } else {
        validated_ao_factor(side1_occ, side2_occ, corner_occ)
    };

    let light = averaged_vertex_light_from_samples(raw_lights, occluded);
    (face_shade(face) * ao, light)
}

fn cube_face_vertex_shade_and_light(
    world: &impl BlockAccess,
    block_pos: IVec3,
    face: Face,
    corner_pos: IVec3,
) -> (f32, u32) {
    face_vertex_shade_and_light(
        world,
        block_pos,
        face,
        RenderBounds::UNIT_CUBE,
        [
            (corner_pos.x - block_pos.x) as f32,
            (corner_pos.y - block_pos.y) as f32,
            (corner_pos.z - block_pos.z) as f32,
        ],
    )
}

#[inline]
fn face_vertex_shade_and_light_local(
    neighborhood: &MeshingNeighborhood<'_>,
    texture_lookup: &BlockTextureLookup,
    block_pos: IVec3,
    face: Face,
    render_bounds: RenderBounds,
    vertex_local: [f32; 3],
) -> (f32, u32) {
    let sample_offsets = occlusion_offsets_for_vertex(face, render_bounds, vertex_local);
    let mut occluded = [false; 4];
    let mut raw_lights = [0u8; 4];
    for (index, cell_offset) in sample_offsets.iter().enumerate() {
        let sample_pos = block_pos + *cell_offset;
        let local_point = sample_local_point(vertex_local, *cell_offset);
        occluded[index] = texture_lookup
            .info_for(neighborhood.block_local(sample_pos.x, sample_pos.y, sample_pos.z))
            .occlusion_shape
            .occludes_ao_sample(local_point);
        raw_lights[index] = neighborhood.light_local(sample_pos.x, sample_pos.y, sample_pos.z);
    }

    let side1_occ = occluded[1];
    let side2_occ = occluded[2];
    let corner_occ = occluded[3];
    let ao = if debug_disable_ao() {
        1.0
    } else {
        validated_ao_factor(side1_occ, side2_occ, corner_occ)
    };

    let light = averaged_vertex_light_from_samples(raw_lights, occluded);
    (face_shade(face) * ao, light)
}

#[inline]
fn cube_face_vertex_shade_and_light_local(
    neighborhood: &MeshingNeighborhood<'_>,
    texture_lookup: &BlockTextureLookup,
    block_pos: IVec3,
    face: Face,
    corner_pos: IVec3,
) -> (f32, u32) {
    face_vertex_shade_and_light_local(
        neighborhood,
        texture_lookup,
        block_pos,
        face,
        RenderBounds::UNIT_CUBE,
        [
            (corner_pos.x - block_pos.x) as f32,
            (corner_pos.y - block_pos.y) as f32,
            (corner_pos.z - block_pos.z) as f32,
        ],
    )
}

#[inline]
fn face_vertex_shade_and_light_local_fast(
    neighborhood: &MeshingNeighborhood<'_>,
    texture_lookup: &BlockTextureLookup,
    _block_pos: [i32; 3],
    block_index: usize,
    face: Face,
    render_bounds: RenderBounds,
    vertex_local: [f32; 3],
) -> (f32, u32) {
    let (u_axis, v_axis) = face_axes_indices(face);
    let su = axis_sign_from_vertex_local(u_axis, render_bounds, vertex_local);
    let sv = axis_sign_from_vertex_local(v_axis, render_bounds, vertex_local);
    let outer = offset_index(block_index, face_normal_offset(face));
    let side1 = offset_index(outer, axis_offset(u_axis, su));
    let side2 = offset_index(outer, axis_offset(v_axis, sv));
    let corner = offset_index(side1, axis_offset(v_axis, sv));

    let outer_offset = face_normal_vec(face);
    let side1_offset = outer_offset + axis_vec(u_axis, su);
    let side2_offset = outer_offset + axis_vec(v_axis, sv);
    let corner_offset = side1_offset + axis_vec(v_axis, sv);
    let side1_occ = texture_lookup
        .info_for(neighborhood.block_at_index(side1))
        .occlusion_shape
        .occludes_ao_sample(sample_local_point(vertex_local, side1_offset));
    let side2_occ = texture_lookup
        .info_for(neighborhood.block_at_index(side2))
        .occlusion_shape
        .occludes_ao_sample(sample_local_point(vertex_local, side2_offset));
    let corner_occ = texture_lookup
        .info_for(neighborhood.block_at_index(corner))
        .occlusion_shape
        .occludes_ao_sample(sample_local_point(vertex_local, corner_offset));
    let ao = if debug_disable_ao() {
        1.0
    } else {
        validated_ao_factor(side1_occ, side2_occ, corner_occ)
    };

    let raw_lights = [
        neighborhood.light_at_index(outer),
        neighborhood.light_at_index(side1),
        neighborhood.light_at_index(side2),
        neighborhood.light_at_index(corner),
    ];
    let occluded = [false, side1_occ, side2_occ, corner_occ];
    (
        face_shade(face) * ao,
        averaged_vertex_light_from_samples(raw_lights, occluded),
    )
}

#[inline]
fn cube_face_vertex_shade_and_light_local_fast(
    neighborhood: &MeshingNeighborhood<'_>,
    texture_lookup: &BlockTextureLookup,
    block_pos: [i32; 3],
    block_index: usize,
    face: Face,
    corner_pos: [i32; 3],
) -> (f32, u32) {
    face_vertex_shade_and_light_local_fast(
        neighborhood,
        texture_lookup,
        block_pos,
        block_index,
        face,
        RenderBounds::UNIT_CUBE,
        [
            (corner_pos[0] - block_pos[0]) as f32,
            (corner_pos[1] - block_pos[1]) as f32,
            (corner_pos[2] - block_pos[2]) as f32,
        ],
    )
}

#[inline]
fn should_emit_cube_face_local(
    neighborhood: &MeshingNeighborhood<'_>,
    texture_lookup: &BlockTextureLookup,
    current_local: IVec3,
    current: BlockId,
    face: Face,
) -> bool {
    let normal = face.normal_i32();
    let neighbor = neighborhood.block_local(
        current_local.x + normal[0],
        current_local.y + normal[1],
        current_local.z + normal[2],
    );
    if !neighbor.is_voxel_filled() {
        return true;
    }

    let neighbor_info = texture_lookup.info_for(neighbor);
    let current_group = texture_lookup.cull_group_for(current);
    if current_group != CullGroup::None && current_group == neighbor_info.cull_group {
        if neighbor_covers_boundary(
            neighbor_info.occlusion_shape,
            face.opposite(),
            FaceCoverage::Full,
        ) {
            return false;
        }
    }

    !neighbor_covers_boundary(
        neighbor_info.occlusion_shape,
        face.opposite(),
        FaceCoverage::Full,
    )
}

#[inline]
fn should_emit_cube_face_local_fast(
    neighborhood: &MeshingNeighborhood<'_>,
    texture_lookup: &BlockTextureLookup,
    current_info: crate::blocks::BlockTextureInfo,
    current_index: usize,
    face: Face,
) -> bool {
    let neighbor =
        neighborhood.block_at_index(offset_index(current_index, face_normal_offset(face)));
    let neighbor_info = texture_lookup.info_for(neighbor);
    if !neighbor.is_voxel_filled() {
        return true;
    }

    if current_info.cull_group != CullGroup::None
        && current_info.cull_group == neighbor_info.cull_group
        && neighbor_covers_boundary(
            neighbor_info.occlusion_shape,
            face.opposite(),
            FaceCoverage::Full,
        )
    {
        return false;
    }

    !neighbor_covers_boundary(
        neighbor_info.occlusion_shape,
        face.opposite(),
        FaceCoverage::Full,
    )
}

#[inline]
fn greedy_vertex_owning_block_pos(
    corner_world_i: IVec3,
    origin_plane: [i32; 3],
    d: usize,
    u: usize,
    v: usize,
    w: i32,
    h: i32,
    normal_positive: bool,
) -> IVec3 {
    let mut p = [corner_world_i.x, corner_world_i.y, corner_world_i.z];
    if normal_positive {
        p[d] -= 1;
    }
    if p[u] == origin_plane[u] + w {
        p[u] -= 1;
    }
    if p[v] == origin_plane[v] + h {
        p[v] -= 1;
    }
    IVec3::new(p[0], p[1], p[2])
}

pub(crate) fn build_chunk_mesh_layered_naive_dense_pooled(
    neighborhood: &MeshingNeighborhood<'_>,
    coord: ChunkCoord,
    texture_lookup: &BlockTextureLookup,
    pool: &mut super::pool::MeshingPool,
    requested_layers: MeshLayerMask,
) -> (LayeredChunkGeometry, MeshGenerationStats) {
    let mut layered = LayeredChunkGeometry::default();
    let mut stats = MeshGenerationStats::default();
    if requested_layers.is_empty() {
        return (layered, stats);
    }
    stats.scratch_buffers_reused = pool.scratch_buffers_reused();
    stats.capacity_hints_used = pool.prepare_layered_geometry(&mut layered);
    build_chunk_mesh_layered_naive_dense_internal(
        neighborhood,
        coord,
        texture_lookup,
        pool,
        requested_layers,
        &mut layered,
        &mut stats,
    );
    stats.geometry_growth_events = pool.record_layered_geometry_usage(&layered);
    stats.quads_emitted = (layered.total_index_count() / 6) as u32;
    stats.opaque_quads = (layered.opaque.indices.len() / 6) as u32;
    stats.cutout_quads = (layered.cutout.indices.len() / 6) as u32;
    stats.transparent_quads = (layered.transparent.indices.len() / 6) as u32;
    (layered, stats)
}

fn build_chunk_mesh_layered_naive_dense_internal(
    neighborhood: &MeshingNeighborhood<'_>,
    coord: ChunkCoord,
    texture_lookup: &BlockTextureLookup,
    pool: &mut super::pool::MeshingPool,
    requested_layers: MeshLayerMask,
    layered: &mut LayeredChunkGeometry,
    stats: &mut MeshGenerationStats,
) {
    let chunk_offset = chunk_offset(coord);
    let chunk_origin = chunk_world_origin(coord);

    let faces: [(Face, [[i32; 3]; 4]); 6] = [
        (Face::South, [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]]),
        (Face::North, [[1, 0, 0], [0, 0, 0], [0, 1, 0], [1, 1, 0]]),
        (Face::East, [[1, 0, 1], [1, 0, 0], [1, 1, 0], [1, 1, 1]]),
        (Face::West, [[0, 0, 0], [0, 0, 1], [0, 1, 1], [0, 1, 0]]),
        (Face::Up, [[0, 1, 1], [1, 1, 1], [1, 1, 0], [0, 1, 0]]),
        (Face::Down, [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]]),
    ];

    for y in 0..CHUNK_SIZE {
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                stats.voxels_visited = stats.voxels_visited.saturating_add(1);
                let id = neighborhood.block_local(x, y, z);
                if !id.is_voxel_filled() {
                    continue;
                }
                let info = texture_lookup.info_for(id);
                if info.model != BlockModel::Cube {
                    continue;
                }
                if !requested_layers.contains_render_layer(info.render_layer) {
                    continue;
                }

                let local_pos = [x, y, z];
                let local_index = MeshingNeighborhood::local_padded_index(x, y, z);
                for (face, quad) in faces {
                    stats.cube_faces_considered = stats.cube_faces_considered.saturating_add(1);
                    if !should_emit_cube_face_local_fast(
                        neighborhood,
                        texture_lookup,
                        info,
                        local_index,
                        face,
                    ) {
                        continue;
                    }
                    stats.cube_faces_emitted = stats.cube_faces_emitted.saturating_add(1);

                    let render_layer = info.render_layer;
                    let mesh = layered.mesh_mut(render_layer);
                    let base_index = mesh.vertices.len() as u32;
                    let tex_layer = texture_lookup.layer_for_info(info, face);
                    let tint_kind = texture_lookup.tint_kind_for_info(info, face);
                    let quad_world = [
                        [x + quad[0][0], y + quad[0][1], z + quad[0][2]],
                        [x + quad[1][0], y + quad[1][1], z + quad[1][2]],
                        [x + quad[2][0], y + quad[2][1], z + quad[2][2]],
                        [x + quad[3][0], y + quad[3][1], z + quad[3][2]],
                    ];
                    let quad_uvs = face_uvs_for_quad(face, quad_world);

                    for (corner_index, p) in quad.iter().enumerate() {
                        let corner_local = [x + p[0], y + p[1], z + p[2]];
                        let (shade, light_packed) = cube_face_vertex_shade_and_light_local_fast(
                            neighborhood,
                            texture_lookup,
                            local_pos,
                            local_index,
                            face,
                            corner_local,
                        );
                        mesh.vertices.push(WorldVertex {
                            position: [
                                (x + p[0]) as f32 + chunk_offset[0],
                                (y + p[1]) as f32 + chunk_offset[1],
                                (z + p[2]) as f32 + chunk_offset[2],
                            ],
                            uv: pack_uv(quad_uvs[corner_index][0], quad_uvs[corner_index][1]),
                            tex_layer,
                            shade: pack_shade(shade),
                            tint_kind: tint_kind as u8,
                            light_packed: light_packed as u8,
                            _pad: 0,
                        });
                    }

                    mesh.indices.extend_from_slice(&[
                        base_index,
                        base_index + 1,
                        base_index + 2,
                        base_index,
                        base_index + 2,
                        base_index + 3,
                    ]);
                }
            }
        }
    }

    use super::liquids::append_liquids_in_chunk_pooled;
    use super::plants::append_cross_plants_in_chunk_pooled;

    append_bottom_slabs_in_chunk(
        neighborhood,
        neighborhood.center_chunk(),
        chunk_origin,
        chunk_offset,
        texture_lookup,
        requested_layers,
        layered,
    );
    if requested_layers.contains(MeshLayerMask::CUTOUT) {
        append_cross_plants_in_chunk_pooled(
            neighborhood.center_chunk(),
            chunk_offset,
            texture_lookup,
            &mut layered.cutout,
            Some(pool),
        );
    }
    append_liquids_in_chunk_pooled(
        neighborhood,
        coord,
        chunk_offset,
        texture_lookup,
        requested_layers,
        layered,
        Some(pool),
    );
}

pub(crate) fn build_chunk_mesh_greedy_layered_dense_pooled(
    neighborhood: &MeshingNeighborhood<'_>,
    coord: ChunkCoord,
    texture_lookup: &BlockTextureLookup,
    pool: &mut super::pool::MeshingPool,
    requested_layers: MeshLayerMask,
) -> (LayeredChunkGeometry, MeshGenerationStats) {
    let mut layered = LayeredChunkGeometry::default();
    let mut stats = MeshGenerationStats::default();
    if requested_layers.is_empty() {
        return (layered, stats);
    }
    stats.scratch_buffers_reused = pool.scratch_buffers_reused();
    stats.capacity_hints_used = pool.prepare_layered_geometry(&mut layered);
    build_chunk_mesh_greedy_layered_dense_internal(
        neighborhood,
        coord,
        texture_lookup,
        pool,
        requested_layers,
        &mut layered,
        &mut stats,
    );
    stats.geometry_growth_events = pool.record_layered_geometry_usage(&layered);
    stats.quads_emitted = (layered.total_index_count() / 6) as u32;
    stats.opaque_quads = (layered.opaque.indices.len() / 6) as u32;
    stats.cutout_quads = (layered.cutout.indices.len() / 6) as u32;
    stats.transparent_quads = (layered.transparent.indices.len() / 6) as u32;
    (layered, stats)
}

fn build_chunk_mesh_greedy_layered_dense_internal(
    neighborhood: &MeshingNeighborhood<'_>,
    coord: ChunkCoord,
    texture_lookup: &BlockTextureLookup,
    pool: &mut super::pool::MeshingPool,
    requested_layers: MeshLayerMask,
    layered: &mut LayeredChunkGeometry,
    stats: &mut MeshGenerationStats,
) {
    let chunk_offset = chunk_offset(coord);
    let chunk_origin = chunk_world_origin(coord);

    let mask = &mut pool.greedy_mask;
    mask.clear();
    mask.resize((CHUNK_SIZE * CHUNK_SIZE) as usize, None);
    let mut x = [0_i32; 3];
    for d in 0..3 {
        let u = (d + 1) % 3;
        let v = (d + 2) % 3;
        let mut q = [0_i32; 3];
        q[d] = 1;
        let face_positive = match d {
            0 => Face::East,
            1 => Face::Up,
            _ => Face::South,
        };
        let face_negative = match d {
            0 => Face::West,
            1 => Face::Down,
            _ => Face::North,
        };

        for slice in -1..CHUNK_SIZE {
            x[d] = slice;

            let mut n = 0usize;
            for j in 0..CHUNK_SIZE {
                x[v] = j;
                for i in 0..CHUNK_SIZE {
                    x[u] = i;
                    stats.voxels_visited = stats.voxels_visited.saturating_add(1);

                    let a_local = IVec3::new(x[0], x[1], x[2]);
                    let b_local = IVec3::new(x[0] + q[0], x[1] + q[1], x[2] + q[2]);
                    let a = if slice >= 0 {
                        neighborhood.block_local(a_local.x, a_local.y, a_local.z)
                    } else {
                        BlockId::Air
                    };
                    let b = if slice < CHUNK_SIZE - 1 {
                        neighborhood.block_local(b_local.x, b_local.y, b_local.z)
                    } else {
                        BlockId::Air
                    };
                    let a_info = texture_lookup.info_for(a);
                    let b_info = texture_lookup.info_for(b);
                    let a_cube = a.is_voxel_filled()
                        && a_info.model == BlockModel::Cube
                        && requested_layers.contains_render_layer(a_info.render_layer);
                    let b_cube = b.is_voxel_filled()
                        && b_info.model == BlockModel::Cube
                        && requested_layers.contains_render_layer(b_info.render_layer);
                    let a_index = if slice >= 0 {
                        Some(MeshingNeighborhood::local_padded_index(
                            a_local.x, a_local.y, a_local.z,
                        ))
                    } else {
                        None
                    };
                    let b_index = if slice < CHUNK_SIZE - 1 {
                        Some(MeshingNeighborhood::local_padded_index(
                            b_local.x, b_local.y, b_local.z,
                        ))
                    } else {
                        None
                    };

                    mask[n] = if a_cube && {
                        stats.cube_faces_considered = stats.cube_faces_considered.saturating_add(1);
                        should_emit_cube_face_local_fast(
                            neighborhood,
                            texture_lookup,
                            a_info,
                            a_index.expect("a index should exist"),
                            face_positive,
                        )
                    } {
                        stats.cube_faces_emitted = stats.cube_faces_emitted.saturating_add(1);
                        Some(MaskCell {
                            block: a,
                            normal_positive: true,
                        })
                    } else if b_cube && {
                        stats.cube_faces_considered = stats.cube_faces_considered.saturating_add(1);
                        should_emit_cube_face_local_fast(
                            neighborhood,
                            texture_lookup,
                            b_info,
                            b_index.expect("b index should exist"),
                            face_negative,
                        )
                    } {
                        stats.cube_faces_emitted = stats.cube_faces_emitted.saturating_add(1);
                        Some(MaskCell {
                            block: b,
                            normal_positive: false,
                        })
                    } else {
                        None
                    };
                    n += 1;
                }
            }

            x[d] = slice + 1;

            let mut j = 0_i32;
            while j < CHUNK_SIZE {
                let mut i = 0_i32;
                while i < CHUNK_SIZE {
                    let idx = (j * CHUNK_SIZE + i) as usize;
                    let Some(cell) = mask[idx] else {
                        i += 1;
                        continue;
                    };

                    let mut w = 1_i32;
                    while i + w < CHUNK_SIZE {
                        if mask[(j * CHUNK_SIZE + (i + w)) as usize] != Some(cell) {
                            break;
                        }
                        w += 1;
                    }

                    let mut h = 1_i32;
                    'height: while j + h < CHUNK_SIZE {
                        for k in 0..w {
                            if mask[((j + h) * CHUNK_SIZE + (i + k)) as usize] != Some(cell) {
                                break 'height;
                            }
                        }
                        h += 1;
                    }

                    let mut du = [0_i32; 3];
                    let mut dv = [0_i32; 3];
                    du[u] = w;
                    dv[v] = h;

                    let origin = {
                        x[u] = i;
                        x[v] = j;
                        x
                    };

                    let p = |px: [i32; 3]| -> [f32; 3] {
                        [
                            (px[0] as f32) + chunk_offset[0],
                            (px[1] as f32) + chunk_offset[1],
                            (px[2] as f32) + chunk_offset[2],
                        ]
                    };

                    let o = origin;
                    let p0 = o;
                    let p1 = [o[0] + du[0], o[1] + du[1], o[2] + du[2]];
                    let p2 = [
                        o[0] + du[0] + dv[0],
                        o[1] + du[1] + dv[1],
                        o[2] + du[2] + dv[2],
                    ];
                    let p3 = [o[0] + dv[0], o[1] + dv[1], o[2] + dv[2]];

                    let face = match (d, cell.normal_positive) {
                        (0, true) => Face::East,
                        (0, false) => Face::West,
                        (1, true) => Face::Up,
                        (1, false) => Face::Down,
                        (2, true) => Face::South,
                        (2, false) => Face::North,
                        _ => Face::Up,
                    };
                    let cell_info = texture_lookup.info_for(cell.block);
                    let render_layer = cell_info.render_layer;
                    let mesh = layered.mesh_mut(render_layer);
                    let base = mesh.vertices.len() as u32;
                    let tex_layer = texture_lookup.layer_for_info(cell_info, face);
                    let tint_kind = texture_lookup.tint_kind_for_info(cell_info, face);
                    let quad_uvs = face_uvs_for_quad(face, [p0, p1, p2, p3]);

                    let uv0 = quad_uvs[0];
                    let uv1 = quad_uvs[1];
                    let uv2 = quad_uvs[2];
                    let uv3 = quad_uvs[3];
                    let quad = if cell.normal_positive {
                        [(p0, uv0), (p1, uv1), (p2, uv2), (p3, uv3)]
                    } else {
                        [(p0, uv0), (p3, uv3), (p2, uv2), (p1, uv1)]
                    };
                    let origin_plane_local = o;

                    for (qp, uv) in quad {
                        let corner_local = [qp[0], qp[1], qp[2]];
                        let corner_local_i = IVec3::new(qp[0], qp[1], qp[2]);
                        let block_local = greedy_vertex_owning_block_pos(
                            corner_local_i,
                            origin_plane_local,
                            d,
                            u,
                            v,
                            w,
                            h,
                            cell.normal_positive,
                        );
                        let block_local_arr = [block_local.x, block_local.y, block_local.z];
                        let block_index = MeshingNeighborhood::local_padded_index(
                            block_local.x,
                            block_local.y,
                            block_local.z,
                        );
                        let (shade, light_packed) = cube_face_vertex_shade_and_light_local_fast(
                            neighborhood,
                            texture_lookup,
                            block_local_arr,
                            block_index,
                            face,
                            corner_local,
                        );
                        mesh.vertices.push(WorldVertex {
                            position: p(qp),
                            uv: pack_uv(uv[0], uv[1]),
                            tex_layer,
                            shade: pack_shade(shade),
                            tint_kind: tint_kind as u8,
                            light_packed: light_packed as u8,
                            _pad: 0,
                        });
                    }
                    mesh.indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base,
                        base + 2,
                        base + 3,
                    ]);

                    for dy in 0..h {
                        for dx in 0..w {
                            mask[((j + dy) * CHUNK_SIZE + (i + dx)) as usize] = None;
                        }
                    }

                    i += w;
                }
                j += 1;
            }
        }
    }

    use super::liquids::append_liquids_in_chunk_pooled;
    use super::plants::append_cross_plants_in_chunk_pooled;

    append_bottom_slabs_in_chunk(
        neighborhood,
        neighborhood.center_chunk(),
        chunk_origin,
        chunk_offset,
        texture_lookup,
        requested_layers,
        layered,
    );
    if requested_layers.contains(MeshLayerMask::CUTOUT) {
        append_cross_plants_in_chunk_pooled(
            neighborhood.center_chunk(),
            chunk_offset,
            texture_lookup,
            &mut layered.cutout,
            Some(pool),
        );
    }
    append_liquids_in_chunk_pooled(
        neighborhood,
        coord,
        chunk_offset,
        texture_lookup,
        requested_layers,
        layered,
        Some(pool),
    );
}

/// Baut ein Layered-Mesh (Opaque/Cutout/Transparent) aus einem Chunk.
/// Naives Meshing: Jede sichtbare Face -> 1 Quad -> 2 Triangles.
/// Pool-aware version for improved memory allocation behavior.
pub(crate) fn build_chunk_mesh_layered_naive_pooled(
    world: &impl BlockAccess,
    coord: ChunkCoord,
    texture_lookup: &BlockTextureLookup,
    pool: &mut super::pool::MeshingPool,
) -> LayeredChunkGeometry {
    let mut layered = LayeredChunkGeometry::default();
    let Some(chunk) = world.get_chunk(coord) else {
        return layered;
    };
    build_chunk_mesh_layered_naive_internal(
        world,
        coord,
        chunk,
        texture_lookup,
        pool,
        &mut layered,
    );
    layered
}

/// Baut ein Layered-Mesh (Opaque/Cutout/Transparent) aus einem Chunk.
/// Naives Meshing: Jede sichtbare Face -> 1 Quad -> 2 Triangles.
pub(crate) fn build_chunk_mesh_layered_naive(
    world: &impl BlockAccess,
    coord: ChunkCoord,
    texture_lookup: &BlockTextureLookup,
) -> LayeredChunkGeometry {
    super::pool::with_meshing_pool(|pool| {
        build_chunk_mesh_layered_naive_pooled(world, coord, texture_lookup, pool)
    })
}

fn build_chunk_mesh_layered_naive_internal(
    world: &impl BlockAccess,
    coord: ChunkCoord,
    chunk: &crate::world::Chunk,
    texture_lookup: &BlockTextureLookup,
    pool: &mut super::pool::MeshingPool,
    layered: &mut LayeredChunkGeometry,
) {
    let chunk_offset = chunk_offset(coord);
    let chunk_origin = chunk_world_origin(coord);

    // 6 Richtungen: (dx,dy,dz)
    // plus pro Richtung die 4 Eckpunkte des Quads (in lokaler Blockspace)
    //
    // Wir bauen pro Face 4 Vertices in CCW-Order (FrontFace CCW).
    // Koordinaten sind im Blockspace: ein Block ist [0..1] in x/y/z,
    // dann addieren wir Blockposition (x,y,z).
    let faces: [(Face, [[i32; 3]; 4]); 6] = [
        // +Z
        (Face::South, [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]]),
        // -Z
        (Face::North, [[1, 0, 0], [0, 0, 0], [0, 1, 0], [1, 1, 0]]),
        // +X
        (Face::East, [[1, 0, 1], [1, 0, 0], [1, 1, 0], [1, 1, 1]]),
        // -X
        (Face::West, [[0, 0, 0], [0, 0, 1], [0, 1, 1], [0, 1, 0]]),
        // +Y
        (Face::Up, [[0, 1, 1], [1, 1, 1], [1, 1, 0], [0, 1, 0]]),
        // -Y
        (Face::Down, [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]]),
    ];

    for y in 0..CHUNK_SIZE {
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let id = chunk.get(x, y, z);
                if !id.is_voxel_filled() {
                    continue;
                }
                if texture_lookup.model_for(id) != BlockModel::Cube {
                    continue;
                }

                for (face, quad) in faces {
                    let world_pos = chunk_origin + IVec3::new(x, y, z);
                    if !should_emit_cube_face(world, texture_lookup, world_pos, id, face) {
                        continue;
                    }

                    let render_layer = texture_lookup.render_layer_for(id);
                    let mesh = layered.mesh_mut(render_layer);
                    let base_index = mesh.vertices.len() as u32;
                    let tex_layer = texture_lookup.layer_for(id, face);
                    let tint_kind = texture_lookup.tint_kind_for(id, face);
                    let quad_world = [
                        [x + quad[0][0], y + quad[0][1], z + quad[0][2]],
                        [x + quad[1][0], y + quad[1][1], z + quad[1][2]],
                        [x + quad[2][0], y + quad[2][1], z + quad[2][2]],
                        [x + quad[3][0], y + quad[3][1], z + quad[3][2]],
                    ];
                    let quad_uvs = face_uvs_for_quad(face, quad_world);

                    // 4 Vertices fürs Quad
                    for (corner_index, p) in quad.iter().enumerate() {
                        let corner_world_i = world_pos + IVec3::new(p[0], p[1], p[2]);
                        let (shade, light_packed) = cube_face_vertex_shade_and_light(
                            world,
                            world_pos,
                            face,
                            corner_world_i,
                        );
                        mesh.vertices.push(WorldVertex {
                            position: [
                                (x + p[0]) as f32 + chunk_offset[0],
                                (y + p[1]) as f32 + chunk_offset[1],
                                (z + p[2]) as f32 + chunk_offset[2],
                            ],
                            uv: pack_uv(quad_uvs[corner_index][0], quad_uvs[corner_index][1]),
                            tex_layer,
                            shade: pack_shade(shade),
                            tint_kind: tint_kind as u8,
                            light_packed: light_packed as u8,
                            _pad: 0,
                        });
                    }

                    // 2 Triangles: (0,1,2) und (0,2,3)
                    mesh.indices.extend_from_slice(&[
                        base_index + 0,
                        base_index + 1,
                        base_index + 2,
                        base_index + 0,
                        base_index + 2,
                        base_index + 3,
                    ]);
                }
            }
        }
    }

    use super::liquids::append_liquids_in_chunk_pooled;
    use super::plants::append_cross_plants_in_chunk_pooled;

    append_bottom_slabs_in_chunk(
        world,
        chunk,
        chunk_origin,
        chunk_offset,
        texture_lookup,
        MeshLayerMask::ALL,
        layered,
    );
    append_cross_plants_in_chunk_pooled(
        chunk,
        chunk_offset,
        texture_lookup,
        &mut layered.cutout,
        Some(pool),
    );
    append_liquids_in_chunk_pooled(
        world,
        coord,
        chunk_offset,
        texture_lookup,
        MeshLayerMask::ALL,
        layered,
        Some(pool),
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MaskCell {
    pub(crate) block: BlockId,
    pub(crate) normal_positive: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MeshGenerationStats {
    pub(crate) voxels_visited: u32,
    pub(crate) cube_faces_considered: u32,
    pub(crate) cube_faces_emitted: u32,
    pub(crate) quads_emitted: u32,
    pub(crate) opaque_quads: u32,
    pub(crate) cutout_quads: u32,
    pub(crate) transparent_quads: u32,
    pub(crate) scratch_buffers_reused: u32,
    pub(crate) capacity_hints_used: u32,
    pub(crate) geometry_growth_events: u32,
}

/// Greedy-Meshing pro Achse:
/// - Wir bauen pro Slice eine 2D-Maske mit sichtbaren Faces (inkl. Richtung + BlockId).
/// - In der Maske werden gleichartige, benachbarte Einträge zu maximalen Rechtecken gemerged.
/// - Pro Rechteck entsteht genau ein Quad.
/// Pool-aware version for improved memory allocation behavior.
pub(crate) fn build_chunk_mesh_greedy_layered_pooled(
    world: &impl BlockAccess,
    coord: ChunkCoord,
    texture_lookup: &BlockTextureLookup,
    pool: &mut super::pool::MeshingPool,
) -> LayeredChunkGeometry {
    let mut layered = LayeredChunkGeometry::default();
    let Some(chunk) = world.get_chunk(coord) else {
        return layered;
    };
    build_chunk_mesh_greedy_layered_internal(
        world,
        coord,
        chunk,
        texture_lookup,
        pool,
        &mut layered,
    );
    layered
}

/// Greedy-Meshing pro Achse:
/// - Wir bauen pro Slice eine 2D-Maske mit sichtbaren Faces (inkl. Richtung + BlockId).
/// - In der Maske werden gleichartige, benachbarte Einträge zu maximalen Rechtecken gemerged.
/// - Pro Rechteck entsteht genau ein Quad.
pub(crate) fn build_chunk_mesh_greedy_layered(
    world: &impl BlockAccess,
    coord: ChunkCoord,
    texture_lookup: &BlockTextureLookup,
) -> LayeredChunkGeometry {
    super::pool::with_meshing_pool(|pool| {
        build_chunk_mesh_greedy_layered_pooled(world, coord, texture_lookup, pool)
    })
}

fn build_chunk_mesh_greedy_layered_internal(
    world: &impl BlockAccess,
    coord: ChunkCoord,
    chunk: &crate::world::Chunk,
    texture_lookup: &BlockTextureLookup,
    pool: &mut super::pool::MeshingPool,
    layered: &mut LayeredChunkGeometry,
) {
    let chunk_offset = chunk_offset(coord);
    let chunk_origin = chunk_world_origin(coord);

    // Use pooled mask buffer to avoid allocation
    let mask = &mut pool.greedy_mask;
    mask.clear();
    mask.resize((CHUNK_SIZE * CHUNK_SIZE) as usize, None);
    let mut x = [0_i32; 3];
    for d in 0..3 {
        let u = (d + 1) % 3;
        let v = (d + 2) % 3;
        let mut q = [0_i32; 3];
        q[d] = 1;
        let face_positive = match d {
            0 => Face::East,
            1 => Face::Up,
            _ => Face::South,
        };
        let face_negative = match d {
            0 => Face::West,
            1 => Face::Down,
            _ => Face::North,
        };

        for slice in -1..CHUNK_SIZE {
            x[d] = slice;

            // Maskenaufbau:
            // Ein Eintrag ist genau dann gesetzt, wenn zwischen a/b eine sichtbare Fläche liegt.
            // Gespeichert wird BlockId + Flächennormalen-Richtung (positiv/negativ entlang Achse d).
            let mut n = 0usize;
            for j in 0..CHUNK_SIZE {
                x[v] = j;
                for i in 0..CHUNK_SIZE {
                    x[u] = i;

                    let a_local = IVec3::new(x[0], x[1], x[2]);
                    let b_local = IVec3::new(x[0] + q[0], x[1] + q[1], x[2] + q[2]);
                    let a_pos = chunk_origin + a_local;
                    let b_pos = chunk_origin + b_local;
                    let a = if slice >= 0 {
                        world.get_block(a_pos)
                    } else {
                        BlockId::Air
                    };
                    let b = if slice < CHUNK_SIZE - 1 {
                        world.get_block(b_pos)
                    } else {
                        BlockId::Air
                    };
                    let a_cube =
                        a.is_voxel_filled() && texture_lookup.model_for(a) == BlockModel::Cube;
                    let b_cube =
                        b.is_voxel_filled() && texture_lookup.model_for(b) == BlockModel::Cube;

                    mask[n] = if a_cube
                        && should_emit_cube_face(world, texture_lookup, a_pos, a, face_positive)
                    {
                        Some(MaskCell {
                            block: a,
                            normal_positive: true,
                        })
                    } else if b_cube
                        && should_emit_cube_face(world, texture_lookup, b_pos, b, face_negative)
                    {
                        Some(MaskCell {
                            block: b,
                            normal_positive: false,
                        })
                    } else {
                        None
                    };
                    n += 1;
                }
            }

            // Wir emittieren Faces auf der Slice-Grenzfläche x[d] = slice + 1.
            x[d] = slice + 1;

            let mut j = 0_i32;
            while j < CHUNK_SIZE {
                let mut i = 0_i32;
                while i < CHUNK_SIZE {
                    let idx = (j * CHUNK_SIZE + i) as usize;
                    let Some(cell) = mask[idx] else {
                        i += 1;
                        continue;
                    };

                    // Rechteckbreite w finden.
                    let mut w = 1_i32;
                    while i + w < CHUNK_SIZE {
                        if mask[(j * CHUNK_SIZE + (i + w)) as usize] != Some(cell) {
                            break;
                        }
                        w += 1;
                    }

                    // Rechteckhöhe h finden.
                    let mut h = 1_i32;
                    'height: while j + h < CHUNK_SIZE {
                        for k in 0..w {
                            if mask[((j + h) * CHUNK_SIZE + (i + k)) as usize] != Some(cell) {
                                break 'height;
                            }
                        }
                        h += 1;
                    }

                    let mut du = [0_i32; 3];
                    let mut dv = [0_i32; 3];
                    du[u] = w;
                    dv[v] = h;

                    let origin = {
                        x[u] = i;
                        x[v] = j;
                        x
                    };

                    let p = |px: [i32; 3]| -> [f32; 3] {
                        [
                            (px[0] as f32) + chunk_offset[0],
                            (px[1] as f32) + chunk_offset[1],
                            (px[2] as f32) + chunk_offset[2],
                        ]
                    };

                    let o = origin;
                    let p0 = o;
                    let p1 = [o[0] + du[0], o[1] + du[1], o[2] + du[2]];
                    let p2 = [
                        o[0] + du[0] + dv[0],
                        o[1] + du[1] + dv[1],
                        o[2] + du[2] + dv[2],
                    ];
                    let p3 = [o[0] + dv[0], o[1] + dv[1], o[2] + dv[2]];

                    let face = match (d, cell.normal_positive) {
                        (0, true) => Face::East,
                        (0, false) => Face::West,
                        (1, true) => Face::Up,
                        (1, false) => Face::Down,
                        (2, true) => Face::South,
                        (2, false) => Face::North,
                        _ => Face::Up,
                    };
                    let render_layer = texture_lookup.render_layer_for(cell.block);
                    let mesh = layered.mesh_mut(render_layer);
                    let base = mesh.vertices.len() as u32;
                    let tex_layer = texture_lookup.layer_for(cell.block, face);
                    let tint_kind = texture_lookup.tint_kind_for(cell.block, face);
                    let quad_uvs = face_uvs_for_quad(face, [p0, p1, p2, p3]);

                    // UV-Tiling für gemergte Faces:
                    // statt (0..1) über das ganze Rechteck zu stretchen, nutzen wir
                    // die Face-Basis und die tatsächliche Quad-Ausdehnung (0..u_len, 0..v_len).
                    // Mit Repeat-Sampler wird die Blocktextur pro Block gekachelt.
                    let uv0 = quad_uvs[0];
                    let uv1 = quad_uvs[1];
                    let uv2 = quad_uvs[2];
                    let uv3 = quad_uvs[3];

                    // CCW-Winding:
                    // +Normal: p0,p1,p2,p3
                    // -Normal: p0,p3,p2,p1 (invertiert, damit FrontFace::Ccw konsistent bleibt)
                    let quad = if cell.normal_positive {
                        [(p0, uv0), (p1, uv1), (p2, uv2), (p3, uv3)]
                    } else {
                        [(p0, uv0), (p3, uv3), (p2, uv2), (p1, uv1)]
                    };
                    let origin_plane_world = [
                        chunk_origin.x + o[0],
                        chunk_origin.y + o[1],
                        chunk_origin.z + o[2],
                    ];

                    for (qp, uv) in quad {
                        let corner_world_i = chunk_origin + IVec3::new(qp[0], qp[1], qp[2]);
                        let block_pos = greedy_vertex_owning_block_pos(
                            corner_world_i,
                            origin_plane_world,
                            d,
                            u,
                            v,
                            w,
                            h,
                            cell.normal_positive,
                        );
                        let (shade, light_packed) = cube_face_vertex_shade_and_light(
                            world,
                            block_pos,
                            face,
                            corner_world_i,
                        );
                        mesh.vertices.push(WorldVertex {
                            position: p(qp),
                            uv: pack_uv(uv[0], uv[1]),
                            tex_layer,
                            shade: pack_shade(shade),
                            tint_kind: tint_kind as u8,
                            light_packed: light_packed as u8,
                            _pad: 0,
                        });
                    }
                    mesh.indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base,
                        base + 2,
                        base + 3,
                    ]);

                    // Die gemergte Region in der Maske leeren, damit sie nicht erneut verarbeitet wird.
                    for dy in 0..h {
                        for dx in 0..w {
                            mask[((j + dy) * CHUNK_SIZE + (i + dx)) as usize] = None;
                        }
                    }

                    i += w;
                }
                j += 1;
            }
        }
    }

    use super::liquids::append_liquids_in_chunk_pooled;
    use super::plants::append_cross_plants_in_chunk_pooled;

    append_bottom_slabs_in_chunk(
        world,
        chunk,
        chunk_origin,
        chunk_offset,
        texture_lookup,
        MeshLayerMask::ALL,
        layered,
    );
    append_cross_plants_in_chunk_pooled(
        chunk,
        chunk_offset,
        texture_lookup,
        &mut layered.cutout,
        Some(pool),
    );
    append_liquids_in_chunk_pooled(
        world,
        coord,
        chunk_offset,
        texture_lookup,
        MeshLayerMask::ALL,
        layered,
        Some(pool),
    );
}

#[cfg(test)]
pub(crate) fn build_chunk_mesh_greedy(
    world: &World,
    coord: ChunkCoord,
    texture_lookup: &BlockTextureLookup,
) -> (Vec<WorldVertex>, Vec<u32>) {
    flatten_layered_geometry(build_chunk_mesh_greedy_layered(
        world,
        coord,
        texture_lookup,
    ))
}

/// SIMD-optimized vertex creation for quads
/// Reduces temporary allocations and improves cache performance
#[inline]
pub(super) fn push_textured_quad_simd(
    mesh: &mut WorldCpuMeshData,
    positions: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    tex_layer: u32,
    tint_kind: u32,
) {
    let base = mesh.vertices.len() as u32;

    mesh.vertices.reserve(4);
    mesh.indices.reserve(6);

    let light = FULLBRIGHT_LIGHT_PACKED as u8;
    let tk = tint_kind as u8;

    mesh.vertices.extend_from_slice(&[
        WorldVertex {
            position: positions[0],
            uv: pack_uv(uvs[0][0], uvs[0][1]),
            tex_layer,
            shade: 255,
            tint_kind: tk,
            light_packed: light,
            _pad: 0,
        },
        WorldVertex {
            position: positions[1],
            uv: pack_uv(uvs[1][0], uvs[1][1]),
            tex_layer,
            shade: 255,
            tint_kind: tk,
            light_packed: light,
            _pad: 0,
        },
        WorldVertex {
            position: positions[2],
            uv: pack_uv(uvs[2][0], uvs[2][1]),
            tex_layer,
            shade: 255,
            tint_kind: tk,
            light_packed: light,
            _pad: 0,
        },
        WorldVertex {
            position: positions[3],
            uv: pack_uv(uvs[3][0], uvs[3][1]),
            tex_layer,
            shade: 255,
            tint_kind: tk,
            light_packed: light,
            _pad: 0,
        },
    ]);

    // Add indices for two triangles (quad = 2 triangles)
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Original implementation kept for fallback
pub(super) fn push_textured_quad_scalar(
    mesh: &mut WorldCpuMeshData,
    positions: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    tex_layer: u32,
    tint_kind: u32,
) {
    let base = mesh.vertices.len() as u32;
    for i in 0..4 {
        mesh.vertices.push(WorldVertex {
            position: positions[i],
            uv: pack_uv(uvs[i][0], uvs[i][1]),
            tex_layer,
            shade: 255,
            tint_kind: tint_kind as u8,
            light_packed: FULLBRIGHT_LIGHT_PACKED as u8,
            _pad: 0,
        });
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Optimized quad creation with improved memory allocation patterns
#[inline]
pub(super) fn push_textured_quad(
    mesh: &mut WorldCpuMeshData,
    positions: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    tex_layer: u32,
    tint_kind: u32,
) {
    push_textured_quad_simd(mesh, positions, uvs, tex_layer, tint_kind)
}

/// Batch quad creation that reserves memory upfront for better performance
/// Use this for large numbers of quads to reduce reallocations
#[inline]
pub(super) fn push_textured_quads_batch(
    mesh: &mut WorldCpuMeshData,
    quads: &[([[f32; 3]; 4], [[f32; 2]; 4], u32, u32)], // (positions, uvs, tex_layer, tint_kind)
) {
    let quad_count = quads.len();

    mesh.vertices.reserve(quad_count * 4);
    mesh.indices.reserve(quad_count * 6);

    let light = FULLBRIGHT_LIGHT_PACKED as u8;

    for (positions, uvs, tex_layer, tint_kind) in quads {
        let base = mesh.vertices.len() as u32;
        let tk = *tint_kind as u8;

        mesh.vertices.extend_from_slice(&[
            WorldVertex {
                position: positions[0],
                uv: pack_uv(uvs[0][0], uvs[0][1]),
                tex_layer: *tex_layer,
                shade: 255,
                tint_kind: tk,
                light_packed: light,
                _pad: 0,
            },
            WorldVertex {
                position: positions[1],
                uv: pack_uv(uvs[1][0], uvs[1][1]),
                tex_layer: *tex_layer,
                shade: 255,
                tint_kind: tk,
                light_packed: light,
                _pad: 0,
            },
            WorldVertex {
                position: positions[2],
                uv: pack_uv(uvs[2][0], uvs[2][1]),
                tex_layer: *tex_layer,
                shade: 255,
                tint_kind: tk,
                light_packed: light,
                _pad: 0,
            },
            WorldVertex {
                position: positions[3],
                uv: pack_uv(uvs[3][0], uvs[3][1]),
                tex_layer: *tex_layer,
                shade: 255,
                tint_kind: tk,
                light_packed: light,
                _pad: 0,
            },
        ]);

        // Add indices
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

#[cfg(test)]
mod simd_uv_tests {
    use super::*;
    use crate::blocks::{BlockRegistry, BlockTextureLookup, Face};
    use crate::meshing::{ChunkSnapshot, MeshingNeighborhoodScratch, NeighborhoodSnapshot};
    use glam::IVec3;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn test_texture_lookup() -> BlockTextureLookup {
        let registry = BlockRegistry::new();
        let key_to_layer: HashMap<String, u32> = registry
            .collect_texture_keys()
            .into_iter()
            .enumerate()
            .map(|(i, key)| (key, i as u32 + 1))
            .collect();
        BlockTextureLookup::new(&registry, &key_to_layer, 0)
    }

    fn snapshot_for(world: &World, coord: ChunkCoord) -> NeighborhoodSnapshot {
        let center = world
            .get_chunk(coord)
            .expect("missing center chunk for meshing snapshot");
        let center_snapshot = ChunkSnapshot {
            blocks: Arc::clone(&center.blocks),
            block_data: Arc::clone(&center.block_data),
            light: Arc::clone(&center.light),
        };
        let neighbors = [
            ChunkCoord {
                x: coord.x + 1,
                y: coord.y,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x - 1,
                y: coord.y,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y + 1,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y - 1,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y,
                z: coord.z + 1,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y,
                z: coord.z - 1,
            },
        ]
        .map(|neighbor_coord| {
            world.get_chunk(neighbor_coord).map(|chunk| ChunkSnapshot {
                blocks: Arc::clone(&chunk.blocks),
                block_data: Arc::clone(&chunk.block_data),
                light: Arc::clone(&chunk.light),
            })
        });
        NeighborhoodSnapshot {
            center: center_snapshot,
            neighbors,
        }
    }

    /// Test that SIMD and scalar UV calculation produce identical results
    #[test]
    fn test_uv_calculation_equivalence() {
        let test_cases = [
            // North face - regular block
            (Face::North, [[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0]]),
            // Greedy-merged face - 2x3 blocks
            (Face::Up, [[0, 5, 0], [2, 5, 0], [2, 5, 3], [0, 5, 3]]),
            // Large merged face - stress test
            (
                Face::East,
                [[10, 0, 5], [10, 8, 5], [10, 8, 12], [10, 0, 12]],
            ),
            // Edge case: single point (should have zero UV range)
            (Face::South, [[5, 5, 5], [5, 5, 5], [5, 5, 5], [5, 5, 5]]),
            // Irregular quad
            (Face::West, [[0, 0, 0], [0, 2, 0], [0, 3, 4], [0, 1, 4]]),
        ];

        for (face, corners) in test_cases.iter() {
            let simd_result = face_uvs_for_quad_simd(*face, *corners);
            let scalar_result = face_uvs_for_quad_scalar(*face, *corners);

            // Compare with small epsilon for floating point precision
            for i in 0..4 {
                let eps = 1e-6;
                assert!(
                    (simd_result[i][0] - scalar_result[i][0]).abs() < eps,
                    "UV U coordinate mismatch at vertex {}: SIMD={}, Scalar={} (face={:?}, corners={:?})",
                    i,
                    simd_result[i][0],
                    scalar_result[i][0],
                    face,
                    corners
                );
                assert!(
                    (simd_result[i][1] - scalar_result[i][1]).abs() < eps,
                    "UV V coordinate mismatch at vertex {}: SIMD={}, Scalar={} (face={:?}, corners={:?})",
                    i,
                    simd_result[i][1],
                    scalar_result[i][1],
                    face,
                    corners
                );
            }

            crate::console_info!("✓ Face {:?} test passed - UVs are identical", face);
        }
    }

    /// Benchmark UV calculation performance - single quad vs batch processing
    #[test]
    fn benchmark_uv_calculation() {
        use std::time::Instant;

        let test_face = Face::North;
        let test_corners = [[0, 0, 0], [100, 0, 0], [100, 100, 0], [0, 100, 0]];
        let iterations = 10_000;
        let batch_size = 100;

        // Create batch data
        let batch_corners: Vec<_> = (0..batch_size)
            .map(|i| [[i, 0, 0], [i + 1, 0, 0], [i + 1, 1, 0], [i, 1, 0]])
            .collect();

        // Benchmark scalar implementation (single quad)
        let start_scalar = Instant::now();
        for _ in 0..iterations {
            let _ = face_uvs_for_quad_scalar(test_face, test_corners);
        }
        let scalar_time = start_scalar.elapsed();

        // Benchmark scalar batch processing
        let start_scalar_batch = Instant::now();
        for _ in 0..(iterations / batch_size) {
            for corners in &batch_corners {
                let _ = face_uvs_for_quad_scalar(test_face, *corners);
            }
        }
        let scalar_batch_time = start_scalar_batch.elapsed();

        // Benchmark SIMD batch implementation
        let start_simd_batch = Instant::now();
        for _ in 0..(iterations / batch_size) {
            let _ = batch_face_uvs_for_quads_simd(test_face, &batch_corners);
        }
        let simd_batch_time = start_simd_batch.elapsed();

        crate::console_info!("Performance comparison:");
        crate::console_info!(
            "  Single quad scalar ({} iterations): {:?}",
            iterations,
            scalar_time
        );
        crate::console_info!(
            "  Batch scalar ({} quads, {} iterations): {:?}",
            batch_size,
            iterations / batch_size,
            scalar_batch_time
        );
        crate::console_info!(
            "  Batch SIMD ({} quads, {} iterations): {:?}",
            batch_size,
            iterations / batch_size,
            simd_batch_time
        );

        if simd_batch_time < scalar_batch_time {
            let speedup = scalar_batch_time.as_nanos() as f64 / simd_batch_time.as_nanos() as f64;
            crate::console_info!("  Batch SIMD speedup: {:.2}x", speedup);
        } else {
            let slowdown = simd_batch_time.as_nanos() as f64 / scalar_batch_time.as_nanos() as f64;
            crate::console_info!("  Batch SIMD is {:.2}x slower", slowdown);
        }
    }

    /// Test that mesh creation produces identical results
    #[test]
    fn test_mesh_creation_equivalence() {
        let positions = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let tex_layer = 5;
        let tint_kind = 2;

        let mut scalar_mesh = super::super::WorldCpuMeshData::default();
        let mut simd_mesh = super::super::WorldCpuMeshData::default();

        push_textured_quad_scalar(&mut scalar_mesh, positions, uvs, tex_layer, tint_kind);
        push_textured_quad_simd(&mut simd_mesh, positions, uvs, tex_layer, tint_kind);

        // Compare vertices
        assert_eq!(scalar_mesh.vertices.len(), simd_mesh.vertices.len());
        for i in 0..scalar_mesh.vertices.len() {
            let sv = &scalar_mesh.vertices[i];
            let dv = &simd_mesh.vertices[i];

            assert_eq!(
                sv.position, dv.position,
                "Position mismatch at vertex {}",
                i
            );
            assert_eq!(sv.uv, dv.uv, "UV mismatch at vertex {}", i);
            assert_eq!(
                sv.tex_layer, dv.tex_layer,
                "Texture layer mismatch at vertex {}",
                i
            );
            assert_eq!(sv.shade, dv.shade, "Shade mismatch at vertex {}", i);
            assert_eq!(
                sv.tint_kind, dv.tint_kind,
                "Tint kind mismatch at vertex {}",
                i
            );
            assert_eq!(
                sv.light_packed, dv.light_packed,
                "Light mismatch at vertex {}",
                i
            );
        }

        // Compare indices
        assert_eq!(scalar_mesh.indices, simd_mesh.indices);

        crate::console_info!("✓ Mesh creation test passed - identical results");
    }

    #[test]
    fn greedy_vertex_owning_block_pos_matches_expected_corners() {
        // Top face (+Y) merged 2x2 quad spanning x=[15,17], z=[0,2] on plane y=1.
        let origin_plane = [15, 1, 0];
        let d = 1usize;
        let u = 0usize;
        let v = 2usize;
        let w = 2;
        let h = 2;

        let c0 = IVec3::new(15, 1, 0);
        let c1 = IVec3::new(17, 1, 0);
        let c2 = IVec3::new(17, 1, 2);
        let c3 = IVec3::new(15, 1, 2);

        assert_eq!(
            greedy_vertex_owning_block_pos(c0, origin_plane, d, u, v, w, h, true),
            IVec3::new(15, 0, 0)
        );
        assert_eq!(
            greedy_vertex_owning_block_pos(c1, origin_plane, d, u, v, w, h, true),
            IVec3::new(16, 0, 0)
        );
        assert_eq!(
            greedy_vertex_owning_block_pos(c2, origin_plane, d, u, v, w, h, true),
            IVec3::new(16, 0, 1)
        );
        assert_eq!(
            greedy_vertex_owning_block_pos(c3, origin_plane, d, u, v, w, h, true),
            IVec3::new(15, 0, 1)
        );
    }

    #[test]
    fn ao_same_world_corner_matches_across_chunk_boundary() {
        let mut world = World::new();
        // 2x2 platform crossing x-chunk boundary at x=16.
        for x in 15..=16 {
            for z in 0..=1 {
                world
                    .set_block(IVec3::new(x, 0, z), crate::blocks::BlockId::Stone)
                    .unwrap();
            }
        }
        // Add a few occluders near the sampled top-corner to make AO non-trivial.
        world
            .set_block(IVec3::new(17, 1, 1), crate::blocks::BlockId::Stone)
            .unwrap();
        world
            .set_block(IVec3::new(16, 1, 2), crate::blocks::BlockId::Stone)
            .unwrap();

        let corner = IVec3::new(16, 1, 1);
        let left = cube_face_vertex_shade_and_light(&world, IVec3::new(15, 0, 0), Face::Up, corner);
        let right =
            cube_face_vertex_shade_and_light(&world, IVec3::new(16, 0, 0), Face::Up, corner);
        assert_eq!(
            left, right,
            "AO/light mismatch across chunk boundary at shared corner"
        );
    }

    #[test]
    fn greedy_and_naive_mesh_match_for_boundary_spanning_platform() {
        let mut world = World::new();
        for x in 15..=16 {
            for z in 0..=1 {
                world
                    .set_block(IVec3::new(x, 0, z), crate::blocks::BlockId::Stone)
                    .unwrap();
            }
        }
        let texture_lookup = test_texture_lookup();

        let coord = ChunkCoord { x: 0, y: 0, z: 0 };
        let (naive_vertices, _naive_indices) = flatten_layered_geometry(
            build_chunk_mesh_layered_naive(&world, coord, &texture_lookup),
        );
        let (greedy_vertices, _greedy_indices) = flatten_layered_geometry(
            build_chunk_mesh_greedy_layered(&world, coord, &texture_lookup),
        );

        // Compare a top outer corner that lies on the x=16 chunk boundary and survives greedy merging.
        let target = [16.0, 1.0, 2.0];
        let naive_at_target: Vec<_> = naive_vertices
            .iter()
            .filter(|v| v.position == target)
            .map(|v| (v.shade, v.light_packed))
            .collect();
        let greedy_at_target: Vec<_> = greedy_vertices
            .iter()
            .filter(|v| v.position == target)
            .map(|v| (v.shade, v.light_packed))
            .collect();

        assert!(
            !naive_at_target.is_empty() && !greedy_at_target.is_empty(),
            "expected seam corner vertex in both naive and greedy meshes"
        );
        for sample in &greedy_at_target {
            assert!(
                naive_at_target.contains(sample),
                "greedy seam vertex shading/light {:?} not found in naive mesh {:?}",
                sample,
                naive_at_target
            );
        }
    }

    #[test]
    fn slab_vertex_shading_stays_deterministic_with_multiple_occluders() {
        let mut world = World::new();
        let slab_pos = IVec3::new(0, 1, 0);
        world
            .set_block(slab_pos, crate::blocks::BlockId::StoneSlab)
            .unwrap();
        for occluder in [
            IVec3::new(1, 1, 1),
            IVec3::new(0, 0, 1),
            IVec3::new(1, 0, 1),
        ] {
            world
                .set_block(occluder, crate::blocks::BlockId::Stone)
                .unwrap();
        }
        world.set_light_packed(IVec3::new(0, 1, 1), 0xF0);

        let texture_lookup = test_texture_lookup();
        let vertex_local = [1.0, 0.0, 1.0];
        let shaded = face_vertex_shade_and_light(
            &world,
            slab_pos,
            Face::South,
            RenderBounds::LOWER_SLAB,
            vertex_local,
        );
        assert!(
            (shaded.0 - 0.4).abs() < 1e-6,
            "unexpected slab AO shade for south-east top vertex: {:?}",
            shaded
        );
        assert_ne!(
            shaded.1, 0,
            "slab vertex light must not collapse to black when AO occluders are present"
        );

        let coord = ChunkCoord { x: 0, y: 0, z: 0 };
        let mut scratch = MeshingNeighborhoodScratch::new();
        let (neighborhood, _) = scratch.build(coord, snapshot_for(&world, coord));
        let local = face_vertex_shade_and_light_local_fast(
            &neighborhood,
            &texture_lookup,
            [0, 1, 0],
            MeshingNeighborhood::local_padded_index(0, 1, 0),
            Face::South,
            RenderBounds::LOWER_SLAB,
            vertex_local,
        );
        assert_eq!(
            shaded, local,
            "dense neighborhood AO/light sampling must match the world path for slabs"
        );
    }

    #[test]
    fn lower_slab_occludes_cube_top_boundary_but_not_cube_side_boundary() {
        let mut world = World::new();
        let texture_lookup = test_texture_lookup();
        let cube_pos = IVec3::new(0, 0, 0);
        world
            .set_block(cube_pos, crate::blocks::BlockId::Stone)
            .unwrap();
        world
            .set_block(IVec3::new(0, 1, 0), crate::blocks::BlockId::StoneSlab)
            .unwrap();
        world
            .set_block(IVec3::new(1, 0, 0), crate::blocks::BlockId::StoneSlab)
            .unwrap();

        assert!(
            !should_emit_cube_face(
                &world,
                &texture_lookup,
                cube_pos,
                crate::blocks::BlockId::Stone,
                Face::Up,
            ),
            "a lower slab above should occlude the shared top boundary of a full cube"
        );
        assert!(
            should_emit_cube_face(
                &world,
                &texture_lookup,
                cube_pos,
                crate::blocks::BlockId::Stone,
                Face::East,
            ),
            "a lower slab beside a full cube must not be treated like a full side-face blocker"
        );
    }
}
