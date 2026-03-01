use std::time::{Duration, Instant};

use crate::meshing::{CpuMeshData, LayeredChunkGeometry, Vertex};

impl Vertex {
    pub(crate) fn layout() -> wgpu::VertexBufferLayout<'static> {
        use std::mem;
        wgpu::VertexBufferLayout {
            array_stride: mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 3]>() as wgpu::BufferAddress,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: (mem::size_of::<[f32; 3]>() + mem::size_of::<[f32; 2]>())
                        as wgpu::BufferAddress,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Uint32,
                },
                wgpu::VertexAttribute {
                    offset: (mem::size_of::<[f32; 3]>()
                        + mem::size_of::<[f32; 2]>()
                        + mem::size_of::<u32>()) as wgpu::BufferAddress,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: (mem::size_of::<[f32; 3]>()
                        + mem::size_of::<[f32; 2]>()
                        + mem::size_of::<u32>()
                        + mem::size_of::<[f32; 3]>())
                        as wgpu::BufferAddress,
                    shader_location: 4,
                    format: wgpu::VertexFormat::Uint32,
                },
                wgpu::VertexAttribute {
                    offset: (mem::size_of::<[f32; 3]>()
                        + mem::size_of::<[f32; 2]>()
                        + mem::size_of::<u32>()
                        + mem::size_of::<[f32; 3]>()
                        + mem::size_of::<u32>()) as wgpu::BufferAddress,
                    shader_location: 5,
                    format: wgpu::VertexFormat::Uint32,
                },
            ],
        }
    }
}

pub(crate) struct ChunkMesh {
    pub(crate) vertex_buffer: wgpu::Buffer,
    pub(crate) index_buffer: wgpu::Buffer,
    pub(crate) index_count: u32,
    pub(crate) vertex_capacity_bytes: u64,
    pub(crate) index_capacity_bytes: u64,
}

#[derive(Default)]
pub(crate) struct LayeredChunkMesh {
    pub(crate) opaque: Option<ChunkMesh>,
    pub(crate) cutout: Option<ChunkMesh>,
    pub(crate) transparent: Option<ChunkMesh>,
}

impl LayeredChunkMesh {
    pub(crate) fn is_empty(&self) -> bool {
        self.opaque.is_none() && self.cutout.is_none() && self.transparent.is_none()
    }
}

#[derive(Default, Debug, Clone, Copy)]
pub(crate) struct MeshUploadStats {
    pub(crate) created_buffers: u32,
    pub(crate) reused_buffer_writes: u32,
    pub(crate) bytes_created: u64,
    pub(crate) bytes_written: u64,
    pub(crate) buffer_create_time: Duration,
    pub(crate) buffer_write_time: Duration,
}

impl MeshUploadStats {
    pub(crate) fn add_assign(&mut self, other: Self) {
        self.created_buffers += other.created_buffers;
        self.reused_buffer_writes += other.reused_buffer_writes;
        self.bytes_created += other.bytes_created;
        self.bytes_written += other.bytes_written;
        self.buffer_create_time += other.buffer_create_time;
        self.buffer_write_time += other.buffer_write_time;
    }
}

fn grown_capacity(required: u64) -> u64 {
    required.max(256).next_power_of_two()
}

fn create_gpu_buffer(
    device: &wgpu::Device,
    label: String,
    usage: wgpu::BufferUsages,
    size: u64,
    stats: &mut MeshUploadStats,
) -> wgpu::Buffer {
    let started = Instant::now();
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(&label),
        size,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    stats.created_buffers += 1;
    stats.bytes_created += size;
    stats.buffer_create_time += started.elapsed();
    buffer
}

fn write_buffer_bytes(
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    bytes: &[u8],
    reused_existing_buffer: bool,
    stats: &mut MeshUploadStats,
) {
    let started = Instant::now();
    queue.write_buffer(buffer, 0, bytes);
    if reused_existing_buffer {
        stats.reused_buffer_writes += 1;
    }
    stats.bytes_written += bytes.len() as u64;
    stats.buffer_write_time += started.elapsed();
}

