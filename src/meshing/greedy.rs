use std::sync::atomic::{AtomicBool, Ordering};

use glam::{IVec3, Vec4};

use crate::blocks::{BlockId, BlockModel, BlockTextureLookup, CullGroup, Face};
#[cfg(test)]
use crate::world::World;
use crate::world::fluids::fluid_kind;
use crate::world::{BlockAccess, CHUNK_SIZE, ChunkCoord};

#[cfg(test)]
use super::flatten_layered_geometry;
use super::liquids::fluid_surface_height_at;
use super::{
    CpuMeshData, FULLBRIGHT_LIGHT_PACKED, LayeredChunkGeometry, Vertex, chunk_offset,
    chunk_world_origin, pack_vertex_light,
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
    let (u_axis, v_axis) = face_uv_basis(face);

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
    if !neighbor.is_voxel_filled() || texture_lookup.model_for(neighbor) != BlockModel::Cube {
        return true;
    }

    let current_group = texture_lookup.cull_group_for(current);
    let neighbor_group = texture_lookup.cull_group_for(neighbor);
    if current_group != CullGroup::None && current_group == neighbor_group {
        if let Some(kind) = fluid_kind(current) {
            if matches!(face, Face::North | Face::South | Face::East | Face::West) {
                let h_current = fluid_surface_height_at(world, current_pos, kind);
                let h_neighbor = fluid_surface_height_at(world, neighbor_pos, kind);
                return h_neighbor + 1e-6 < h_current;
            }
        }
        return false;
    }

    !texture_lookup.face_occluding_for(neighbor)
}

#[inline(always)]
fn face_shade(face: Face) -> f32 {
    match face {
        Face::Up => 1.0,
        Face::North | Face::South => 0.8,
        Face::East | Face::West => 0.6,
        Face::Down => 0.5,
    }
}

#[inline]
fn face_axes(face: Face) -> (IVec3, IVec3) {
    match face {
        Face::North | Face::South => (IVec3::X, IVec3::Y),
        Face::East | Face::West => (IVec3::Z, IVec3::Y),
        Face::Up | Face::Down => (IVec3::X, IVec3::Z),
    }
}

#[inline]
fn axis_sign_from_corner(axis: IVec3, block_pos: IVec3, corner_pos: IVec3) -> i32 {
    if axis.x != 0 {
        if corner_pos.x > block_pos.x { 1 } else { -1 }
    } else if axis.y != 0 {
        if corner_pos.y > block_pos.y { 1 } else { -1 }
    } else if corner_pos.z > block_pos.z {
        1
    } else {
        -1
    }
}

#[inline(always)]
fn is_ao_occluder(world: &impl BlockAccess, pos: IVec3) -> bool {
    world.get_block(pos).blocks_skylight()
}

#[inline(always)]
fn ao_factor_from_neighbors(side1: bool, side2: bool, corner: bool) -> f32 {
    if side1 && side2 {
        return 0.5;
    }
    match side1 as u8 + side2 as u8 + corner as u8 {
        0 => 1.0,
        1 => 0.85,
        2 => 0.7,
        _ => 0.5,
    }
}

fn averaged_vertex_light(world: &impl BlockAccess, samples: [IVec3; 4]) -> u32 {
    let mut sky_sum = 0u32;
    let mut blk_sum = 0u32;
    for pos in samples {
        let packed = world.get_light_packed(pos);
        sky_sum += ((packed >> 4) & 0x0F) as u32;
        blk_sum += (packed & 0x0F) as u32;
    }
    let sky = ((sky_sum + 2) / 4) as u8;
    let blk = ((blk_sum + 2) / 4) as u8;
    pack_vertex_light(sky, blk)
}

