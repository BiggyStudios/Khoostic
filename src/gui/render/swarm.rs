//! The 3D visualizer's GPU side: a compute pass moves ~30k cubes, a render pass draws them
//! (4x MSAA, depth) into an offscreen texture, and the UI shows that texture like any image
//! ([`ImageId::SCENE`]), so clipping, rounded corners and fades work as usual.

use crate::gui::render::Color;
use crate::gui::render::images::{ImageId, ImageStore};

const SWARM: u32 = 27648;
const WALLS: u32 = 2 * 32 * 32;
const CUBES: u32 = SWARM + WALLS;
const SAMPLES: u32 = 4;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// What the visualizer page asks for this frame.
#[derive(Debug, Clone)]
pub struct SwarmFrame {
    /// Physical pixels of the view (set by [`Canvas::swarm`](crate::gui::render::Canvas)).
    pub size: (u32, u32),
    /// Logical size of the view (the bars are laid out in logical pixels, like the 2D ones).
    pub logical: (f32, f32),
    /// Where the 2D bars are inside the view (logical x, y, width, height); the cubes stack
    /// up exactly there before breaking loose.
    pub bars: [f32; 4],
    /// Seconds the storm has been running (drives the flow and the camera).
    pub time: f32,
    /// Music time: runs faster when the music is loud; moves the wave along its path.
    pub tau: f32,
    /// True on the frame a beat lands (one burst per beat, however fast frames come).
    pub kick: bool,
    /// How recently a beat landed: 1 then, fading over about half a second.
    pub impact: f32,
    /// Sudden rises in the bass (0..1).
    pub punch: f32,
    /// How far the ribbon has spun around itself (radians; mostly the mids).
    pub spin: f32,
    /// Sudden rises in the mids (0..1).
    pub mid_punch: f32,
    /// How full the music is overall (smoothed, 0..1).
    pub fullness: f32,
    pub dt: f32,
    pub mode: Mode,
    /// Seconds since `mode` started.
    pub phase: f32,
    /// How far into 3D the camera has moved, 0..1.
    pub depth: f32,
    /// How far the side walls have risen, 0..1.
    pub walls: f32,
    pub bands: [f32; 64],
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    pub beat: f32,
    pub spectrum: [Color; 4],
    pub accent: Color,
    pub surface: Color,
    pub fg: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Cubes stacked into the 2D bars.
    Bars,
    /// Breaking loose into the storm (and staying there).
    Storm,
    /// Flying back into the bars.
    Assemble,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    cam: [f32; 4],
    params: [f32; 4],
    audio: [f32; 4],
    layout: [f32; 4],
    bars: [f32; 4],
    spectrum: [[f32; 4]; 4],
    accent: [f32; 4],
    surface: [f32; 4],
    fg: [f32; 4],
    bands: [[f32; 4]; 16],
    motion: [f32; 4],
    motion2: [f32; 4],
}

struct Targets {
    size: (u32, u32),
    msaa: wgpu::TextureView,
    depth: wgpu::TextureView,
    resolve: wgpu::TextureView,
}

pub struct Swarm {
    uniforms: wgpu::Buffer,
    sim_group: wgpu::BindGroup,
    draw_group: wgpu::BindGroup,
    sim: wgpu::ComputePipeline,
    cubes: wgpu::RenderPipeline,
    background: wgpu::RenderPipeline,
    targets: Option<Targets>,
    /// Set by `prepare`: draw this frame.
    pending: bool,
}

