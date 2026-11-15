//! Draws [`Quad`]s: one instanced triangle-strip draw per layer.

use crate::gui::render::Quad;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    size: [f32; 2],
    _pad: [f32; 2],
}

pub struct Painter {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    /// Screen-size uniform, shared with the image pipeline.
    pub bind_group: wgpu::BindGroup,
    pub globals_layout: wgpu::BindGroupLayout,
    instances: wgpu::Buffer,
    capacity: usize,
    /// Instance ranges per layer for the current frame.
    ranges: Vec<std::ops::Range<u32>>,
}

impl Painter {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/quad.wgsl"));
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("quad globals"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quad"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        const ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Quad>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &ATTRS,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let capacity = 1024;
        let instances = Self::alloc(device, capacity);
        Self {
            pipeline,
            globals,
            bind_group,
            globals_layout: bgl,
            instances,
            capacity,
            ranges: Vec::new(),
        }
    }

    fn alloc(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad instances"),
            size: (capacity * std::mem::size_of::<Quad>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Uploads all layers' quads into one buffer.
    pub fn prepare<'a>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: (u32, u32),
        layers: impl Iterator<Item = &'a [Quad]>,
    ) {
        let globals = Globals {
            size: [size.0 as f32, size.1 as f32],
            _pad: [0.0; 2],
        };
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));

        let mut all: Vec<Quad> = Vec::new();
        self.ranges.clear();
        for quads in layers {
            let start = all.len() as u32;
            all.extend_from_slice(quads);
            self.ranges.push(start..all.len() as u32);
        }
        if all.len() > self.capacity {
            self.capacity = all.len().next_power_of_two();
            self.instances = Self::alloc(device, self.capacity);
        }
        if !all.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&all));
        }
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, layer: usize) {
        let Some(range) = self.ranges.get(layer).filter(|r| !r.is_empty()) else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..4, range.clone());
    }
}
