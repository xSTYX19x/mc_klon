pub(crate) struct DepthTexture {
    pub(crate) view: wgpu::TextureView,
    pub(crate) format: wgpu::TextureFormat,
}

impl DepthTexture {
    /// Every pipeline drawing into a pass that carries the depth attachment has
    /// to declare this format, so it lives here rather than being repeated.
    pub(crate) const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

    pub(crate) fn create(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> Self {
        let format = Self::FORMAT;

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
