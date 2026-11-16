//! The theme swipe: keeps a copy of one frame (the last one in the old theme) and draws it
//! over the following frames, wiped away by an edge that sweeps across the window.

use crate::gui::render::Color;

/// One frame of a swipe: how far across it is (`0..=1`) and the color the edge glows in.
#[derive(Debug, Clone, Copy)]
pub struct WipeFrame {
    pub progress: f32,
    pub glow: Color,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    size: [f32; 2],
    progress: f32,
    scale: f32,
    color: [f32; 4],
}

struct Snapshot {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    size: (u32, u32),
}

pub struct Wipe {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
    snapshot: Option<Snapshot>,
}

impl Wipe {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/wipe.wgsl"));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wipe"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wipe"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wipe"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // The shader outputs old * a + glow: premultiplied, with additive glow.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wipe"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wipe"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            uniforms,
            sampler,
            format,
            snapshot: None,
        }
    }

    /// The texture to draw the snapshot frame into, (re)made at `size`.
    pub fn snapshot_target(
        &mut self,
        device: &wgpu::Device,
        size: (u32, u32),
    ) -> &wgpu::TextureView {
        if self.snapshot.as_ref().is_none_or(|s| s.size != size) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wipe snapshot"),
                size: wgpu::Extent3d {
                    width: size.0.max(1),
                    height: size.1.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("wipe"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.snapshot = Some(Snapshot {
                texture,
                view,
                bind_group,
                size,
            });
        }
        &self.snapshot.as_ref().expect("just made").view
    }

    /// Sets up this frame's swipe; false if there's no snapshot to swipe away at this size.
    pub fn prepare(
        &self,
        queue: &wgpu::Queue,
        size: (u32, u32),
        scale: f32,
        frame: WipeFrame,
    ) -> bool {
        if self.snapshot.as_ref().is_none_or(|s| s.size != size) {
            return false;
        }
        let u = Uniforms {
            size: [size.0 as f32, size.1 as f32],
            progress: frame.progress.clamp(0.0, 1.0),
            scale,
            color: frame.glow.linear(),
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
        true
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if let Some(s) = &self.snapshot {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &s.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// Frees the snapshot once a swipe is over.
    pub fn release(&mut self) {
        if let Some(s) = self.snapshot.take() {
            s.texture.destroy();
        }
    }

    pub fn has_snapshot(&self) -> bool {
        self.snapshot.is_some()
    }
}