impl Swarm {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/swarm.wgsl"));
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("swarm"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cubes = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("swarm cubes"),
            size: CUBES as u64 * 32,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        // The simulation writes the cubes; drawing reads them (vertex shaders can't bind
        // writable storage), so each gets its own layout and bind group.
        let make_layout = |binding, read_only, stages| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("swarm"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::all(),
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding,
                        visibility: stages,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            })
        };
        let make_group = |layout: &wgpu::BindGroupLayout, binding| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("swarm"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding,
                        resource: cubes.as_entire_binding(),
                    },
                ],
            })
        };
        let sim_layout = make_layout(1, false, wgpu::ShaderStages::COMPUTE);
        let draw_layout = make_layout(2, true, wgpu::ShaderStages::VERTEX);
        let sim_group = make_group(&sim_layout, 1);
        let draw_group = make_group(&draw_layout, 2);
        let pipeline_layout = |layout: &wgpu::BindGroupLayout| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("swarm"),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            })
        };
        let sim_pipeline_layout = pipeline_layout(&sim_layout);
        let draw_pipeline_layout = pipeline_layout(&draw_layout);
        let sim = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("swarm sim"),
            layout: Some(&sim_pipeline_layout),
            module: &shader,
            entry_point: Some("sim"),
            compilation_options: Default::default(),
            cache: None,
        });
        let render = |label, vs, fs, depth_write: bool, compare| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&draw_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(compare),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: SAMPLES,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let cubes_pipeline = render(
            "swarm cubes",
            "vs_cube",
            "fs_cube",
            true,
            wgpu::CompareFunction::Less,
        );
        let background = render(
            "swarm backdrop",
            "vs_bg",
            "fs_bg",
            false,
            wgpu::CompareFunction::Always,
        );
        Self {
            uniforms,
            sim_group,
            draw_group,
            sim,
            cubes: cubes_pipeline,
            background,
            targets: None,
            pending: false,
        }
    }

    /// Sizes the targets, uploads this frame's uniforms and (re)registers the result with the
    /// image store. Call before the image store prepares the frame.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        images: &mut ImageStore,
        f: &SwarmFrame,
    ) {
        let size = (f.size.0.max(1), f.size.1.max(1));
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            let make = |label, format, samples, usage| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width: size.0,
                            height: size.1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: samples,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            };
            let attach = wgpu::TextureUsages::RENDER_ATTACHMENT;
            let targets = Targets {
                size,
                msaa: make("swarm msaa", FORMAT, SAMPLES, attach),
                depth: make("swarm depth", DEPTH, SAMPLES, attach),
                resolve: make(
                    "swarm",
                    FORMAT,
                    1,
                    attach | wgpu::TextureUsages::TEXTURE_BINDING,
                ),
            };
            images.set_external(ImageId::SCENE, &targets.resolve);
            self.targets = Some(targets);
        }

        let aspect = size.0 as f32 / size.1 as f32;
        let (eye, target, fov) = camera(f);
        let view_proj = mul(
            perspective(fov, aspect, 0.05, 40.0),
            look_at(eye, target, [0.0, 1.0, 0.0]),
        );
        let lin = |c: Color| c.linear();
        let mut bands = [[0.0; 4]; 16];
        for (i, b) in f.bands.iter().enumerate() {
            bands[i / 4][i % 4] = b.clamp(0.0, 1.0);
        }
        let mode = match f.mode {
            Mode::Bars => 0.0,
            Mode::Storm => 1.0,
            Mode::Assemble => 2.0,
        };
        let u = Uniforms {
            view_proj,
            cam: [eye[0], eye[1], eye[2], f.time],
            params: [f.dt.clamp(0.0, 1.0 / 20.0), f.phase, mode, f.depth],
            audio: [f.bass, f.mid, f.treble, f.beat],
            layout: [f.logical.0, f.logical.1, f.walls, 0.45 * f.depth],
            bars: f.bars,
            spectrum: f.spectrum.map(lin),
            accent: lin(f.accent),
            surface: lin(f.surface),
            fg: lin(f.fg),
            bands,
            motion: [f.tau, f.kick as u8 as f32, f.impact, f.punch],
            motion2: [f.spin, f.mid_punch, f.fullness, 0.0],
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
        self.pending = true;
    }

    /// Records this frame's simulation and drawing (if `prepare` asked for one).
    pub fn encode(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let Some(t) = self.targets.as_ref().filter(|_| self.pending) else {
            return;
        };
        self.pending = false;
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("swarm sim"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.sim);
            pass.set_bind_group(0, &self.sim_group, &[]);
            pass.dispatch_workgroups(CUBES.div_ceil(256), 1, 1);
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("swarm"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &t.msaa,
                depth_slice: None,
                resolve_target: Some(&t.resolve),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &t.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &self.draw_group, &[]);
        pass.set_pipeline(&self.background);
        pass.draw(0..3, 0..1);
        pass.set_pipeline(&self.cubes);
        pass.draw(0..36, 0..CUBES);
    }
}