pub(crate) fn upload_cpu_mesh_to_gpu_reuse(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label_prefix: &str,
    mesh: CpuMeshData,
    existing: Option<ChunkMesh>,
) -> (Option<ChunkMesh>, MeshUploadStats) {
    let mut stats = MeshUploadStats::default();
    if mesh.indices.is_empty() {
        return (None, stats);
    }

    let vertex_bytes = bytemuck::cast_slice(&mesh.vertices);
    let index_bytes = bytemuck::cast_slice(&mesh.indices);
    let required_vertex_bytes = vertex_bytes.len() as u64;
    let required_index_bytes = index_bytes.len() as u64;

    let (mut vertex_buffer, vertex_capacity_bytes, vertex_reused) = if let Some(ref old) = existing
    {
        if old.vertex_capacity_bytes >= required_vertex_bytes {
            (
                Some(old.vertex_buffer.clone()),
                old.vertex_capacity_bytes,
                true,
            )
        } else {
            (None, grown_capacity(required_vertex_bytes), false)
        }
    } else {
        (None, grown_capacity(required_vertex_bytes), false)
    };
    if vertex_buffer.is_none() {
        vertex_buffer = Some(create_gpu_buffer(
            device,
            format!("{label_prefix} Vertex Buffer"),
            wgpu::BufferUsages::VERTEX,
            vertex_capacity_bytes,
            &mut stats,
        ));
    }
    let vertex_buffer = vertex_buffer.expect("vertex buffer must be allocated");
    write_buffer_bytes(
        queue,
        &vertex_buffer,
        vertex_bytes,
        vertex_reused,
        &mut stats,
    );

    let (mut index_buffer, index_capacity_bytes, index_reused) = if let Some(ref old) = existing {
        if old.index_capacity_bytes >= required_index_bytes {
            (
                Some(old.index_buffer.clone()),
                old.index_capacity_bytes,
                true,
            )
        } else {
            (None, grown_capacity(required_index_bytes), false)
        }
    } else {
        (None, grown_capacity(required_index_bytes), false)
    };
    if index_buffer.is_none() {
        index_buffer = Some(create_gpu_buffer(
            device,
            format!("{label_prefix} Index Buffer"),
            wgpu::BufferUsages::INDEX,
            index_capacity_bytes,
            &mut stats,
        ));
    }
    let index_buffer = index_buffer.expect("index buffer must be allocated");
    write_buffer_bytes(queue, &index_buffer, index_bytes, index_reused, &mut stats);

    (
        Some(ChunkMesh {
            vertex_buffer,
            index_buffer,
            index_count: mesh.indices.len() as u32,
            vertex_capacity_bytes,
            index_capacity_bytes,
        }),
        stats,
    )
}

pub(crate) fn upload_cpu_mesh_to_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label_prefix: &str,
    mesh: CpuMeshData,
) -> Option<ChunkMesh> {
    upload_cpu_mesh_to_gpu_reuse(device, queue, label_prefix, mesh, None).0
}

pub(crate) fn upload_layered_chunk_mesh_to_gpu_reuse(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layered: LayeredChunkGeometry,
    existing: Option<LayeredChunkMesh>,
) -> (Option<LayeredChunkMesh>, MeshUploadStats) {
    let mut stats = MeshUploadStats::default();
    let mut existing = existing.unwrap_or_default();

    let (opaque, opaque_stats) = upload_cpu_mesh_to_gpu_reuse(
        device,
        queue,
        "Chunk Opaque",
        layered.opaque,
        existing.opaque.take(),
    );
    stats.add_assign(opaque_stats);
    let (cutout, cutout_stats) = upload_cpu_mesh_to_gpu_reuse(
        device,
        queue,
        "Chunk Cutout",
        layered.cutout,
        existing.cutout.take(),
    );
    stats.add_assign(cutout_stats);
    let (transparent, transparent_stats) = upload_cpu_mesh_to_gpu_reuse(
        device,
        queue,
        "Chunk Transparent",
        layered.transparent,
        existing.transparent.take(),
    );
    stats.add_assign(transparent_stats);

    let layered_mesh = LayeredChunkMesh {
        opaque,
        cutout,
        transparent,
    };
    if layered_mesh.is_empty() {
        (None, stats)
    } else {
        (Some(layered_mesh), stats)
    }
}

pub(crate) fn upload_layered_chunk_mesh_to_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layered: LayeredChunkGeometry,
) -> Option<LayeredChunkMesh> {
    upload_layered_chunk_mesh_to_gpu_reuse(device, queue, layered, None).0
}