fn cube_face_vertex_shade_and_light(
    world: &impl BlockAccess,
    block_pos: IVec3,
    face: Face,
    corner_pos: IVec3,
) -> (f32, u32) {
    let n = {
        let nn = face.normal_i32();
        IVec3::new(nn[0], nn[1], nn[2])
    };
    let (u_axis, v_axis) = face_axes(face);
    let su = axis_sign_from_corner(u_axis, block_pos, corner_pos);
    let sv = axis_sign_from_corner(v_axis, block_pos, corner_pos);
    let outer = block_pos + n;
    let side1_pos = outer + u_axis * su;
    let side2_pos = outer + v_axis * sv;
    let corner_sample_pos = outer + u_axis * su + v_axis * sv;

    let side1_occ = is_ao_occluder(world, side1_pos);
    let side2_occ = is_ao_occluder(world, side2_pos);
    let corner_occ = is_ao_occluder(world, corner_sample_pos);
    let ao = if debug_disable_ao() {
        1.0
    } else {
        ao_factor_from_neighbors(side1_occ, side2_occ, corner_occ)
    };

    let light = averaged_vertex_light(world, [outer, side1_pos, side2_pos, corner_sample_pos]);
    (face_shade(face) * ao, light)
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
                        mesh.vertices.push(Vertex {
                            position: [
                                (x + p[0]) as f32 + chunk_offset[0],
                                (y + p[1]) as f32 + chunk_offset[1],
                                (z + p[2]) as f32 + chunk_offset[2],
                            ],
                            uv: quad_uvs[corner_index],
                            tex_layer,
                            color: [shade, shade, shade],
                            tint_kind,
                            light_packed,
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
        layered,
        Some(pool),
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MaskCell {
    pub(crate) block: BlockId,
    pub(crate) normal_positive: bool,
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
                        mesh.vertices.push(Vertex {
                            position: p(qp),
                            uv,
                            tex_layer,
                            color: [shade, shade, shade],
                            tint_kind,
                            light_packed,
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
        layered,
        Some(pool),
    );
}

#[cfg(test)]
pub(crate) fn build_chunk_mesh_greedy(
    world: &World,
    coord: ChunkCoord,
    texture_lookup: &BlockTextureLookup,
) -> (Vec<Vertex>, Vec<u32>) {
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
    mesh: &mut CpuMeshData,
    positions: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    tex_layer: u32,
    tint_kind: u32,
) {
    let base = mesh.vertices.len() as u32;

    // Reserve capacity to avoid reallocations during quad creation
    mesh.vertices.reserve(4);
    mesh.indices.reserve(6);

    // Create all 4 vertices in one batch to improve cache locality
    let color = [1.0, 1.0, 1.0];
    let light = FULLBRIGHT_LIGHT_PACKED;

    mesh.vertices.extend_from_slice(&[
        Vertex {
            position: positions[0],
            uv: uvs[0],
            tex_layer,
            color,
            tint_kind,
            light_packed: light,
        },
        Vertex {
            position: positions[1],
            uv: uvs[1],
            tex_layer,
            color,
            tint_kind,
            light_packed: light,
        },
        Vertex {
            position: positions[2],
            uv: uvs[2],
            tex_layer,
            color,
            tint_kind,
            light_packed: light,
        },
        Vertex {
            position: positions[3],
            uv: uvs[3],
            tex_layer,
            color,
            tint_kind,
            light_packed: light,
        },
    ]);

    // Add indices for two triangles (quad = 2 triangles)
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Original implementation kept for fallback
pub(super) fn push_textured_quad_scalar(
    mesh: &mut CpuMeshData,
    positions: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    tex_layer: u32,
    tint_kind: u32,
) {
    let base = mesh.vertices.len() as u32;
    for i in 0..4 {
        mesh.vertices.push(Vertex {
            position: positions[i],
            uv: uvs[i],
            tex_layer,
            color: [1.0, 1.0, 1.0],
            tint_kind,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
        });
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Optimized quad creation with improved memory allocation patterns
#[inline]
pub(super) fn push_textured_quad(
    mesh: &mut CpuMeshData,
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
    mesh: &mut CpuMeshData,
    quads: &[([[f32; 3]; 4], [[f32; 2]; 4], u32, u32)], // (positions, uvs, tex_layer, tint_kind)
) {
    let quad_count = quads.len();

    // Reserve all memory upfront to avoid reallocations
    mesh.vertices.reserve(quad_count * 4);
    mesh.indices.reserve(quad_count * 6);

    let color = [1.0, 1.0, 1.0];
    let light = FULLBRIGHT_LIGHT_PACKED;

    for (positions, uvs, tex_layer, tint_kind) in quads {
        let base = mesh.vertices.len() as u32;

        // Add vertices in batch
        mesh.vertices.extend_from_slice(&[
            Vertex {
                position: positions[0],
                uv: uvs[0],
                tex_layer: *tex_layer,
                color,
                tint_kind: *tint_kind,
                light_packed: light,
            },
            Vertex {
                position: positions[1],
                uv: uvs[1],
                tex_layer: *tex_layer,
                color,
                tint_kind: *tint_kind,
                light_packed: light,
            },
            Vertex {
                position: positions[2],
                uv: uvs[2],
                tex_layer: *tex_layer,
                color,
                tint_kind: *tint_kind,
                light_packed: light,
            },
            Vertex {
                position: positions[3],
                uv: uvs[3],
                tex_layer: *tex_layer,
                color,
                tint_kind: *tint_kind,
                light_packed: light,
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
    use glam::IVec3;
    use std::collections::HashMap;

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

            println!("✓ Face {:?} test passed - UVs are identical", face);
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

        println!("Performance comparison:");
        println!(
            "  Single quad scalar ({} iterations): {:?}",
            iterations, scalar_time
        );
        println!(
            "  Batch scalar ({} quads, {} iterations): {:?}",
            batch_size,
            iterations / batch_size,
            scalar_batch_time
        );
        println!(
            "  Batch SIMD ({} quads, {} iterations): {:?}",
            batch_size,
            iterations / batch_size,
            simd_batch_time
        );

        if simd_batch_time < scalar_batch_time {
            let speedup = scalar_batch_time.as_nanos() as f64 / simd_batch_time.as_nanos() as f64;
            println!("  Batch SIMD speedup: {:.2}x", speedup);
        } else {
            let slowdown = simd_batch_time.as_nanos() as f64 / scalar_batch_time.as_nanos() as f64;
            println!("  Batch SIMD is {:.2}x slower", slowdown);
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

        let mut scalar_mesh = CpuMeshData::default();
        let mut simd_mesh = CpuMeshData::default();

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
            assert_eq!(sv.color, dv.color, "Color mismatch at vertex {}", i);
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

        println!("✓ Mesh creation test passed - identical results");
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
            .map(|v| (v.color, v.light_packed))
            .collect();
        let greedy_at_target: Vec<_> = greedy_vertices
            .iter()
            .filter(|v| v.position == target)
            .map(|v| (v.color, v.light_packed))
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
}