/// Eye, target and vertical field of view. Flat on, the camera frames the view exactly like
/// the 2D page (the bars' plane fills it); in 3D it sits down a corridor between the walls,
/// drifting, pushed in by the bass.
fn camera(f: &SwarmFrame) -> ([f32; 3], [f32; 3], f32) {
    let flat_dist = 6.0_f32;
    let flat_fov = 2.0 * (1.0 / flat_dist).atan();
    let d = smooth(f.depth);
    let t = f.time;
    let swing = 0.2 * (t * 0.07).sin() + 0.06 * (t * 0.19).sin();
    // Each hit punches the camera in a little.
    let dist = 4.3 - 0.25 * f.bass - 0.45 * f.impact * f.impact;
    let deep = [
        swing.sin() * dist,
        0.35 + 0.18 * (t * 0.13).sin(),
        swing.cos() * dist,
    ];
    let shake = 0.05 * f.impact * f.impact;
    let eye = [
        lerp(0.0, deep[0] + shake * (t * 47.0).sin(), d),
        lerp(0.0, deep[1] + shake * (t * 53.0).cos(), d),
        lerp(flat_dist, deep[2], d),
    ];
    let look = [0.0, lerp(0.0, -0.05, d), 0.0];
    (eye, look, lerp(flat_fov, 50f32.to_radians(), d))
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

type Mat = [[f32; 4]; 4];

/// Right-handed perspective with depth 0..1 (column-major).
fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat {
    let f = 1.0 / (fov_y / 2.0).tan();
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far / (near - far), -1.0],
        [0.0, 0.0, near * far / (near - far), 0.0],
    ]
}

fn look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> Mat {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let norm = |a: [f32; 3]| {
        let l = dot(a, a).sqrt().max(1e-6);
        [a[0] / l, a[1] / l, a[2] / l]
    };
    let f = norm(sub(target, eye));
    let s = norm(cross(f, up));
    let u = cross(s, f);
    [
        [s[0], u[0], -f[0], 0.0],
        [s[1], u[1], -f[1], 0.0],
        [s[2], u[2], -f[2], 0.0],
        [-dot(s, eye), -dot(u, eye), dot(f, eye), 1.0],
    ]
}

fn mul(a: Mat, b: Mat) -> Mat {
    let mut out = [[0.0; 4]; 4];
    for (c, col) in out.iter_mut().enumerate() {
        for (r, v) in col.iter_mut().enumerate() {
            *v = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(m: &Mat, p: [f32; 3]) -> [f32; 3] {
        let v = [p[0], p[1], p[2], 1.0];
        let mut o = [0.0; 4];
        for (r, x) in o.iter_mut().enumerate() {
            *x = (0..4).map(|c| m[c][r] * v[c]).sum();
        }
        [o[0] / o[3], o[1] / o[3], o[2] / o[3]]
    }

    #[test]
    fn flat_camera_frames_the_bars_like_the_2d_page() {
        let mut f = SwarmFrame {
            size: (1200, 600),
            logical: (1200.0, 600.0),
            bars: [0.0, 0.0, 1200.0, 600.0],
            time: 3.0,
            tau: 3.0,
            kick: false,
            impact: 0.0,
            punch: 0.0,
            spin: 0.0,
            mid_punch: 0.0,
            fullness: 0.0,
            dt: 0.016,
            mode: Mode::Bars,
            phase: 0.0,
            depth: 0.0,
            walls: 0.0,
            bands: [0.0; 64],
            bass: 0.5,
            mid: 0.0,
            treble: 0.0,
            beat: 1.0,
            spectrum: [Color::WHITE; 4],
            accent: Color::WHITE,
            surface: Color::BLACK,
            fg: Color::WHITE,
        };
        let aspect = 2.0;
        let (eye, target, fov) = camera(&f);
        let m = mul(
            perspective(fov, aspect, 0.05, 40.0),
            look_at(eye, target, [0.0, 1.0, 0.0]),
        );
        // The view is 2 world units high, centered: its corners land on the screen's corners.
        let tl = project(&m, [-aspect, 1.0, 0.0]);
        let br = project(&m, [aspect, -1.0, 0.0]);
        assert!(
            (tl[0] + 1.0).abs() < 1e-3 && (tl[1] - 1.0).abs() < 1e-3,
            "{tl:?}"
        );
        assert!(
            (br[0] - 1.0).abs() < 1e-3 && (br[1] + 1.0).abs() < 1e-3,
            "{br:?}"
        );
        assert!(tl[2] > 0.0 && tl[2] < 1.0);
        // Deep in 3D the storm's heart is in front of the camera.
        f.depth = 1.0;
        let (eye, target, fov) = camera(&f);
        let m = mul(
            perspective(fov, aspect, 0.05, 40.0),
            look_at(eye, target, [0.0, 1.0, 0.0]),
        );
        let c = project(&m, [0.0, 0.0, 0.0]);
        assert!(
            c[0].abs() < 0.2 && c[1].abs() < 0.3 && c[2] > 0.0 && c[2] < 1.0,
            "{c:?}"
        );
    }
}
