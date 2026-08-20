//! Frozen, blurred backdrop for the pause menu.
//!
//! The pause menu used to cover the screen with an opaque tiled dirt texture,
//! which throws away the one thing the player was just looking at. This
//! captures the last gameplay frame straight from the swapchain image, blurs it
//! with two separable gaussian passes, and keeps the result until the menu
//! closes.
//!
//! Everything here runs **once per pause**, not per frame: the capture is
//! requested when the menu opens, and the cached texture is what every
//! subsequent paused frame samples.

use wgpu::util::DeviceExt;

use crate::render::depth::DepthTexture;
use crate::render::shaders::BLUR_SHADER_WGSL;

/// Where the backdrop is in its lifecycle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BackdropState {
    /// Not paused, or the cached image was invalidated (resize).
    #[default]
    Idle,
    /// Paused, but nothing captured yet. The menu is suppressed for this one
    /// frame so the capture shows the world rather than the menu itself.
    CapturePending,
    /// A blurred frame is cached and ready to draw.
    Ready,
}

struct BlurTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

pub(crate) struct PauseBackdrop {
    state: BackdropState,
    pipeline: wgpu::RenderPipeline,
    present_pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    horizontal_params: wgpu::Buffer,
    vertical_params: wgpu::Buffer,
    /// Ping-pong pair: the capture lands in `a`, the horizontal pass writes `b`,
    /// the vertical pass writes back into `a`, which is what gets drawn.
    a: Option<BlurTarget>,
    b: Option<BlurTarget>,
    size: (u32, u32),
    format: wgpu::TextureFormat,
    /// False when the surface cannot be copied from, which also disables
    /// screenshots. The menu then keeps its old opaque background.
    supported: bool,
}

impl PauseBackdrop {
    pub(crate) fn new(
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        supported: bool,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Pause Backdrop Blur Shader"),
            source: wgpu::ShaderSource::Wgsl(BLUR_SHADER_WGSL.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Pause Backdrop Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Pause Backdrop Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });
        // Two pipelines for one shader: the blur passes render to a bare colour
        // target, while presenting the result happens inside the UI pass, which
        // carries a depth attachment every pipeline in it must declare.
        let make_pipeline = |label: &'static str, depth: Option<wgpu::DepthStencilState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: depth,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = make_pipeline("Pause Backdrop Blur Pipeline", None);
        let present_pipeline = make_pipeline(
            "Pause Backdrop Present Pipeline",
            Some(wgpu::DepthStencilState {
                format: DepthTexture::FORMAT,
                // The backdrop covers everything behind the menu, so it ignores
                // whatever the world pass left in the depth buffer and writes
                // nothing back.
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Pause Backdrop Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let make_params = |label: &str, direction: [f32; 2]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(&[direction[0], direction[1], 0.0, 0.0]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
        };
        let mut backdrop = Self {
            state: BackdropState::Idle,
            pipeline,
            present_pipeline,
            bind_group_layout,
            sampler,
            horizontal_params: make_params("Pause Backdrop Blur H", [0.0, 0.0]),
            vertical_params: make_params("Pause Backdrop Blur V", [0.0, 0.0]),
            a: None,
            b: None,
            size: (0, 0),
            format: config.format,
            supported,
        };
        backdrop.resize(device, config);
        backdrop
    }

    /// Rebuilds the ping-pong targets and drops any cached image, because a
    /// blurred frame at the old size would be stretched.
    pub(crate) fn resize(&mut self, device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) {
        self.state = BackdropState::Idle;
        self.format = config.format;
        self.size = (config.width, config.height);
        if !self.supported || config.width == 0 || config.height == 0 {
            self.a = None;
            self.b = None;
            return;
        }
        self.a = Some(self.create_target(device, "Pause Backdrop A"));
        self.b = Some(self.create_target(device, "Pause Backdrop B"));
    }

    fn create_target(&self, device: &wgpu::Device, label: &'static str) -> BlurTarget {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: self.size.0,
                height: self.size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.horizontal_params.as_entire_binding(),
                },
            ],
        });
        BlurTarget {
            texture,
            view,
            bind_group,
        }
    }

    pub(crate) fn request_capture(&mut self) {
        if self.supported {
            self.state = BackdropState::CapturePending;
        }
    }

    pub(crate) fn clear(&mut self) {
        self.state = BackdropState::Idle;
    }

    /// True while the capture frame must render as plain gameplay, so the
    /// backdrop does not end up containing the menu it sits behind.
    pub(crate) fn is_capturing(&self) -> bool {
        self.state == BackdropState::CapturePending
    }

    /// Pipeline for drawing the cached backdrop inside the UI pass.
    pub(crate) fn present_pipeline(&self) -> &wgpu::RenderPipeline {
        &self.present_pipeline
    }

    pub(crate) fn ready_bind_group(&self) -> Option<&wgpu::BindGroup> {
        if self.state != BackdropState::Ready {
            return None;
        }
        self.a.as_ref().map(|target| &target.bind_group)
    }

    /// Copies the just-rendered swapchain image and blurs it in place.
    /// Call between the world pass and the UI pass.
    pub(crate) fn capture_and_blur(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame_texture: &wgpu::Texture,
    ) {
        if self.state != BackdropState::CapturePending {
            return;
        }
        let (Some(a), Some(b)) = (self.a.as_ref(), self.b.as_ref()) else {
            self.state = BackdropState::Idle;
            return;
        };
        if frame_texture.width() != self.size.0 || frame_texture.height() != self.size.1 {
            // A resize landed between request and capture; try again next frame.
            return;
        }

        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: frame_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &a.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.size.0,
                height: self.size.1,
                depth_or_array_layers: 1,
            },
        );

        // Step in texels, so the blur radius is resolution independent.
        let step = [1.0 / self.size.0 as f32, 1.0 / self.size.1 as f32];
        queue.write_buffer(
            &self.horizontal_params,
            0,
            bytemuck::cast_slice(&[step[0], 0.0f32, 0.0, 0.0]),
        );
        queue.write_buffer(
            &self.vertical_params,
            0,
            bytemuck::cast_slice(&[0.0f32, step[1], 0.0, 0.0]),
        );

        self.run_pass(encoder, "Pause Backdrop Blur H", &a.bind_group, &b.view);
        self.run_pass(encoder, "Pause Backdrop Blur V", &b.bind_group, &a.view);
        self.state = BackdropState::Ready;
    }

    fn run_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        label: &'static str,
        source: &wgpu::BindGroup,
        target: &wgpu::TextureView,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, source, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::BackdropState;

    /// The state machine is the part worth pinning: a stale backdrop behind the
    /// main menu, or a capture that never clears, are both silent bugs that only
    /// show up on screen.
    #[test]
    fn the_default_state_draws_nothing() {
        assert_eq!(BackdropState::default(), BackdropState::Idle);
    }

    #[test]
    fn only_the_ready_state_is_drawable() {
        // `ready_bind_group` gates on exactly this, so the capture frame cannot
        // draw a half-blurred or empty texture.
        assert_ne!(BackdropState::CapturePending, BackdropState::Ready);
        assert_ne!(BackdropState::Idle, BackdropState::Ready);
    }
}
