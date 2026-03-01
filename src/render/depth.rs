pub(crate) struct DepthTexture {
    pub(crate) view: wgpu::TextureView,
    pub(crate) format: wgpu::TextureFormat,
}

impl DepthTexture {
    pub(crate) fn create(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> Self {
        // Typisches Depth Format
        let format = wgpu::TextureFormat::Depth24Plus;

        let desc = wgpu::TextureDescriptor {
            label: Some("Depth Texture"),
            size: wgpu::Extent3d {
                width: config.width,
                height: config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        };

        let tex = device.create_texture(&desc);
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());

        Self { view, format }
    }
}
