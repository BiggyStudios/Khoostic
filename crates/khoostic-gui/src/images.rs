//! GPU images (cover art): async decode with CPU-built mipmaps, LRU eviction, and a pipeline
//! that draws them as rounded, clipped quads.

use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender};

use crate::canvas::ImageCmd;

/// Produces the file to decode for an image; runs on a loader thread.
type Loader = Box<dyn FnOnce() -> Option<PathBuf> + Send>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageId(u64);

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    rect: [f32; 4],
    params: [f32; 4],
    clip: [f32; 4],
}

struct Decoded {
    key: String,
    /// Mip chain, largest first: (width, height, rgba).
    levels: Vec<(u32, u32, Vec<u8>)>,
}

enum Slot {
    Loading,
    Failed,
    Ready { id: ImageId, last_used: u64 },
}

struct GpuImage {
    bind_group: wgpu::BindGroup,
}

pub struct ImageStore {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    instances: wgpu::Buffer,
    capacity: usize,
    slots: HashMap<String, Slot>,
    gpu: HashMap<ImageId, GpuImage>,
    next_id: u64,
    frame: u64,
    requests: Sender<(String, Loader)>,
    decoded: Receiver<Option<Decoded>>,
    failed: Receiver<String>,
    /// Per layer: (image, instance index) for the current frame.
    draws: Vec<Vec<(ImageId, u32)>>,
}

const MAX_TEXTURES: usize = 384;

impl ImageStore {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        globals_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/image.wgsl"));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image"),
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
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image"),
            bind_group_layouts: &[Some(globals_layout), Some(&layout)],
            immediate_size: 0,
        });
        const ATTRS: [wgpu::VertexAttribute; 3] =
            wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        // Decoding happens off the render thread.
        let (req_tx, req_rx) = crossbeam_channel::unbounded::<(String, Loader)>();
        let (dec_tx, decoded) = crossbeam_channel::unbounded();
        let (fail_tx, failed) = crossbeam_channel::unbounded();
        for i in 0..2 {
            let (rx, tx, ftx) = (req_rx.clone(), dec_tx.clone(), fail_tx.clone());
            std::thread::Builder::new()
                .name(format!("khoostic-images-{i}"))
                .spawn(move || {
                    for (key, load) in rx {
                        // The loader may hit disk or the network (remote cover art).
                        match load().as_deref().and_then(decode) {
                            Some(levels) => {
                                let _ = tx.send(Some(Decoded { key, levels }));
                            }
                            None => {
                                let _ = ftx.send(key);
                            }
                        }
                    }
                })
                .expect("spawn image decoder");
        }

        Self {
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
            layout,
            sampler,
            instances: alloc(device, 64),
            capacity: 64,
            slots: HashMap::new(),
            gpu: HashMap::new(),
            next_id: 1,
            frame: 0,
            requests: req_tx,
            decoded,
            failed,
            draws: Vec::new(),
        }
    }

    /// The image for `key` if it's ready; otherwise queues `load` (run on a background thread,
    /// returning a file to decode) and returns `None`.
    pub fn get(
        &mut self,
        key: &str,
        load: impl FnOnce() -> Option<PathBuf> + Send + 'static,
    ) -> Option<ImageId> {
        match self.slots.get_mut(key) {
            Some(Slot::Ready { id, last_used }) => {
                *last_used = self.frame;
                Some(*id)
            }
            Some(_) => None,
            None => {
                self.slots.insert(key.to_owned(), Slot::Loading);
                let _ = self.requests.send((key.to_owned(), Box::new(load)));
                None
            }
        }
    }

    /// Uploads finished decodes. Returns true if anything new became visible.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(key) = self.failed.try_recv() {
            self.slots.insert(key, Slot::Failed);
        }
        while let Ok(Some(d)) = self.decoded.try_recv() {
            let id = ImageId(self.next_id);
            self.next_id += 1;
            self.gpu.insert(id, self.upload(&d.levels));
            self.slots.insert(
                d.key,
                Slot::Ready {
                    id,
                    last_used: self.frame,
                },
            );
            changed = true;
        }
        self.evict();
        changed
    }

    pub fn is_loading(&self) -> bool {
        self.slots.values().any(|s| matches!(s, Slot::Loading))
    }

    fn upload(&self, levels: &[(u32, u32, Vec<u8>)]) -> GpuImage {
        let (w, h, _) = levels[0];
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cover"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, (lw, lh, data)) in levels.iter().enumerate() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(lw * 4),
                    rows_per_image: Some(*lh),
                },
                wgpu::Extent3d {
                    width: *lw,
                    height: *lh,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&Default::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cover"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        GpuImage { bind_group }
    }

    fn evict(&mut self) {
        if self.gpu.len() <= MAX_TEXTURES {
            return;
        }
        let mut ready: Vec<(u64, String, ImageId)> = self
            .slots
            .iter()
            .filter_map(|(k, s)| match s {
                Slot::Ready { id, last_used } => Some((*last_used, k.clone(), *id)),
                _ => None,
            })
            .collect();
        ready.sort_by_key(|r| r.0);
        for (last_used, key, id) in ready.into_iter().take(self.gpu.len() - MAX_TEXTURES) {
            if self.frame - last_used < 2 {
                break; // never evict something on screen
            }
            self.slots.remove(&key);
            self.gpu.remove(&id);
        }
    }

    pub fn prepare(&mut self, layers: &[&[ImageCmd]]) {
        let mut all = Vec::new();
        self.draws.clear();
        for cmds in layers {
            let mut draws = Vec::new();
            for c in cmds.iter().filter(|c| self.gpu.contains_key(&c.image)) {
                draws.push((c.image, all.len() as u32));
                all.push(Instance {
                    rect: c.rect,
                    params: [c.radius, c.opacity, 0.0, 0.0],
                    clip: c.clip,
                });
            }
            self.draws.push(draws);
        }
        if all.len() > self.capacity {
            self.capacity = all.len().next_power_of_two();
            self.instances = alloc(&self.device, self.capacity);
        }
        if !all.is_empty() {
            self.queue
                .write_buffer(&self.instances, 0, bytemuck::cast_slice(&all));
        }
        self.frame += 1;
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, layer: usize, globals: &wgpu::BindGroup) {
        let Some(draws) = self.draws.get(layer).filter(|d| !d.is_empty()) else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, globals, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        for &(id, i) in draws {
            if let Some(img) = self.gpu.get(&id) {
                pass.set_bind_group(1, &img.bind_group, &[]);
                pass.draw(0..4, i..i + 1);
            }
        }
    }
}

fn alloc(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("image instances"),
        size: (capacity * std::mem::size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Decodes an image and builds its mip chain (box-filtered halvings).
fn decode(path: &std::path::Path) -> Option<Vec<(u32, u32, Vec<u8>)>> {
    let img = image::open(path).ok()?.to_rgba8();
    let mut levels = vec![(img.width(), img.height(), img.clone().into_raw())];
    let mut cur = img;
    while cur.width() > 1 || cur.height() > 1 {
        let (w, h) = ((cur.width() / 2).max(1), (cur.height() / 2).max(1));
        cur = image::imageops::resize(&cur, w, h, image::imageops::FilterType::Triangle);
        levels.push((w, h, cur.clone().into_raw()));
    }
    Some(levels)
}
