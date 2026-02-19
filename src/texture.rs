use anyhow::{Context, Result, ensure};
use std::path::Path;

/// CPU-seitiger PNG-Loader:
/// - liest eine PNG von Disk
/// - dekodiert in RGBA8 (row-major, 4 bytes/pixel)
/// - gibt (width, height, raw_rgba) zurück
pub fn load_png_rgba(path: impl AsRef<Path>) -> Result<(u32, u32, Vec<u8>)> {
    let path = path.as_ref();
    let image = image::open(path)
        .with_context(|| format!("failed to decode PNG at '{}'", path.display()))?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok((width, height, rgba.into_raw()))
}

/// GPU-Texturarray für Blockfaces.
/// Jede Layer enthält eine Block-Textur (z.B. stone, grass_top, dirt, ...).
/// Das erlaubt pro Face einen Layer-Index statt nur "eine Textur für alles".
#[allow(dead_code)]
pub struct TextureArray {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    pub format: wgpu::TextureFormat,
    pub layer_count: u32,
}

impl TextureArray {
    pub fn create_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Texture Array BindGroup Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        })
    }

    pub fn create_bind_group(
        &self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        label: &str,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Erstellt ein TextureArray aus RGBA8-Layern und lädt sie per `queue.write_texture` hoch.
    pub fn from_rgba8_layers(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers: &[Vec<u8>],
        width: u32,
        height: u32,
        label: &str,
    ) -> Result<Self> {
        ensure!(
            width > 0 && height > 0,
            "invalid texture size: {}x{}",
            width,
            height
        );
        ensure!(!layers.is_empty(), "texture array needs at least one layer");

        let expected_len = (width as usize) * (height as usize) * 4;
        for (layer_idx, rgba) in layers.iter().enumerate() {
            ensure!(
                rgba.len() == expected_len,
                "invalid RGBA payload length in layer {}: got {}, expected {} for {}x{}",
                layer_idx,
                rgba.len(),
                expected_len,
                width,
                height
            );
        }

        let layer_count = layers.len() as u32;
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: layer_count,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        for (layer_idx, rgba) in layers.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer_idx as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * width),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }

        // Repeat + Nearest:
        // - Repeat ist nötig, damit Greedy-Quads mit UV > 1.0 die Textur pro Block kacheln.
        // - Nearest liefert den Minecraft-ähnlichen Pixel-Look.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Texture Array Sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("Texture Array View"),
            format: None,
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: 0,
            array_layer_count: Some(layer_count),
        });

        Ok(Self {
            texture,
            view,
            sampler,
            format,
            layer_count,
        })
    }
}
