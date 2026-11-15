//! Turns a [`Canvas`] into pixels: quads then text, per layer, in one render pass.

use anyhow::Context;

use crate::canvas::Canvas;
use crate::gpu::Gpu;
use crate::images::ImageStore;
use crate::painter::Painter;
use crate::text::TextSystem;

pub struct Renderer {
    pub gpu: Gpu,
    pub format: wgpu::TextureFormat,
    painter: Painter,
    pub text: TextSystem,
    pub images: ImageStore,
}

impl Renderer {
    pub fn new(gpu: Gpu, format: wgpu::TextureFormat, font: Option<String>) -> Self {
        let painter = Painter::new(&gpu.device, format);
        let text = TextSystem::new(&gpu.device, &gpu.queue, format, font);
        let images = ImageStore::new(&gpu.device, &gpu.queue, format, &painter.globals_layout);
        Self {
            gpu,
            format,
            painter,
            text,
            images,
        }
    }

    pub fn render(
        &mut self,
        canvas: &Canvas,
        target: &wgpu::TextureView,
        size: (u32, u32),
    ) -> anyhow::Result<()> {
        let (device, queue) = (&self.gpu.device, &self.gpu.queue);
        self.painter.prepare(
            device,
            queue,
            size,
            canvas.layers.iter().map(|l| l.quads.as_slice()),
        );
        self.text.prepare(device, queue, size, &canvas.layers)?;
        let image_layers: Vec<&[crate::canvas::ImageCmd]> =
            canvas.layers.iter().map(|l| l.images.as_slice()).collect();
        self.images.prepare(&image_layers);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        {
            let [r, g, b, a] = canvas.clear.linear();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: r as f64,
                            g: g as f64,
                            b: b as f64,
                            a: a as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for layer in 0..canvas.layers.len() {
                self.painter.draw(&mut pass, layer);
                self.images.draw(&mut pass, layer, &self.painter.bind_group);
                self.text.render(&mut pass, layer)?;
            }
        }
        queue.submit(Some(encoder.finish()));
        self.text.end_frame();
        Ok(())
    }

    /// Renders offscreen and returns tightly packed RGBA8 (sRGB) pixels.
    pub fn capture(&mut self, canvas: &Canvas, size: (u32, u32)) -> anyhow::Result<Vec<u8>> {
        let device = &self.gpu.device;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        self.render(canvas, &view, size)?;

        let device = &self.gpu.device;
        let bytes_per_row = (size.0 * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture readback"),
            size: (bytes_per_row * size.1) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(size.1),
                },
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
        self.gpu.queue.submit(Some(encoder.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        readback.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .context("waiting for GPU")?;
        rx.recv()?.context("mapping readback buffer")?;
        let data = readback.get_mapped_range(..)?;
        let bgra = matches!(
            self.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let mut out = Vec::with_capacity((size.0 * size.1 * 4) as usize);
        for row in data.chunks(bytes_per_row as usize).take(size.1 as usize) {
            for px in row[..(size.0 * 4) as usize].as_chunks::<4>().0 {
                if bgra {
                    out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                } else {
                    out.extend_from_slice(px);
                }
            }
        }
        Ok(out)
    }
}
