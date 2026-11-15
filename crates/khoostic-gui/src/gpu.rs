//! Vulkan device setup through wgpu. Only the Vulkan backend is compiled in and enabled.

use std::sync::Arc;

use anyhow::Context;
use winit::window::Window;

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

pub struct WindowSurface {
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    pub window: Arc<Window>,
}

impl Gpu {
    fn instance(display: Option<Box<dyn wgpu::wgt::WgpuHasDisplayHandle>>) -> wgpu::Instance {
        let mut desc = match display {
            Some(d) => wgpu::InstanceDescriptor::new_with_display_handle(d),
            None => wgpu::InstanceDescriptor::new_without_display_handle(),
        };
        desc.backends = wgpu::Backends::VULKAN;
        if cfg!(debug_assertions) {
            desc.flags |= wgpu::InstanceFlags::VALIDATION;
        }
        wgpu::Instance::new(desc)
    }

    async fn finish(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> anyhow::Result<Self> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: surface,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .context("no Vulkan-capable GPU found (Khoostic's GUI requires Vulkan; the TUI works without it)")?;
        let info = adapter.get_info();
        tracing::info!(name = %info.name, driver = %info.driver_info, "using Vulkan adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("khoostic"),
                required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::Performance,
                ..Default::default()
            })
            .await
            .context("creating Vulkan device")?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    pub async fn for_window(
        window: Arc<Window>,
        display: Box<dyn wgpu::wgt::WgpuHasDisplayHandle>,
        vsync: bool,
    ) -> anyhow::Result<(Self, WindowSurface)> {
        let instance = Self::instance(Some(display));
        let surface = instance
            .create_surface(window.clone())
            .context("creating Vulkan surface")?;
        let gpu = Self::finish(instance, Some(&surface)).await?;
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .context("surface reports no formats")?;
        let present_mode = pick_present_mode(&caps.present_modes, vsync);
        let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            caps.alpha_modes[0]
        };
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&gpu.device, &config);
        tracing::info!(?format, ?present_mode, "surface configured");
        Ok((
            gpu,
            WindowSurface {
                surface,
                config,
                window,
            },
        ))
    }

    /// A device with no window, for offscreen rendering (screenshots, tests).
    pub async fn headless() -> anyhow::Result<Self> {
        Self::finish(Self::instance(None), None).await
    }
}

fn pick_present_mode(modes: &[wgpu::PresentMode], vsync: bool) -> wgpu::PresentMode {
    use wgpu::PresentMode::*;
    let prefs: &[wgpu::PresentMode] = if vsync {
        &[Fifo]
    } else {
        &[Mailbox, Immediate, Fifo]
    };
    prefs
        .iter()
        .copied()
        .find(|m| modes.contains(m))
        .unwrap_or(Fifo)
}

impl WindowSurface {
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(device, &self.config);
    }
}
